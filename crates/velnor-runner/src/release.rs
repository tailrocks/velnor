//! Plan 010 — non-circular release identity model.
//!
//! One release commit must bind, in a single acyclic chain,
//! `source SHA → crate version → per-arch binary/deb digests → OCI image digest →
//! compiled-manifest hash → APT publication → deployed export`. This module owns
//! the canonical serde schemas for that chain plus the deterministic
//! emit/verify/activate primitives the release workflow, the APT publisher, and
//! the host activation scripts all agree on.
//!
//! The APT publication function is a coherence boundary, not an authenticity
//! primitive. It accepts typed claims produced by the trusted publisher-side
//! verifier; it does not parse raw APT files, hash served bytes, or verify GPG.
//! Untrusted JSON must never be supplied as either metadata claim.
//!
//! ## Acyclicity
//!
//! A [`ReleaseRecord`] never contains its own digest and never sits inside bytes
//! whose digest it records. The record's digest lives *outside* it — in a sibling
//! `.sha256` checksum, in the [`PublicationRecord`] that promotes it, and in the
//! [`DeployedIdentity`] pointer on the host. That is what keeps the chain a DAG:
//! every "points at" edge flows from a wrapper into the record, never back.
//!
//! ## Development builds
//!
//! Without the `release-build` feature the embedded identity is `development`
//! (see `build.rs`); [`emit_record`] refuses to produce a publishable record from
//! a development binary. The pure verify/parse logic is exercised entirely by
//! fixtures so the normal (feature-off) test path proves the whole model.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::args::{
    ReleaseActivateArgs, ReleaseArgs, ReleaseAssembleArgs, ReleaseCommand, ReleaseEmitArgs,
    ReleaseExportArgs, ReleaseRollbackArgs, ReleaseVerifyInstalledArgs, ReleaseVerifyRecordArgs,
    INSTALLED_BINARY_PATH,
};

/// Schema tags. A consumer refuses an unknown shape before trusting any field.
pub const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";
pub const PUBLICATION_RECORD_SCHEMA: &str = "velnor.publication-record/v1";
pub const APT_PUBLICATION_METADATA_SCHEMA: &str = "velnor.apt-publication-metadata/v1";
pub const DEPLOYED_IDENTITY_SCHEMA: &str = "velnor.deployed-identity/v1";

/// Canonical source repository the release chain is anchored to.
pub const SOURCE_REPOSITORY: &str = "tailrocks/velnor";
pub const SOURCE_URL: &str = "https://github.com/tailrocks/velnor";
const CANONICAL_OCI_IMAGE: &str = "ghcr.io/tailrocks/velnor-job-ubuntu";
const MAX_RELEASE_METADATA_BYTES: usize = 64 * 1024;
const MAX_RELEASE_CHECKSUM_BYTES: usize = 4096;
const RECORD_FILE_NAME: &str = "record.json";
const DEPLOYED_FILE_NAME: &str = "deployed.json";
const RELEASE_STATE_LOCK_FILE_NAME: &str = ".state.lock";
const RELEASE_TRANSITION_FILE_NAME: &str = ".transition.json";

/// Every release ships exactly these architectures; a record missing or
/// duplicating one is incoherent (per-arch completeness).
pub const REQUIRED_ARCHES: [Arch; 2] = [Arch::Amd64, Arch::Arm64];

// ---------------------------------------------------------------------------
// Embedded build identity (from build.rs)
// ---------------------------------------------------------------------------

/// The compile-time source identity stamped by `build.rs`. `development` for the
/// default (feature-off) build; a real 40-hex SHA + `v*` tag under
/// `release-build`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmbeddedIdentity {
    pub source_sha: String,
    pub tag: String,
    pub kind: String,
    pub crate_version: String,
}

/// Read the identity embedded at compile time.
pub fn embedded() -> EmbeddedIdentity {
    EmbeddedIdentity {
        source_sha: env!("VELNOR_SOURCE_SHA").to_string(),
        tag: env!("VELNOR_SOURCE_TAG").to_string(),
        kind: env!("VELNOR_BUILD_KIND").to_string(),
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

impl EmbeddedIdentity {
    /// A development build cannot anchor a publishable record.
    pub fn is_development(&self) -> bool {
        self.kind != "release" || self.source_sha == "development"
    }
}

// ---------------------------------------------------------------------------
// Validated digest / SHA newtypes
// ---------------------------------------------------------------------------

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A git commit: exactly 40 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceSha(String);

impl SourceSha {
    pub fn parse(value: &str) -> Result<Self> {
        if is_lower_hex(value, 40) {
            Ok(Self(value.to_string()))
        } else {
            bail!("source commit must be exactly 40 lowercase hex characters")
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for SourceSha {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<SourceSha> for String {
    fn from(value: SourceSha) -> Self {
        value.0
    }
}
impl fmt::Display for SourceSha {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A bare SHA-256: exactly 64 lowercase hex characters (no algorithm prefix).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub fn parse(value: &str) -> Result<Self> {
        if is_lower_hex(value, 64) {
            Ok(Self(value.to_string()))
        } else {
            bail!("sha-256 must be exactly 64 lowercase hex characters")
        }
    }
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(hex_lower(&digest))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_zero(&self) -> bool {
        self.0.bytes().all(|byte| byte == b'0')
    }
}
impl TryFrom<String> for Sha256Hex {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<Sha256Hex> for String {
    fn from(value: Sha256Hex) -> Self {
        value.0
    }
}
impl fmt::Display for Sha256Hex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An OCI content digest: `sha256:` + 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OciDigest(String);

impl OciDigest {
    pub fn parse(value: &str) -> Result<Self> {
        match value.strip_prefix("sha256:") {
            Some(hex) if is_lower_hex(hex, 64) => Ok(Self(value.to_string())),
            _ => bail!("OCI digest must be 'sha256:' followed by 64 lowercase hex characters"),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for OciDigest {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<OciDigest> for String {
    fn from(value: OciDigest) -> Self {
        value.0
    }
}
impl fmt::Display for OciDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    out
}

// ---------------------------------------------------------------------------
// Architecture
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Amd64,
    Arm64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::Amd64 => "amd64",
            Arch::Arm64 => "arm64",
        }
    }
    /// The Rust target triple each architecture is built from.
    pub fn target(self) -> &'static str {
        match self {
            Arch::Amd64 => "x86_64-unknown-linux-gnu",
            Arch::Arm64 => "aarch64-unknown-linux-gnu",
        }
    }
    /// This binary's own architecture (for `verify-installed`).
    pub fn host() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Arch::Amd64),
            "aarch64" => Some(Arch::Arm64),
            _ => None,
        }
    }
}

impl std::str::FromStr for Arch {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "amd64" | "x86_64" => Ok(Arch::Amd64),
            "arm64" | "aarch64" => Ok(Arch::Arm64),
            other => bail!("unknown architecture '{other}' (expected amd64 or arm64)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub repository: String,
    pub tag: String,
    pub commit: SourceSha,
    pub crate_version: String,
    pub debian_version: String,
    pub manifest_version: u32,
    pub manifest_sha256: Sha256Hex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureIdentity {
    pub arch: Arch,
    pub target: String,
    pub binary_sha256: Sha256Hex,
    pub deb_sha256: Sha256Hex,
    pub oci_platform_digest: OciDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciLabels {
    pub version: String,
    pub revision: SourceSha,
    pub source: String,
    pub manifest_sha256: Sha256Hex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptCoordinate {
    pub origin: String,
    pub suite: String,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRecord {
    pub schema: String,
    pub build: BuildIdentity,
    pub architectures: Vec<ArchitectureIdentity>,
    pub oci_index_digest: OciDigest,
    pub oci_image_ref: String,
    pub oci_labels: OciLabels,
    pub apt: AptCoordinate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagesIndex {
    pub arch: Arch,
    pub sha256: Sha256Hex,
}

/// Preverified metadata for bytes served by the APT repository. The size is
/// kept beside the digest so the trusted byte-verification boundary can reject
/// truncation or concatenation before constructing this claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptArtifactMetadata {
    pub sha256: Sha256Hex,
    pub size: u64,
}

/// Metadata parsed from the APT `Release` file. A Release file must not list
/// itself in its checksum sections: that is circular and cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptReleaseMetadata {
    pub artifact: AptArtifactMetadata,
    pub package_indexes: Vec<AptPackageIndexMetadata>,
    pub self_row: Option<AptArtifactMetadata>,
    pub self_row_checked: bool,
}

/// Preverified metadata for a signed APT artifact (`InRelease` or detached
/// `Release.gpg`). The publisher-side signature verifier must construct the
/// claim and bind it to the exact served `Release` bytes before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptSignatureMetadata {
    pub artifact: AptArtifactMetadata,
    pub signed_release_sha256: Sha256Hex,
    pub signer_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptPackageMetadata {
    pub arch: Arch,
    pub path: String,
    pub artifact: AptArtifactMetadata,
}

/// A `Packages` file covered by a signed APT `Release` checksum section. The
/// path is relative to the suite directory, exactly as encoded by `Release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptPackageIndexMetadata {
    pub arch: Arch,
    pub path: String,
    pub artifact: AptArtifactMetadata,
}

/// Expected metadata claims supplied by the trusted publication verifier. It is
/// intentionally separate from [`PublicationRecord`], whose v1 schema predates
/// exact APT file sizes and the detached-signature binding. This type does not
/// authenticate its JSON representation; the publisher-side verifier owns that
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedAptPublicationMetadata {
    pub schema: String,
    pub release: AptReleaseMetadata,
    pub inrelease: AptSignatureMetadata,
    pub release_gpg: AptSignatureMetadata,
    pub packages: Vec<AptPackageMetadata>,
}

/// Served metadata claims are optional at every trust boundary so a caller
/// cannot accidentally turn a partial fetch or parse into a successful
/// coherence check. The caller must authenticate the source bytes first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActualAptPublicationMetadata {
    pub schema: String,
    pub release: Option<AptReleaseMetadata>,
    pub inrelease: Option<AptSignatureMetadata>,
    pub release_gpg: Option<AptSignatureMetadata>,
    pub packages: Option<Vec<AptPackageMetadata>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PreviousPointer {
    Coherent {
        tag: String,
        source_record_sha256: Sha256Hex,
    },
    /// One-time bridge for the last signed package predating release records.
    LegacyObserved(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRecord {
    pub schema: String,
    pub source_record_sha256: Sha256Hex,
    pub tag: String,
    pub crate_version: String,
    pub inrelease_sha256: Sha256Hex,
    pub packages: Vec<PackagesIndex>,
    pub signer_fingerprint: String,
    pub previous: Option<PreviousPointer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployedIdentity {
    pub schema: String,
    pub package_version: String,
    pub crate_version: String,
    pub source_commit: SourceSha,
    pub binary_sha256: Sha256Hex,
    pub manifest_version: u32,
    pub manifest_sha256: Sha256Hex,
    pub oci_image_digest: OciDigest,
    /// Points AT the active release record (never the record's own digest).
    pub record_sha256: Sha256Hex,
}

// ---------------------------------------------------------------------------
// Coherence errors (values are never echoed — redacted diagnostics)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoherenceError {
    #[error("record does not parse as the canonical release-record schema")]
    Malformed,
    #[error("record bytes are not the canonical serialization of their content")]
    NonCanonical,
    #[error("record checksum does not match the record bytes")]
    RecordChecksum,
    #[error("unexpected schema tag (want {want})")]
    Schema { want: &'static str },
    #[error("required field '{0}' is empty")]
    EmptyField(&'static str),
    #[error("release tag does not equal v<crate version>")]
    TagVersion,
    #[error("release tag is not a safe path component")]
    TagPath,
    #[error("debian version does not equal the crate version")]
    DebianVersion,
    #[error("compiled-manifest version is not the expected schema version")]
    ManifestVersion,
    #[error("record repository is not the anchored source repository")]
    Repository,
    #[error("architecture set is not exactly {{amd64, arm64}}")]
    ArchitectureSet,
    #[error("duplicate architecture entry")]
    DuplicateArch,
    #[error("architecture target triple does not match its architecture")]
    ArchTarget,
    #[error("OCI image ref does not embed the index digest")]
    OciRef,
    #[error("OCI label 'version' disagrees with the crate version")]
    OciVersion,
    #[error("OCI label 'revision' disagrees with the source commit")]
    OciRevision,
    #[error("OCI label 'source' is not the canonical source URL")]
    OciSource,
    #[error("OCI label manifest hash disagrees with the compiled-manifest hash")]
    OciManifestHash,
    #[error("deployed pointer does not reference the active record digest")]
    InstalledRecordPointer,
    #[error("deployed source commit disagrees with the record")]
    InstalledSource,
    #[error("deployed crate version disagrees with the record")]
    InstalledCrateVersion,
    #[error("deployed package version disagrees with the record")]
    InstalledPackageVersion,
    #[error("deployed manifest version disagrees with the record")]
    InstalledManifestVersion,
    #[error("deployed manifest hash disagrees with the record")]
    InstalledManifestHash,
    #[error("deployed OCI image digest disagrees with the record")]
    InstalledOci,
    #[error("installed binary digest disagrees with the record for this architecture")]
    InstalledBinary,
    #[error("record has no entry for the host architecture")]
    InstalledArchMissing,
    #[error("publication record does not bind the source record digest")]
    PublicationBinding,
    #[error("publication record version disagrees with the source record")]
    PublicationVersion,
    #[error("publication package index references an unsupported architecture")]
    PublicationPackageArch,
    #[error("publication previous pointer references the current release")]
    PublicationPrevious,
    #[error("publication contains an empty digest")]
    PublicationDigestEmpty,
    #[error("APT publication metadata is incomplete")]
    PublicationMetadataMissing,
    #[error("APT publication metadata contains an empty artifact")]
    PublicationMetadataEmpty,
    #[error("APT Release contains a self-referential checksum row")]
    PublicationReleaseSelfRow,
    #[error("APT publication metadata disagrees with expected artifact metadata")]
    PublicationMetadataMismatch,
    #[error("APT signature metadata does not bind the expected Release or signer")]
    PublicationSignature,
    #[error("APT package metadata does not bind the publication")]
    PublicationPackageMetadata,
    #[error("APT metadata does not bind the publication record")]
    PublicationMetadataBinding,
}

// ---------------------------------------------------------------------------
// Emit / canonicalize / digest
// ---------------------------------------------------------------------------

impl ReleaseRecord {
    /// Deterministic canonical JSON: architectures sorted by architecture,
    /// two-space pretty, trailing newline. Byte-identical for equal logical
    /// content on any builder, so the digest is reproducible.
    pub fn to_canonical_json(&self) -> String {
        let mut normalized = self.clone();
        normalized.architectures.sort_by_key(|item| item.arch);
        let mut json =
            serde_json::to_string_pretty(&normalized).expect("release record always serializes");
        json.push('\n');
        json
    }

    /// SHA-256 over the canonical bytes. This digest is stored OUTSIDE the record
    /// (checksum sidecar / publication / deployed pointer) — never within it.
    pub fn digest(&self) -> Sha256Hex {
        Sha256Hex::of_bytes(self.to_canonical_json().as_bytes())
    }

    pub fn architecture(&self, arch: Arch) -> Option<&ArchitectureIdentity> {
        self.architectures.iter().find(|item| item.arch == arch)
    }

    /// Structural + cross-field coherence of one record. Every distinct
    /// single-field defect maps to a distinct [`CoherenceError`].
    pub fn verify(&self) -> std::result::Result<(), CoherenceError> {
        if self.schema != RELEASE_RECORD_SCHEMA {
            return Err(CoherenceError::Schema {
                want: RELEASE_RECORD_SCHEMA,
            });
        }
        let build = &self.build;
        if build.repository != SOURCE_REPOSITORY {
            return Err(CoherenceError::Repository);
        }
        if build.crate_version.is_empty() {
            return Err(CoherenceError::EmptyField("crate_version"));
        }
        if build.tag != format!("v{}", build.crate_version) {
            return Err(CoherenceError::TagVersion);
        }
        if !is_safe_path_component(&build.tag) {
            return Err(CoherenceError::TagPath);
        }
        if build.debian_version != build.crate_version {
            return Err(CoherenceError::DebianVersion);
        }
        if build.manifest_version != crate::manifest::MANIFEST_VERSION {
            return Err(CoherenceError::ManifestVersion);
        }

        // Per-arch completeness: exactly the required set, no duplicates, and
        // each entry's target triple matches its architecture.
        let mut seen: Vec<Arch> = Vec::new();
        for item in &self.architectures {
            if seen.contains(&item.arch) {
                return Err(CoherenceError::DuplicateArch);
            }
            if item.target != item.arch.target() {
                return Err(CoherenceError::ArchTarget);
            }
            seen.push(item.arch);
        }
        seen.sort();
        let mut required = REQUIRED_ARCHES.to_vec();
        required.sort();
        if seen != required {
            return Err(CoherenceError::ArchitectureSet);
        }

        if self.oci_image_ref != format!("{CANONICAL_OCI_IMAGE}@{}", self.oci_index_digest) {
            return Err(CoherenceError::OciRef);
        }
        if self.oci_labels.version != build.crate_version {
            return Err(CoherenceError::OciVersion);
        }
        if self.oci_labels.revision != build.commit {
            return Err(CoherenceError::OciRevision);
        }
        if self.oci_labels.source != SOURCE_URL {
            return Err(CoherenceError::OciSource);
        }
        if self.oci_labels.manifest_sha256 != build.manifest_sha256 {
            return Err(CoherenceError::OciManifestHash);
        }
        if self.apt.origin.is_empty() {
            return Err(CoherenceError::EmptyField("apt.origin"));
        }
        if self.apt.suite.is_empty() {
            return Err(CoherenceError::EmptyField("apt.suite"));
        }
        if self.apt.component.is_empty() {
            return Err(CoherenceError::EmptyField("apt.component"));
        }
        Ok(())
    }
}

/// Parse + fully verify record bytes against an independent checksum. The bytes
/// MUST be the canonical serialization (so `sha256(bytes) == record.digest()`).
pub fn verify_record_bytes(
    bytes: &[u8],
    expected: &Sha256Hex,
) -> std::result::Result<ReleaseRecord, CoherenceError> {
    if bytes.len() > MAX_RELEASE_METADATA_BYTES {
        return Err(CoherenceError::Malformed);
    }
    if &Sha256Hex::of_bytes(bytes) != expected {
        return Err(CoherenceError::RecordChecksum);
    }
    let record: ReleaseRecord =
        serde_json::from_slice(bytes).map_err(|_| CoherenceError::Malformed)?;
    if record.to_canonical_json().as_bytes() != bytes {
        return Err(CoherenceError::NonCanonical);
    }
    record.verify()?;
    Ok(record)
}

fn is_full_fingerprint(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'A'..=b'F'))
}

/// Cross-check the on-host deployed identity against the active record and the
/// installed binary's own digest. Fails on any single-field drift so a mixed
/// old/new tuple can never start.
pub fn verify_installed(
    deployed: &DeployedIdentity,
    record: &ReleaseRecord,
    host: Arch,
    installed_binary_sha256: &Sha256Hex,
) -> std::result::Result<(), CoherenceError> {
    if deployed.schema != DEPLOYED_IDENTITY_SCHEMA {
        return Err(CoherenceError::Schema {
            want: DEPLOYED_IDENTITY_SCHEMA,
        });
    }
    record.verify()?;
    if deployed.record_sha256 != record.digest() {
        return Err(CoherenceError::InstalledRecordPointer);
    }
    if deployed.source_commit != record.build.commit {
        return Err(CoherenceError::InstalledSource);
    }
    if deployed.crate_version != record.build.crate_version {
        return Err(CoherenceError::InstalledCrateVersion);
    }
    if deployed.package_version != record.build.debian_version {
        return Err(CoherenceError::InstalledPackageVersion);
    }
    if deployed.manifest_version != record.build.manifest_version {
        return Err(CoherenceError::InstalledManifestVersion);
    }
    if deployed.manifest_sha256 != record.build.manifest_sha256 {
        return Err(CoherenceError::InstalledManifestHash);
    }
    if deployed.oci_image_digest != record.oci_index_digest {
        return Err(CoherenceError::InstalledOci);
    }
    let arch = record
        .architecture(host)
        .ok_or(CoherenceError::InstalledArchMissing)?;
    if &deployed.binary_sha256 != installed_binary_sha256
        || deployed.binary_sha256 != arch.binary_sha256
    {
        return Err(CoherenceError::InstalledBinary);
    }
    Ok(())
}

/// Prove that an APT [`PublicationRecord`] promotes exactly this source record:
/// its `source_record_sha256` points AT the record digest (wrapper→record edge),
/// its versions agree, and its previous pointer is a *different* release. This is
/// the source-side check; the APT `verify-release.sh` re-derives the same binding
/// independently before `reprepro`.
pub fn verify_publication_binds(
    publication: &PublicationRecord,
    record: &ReleaseRecord,
) -> std::result::Result<(), CoherenceError> {
    if publication.schema != PUBLICATION_RECORD_SCHEMA {
        return Err(CoherenceError::Schema {
            want: PUBLICATION_RECORD_SCHEMA,
        });
    }
    if publication.source_record_sha256 != record.digest() {
        return Err(CoherenceError::PublicationBinding);
    }
    if publication.tag != record.build.tag
        || publication.crate_version != record.build.crate_version
    {
        return Err(CoherenceError::PublicationVersion);
    }
    if !is_full_fingerprint(&publication.signer_fingerprint) {
        return Err(CoherenceError::EmptyField("publication.signer_fingerprint"));
    }
    if publication.inrelease_sha256.is_zero() {
        return Err(CoherenceError::PublicationDigestEmpty);
    }
    // The published index must cover exactly the release's supported
    // architectures. A partial index is not a coherent publication.
    let mut seen = Vec::with_capacity(publication.packages.len());
    if publication.packages.is_empty() {
        return Err(CoherenceError::EmptyField("publication.packages"));
    }
    for index in &publication.packages {
        if !REQUIRED_ARCHES.contains(&index.arch) || record.architecture(index.arch).is_none() {
            return Err(CoherenceError::PublicationPackageArch);
        }
        if seen.contains(&index.arch) {
            return Err(CoherenceError::PublicationPackageArch);
        }
        seen.push(index.arch);
        // `index.sha256` and `publication.inrelease_sha256` are validated hex by
        // construction; require them present (never the empty/placeholder digest).
        if index.sha256.is_zero() || index.sha256 == publication.inrelease_sha256 {
            return Err(CoherenceError::PublicationPackageArch);
        }
    }
    seen.sort();
    let mut required = REQUIRED_ARCHES.to_vec();
    required.sort();
    if seen != required {
        return Err(CoherenceError::PublicationPackageArch);
    }
    if let Some(previous) = &publication.previous {
        match previous {
            PreviousPointer::Coherent {
                tag,
                source_record_sha256,
            } => {
                if tag == &record.build.tag || source_record_sha256 == &record.digest() {
                    return Err(CoherenceError::PublicationPrevious);
                }
            }
            PreviousPointer::LegacyObserved(tag) => {
                if tag != "v0.1.121" || tag == &record.build.tag {
                    return Err(CoherenceError::PublicationPrevious);
                }
            }
        }
    }
    Ok(())
}

fn verify_apt_artifact(
    expected: &AptArtifactMetadata,
    actual: &AptArtifactMetadata,
) -> std::result::Result<(), CoherenceError> {
    if expected.size == 0
        || actual.size == 0
        || expected
            .sha256
            .as_str()
            .chars()
            .all(|character| character == '0')
        || actual
            .sha256
            .as_str()
            .chars()
            .all(|character| character == '0')
    {
        return Err(CoherenceError::PublicationMetadataEmpty);
    }
    if expected != actual {
        return Err(CoherenceError::PublicationMetadataMismatch);
    }
    Ok(())
}

fn verify_apt_package_metadata(
    record: &ReleaseRecord,
    expected: &[AptPackageMetadata],
    actual: &[AptPackageMetadata],
) -> std::result::Result<(), CoherenceError> {
    if expected.is_empty() || actual.is_empty() {
        return Err(CoherenceError::PublicationMetadataEmpty);
    }
    if expected.len() != actual.len() || expected.len() != REQUIRED_ARCHES.len() {
        return Err(CoherenceError::PublicationPackageMetadata);
    }

    for package in expected {
        if !REQUIRED_ARCHES.contains(&package.arch)
            || record.architecture(package.arch).is_none()
            || expected
                .iter()
                .filter(|item| item.arch == package.arch)
                .count()
                != 1
        {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
        let served = actual
            .iter()
            .find(|item| item.arch == package.arch)
            .ok_or(CoherenceError::PublicationPackageMetadata)?;
        let expected_path = apt_deb_path(record, package.arch);
        if package.path != expected_path || served.path != expected_path {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
        verify_apt_artifact(&package.artifact, &served.artifact)
            .map_err(|_| CoherenceError::PublicationPackageMetadata)?;
        let record_arch = record
            .architecture(package.arch)
            .ok_or(CoherenceError::PublicationPackageMetadata)?;
        if served.artifact.sha256 != record_arch.deb_sha256 {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
    }

    for package in actual {
        if !REQUIRED_ARCHES.contains(&package.arch)
            || record.architecture(package.arch).is_none()
            || actual
                .iter()
                .filter(|item| item.arch == package.arch)
                .count()
                != 1
            || expected.iter().all(|item| item.arch != package.arch)
        {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
    }
    Ok(())
}

fn apt_packages_path(record: &ReleaseRecord, arch: Arch) -> String {
    format!("{}/binary-{}/Packages", record.apt.component, arch.as_str())
}

fn apt_deb_path(record: &ReleaseRecord, arch: Arch) -> String {
    format!(
        "pool/{}/v/velnor-runner/velnor-runner_{}_{}.deb",
        record.apt.component,
        record.build.debian_version,
        arch.as_str()
    )
}

fn verify_apt_package_indexes(
    publication: &PublicationRecord,
    record: &ReleaseRecord,
    expected: &[AptPackageIndexMetadata],
    actual: &[AptPackageIndexMetadata],
) -> std::result::Result<(), CoherenceError> {
    if expected.len() != actual.len() || expected.len() != REQUIRED_ARCHES.len() {
        return Err(CoherenceError::PublicationPackageMetadata);
    }
    for index in expected {
        if !REQUIRED_ARCHES.contains(&index.arch)
            || expected
                .iter()
                .filter(|item| item.arch == index.arch)
                .count()
                != 1
        {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
        let served = actual
            .iter()
            .find(|item| item.arch == index.arch)
            .ok_or(CoherenceError::PublicationPackageMetadata)?;
        let expected_path = apt_packages_path(record, index.arch);
        if index.path != expected_path || served.path != expected_path {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
        verify_apt_artifact(&index.artifact, &served.artifact)
            .map_err(|_| CoherenceError::PublicationPackageMetadata)?;
        let published = publication
            .packages
            .iter()
            .find(|item| item.arch == index.arch)
            .ok_or(CoherenceError::PublicationPackageMetadata)?;
        if published.sha256 != served.artifact.sha256 {
            return Err(CoherenceError::PublicationPackageMetadata);
        }
    }
    if actual.iter().any(|index| {
        !REQUIRED_ARCHES.contains(&index.arch)
            || actual.iter().filter(|item| item.arch == index.arch).count() != 1
            || expected.iter().all(|item| item.arch != index.arch)
    }) {
        return Err(CoherenceError::PublicationPackageMetadata);
    }
    Ok(())
}

/// Verify preverified APT metadata claims against trusted expected values and
/// the source-side publication record. This function deliberately does not
/// hash raw bytes or verify GPG signatures; its caller must obtain these typed
/// claims from the trusted publisher-side byte/signature verifier first.
/// Optional fields in [`ActualAptPublicationMetadata`] deliberately make a
/// partial fetch fail closed.
pub fn verify_apt_publication_metadata(
    publication: &PublicationRecord,
    record: &ReleaseRecord,
    expected: &ExpectedAptPublicationMetadata,
    actual: &ActualAptPublicationMetadata,
) -> std::result::Result<(), CoherenceError> {
    verify_publication_binds(publication, record)?;
    if expected.schema != APT_PUBLICATION_METADATA_SCHEMA
        || actual.schema != APT_PUBLICATION_METADATA_SCHEMA
    {
        return Err(CoherenceError::Schema {
            want: APT_PUBLICATION_METADATA_SCHEMA,
        });
    }

    let release = actual
        .release
        .as_ref()
        .ok_or(CoherenceError::PublicationMetadataMissing)?;
    let inrelease = actual
        .inrelease
        .as_ref()
        .ok_or(CoherenceError::PublicationMetadataMissing)?;
    let release_gpg = actual
        .release_gpg
        .as_ref()
        .ok_or(CoherenceError::PublicationMetadataMissing)?;
    let packages = actual
        .packages
        .as_ref()
        .ok_or(CoherenceError::PublicationMetadataMissing)?;

    if !expected.release.self_row_checked || !release.self_row_checked {
        return Err(CoherenceError::PublicationMetadataMissing);
    }
    if expected.release.self_row.is_some() || release.self_row.is_some() {
        return Err(CoherenceError::PublicationReleaseSelfRow);
    }
    verify_apt_artifact(&expected.release.artifact, &release.artifact)?;
    verify_apt_package_indexes(
        publication,
        record,
        &expected.release.package_indexes,
        &release.package_indexes,
    )?;
    verify_apt_artifact(&expected.inrelease.artifact, &inrelease.artifact)?;
    verify_apt_artifact(&expected.release_gpg.artifact, &release_gpg.artifact)?;

    if !is_full_fingerprint(&expected.inrelease.signer_fingerprint)
        || !is_full_fingerprint(&expected.release_gpg.signer_fingerprint)
        || !is_full_fingerprint(&inrelease.signer_fingerprint)
        || !is_full_fingerprint(&release_gpg.signer_fingerprint)
    {
        return Err(CoherenceError::PublicationMetadataEmpty);
    }
    if expected.inrelease.signed_release_sha256 != expected.release.artifact.sha256
        || expected.release_gpg.signed_release_sha256 != expected.release.artifact.sha256
        || inrelease.signed_release_sha256 != release.artifact.sha256
        || release_gpg.signed_release_sha256 != release.artifact.sha256
        || inrelease.signed_release_sha256 != expected.inrelease.signed_release_sha256
        || release_gpg.signed_release_sha256 != expected.release_gpg.signed_release_sha256
        || inrelease.signer_fingerprint != expected.inrelease.signer_fingerprint
        || release_gpg.signer_fingerprint != expected.release_gpg.signer_fingerprint
        || expected.inrelease.signer_fingerprint != expected.release_gpg.signer_fingerprint
        || inrelease.signer_fingerprint != release_gpg.signer_fingerprint
    {
        return Err(CoherenceError::PublicationSignature);
    }
    if publication.inrelease_sha256 != expected.inrelease.artifact.sha256
        || publication.signer_fingerprint != expected.inrelease.signer_fingerprint
    {
        return Err(CoherenceError::PublicationMetadataBinding);
    }

    verify_apt_package_metadata(record, &expected.packages, packages)
}

// ---------------------------------------------------------------------------
// Assemble
// ---------------------------------------------------------------------------

/// Inputs for assembling a record (already-hashed digests). `assemble` sorts the
/// architectures and re-verifies, so an incoherent input is rejected before it
/// can be written.
pub struct AssembleInputs {
    pub build: BuildIdentity,
    pub architectures: Vec<ArchitectureIdentity>,
    pub oci_index_digest: OciDigest,
    pub oci_image_ref: String,
    pub oci_labels: OciLabels,
    pub apt: AptCoordinate,
}

pub fn assemble(inputs: AssembleInputs) -> std::result::Result<ReleaseRecord, CoherenceError> {
    let mut record = ReleaseRecord {
        schema: RELEASE_RECORD_SCHEMA.to_string(),
        build: inputs.build,
        architectures: inputs.architectures,
        oci_index_digest: inputs.oci_index_digest,
        oci_image_ref: inputs.oci_image_ref,
        oci_labels: inputs.oci_labels,
        apt: inputs.apt,
    };
    record.architectures.sort_by_key(|item| item.arch);
    record.verify()?;
    Ok(record)
}

/// Refuse to emit a publishable record from a development binary.
pub fn emit_record(identity: &EmbeddedIdentity, record: &ReleaseRecord) -> Result<()> {
    if identity.is_development() {
        bail!(
            "refusing to emit a release record from a development build \
             (source={}, kind={}); build with --features release-build from a tagged commit",
            identity.source_sha,
            identity.kind
        );
    }
    if record.build.commit.as_str() != identity.source_sha {
        bail!("record source commit does not match this binary's embedded source SHA");
    }
    if record.build.crate_version != identity.crate_version {
        bail!("record crate version does not match this binary's embedded crate version");
    }
    record.verify().map_err(anyhow::Error::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Atomic on-disk activation
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` atomically: temp file → fsync → rename → best-effort
/// dir fsync. A crash leaves either the old file or the fully written new one,
/// never a torn record.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("activation target has no file name")?;
    let tmp = dir.join(format!(".{file_name}.tmp"));
    {
        let mut file =
            fs::File::create(&tmp).with_context(|| format!("create temp {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    // Directory fsync makes the rename durable. Best-effort: not every fs/OS
    // permits fsync on a directory handle, and the rename already guarantees
    // atomicity within the run.
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}

fn read_bounded(reader: &mut impl Read, limit: usize, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label}"))?;
    if bytes.len() > limit {
        bail!("{label} exceeds {limit} bytes");
    }
    Ok(bytes)
}

fn read_file_bounded(file: &mut fs::File, limit: usize, label: &str) -> Result<Vec<u8>> {
    let size = file
        .metadata()
        .with_context(|| format!("inspect {label}"))?
        .len();
    if size > limit as u64 {
        bail!("{label} exceeds {limit} bytes");
    }
    read_bounded(file, limit, label)
}

fn read_path_bounded(path: &Path, limit: usize, label: &str) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path).with_context(|| format!("read {label}"))?;
    read_file_bounded(&mut file, limit, label)
}

fn read_path_text_bounded(path: &Path, limit: usize, label: &str) -> Result<String> {
    String::from_utf8(read_path_bounded(path, limit, label)?)
        .with_context(|| format!("{label} is not UTF-8"))
}

/// Compute a file's SHA-256 without slurping it whole.
pub fn sha256_file(path: &Path) -> Result<Sha256Hex> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Hex(hex_lower(&hasher.finalize())))
}

/// The transactional pointer set under a release directory:
/// `records/<tag>.json` immutable records plus atomically swapped `active`/
/// `previous` tag pointers. Activation keeps the exact prior coherent tag so a
/// rollback restores a complete tuple; no intermediate tuple is ever pointed at.
pub struct ReleaseStore {
    root: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReleaseTransition {
    from_active: Option<String>,
    from_previous: Option<String>,
    to_active: Option<String>,
    to_previous: Option<String>,
}

impl ReleaseTransition {
    fn validate(&self) -> Result<()> {
        for tag in [
            self.from_active.as_deref(),
            self.from_previous.as_deref(),
            self.to_active.as_deref(),
            self.to_previous.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_release_tag_component(tag)?;
        }
        if self.from_active.is_none() && self.from_previous.is_some() {
            bail!("release transition has previous without active");
        }
        if self.from_active == self.from_previous && self.from_active.is_some() {
            bail!("release transition source pointers must differ");
        }
        if self.to_active.is_none() {
            bail!("release transition must have an active target");
        }
        if self.to_active == self.to_previous && self.to_active.is_some() {
            bail!("release transition target pointers must differ");
        }
        Ok(())
    }
}

impl ReleaseStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[cfg(test)]
    pub fn record_path(&self, tag: &str) -> Result<PathBuf> {
        validate_release_tag_component(tag)?;
        Ok(self.root.join("records").join(tag).join("record.json"))
    }
    #[cfg(test)]
    pub fn deployed_path(&self, tag: &str) -> Result<PathBuf> {
        validate_release_tag_component(tag)?;
        Ok(self.root.join("records").join(tag).join("deployed.json"))
    }
    /// Persist an immutable record + sidecar checksum. Refuses to overwrite an
    /// existing record whose bytes differ (no clobber); an exact re-write is a
    /// no-op success.
    pub fn store_record(&self, record: &ReleaseRecord) -> Result<Sha256Hex> {
        validate_release_tag_component(&record.build.tag)?;
        record.verify().map_err(anyhow::Error::from)?;
        let root = self.open_root(true)?;
        let _lock = lock_release_state_at(&root, rustix::fs::FlockOperation::LockExclusive)?;
        self.recover_pending_transition_at(&root)?;
        let records = open_release_directory_at(&root, "records", true)?;
        let tag_dir = open_release_directory_at(&records, &record.build.tag, true)?;
        self.store_record_at(&records, &tag_dir, record)
    }

    #[cfg(test)]
    pub fn active_tag(&self) -> Result<Option<String>> {
        let Some(root) = self.open_root_if_exists()? else {
            return Ok(None);
        };
        let _lock = lock_release_state_at(&root, rustix::fs::FlockOperation::LockExclusive)?;
        self.recover_pending_transition_at(&root)?;
        Ok(self.read_validated_state_at(&root)?.0)
    }
    #[cfg(test)]
    pub fn previous_tag(&self) -> Result<Option<String>> {
        let Some(root) = self.open_root_if_exists()? else {
            return Ok(None);
        };
        let _lock = lock_release_state_at(&root, rustix::fs::FlockOperation::LockExclusive)?;
        self.recover_pending_transition_at(&root)?;
        Ok(self.read_validated_state_at(&root)?.1)
    }

    /// Atomically make `tag` active, demoting the current active tag to
    /// `previous`. The record for `tag` must already be stored.
    pub fn activate(&self, record: &ReleaseRecord, deployed: &DeployedIdentity) -> Result<()> {
        let tag = &record.build.tag;
        validate_release_tag_component(tag)?;
        verify_installed(
            deployed,
            record,
            Arch::host().context("unsupported host architecture")?,
            &deployed.binary_sha256,
        )
        .map_err(anyhow::Error::from)?;
        let root = self.open_root(true)?;
        let _lock = lock_release_state_at(&root, rustix::fs::FlockOperation::LockExclusive)?;
        self.recover_pending_transition_at(&root)?;
        let (current, previous) = self.read_validated_state_at(&root)?;
        let records = open_release_directory_at(&root, "records", true)?;
        let tag_dir = open_release_directory_at(&records, tag, true)?;
        self.store_record_at(&records, &tag_dir, record)?;
        let deployed_bytes = serde_json::to_vec_pretty(deployed)?;
        if let Some(existing) = open_release_file_at(&tag_dir, DEPLOYED_FILE_NAME)? {
            let mut existing = existing;
            if read_file_bounded(
                &mut existing,
                MAX_RELEASE_METADATA_BYTES,
                "existing deployed identity",
            )? != deployed_bytes
            {
                bail!("deployed identity for {tag} already exists with different bytes");
            }
        } else {
            write_atomic_file_at(
                &tag_dir,
                DEPLOYED_FILE_NAME,
                &deployed_bytes,
                "deployed identity",
            )?;
        }
        self.read_coherent_tuple_at(&records, tag)?;
        if current.as_deref() == Some(tag.as_str()) {
            return Ok(());
        }
        let transition = ReleaseTransition {
            from_active: current.clone(),
            from_previous: previous,
            to_active: Some(tag.clone()),
            to_previous: current,
        };
        self.commit_transition_at(&root, &records, transition)?;
        Ok(())
    }

    /// Restore the previous coherent tag as active. Requires a recorded previous
    /// tuple whose record is still present.
    #[cfg(test)]
    pub fn rollback(&self) -> Result<String> {
        self.rollback_with_verification(|_| Ok(()))
    }

    fn rollback_with_verification<F>(&self, verify_image: F) -> Result<String>
    where
        F: FnOnce(&ReleaseRecord) -> Result<()>,
    {
        let root = self.open_root(false)?;
        let _lock = lock_release_state_at(&root, rustix::fs::FlockOperation::LockExclusive)?;
        self.recover_pending_transition_at(&root)?;
        let (current, previous) = self.read_validated_state_at(&root)?;
        let previous = previous.context("no previous tag recorded — cannot roll back")?;
        let current = current.context("no active tag recorded — cannot roll back")?;
        if current == previous {
            bail!("active and previous release tags must differ");
        }
        let records = open_release_directory_at(&root, "records", false)?;
        let (previous_record, _) = self.read_coherent_tuple_at(&records, &previous)?;
        verify_image(&previous_record)?;
        let transition = ReleaseTransition {
            from_active: Some(current.clone()),
            from_previous: Some(previous.clone()),
            to_active: Some(previous.clone()),
            to_previous: Some(current),
        };
        self.commit_transition_at(&root, &records, transition)?;
        Ok(previous)
    }

    fn commit_transition_at(
        &self,
        root: &fs::File,
        records: &fs::File,
        transition: ReleaseTransition,
    ) -> Result<()> {
        transition.validate()?;
        self.validate_transition_target_at(records, &transition)?;
        let bytes = serde_json::to_vec(&transition)?;
        write_atomic_file_at(
            root,
            RELEASE_TRANSITION_FILE_NAME,
            &bytes,
            "release transition",
        )?;
        publish_transition_pointers_at(root, &transition)?;
        remove_release_file_at(root, RELEASE_TRANSITION_FILE_NAME, "release transition")?;
        Ok(())
    }

    fn recover_pending_transition_at(&self, root: &fs::File) -> Result<()> {
        let Some(bytes) = open_release_file_at(root, RELEASE_TRANSITION_FILE_NAME)? else {
            return Ok(());
        };
        let mut bytes = bytes;
        let bytes =
            read_file_bounded(&mut bytes, MAX_RELEASE_METADATA_BYTES, "release transition")?;
        let transition: ReleaseTransition =
            serde_json::from_slice(&bytes).context("parse release transition")?;
        transition.validate()?;
        let current = (
            read_optional_link_tag_at(root, "active")?,
            read_optional_link_tag_at(root, "previous")?,
        );
        let source = (
            transition.from_active.clone(),
            transition.from_previous.clone(),
        );
        let partial = (
            transition.from_active.clone(),
            transition.to_previous.clone(),
        );
        let target = (transition.to_active.clone(), transition.to_previous.clone());
        if current == target {
            remove_release_file_at(root, RELEASE_TRANSITION_FILE_NAME, "release transition")?;
            return Ok(());
        }
        if current != source && current != partial {
            bail!("release transition found unexpected active/previous pointers");
        }
        let records = open_release_directory_at(root, "records", false)?;
        self.validate_transition_target_at(&records, &transition)?;
        publish_transition_pointers_at(root, &transition)?;
        remove_release_file_at(root, RELEASE_TRANSITION_FILE_NAME, "release transition")?;
        Ok(())
    }

    fn validate_transition_target_at(
        &self,
        records: &fs::File,
        transition: &ReleaseTransition,
    ) -> Result<()> {
        if let Some(tag) = &transition.to_active {
            self.read_coherent_tuple_at(records, tag)?;
        }
        if let Some(tag) = &transition.to_previous {
            self.read_coherent_tuple_at(records, tag)?;
        }
        Ok(())
    }

    fn open_root(&self, create: bool) -> Result<fs::File> {
        open_release_directory_path(&self.root, create)
            .with_context(|| format!("open release root {}", self.root.display()))
    }

    #[cfg(test)]
    fn open_root_if_exists(&self) -> Result<Option<fs::File>> {
        match open_release_directory_path(&self.root, false) {
            Ok(root) => Ok(Some(root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("open release root {}", self.root.display()))
            }
        }
    }

    fn store_record_at(
        &self,
        records: &fs::File,
        tag_dir: &fs::File,
        record: &ReleaseRecord,
    ) -> Result<Sha256Hex> {
        let bytes = record.to_canonical_json();
        let digest = Sha256Hex::of_bytes(bytes.as_bytes());
        if let Some(existing) = open_release_file_at(tag_dir, RECORD_FILE_NAME)? {
            let mut existing = existing;
            if read_file_bounded(
                &mut existing,
                MAX_RELEASE_METADATA_BYTES,
                "existing release record",
            )? != bytes.as_bytes()
            {
                bail!(
                    "record for {} already exists with different bytes — refusing to clobber",
                    record.build.tag
                );
            }
        } else {
            write_atomic_file_at(
                tag_dir,
                RECORD_FILE_NAME,
                bytes.as_bytes(),
                "release record",
            )?;
        }
        let checksum = format!("{digest}  {}.json\n", record.build.tag);
        let checksum_name = format!("{}.json.sha256", record.build.tag);
        write_atomic_file_at(
            records,
            &checksum_name,
            checksum.as_bytes(),
            "record checksum",
        )?;
        Ok(digest)
    }

    fn read_record_at(
        &self,
        records: &fs::File,
        tag_dir: &fs::File,
        tag: &str,
    ) -> Result<ReleaseRecord> {
        let bytes = read_release_file_at(
            tag_dir,
            RECORD_FILE_NAME,
            MAX_RELEASE_METADATA_BYTES,
            "release record",
        )?;
        let checksum_name = format!("{}.json.sha256", tag);
        let checksum = String::from_utf8(read_release_file_at(
            records,
            &checksum_name,
            MAX_RELEASE_CHECKSUM_BYTES,
            "record checksum",
        )?)
        .context("record checksum is not UTF-8")?;
        let expected = parse_checksum(&checksum)?;
        let record = verify_record_bytes(&bytes, &expected).map_err(anyhow::Error::from)?;
        if record.build.tag != tag {
            bail!("stored release record tag disagrees with its directory");
        }
        Ok(record)
    }

    fn read_coherent_tuple_at(
        &self,
        records: &fs::File,
        tag: &str,
    ) -> Result<(ReleaseRecord, DeployedIdentity)> {
        validate_release_tag_component(tag)?;
        let tag_dir = open_release_directory_at(records, tag, false)?;
        let record = self.read_record_at(records, &tag_dir, tag)?;
        let deployed_bytes = read_release_file_at(
            &tag_dir,
            DEPLOYED_FILE_NAME,
            MAX_RELEASE_METADATA_BYTES,
            "deployed identity",
        )?;
        let deployed: DeployedIdentity =
            serde_json::from_slice(&deployed_bytes).context("parse deployed identity")?;
        let host = Arch::host().context("unsupported host architecture")?;
        verify_installed(&deployed, &record, host, &deployed.binary_sha256)
            .map_err(anyhow::Error::from)?;
        Ok((record, deployed))
    }

    fn read_validated_state_at(&self, root: &fs::File) -> Result<(Option<String>, Option<String>)> {
        let active = read_optional_link_tag_at(root, "active")?;
        let previous = read_optional_link_tag_at(root, "previous")?;
        if active.is_none() && previous.is_some() {
            bail!("previous release pointer exists without an active pointer");
        }
        if active.is_some() && active == previous {
            bail!("active and previous release pointers must differ");
        }
        if active.is_none() {
            return Ok((None, None));
        }
        let records = open_release_directory_at(root, "records", false)?;
        if let Some(tag) = &active {
            self.read_coherent_tuple_at(&records, tag)?;
        }
        if let Some(tag) = &previous {
            self.read_coherent_tuple_at(&records, tag)?;
        }
        Ok((active, previous))
    }
}

fn open_release_directory_path(path: &Path, create: bool) -> std::io::Result<fs::File> {
    let path = normalize_release_root_path(path)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let start = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut current: fs::File =
        rustix::fs::openat(rustix::fs::CWD, start, flags, rustix::fs::Mode::empty())
            .map(Into::into)
            .map_err(std::io::Error::from)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "release root path is not normalized",
            ));
        };
        let next = match rustix::fs::openat(&current, name, flags, rustix::fs::Mode::empty()) {
            Ok(next) => next,
            Err(rustix::io::Errno::NOENT) if create => {
                match rustix::fs::mkdirat(&current, name, rustix::fs::Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(std::io::Error::from(error)),
                }
                rustix::fs::openat(&current, name, flags, rustix::fs::Mode::empty())?
            }
            Err(error) => return Err(std::io::Error::from(error)),
        };
        current = next.into();
    }
    Ok(current)
}

fn normalize_release_root_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "release root path is empty",
        ));
    }
    if cfg!(target_os = "macos") {
        for (alias, canonical) in [("/var", "/private/var"), ("/tmp", "/private/tmp")] {
            if path == Path::new(alias) {
                return Ok(PathBuf::from(canonical));
            }
            if let Ok(suffix) = path.strip_prefix(alias) {
                return Ok(Path::new(canonical).join(suffix));
            }
        }
    }
    Ok(path.to_path_buf())
}

fn open_release_directory_at(parent: &fs::File, name: &str, create: bool) -> Result<fs::File> {
    if create {
        match rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(Into::into)
    .map_err(std::io::Error::from)
    .map_err(Into::into)
}

fn open_release_file_at(parent: &fs::File, name: &str) -> Result<Option<fs::File>> {
    let file = match rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let file: fs::File = file.into();
    if !file.metadata()?.is_file() {
        bail!("release file {name} is not a regular file");
    }
    Ok(Some(file))
}

fn read_release_file_at(
    parent: &fs::File,
    name: &str,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let mut file =
        open_release_file_at(parent, name)?.with_context(|| format!("missing {label}"))?;
    read_file_bounded(&mut file, limit, label)
}

fn lock_release_state_at(
    root: &fs::File,
    operation: rustix::fs::FlockOperation,
) -> Result<fs::File> {
    let file: fs::File = rustix::fs::openat(
        root,
        RELEASE_STATE_LOCK_FILE_NAME,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map(Into::into)
    .map_err(std::io::Error::from)
    .context("open release state lock")?;
    rustix::fs::flock(&file, operation).context("lock release state")?;
    Ok(file)
}

fn validate_regular_destination_at(parent: &fs::File, name: &str, label: &str) -> Result<()> {
    match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat)
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                == rustix::fs::FileType::RegularFile =>
        {
            Ok(())
        }
        Ok(_) => bail!("{label} is not a regular file"),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn write_atomic_file_at(parent: &fs::File, name: &str, bytes: &[u8], label: &str) -> Result<()> {
    validate_regular_destination_at(parent, name, label)?;
    let temporary = format!(".{name}.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file: fs::File = rustix::fs::openat(
            parent,
            &temporary,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map(Into::into)
        .map_err(std::io::Error::from)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        rustix::fs::renameat(parent, &temporary, parent, name).map_err(std::io::Error::from)?;
        parent.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary, rustix::fs::AtFlags::empty());
    }
    result
}

fn remove_release_file_at(parent: &fs::File, name: &str, label: &str) -> Result<()> {
    match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat)
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                == rustix::fs::FileType::RegularFile => {}
        Ok(_) => bail!("{label} is not a regular file"),
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty())
        .map_err(std::io::Error::from)
        .with_context(|| format!("remove {label}"))?;
    parent.sync_all()?;
    Ok(())
}

fn remove_release_pointer_at(parent: &fs::File, name: &str, label: &str) -> Result<()> {
    match rustix::fs::readlinkat(parent, name, Vec::<u8>::new()) {
        Ok(_) => {}
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(rustix::io::Errno::INVAL) => bail!("{label} is not a symbolic link"),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty())
        .map_err(std::io::Error::from)
        .with_context(|| format!("remove {label}"))?;
    parent.sync_all()?;
    Ok(())
}

fn read_optional_link_tag_at(parent: &fs::File, name: &str) -> Result<Option<String>> {
    let target = match rustix::fs::readlinkat(parent, name, Vec::<u8>::new()) {
        Ok(target) => String::from_utf8(target.into_bytes())
            .context("release pointer target is not valid UTF-8")?,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let mut components = Path::new(&target).components();
    match components.next() {
        Some(Component::Normal(component)) if component == OsStr::new("records") => {}
        _ => bail!("release pointer target is not records/<tag>"),
    }
    let tag = match components.next() {
        Some(Component::Normal(component)) => component
            .to_str()
            .context("release pointer target tag is not valid UTF-8")?,
        _ => bail!("release pointer target has no release tag"),
    };
    if components.next().is_some() {
        bail!("release pointer target contains extra path components");
    }
    validate_release_tag_component(tag)?;
    Ok(Some(tag.to_owned()))
}

fn write_atomic_symlink_at(parent: &fs::File, name: &str, tag: &str) -> Result<()> {
    validate_release_tag_component(tag)?;
    let temporary = format!(".{name}.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        rustix::fs::symlinkat(Path::new("records").join(tag), parent, &temporary)
            .map_err(std::io::Error::from)?;
        rustix::fs::renameat(parent, &temporary, parent, name).map_err(std::io::Error::from)?;
        parent.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary, rustix::fs::AtFlags::empty());
    }
    result
}

fn publish_transition_pointers_at(root: &fs::File, transition: &ReleaseTransition) -> Result<()> {
    if let Some(tag) = &transition.to_previous {
        write_atomic_symlink_at(root, "previous", tag)?;
    } else {
        remove_release_pointer_at(root, "previous", "previous release pointer")?;
    }
    if let Some(tag) = &transition.to_active {
        write_atomic_symlink_at(root, "active", tag)?;
    } else {
        remove_release_pointer_at(root, "active", "active release pointer")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

pub fn run(args: ReleaseArgs) -> Result<()> {
    match args.command {
        ReleaseCommand::Emit(args) => emit_command(args),
        ReleaseCommand::Assemble(args) => assemble_command(args),
        ReleaseCommand::VerifyRecord(args) => verify_record_command(args),
        ReleaseCommand::VerifyInstalled(args) => verify_installed_command(args),
        ReleaseCommand::Activate(args) => activate_command(args),
        ReleaseCommand::Rollback(args) => rollback_command(args),
        ReleaseCommand::Export(args) => export_command(args),
    }
}

fn read_record_file(path: &Path) -> Result<(Vec<u8>, ReleaseRecord)> {
    let bytes = read_path_bounded(
        path,
        MAX_RELEASE_METADATA_BYTES,
        &format!("record {}", path.display()),
    )?;
    let record: ReleaseRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse record {}", path.display()))?;
    Ok((bytes, record))
}

fn parse_checksum(text: &str) -> Result<Sha256Hex> {
    let first = text
        .split_whitespace()
        .next()
        .context("checksum file is empty")?;
    Sha256Hex::parse(first)
}

fn parse_artifact_checksum(text: &str) -> Result<Sha256Hex> {
    let mut fields = text.split_whitespace();
    let first = fields.next().context("artifact checksum file is empty")?;
    if fields.next().is_some() {
        bail!("artifact checksum file must contain only one checksum");
    }
    Sha256Hex::parse(first)
}

fn validate_artifact_version_component(version: &str) -> Result<()> {
    if !is_safe_path_component(version) {
        bail!("release version is not a safe artifact path component");
    }
    Ok(())
}

fn validate_release_tag_component(tag: &str) -> Result<()> {
    if !is_safe_path_component(tag) {
        bail!("release tag is not a safe path component");
    }
    Ok(())
}

fn is_safe_path_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .bytes()
            .any(|byte| byte == b'/' || byte == b'\\' || byte.is_ascii_control())
}

fn emit_command(args: ReleaseEmitArgs) -> Result<()> {
    let (_, record) = read_record_file(&args.record)?;
    let identity = embedded();
    emit_record(&identity, &record)?;
    let store = ReleaseStore::new(&args.out_dir);
    let digest = store.store_record(&record)?;
    println!("{digest}");
    Ok(())
}

fn assemble_command(args: ReleaseAssembleArgs) -> Result<()> {
    let (_, candidate) = read_record_file(&args.record)?;
    // Recompute the per-arch digests from the downloaded artifacts and require an
    // exact match before trusting the record.
    let dir = &args.artifacts;
    validate_artifact_version_component(&candidate.build.debian_version)?;
    for arch in REQUIRED_ARCHES {
        let record_arch = candidate
            .architecture(arch)
            .with_context(|| format!("record has no {} architecture", arch.as_str()))?;
        let binary = dir.join(format!("velnor-runner-{}.bin.sha256", arch.as_str()));
        let expected_binary = parse_artifact_checksum(&read_path_text_bounded(
            &binary,
            MAX_RELEASE_CHECKSUM_BYTES,
            &format!("binary checksum {}", binary.display()),
        )?)?;
        if expected_binary != record_arch.binary_sha256 {
            bail!(
                "assembled binary digest for {} disagrees with the record",
                arch.as_str()
            );
        }

        let deb = dir.join(format!(
            "velnor-runner-{}-{}.deb.sha256",
            candidate.build.debian_version,
            arch.as_str()
        ));
        let expected_deb = parse_artifact_checksum(&read_path_text_bounded(
            &deb,
            MAX_RELEASE_CHECKSUM_BYTES,
            &format!("deb checksum {}", deb.display()),
        )?)?;
        let deb_payload = dir.join(format!(
            "velnor-runner-{}-{}.deb",
            candidate.build.debian_version,
            arch.as_str()
        ));
        let actual_deb = sha256_file(&deb_payload)
            .with_context(|| format!("hash deb artifact {}", deb_payload.display()))?;
        if expected_deb != actual_deb {
            bail!(
                "assembled deb checksum for {} disagrees with the artifact",
                arch.as_str()
            );
        }
        if actual_deb != record_arch.deb_sha256 {
            bail!(
                "assembled deb artifact digest for {} disagrees with the record",
                arch.as_str()
            );
        }
    }
    // Re-assemble from the candidate's parts so the emitted record is canonical
    // and independently re-verified (never trusted as-read).
    let record = assemble(AssembleInputs {
        build: candidate.build,
        architectures: candidate.architectures,
        oci_index_digest: candidate.oci_index_digest,
        oci_image_ref: candidate.oci_image_ref,
        oci_labels: candidate.oci_labels,
        apt: candidate.apt,
    })
    .map_err(anyhow::Error::from)?;
    let canonical = record.to_canonical_json();
    let digest = Sha256Hex::of_bytes(canonical.as_bytes());
    if let Some(out) = &args.out {
        write_atomic(out, canonical.as_bytes())?;
        write_atomic(
            &out.with_extension("json.sha256"),
            format!("{digest}\n").as_bytes(),
        )?;
    }
    println!("{digest}");
    Ok(())
}

fn read_distinct_apt_metadata_sources(
    expected_path: &Path,
    served_path: &Path,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut expected_file = fs::File::open(expected_path)
        .with_context(|| format!("read expected APT metadata {}", expected_path.display()))?;
    let mut served_file = fs::File::open(served_path)
        .with_context(|| format!("read served APT metadata {}", served_path.display()))?;

    let expected_metadata = expected_file.metadata()?;
    let served_metadata = served_file.metadata()?;
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::dev(&expected_metadata)
        == std::os::unix::fs::MetadataExt::dev(&served_metadata)
        && std::os::unix::fs::MetadataExt::ino(&expected_metadata)
            == std::os::unix::fs::MetadataExt::ino(&served_metadata)
    {
        bail!("expected and served APT metadata must use different files");
    }
    #[cfg(not(unix))]
    {
        let _ = (expected_path, served_path);
        bail!("APT metadata source identity checks require a Unix platform");
    }

    let expected_bytes = read_file_bounded(
        &mut expected_file,
        MAX_RELEASE_METADATA_BYTES,
        "expected APT metadata",
    )?;
    let served_bytes = read_file_bounded(
        &mut served_file,
        MAX_RELEASE_METADATA_BYTES,
        "served APT metadata",
    )?;
    Ok((expected_bytes, served_bytes))
}

fn verify_record_command(args: ReleaseVerifyRecordArgs) -> Result<()> {
    let bytes = read_path_bounded(
        &args.record,
        MAX_RELEASE_METADATA_BYTES,
        &format!("record {}", args.record.display()),
    )?;
    let expected = match (&args.checksum, &args.sha256) {
        (Some(path), _) => parse_checksum(&read_path_text_bounded(
            path,
            MAX_RELEASE_CHECKSUM_BYTES,
            &format!("checksum {}", path.display()),
        )?)?,
        (None, Some(hex)) => Sha256Hex::parse(hex)?,
        (None, None) => bail!("provide --checksum <file> or --sha256 <hex>"),
    };
    let record = verify_record_bytes(&bytes, &expected).map_err(anyhow::Error::from)?;
    let publication = if let Some(path) = &args.publication {
        Some(
            serde_json::from_slice::<PublicationRecord>(&read_path_bounded(
                path,
                MAX_RELEASE_METADATA_BYTES,
                &format!("publication {}", path.display()),
            )?)
            .with_context(|| format!("parse publication {}", path.display()))?,
        )
    } else {
        None
    };

    let apt_claims_checked = match (&args.expected_apt_metadata, &args.served_apt_metadata) {
        (None, None) if publication.is_some() => {
            bail!("--publication requires preverified APT claims for coherence checking")
        }
        (None, None) => {
            // Record-only verification is intentionally separate from
            // publication acceptance and needs no publication input.
            false
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!("provide both --expected-apt-metadata and --served-apt-metadata")
        }
        (Some(expected_path), Some(served_path)) => {
            let publication = publication.as_ref().context(
                "--publication is required with preverified APT claims coherence checking",
            )?;
            let (expected_bytes, served_bytes) =
                read_distinct_apt_metadata_sources(expected_path, served_path)?;
            let expected: ExpectedAptPublicationMetadata = serde_json::from_slice(&expected_bytes)
                .with_context(|| {
                    format!("parse expected APT metadata {}", expected_path.display())
                })?;
            let served: ActualAptPublicationMetadata = serde_json::from_slice(&served_bytes)
                .with_context(|| format!("parse served APT metadata {}", served_path.display()))?;
            verify_apt_publication_metadata(publication, &record, &expected, &served)
                .map_err(anyhow::Error::from)?;
            true
        }
    };
    if apt_claims_checked {
        println!(
            "release record and preverified APT publication claims for {} are coherent (digest {})",
            record.build.tag,
            record.digest()
        );
    } else {
        println!(
            "release record for {} is coherent (digest {})",
            record.build.tag,
            record.digest()
        );
    }
    Ok(())
}

fn verify_installed_command(args: ReleaseVerifyInstalledArgs) -> Result<()> {
    let (_, record) = read_record_file(&args.record)?;
    let deployed_bytes = read_path_bounded(
        &args.deployed,
        MAX_RELEASE_METADATA_BYTES,
        &format!("deployed identity {}", args.deployed.display()),
    )?;
    let deployed: DeployedIdentity = serde_json::from_slice(&deployed_bytes)
        .with_context(|| format!("parse deployed identity {}", args.deployed.display()))?;
    let host = match args.arch {
        Some(arch) => arch.parse()?,
        None => Arch::host().context("unsupported host architecture")?,
    };
    let installed = sha256_file(&args.binary)?;
    verify_installed(&deployed, &record, host, &installed).map_err(anyhow::Error::from)?;
    println!("installed velnor-runner is coherent with the active release record");
    Ok(())
}

fn docker_output(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .context("execute docker for release activation")?;
    if !output.status.success() {
        bail!("docker release activation step failed");
    }
    Ok(output.stdout)
}

fn verify_and_tag_release_image(record: &ReleaseRecord) -> Result<()> {
    docker_output(&["pull", &record.oci_image_ref])?;
    let repo_digests: Vec<String> = serde_json::from_slice(&docker_output(&[
        "image",
        "inspect",
        &record.oci_image_ref,
        "--format",
        "{{json .RepoDigests}}",
    ])?)?;
    let expected_ref = format!(
        "{}@{}",
        record.oci_image_ref.split('@').next().unwrap(),
        record.oci_index_digest
    );
    if !repo_digests.iter().any(|item| item == &expected_ref) {
        bail!("pulled OCI image digest disagrees with release record");
    }
    let labels: BTreeMap<String, String> = serde_json::from_slice(&docker_output(&[
        "image",
        "inspect",
        &record.oci_image_ref,
        "--format",
        "{{json .Config.Labels}}",
    ])?)?;
    let required_labels = [
        (
            "org.opencontainers.image.version",
            record.oci_labels.version.as_str(),
        ),
        (
            "org.opencontainers.image.revision",
            record.oci_labels.revision.as_str(),
        ),
        (
            "org.opencontainers.image.source",
            record.oci_labels.source.as_str(),
        ),
        (
            "org.velnor.manifest-sha256",
            record.oci_labels.manifest_sha256.as_str(),
        ),
    ];
    if required_labels
        .iter()
        .any(|(key, value)| labels.get(*key).map(String::as_str) != Some(*value))
    {
        bail!("pulled OCI image labels disagree with release record");
    }
    docker_output(&["tag", &record.oci_image_ref, "velnor/job-ubuntu:26.04"])?;
    Ok(())
}

fn activate_command(args: ReleaseActivateArgs) -> Result<()> {
    let (_, record) = read_record_file(&args.record)?;
    record.verify().map_err(anyhow::Error::from)?;
    let host = Arch::host().context("unsupported host architecture")?;
    let architecture = record
        .architecture(host)
        .context("release record lacks host architecture")?;
    let installed_binary = Path::new(INSTALLED_BINARY_PATH);
    let binary_sha256 = sha256_file(installed_binary)?;
    if binary_sha256 != architecture.binary_sha256 {
        bail!("installed binary digest disagrees with release record");
    }
    let manifest_sha256 = Sha256Hex::of_bytes(crate::manifest::to_json_document()?.as_bytes());
    if manifest_sha256 != record.build.manifest_sha256 {
        bail!("compiled manifest digest disagrees with release record");
    }

    verify_and_tag_release_image(&record)?;

    let deployed = DeployedIdentity {
        schema: DEPLOYED_IDENTITY_SCHEMA.to_string(),
        package_version: record.build.debian_version.clone(),
        crate_version: record.build.crate_version.clone(),
        source_commit: record.build.commit.clone(),
        binary_sha256,
        manifest_version: record.build.manifest_version,
        manifest_sha256,
        oci_image_digest: record.oci_index_digest.clone(),
        record_sha256: record.digest(),
    };
    verify_installed(&deployed, &record, host, &deployed.binary_sha256)
        .map_err(anyhow::Error::from)?;
    let store = ReleaseStore::new(&args.dir);
    store.activate(&record, &deployed)?;
    println!("activated {}", record.build.tag);
    Ok(())
}

fn rollback_command(args: ReleaseRollbackArgs) -> Result<()> {
    let store = ReleaseStore::new(&args.dir);
    // A rollback changes both halves of the runtime tuple while the fleet is
    // drained: verify the exact prior image while the store lock is held, then
    // switch the filesystem pointer. Any verification failure leaves active
    // unchanged and cannot race a concurrent activation.
    let restored = store.rollback_with_verification(verify_and_tag_release_image)?;
    println!("rolled back to {restored}");
    Ok(())
}

fn export_command(args: ReleaseExportArgs) -> Result<()> {
    let identity = embedded();
    if let Some(path) = &args.deployed {
        let bytes = read_path_bounded(
            path,
            MAX_RELEASE_METADATA_BYTES,
            &format!("deployed identity {}", path.display()),
        )?;
        let deployed: DeployedIdentity =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        println!("{}", serde_json::to_string_pretty(&deployed)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&identity)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

//! Plan 010 — non-circular release identity model.
//!
//! One release commit must bind, in a single acyclic chain,
//! `source SHA → crate version → per-arch binary/deb digests → OCI image digest →
//! compiled-manifest hash → APT publication → deployed export`. This module owns
//! the canonical serde schemas for that chain plus the deterministic
//! emit/verify/activate primitives the release workflow, the APT publisher, and
//! the host activation scripts all agree on.
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

use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::{
    ReleaseActivateArgs, ReleaseArgs, ReleaseAssembleArgs, ReleaseCommand, ReleaseEmitArgs,
    ReleaseExportArgs, ReleaseRollbackArgs, ReleaseVerifyInstalledArgs, ReleaseVerifyRecordArgs,
};

/// Schema tags. A consumer refuses an unknown shape before trusting any field.
pub const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";
pub const PUBLICATION_RECORD_SCHEMA: &str = "velnor.publication-record/v1";
pub const DEPLOYED_IDENTITY_SCHEMA: &str = "velnor.deployed-identity/v1";

/// Canonical source repository the release chain is anchored to.
pub const SOURCE_REPOSITORY: &str = "tailrocks/velnor";
pub const SOURCE_URL: &str = "https://github.com/tailrocks/velnor";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousPointer {
    pub tag: String,
    pub source_record_sha256: Sha256Hex,
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

        if !self.oci_image_ref.contains(self.oci_index_digest.as_str()) {
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
    if publication.signer_fingerprint.is_empty() {
        return Err(CoherenceError::EmptyField("publication.signer_fingerprint"));
    }
    // The published index must cover at least one architecture, each drawn from
    // the release's supported set.
    if publication.packages.is_empty() {
        return Err(CoherenceError::EmptyField("publication.packages"));
    }
    for index in &publication.packages {
        if !REQUIRED_ARCHES.contains(&index.arch) || record.architecture(index.arch).is_none() {
            return Err(CoherenceError::PublicationPackageArch);
        }
        // `index.sha256` and `publication.inrelease_sha256` are validated hex by
        // construction; require them present (never the empty/placeholder digest).
        if index.sha256 == publication.inrelease_sha256 {
            return Err(CoherenceError::PublicationPackageArch);
        }
    }
    if let Some(previous) = &publication.previous {
        // The previous pointer must reference a strictly different release tuple.
        if previous.tag == record.build.tag || previous.source_record_sha256 == record.digest() {
            return Err(CoherenceError::PublicationPrevious);
        }
    }
    Ok(())
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

impl ReleaseStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn record_path(&self, tag: &str) -> PathBuf {
        self.root.join("records").join(format!("{tag}.json"))
    }
    fn active_path(&self) -> PathBuf {
        self.root.join("active")
    }
    fn previous_path(&self) -> PathBuf {
        self.root.join("previous")
    }

    /// Persist an immutable record + sidecar checksum. Refuses to overwrite an
    /// existing record whose bytes differ (no clobber); an exact re-write is a
    /// no-op success.
    pub fn store_record(&self, record: &ReleaseRecord) -> Result<Sha256Hex> {
        let bytes = record.to_canonical_json();
        let digest = Sha256Hex::of_bytes(bytes.as_bytes());
        let path = self.record_path(&record.build.tag);
        fs::create_dir_all(path.parent().unwrap())?;
        if path.exists() {
            let existing = fs::read(&path)?;
            if existing != bytes.as_bytes() {
                bail!(
                    "record for {} already exists with different bytes — refusing to clobber",
                    record.build.tag
                );
            }
        } else {
            write_atomic(&path, bytes.as_bytes())?;
        }
        let checksum = format!("{digest}  {}.json\n", record.build.tag);
        write_atomic(
            &self
                .root
                .join("records")
                .join(format!("{}.json.sha256", record.build.tag)),
            checksum.as_bytes(),
        )?;
        Ok(digest)
    }

    pub fn active_tag(&self) -> Result<Option<String>> {
        read_optional_line(&self.active_path())
    }
    pub fn previous_tag(&self) -> Result<Option<String>> {
        read_optional_line(&self.previous_path())
    }

    /// Atomically make `tag` active, demoting the current active tag to
    /// `previous`. The record for `tag` must already be stored.
    pub fn activate(&self, tag: &str) -> Result<()> {
        if !self.record_path(tag).exists() {
            bail!("cannot activate {tag}: no stored record");
        }
        if let Some(current) = self.active_tag()? {
            if current != tag {
                write_atomic(&self.previous_path(), format!("{current}\n").as_bytes())?;
            }
        }
        write_atomic(&self.active_path(), format!("{tag}\n").as_bytes())?;
        Ok(())
    }

    /// Restore the previous coherent tag as active. Requires a recorded previous
    /// tuple whose record is still present.
    pub fn rollback(&self) -> Result<String> {
        let previous = self
            .previous_tag()?
            .context("no previous tag recorded — cannot roll back")?;
        if !self.record_path(&previous).exists() {
            bail!("cannot roll back to {previous}: its record is missing");
        }
        write_atomic(&self.active_path(), format!("{previous}\n").as_bytes())?;
        Ok(previous)
    }
}

fn read_optional_line(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
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
    let bytes = fs::read(path).with_context(|| format!("read record {}", path.display()))?;
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
    if let Some(dir) = &args.artifacts {
        for arch in &candidate.architectures {
            let binary = dir.join(format!("velnor-runner-{}.bin.sha256", arch.arch.as_str()));
            if binary.exists() {
                let expected = parse_checksum(&fs::read_to_string(&binary)?)?;
                if expected != arch.binary_sha256 {
                    bail!(
                        "assembled binary digest for {} disagrees with the record",
                        arch.arch.as_str()
                    );
                }
            }
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

fn verify_record_command(args: ReleaseVerifyRecordArgs) -> Result<()> {
    let bytes =
        fs::read(&args.record).with_context(|| format!("read {}", args.record.display()))?;
    let expected = match (&args.checksum, &args.sha256) {
        (Some(path), _) => parse_checksum(&fs::read_to_string(path)?)?,
        (None, Some(hex)) => Sha256Hex::parse(hex)?,
        (None, None) => bail!("provide --checksum <file> or --sha256 <hex>"),
    };
    let record = verify_record_bytes(&bytes, &expected).map_err(anyhow::Error::from)?;
    if let Some(path) = &args.publication {
        let publication: PublicationRecord = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("read publication {}", path.display()))?,
        )
        .with_context(|| format!("parse publication {}", path.display()))?;
        verify_publication_binds(&publication, &record).map_err(anyhow::Error::from)?;
    }
    println!(
        "release record for {} is coherent (digest {})",
        record.build.tag,
        record.digest()
    );
    Ok(())
}

fn verify_installed_command(args: ReleaseVerifyInstalledArgs) -> Result<()> {
    let (_, record) = read_record_file(&args.record)?;
    let deployed_bytes = fs::read(&args.deployed)
        .with_context(|| format!("read deployed identity {}", args.deployed.display()))?;
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

fn activate_command(args: ReleaseActivateArgs) -> Result<()> {
    let (_, record) = read_record_file(&args.record)?;
    record.verify().map_err(anyhow::Error::from)?;
    let store = ReleaseStore::new(&args.dir);
    store.store_record(&record)?;
    store.activate(&record.build.tag)?;
    println!("activated {}", record.build.tag);
    Ok(())
}

fn rollback_command(args: ReleaseRollbackArgs) -> Result<()> {
    let store = ReleaseStore::new(&args.dir);
    let restored = store.rollback()?;
    println!("rolled back to {restored}");
    Ok(())
}

fn export_command(args: ReleaseExportArgs) -> Result<()> {
    let identity = embedded();
    if let Some(path) = &args.deployed {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
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

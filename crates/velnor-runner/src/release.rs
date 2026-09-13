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
//!
//! ## Preview packages
//!
//! The rolling preview lane builds from an untagged main commit, so no
//! [`ReleaseRecord`] can exist for it: a stable record needs a `v*` tag and the
//! OCI image the preview lane never builds. Without a record, no installed
//! preview package could ever pass `release verify-installed`, and the package
//! units would refuse to start it (issue #673). A preview deb therefore ships
//! its own [`PackageRecord`] — `kind=preview`, bound to the exact source commit,
//! crate/debian version, compiled-manifest hash, and the runner binary digest of
//! the architecture the deb was built for. It records no deb digest and no OCI
//! digest, so packaging it inside the deb is acyclic. Emission still refuses a
//! development binary, and a preview record can only be emitted by a binary
//! whose embedded build kind is `preview`, i.e. one bound to that exact commit.
//! `verify-installed` applies the same installed-bytes coherence checks to both
//! record kinds.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
/// A deb's own source-derived identity record, shipped inside the deb at
/// `/usr/share/velnor/package-record.json`.
pub const PACKAGE_RECORD_SCHEMA: &str = "velnor.package-record/v1";

/// Package lanes that ship a [`PackageRecord`]. A `stable` package record is
/// informational only: stable activation demands the out-of-band release-record
/// chain (tagged commit, OCI image, APT publication). Only a `preview` package
/// record activates, because the preview lane builds no OCI image and has no
/// release record to activate.
pub const PACKAGE_KIND_STABLE: &str = "stable";
pub const PACKAGE_KIND_PREVIEW: &str = "preview";

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

/// The identity a deb carries about itself. It names only source-derived bytes:
/// the source commit, both versions, the compiled-manifest hash, and the runner
/// binary digest for the one architecture this deb was built for. It records no
/// deb digest (circular) and no OCI digest (a package is not an image), so
/// shipping it inside the deb is acyclic — the same property that lets the deb
/// ship `build-identity.json` and `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
    pub schema: String,
    pub build: PackageBuildIdentity,
    pub architecture: PackageArchitectureIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageBuildIdentity {
    pub repository: String,
    /// [`PACKAGE_KIND_STABLE`] or [`PACKAGE_KIND_PREVIEW`]. Emission refuses a
    /// record whose kind is not the emitting binary's own embedded build kind.
    pub kind: String,
    pub commit: SourceSha,
    pub crate_version: String,
    pub debian_version: String,
    pub manifest_version: u32,
    pub manifest_sha256: Sha256Hex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageArchitectureIdentity {
    pub arch: Arch,
    pub target: String,
    pub binary_sha256: Sha256Hex,
}

impl PackageRecord {
    /// Same canonical-bytes contract as a [`ReleaseRecord`]: two-space pretty
    /// JSON plus a trailing newline, so the digest is reproducible.
    pub fn to_canonical_json(&self) -> String {
        let mut json =
            serde_json::to_string_pretty(self).expect("package record always serializes");
        json.push('\n');
        json
    }

    /// SHA-256 over the canonical bytes, stored outside the record.
    pub fn digest(&self) -> Sha256Hex {
        Sha256Hex::of_bytes(self.to_canonical_json().as_bytes())
    }

    pub fn is_preview(&self) -> bool {
        self.build.kind == PACKAGE_KIND_PREVIEW
    }

    /// Structural + cross-field coherence of one package record. The kind and
    /// the Debian version validate each other, so a stable record can never be
    /// re-labelled as a preview record (or vice versa) without also breaking the
    /// version contract.
    pub fn verify(&self) -> std::result::Result<(), CoherenceError> {
        if self.schema != PACKAGE_RECORD_SCHEMA {
            return Err(CoherenceError::Schema {
                want: PACKAGE_RECORD_SCHEMA,
            });
        }
        if self.build.repository != SOURCE_REPOSITORY {
            return Err(CoherenceError::Repository);
        }
        if self.build.crate_version.is_empty() {
            return Err(CoherenceError::EmptyField("crate_version"));
        }
        if self.build.kind != PACKAGE_KIND_STABLE && self.build.kind != PACKAGE_KIND_PREVIEW {
            return Err(CoherenceError::Kind);
        }
        let version_coherent = match self.build.kind.as_str() {
            PACKAGE_KIND_STABLE => self.build.debian_version == self.build.crate_version,
            _ => {
                is_preview_debian_version(&self.build.debian_version, &self.build.crate_version)
                    && self.build.debian_version != self.build.crate_version
            }
        };
        if !version_coherent {
            return Err(CoherenceError::PackageVersion);
        }
        if self.build.manifest_version != crate::manifest::MANIFEST_VERSION {
            return Err(CoherenceError::ManifestVersion);
        }
        if self.architecture.target != self.architecture.arch.target() {
            return Err(CoherenceError::ArchTarget);
        }
        if self.architecture.binary_sha256.is_zero() {
            return Err(CoherenceError::EmptyField("architecture.binary_sha256"));
        }
        Ok(())
    }
}

/// `<crate>~preview.<run>+<7-hex>`: dpkg sorts `~` before the plain release, so
/// a preview of X is strictly older than X and can never win an upgrade race
/// against its own stable release.
fn is_preview_debian_version(debian: &str, crate_version: &str) -> bool {
    let Some(rest) = debian.strip_prefix(crate_version) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("~preview.") else {
        return false;
    };
    let Some((run, short_commit)) = rest.split_once('+') else {
        return false;
    };
    !run.is_empty()
        && run.bytes().all(|byte| byte.is_ascii_digit())
        && short_commit.len() == 7
        && short_commit
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
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
    /// The OCI index the deployed tuple runs on. `None` only for a package
    /// record: a preview package ships no image, so a deployed identity that
    /// names one for it is incoherent and fails verification.
    #[serde(default)]
    pub oci_image_digest: Option<OciDigest>,
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
    #[error("record build kind is neither 'stable' nor 'preview'")]
    Kind,
    #[error("package version does not satisfy its kind's version contract")]
    PackageVersion,
    #[error("bytes carry neither the release-record nor the package-record schema")]
    RecordSchema,
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

/// Same contract as [`verify_record_bytes`] for a deb's own [`PackageRecord`].
pub fn verify_package_record_bytes(
    bytes: &[u8],
    expected: &Sha256Hex,
) -> std::result::Result<PackageRecord, CoherenceError> {
    if &Sha256Hex::of_bytes(bytes) != expected {
        return Err(CoherenceError::RecordChecksum);
    }
    let record: PackageRecord =
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

/// One record a host can hold active in its release store and verify against the
/// installed bytes. Both variants share one on-disk tuple (`record.json` +
/// `deployed.json`), one store layout, and one verification contract: every
/// installed byte must be named by the active record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveRecord {
    /// The stable chain's release record: tagged commit, OCI image, APT
    /// publication, per-arch deb digests.
    Release(ReleaseRecord),
    /// A deb's own package record. The preview lane's activatable record; a
    /// `stable` package record is refused at activation (the stable chain
    /// activates from the out-of-band release record instead).
    Package(PackageRecord),
}

impl ActiveRecord {
    /// Parse record bytes, dispatching on their schema tag. An unknown schema is
    /// refused before any field is trusted.
    pub fn parse(bytes: &[u8]) -> std::result::Result<Self, CoherenceError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| CoherenceError::Malformed)?;
        match schema_tag_of_value(&value)? {
            RELEASE_RECORD_SCHEMA => Ok(Self::Release(
                serde_json::from_value(value).map_err(|_| CoherenceError::Malformed)?,
            )),
            PACKAGE_RECORD_SCHEMA => Ok(Self::Package(
                serde_json::from_value(value).map_err(|_| CoherenceError::Malformed)?,
            )),
            _ => Err(CoherenceError::RecordSchema),
        }
    }

    pub fn verify(&self) -> std::result::Result<(), CoherenceError> {
        match self {
            Self::Release(record) => record.verify(),
            Self::Package(record) => record.verify(),
        }
    }

    /// Canonical bytes: two-space pretty JSON, trailing newline, deterministic.
    pub fn to_canonical_json(&self) -> String {
        match self {
            Self::Release(record) => record.to_canonical_json(),
            Self::Package(record) => record.to_canonical_json(),
        }
    }

    /// SHA-256 over the canonical bytes, stored outside the record.
    pub fn digest(&self) -> Sha256Hex {
        match self {
            Self::Release(record) => record.digest(),
            Self::Package(record) => record.digest(),
        }
    }

    /// The release-store directory key this record is immutable under: the
    /// stable release tag, or the package's Debian version.
    pub fn store_key(&self) -> &str {
        match self {
            Self::Release(record) => &record.build.tag,
            Self::Package(record) => &record.build.debian_version,
        }
    }

    /// Human-facing identity for diagnostics and command output: the stable tag,
    /// or `<kind> <debian version>` for a package record.
    pub fn label(&self) -> String {
        match self {
            Self::Release(record) => record.build.tag.clone(),
            Self::Package(record) => {
                format!("{} {}", record.build.kind, record.build.debian_version)
            }
        }
    }

    pub fn source_commit(&self) -> &SourceSha {
        match self {
            Self::Release(record) => &record.build.commit,
            Self::Package(record) => &record.build.commit,
        }
    }

    pub fn crate_version(&self) -> &str {
        match self {
            Self::Release(record) => &record.build.crate_version,
            Self::Package(record) => &record.build.crate_version,
        }
    }

    pub fn package_version(&self) -> &str {
        match self {
            Self::Release(record) => &record.build.debian_version,
            Self::Package(record) => &record.build.debian_version,
        }
    }

    pub fn manifest_version(&self) -> u32 {
        match self {
            Self::Release(record) => record.build.manifest_version,
            Self::Package(record) => record.build.manifest_version,
        }
    }

    pub fn manifest_sha256(&self) -> &Sha256Hex {
        match self {
            Self::Release(record) => &record.build.manifest_sha256,
            Self::Package(record) => &record.build.manifest_sha256,
        }
    }

    /// The OCI index this record's tuple runs on. `None` for a package record: a
    /// package ships no image, so its deployed identity must name none.
    pub fn oci_index_digest(&self) -> Option<&OciDigest> {
        match self {
            Self::Release(record) => Some(&record.oci_index_digest),
            Self::Package(_) => None,
        }
    }

    /// The installed runner binary digest this record claims for `arch`.
    pub fn binary_sha256(&self, arch: Arch) -> Option<&Sha256Hex> {
        match self {
            Self::Release(record) => record.architecture(arch).map(|item| &item.binary_sha256),
            Self::Package(record) => {
                (record.architecture.arch == arch).then_some(&record.architecture.binary_sha256)
            }
        }
    }
}

fn schema_tag_of_value(value: &serde_json::Value) -> std::result::Result<&str, CoherenceError> {
    value
        .get("schema")
        .and_then(|schema| schema.as_str())
        .ok_or(CoherenceError::Malformed)
}

/// Cross-check the on-host deployed identity against the active record and the
/// installed binary's own digest. Fails on any single-field drift so a mixed
/// old/new tuple can never start. The same checks apply to both record kinds;
/// a package record simply contributes no OCI index for the deployed identity
/// to agree with.
pub fn verify_installed(
    deployed: &DeployedIdentity,
    record: &ActiveRecord,
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
    if deployed.source_commit != *record.source_commit() {
        return Err(CoherenceError::InstalledSource);
    }
    if deployed.crate_version != record.crate_version() {
        return Err(CoherenceError::InstalledCrateVersion);
    }
    if deployed.package_version != record.package_version() {
        return Err(CoherenceError::InstalledPackageVersion);
    }
    if deployed.manifest_version != record.manifest_version() {
        return Err(CoherenceError::InstalledManifestVersion);
    }
    if deployed.manifest_sha256 != *record.manifest_sha256() {
        return Err(CoherenceError::InstalledManifestHash);
    }
    if deployed.oci_image_digest.as_ref() != record.oci_index_digest() {
        return Err(CoherenceError::InstalledOci);
    }
    let arch_binary = record
        .binary_sha256(host)
        .ok_or(CoherenceError::InstalledArchMissing)?;
    if &deployed.binary_sha256 != installed_binary_sha256 || deployed.binary_sha256 != *arch_binary
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

/// Inputs for emitting a deb's own package record.
pub struct PackageRecordEmission<'a> {
    pub identity: &'a EmbeddedIdentity,
    pub record: &'a PackageRecord,
    /// The exact runner binary this deb ships. Its digest must equal the
    /// record's architecture entry, so a record can never be staged against
    /// bytes it does not describe.
    pub binary: &'a Path,
}

/// Emit a deb's own package record. The record's `kind` must be the emitting
/// binary's own embedded build kind: a preview record only ever comes from a
/// binary bound to the exact preview commit (`VELNOR_PREVIEW_SOURCE_SHA`), a
/// stable package record only from a tagged `release-build` binary. A
/// development binary emits nothing, so no package record can be produced from
/// bytes whose provenance the binary cannot name.
pub fn emit_package_record(emission: PackageRecordEmission<'_>) -> Result<()> {
    let PackageRecordEmission {
        identity,
        record,
        binary,
    } = emission;
    // A package record may come from either shippable build kind (a preview
    // build is deliberately not a release build), but never from a development
    // build, whose bytes name no source at all. This is strictly weaker than
    // [`EmbeddedIdentity::is_development`] only in allowing kind=preview.
    if identity.source_sha == "development"
        || (identity.kind != "release" && identity.kind != PACKAGE_KIND_PREVIEW)
    {
        bail!(
            "refusing to emit a package record from a development build \
             (source={}, kind={}); build with --features release-build",
            identity.source_sha,
            identity.kind
        );
    }
    if record.build.kind != identity.kind {
        bail!(
            "package record kind {} does not match this binary's embedded build kind {}",
            record.build.kind,
            identity.kind
        );
    }
    if record.build.commit.as_str() != identity.source_sha {
        bail!("package record source commit does not match this binary's embedded source SHA");
    }
    if record.build.crate_version != identity.crate_version {
        bail!("package record crate version does not match this binary's embedded crate version");
    }
    record.verify().map_err(anyhow::Error::from)?;
    let staged = sha256_file(binary)?;
    if staged != record.architecture.binary_sha256 {
        bail!("package record binary digest disagrees with the bytes it is staged against");
    }
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

    pub fn record_path(&self, key: &str) -> PathBuf {
        self.root.join("records").join(key).join("record.json")
    }
    pub fn deployed_path(&self, key: &str) -> PathBuf {
        self.root.join("records").join(key).join("deployed.json")
    }
    pub fn read_record(&self, key: &str) -> Result<ActiveRecord> {
        let path = self.record_path(key);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let record = ActiveRecord::parse(&bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("parse {}", path.display()))?;
        record.verify().map_err(anyhow::Error::from)?;
        if record.store_key() != key {
            bail!("stored release record key disagrees with its directory");
        }
        Ok(record)
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
    pub fn store_record(&self, record: &ActiveRecord) -> Result<Sha256Hex> {
        let key = record.store_key();
        validate_store_key(key)?;
        let bytes = record.to_canonical_json();
        let digest = Sha256Hex::of_bytes(bytes.as_bytes());
        let path = self.record_path(key);
        fs::create_dir_all(path.parent().unwrap())?;
        if path.exists() {
            let existing = fs::read(&path)?;
            if existing != bytes.as_bytes() {
                bail!("record for {key} already exists with different bytes — refusing to clobber");
            }
        } else {
            write_atomic(&path, bytes.as_bytes())?;
        }
        let checksum = format!("{digest}  {key}.json\n");
        write_atomic(
            &self.root.join("records").join(format!("{key}.json.sha256")),
            checksum.as_bytes(),
        )?;
        Ok(digest)
    }

    pub fn active_tag(&self) -> Result<Option<String>> {
        read_optional_link_tag(&self.active_path())
    }
    pub fn previous_tag(&self) -> Result<Option<String>> {
        read_optional_link_tag(&self.previous_path())
    }

    /// Atomically make `key` active, demoting the current active key to
    /// `previous`. The record for `key` must already be stored.
    pub fn activate(&self, record: &ActiveRecord, deployed: &DeployedIdentity) -> Result<()> {
        let key = record.store_key();
        verify_installed(
            deployed,
            record,
            Arch::host().context("unsupported host architecture")?,
            &deployed.binary_sha256,
        )
        .map_err(anyhow::Error::from)?;
        self.store_record(record)?;
        let deployed_bytes = serde_json::to_vec_pretty(deployed)?;
        let deployed_path = self.deployed_path(key);
        if deployed_path.exists() {
            if fs::read(&deployed_path)? != deployed_bytes {
                bail!("deployed identity for {key} already exists with different bytes");
            }
        } else {
            write_atomic(&deployed_path, &deployed_bytes)?;
        }
        if let Some(current) = self.active_tag()?
            && current != key
        {
            write_atomic_symlink(&self.previous_path(), &current)?;
        }
        write_atomic_symlink(&self.active_path(), key)?;
        Ok(())
    }

    /// Restore the previous coherent tag as active. Requires a recorded previous
    /// tuple whose record is still present.
    pub fn rollback(&self) -> Result<String> {
        let previous = self
            .previous_tag()?
            .context("no previous tag recorded — cannot roll back")?;
        if !self.record_path(&previous).exists() || !self.deployed_path(&previous).exists() {
            bail!("cannot roll back to {previous}: its record is missing");
        }
        if let Some(current) = self.active_tag()? {
            write_atomic_symlink(&self.previous_path(), &current)?;
        }
        write_atomic_symlink(&self.active_path(), &previous)?;
        Ok(previous)
    }
}

fn read_optional_link_tag(path: &Path) -> Result<Option<String>> {
    match fs::read_link(path) {
        Ok(target) => Ok(target
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn write_atomic_symlink(path: &Path, tag: &str) -> Result<()> {
    use std::os::unix::fs::symlink;
    let parent = path.parent().context("release pointer has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_file(&tmp);
    symlink(Path::new("records").join(tag), &tmp)?;
    fs::rename(&tmp, path)?;
    if let Ok(handle) = fs::File::open(parent) {
        let _ = handle.sync_all();
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
    let bytes = fs::read(path).with_context(|| format!("read record {}", path.display()))?;
    let record: ReleaseRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse record {}", path.display()))?;
    Ok((bytes, record))
}

/// Read either record kind. Refuses bytes that carry neither known schema, so a
/// command never has to guess what a record file holds.
fn read_active_record_file(path: &Path) -> Result<ActiveRecord> {
    let bytes = fs::read(path).with_context(|| format!("read record {}", path.display()))?;
    ActiveRecord::parse(&bytes)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("parse record {}", path.display()))
}

fn parse_checksum(text: &str) -> Result<Sha256Hex> {
    let first = text
        .split_whitespace()
        .next()
        .context("checksum file is empty")?;
    Sha256Hex::parse(first)
}

const MAX_ARTIFACT_CHECKSUM_BYTES: usize = 4096;

fn parse_artifact_checksum(text: &str) -> Result<Sha256Hex> {
    let mut fields = text.split_whitespace();
    let first = fields.next().context("artifact checksum file is empty")?;
    if fields.next().is_some() {
        bail!("artifact checksum file must contain only one checksum");
    }
    Sha256Hex::parse(first)
}

fn read_artifact_checksum(path: &Path, kind: &str) -> Result<Sha256Hex> {
    let file =
        fs::File::open(path).with_context(|| format!("read {kind} checksum {}", path.display()))?;
    let mut bytes = Vec::with_capacity(MAX_ARTIFACT_CHECKSUM_BYTES + 1);
    file.take((MAX_ARTIFACT_CHECKSUM_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {kind} checksum {}", path.display()))?;
    if bytes.len() > MAX_ARTIFACT_CHECKSUM_BYTES {
        bail!(
            "{kind} checksum {} exceeds {MAX_ARTIFACT_CHECKSUM_BYTES} bytes",
            path.display()
        );
    }
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("{kind} checksum {} is not UTF-8", path.display()))?;
    parse_artifact_checksum(text)
}

fn validate_artifact_version_component(version: &str) -> Result<()> {
    if is_safe_path_component(version) {
        Ok(())
    } else {
        bail!("release version is not a safe artifact path component");
    }
}

/// A record's store key becomes a directory name under `records/`, so it must
/// never be empty, a relative traversal, or carry a separator.
fn validate_store_key(key: &str) -> Result<()> {
    if is_safe_path_component(key) {
        Ok(())
    } else {
        bail!("release store key is not a safe path component");
    }
}

fn is_safe_path_component(value: &str) -> bool {
    !(value.is_empty()
        || value == "."
        || value == ".."
        || value
            .bytes()
            .any(|byte| byte == b'/' || byte == b'\\' || byte.is_ascii_control()))
}

fn emit_command(args: ReleaseEmitArgs) -> Result<()> {
    let record = read_active_record_file(&args.record)?;
    let identity = embedded();
    match &record {
        ActiveRecord::Release(release) => emit_record(&identity, release)?,
        ActiveRecord::Package(package) => {
            let binary = args.binary.as_deref().context(
                "emitting a package record requires --binary: the record must be staged \
                 against the exact runner bytes it names",
            )?;
            emit_package_record(PackageRecordEmission {
                identity: &identity,
                record: package,
                binary,
            })?;
        }
    }
    let canonical = record.to_canonical_json();
    let digest = Sha256Hex::of_bytes(canonical.as_bytes());
    if let Some(out) = &args.out {
        write_atomic(out, canonical.as_bytes())?;
        write_atomic(
            &out.with_extension("json.sha256"),
            format!("{digest}\n").as_bytes(),
        )?;
    } else {
        ReleaseStore::new(&args.out_dir).store_record(&record)?;
    }
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
        let expected_binary = read_artifact_checksum(&binary, "binary")?;
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
        let expected_deb = read_artifact_checksum(&deb, "deb")?;
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

    let mut expected_bytes = Vec::new();
    let mut served_bytes = Vec::new();
    expected_file.read_to_end(&mut expected_bytes)?;
    served_file.read_to_end(&mut served_bytes)?;
    Ok((expected_bytes, served_bytes))
}

/// Peek at a record file's schema tag so a command can pick the right verifier
/// before spending any effort on the bytes. Refuses an unknown tag.
fn record_schema_tag(bytes: &[u8]) -> Result<&'static str> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).with_context(|| "record is not a JSON object".to_string())?;
    match schema_tag_of_value(&value).map_err(anyhow::Error::from)? {
        RELEASE_RECORD_SCHEMA => Ok(RELEASE_RECORD_SCHEMA),
        PACKAGE_RECORD_SCHEMA => Ok(PACKAGE_RECORD_SCHEMA),
        other => bail!("unsupported record schema '{other}'"),
    }
}

fn verify_record_command(args: ReleaseVerifyRecordArgs) -> Result<()> {
    let bytes =
        fs::read(&args.record).with_context(|| format!("read {}", args.record.display()))?;
    let expected = match (&args.checksum, &args.sha256) {
        (Some(path), _) => parse_checksum(&fs::read_to_string(path)?)?,
        (None, Some(hex)) => Sha256Hex::parse(hex)?,
        (None, None) => bail!("provide --checksum <file> or --sha256 <hex>"),
    };
    match record_schema_tag(&bytes)? {
        RELEASE_RECORD_SCHEMA => verify_release_record_command(&args, &bytes, expected),
        PACKAGE_RECORD_SCHEMA => verify_package_record_command(&args, &bytes, expected),
        other => bail!("unsupported record schema '{other}'"),
    }
}

fn verify_release_record_command(
    args: &ReleaseVerifyRecordArgs,
    bytes: &[u8],
    expected: Sha256Hex,
) -> Result<()> {
    let record = verify_record_bytes(bytes, &expected).map_err(anyhow::Error::from)?;
    let publication = if let Some(path) = &args.publication {
        Some(
            serde_json::from_slice::<PublicationRecord>(
                &fs::read(path).with_context(|| format!("read publication {}", path.display()))?,
            )
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

fn verify_package_record_command(
    args: &ReleaseVerifyRecordArgs,
    bytes: &[u8],
    expected: Sha256Hex,
) -> Result<()> {
    if args.publication.is_some()
        || args.expected_apt_metadata.is_some()
        || args.served_apt_metadata.is_some()
    {
        bail!("APT publication claims verify the stable release chain; a package record has no publication to check");
    }
    let record = verify_package_record_bytes(bytes, &expected).map_err(anyhow::Error::from)?;
    println!(
        "package record for {} is coherent (digest {})",
        record.build.debian_version,
        record.digest()
    );
    Ok(())
}

fn verify_installed_command(args: ReleaseVerifyInstalledArgs) -> Result<()> {
    let record = read_active_record_file(&args.record)?;
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
    let record = read_active_record_file(&args.record)?;
    record.verify().map_err(anyhow::Error::from)?;
    let host = Arch::host().context("unsupported host architecture")?;

    // A stable activation additionally proves the OCI image is exactly the one
    // the release record names. A preview package ships no image, so its
    // activation proves only the package bytes — binary, manifest, versions,
    // and commit — which is all a preview deb claims to be.
    let expected_binary = match &record {
        ActiveRecord::Release(release) => release
            .architecture(host)
            .context("release record lacks host architecture")?
            .binary_sha256
            .clone(),
        ActiveRecord::Package(package) => {
            if !package.is_preview() {
                bail!(
                    "package record {} is kind={}; a stable package activates from its \
                     out-of-band release record, never from the deb",
                    package.build.debian_version,
                    package.build.kind
                );
            }
            if package.architecture.arch != host {
                bail!(
                    "package record names {} bytes but this host is {}",
                    package.architecture.arch.as_str(),
                    host.as_str()
                );
            }
            package.architecture.binary_sha256.clone()
        }
    };
    let installed_binary = Path::new(INSTALLED_BINARY_PATH);
    let binary_sha256 = sha256_file(installed_binary)?;
    if binary_sha256 != expected_binary {
        bail!("installed binary digest disagrees with release record");
    }
    let manifest_sha256 = Sha256Hex::of_bytes(crate::manifest::to_json_document()?.as_bytes());
    if manifest_sha256 != *record.manifest_sha256() {
        bail!("compiled manifest digest disagrees with release record");
    }
    if let ActiveRecord::Release(release) = &record {
        verify_and_tag_release_image(release)?;
    }

    let deployed = DeployedIdentity {
        schema: DEPLOYED_IDENTITY_SCHEMA.to_string(),
        package_version: record.package_version().to_string(),
        crate_version: record.crate_version().to_string(),
        source_commit: record.source_commit().clone(),
        binary_sha256,
        manifest_version: record.manifest_version(),
        manifest_sha256,
        oci_image_digest: record.oci_index_digest().cloned(),
        record_sha256: record.digest(),
    };
    verify_installed(&deployed, &record, host, &deployed.binary_sha256)
        .map_err(anyhow::Error::from)?;
    let store = ReleaseStore::new(&args.dir);
    store.activate(&record, &deployed)?;
    println!("activated {}", record.label());
    Ok(())
}

fn rollback_command(args: ReleaseRollbackArgs) -> Result<()> {
    let store = ReleaseStore::new(&args.dir);
    let previous = store
        .previous_tag()?
        .context("no previous release is available")?;
    let record = store.read_record(&previous)?;
    // A rollback changes both halves of the runtime tuple while the fleet is
    // drained: first make the exact prior image locally runnable, then switch
    // the filesystem pointer. Any verification failure leaves active unchanged.
    // A preview package tuple has no image to restore.
    if let ActiveRecord::Release(release) = &record {
        verify_and_tag_release_image(release)?;
    }
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

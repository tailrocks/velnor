//! The producer-owned application release manifest.
//!
//! A product release has one authority.  Package feeds and formulae consume
//! this manifest (or a projection of it); they do not discover a second
//! manifest by filename.  The manifest deliberately has no
//! `manifest_sha256` member: a document cannot contain the digest of the
//! bytes it is part of.  Publishers keep that digest in a sibling checksum or
//! release record instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use anyhow::{bail, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::release::EmbeddedIdentity;

/// The only product-manifest schema accepted by current consumers.
pub const PRODUCT_MANIFEST_SCHEMA: &str = "velnor.product-manifest/v1";
/// The producer's one authoritative manifest filename.
pub const PRODUCT_MANIFEST_FILE: &str = "product-manifest.json";
/// The external digest sidecar for [`PRODUCT_MANIFEST_FILE`].
pub const PRODUCT_MANIFEST_DIGEST_FILE: &str = "product-manifest.json.sha256";
/// The source-generated typed component contract consumed by the publisher.
pub const PRODUCT_COMPONENT_CONTRACT_SCHEMA: &str = "velnor.native-product-contract/v1";

/// The immutable application release identity and complete artifact inventory.
///
/// Product `version` is intentionally independent of every component's crate
/// version.  A component can therefore be rebuilt or bumped without silently
/// changing the product release identity.  All byte digests live in
/// [`ApplicationArtifact`] rows; the manifest itself is never listed as one of
/// those rows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationManifest {
    pub schema: String,
    pub product_id: String,
    pub channel: String,
    pub version: String,
    pub source_repository: String,
    pub source_ref: String,
    pub source_commit: String,
    pub release_tag: String,
    pub release_id: String,
    pub artifacts: Vec<ApplicationArtifact>,
    pub components: Vec<ApplicationComponent>,
}

/// One immutable byte product.  `name` is a basename, not an untrusted path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationArtifact {
    pub name: String,
    pub target: String,
    pub kind: String,
    pub sha256: String,
    pub size: u64,
}

/// One typed sibling binary in the product.  Every `(binary, target)` pair
/// must have a matching `kind = "binary"` artifact row, which makes incomplete
/// sibling archives fail before publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationComponent {
    pub name: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub feature: Option<String>,
    pub identity: String,
    pub version: String,
    pub binary: String,
    pub targets: Vec<String>,
}

/// One component declaration independently generated from the native-product
/// source configuration and Cargo metadata.  Downloaded component rows are
/// never accepted as this contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductComponentContract {
    pub name: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub binary: String,
    pub feature: Option<String>,
    pub identity: String,
    pub version: String,
    pub targets: Vec<String>,
}

/// The typed producer configuration that binds rows, manifests, and archive
/// payloads to one source-owned component map.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProductContract {
    pub schema: String,
    pub product_id: String,
    pub channel: String,
    pub manifest_schema: String,
    pub archive_component: String,
    pub archive_identity_schema: String,
    pub archive_manifest_schema: String,
    pub targets: Vec<String>,
    pub blocked_targets: Vec<String>,
    pub components: Vec<ProductComponentContract>,
}

/// The exact field-level failure from a producer or consumer check.  Error
/// values name the field, never a digest or source value.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApplicationManifestError {
    #[error("application manifest schema is not the current schema")]
    Schema,
    #[error("application manifest field '{0}' is empty or malformed")]
    Field(&'static str),
    #[error("application manifest has duplicate {0}")]
    Duplicate(&'static str),
    #[error("application manifest has an unsupported target")]
    Target,
    #[error("application manifest is missing a component binary artifact")]
    ComponentArtifact,
    #[error("application manifest contains the manifest itself as an artifact")]
    SelfArtifact,
    #[error("application manifest artifact is missing")]
    ArtifactMissing,
    #[error("application manifest artifact digest does not match its bytes")]
    ArtifactDigest,
    #[error("application manifest artifact size does not match its bytes")]
    ArtifactSize,
    #[error("application manifest artifact inventory is not exact")]
    ArtifactInventory,
    #[error("application manifest binary architecture does not match its target")]
    Architecture,
    #[error("application manifest is missing a target archive")]
    ArchiveMissing,
    #[error("application manifest archive contains unsafe or unexpected members")]
    ArchiveUnsafe,
    #[error("application manifest is not canonical JSON")]
    NonCanonical,
    #[error("application manifest digest does not match its bytes")]
    Digest,
    #[error("native product component contract is invalid or disagrees with the manifest")]
    Contract,
}

impl NativeProductContract {
    /// Parse and validate the independently generated source contract.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ApplicationManifestError> {
        let contract: Self =
            serde_json::from_slice(bytes).map_err(|_| ApplicationManifestError::Contract)?;
        contract.verify()?;
        Ok(contract)
    }

    /// Validate the contract's complete target/component census and identity
    /// modes before any downloaded row can be used as a path.
    pub fn verify(&self) -> Result<(), ApplicationManifestError> {
        if self.schema != PRODUCT_COMPONENT_CONTRACT_SCHEMA
            || !safe_slug(&self.product_id)
            || (self.channel != "stable" && self.channel != "preview")
            || !safe_schema(&self.manifest_schema)
            || !safe_schema(&self.archive_identity_schema)
            || !safe_schema(&self.archive_manifest_schema)
            || !safe_slug(&self.archive_component)
            || self.targets.is_empty()
        {
            return Err(ApplicationManifestError::Contract);
        }
        let expected_targets = self
            .targets
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let blocked_targets = self
            .blocked_targets
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected_targets.len() != self.targets.len()
            || blocked_targets.len() != self.blocked_targets.len()
            || blocked_targets
                .iter()
                .any(|target| !expected_targets.contains(target))
        {
            return Err(ApplicationManifestError::Contract);
        }
        if self.targets.iter().any(|target| !supported_target(target)) {
            return Err(ApplicationManifestError::Contract);
        }
        let mut names = BTreeSet::new();
        let mut binaries = BTreeSet::new();
        for component in &self.components {
            if !names.insert(component.name.as_str())
                || !safe_slug(&component.name)
                || !safe_slug(&component.crate_name)
                || !safe_basename(&component.binary)
                || !safe_version(&component.version)
                || component.version == "development"
                || component.version == "unknown"
                || !matches!(component.identity.as_str(), "version" | "revision")
                || component
                    .feature
                    .as_deref()
                    .is_some_and(|feature| feature != "release-build")
            {
                return Err(ApplicationManifestError::Contract);
            }
            let component_targets = component
                .targets
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if component_targets != expected_targets
                || component_targets.len() != component.targets.len()
                || component
                    .targets
                    .iter()
                    .any(|target| !supported_target(target))
                || !binaries.insert(component.binary.as_str())
            {
                return Err(ApplicationManifestError::Contract);
            }
        }
        if self.components.is_empty() || !names.contains(self.archive_component.as_str()) {
            return Err(ApplicationManifestError::Contract);
        }
        Ok(())
    }
}

impl ApplicationManifest {
    /// Sort all unordered rows before serializing.  JSON object field order is
    /// derived from the struct; array order is normalized here so two
    /// independent publishers hash equal logical manifests identically.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.artifacts.sort_by(|left, right| {
            (&left.target, &left.kind, &left.name).cmp(&(&right.target, &right.kind, &right.name))
        });
        normalized
            .components
            .sort_by(|left, right| left.name.cmp(&right.name));
        for component in &mut normalized.components {
            component.targets.sort();
        }
        normalized
    }

    /// Canonical bytes: two-space pretty JSON and one trailing newline.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let normalized = self.normalized();
        // All fields are serde strings, integers, and vectors; serialization
        // cannot fail for this derived representation.
        #[allow(
            clippy::expect_used,
            reason = "derived JSON serialization is infallible"
        )]
        let mut json = serde_json::to_string_pretty(&normalized)
            .expect("application manifest always serializes");
        json.push('\n');
        json
    }

    /// SHA-256 of the canonical manifest bytes.  Callers store this outside
    /// the manifest, in [`PRODUCT_MANIFEST_DIGEST_FILE`] or a parent
    /// release record.
    #[must_use]
    pub fn digest(&self) -> String {
        hex_lower(&Sha256::digest(self.to_canonical_json().as_bytes()))
    }

    /// Validate structure, identity cross-fields, target coverage, and the
    /// complete required sibling inventory.
    pub fn verify(&self) -> std::result::Result<(), ApplicationManifestError> {
        if self.schema != PRODUCT_MANIFEST_SCHEMA {
            return Err(ApplicationManifestError::Schema);
        }
        for (name, value) in [
            ("product_id", self.product_id.as_str()),
            ("channel", self.channel.as_str()),
            ("version", self.version.as_str()),
            ("source_repository", self.source_repository.as_str()),
            ("source_ref", self.source_ref.as_str()),
            ("source_commit", self.source_commit.as_str()),
            ("release_tag", self.release_tag.as_str()),
            ("release_id", self.release_id.as_str()),
        ] {
            if value.is_empty() || value.contains(['\n', '\r']) {
                return Err(ApplicationManifestError::Field(name));
            }
        }
        if !safe_version(&self.version) {
            return Err(ApplicationManifestError::Field("version"));
        }
        if !safe_slug(&self.product_id)
            || !repository_slug(&self.source_repository)
            || !safe_ref(&self.source_ref)
            || !provider_release_id(&self.release_id)
        {
            return Err(ApplicationManifestError::Field("identity"));
        }
        if self.channel != "stable" && self.channel != "preview" {
            return Err(ApplicationManifestError::Field("channel"));
        }
        if !lower_hex(&self.source_commit, 40) {
            return Err(ApplicationManifestError::Field("source_commit"));
        }
        if self.channel == "stable" {
            if !stable_version(&self.version) {
                return Err(ApplicationManifestError::Field("version"));
            }
            if self.release_tag != format!("v{}", self.version) {
                return Err(ApplicationManifestError::Field("release_tag"));
            }
            if self.source_ref != format!("refs/tags/{}", self.release_tag) {
                return Err(ApplicationManifestError::Field("source_ref"));
            }
        } else {
            if !preview_version(&self.version, &self.source_commit) {
                return Err(ApplicationManifestError::Field("version"));
            }
            if self.source_ref != "refs/heads/main" {
                return Err(ApplicationManifestError::Field("source_ref"));
            }
            if self.release_tag != format!("preview-{}", self.source_commit) {
                return Err(ApplicationManifestError::Field("release_tag"));
            }
        }

        let mut artifact_names = BTreeSet::new();
        for artifact in &self.artifacts {
            if artifact.name == PRODUCT_MANIFEST_FILE
                || artifact.name == PRODUCT_MANIFEST_DIGEST_FILE
            {
                return Err(ApplicationManifestError::SelfArtifact);
            }
            if !safe_basename(&artifact.name)
                || artifact.target.is_empty()
                || !supported_artifact_kind(&artifact.kind)
                || !supported_target(&artifact.target)
                || !lower_hex(&artifact.sha256, 64)
                || artifact.size == 0
            {
                return Err(ApplicationManifestError::Field("artifacts"));
            }
            if !artifact_names.insert(artifact.name.clone()) {
                return Err(ApplicationManifestError::Duplicate("artifact name"));
            }
        }
        if self.artifacts.is_empty() {
            return Err(ApplicationManifestError::Field("artifacts"));
        }

        let mut component_names = BTreeSet::new();
        let mut component_binaries = BTreeSet::new();
        let mut component_targets = BTreeSet::new();
        for component in &self.components {
            if !safe_slug(&component.name)
                || !safe_slug(&component.crate_name)
                || component
                    .feature
                    .as_deref()
                    .is_some_and(|feature| feature != "release-build")
                || !matches!(component.identity.as_str(), "version" | "revision")
                || !safe_version(&component.version)
                || matches!(component.version.as_str(), "development" | "unknown")
                || !safe_basename(&component.binary)
                || component.targets.is_empty()
            {
                return Err(ApplicationManifestError::Field("components"));
            }
            if !component_names.insert(component.name.clone()) {
                return Err(ApplicationManifestError::Duplicate("component name"));
            }
            let mut targets = BTreeSet::new();
            for target in &component.targets {
                if !supported_target(target) || !targets.insert(target) {
                    return Err(ApplicationManifestError::Target);
                }
                if !component_binaries.insert((component.binary.as_str(), target.as_str())) {
                    return Err(ApplicationManifestError::Duplicate("component binary"));
                }
                component_targets.insert(target.as_str());
                let found = self.artifacts.iter().any(|artifact| {
                    artifact.kind == "binary"
                        && (artifact.name == component.binary
                            || artifact.name == format!("{}-{}", component.binary, target))
                        && artifact.target == *target
                });
                if !found {
                    return Err(ApplicationManifestError::ComponentArtifact);
                }
            }
        }
        if self.components.is_empty() {
            return Err(ApplicationManifestError::Field("components"));
        }
        for target in &component_targets {
            let archive_kind = if target.ends_with("-apple-darwin") {
                "homebrew-archive"
            } else {
                "archive"
            };
            let archive_count = self
                .artifacts
                .iter()
                .filter(|artifact| artifact.target == *target && artifact.kind == archive_kind)
                .count();
            if archive_count != 1 {
                return Err(ApplicationManifestError::ArchiveMissing);
            }
        }
        if self
            .artifacts
            .iter()
            .any(|artifact| !component_targets.contains(artifact.target.as_str()))
        {
            return Err(ApplicationManifestError::Target);
        }
        Ok(())
    }

    /// Verify the exact typed profile supplied by the producer configuration.
    /// The base manifest validator remains reusable for other product shapes;
    /// publication additionally supplies the target/component census so a
    /// consumer cannot accept a self-authored partial inventory.
    pub fn verify_profile(
        &self,
        expected_targets: &[String],
        expected_components: &[String],
    ) -> std::result::Result<(), ApplicationManifestError> {
        self.verify()?;
        let expected_target_count = expected_targets.len();
        let expected_targets = expected_targets
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_component_count = expected_components.len();
        let expected_components = expected_components
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected_targets.is_empty()
            || expected_components.is_empty()
            || expected_target_count != expected_targets.len()
            || expected_component_count != expected_components.len()
        {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        let actual_components = self
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect::<BTreeSet<_>>();
        if actual_components != expected_components {
            return Err(ApplicationManifestError::ArtifactInventory);
        }

        let actual_targets = self
            .components
            .iter()
            .flat_map(|component| component.targets.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        if actual_targets != expected_targets {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        for component in &self.components {
            let component_targets = component
                .targets
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if component_targets != expected_targets {
                return Err(ApplicationManifestError::ArtifactInventory);
            }
        }

        let expected_binary_names = self
            .components
            .iter()
            .flat_map(|component| {
                expected_targets
                    .iter()
                    .map(move |target| format!("{}-{target}", component.binary))
            })
            .collect::<BTreeSet<_>>();
        let actual_binary_names = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.kind == "binary")
            .map(|artifact| artifact.name.clone())
            .collect::<BTreeSet<_>>();
        if actual_binary_names != expected_binary_names {
            return Err(ApplicationManifestError::ArtifactInventory);
        }

        let archive_targets = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.kind == "archive" || artifact.kind == "homebrew-archive")
            .map(|artifact| artifact.target.as_str())
            .collect::<BTreeSet<_>>();
        if archive_targets != expected_targets {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        let apt_targets = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.kind == "apt-package")
            .map(|artifact| artifact.target.as_str())
            .collect::<BTreeSet<_>>();
        let expected_apt_targets = expected_targets
            .iter()
            .copied()
            .filter(|target| target.ends_with("-unknown-linux-gnu"))
            .collect::<BTreeSet<_>>();
        if apt_targets != expected_apt_targets {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        let expected_artifacts =
            expected_binary_names.len() + expected_targets.len() + expected_apt_targets.len();
        if self.artifacts.len() != expected_artifacts {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        Ok(())
    }

    /// Verify the complete source-owned component declaration, including
    /// crate, feature, identity mode, binary, component version, and target
    /// coverage.  These fields stay in the canonical rows so every consumer
    /// binds to the same source-owned component contract.
    pub fn verify_typed_profile(
        &self,
        contract: &NativeProductContract,
    ) -> std::result::Result<(), ApplicationManifestError> {
        contract.verify()?;
        self.verify_profile(&contract.targets, &contract.component_names())?;
        if self.schema != contract.manifest_schema
            || self.product_id != contract.product_id
            || self.channel != contract.channel
        {
            return Err(ApplicationManifestError::Contract);
        }
        let expected = contract
            .components
            .iter()
            .map(|component| (component.name.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        let actual = self
            .components
            .iter()
            .map(|component| (component.name.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        if expected.len() != actual.len() {
            return Err(ApplicationManifestError::Contract);
        }
        for (name, expected) in expected {
            let Some(actual) = actual.get(name) else {
                return Err(ApplicationManifestError::Contract);
            };
            if actual.crate_name != expected.crate_name
                || actual.feature != expected.feature
                || actual.identity != expected.identity
                || actual.binary != expected.binary
                || actual.version != expected.version
                || actual
                    .targets
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    != expected
                        .targets
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
            {
                return Err(ApplicationManifestError::Contract);
            }
        }
        Ok(())
    }

    /// Verify canonical bytes and the required externally supplied digest.
    pub fn verify_bytes(
        bytes: &[u8],
        expected_digest: &str,
    ) -> Result<Self, ApplicationManifestError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| ApplicationManifestError::NonCanonical)?;
        manifest.verify()?;
        if manifest.to_canonical_json().as_bytes() != bytes {
            return Err(ApplicationManifestError::NonCanonical);
        }
        if expected_digest != manifest.digest() {
            return Err(ApplicationManifestError::Digest);
        }
        Ok(manifest)
    }

    /// Hash every listed artifact beneath `root`, rejecting missing, changed,
    /// or truncated bytes.  This is intentionally a filesystem-only check;
    /// it never starts a daemon, installs a package, or runs a product binary.
    pub fn verify_artifacts(&self, root: &Path) -> Result<(), ApplicationManifestError> {
        self.verify_artifacts_with_contract(root, None)
    }

    /// Verify payload bytes and archives against the optional independent
    /// contract.  Publication supplies the contract; the `None` form keeps
    /// the structural verifier reusable for existing package checks.
    pub fn verify_artifacts_with_contract(
        &self,
        root: &Path,
        contract: Option<&NativeProductContract>,
    ) -> Result<(), ApplicationManifestError> {
        self.verify()?;
        if let Some(contract) = contract {
            self.verify_typed_profile(contract)?;
        }
        let expected_names: BTreeSet<&str> = self
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        let mut actual_names = BTreeSet::new();
        for entry in fs::read_dir(root).map_err(|_| ApplicationManifestError::ArtifactMissing)? {
            let entry = entry.map_err(|_| ApplicationManifestError::ArtifactMissing)?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ApplicationManifestError::ArtifactMissing)?;
            if !metadata.file_type().is_file() {
                return Err(ApplicationManifestError::ArtifactInventory);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ApplicationManifestError::ArtifactInventory)?;
            if !actual_names.insert(name) {
                return Err(ApplicationManifestError::ArtifactInventory);
            }
        }
        if actual_names.len() != expected_names.len()
            || actual_names
                .iter()
                .any(|name| !expected_names.contains(name.as_str()))
        {
            return Err(ApplicationManifestError::ArtifactInventory);
        }
        for artifact in &self.artifacts {
            let path = root.join(&artifact.name);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| ApplicationManifestError::ArtifactMissing)?;
            if !metadata.file_type().is_file() {
                return Err(ApplicationManifestError::ArtifactMissing);
            }
            if metadata.len() != artifact.size {
                return Err(ApplicationManifestError::ArtifactSize);
            }
            let bytes = fs::read(&path).map_err(|_| ApplicationManifestError::ArtifactMissing)?;
            let digest = hex_lower(&Sha256::digest(bytes));
            if digest != artifact.sha256 {
                return Err(ApplicationManifestError::ArtifactDigest);
            }
            if artifact.kind == "binary" {
                verify_binary_architecture(&path, &artifact.target)?;
            } else if matches!(artifact.kind.as_str(), "archive" | "homebrew-archive") {
                verify_archive_members(&path, self, &artifact.target, contract)?;
            }
        }
        Ok(())
    }
}

impl NativeProductContract {
    pub(crate) fn component_names(&self) -> Vec<String> {
        self.components
            .iter()
            .map(|component| component.name.clone())
            .collect()
    }
}

/// Inspect a product archive without extracting it.  Product archives are
/// gzip-compressed tar streams containing only the target's sibling binaries
/// and the two typed identity documents.  Refuse paths, duplicate members,
/// links, directories, and undeclared files before any consumer can extract
/// the archive.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveIdentity {
    schema: String,
    product_id: String,
    channel: String,
    version: String,
    source_repository: String,
    source_ref: String,
    source_commit: String,
    release_tag: String,
    parent_manifest_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveComponent {
    name: String,
    #[serde(rename = "crate")]
    crate_name: String,
    crate_version: String,
    feature: Option<String>,
    identity: String,
    release_version: String,
    source_commit: String,
    binary_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveManifest {
    schema: String,
    product_id: String,
    channel: String,
    version: String,
    source_repository: String,
    source_ref: String,
    source_commit: String,
    release_tag: String,
    parent_manifest_id: String,
    components: Vec<ArchiveComponent>,
}

fn verify_archive_members(
    path: &Path,
    manifest: &ApplicationManifest,
    target: &str,
    contract: Option<&NativeProductContract>,
) -> std::result::Result<(), ApplicationManifestError> {
    let expected = manifest
        .components
        .iter()
        .filter(|component| {
            component
                .targets
                .iter()
                .any(|candidate| candidate == target)
        })
        .map(|component| component.binary.as_str())
        .chain(["identity.json", "manifest.json"])
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected_binary_artifacts = manifest
        .components
        .iter()
        .filter(|component| {
            component
                .targets
                .iter()
                .any(|candidate| candidate == target)
        })
        .map(|component| {
            let name = format!("{}-{target}", component.binary);
            let artifact = manifest.artifacts.iter().find(|artifact| {
                artifact.kind == "binary" && artifact.target == target && artifact.name == name
            });
            (component.binary.as_str(), artifact)
        })
        .collect::<BTreeMap<_, _>>();
    if expected_binary_artifacts.values().any(Option::is_none) {
        return Err(ApplicationManifestError::ArchiveUnsafe);
    }
    let file = fs::File::open(path).map_err(|_| ApplicationManifestError::ArtifactMissing)?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut actual = BTreeSet::new();
    let mut identity_bytes = None;
    let mut manifest_bytes = None;
    for entry in archive
        .entries()
        .map_err(|_| ApplicationManifestError::ArchiveUnsafe)?
    {
        let entry = entry.map_err(|_| ApplicationManifestError::ArchiveUnsafe)?;
        if !entry.header().entry_type().is_file() {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        let path = entry
            .path()
            .map_err(|_| ApplicationManifestError::ArchiveUnsafe)?;
        let mut components = path.components();
        let Some(Component::Normal(name)) = components.next() else {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        };
        if components.next().is_some() {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        let Some(name) = name.to_str().map(str::to_owned) else {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        };
        if !actual.insert(name.to_owned()) {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        let mut bytes = Vec::new();
        entry
            .take(usize::MAX as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ApplicationManifestError::ArchiveUnsafe)?;
        match name.as_str() {
            "identity.json" => identity_bytes = Some(bytes),
            "manifest.json" => manifest_bytes = Some(bytes),
            binary => {
                let Some(Some(artifact)) = expected_binary_artifacts.get(binary) else {
                    return Err(ApplicationManifestError::ArchiveUnsafe);
                };
                if bytes.len() as u64 != artifact.size
                    || hex_lower(&Sha256::digest(&bytes)) != artifact.sha256
                {
                    return Err(ApplicationManifestError::ArchiveUnsafe);
                }
            }
        }
    }
    if actual != expected || identity_bytes.is_none() || manifest_bytes.is_none() {
        Err(ApplicationManifestError::ArchiveUnsafe)
    } else if let Some(contract) = contract {
        let identity: ArchiveIdentity = serde_json::from_slice(
            identity_bytes
                .as_deref()
                .ok_or(ApplicationManifestError::ArchiveUnsafe)?,
        )
        .map_err(|_| ApplicationManifestError::ArchiveUnsafe)?;
        let archive_manifest: ArchiveManifest = serde_json::from_slice(
            manifest_bytes
                .as_deref()
                .ok_or(ApplicationManifestError::ArchiveUnsafe)?,
        )
        .map_err(|_| ApplicationManifestError::ArchiveUnsafe)?;
        let identity_matches = identity.schema == contract.archive_identity_schema
            && identity.product_id == manifest.product_id
            && identity.channel == manifest.channel
            && identity.version == manifest.version
            && identity.source_repository == manifest.source_repository
            && identity.source_ref == manifest.source_ref
            && identity.source_commit == manifest.source_commit
            && identity.release_tag == manifest.release_tag
            && identity.parent_manifest_id == manifest.release_id;
        let manifest_matches = archive_manifest.schema == contract.archive_manifest_schema
            && archive_manifest.product_id == manifest.product_id
            && archive_manifest.channel == manifest.channel
            && archive_manifest.version == manifest.version
            && archive_manifest.source_repository == manifest.source_repository
            && archive_manifest.source_ref == manifest.source_ref
            && archive_manifest.source_commit == manifest.source_commit
            && archive_manifest.release_tag == manifest.release_tag
            && archive_manifest.parent_manifest_id == manifest.release_id;
        if !identity_matches || !manifest_matches {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        let expected_components = manifest
            .components
            .iter()
            .filter(|component| {
                component
                    .targets
                    .iter()
                    .any(|candidate| candidate == target)
            })
            .map(|component| {
                let binary_sha256 = manifest
                    .artifacts
                    .iter()
                    .find(|artifact| {
                        artifact.kind == "binary"
                            && artifact.target == target
                            && artifact.name == format!("{}-{target}", component.binary)
                    })
                    .map(|artifact| artifact.sha256.clone());
                (
                    component.name.clone(),
                    (
                        component.crate_name.clone(),
                        component.feature.clone(),
                        component.identity.clone(),
                        component.version.clone(),
                        binary_sha256,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut actual_component_names = BTreeSet::new();
        if archive_manifest.components.len() != expected_components.len()
            || archive_manifest
                .components
                .iter()
                .any(|component| !actual_component_names.insert(component.name.as_str()))
        {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        let actual_components = archive_manifest
            .components
            .into_iter()
            .map(|component| {
                (
                    component.name,
                    (
                        component.crate_name,
                        component.feature,
                        component.identity,
                        component.crate_version,
                        Some(component.binary_sha256),
                        component.release_version,
                        component.source_commit,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let expected_components = expected_components
            .into_iter()
            .map(
                |(name, (crate_name, feature, identity, version, binary_sha256))| {
                    (
                        name,
                        (
                            crate_name,
                            feature,
                            identity,
                            version,
                            binary_sha256,
                            manifest.version.clone(),
                            manifest.source_commit.clone(),
                        ),
                    )
                },
            )
            .collect::<BTreeMap<_, _>>();
        if actual_components != expected_components {
            return Err(ApplicationManifestError::ArchiveUnsafe);
        }
        Ok(())
    } else {
        Ok(())
    }
}

/// Verify the executable format and architecture encoded by a native target.
/// This is deliberately byte-level and offline: it never executes the file.
pub fn verify_binary_architecture(
    path: &Path,
    target: &str,
) -> std::result::Result<(), ApplicationManifestError> {
    let bytes = fs::read(path).map_err(|_| ApplicationManifestError::ArtifactMissing)?;
    let matches = match target {
        "x86_64-unknown-linux-gnu" => {
            bytes.starts_with(&[0x7f, b'E', b'L', b'F', 2, 1])
                && bytes.get(18..20) == Some(&[0x3e, 0x00])
        }
        "aarch64-unknown-linux-gnu" => {
            bytes.starts_with(&[0x7f, b'E', b'L', b'F', 2, 1])
                && bytes.get(18..20) == Some(&[0xb7, 0x00])
        }
        "aarch64-apple-darwin" => {
            bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
                && bytes.get(4..8) == Some(&[0x0c, 0x00, 0x00, 0x01])
        }
        "x86_64-apple-darwin" => {
            bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
                && bytes.get(4..8) == Some(&[0x07, 0x00, 0x00, 0x01])
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(ApplicationManifestError::Architecture)
    }
}

/// Refuse to publish a binary without exact release source identity.  This is
/// shared by a future product command and keeps the source-identity rule next
/// to the manifest producer rather than in an ad-hoc workflow string.
pub fn publication_identity() -> Result<EmbeddedIdentity> {
    let identity = crate::release::embedded();
    if identity.is_development() || identity.source_sha == "unknown" {
        bail!("publication requires a release-build binary with exact source identity");
    }
    if !lower_hex(&identity.source_sha, 40)
        || identity.tag == "unknown"
        || identity.tag == "development"
        || identity.tag.is_empty()
    {
        bail!("publication requires a known release tag and 40-hex source commit");
    }
    Ok(identity)
}

fn supported_target(target: &str) -> bool {
    matches!(
        target,
        "x86_64-unknown-linux-gnu"
            | "aarch64-unknown-linux-gnu"
            | "aarch64-apple-darwin"
            | "x86_64-apple-darwin"
    )
}

fn supported_artifact_kind(kind: &str) -> bool {
    matches!(
        kind,
        "binary" | "archive" | "homebrew-archive" | "apt-package"
    )
}

fn safe_slug(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

fn safe_schema(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'/' | b'_')
        })
}

/// The provider's numeric release id is the only release identity accepted by
/// package projections.  A producer tag, repository slug, or locally invented
/// coordinate is not an API release id and cannot safely join an external
/// archive record to this manifest.
fn provider_release_id(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_' | b'~')
        })
}

fn stable_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| numeric_version_part(part))
}

fn numeric_version_part(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

fn preview_version(value: &str, source_commit: &str) -> bool {
    let Some((base, suffix)) = value.split_once("-preview.") else {
        return false;
    };
    let Some((run, sha7)) = suffix.split_once('+') else {
        return false;
    };
    stable_version(base)
        && !run.is_empty()
        && numeric_version_part(run)
        && sha7 == &source_commit[..7]
        && lower_hex(sha7, 7)
}

fn safe_ref(value: &str) -> bool {
    value.starts_with("refs/") && !value.contains("..") && !value.contains(['\n', '\r'])
}

fn repository_slug(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repo), None) if safe_slug(owner) && safe_slug(repo))
}

fn safe_basename(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['/', '\\'])
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn x86_elf() -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        bytes[18..20].copy_from_slice(&[0x3e, 0x00]);
        bytes
    }

    fn archive_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            if header.set_path("placeholder").is_err() {
                return Vec::new();
            }
            let path_bytes = name.as_bytes();
            let header_bytes = header.as_mut_bytes();
            header_bytes[..100].fill(0);
            header_bytes[..path_bytes.len()].copy_from_slice(path_bytes);
            header.set_cksum();
            tar_bytes.extend_from_slice(header.as_bytes());
            tar_bytes.extend_from_slice(contents);
            let padding = (512 - contents.len() % 512) % 512;
            tar_bytes.resize(tar_bytes.len() + padding, 0);
        }
        tar_bytes.resize(tar_bytes.len() + 1024, 0);
        let mut bytes = Vec::new();
        let mut encoder = GzEncoder::new(&mut bytes, Compression::default());
        if encoder.write_all(&tar_bytes).is_err() || encoder.finish().is_err() {
            return Vec::new();
        }
        bytes
    }

    fn manifest() -> ApplicationManifest {
        let binary = x86_elf();
        let archive = archive_bytes(&[
            ("runner", &binary),
            ("identity.json", b"{}"),
            ("manifest.json", b"{}"),
        ]);
        ApplicationManifest {
            schema: PRODUCT_MANIFEST_SCHEMA.to_owned(),
            product_id: "example".to_owned(),
            channel: "stable".to_owned(),
            version: "1.2.3".to_owned(),
            source_repository: "owner/example".to_owned(),
            source_ref: "refs/tags/v1.2.3".to_owned(),
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            release_tag: "v1.2.3".to_owned(),
            release_id: "123456789".to_owned(),
            artifacts: vec![
                ApplicationArtifact {
                    name: "runner-x86_64-unknown-linux-gnu".to_owned(),
                    target: "x86_64-unknown-linux-gnu".to_owned(),
                    kind: "binary".to_owned(),
                    sha256: hex_lower(&Sha256::digest(&binary)),
                    size: binary.len() as u64,
                },
                ApplicationArtifact {
                    name: "runner-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
                    target: "x86_64-unknown-linux-gnu".to_owned(),
                    kind: "archive".to_owned(),
                    sha256: hex_lower(&Sha256::digest(&archive)),
                    size: archive.len() as u64,
                },
                ApplicationArtifact {
                    name: "runner-x86_64-unknown-linux-gnu.deb".to_owned(),
                    target: "x86_64-unknown-linux-gnu".to_owned(),
                    kind: "apt-package".to_owned(),
                    sha256: hex_lower(&Sha256::digest(b"deb")),
                    size: 3,
                },
            ],
            components: vec![ApplicationComponent {
                name: "runner".to_owned(),
                crate_name: "runner".to_owned(),
                feature: None,
                identity: "version".to_owned(),
                version: "0.1.0".to_owned(),
                binary: "runner".to_owned(),
                targets: vec!["x86_64-unknown-linux-gnu".to_owned()],
            }],
        }
    }

    fn component_contract() -> NativeProductContract {
        NativeProductContract {
            schema: PRODUCT_COMPONENT_CONTRACT_SCHEMA.to_owned(),
            product_id: "example".to_owned(),
            channel: "stable".to_owned(),
            manifest_schema: PRODUCT_MANIFEST_SCHEMA.to_owned(),
            archive_component: "runner".to_owned(),
            archive_identity_schema: "velnor.identity/v1".to_owned(),
            archive_manifest_schema: "velnor.archive/v1".to_owned(),
            targets: vec!["x86_64-unknown-linux-gnu".to_owned()],
            blocked_targets: Vec::new(),
            components: vec![ProductComponentContract {
                name: "runner".to_owned(),
                crate_name: "runner".to_owned(),
                binary: "runner".to_owned(),
                feature: None,
                identity: "version".to_owned(),
                version: "0.1.0".to_owned(),
                targets: vec!["x86_64-unknown-linux-gnu".to_owned()],
            }],
        }
    }

    #[test]
    fn typed_component_contract_binds_crate_binary_version_and_targets() {
        let value = manifest();
        let contract = component_contract();
        assert_eq!(value.verify_typed_profile(&contract), Ok(()));

        let mut mismatch = contract.clone();
        mismatch.components[0].crate_name = "other".to_owned();
        assert_eq!(
            value.verify_typed_profile(&mismatch),
            Err(ApplicationManifestError::Contract)
        );
        mismatch = contract.clone();
        mismatch.components[0].version = "0.2.0".to_owned();
        assert_eq!(
            value.verify_typed_profile(&mismatch),
            Err(ApplicationManifestError::Contract)
        );
        mismatch = contract.clone();
        mismatch.components[0].identity = "revision".to_owned();
        assert_eq!(
            value.verify_typed_profile(&mismatch),
            Err(ApplicationManifestError::Contract)
        );
    }

    #[test]
    fn canonical_digest_is_external_and_order_stable() {
        let mut value = manifest();
        value.components[0].targets.reverse();
        let bytes = value.to_canonical_json();
        assert!(!bytes.contains("manifest_sha256"));
        let digest = value.digest();
        assert!(ApplicationManifest::verify_bytes(bytes.as_bytes(), &digest).is_ok());
    }

    #[test]
    fn canonical_component_rows_preserve_typed_fields() {
        let value = serde_json::to_value(&manifest().components[0]).unwrap();
        let fields = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            fields,
            BTreeSet::from([
                "binary", "crate", "feature", "identity", "name", "targets", "version"
            ])
        );
    }

    #[test]
    fn typed_profile_rejects_missing_or_extra_inventory() {
        let mut value = manifest();
        value.artifacts[0].name = "runner-x86_64-unknown-linux-gnu".to_owned();
        let targets = vec!["x86_64-unknown-linux-gnu".to_owned()];
        let components = vec!["runner".to_owned()];
        let profile_result = value.verify_profile(&targets, &components);
        assert_eq!(profile_result, Ok(()));
        value
            .artifacts
            .retain(|artifact| artifact.kind != "archive");
        assert_eq!(
            value.verify_profile(&targets, &components),
            Err(ApplicationManifestError::ArchiveMissing)
        );
        value = manifest();
        value.artifacts[0].name = "runner-x86_64-unknown-linux-gnu".to_owned();
        let extra_target = vec![
            "x86_64-unknown-linux-gnu".to_owned(),
            "aarch64-unknown-linux-gnu".to_owned(),
        ];
        assert_eq!(
            value.verify_profile(&extra_target, &components),
            Err(ApplicationManifestError::ArtifactInventory)
        );
    }

    #[test]
    fn provider_release_id_rejects_local_aliases_and_leading_zeroes() {
        let mut value = manifest();
        value.release_id = "owner/example/v1.2.3".to_owned();
        assert_eq!(
            value.verify(),
            Err(ApplicationManifestError::Field("identity"))
        );
        value.release_id = "000123".to_owned();
        assert_eq!(
            value.verify(),
            Err(ApplicationManifestError::Field("identity"))
        );
        value.release_id = "123".to_owned();
        assert!(value.verify().is_ok());
    }

    #[test]
    fn binary_architecture_mismatch_is_rejected_without_execution() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-product-architecture-{}-{}",
            std::process::id(),
            nanos
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        let mut elf_x86 = vec![0; 32];
        elf_x86[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        elf_x86[18..20].copy_from_slice(&[0x3e, 0x00]);
        assert!(fs::write(root.join("runner"), elf_x86).is_ok());
        assert_eq!(
            verify_binary_architecture(&root.join("runner"), "aarch64-unknown-linux-gnu"),
            Err(ApplicationManifestError::Architecture)
        );
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn development_component_version_is_rejected() {
        let mut value = manifest();
        value.components[0].version = "development".to_owned();
        assert_eq!(
            value.verify(),
            Err(ApplicationManifestError::Field("components"))
        );
    }

    #[test]
    fn incomplete_sibling_inventory_is_rejected() {
        let mut value = manifest();
        value.components[0]
            .targets
            .push("aarch64-unknown-linux-gnu".to_owned());
        assert_eq!(
            value.verify(),
            Err(ApplicationManifestError::ComponentArtifact)
        );
    }

    #[test]
    fn source_identity_mismatch_is_rejected() {
        let mut value = manifest();
        value.source_ref = "refs/tags/v9.9.9".to_owned();
        assert_eq!(
            value.verify(),
            Err(ApplicationManifestError::Field("source_ref"))
        );
    }

    #[test]
    fn artifact_digest_and_size_are_verified_without_execution() {
        let value = manifest();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-product-manifest-{}-{}",
            std::process::id(),
            nanos
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu"), x86_elf()).is_ok());
        assert!(fs::write(
            root.join("runner-x86_64-unknown-linux-gnu.tar.gz"),
            archive_bytes(&[
                ("runner", &x86_elf()),
                ("identity.json", b"{}"),
                ("manifest.json", b"{}"),
            ])
        )
        .is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.deb"), b"deb").is_ok());
        assert!(value.verify_artifacts(&root).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu"), b"changed").is_ok());
        assert_eq!(
            value.verify_artifacts(&root),
            Err(ApplicationManifestError::ArtifactSize)
        );
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn archive_member_bytes_must_match_the_sibling_binary_digest() {
        let mut value = manifest();
        let mut altered = x86_elf();
        altered[31] = 1;
        let archive = archive_bytes(&[
            ("runner", &altered),
            ("identity.json", b"{}"),
            ("manifest.json", b"{}"),
        ]);
        value.artifacts[1].sha256 = hex_lower(&Sha256::digest(&archive));
        value.artifacts[1].size = archive.len() as u64;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-product-archive-digest-{}-{}",
            std::process::id(),
            nanos
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu"), x86_elf()).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.tar.gz"), archive).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.deb"), b"deb").is_ok());
        assert_eq!(
            value.verify_artifacts(&root),
            Err(ApplicationManifestError::ArchiveUnsafe)
        );
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn duplicate_archive_components_are_rejected_before_map_conversion() {
        let mut value = manifest();
        let contract = component_contract();
        let identity_bytes = serde_json::to_vec(&serde_json::json!({
            "schema": contract.archive_identity_schema,
            "product_id": value.product_id,
            "channel": value.channel,
            "version": value.version,
            "source_repository": value.source_repository,
            "source_ref": value.source_ref,
            "source_commit": value.source_commit,
            "release_tag": value.release_tag,
            "parent_manifest_id": value.release_id,
        }))
        .unwrap();
        let component = serde_json::json!({
            "name": "runner",
            "crate": "runner",
            "crate_version": "0.1.0",
            "release_version": "1.2.3",
            "source_commit": "0123456789abcdef0123456789abcdef01234567",
            "binary_sha256": hex_lower(&Sha256::digest(x86_elf())),
        });
        let archive_manifest_bytes = serde_json::to_vec(&serde_json::json!({
            "schema": contract.archive_manifest_schema,
            "product_id": value.product_id,
            "channel": value.channel,
            "version": value.version,
            "source_repository": value.source_repository,
            "source_ref": value.source_ref,
            "source_commit": value.source_commit,
            "release_tag": value.release_tag,
            "parent_manifest_id": value.release_id,
            "components": [component.clone(), component],
        }))
        .unwrap();
        let archive = archive_bytes(&[
            ("runner", &x86_elf()),
            ("identity.json", &identity_bytes),
            ("manifest.json", &archive_manifest_bytes),
        ]);
        value.artifacts[1].sha256 = hex_lower(&Sha256::digest(&archive));
        value.artifacts[1].size = archive.len() as u64;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-product-duplicate-component-{}-{}",
            std::process::id(),
            nanos
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu"), x86_elf()).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.tar.gz"), archive).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.deb"), b"deb").is_ok());
        assert_eq!(
            value.verify_artifacts_with_contract(&root, Some(&contract)),
            Err(ApplicationManifestError::ArchiveUnsafe)
        );
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn unsafe_archive_members_are_rejected_without_extraction() {
        let mut value = manifest();
        let unsafe_archive = archive_bytes(&[
            ("../escape", b"binary"),
            ("identity.json", b"{}"),
            ("manifest.json", b"{}"),
        ]);
        value.artifacts[1].sha256 = hex_lower(&Sha256::digest(&unsafe_archive));
        value.artifacts[1].size = unsafe_archive.len() as u64;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-product-unsafe-archive-{}-{}",
            std::process::id(),
            nanos
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu"), x86_elf()).is_ok());
        assert!(fs::write(
            root.join("runner-x86_64-unknown-linux-gnu.tar.gz"),
            unsafe_archive
        )
        .is_ok());
        assert!(fs::write(root.join("runner-x86_64-unknown-linux-gnu.deb"), b"deb").is_ok());
        assert_eq!(
            value.verify_artifacts(&root),
            Err(ApplicationManifestError::ArchiveUnsafe)
        );
        assert!(fs::read(root.join("escape")).is_err());
        assert!(fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn self_referential_manifest_artifact_is_rejected() {
        let mut value = manifest();
        value.artifacts[0].name = PRODUCT_MANIFEST_FILE.to_owned();
        assert_eq!(value.verify(), Err(ApplicationManifestError::SelfArtifact));
    }
}

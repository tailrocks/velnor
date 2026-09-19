//! The producer-owned application release manifest.
//!
//! A product release has one authority.  Package feeds and formulae consume
//! this manifest (or a projection of it); they do not discover a second
//! manifest by filename.  The manifest deliberately has no
//! `manifest_sha256` member: a document cannot contain the digest of the
//! bytes it is part of.  Publishers keep that digest in a sibling checksum or
//! release record instead.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::release::EmbeddedIdentity;

/// The only product-manifest schema accepted by current consumers.
pub const PRODUCT_MANIFEST_SCHEMA: &str = "velnor.product-manifest/v1";
/// The producer's one authoritative manifest filename.
pub const PRODUCT_MANIFEST_FILE: &str = "product-manifest.json";
/// The external digest sidecar for [`PRODUCT_MANIFEST_FILE`].
pub const PRODUCT_MANIFEST_DIGEST_FILE: &str = "product-manifest.json.sha256";

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
    pub version: String,
    pub binary: String,
    pub targets: Vec<String>,
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
    #[error("application manifest is not canonical JSON")]
    NonCanonical,
    #[error("application manifest digest does not match its bytes")]
    Digest,
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
            || !safe_release_id(&self.release_id)
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
                || !safe_slug(&artifact.kind)
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
        for component in &self.components {
            if !safe_slug(&component.name)
                || !safe_slug(&component.crate_name)
                || !safe_version(&component.version)
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
        Ok(())
    }

    /// Verify canonical bytes and the optional externally supplied digest.
    pub fn verify_bytes(
        bytes: &[u8],
        expected_digest: Option<&str>,
    ) -> Result<Self, ApplicationManifestError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| ApplicationManifestError::NonCanonical)?;
        manifest.verify()?;
        if manifest.to_canonical_json().as_bytes() != bytes {
            return Err(ApplicationManifestError::NonCanonical);
        }
        if expected_digest.is_some_and(|expected| expected != manifest.digest()) {
            return Err(ApplicationManifestError::Digest);
        }
        Ok(manifest)
    }

    /// Hash every listed artifact beneath `root`, rejecting missing, changed,
    /// or truncated bytes.  This is intentionally a filesystem-only check;
    /// it never starts a daemon, installs a package, or runs a product binary.
    pub fn verify_artifacts(&self, root: &Path) -> Result<(), ApplicationManifestError> {
        self.verify()?;
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
        }
        Ok(())
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

fn safe_slug(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

fn safe_release_id(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/' | b':')
        })
}

fn safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_' | b'~')
        })
}

fn stable_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
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
        && run.bytes().all(|byte| byte.is_ascii_digit())
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn manifest() -> ApplicationManifest {
        let binary = b"runner";
        ApplicationManifest {
            schema: PRODUCT_MANIFEST_SCHEMA.to_owned(),
            product_id: "example".to_owned(),
            channel: "stable".to_owned(),
            version: "1.2.3".to_owned(),
            source_repository: "owner/example".to_owned(),
            source_ref: "refs/tags/v1.2.3".to_owned(),
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            release_tag: "v1.2.3".to_owned(),
            release_id: "owner/example/v1.2.3".to_owned(),
            artifacts: vec![ApplicationArtifact {
                name: "runner".to_owned(),
                target: "aarch64-apple-darwin".to_owned(),
                kind: "binary".to_owned(),
                sha256: hex_lower(&Sha256::digest(binary)),
                size: binary.len() as u64,
            }],
            components: vec![ApplicationComponent {
                name: "runner".to_owned(),
                crate_name: "runner".to_owned(),
                version: "0.1.0".to_owned(),
                binary: "runner".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
            }],
        }
    }

    #[test]
    fn canonical_digest_is_external_and_order_stable() {
        let mut value = manifest();
        value.components[0].targets.reverse();
        let bytes = value.to_canonical_json();
        assert!(!bytes.contains("manifest_sha256"));
        assert_eq!(
            ApplicationManifest::verify_bytes(bytes.as_bytes(), Some(&value.digest())).is_ok(),
            true
        );
    }

    #[test]
    fn incomplete_sibling_inventory_is_rejected() {
        let mut value = manifest();
        value.components[0]
            .targets
            .push("x86_64-unknown-linux-gnu".to_owned());
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
        let root = std::env::temp_dir().join(format!(
            "velnor-product-manifest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create test directory");
        fs::write(root.join("runner"), b"runner").expect("write test artifact");
        assert!(value.verify_artifacts(&root).is_ok());
        fs::write(root.join("runner"), b"changed").expect("rewrite test artifact");
        assert_eq!(
            value.verify_artifacts(&root),
            Err(ApplicationManifestError::ArtifactSize)
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn self_referential_manifest_artifact_is_rejected() {
        let mut value = manifest();
        value.artifacts[0].name = PRODUCT_MANIFEST_FILE.to_owned();
        assert_eq!(value.verify(), Err(ApplicationManifestError::SelfArtifact));
    }
}

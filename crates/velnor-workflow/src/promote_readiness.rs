//! Local publication evidence required before renderer activation.
//!
//! Promotion changes the renderer pin and the generated tree together.  A
//! successful render therefore is not enough: every supported consumer
//! platform must already have an immutable product that the trusted publisher
//! has described.  This module is deliberately local and side-effect free.
//! It parses that publisher hand-off and validates the activation precondition;
//! it does not discover, download, or publish products.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{is_full_revision, GeneratorError};

/// Versioned schema for the local hand-off from the trusted publisher to
/// `promote`.
pub(crate) const PUBLICATION_READINESS_SCHEMA: &str = "velnor-workflow.publication-readiness.v2";

/// The only workflow allowed to publish renderer products.  The repository is
/// derived from the pinned setup action coordinate so this validator cannot be
/// redirected to a caller-selected repository.
const PUBLISHER_WORKFLOW_FILE: &str = "ci-runtime-products.yml";

/// The consumer platforms for which a renderer product must be available
/// before its pin can become active.  These are product compatibility
/// identities, not runner labels.
const REQUIRED_PLATFORMS: [PublicationPlatform; 3] = [
    PublicationPlatform::LinuxX64,
    PublicationPlatform::LinuxArm64,
    PublicationPlatform::MacosArm64,
];

/// A supported renderer consumer platform.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum PublicationPlatform {
    #[serde(rename = "Linux-X64")]
    LinuxX64,
    #[serde(rename = "Linux-ARM64")]
    LinuxArm64,
    #[serde(rename = "macOS-ARM64")]
    MacosArm64,
}

impl PublicationPlatform {
    fn as_str(self) -> &'static str {
        match self {
            Self::LinuxX64 => "Linux-X64",
            Self::LinuxArm64 => "Linux-ARM64",
            Self::MacosArm64 => "macOS-ARM64",
        }
    }
}

/// One immutable, publisher-verified renderer product.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationProduct {
    /// The platform key is explicit so an artifact cannot be rebound by a
    /// caller that merely changes a map key or asset name.
    pub(crate) platform: PublicationPlatform,
    /// SHA-256 of the exact executable bytes consumers retrieve.
    pub(crate) digest: String,
    /// Publisher-side revocation state.  A revoked product is never usable,
    /// even when its bytes and other identity fields still match.
    pub(crate) revoked: bool,
    /// Absolute Unix time in seconds after which this evidence is unusable.
    pub(crate) expires_at: u64,
}

/// The exact trusted workflow run that published the immutable products.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationPublisher {
    /// Immutable repository workflow reference used for attestation.
    pub(crate) workflow: String,
    /// Repository that owns the publisher workflow.
    pub(crate) repository: String,
    /// GitHub Actions workflow run that produced the publication.
    pub(crate) run_id: u64,
    /// Attempt within the workflow run that produced the publication.
    pub(crate) run_attempt: u64,
}

/// Product identity carried by the trusted publication manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationProductIdentity {
    /// Source/build-input closure named by the immutable product.
    pub(crate) closure: String,
    /// Source commit from which the immutable product was built.
    pub(crate) revision: String,
}

/// The trusted publisher's local readiness hand-off.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationReadinessManifest {
    pub(crate) schema: String,
    /// Renderer commit the activation transaction will stamp.
    pub(crate) activation_revision: String,
    /// The trusted publisher run that produced the manifest and products.
    pub(crate) publisher: PublicationPublisher,
    /// Source/build identity of the immutable product.
    pub(crate) product: PublicationProductIdentity,
    /// SHA-256 over the canonical readiness claims.  The digest excludes
    /// itself and therefore binds the publisher and product identity instead
    /// of merely accepting a well-shaped `product_revision` string.
    pub(crate) manifest_digest: String,
    /// One complete product row for every supported consumer platform.
    pub(crate) products: Vec<PublicationProduct>,
}

impl PublicationReadinessManifest {
    /// Read the publisher hand-off from a caller-selected local file.  The
    /// file is evidence only; this function performs no network or artifact
    /// lookup.
    pub(crate) fn from_file(path: &Path) -> Result<Self, GeneratorError> {
        let bytes = std::fs::read(path).map_err(|error| {
            GeneratorError::io("read publication readiness manifest", path, &error)
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "publication readiness manifest {} is invalid JSON: {error}",
                path.display()
            ))
        })
    }

    /// Validate every activation precondition against the exact renderer
    /// closure and revision that promotion is about to stamp.
    ///
    /// `now_unix_seconds` is supplied by the caller so hosted checks can use a
    /// trusted clock and tests can exercise expiry without sleeping.
    pub(crate) fn validate_for_activation(
        &self,
        expected_closure: &str,
        expected_revision: &str,
        now_unix_seconds: u64,
    ) -> Result<(), GeneratorError> {
        if self.schema != PUBLICATION_READINESS_SCHEMA {
            return Err(GeneratorError::usage(format!(
                "publication readiness manifest schema must be {PUBLICATION_READINESS_SCHEMA:?}, got {:?}",
                self.schema
            )));
        }
        if !is_full_revision(expected_revision) || !is_lower_hex(expected_revision, 40) {
            return Err(GeneratorError::usage(
                "promotion expected revision must be 40 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }
        if !is_lower_hex(&self.activation_revision, 40) {
            return Err(GeneratorError::usage(
                "publication readiness activation_revision must be 40 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }
        if self.activation_revision != expected_revision {
            return Err(GeneratorError::usage(
                format!(
                    "publication readiness activation_revision {} does not match the renderer revision {}",
                    self.activation_revision, expected_revision
                ),
            ));
        }
        let trusted_repository = crate::workflow_setup_action_repository();
        if self.publisher.repository != trusted_repository {
            return Err(GeneratorError::usage(
                format!(
                    "publication readiness publisher repository {} does not match the trusted repository {}",
                    self.publisher.repository, trusted_repository
                ),
            ));
        }
        let trusted_workflow =
            format!("{trusted_repository}/.github/workflows/{PUBLISHER_WORKFLOW_FILE}");
        if self.publisher.workflow != trusted_workflow {
            return Err(GeneratorError::usage(format!(
                "publication readiness publisher workflow {} does not match the trusted workflow {}",
                self.publisher.workflow, trusted_workflow
            )));
        }
        if self.publisher.run_id == 0 {
            return Err(GeneratorError::usage(format!(
                "publication readiness publisher run_id must be non-zero, got {}",
                self.publisher.run_id
            )));
        }
        if self.publisher.run_attempt == 0 {
            return Err(GeneratorError::usage(format!(
                "publication readiness publisher run_attempt must be non-zero, got {}",
                self.publisher.run_attempt
            )));
        }
        if !is_lower_hex(&self.product.closure, 64) {
            return Err(GeneratorError::usage(
                "publication readiness product closure must be 64 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }
        if self.product.closure != expected_closure {
            return Err(GeneratorError::usage(format!(
                "publication readiness product closure {} does not match the renderer closure {}",
                self.product.closure, expected_closure
            )));
        }
        if !is_lower_hex(&self.product.revision, 40) {
            return Err(GeneratorError::usage(
                "publication readiness product revision must be 40 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }
        if !is_lower_hex(&self.manifest_digest, 64) {
            return Err(GeneratorError::usage(
                "publication readiness manifest_digest must be 64 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }

        let mut seen = BTreeSet::new();
        for product in &self.products {
            if !seen.insert(product.platform) {
                return Err(GeneratorError::usage(format!(
                    "publication readiness lists platform {} more than once",
                    product.platform.as_str()
                )));
            }
            if !is_lower_hex(&product.digest, 64) {
                return Err(GeneratorError::usage(format!(
                    "publication readiness digest for {} must be 64 lowercase hexadecimal characters",
                    product.platform.as_str()
                )));
            }
            if product.revoked {
                return Err(GeneratorError::usage(format!(
                    "publication readiness product for {} is revoked",
                    product.platform.as_str()
                )));
            }
            if product.expires_at <= now_unix_seconds {
                return Err(GeneratorError::usage(format!(
                    "publication readiness product for {} expired at {}",
                    product.platform.as_str(),
                    product.expires_at
                )));
            }
        }

        let required = REQUIRED_PLATFORMS.into_iter().collect::<BTreeSet<_>>();
        if seen != required {
            let missing = required
                .difference(&seen)
                .map(|platform| platform.as_str())
                .collect::<Vec<_>>();
            let unexpected = seen
                .difference(&required)
                .map(|platform| platform.as_str())
                .collect::<Vec<_>>();
            let mut reason = Vec::new();
            if !missing.is_empty() {
                reason.push(format!("missing {}", missing.join(", ")));
            }
            if !unexpected.is_empty() {
                reason.push(format!("unexpected {}", unexpected.join(", ")));
            }
            return Err(GeneratorError::usage(format!(
                "publication readiness must contain exactly the supported products ({})",
                reason.join("; ")
            )));
        }

        let expected_manifest_digest = self.canonical_digest()?;
        if self.manifest_digest != expected_manifest_digest {
            return Err(GeneratorError::usage(format!(
                "publication readiness manifest_digest {} does not match canonical claims {}",
                self.manifest_digest, expected_manifest_digest
            )));
        }
        Ok(())
    }

    /// Compute the digest over every readiness claim except the digest field
    /// itself. Products are sorted by platform so JSON array order cannot
    /// create a second identity for the same publication.
    fn canonical_digest(&self) -> Result<String, GeneratorError> {
        #[derive(Serialize)]
        struct Canonical<'a> {
            schema: &'a str,
            activation_revision: &'a str,
            publisher: &'a PublicationPublisher,
            product: &'a PublicationProductIdentity,
            products: Vec<PublicationProduct>,
        }

        let mut products = self.products.clone();
        products.sort_by_key(|product| product.platform);
        let canonical = serde_json::to_vec(&Canonical {
            schema: &self.schema,
            activation_revision: &self.activation_revision,
            publisher: &self.publisher,
            product: &self.product,
            products,
        })
        .map_err(|error| {
            GeneratorError::usage(format!(
                "serialize publication readiness canonical claims: {error}"
            ))
        })?;
        Ok(sha256_hex(&canonical))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use super::*;

    const CLOSURE: &str = "c";
    const REVISION: &str = "a";
    const DIGEST: &str = "d";
    const MANIFEST_DIGEST: &str = "e";

    fn valid_manifest() -> PublicationReadinessManifest {
        let mut manifest = PublicationReadinessManifest {
            schema: PUBLICATION_READINESS_SCHEMA.to_owned(),
            activation_revision: REVISION.repeat(40),
            publisher: PublicationPublisher {
                workflow: trusted_publisher_workflow(),
                repository: crate::workflow_setup_action_repository().to_owned(),
                run_id: 42,
                run_attempt: 1,
            },
            product: PublicationProductIdentity {
                closure: CLOSURE.repeat(64),
                revision: "b".repeat(40),
            },
            manifest_digest: String::new(),
            products: REQUIRED_PLATFORMS
                .into_iter()
                .map(|platform| PublicationProduct {
                    platform,
                    digest: DIGEST.repeat(64),
                    revoked: false,
                    expires_at: 101,
                })
                .collect(),
        };
        manifest.manifest_digest = match manifest.canonical_digest() {
            Ok(digest) => digest,
            Err(error) => panic!("valid readiness fixture must serialize: {error}"),
        };
        manifest
    }

    fn trusted_publisher_workflow() -> String {
        format!(
            "{}/.github/workflows/{PUBLISHER_WORKFLOW_FILE}",
            crate::workflow_setup_action_repository()
        )
    }

    fn must_fail(manifest: &PublicationReadinessManifest, now: u64) -> String {
        match manifest.validate_for_activation(&CLOSURE.repeat(64), &REVISION.repeat(40), now) {
            Ok(()) => panic!("expected readiness validation to fail"),
            Err(error) => error.to_string(),
        }
    }

    fn must_err<T, E>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: unexpectedly succeeded"),
            Err(error) => error,
        }
    }

    #[test]
    fn complete_manifest_matches_exact_activation_identity() {
        assert!(valid_manifest()
            .validate_for_activation(&CLOSURE.repeat(64), &REVISION.repeat(40), 100)
            .is_ok());
    }

    #[test]
    fn closure_equivalent_activation_preserves_product_provenance() {
        let manifest = valid_manifest();
        assert_ne!(
            manifest.activation_revision, manifest.product.revision,
            "the fixture models a reusable product built from another closure-equivalent commit"
        );
        assert!(manifest
            .validate_for_activation(&CLOSURE.repeat(64), &REVISION.repeat(40), 100)
            .is_ok());
    }

    #[test]
    fn malformed_identity_and_digest_fail_closed() {
        let mut manifest = valid_manifest();
        manifest.activation_revision = "R".repeat(40);
        assert!(must_fail(&manifest, 100).contains("activation_revision"));

        let mut manifest = valid_manifest();
        manifest.product.closure = "C".repeat(64);
        assert!(must_fail(&manifest, 100).contains("product closure"));

        let mut manifest = valid_manifest();
        manifest.product.revision = "R".repeat(40);
        assert!(must_fail(&manifest, 100).contains("product revision"));

        let mut manifest = valid_manifest();
        manifest.products[0].digest = "not-a-digest".to_owned();
        assert!(must_fail(&manifest, 100).contains("digest"));
    }

    #[test]
    fn forged_or_mismatched_publisher_identity_is_rejected() {
        let mut manifest = valid_manifest();
        manifest.publisher.repository = "attacker/velnor".to_owned();
        assert!(must_fail(&manifest, 100).contains("publisher repository"));

        let mut manifest = valid_manifest();
        manifest.publisher.workflow = format!(
            "{}/.github/workflows/forged.yml",
            crate::workflow_setup_action_repository()
        );
        assert!(must_fail(&manifest, 100).contains("publisher workflow"));

        let mut manifest = valid_manifest();
        manifest.publisher.run_id = 0;
        assert!(must_fail(&manifest, 100).contains("run_id"));

        let mut manifest = valid_manifest();
        manifest.publisher.run_attempt = 0;
        assert!(must_fail(&manifest, 100).contains("run_attempt"));
    }

    #[test]
    fn wrong_manifest_digest_is_rejected_even_when_well_shaped() {
        let mut manifest = valid_manifest();
        manifest.manifest_digest = MANIFEST_DIGEST.repeat(64);
        let error = must_fail(&manifest, 100);
        assert!(error.contains("manifest_digest"), "{error}");
        assert!(error.contains("canonical claims"), "{error}");

        let mut manifest = valid_manifest();
        manifest.product.revision = "f".repeat(40);
        let error = must_fail(&manifest, 100);
        assert!(error.contains("manifest_digest"), "{error}");
        assert!(error.contains("canonical claims"), "{error}");
    }

    #[test]
    fn legacy_schema_is_rejected_without_a_compatibility_alias() {
        let mut manifest = valid_manifest();
        manifest.schema = "velnor-workflow.publication-readiness.v1".to_owned();
        assert!(must_fail(&manifest, 100).contains("schema"));
    }

    #[test]
    fn mismatched_identity_is_rejected() {
        let manifest = valid_manifest();
        let error = must_err(
            manifest.validate_for_activation(&"e".repeat(64), &REVISION.repeat(40), 100),
            "a different closure cannot activate",
        );
        assert!(error.to_string().contains("does not match"));

        let error = must_err(
            manifest.validate_for_activation(&CLOSURE.repeat(64), &"e".repeat(40), 100),
            "a different revision cannot activate",
        );
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn revoked_or_expired_product_is_rejected() {
        let mut manifest = valid_manifest();
        manifest.products[0].revoked = true;
        assert!(must_fail(&manifest, 100).contains("revoked"));

        let mut manifest = valid_manifest();
        manifest.products[0].expires_at = 100;
        assert!(must_fail(&manifest, 100).contains("expired"));
    }

    #[test]
    fn incomplete_and_duplicate_platform_sets_are_rejected() {
        let mut manifest = valid_manifest();
        manifest.products.pop();
        assert!(must_fail(&manifest, 100).contains("exactly"));

        let mut manifest = valid_manifest();
        manifest.products[1].platform = manifest.products[0].platform;
        assert!(must_fail(&manifest, 100).contains("more than once"));
    }

    #[test]
    fn malformed_json_and_unknown_fields_fail_closed_before_activation() {
        let root = std::env::temp_dir().join(format!(
            "velnor-publication-readiness-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture directory: {error}"));
        let path = root.join("manifest.json");
        let bytes = br#"{"schema":"velnor-workflow.publication-readiness.v1","closure":"x","revision":"y","products":[],"extra":true}"#;
        std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("write manifest: {error}"));
        let before = std::fs::read(&path).unwrap_or_else(|error| panic!("read fixture: {error}"));
        let error = must_err(
            PublicationReadinessManifest::from_file(&path),
            "unknown fields are not part of the contract",
        );
        assert!(error.to_string().contains("invalid JSON"));
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|error| panic!("read fixture: {error}")),
            before
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

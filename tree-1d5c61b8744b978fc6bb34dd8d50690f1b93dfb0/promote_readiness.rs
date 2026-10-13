//! Local publication evidence required before renderer activation.
//!
//! Promotion changes the renderer pin and the generated tree together.  A
//! successful render therefore is not enough: every supported consumer
//! platform must already have an immutable product that the trusted publisher
//! has described.  This module is deliberately local and side-effect free.
//! It parses that publisher hand-off and validates the activation precondition;
//! it does not discover, download, or publish products.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use super::{is_full_revision, GeneratorError};

/// Versioned schema for the local hand-off from the trusted publisher to
/// `promote`.
pub(crate) const PUBLICATION_READINESS_SCHEMA: &str = "velnor-workflow.publication-readiness.v1";

/// The consumer platforms for which a renderer product must be available
/// before its pin can become active.  These are product compatibility
/// identities, not runner labels.
const REQUIRED_PLATFORMS: [PublicationPlatform; 3] = [
    PublicationPlatform::LinuxX64,
    PublicationPlatform::LinuxArm64,
    PublicationPlatform::MacosArm64,
];

/// A supported renderer consumer platform.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
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

/// The trusted publisher's local readiness hand-off.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationReadinessManifest {
    pub(crate) schema: String,
    /// Source/build-input identity of the renderer product.
    pub(crate) closure: String,
    /// Renderer commit the activation transaction will stamp.
    pub(crate) activation_revision: String,
    /// Original source commit the immutable product was built from. This is
    /// deliberately distinct from `activation_revision`: closure-equivalent
    /// commits may reuse a product without rewriting its provenance.
    pub(crate) product_revision: String,
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
        if !is_lower_hex(&self.closure, 64) {
            return Err(GeneratorError::usage(
                "publication readiness closure must be 64 lowercase hexadecimal characters"
                    .to_owned(),
            ));
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
        if !is_lower_hex(&self.product_revision, 40) {
            return Err(GeneratorError::usage(
                "publication readiness product_revision must be 40 lowercase hexadecimal characters"
                    .to_owned(),
            ));
        }
        if self.closure != expected_closure {
            return Err(GeneratorError::usage(format!(
                "publication readiness closure {} does not match the renderer closure {}",
                self.closure, expected_closure
            )));
        }
        if self.activation_revision != expected_revision {
            return Err(GeneratorError::usage(format!(
                "publication readiness activation_revision {} does not match the renderer revision {}",
                self.activation_revision, expected_revision
            )));
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
        Ok(())
    }
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

    fn valid_manifest() -> PublicationReadinessManifest {
        PublicationReadinessManifest {
            schema: PUBLICATION_READINESS_SCHEMA.to_owned(),
            closure: CLOSURE.repeat(64),
            activation_revision: REVISION.repeat(40),
            product_revision: "b".repeat(40),
            products: REQUIRED_PLATFORMS
                .into_iter()
                .map(|platform| PublicationProduct {
                    platform,
                    digest: DIGEST.repeat(64),
                    revoked: false,
                    expires_at: 101,
                })
                .collect(),
        }
    }

    fn must_fail(manifest: &PublicationReadinessManifest, now: u64) -> String {
        match manifest.validate_for_activation(&CLOSURE.repeat(64), &REVISION.repeat(40), now) {
            Ok(()) => panic!("expected readiness validation to fail"),
            Err(error) => error.to_string(),
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
            manifest.activation_revision, manifest.product_revision,
            "the fixture models a reusable product built from another closure-equivalent commit"
        );
        assert!(manifest
            .validate_for_activation(&CLOSURE.repeat(64), &REVISION.repeat(40), 100)
            .is_ok());
    }

    #[test]
    fn malformed_identity_and_digest_fail_closed() {
        let mut manifest = valid_manifest();
        manifest.closure = "C".repeat(64);
        assert!(must_fail(&manifest, 100).contains("closure"));

        let mut manifest = valid_manifest();
        manifest.activation_revision = "R".repeat(40);
        assert!(must_fail(&manifest, 100).contains("activation_revision"));

        let mut manifest = valid_manifest();
        manifest.product_revision = "R".repeat(40);
        assert!(must_fail(&manifest, 100).contains("product_revision"));

        let mut manifest = valid_manifest();
        manifest.products[0].digest = "not-a-digest".to_owned();
        assert!(must_fail(&manifest, 100).contains("digest"));
    }

    #[test]
    fn mismatched_identity_is_rejected() {
        let manifest = valid_manifest();
        let error = manifest
            .validate_for_activation(&"e".repeat(64), &REVISION.repeat(40), 100)
            .expect_err("a different closure cannot activate");
        assert!(error.to_string().contains("does not match"));

        let error = manifest
            .validate_for_activation(&CLOSURE.repeat(64), &"e".repeat(40), 100)
            .expect_err("a different revision cannot activate");
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
        let error = PublicationReadinessManifest::from_file(&path)
            .expect_err("unknown fields are not part of the contract");
        assert!(error.to_string().contains("invalid JSON"));
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|error| panic!("read fixture: {error}")),
            before
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

//! Producer-owned authority boundary for live evidence.
//!
//! The checker does not deserialize a caller capture and call it live.  A
//! producer implementation must replace `UnavailableCollector` with an
//! authenticated, read-only collector that performs the closing reconciliation
//! and returns typed documents plus a verified raw-store handle.  Keeping this
//! boundary explicit lets the checker compile against the producer contract
//! while the capability is still safely unavailable.

use crate::evidence_check::{
    CanonicalReleaseDocument, EvidenceCheckInput, EvidenceDocument, ManifestDocument,
    SnapshotDocument,
};
use crate::g0_contract::G0InventoryEvidence;
use anyhow::{bail, Result};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Versioned seam between the authenticated producer and deterministic
/// checker.  A producer must implement this exact contract before live mode
/// can be enabled.
pub(crate) const AUTHORITY_INTERFACE: &str = "velnor.authenticated-closing/v1";

#[derive(Debug, Clone)]
pub(crate) struct ClosingCaptureRequest {
    pub(crate) stage: String,
    pub(crate) reviewed_manifest: PathBuf,
}

impl ClosingCaptureRequest {
    pub(crate) fn from_input(input: &EvidenceCheckInput) -> Self {
        Self {
            stage: input.stage.clone(),
            reviewed_manifest: input.manifest.clone(),
        }
    }
}

/// Store implementation supplied by the producer's one canonical raw-object
/// store. The method must reopen and verify all referenced bytes; digest and
/// URI shape alone are not an implementation.
pub(crate) trait VerifiedRawStore: Send + Sync {
    fn verify_g0(&self, inventory: &G0InventoryEvidence) -> Result<()>;
}

pub(crate) struct VerifiedRawStoreHandle {
    verifier: Box<dyn VerifiedRawStore>,
}

impl VerifiedRawStoreHandle {
    /// Producer adapter constructor.  The caller must pass the production
    /// descriptor-relative store, never a deserialized path or caller digest.
    #[allow(dead_code, reason = "used by the producer adapter when integrated")]
    pub(super) fn from_producer(verifier: Box<dyn VerifiedRawStore>) -> Self {
        Self { verifier }
    }

    pub(crate) fn verify_g0(&self, inventory: &G0InventoryEvidence) -> Result<()> {
        self.verifier.verify_g0(inventory)
    }
}

/// Capture returned by an authenticated producer. Its fields are private so a
/// caller cannot construct a trusted capture from JSON at the command edge.
pub(crate) struct AuthenticatedClosingCapture {
    manifest: ManifestDocument,
    snapshot: SnapshotDocument,
    evidence: EvidenceDocument,
    release: Option<CanonicalReleaseDocument>,
    raw_store: VerifiedRawStoreHandle,
}

impl AuthenticatedClosingCapture {
    /// Producer adapter constructor. The collector must have independently
    /// authenticated, captured, and closing-reconciled all returned values.
    #[allow(dead_code, reason = "used by the producer adapter when integrated")]
    pub(super) fn from_producer(
        manifest: ManifestDocument,
        snapshot: SnapshotDocument,
        evidence: EvidenceDocument,
        release: Option<CanonicalReleaseDocument>,
        raw_store: VerifiedRawStoreHandle,
    ) -> Self {
        Self {
            manifest,
            snapshot,
            evidence,
            release,
            raw_store,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ManifestDocument,
        SnapshotDocument,
        EvidenceDocument,
        Option<CanonicalReleaseDocument>,
        VerifiedRawStoreHandle,
    ) {
        (
            self.manifest,
            self.snapshot,
            self.evidence,
            self.release,
            self.raw_store,
        )
    }
}

pub(crate) type CollectorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthenticatedClosingCapture>> + Send + 'a>>;

/// Producer interface. It must collect current API facts itself; input paths
/// are only reviewed-source selectors and never an authority-bearing capture.
pub(crate) trait AuthenticatedClosingCollector: Send + Sync {
    fn collect_closing<'a>(&'a self, request: ClosingCaptureRequest) -> CollectorFuture<'a>;
}

struct UnavailableCollector;

impl AuthenticatedClosingCollector for UnavailableCollector {
    fn collect_closing<'a>(&'a self, request: ClosingCaptureRequest) -> CollectorFuture<'a> {
        Box::pin(async move {
            let _ = (&request.stage, &request.reviewed_manifest);
            bail!(
                "trusted authenticated collector/current-API reconciliation is unavailable: producer capability {AUTHORITY_INTERFACE} is not wired"
            )
        })
    }
}

/// Current capability provider. Until the producer adapter is integrated,
/// every live invocation reaches the explicit fail-closed error above.
pub(crate) fn current_collector() -> impl AuthenticatedClosingCollector {
    UnavailableCollector
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "the capability regression intentionally asserts a fail-closed error"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_capability_cannot_authorize_live_capture() {
        let result = current_collector()
            .collect_closing(ClosingCaptureRequest {
                stage: "G0".to_owned(),
                reviewed_manifest: PathBuf::from("manifest.json"),
            })
            .await;
        assert!(result.is_err());
        let error = result.err().expect("unwired producer must fail closed");
        assert!(error.to_string().contains(AUTHORITY_INTERFACE));
    }
}

//! Versioned storage accounting and reclamation resources.

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;

/// Canonical storage class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StorageClass {
    /// Persistent compiler/build target materialization.
    Target,
    /// Reusable dependency/cache content.
    Cache,
    /// Workflow artifact content.
    Artifact,
    /// Cargo registry/build data.
    Cargo,
    /// Mise tool data.
    Mise,
}

/// Ownership and lifetime of one storage object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageObject {
    /// Stable catalog identity.
    pub id: String,
    /// Canonical class.
    pub class: StorageClass,
    /// Trust/repository/job ownership scope.
    pub scope: String,
    /// Exact owner identity.
    pub owner: String,
    /// Logical bytes represented by the object.
    pub logical_bytes: u64,
    /// Physically allocated bytes, unknown when measurement is unavailable.
    pub physical_bytes: Option<u64>,
    /// Whether a current lease protects the object.
    pub active: bool,
    /// Catalog version at observation.
    pub resource_version: u64,
    /// Last catalog observation.
    pub observed_at: Timestamp,
}

/// Aggregate storage accounting for one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageSnapshot {
    /// Scope selected by the caller.
    pub scope: String,
    /// Objects included by the snapshot.
    pub objects: Vec<StorageObject>,
    /// Total logical bytes.
    pub logical_bytes: u64,
    /// Total physical bytes, unknown if any object lacks a measurement.
    pub physical_bytes: Option<u64>,
    /// Snapshot version.
    pub resource_version: u64,
    /// Observation time.
    pub observed_at: Timestamp,
}

/// One exact candidate in a reviewed GC plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GcCandidate {
    /// Catalog object id.
    pub object_id: String,
    /// Version reviewed by the planner.
    pub resource_version: u64,
    /// Expected physical reclaim, if known.
    pub physical_bytes: Option<u64>,
}

/// Immutable dry-run-first garbage-collection plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GcPlan {
    /// Unique plan id.
    pub plan_id: String,
    /// Canonical digest of the reviewed candidates.
    pub digest: String,
    /// Exact candidates; execution never recomputes them.
    pub candidates: Vec<GcCandidate>,
    /// Plan creation time.
    pub created_at: Timestamp,
    /// Expiry time.
    pub expires_at: Timestamp,
}

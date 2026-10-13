//! Conditions and the shared resource header.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::source::Source;
use crate::time::Timestamp;

/// Whether a condition is satisfied.
///
/// Serialized exactly `TRUE`, `FALSE`, or `UNKNOWN`; fail-closed on any other
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

impl ConditionStatus {
    /// Every variant, canonical order.
    pub const ALL: [ConditionStatus; 3] = [
        ConditionStatus::True,
        ConditionStatus::False,
        ConditionStatus::Unknown,
    ];

    /// Canonical serialized spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ConditionStatus::True => "TRUE",
            ConditionStatus::False => "FALSE",
            ConditionStatus::Unknown => "UNKNOWN",
        }
    }
}

/// One observed condition on a resource.
///
/// Mirrors the Kubernetes condition shape: a stable `type`, tri-state
/// `status`, machine-oriented `reason`, human `message`, and the RFC 3339
/// `lastTransitionTime` of the most recent status change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    /// Condition kind, for example `Ready` or `Registered`.
    pub kind: String,
    pub status: ConditionStatus,
    /// Stable machine reason; commands refine reasons, never codes.
    pub reason: Option<String>,
    /// Human-readable message; must never embed secret material.
    pub message: Option<String>,
    pub last_transition_time: Timestamp,
}

impl Condition {
    /// A satisfied condition with no reason or message.
    #[must_use]
    pub fn ready(kind: &str, at: Timestamp) -> Self {
        Self {
            kind: kind.to_owned(),
            status: ConditionStatus::True,
            reason: None,
            message: None,
            last_transition_time: at,
        }
    }

    /// A degraded condition carrying reason and message.
    #[must_use]
    pub fn degraded(kind: &str, reason: &str, message: &str, at: Timestamp) -> Self {
        Self {
            kind: kind.to_owned(),
            status: ConditionStatus::False,
            reason: Some(reason.to_owned()),
            message: Some(message.to_owned()),
            last_transition_time: at,
        }
    }

    /// True when renderers should surface this condition as a warning.
    #[must_use]
    pub fn is_warning(&self) -> bool {
        matches!(self.status, ConditionStatus::False)
    }
}

/// Header embedded (flattened) in every versioned resource.
///
/// Carries the schema version, stable identity, provenance, conditions,
/// reason, message, and RFC 3339 `lastTransitionTime`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceMeta {
    /// Schema version of the embedding resource. Bumped only on breaking
    /// field changes; later additions must be optional or versioned.
    pub schema_version: u32,
    /// Stable canonical name within the resource kind.
    pub name: String,
    /// Opaque unique identity when one exists upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Stable machine reason for the current summary state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Human-readable summary; must never embed secret material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub last_transition_time: Timestamp,
}

impl ResourceMeta {
    /// Build a header for `name` observed from `source`.
    #[must_use]
    pub fn new(name: &str, source: Source, last_transition_time: Timestamp) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            name: name.to_owned(),
            uid: None,
            source,
            conditions: Vec::new(),
            reason: None,
            message: None,
            last_transition_time,
        }
    }

    /// Attach a UID.
    #[must_use]
    pub fn with_uid(mut self, uid: impl Into<String>) -> Self {
        self.uid = Some(uid.into());
        self
    }

    /// Attach a summary reason and message.
    #[must_use]
    pub fn with_summary(mut self, reason: &str, message: &str) -> Self {
        self.reason = Some(reason.to_owned());
        self.message = Some(message.to_owned());
        self
    }

    /// Attach conditions.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<Condition>) -> Self {
        self.conditions = conditions;
        self
    }
}

/// Free-form labels attached to resources that carry them.
pub type Labels = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::golden_timestamp;

    #[test]
    fn condition_field_names_match_kubernetes_shape() {
        let condition = Condition::degraded(
            "Ready",
            "TokenRejected",
            "registration token expired",
            golden_timestamp(),
        );
        let json = serde_json::to_string(&condition).unwrap();
        assert_eq!(
            json,
            "{\"kind\":\"Ready\",\"status\":\"FALSE\",\"reason\":\"TokenRejected\",\
             \"message\":\"registration token expired\",\
             \"lastTransitionTime\":\"2026-08-24T12:30:45Z\"}"
        );
        assert!(serde_json::from_str::<Condition>(&json).is_ok());
    }

    #[test]
    fn condition_status_is_fail_closed() {
        assert!(serde_json::from_str::<ConditionStatus>("\"MAYBE\"").is_err());
    }

    #[test]
    fn meta_omits_absent_optionals_but_keeps_required_fields() {
        let meta = ResourceMeta::new("host-a", Source::Local, golden_timestamp());
        let json = serde_json::to_string(&meta).unwrap();
        assert_eq!(
            json,
            "{\"schemaVersion\":1,\"name\":\"host-a\",\"source\":\"LOCAL\",\
             \"lastTransitionTime\":\"2026-08-24T12:30:45Z\"}"
        );
    }

    #[test]
    fn meta_rejects_unknown_fields() {
        let json = r#"{"schemaVersion":1,"name":"x","source":"LOCAL",
            "lastTransitionTime":"2026-08-24T12:30:45Z","surprise":1}"#;
        assert!(serde_json::from_str::<ResourceMeta>(json).is_err());
    }
}

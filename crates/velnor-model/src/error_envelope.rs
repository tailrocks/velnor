//! Machine-readable error envelope and the one public [`ExitClass`]
//! contract shared by every leaf command and transport.

use serde::{Deserialize, Serialize};

/// The single exit-class contract for every command and transport.
///
/// Commands may refine reasons inside a machine envelope; they never invent
/// another numeric mapping. Transport and domain failures map to their own
/// classes so they can never collapse into usage or success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitClass {
    /// Requested operation completed, or an idempotent target already matched.
    Success,
    /// Inspection completed and authoritatively found a degraded condition.
    Condition,
    /// CLI syntax, selector, field, or local input is invalid.
    Usage,
    /// Authentication failed or the identity lacks required permission.
    Authorization,
    /// An authoritative resource is absent, unavailable, or not found.
    Unavailable,
    /// The requested deadline elapsed before a terminal result.
    Timeout,
    /// Version, state, plan, or safety precondition no longer matches.
    Conflict,
    /// Connection, rate-limit, or ambiguous upstream transport outcome.
    Transport,
    /// An accepted domain operation reached a definite failure.
    Operation,
    /// Local user interruption (`SIGINT`) stopped observation.
    Interrupted,
}

impl ExitClass {
    /// Every class in numeric order except the interrupt sentinel last.
    pub const ALL: [ExitClass; 10] = [
        ExitClass::Success,
        ExitClass::Condition,
        ExitClass::Usage,
        ExitClass::Authorization,
        ExitClass::Unavailable,
        ExitClass::Timeout,
        ExitClass::Conflict,
        ExitClass::Transport,
        ExitClass::Operation,
        ExitClass::Interrupted,
    ];

    /// Canonical `SCREAMING_SNAKE_CASE` spelling used in envelopes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExitClass::Success => "SUCCESS",
            ExitClass::Condition => "CONDITION",
            ExitClass::Usage => "USAGE",
            ExitClass::Authorization => "AUTHORIZATION",
            ExitClass::Unavailable => "UNAVAILABLE",
            ExitClass::Timeout => "TIMEOUT",
            ExitClass::Conflict => "CONFLICT",
            ExitClass::Transport => "TRANSPORT",
            ExitClass::Operation => "OPERATION",
            ExitClass::Interrupted => "INTERRUPTED",
        }
    }

    /// Numeric process exit code fixed by the plan table.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            ExitClass::Success => 0,
            ExitClass::Condition => 1,
            ExitClass::Usage => 2,
            ExitClass::Authorization => 3,
            ExitClass::Unavailable => 4,
            ExitClass::Timeout => 5,
            ExitClass::Conflict => 6,
            ExitClass::Transport => 7,
            ExitClass::Operation => 8,
            ExitClass::Interrupted => 130,
        }
    }

    /// The unique class carrying this numeric code, if any.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            0 => Some(ExitClass::Success),
            1 => Some(ExitClass::Condition),
            2 => Some(ExitClass::Usage),
            3 => Some(ExitClass::Authorization),
            4 => Some(ExitClass::Unavailable),
            5 => Some(ExitClass::Timeout),
            6 => Some(ExitClass::Conflict),
            7 => Some(ExitClass::Transport),
            8 => Some(ExitClass::Operation),
            130 => Some(ExitClass::Interrupted),
            _ => None,
        }
    }
}

/// Numeric process mapping used by every leaf command and transport.
#[must_use]
pub const fn exit_code_for_class(class: ExitClass) -> u16 {
    class.code()
}

impl From<ExitClass> for MachineErrorEnvelope {
    fn from(class: ExitClass) -> Self {
        Self::new(class.as_str(), exit_code_for_class(class), "command.failed")
    }
}

/// Envelope describing one failed operation for machines.
///
/// Carries the exit class spelling, its numeric process code, a stable
/// machine reason, the request id when one exists, and safe remediation.
/// Commands may refine reasons; they never invent another numeric mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MachineErrorEnvelope {
    /// Exit class spelled `SCREAMING_SNAKE_CASE` (for example `USAGE`).
    pub class: String,
    /// Numeric process exit code matching [`crate::exit_code_for_class`].
    pub code: u16,
    /// Stable machine reason token (dot-separated), refinable per command.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Safe remediation hint; must never embed secret material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl MachineErrorEnvelope {
    #[must_use]
    pub fn new(class: &str, code: u16, reason: &str) -> Self {
        Self {
            class: class.to_owned(),
            code,
            reason: reason.to_owned(),
            request_id: None,
            remediation: None,
        }
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    #[must_use]
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_maps_to_exactly_one_unique_code() {
        let mut seen: Vec<u16> = ExitClass::ALL.iter().map(|c| c.code()).collect();
        let count = seen.len();
        assert_eq!(count, 10);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "two classes share a numeric code");
        for class in ExitClass::ALL {
            assert_eq!(ExitClass::from_code(class.code()), Some(class));
            assert_eq!(exit_code_for_class(class), class.code());
        }
        for unmapped in [9_u16, 100, 129, 131, u16::MAX] {
            assert_eq!(ExitClass::from_code(unmapped), None);
        }
        assert_eq!(ExitClass::Interrupted.code(), 130);
    }

    #[test]
    fn spellings_are_screaming_and_stable() {
        assert_eq!(ExitClass::Usage.as_str(), "USAGE");
        assert_eq!(ExitClass::Interrupted.as_str(), "INTERRUPTED");
        let mut spellings: Vec<&str> = ExitClass::ALL.iter().map(|c| c.as_str()).collect();
        spellings.sort_unstable();
        spellings.dedup();
        assert_eq!(spellings.len(), 10);
    }

    #[test]
    fn serializes_all_fields_when_present() {
        let envelope = MachineErrorEnvelope::new("TRANSPORT", 7, "broker.unreachable")
            .with_request_id("req-123")
            .with_remediation("retry with backoff");
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            "{\"class\":\"TRANSPORT\",\"code\":7,\"reason\":\"broker.unreachable\",\
             \"requestId\":\"req-123\",\"remediation\":\"retry with backoff\"}"
        );
        let parsed: MachineErrorEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn omits_absent_optional_fields() {
        let json =
            serde_json::to_string(&MachineErrorEnvelope::new("USAGE", 2, "flag.invalid")).unwrap();
        assert_eq!(
            json,
            "{\"class\":\"USAGE\",\"code\":2,\"reason\":\"flag.invalid\"}"
        );
    }
}

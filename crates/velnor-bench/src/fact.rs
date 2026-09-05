//! Explicit presence for every environment fact.
//!
//! The old bash benchmark recorded whatever `platform` happened to return and
//! silently omitted the rest, so a result could not be told apart from a result
//! whose environment was never captured. `Fact` removes that failure mode: the
//! key is always present in the serialised record, and its value is either a
//! measured observation or an explicit, human-readable reason why the host
//! could not produce one. A record whose environment key is simply absent is
//! rejected by deserialisation, not flagged.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A single environment observation with mandatory provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact<T> {
    /// The host produced this value.
    Known(T),
    /// The host could not produce a value, for the stated reason.
    Unavailable { reason: String },
}

impl<T> Fact<T> {
    /// Record an explicit absence.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    /// True when the host produced a value.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Borrow the measured value, if any.
    #[must_use]
    pub const fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unavailable { .. } => None,
        }
    }

    /// Why the value is missing, if it is.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Known(_) => None,
            Self::Unavailable { reason } => Some(reason.as_str()),
        }
    }
}

impl<T> From<Result<T, String>> for Fact<T> {
    fn from(value: Result<T, String>) -> Self {
        match value {
            Ok(value) => Self::Known(value),
            Err(reason) => Self::Unavailable { reason },
        }
    }
}

impl<T: fmt::Display> fmt::Display for Fact<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(value) => write!(formatter, "{value}"),
            Self::Unavailable { reason } => write!(formatter, "<unavailable: {reason}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unavailable_round_trip() {
        let known: Fact<String> = Fact::Known("apfs".to_owned());
        let json = serde_json::to_string(&known).expect("serialise");
        assert_eq!(json, r#"{"known":"apfs"}"#);
        assert_eq!(serde_json::from_str::<Fact<String>>(&json).unwrap(), known);

        let missing: Fact<String> = Fact::unavailable("docker daemon not reachable");
        let json = serde_json::to_string(&missing).expect("serialise");
        assert_eq!(
            json,
            r#"{"unavailable":{"reason":"docker daemon not reachable"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Fact<String>>(&json).unwrap(),
            missing
        );
    }

    #[test]
    fn a_null_value_is_not_a_fact() {
        assert!(serde_json::from_str::<Fact<String>>("null").is_err());
        assert!(serde_json::from_str::<Fact<String>>(r#""apfs""#).is_err());
    }

    #[test]
    fn reason_and_known_are_exclusive() {
        let known: Fact<u64> = Fact::Known(4);
        assert!(known.is_known());
        assert_eq!(known.known(), Some(&4));
        assert_eq!(known.reason(), None);

        let missing: Fact<u64> = Fact::unavailable("no probe");
        assert!(!missing.is_known());
        assert_eq!(missing.known(), None);
        assert_eq!(missing.reason(), Some("no probe"));
    }
}

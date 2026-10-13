//! Provenance of a resource's authoritative data.

use serde::{Deserialize, Serialize};

/// Where a resource's data was observed.
///
/// Serialized exactly `LOCAL`, `GITHUB`, or `MERGED`; deserialization is
/// fail-closed so later sources can appear only through a schema bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Source {
    /// Observed from local daemon or runner state.
    Local,
    /// Observed from the GitHub API.
    Github,
    /// Local and GitHub observations merged under the documented precedence.
    Merged,
}

impl Source {
    /// Every source in canonical order.
    pub const ALL: [Source; 3] = [Source::Local, Source::Github, Source::Merged];

    /// Canonical serialized spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::Local => "LOCAL",
            Source::Github => "GITHUB",
            Source::Merged => "MERGED",
        }
    }

    /// Parse the canonical spelling, failing closed.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Source::ALL.iter().copied().find(|s| s.as_str() == raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_are_screaming_and_stable() {
        assert_eq!(Source::Local.as_str(), "LOCAL");
        assert_eq!(Source::Github.as_str(), "GITHUB");
        assert_eq!(Source::Merged.as_str(), "MERGED");
        assert_eq!(
            serde_json::to_string(&Source::Github).unwrap(),
            "\"GITHUB\""
        );
    }

    #[test]
    fn parse_is_exact_and_fail_closed() {
        assert_eq!(Source::parse("MERGED"), Some(Source::Merged));
        assert_eq!(Source::parse("merged"), None);
        assert_eq!(Source::parse("ORBIT"), None);
        assert!(serde_json::from_str::<Source>("\"HYPERION\"").is_err());
    }
}

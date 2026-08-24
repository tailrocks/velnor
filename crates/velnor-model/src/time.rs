//! RFC 3339 timestamps and machine durations.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// An instant serialized strictly as RFC 3339 (UTC offset rendered as `Z`
/// only when the instant is UTC; parsing accepts any legal offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub(crate) OffsetDateTime);

impl Timestamp {
    /// Unix epoch, useful as a deterministic test anchor.
    pub const UNIX_EPOCH: Timestamp = Timestamp(OffsetDateTime::UNIX_EPOCH);

    /// Current wall-clock instant.
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// The wrapped instant.
    #[must_use]
    pub const fn as_offset_datetime(&self) -> OffsetDateTime {
        self.0
    }

    /// This instant minus `age`.
    ///
    /// Retention cutoffs use this instead of SQL string arithmetic so
    /// comparisons happen on parsed instants, never lexicographically.
    #[must_use]
    pub fn minus(&self, age: Duration) -> Self {
        let shifted =
            self.0 - time::Duration::seconds(i64::try_from(age.as_secs()).unwrap_or(i64::MAX));
        Self(shifted)
    }

    /// Parse an RFC 3339 string; anything else is rejected.
    pub fn parse(raw: &str) -> Result<Self, InvalidTimestamp> {
        OffsetDateTime::parse(raw, &Rfc3339)
            .map(Self)
            .map_err(|source| InvalidTimestamp {
                raw: raw.to_owned(),
                source: source.to_string(),
            })
    }

    /// Render as an RFC 3339 string.
    ///
    /// # Errors
    /// Fails only if the instant falls outside the RFC 3339 representable
    /// range, which ordinary daemon timestamps never do.
    pub fn to_rfc3339(&self) -> Result<String, InvalidTimestamp> {
        self.0.format(&Rfc3339).map_err(|source| InvalidTimestamp {
            raw: String::new(),
            source: source.to_string(),
        })
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_rfc3339() {
            Ok(rendered) => f.write_str(&rendered),
            Err(_) => write!(f, "<unrepresentable instant {:?}>", self.0),
        }
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.to_rfc3339() {
            Ok(rendered) => serializer.serialize_str(&rendered),
            Err(err) => Err(serde::ser::Error::custom(err.to_string())),
        }
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// A timestamp string was not valid RFC 3339.
#[derive(Debug)]
pub struct InvalidTimestamp {
    raw: String,
    source: String,
}

impl fmt::Display for InvalidTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw.is_empty() {
            write!(
                f,
                "instant cannot be formatted as RFC 3339: {}",
                self.source
            )
        } else {
            write!(
                f,
                "invalid RFC 3339 timestamp {:?}: {}",
                self.raw, self.source
            )
        }
    }
}

impl std::error::Error for InvalidTimestamp {}

/// A machine duration in whole milliseconds.
///
/// Durations are unsigned and named `*_ms`. An unavailable duration is
/// `null`, never zero. Conversions that would overflow `u64` milliseconds
/// produce [`DurationOverflowError`] instead of silently wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DurationMs(pub u64);

impl DurationMs {
    /// The value in milliseconds.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl TryFrom<Duration> for DurationMs {
    type Error = DurationOverflowError;

    fn try_from(duration: Duration) -> Result<Self, Self::Error> {
        let millis = duration.as_millis();
        if millis > u64::MAX as u128 {
            return Err(DurationOverflowError {
                requested_ms: millis,
            });
        }
        Ok(Self(millis as u64))
    }
}

impl fmt::Display for DurationMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// A duration exceeded the unsigned millisecond range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationOverflowError {
    requested_ms: u128,
}

impl fmt::Display for DurationOverflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "duration {}ms exceeds the unsigned 64-bit millisecond range",
            self.requested_ms
        )
    }
}

impl std::error::Error for DurationOverflowError {}

/// Deterministic fixed timestamp used by golden fixtures.
#[cfg(test)]
pub(crate) fn golden_timestamp() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").expect("golden timestamp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_round_trips_as_rfc3339() {
        let rendered = Timestamp::UNIX_EPOCH.to_rfc3339().unwrap();
        assert_eq!(rendered, "1970-01-01T00:00:00Z");
        assert_eq!(Timestamp::parse(&rendered).unwrap(), Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn rejects_non_rfc3339_input() {
        for bad in [
            "",
            "2026-08-24",
            "2026-08-24 12:30:45",
            "not-a-time",
            "2026-13-45T99:00:00Z",
        ] {
            assert!(Timestamp::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn accepts_offset_form_but_stays_rfc3339() {
        let parsed = Timestamp::parse("2026-08-24T14:30:45+02:00").unwrap();
        assert_eq!(parsed.to_rfc3339().unwrap(), "2026-08-24T14:30:45+02:00");
    }

    #[test]
    fn duration_conversion_is_exact_and_typed_on_overflow() {
        let small = DurationMs::try_from(Duration::from_millis(1500)).unwrap();
        assert_eq!(small.as_u64(), 1_500);

        let huge = Duration::from_secs(u64::MAX);
        let err = DurationMs::try_from(huge).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[test]
    fn timestamps_stay_within_the_four_digit_rfc3339_range() {
        // The build pins time's default year cap (9999), so every
        // constructible instant serializes as legal RFC 3339 and anything
        // beyond it fails closed at the parser instead of leaking to wire.
        let edge = Timestamp::parse("9999-12-31T23:59:59Z").unwrap();
        assert_eq!(edge.to_rfc3339().unwrap(), "9999-12-31T23:59:59Z");
        for beyond in ["10000-01-01T00:00:00Z", "+10000-01-01T00:00:00Z"] {
            assert!(Timestamp::parse(beyond).is_err(), "accepted {beyond:?}");
        }
    }
}

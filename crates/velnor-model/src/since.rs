//! `--since` filter parsing shared by the CLI and services.
//!
//! Accepts either an RFC 3339 instant or a relative duration measured back
//! from now (`45s`, `10m`, `2h`, `1h30m`). Parsing is strict: anything else
//! is a typed usage-level error, never an approximation.

use std::fmt;
use std::time::Duration;

use crate::time::{DurationMs, Timestamp};

/// A parsed `--since` bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Everything at or after this absolute instant.
    At(Timestamp),
    /// Everything within this duration before now.
    Within(DurationMs),
}

impl Since {
    /// Parse the canonical CLI grammar.
    pub fn parse(raw: &str) -> Result<Self, InvalidSince> {
        if raw.contains('T') || raw.contains('t') {
            return Timestamp::parse(raw)
                .map(Since::At)
                .map_err(|_| InvalidSince::new(raw));
        }
        Duration::from_str_millis(raw)
            .map(|ms| Since::Within(DurationMs(ms)))
            .map_err(|()| InvalidSince::new(raw))
    }

    /// Resolve against `now` into an absolute lower bound.
    ///
    /// Returns a typed error when the relative duration cannot be
    /// represented as milliseconds or pushes the bound outside the
    /// representable timestamp range; never saturates or panics.
    pub fn resolve(self, now: Timestamp) -> Result<Timestamp, SinceResolveError> {
        match self {
            Since::At(at) => Ok(at),
            Since::Within(ms) => {
                let millis = i64::try_from(ms.as_u64())
                    .map_err(|_| SinceResolveError { ms: ms.as_u64() })?;
                now.as_offset_datetime()
                    .checked_sub(time::Duration::milliseconds(millis))
                    .map(Timestamp)
                    .ok_or(SinceResolveError { ms: ms.as_u64() })
            }
        }
    }
}

impl fmt::Display for Since {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Since::At(at) => write!(f, "since {at}"),
            Since::Within(ms) => write!(f, "within {}ms", ms.as_u64()),
        }
    }
}

/// A `--since` value was neither RFC 3339 nor a valid relative duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSince {
    raw: String,
}

impl InvalidSince {
    fn new(raw: &str) -> Self {
        Self {
            raw: raw.to_owned(),
        }
    }
}

impl fmt::Display for InvalidSince {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid --since value {:?}: expected RFC 3339 timestamp or duration like 45s, 10m, 2h",
            self.raw
        )
    }
}

impl std::error::Error for InvalidSince {}

/// A relative `--since` duration overflows when resolved against `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinceResolveError {
    ms: u64,
}

impl fmt::Display for SinceResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "--since duration {}ms overflows when resolved against now",
            self.ms
        )
    }
}

impl std::error::Error for SinceResolveError {}

trait FromStrMillis {
    fn from_str_millis(raw: &str) -> Result<u64, ()>;
}

impl FromStrMillis for Duration {
    fn from_str_millis(raw: &str) -> Result<u64, ()> {
        let mut total_ms: u64 = 0;
        let bytes = raw.as_bytes();
        let mut start = 0usize;
        if bytes.is_empty() {
            return Err(());
        }
        while start < bytes.len() {
            let digits_end = bytes[start..]
                .iter()
                .position(|b| !b.is_ascii_digit())
                .map_or(bytes.len(), |offset| start + offset);
            if digits_end == start {
                return Err(());
            }
            let number: u64 = raw[start..digits_end].parse().map_err(|_| ())?;
            let unit_end = bytes[digits_end..]
                .iter()
                .position(|b| b.is_ascii_digit())
                .map_or(raw.len(), |offset| digits_end + offset);
            let unit = &raw[digits_end..unit_end];
            let unit_ms: u64 = match unit {
                "ms" => 1,
                "s" => 1_000,
                "m" => 60_000,
                "h" => 3_600_000,
                _ => return Err(()),
            };
            total_ms = total_ms
                .checked_add(number.checked_mul(unit_ms).ok_or(())?)
                .ok_or(())?;
            start = unit_end;
        }
        Ok(total_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_rfc3339() {
        let since = Since::parse("2026-08-24T12:30:45Z").unwrap();
        assert_eq!(
            since,
            Since::At(Timestamp::parse("2026-08-24T12:30:45Z").unwrap())
        );
    }

    #[test]
    fn parses_relative_durations_with_units_and_compounds() {
        assert_eq!(
            Since::parse("90s").unwrap(),
            Since::Within(DurationMs(90_000))
        );
        assert_eq!(
            Since::parse("1h30m").unwrap(),
            Since::Within(DurationMs(5_400_000))
        );
        assert_eq!(
            Since::parse("500ms").unwrap(),
            Since::Within(DurationMs(500))
        );
    }

    #[test]
    fn rejects_garbage_strictly() {
        for bad in ["", "10", "1x", "h", "1.5h", "-5m", "yesterday"] {
            assert!(Since::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn resolve_subtracts_within_bounds() {
        let now = Timestamp::parse("2026-08-24T12:30:45Z").unwrap();
        let resolved = Since::Within(DurationMs(90_000)).resolve(now).unwrap();
        assert_eq!(resolved.to_rfc3339().unwrap(), "2026-08-24T12:29:15Z");
    }

    #[test]
    fn resolve_rejects_u64_max_milliseconds_with_typed_error() {
        let now = Timestamp::parse("2026-08-24T12:30:45Z").unwrap();
        let since = Since::parse("18446744073709551615ms").unwrap();
        assert_eq!(since, Since::Within(DurationMs(u64::MAX)));
        let err = since.resolve(now).unwrap_err();
        assert_eq!(err, SinceResolveError { ms: u64::MAX });
        assert!(err.to_string().contains("18446744073709551615ms"));
    }

    #[test]
    fn resolve_rejects_duration_outside_representable_range() {
        let now = Timestamp::parse("2026-08-24T12:30:45Z").unwrap();
        let since = Since::parse("9223372036854775807ms").unwrap();
        assert!(since.resolve(now).is_err());
    }
}

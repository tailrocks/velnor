//! Honest summary statistics.
//!
//! The rule this module exists to enforce: a percentile is only emitted when
//! the sample actually contains an observation above it. With `n` samples the
//! largest quantile the sample can distinguish from its own maximum is
//! `1 - 1/n`, so `q` requires `n >= 1 / (1 - q)`:
//!
//! | quantile | minimum n |
//! | --- | --- |
//! | p50 | 2 |
//! | p95 | 20 |
//! | p99 | 100 |
//!
//! The replaced bash benchmark ran `n = 5` and printed a "p95" that was, by
//! construction, its maximum. Here the p95 field is `None` with a recorded
//! reason until the sample supports it.
//!
//! A summary of fewer than [`MIN_SAMPLES`] observations is refused outright:
//! a single run is never a measurement.

use serde::{Deserialize, Serialize};

/// No summary is produced below this many observations. One run is an anecdote
/// and two cannot show dispersion.
pub const MIN_SAMPLES: usize = 3;

/// Sample count required before a quantile is distinguishable from the maximum.
#[must_use]
pub fn min_samples_for(quantile: f64) -> usize {
    if !(0.0..1.0).contains(&quantile) {
        return usize::MAX;
    }
    let needed = 1.0 / (1.0 - quantile);
    // ceil, guarding the float -> usize edge.
    let ceiled = needed.ceil();
    if ceiled.is_finite() && ceiled >= 1.0 {
        ceiled as usize
    } else {
        usize::MAX
    }
}

/// A quantile that the sample was large enough to support, or the reason it was
/// not emitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantile {
    /// Estimated value in the sample's unit.
    Value(u64),
    /// Sample too small; carries the count seen and the count required.
    Unsupported { samples: usize, required: usize },
}

impl Quantile {
    /// Borrow the value when the sample supported it.
    #[must_use]
    pub const fn value(&self) -> Option<u64> {
        match self {
            Self::Value(value) => Some(*value),
            Self::Unsupported { .. } => None,
        }
    }
}

/// Refusal to summarise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooFewSamples {
    pub samples: usize,
    pub required: usize,
}

impl std::fmt::Display for TooFewSamples {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "refusing to summarise {} observation(s); {} required",
            self.samples, self.required
        )
    }
}

impl std::error::Error for TooFewSamples {}

/// Distribution summary over integer observations (milliseconds, bytes, counts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub samples: usize,
    pub min: u64,
    pub max: u64,
    pub mean: f64,
    /// Sample variance (Bessel-corrected, `n - 1` denominator).
    pub variance: f64,
    pub p50: Quantile,
    pub p95: Quantile,
    pub p99: Quantile,
}

impl Summary {
    /// Summarise a sample, refusing anything shorter than [`MIN_SAMPLES`].
    ///
    /// # Errors
    /// Returns [`TooFewSamples`] when the sample cannot support a summary.
    pub fn new(observations: &[u64]) -> Result<Self, TooFewSamples> {
        let samples = observations.len();
        if samples < MIN_SAMPLES {
            return Err(TooFewSamples {
                samples,
                required: MIN_SAMPLES,
            });
        }
        let mut sorted = observations.to_vec();
        sorted.sort_unstable();

        let count = samples as f64;
        let mean = sorted.iter().map(|value| *value as f64).sum::<f64>() / count;
        let variance = sorted
            .iter()
            .map(|value| {
                let delta = *value as f64 - mean;
                delta * delta
            })
            .sum::<f64>()
            / (count - 1.0);

        Ok(Self {
            samples,
            min: sorted[0],
            max: sorted[samples - 1],
            mean,
            variance,
            p50: quantile(&sorted, 0.50),
            p95: quantile(&sorted, 0.95),
            p99: quantile(&sorted, 0.99),
        })
    }
}

/// Nearest-rank quantile, emitted only when the sample supports it.
fn quantile(sorted: &[u64], quantile: f64) -> Quantile {
    let samples = sorted.len();
    let required = min_samples_for(quantile);
    if samples < required {
        return Quantile::Unsupported { samples, required };
    }
    // Nearest-rank: smallest value at or above the quantile position.
    let rank = (quantile * samples as f64).ceil().max(1.0) as usize;
    Quantile::Value(sorted[rank.min(samples) - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_thresholds_match_the_documented_table() {
        assert_eq!(min_samples_for(0.50), 2);
        assert_eq!(min_samples_for(0.95), 20);
        assert_eq!(min_samples_for(0.99), 100);
    }

    #[test]
    fn a_single_run_is_refused() {
        let error = Summary::new(&[7]).expect_err("one observation must be refused");
        assert_eq!(error.samples, 1);
        assert_eq!(error.required, MIN_SAMPLES);
        assert!(Summary::new(&[]).is_err());
        assert!(Summary::new(&[1, 2]).is_err());
    }

    #[test]
    fn p95_is_withheld_at_the_old_scripts_sample_size() {
        // The replaced bash benchmark ran n = 5 and printed max as "p95".
        let summary = Summary::new(&[10, 20, 30, 40, 500]).expect("summary");
        assert_eq!(summary.p50, Quantile::Value(30));
        assert_eq!(
            summary.p95,
            Quantile::Unsupported {
                samples: 5,
                required: 20
            }
        );
        assert_eq!(
            summary.p99,
            Quantile::Unsupported {
                samples: 5,
                required: 100
            }
        );
        assert_eq!(summary.max, 500);
        assert_ne!(summary.p95.value(), Some(summary.max));
    }

    #[test]
    fn p95_is_emitted_once_the_sample_supports_it_and_is_below_max() {
        let observations: Vec<u64> = (1..=20).collect();
        let summary = Summary::new(&observations).expect("summary");
        assert_eq!(summary.p95, Quantile::Value(19));
        assert_eq!(summary.max, 20);
        assert_eq!(summary.p50, Quantile::Value(10));
        assert!(matches!(summary.p99, Quantile::Unsupported { .. }));
    }

    #[test]
    fn p99_is_emitted_at_one_hundred_samples() {
        let observations: Vec<u64> = (1..=100).collect();
        let summary = Summary::new(&observations).expect("summary");
        assert_eq!(summary.p99, Quantile::Value(99));
        assert_eq!(summary.max, 100);
    }

    #[test]
    fn moments_are_computed_over_the_whole_sample() {
        let summary = Summary::new(&[2, 4, 4, 4, 5, 5, 7, 9]).expect("summary");
        assert_eq!(summary.min, 2);
        assert_eq!(summary.max, 9);
        assert!((summary.mean - 5.0).abs() < 1e-12);
        // Bessel-corrected variance of the classic sample is 32/7.
        assert!((summary.variance - 32.0 / 7.0).abs() < 1e-12);
    }

    #[test]
    fn summary_round_trips_through_json() {
        let summary = Summary::new(&[1, 2, 3, 4]).expect("summary");
        let json = serde_json::to_string(&summary).expect("serialise");
        let parsed: Summary = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, summary);
    }
}

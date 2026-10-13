//! Honest A/B comparison of two benchmark records.
//!
//! This is the arithmetic behind "Velnor vs actions/runner" and behind the
//! trust-partition cost: two records for the same scenario, one ratio per
//! metric. Every ratio is gated on the same sample rule as the underlying
//! percentiles ([`crate::stats`]): a p95 ratio exists only when both sides
//! have n>=20, otherwise the comparison says so instead of dividing maxima.
//!
//! The two records must share their scenario and driver — comparing a
//! `docker-direct` container timing against a `velnor-job` acquisition timing
//! is meaningless, and the comparison refuses it. What differs between the
//! runs (runner identity, trust class, image) travels in the records'
//! [`context`](crate::record::BenchRecord::context) map and is surfaced in
//! the comparison notes, so the reader sees what was actually compared.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use velnor_model::telemetry::TelemetryLane;

use crate::{
    record::BenchRecord,
    scenario::Driver,
    stage::Stage,
    stats::{Quantile, Summary},
};

/// Stable discriminator for the comparison wire contract.
pub const COMPARISON_SCHEMA: &str = "velnor.bench.comparison.v1";

/// One quantile on both sides plus their ratio.
///
/// The ratio is `candidate / baseline`: below 1.0 the candidate is faster. It
/// is `None` unless both sides emitted the quantile and the baseline is
/// non-zero — never a division of maxima or of unsupported placeholders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantileRatio {
    pub baseline: Quantile,
    pub candidate: Quantile,
    pub ratio: Option<f64>,
}

fn ratio_of(baseline: &Quantile, candidate: &Quantile) -> QuantileRatio {
    let ratio = match (baseline.value(), candidate.value()) {
        (Some(base), Some(cand)) if base != 0 => Some(cand as f64 / base as f64),
        _ => None,
    };
    QuantileRatio {
        baseline: baseline.clone(),
        candidate: candidate.clone(),
        ratio,
    }
}

fn p50_ratio(baseline: &Summary, candidate: &Summary) -> QuantileRatio {
    ratio_of(&baseline.p50, &candidate.p50)
}

fn p95_ratio(baseline: &Summary, candidate: &Summary) -> QuantileRatio {
    ratio_of(&baseline.p95, &candidate.p95)
}

/// Comparison of two records for one scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub schema: String,
    pub baseline_run_id: String,
    pub candidate_run_id: String,
    pub scenario: String,
    pub driver: Driver,
    pub baseline_samples: usize,
    pub candidate_samples: usize,
    pub total_ms_p50: QuantileRatio,
    pub total_ms_p95: QuantileRatio,
    pub stages_ms_p50: BTreeMap<Stage, QuantileRatio>,
    pub stages_ms_p95: BTreeMap<Stage, QuantileRatio>,
    pub lane_ms_p50: BTreeMap<TelemetryLane, QuantileRatio>,
    pub lane_ms_p95: BTreeMap<TelemetryLane, QuantileRatio>,
    /// Context differences between the runs, plus interpretation caveats.
    pub notes: Vec<String>,
}

impl Comparison {
    /// Serialise as one NDJSON line.
    ///
    /// # Errors
    /// Serialisation failure.
    pub fn to_ndjson(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

/// Compare two records.
///
/// # Errors
/// The records are for different scenarios or were produced by different
/// drivers.
pub fn compare(baseline: &BenchRecord, candidate: &BenchRecord) -> Result<Comparison> {
    if baseline.scenario != candidate.scenario {
        anyhow::bail!(
            "cannot compare {} against {}: scenarios differ",
            baseline.scenario,
            candidate.scenario
        );
    }
    if baseline.driver != candidate.driver {
        anyhow::bail!(
            "cannot compare {} against {}: drivers differ ({} vs {})",
            baseline.run_id,
            candidate.run_id,
            baseline.driver.as_str(),
            candidate.driver.as_str()
        );
    }
    let base = &baseline.summaries;
    let cand = &candidate.summaries;
    let mut stages_ms_p50 = BTreeMap::new();
    let mut stages_ms_p95 = BTreeMap::new();
    for stage in Stage::ALL {
        if let (Some(base_stage), Some(cand_stage)) =
            (base.stages_ms.get(&stage), cand.stages_ms.get(&stage))
        {
            stages_ms_p50.insert(stage, p50_ratio(base_stage, cand_stage));
            stages_ms_p95.insert(stage, p95_ratio(base_stage, cand_stage));
        }
    }
    let mut lane_ms_p50 = BTreeMap::new();
    let mut lane_ms_p95 = BTreeMap::new();
    for lane in [TelemetryLane::Velnor, TelemetryLane::Github] {
        if let (Some(base_lane), Some(cand_lane)) =
            (base.lane_ms.get(&lane), cand.lane_ms.get(&lane))
        {
            lane_ms_p50.insert(lane, p50_ratio(base_lane, cand_lane));
            lane_ms_p95.insert(lane, p95_ratio(base_lane, cand_lane));
        }
    }
    Ok(Comparison {
        schema: COMPARISON_SCHEMA.to_owned(),
        baseline_run_id: baseline.run_id.clone(),
        candidate_run_id: candidate.run_id.clone(),
        scenario: baseline.scenario.clone(),
        driver: baseline.driver,
        baseline_samples: baseline.observations.len(),
        candidate_samples: candidate.observations.len(),
        total_ms_p50: p50_ratio(&base.total_ms, &cand.total_ms),
        total_ms_p95: p95_ratio(&base.total_ms, &cand.total_ms),
        stages_ms_p50,
        stages_ms_p95,
        lane_ms_p50,
        lane_ms_p95,
        notes: context_notes(baseline, candidate),
    })
}

/// Surface what differs between the runs so the reader knows what the ratios
/// actually compare. Identical context is a caveat, not an error: re-running
/// the same configuration measures noise, which is itself useful.
fn context_notes(baseline: &BenchRecord, candidate: &BenchRecord) -> Vec<String> {
    let mut notes = Vec::new();
    let mut keys: Vec<&String> = baseline
        .context
        .keys()
        .chain(candidate.context.keys())
        .collect();
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        notes.push(
            "neither record carries comparison context; the ratios compare two runs of \
             the same configuration (noise), not two configurations"
                .to_owned(),
        );
        return notes;
    }
    for key in keys {
        match (baseline.context.get(key), candidate.context.get(key)) {
            (Some(base), Some(cand)) if base == cand => {
                notes.push(format!("context {key} is identical on both sides ({base})"));
            }
            (Some(base), Some(cand)) => {
                notes.push(format!(
                    "context {key} differs: baseline {base:?}, candidate {cand:?}"
                ));
            }
            (Some(base), None) => {
                notes.push(format!("context {key} is baseline-only ({base})"));
            }
            (None, Some(cand)) => {
                notes.push(format!("context {key} is candidate-only ({cand})"));
            }
            (None, None) => {}
        }
    }
    notes
}

/// Recompute check: a comparison is valid when it equals a fresh comparison
/// of the records it names. Kept beside [`compare`] so fixture consumers can
/// verify a comparison line without re-running anything.
pub fn verify_against(
    comparison: &Comparison,
    baseline: &BenchRecord,
    candidate: &BenchRecord,
) -> bool {
    match compare(baseline, candidate) {
        Ok(fresh) => {
            fresh.baseline_run_id == comparison.baseline_run_id
                && fresh.candidate_run_id == comparison.candidate_run_id
                && fresh == *comparison
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        env::{EnvironmentIdentity, ProbeInputs},
        gittrace::GitEvidence,
        record::{Observation, Resources, Summaries},
        scenario::{Family, Requirement, Runnability},
        sys::Runner,
    };

    fn environment() -> EnvironmentIdentity {
        let mut runner = Runner::new();
        EnvironmentIdentity::probe(
            &ProbeInputs {
                velnor_repo: std::env::current_dir().expect("cwd"),
                fixture_repo: None,
                work_root: std::env::temp_dir(),
                job_image: None,
                runner_config_dir: None,
            },
            &mut runner,
        )
    }

    fn observation(total: u64) -> Observation {
        Observation {
            total_ms: total,
            stages_ms: BTreeMap::from([(Stage::ContainerStart, total / 2)]),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources::default(),
            git: GitEvidence::NotMeasured,
            docker_census: crate::census::DockerCensus::default(),
            fault: None,
        }
    }

    fn record(run_id: &str, totals: &[u64], context: &[(&str, &str)]) -> BenchRecord {
        let observations: Vec<Observation> =
            totals.iter().map(|total| observation(*total)).collect();
        BenchRecord {
            schema: crate::record::RESULT_SCHEMA.to_owned(),
            run_id: run_id.to_owned(),
            recorded_at_unix_ms: 1,
            scenario: "docker/existing-image".to_owned(),
            family: Family::Docker,
            driver: Driver::DockerDirect,
            runnability: Runnability::Degraded {
                driver: Driver::DockerDirect,
                missing_for_preferred: vec![Requirement::VelnorJobDriver],
            },
            environment: environment(),
            observations: observations.clone(),
            summaries: Summaries::new(&observations).expect("summaries"),
            context: context
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn ratios_divide_values_and_gate_on_sample_size() {
        // Baseline n=20 supports p95; candidate n=5 does not.
        let baseline: Vec<u64> = (1..=20).map(|index| index * 10).collect();
        let candidate: Vec<u64> = vec![10, 20, 30, 40, 50];
        let comparison = compare(
            &record("base", &baseline, &[("runner", "velnor")]),
            &record("cand", &candidate, &[("runner", "actions-runner")]),
        )
        .expect("comparison");
        assert_eq!(comparison.scenario, "docker/existing-image");
        assert_eq!(comparison.baseline_samples, 20);
        assert_eq!(comparison.candidate_samples, 5);
        // p50 exists on both sides: 100 vs 30.
        assert_eq!(comparison.total_ms_p50.ratio, Some(30.0 / 100.0));
        // p95 is unsupported on the candidate side: no ratio, both sides shown.
        assert_eq!(comparison.total_ms_p95.ratio, None);
        assert!(comparison.total_ms_p95.baseline.value().is_some());
        assert!(comparison.total_ms_p95.candidate.value().is_none());
        assert!(
            comparison
                .notes
                .iter()
                .any(|note| note.contains("runner") && note.contains("differs")),
            "notes must surface the runner difference: {:?}",
            comparison.notes
        );
    }

    #[test]
    fn identical_context_is_a_caveat_not_an_error() {
        let totals: Vec<u64> = (1..=20).map(|index| index * 10).collect();
        let comparison = compare(&record("base", &totals, &[]), &record("cand", &totals, &[]))
            .expect("comparison");
        assert_eq!(comparison.total_ms_p95.ratio, Some(1.0));
        assert!(
            comparison.notes.iter().any(|note| note.contains("noise")),
            "{:?}",
            comparison.notes
        );
    }

    #[test]
    fn different_scenarios_or_drivers_are_refused() {
        let totals = vec![10, 20, 30];
        let base = record("base", &totals, &[]);
        let mut other_scenario = record("cand", &totals, &[]);
        other_scenario.scenario = "docker/image-pull".to_owned();
        assert!(compare(&base, &other_scenario).is_err());
        let mut other_driver = record("cand", &totals, &[]);
        other_driver.driver = Driver::CargoDirect;
        assert!(compare(&base, &other_driver).is_err());
    }

    #[test]
    fn a_zero_baseline_yields_no_ratio() {
        let totals = vec![0, 0, 0, 0];
        let comparison = compare(
            &record("base", &totals, &[]),
            &record("cand", &[1, 2, 3, 4], &[]),
        )
        .expect("comparison");
        assert_eq!(comparison.total_ms_p50.ratio, None);
    }

    #[test]
    fn verify_against_accepts_a_fresh_comparison_and_rejects_edits() {
        let totals: Vec<u64> = (1..=4).map(|index| index * 10).collect();
        let base = record("base", &totals, &[]);
        let cand = record("cand", &totals, &[]);
        let comparison = compare(&base, &cand).expect("comparison");
        assert!(verify_against(&comparison, &base, &cand));
        let mut edited = comparison.clone();
        edited.total_ms_p50.ratio = Some(0.5);
        assert!(!verify_against(&edited, &base, &cand));
    }

    #[test]
    fn the_comparison_round_trips_through_json() {
        let totals: Vec<u64> = (1..=4).map(|index| index * 10).collect();
        let comparison = compare(&record("base", &totals, &[]), &record("cand", &totals, &[]))
            .expect("comparison");
        let line = comparison.to_ndjson().expect("serialise");
        assert!(!line.contains('\n'));
        let parsed: Comparison = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(parsed, comparison);
    }
}

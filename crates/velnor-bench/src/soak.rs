//! Soak: repeated iterations with resource-growth monitors.
//!
//! BC-27 recorded that no soak suite or resource-growth monitor existed at
//! all. A soak run executes a scenario's workload round after round and
//! samples, after every round, the two leak signals the harness can observe
//! honestly:
//!
//! * owned Docker objects still present (`com.velnor.bench.owner`-labelled
//!   containers and networks), which must be zero after every round's own
//!   teardown — any residue is a leak, full stop;
//! * the scratch root's on-disk size, reported as first/last bytes plus a
//!   per-round least-squares slope so growth is visible even when it stays
//!   below any threshold.
//!
//! The verdict passes exactly when no round leaves residue. Disk growth is
//! evidence, not a verdict: no retention policy exists yet to say how much
//! growth is a leak, so the report carries the numbers and lets the reader
//! decide. Timing drift (second-half versus first-half median) is reported
//! the same way.
//!
//! Soak sampling runs its own `docker` invocations between rounds. Drivers
//! reset the shared [`Runner`](crate::sys::Runner) at the start of every
//! iteration, so sampling never pollutes a round's own process census.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    drivers::{Context as DriverContext, Workload},
    record::Observation,
    scenario::Driver,
    sys::tree_bytes,
};

/// Stable discriminator for the soak wire contract.
pub const SOAK_SCHEMA: &str = "velnor.bench.soak.v1";

/// Minimum rounds: fewer cannot show a trend.
pub const MIN_ROUNDS: usize = 3;

/// Label key every bench-owned container and network carries.
const OWNER_LABEL_KEY: &str = "com.velnor.bench.owner";

/// One soak round: the iteration's own observation plus leak samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoakRound {
    pub round: usize,
    pub total_ms: u64,
    /// Bench-owned containers still present after the round.
    pub owned_containers: u64,
    /// Bench-owned networks still present after the round.
    pub owned_networks: u64,
    /// Scratch-root bytes after the round.
    pub work_root_bytes: u64,
}

/// The verdict and the trends behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoakVerdict {
    /// True when no round left any owned object behind.
    pub passed: bool,
    pub rounds: usize,
    /// Largest single-round residue (containers plus networks).
    pub max_residue: u64,
    pub work_root_bytes_first: u64,
    pub work_root_bytes_last: u64,
    /// Least-squares slope of scratch bytes per round; negative is shrinkage.
    pub work_root_bytes_per_round: f64,
    /// Second-half median `total_ms` over first-half median. Above 1.0 is
    /// slowdown; `None` when a half has no rounds (impossible at >= 3).
    pub slowdown_ratio: Option<f64>,
}

/// One soak result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoakReport {
    pub schema: String,
    pub run_id: String,
    pub recorded_at_unix_ms: u64,
    pub scenario: String,
    pub driver: Driver,
    pub rounds: Vec<SoakRound>,
    pub verdict: SoakVerdict,
    pub notes: Vec<String>,
}

impl SoakReport {
    /// Serialise as one NDJSON line.
    ///
    /// # Errors
    /// Serialisation failure.
    pub fn to_ndjson(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

/// Count bench-owned objects of one kind. A sampling failure is an error, not
/// a zero: an unverified round proves nothing.
fn count_owned(context: &mut DriverContext, object: &str, list_args: &[&str]) -> Result<u64> {
    let filter = format!("label={OWNER_LABEL_KEY}");
    let mut args: Vec<String> = list_args.iter().map(|arg| (*arg).to_owned()).collect();
    args.push("--filter".to_owned());
    args.push(filter);
    args.push("--format".to_owned());
    args.push("{{.ID}}".to_owned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let invocation = context
        .runner
        .run("docker", &arg_refs)
        .with_context(|| format!("soak sampling of owned {object}"))?
        .clone();
    if !invocation.ok() {
        anyhow::bail!(
            "soak sampling of owned {object} failed with exit code {}: {}",
            invocation.code,
            invocation.stderr.trim()
        );
    }
    Ok(invocation
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .count() as u64)
}

fn sample_round(
    context: &mut DriverContext,
    round: usize,
    observation: &Observation,
    sample_docker: bool,
) -> Result<SoakRound> {
    // Host-only workloads spawn no containers; their soak still monitors disk
    // growth and timing drift, but Docker residue is meaningless.
    let (owned_containers, owned_networks) = if sample_docker {
        (
            count_owned(context, "containers", &["container", "ls", "--all"])?,
            count_owned(context, "networks", &["network", "ls"])?,
        )
    } else {
        (0, 0)
    };
    Ok(SoakRound {
        round,
        total_ms: observation.total_ms,
        owned_containers,
        owned_networks,
        work_root_bytes: tree_bytes(&context.work_root),
    })
}

/// Least-squares slope of `values` over round index. Empty input is 0.0.
fn slope(values: &[u64]) -> f64 {
    let count = values.len() as f64;
    if values.len() < 2 {
        return 0.0;
    }
    let mean_x = (count - 1.0) / 2.0;
    let mean_y: f64 = values.iter().map(|value| *value as f64).sum::<f64>() / count;
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for (index, value) in values.iter().enumerate() {
        let x = index as f64 - mean_x;
        numerator += x * (*value as f64 - mean_y);
        denominator += x * x;
    }
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn median(mut values: Vec<u64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle] as f64)
    } else {
        Some((values[middle - 1] as f64 + values[middle] as f64) / 2.0)
    }
}

fn verdict(rounds: &[SoakRound]) -> SoakVerdict {
    let max_residue = rounds
        .iter()
        .map(|round| round.owned_containers + round.owned_networks)
        .max()
        .unwrap_or(0);
    let bytes: Vec<u64> = rounds.iter().map(|round| round.work_root_bytes).collect();
    let totals: Vec<u64> = rounds.iter().map(|round| round.total_ms).collect();
    let half = totals.len() / 2;
    let slowdown_ratio = match (
        median(totals[..half].to_vec()),
        median(totals[half..].to_vec()),
    ) {
        (Some(first), Some(second)) if first > 0.0 => Some(second / first),
        _ => None,
    };
    SoakVerdict {
        passed: max_residue == 0,
        rounds: rounds.len(),
        max_residue,
        work_root_bytes_first: bytes.first().copied().unwrap_or(0),
        work_root_bytes_last: bytes.last().copied().unwrap_or(0),
        work_root_bytes_per_round: slope(&bytes),
        slowdown_ratio,
    }
}

/// Run `rounds` soak rounds of one workload.
///
/// # Errors
/// Fewer than [`MIN_ROUNDS`] rounds, preparation or teardown failure, any
/// round's workload failure, or a sampling failure.
pub fn run(
    workload: &mut dyn Workload,
    context: &mut DriverContext,
    rounds: usize,
    run_id: &str,
    recorded_at_unix_ms: u64,
    scenario: &str,
    driver: Driver,
) -> Result<SoakReport> {
    if rounds < MIN_ROUNDS {
        anyhow::bail!("soak needs at least {MIN_ROUNDS} rounds; got {rounds}");
    }
    let sample_docker = driver != Driver::CargoDirect;
    let outcome = (|| {
        workload.prepare(context)?;
        let mut sampled = Vec::with_capacity(rounds);
        for round in 0..rounds {
            let observation = workload.iterate(context)?;
            sampled.push(sample_round(context, round, &observation, sample_docker)?);
        }
        Ok::<Vec<SoakRound>, anyhow::Error>(sampled)
    })();
    let teardown = workload.teardown(context);
    let sampled = match (outcome, teardown) {
        (Ok(rounds), Ok(())) => rounds,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(teardown_error)) => {
            return Err(error.context(format!("soak teardown also failed: {teardown_error:#}")));
        }
    };
    let mut notes = workload.notes();
    let report_verdict = verdict(&sampled);
    if report_verdict.passed {
        notes.push(format!(
            "soak passed: {} rounds, no owned-object residue",
            sampled.len()
        ));
    } else {
        notes.push(format!(
            "soak FAILED: max residue {} owned object(s) in one round",
            report_verdict.max_residue
        ));
    }
    Ok(SoakReport {
        schema: SOAK_SCHEMA.to_owned(),
        run_id: run_id.to_owned(),
        recorded_at_unix_ms,
        scenario: scenario.to_owned(),
        driver,
        rounds: sampled,
        verdict: report_verdict,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gittrace::GitEvidence, record::Resources, stage::Stage, sys::Runner};
    use std::collections::BTreeMap;

    fn observation(total_ms: u64) -> Observation {
        Observation {
            total_ms,
            stages_ms: BTreeMap::from([(Stage::ContainerStart, total_ms)]),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources::default(),
            git: GitEvidence::NotMeasured,
            docker_census: crate::census::DockerCensus::default(),
            fault: None,
        }
    }

    struct ScriptedWorkload {
        totals: Vec<u64>,
        teardown_calls: usize,
    }

    impl Workload for ScriptedWorkload {
        fn iterate(&mut self, _context: &mut DriverContext) -> Result<Observation> {
            let total = self.totals.remove(0);
            Ok(observation(total))
        }

        fn teardown(&mut self, _context: &mut DriverContext) -> Result<()> {
            self.teardown_calls += 1;
            Ok(())
        }
    }

    #[test]
    fn fewer_than_three_rounds_is_refused() {
        let mut workload = ScriptedWorkload {
            totals: vec![1, 2],
            teardown_calls: 0,
        };
        let mut context = DriverContext {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 0,
            concurrency: 1,
            runner: Runner::new(),
        };
        assert!(run(
            &mut workload,
            &mut context,
            2,
            "id",
            0,
            "s",
            Driver::DockerDirect
        )
        .is_err());
        assert_eq!(workload.teardown_calls, 0);
    }

    #[test]
    fn the_slope_and_slowdown_come_from_the_rounds() {
        let rounds: Vec<SoakRound> = [100, 110, 120, 130, 140, 150]
            .into_iter()
            .enumerate()
            .map(|(round, total_ms)| SoakRound {
                round,
                total_ms,
                owned_containers: 0,
                owned_networks: 0,
                work_root_bytes: 1000 + round as u64 * 100,
            })
            .collect();
        let result = verdict(&rounds);
        assert!(result.passed);
        assert_eq!(result.max_residue, 0);
        assert!((result.work_root_bytes_per_round - 100.0).abs() < 1e-9);
        // First-half median 110, second-half median 140.
        let ratio = result.slowdown_ratio.expect("ratio");
        assert!((ratio - 140.0 / 110.0).abs() < 1e-12);
    }

    #[test]
    fn any_residue_fails_the_verdict() {
        let rounds: Vec<SoakRound> = [10, 10, 10]
            .into_iter()
            .enumerate()
            .map(|(round, total_ms)| SoakRound {
                round,
                total_ms,
                owned_containers: u64::from(round == 1),
                owned_networks: 0,
                work_root_bytes: 500,
            })
            .collect();
        let result = verdict(&rounds);
        assert!(!result.passed);
        assert_eq!(result.max_residue, 1);
        assert_eq!(result.work_root_bytes_per_round, 0.0);
    }

    #[test]
    fn median_handles_even_and_odd_halves() {
        assert_eq!(median(vec![3, 1, 2]), Some(2.0));
        assert_eq!(median(vec![4, 1, 2, 3]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn the_report_round_trips_through_json() {
        let report = SoakReport {
            schema: SOAK_SCHEMA.to_owned(),
            run_id: "soak-1".to_owned(),
            recorded_at_unix_ms: 7,
            scenario: "docker/existing-image".to_owned(),
            driver: Driver::DockerDirect,
            rounds: vec![SoakRound {
                round: 0,
                total_ms: 10,
                owned_containers: 0,
                owned_networks: 0,
                work_root_bytes: 100,
            }],
            verdict: SoakVerdict {
                passed: true,
                rounds: 1,
                max_residue: 0,
                work_root_bytes_first: 100,
                work_root_bytes_last: 100,
                work_root_bytes_per_round: 0.0,
                slowdown_ratio: None,
            },
            notes: vec!["note".to_owned()],
        };
        let line = report.to_ndjson().expect("serialise");
        assert!(!line.contains('\n'));
        let parsed: SoakReport = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(parsed, report);
    }
}

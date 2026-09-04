//! The result wire schema.
//!
//! One NDJSON record per scenario run. Two properties are enforced by the
//! schema rather than by convention:
//!
//! 1. Environment identity is mandatory. Every field of
//!    [`EnvironmentIdentity`] must be present, and a record whose environment
//!    block is missing or partial fails to deserialise.
//! 2. A record may not carry a stage its driver cannot observe, so a
//!    container-only measurement can never be read as a claim about broker or
//!    acquisition latency.
//! 3. Summaries are recomputed from the observations during validation, so a
//!    producer cannot attach unrelated aggregate numbers to a real sample.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use velnor_model::telemetry::TelemetryLane;

use crate::{
    env::EnvironmentIdentity,
    gittrace::GitEvidence,
    scenario::{Driver, Family, Requirement, Runnability},
    stage::{CheckoutPhase, Stage},
    stats::{Summary, TooFewSamples},
};

/// Stable discriminator for the result wire contract.
pub const RESULT_SCHEMA: &str = "velnor.bench.result.v2";

/// Everything measured about one iteration of a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Wall time for the whole iteration.
    pub total_ms: u64,
    /// Per-stage wall time. Keys are restricted to the driver's observable set.
    pub stages_ms: BTreeMap<Stage, u64>,
    /// Per-phase checkout breakdown, when the runner emitted the spans.
    pub checkout_phases_ms: BTreeMap<CheckoutPhase, u64>,
    pub resources: Resources,
    pub git: GitEvidence,
}

impl Observation {
    /// Sum of the stages attributed to one lane.
    #[must_use]
    pub fn lane_ms(&self, lane: TelemetryLane) -> u64 {
        self.stages_ms
            .iter()
            .filter(|(stage, _)| stage.lane() == lane)
            .map(|(_, value)| *value)
            .sum()
    }
}

/// Resource cost of one iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu_user_us: u64,
    pub cpu_system_us: u64,
    pub max_rss_bytes: u64,
    pub block_input_ops: u64,
    pub block_output_ops: u64,
    /// Change in on-disk footprint of the measured working root.
    pub disk_bytes_delta: i64,
    /// Host processes spawned by the harness for this iteration.
    pub process_count: u64,
    /// Of those, invocations of the Docker CLI.
    pub docker_invocations: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub bytes_copied: u64,
    pub bytes_downloaded: u64,
    pub bytes_reused: u64,
}

/// Distribution summaries over a scenario's observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summaries {
    pub total_ms: Summary,
    pub stages_ms: BTreeMap<Stage, Summary>,
    pub checkout_phases_ms: BTreeMap<CheckoutPhase, Summary>,
    /// Per-lane totals; the whole point of the lane split.
    pub lane_ms: BTreeMap<TelemetryLane, Summary>,
    pub cpu_user_us: Summary,
    pub cpu_system_us: Summary,
    pub max_rss_bytes: Summary,
    pub block_input_ops: Summary,
    pub block_output_ops: Summary,
    pub process_count: Summary,
    pub docker_invocations: Summary,
    pub cache_hits: Summary,
    pub cache_misses: Summary,
    pub bytes_copied: Summary,
    pub bytes_downloaded: Summary,
    pub bytes_reused: Summary,
}

impl Summaries {
    /// Summarise a sample.
    ///
    /// # Errors
    /// Fewer observations than [`crate::stats::MIN_SAMPLES`].
    pub fn new(observations: &[Observation]) -> Result<Self, TooFewSamples> {
        let field = |extract: fn(&Observation) -> u64| -> Result<Summary, TooFewSamples> {
            let values: Vec<u64> = observations.iter().map(extract).collect();
            Summary::new(&values)
        };

        let mut stages_ms = BTreeMap::new();
        for stage in Stage::ALL {
            if observations
                .iter()
                .all(|observation| observation.stages_ms.contains_key(&stage))
            {
                let values: Vec<u64> = observations
                    .iter()
                    .map(|observation| observation.stages_ms[&stage])
                    .collect();
                stages_ms.insert(stage, Summary::new(&values)?);
            }
        }

        let mut checkout_phases_ms = BTreeMap::new();
        for phase in CheckoutPhase::ALL {
            if observations
                .iter()
                .all(|observation| observation.checkout_phases_ms.contains_key(&phase))
            {
                let values: Vec<u64> = observations
                    .iter()
                    .map(|observation| observation.checkout_phases_ms[&phase])
                    .collect();
                checkout_phases_ms.insert(phase, Summary::new(&values)?);
            }
        }

        let mut lane_ms = BTreeMap::new();
        for lane in [TelemetryLane::Velnor, TelemetryLane::Github] {
            let values: Vec<u64> = observations
                .iter()
                .map(|observation| observation.lane_ms(lane))
                .collect();
            lane_ms.insert(lane, Summary::new(&values)?);
        }

        Ok(Self {
            total_ms: field(|observation| observation.total_ms)?,
            stages_ms,
            checkout_phases_ms,
            lane_ms,
            cpu_user_us: field(|observation| observation.resources.cpu_user_us)?,
            cpu_system_us: field(|observation| observation.resources.cpu_system_us)?,
            max_rss_bytes: field(|observation| observation.resources.max_rss_bytes)?,
            block_input_ops: field(|observation| observation.resources.block_input_ops)?,
            block_output_ops: field(|observation| observation.resources.block_output_ops)?,
            process_count: field(|observation| observation.resources.process_count)?,
            docker_invocations: field(|observation| observation.resources.docker_invocations)?,
            cache_hits: field(|observation| observation.resources.cache_hits)?,
            cache_misses: field(|observation| observation.resources.cache_misses)?,
            bytes_copied: field(|observation| observation.resources.bytes_copied)?,
            bytes_downloaded: field(|observation| observation.resources.bytes_downloaded)?,
            bytes_reused: field(|observation| observation.resources.bytes_reused)?,
        })
    }
}

/// One scenario result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchRecord {
    pub schema: String,
    pub run_id: String,
    pub recorded_at_unix_ms: u64,
    pub scenario: String,
    pub family: Family,
    pub driver: Driver,
    pub runnability: Runnability,
    /// Mandatory: a result without environment identity is not a result.
    pub environment: EnvironmentIdentity,
    pub observations: Vec<Observation>,
    pub summaries: Summaries,
    /// Anything a reader must know to interpret the numbers honestly.
    pub notes: Vec<String>,
}

/// Why a record is not a valid measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    UnknownScenario(String),
    StageOutsideDriverCoverage {
        driver: Driver,
        stage: Stage,
    },
    DriverRunnabilityMismatch {
        driver: Driver,
        runnability_driver: Option<Driver>,
    },
    DeclaredDriverMismatch {
        runnability_driver: Option<Driver>,
        declared_driver: Option<Driver>,
    },
    DegradedRequirementsEmpty,
    DuplicateDegradedRequirement {
        requirement: Requirement,
    },
    DegradedRequirementNotRequired {
        requirement: Requirement,
    },
    ScenarioFamilyMismatch,
    InvalidGitEvidence,
    WrongSchema(String),
    InsufficientObservations {
        samples: usize,
        required: usize,
    },
    SummaryMismatch,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScenario(id) => {
                write!(formatter, "{id} is not a declared scenario")
            }
            Self::StageOutsideDriverCoverage { driver, stage } => write!(
                formatter,
                "driver {} cannot observe stage {}",
                driver.as_str(),
                stage.as_str()
            ),
            Self::DriverRunnabilityMismatch {
                driver,
                runnability_driver,
            } => write!(
                formatter,
                "record driver {} contradicts runnability driver {}",
                driver.as_str(),
                runnability_driver.map_or("unrunnable", Driver::as_str)
            ),
            Self::DeclaredDriverMismatch {
                runnability_driver,
                declared_driver,
            } => write!(
                formatter,
                "record runnability driver {} contradicts declared scenario driver {}",
                runnability_driver.map_or("unrunnable", Driver::as_str),
                declared_driver.map_or("unrunnable", Driver::as_str)
            ),
            Self::DegradedRequirementsEmpty => write!(
                formatter,
                "degraded record must identify at least one missing preferred requirement"
            ),
            Self::DuplicateDegradedRequirement { requirement } => write!(
                formatter,
                "degraded record repeats missing preferred requirement {}",
                requirement.as_str()
            ),
            Self::DegradedRequirementNotRequired { requirement } => write!(
                formatter,
                "degraded record identifies {} as missing, but the scenario does not require it",
                requirement.as_str()
            ),
            Self::ScenarioFamilyMismatch => {
                write!(
                    formatter,
                    "record family does not match the declared scenario"
                )
            }
            Self::InvalidGitEvidence => {
                write!(
                    formatter,
                    "record contains structurally invalid Git evidence"
                )
            }
            Self::WrongSchema(schema) => write!(formatter, "unexpected schema {schema}"),
            Self::InsufficientObservations { samples, required } => write!(
                formatter,
                "record contains {samples} observation(s); {required} required for summaries"
            ),
            Self::SummaryMismatch => {
                write!(
                    formatter,
                    "record summaries are not derived from observations"
                )
            }
        }
    }
}

impl std::error::Error for RecordError {}

impl BenchRecord {
    /// Check the invariants the schema alone cannot express.
    ///
    /// # Errors
    /// Unknown scenario, family mismatch, malformed degraded or Git evidence,
    /// wrong schema, a stage the driver is structurally unable to observe, or
    /// summaries not derived from the observations.
    pub fn validate(&self) -> Result<(), RecordError> {
        if self.schema != RESULT_SCHEMA {
            return Err(RecordError::WrongSchema(self.schema.clone()));
        }
        let scenario = crate::scenario::find(&self.scenario)
            .ok_or_else(|| RecordError::UnknownScenario(self.scenario.clone()))?;
        if scenario.family != self.family {
            return Err(RecordError::ScenarioFamilyMismatch);
        }
        if self.runnability.driver() != Some(self.driver) {
            return Err(RecordError::DriverRunnabilityMismatch {
                driver: self.driver,
                runnability_driver: self.runnability.driver(),
            });
        }
        let declared_driver = match &self.runnability {
            Runnability::Preferred { .. } => Some(scenario.preferred),
            Runnability::Degraded { .. } => scenario.fallback,
            Runnability::Unrunnable { .. } => None,
        };
        if self.runnability.driver() != declared_driver && self.runnability.driver().is_some() {
            return Err(RecordError::DeclaredDriverMismatch {
                runnability_driver: self.runnability.driver(),
                declared_driver,
            });
        }
        if let Runnability::Degraded {
            missing_for_preferred,
            ..
        } = &self.runnability
        {
            if missing_for_preferred.is_empty() {
                return Err(RecordError::DegradedRequirementsEmpty);
            }
            for (index, requirement) in missing_for_preferred.iter().enumerate() {
                if missing_for_preferred[..index].contains(requirement) {
                    return Err(RecordError::DuplicateDegradedRequirement {
                        requirement: *requirement,
                    });
                }
                if !scenario.requires.contains(requirement) {
                    return Err(RecordError::DegradedRequirementNotRequired {
                        requirement: *requirement,
                    });
                }
            }
        }
        let observable = self.driver.observable_stages();
        for observation in &self.observations {
            if !observation.git.is_valid() {
                return Err(RecordError::InvalidGitEvidence);
            }
            for stage in observation.stages_ms.keys() {
                if !observable.contains(stage) {
                    return Err(RecordError::StageOutsideDriverCoverage {
                        driver: self.driver,
                        stage: *stage,
                    });
                }
            }
        }
        let expected = Summaries::new(&self.observations).map_err(|error| {
            RecordError::InsufficientObservations {
                samples: error.samples,
                required: error.required,
            }
        })?;
        if self.summaries != expected {
            return Err(RecordError::SummaryMismatch);
        }
        Ok(())
    }

    /// Serialise as one NDJSON line.
    ///
    /// # Errors
    /// Serialisation failure.
    pub fn to_ndjson(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{env::ProbeInputs, sys::Runner};

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

    fn observation(total: u64, stage: Stage) -> Observation {
        Observation {
            total_ms: total,
            stages_ms: BTreeMap::from([(stage, total)]),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources {
                process_count: 3,
                docker_invocations: 2,
                ..Resources::default()
            },
            git: GitEvidence::NotMeasured,
        }
    }

    fn record(driver: Driver, stage: Stage) -> BenchRecord {
        let observations: Vec<Observation> = (1..=4)
            .map(|index| observation(index * 10, stage))
            .collect();
        BenchRecord {
            schema: RESULT_SCHEMA.to_owned(),
            run_id: "test".to_owned(),
            recorded_at_unix_ms: 1,
            scenario: "docker/existing-image".to_owned(),
            family: Family::Docker,
            driver,
            runnability: Runnability::Degraded {
                driver,
                missing_for_preferred: vec![crate::scenario::Requirement::VelnorJobDriver],
            },
            environment: environment(),
            observations: observations.clone(),
            summaries: Summaries::new(&observations).expect("summaries"),
            notes: Vec::new(),
        }
    }

    fn degraded_record(missing_for_preferred: Vec<Requirement>) -> BenchRecord {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.runnability = Runnability::Degraded {
            driver: Driver::DockerDirect,
            missing_for_preferred,
        };
        record
    }

    #[test]
    fn a_valid_record_round_trips_and_validates() {
        let record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.validate().expect("valid");
        let line = record.to_ndjson().expect("serialise");
        assert!(!line.contains('\n'));
        let parsed: BenchRecord = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(parsed, record);
    }

    #[test]
    fn git_evidence_serialization_retains_its_state() {
        let evidence = GitEvidence::Observed {
            counters: crate::gittrace::GitCounters {
                received_bytes: 17,
                processes: 1,
                ..crate::gittrace::GitCounters::default()
            },
            successful: false,
        };
        let value = serde_json::to_value(&evidence).expect("serialize evidence");
        assert_eq!(value["status"], "observed");
        assert_eq!(value["successful"], false);
        assert_eq!(
            serde_json::from_value::<GitEvidence>(value).expect("deserialize evidence"),
            evidence
        );
        assert_eq!(
            serde_json::to_value(GitEvidence::NoGitTraceObserved).expect("serialize no-Git state")
                ["status"],
            "no_git_trace_observed"
        );
        let mixed = GitEvidence::Mixed {
            counters: crate::gittrace::GitCounters {
                processes: 1,
                ..crate::gittrace::GitCounters::default()
            },
            successful: true,
            observed_workers: 1,
            no_git_workers: 1,
        };
        assert!(mixed.is_valid());
        assert_eq!(
            serde_json::to_value(mixed).expect("serialize mixed state")["status"],
            "mixed"
        );
    }

    #[test]
    fn a_record_rejects_observed_git_evidence_without_processes() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations[0].git = GitEvidence::Observed {
            counters: crate::gittrace::GitCounters::default(),
            successful: true,
        };
        assert_eq!(record.validate(), Err(RecordError::InvalidGitEvidence));
    }

    #[test]
    fn a_degraded_record_requires_missing_preferred_evidence() {
        let record = degraded_record(Vec::new());
        assert_eq!(
            record.validate(),
            Err(RecordError::DegradedRequirementsEmpty)
        );
    }

    #[test]
    fn a_degraded_record_rejects_duplicate_missing_requirements() {
        let record = degraded_record(vec![
            Requirement::VelnorJobDriver,
            Requirement::VelnorJobDriver,
        ]);
        assert_eq!(
            record.validate(),
            Err(RecordError::DuplicateDegradedRequirement {
                requirement: Requirement::VelnorJobDriver,
            })
        );
    }

    #[test]
    fn a_degraded_record_rejects_requirements_not_declared_by_the_scenario() {
        let record = degraded_record(vec![Requirement::LinuxHost]);
        assert_eq!(
            record.validate(),
            Err(RecordError::DegradedRequirementNotRequired {
                requirement: Requirement::LinuxHost,
            })
        );
    }

    #[test]
    fn a_degraded_record_accepts_unique_scenario_requirements() {
        let record = degraded_record(vec![
            Requirement::VelnorJobDriver,
            Requirement::RegisteredRunner,
        ]);
        record.validate().expect("valid degraded evidence");
    }

    #[test]
    fn a_record_without_environment_identity_is_rejected_by_the_schema() {
        let record = record(Driver::DockerDirect, Stage::ContainerStart);
        let mut value = serde_json::to_value(&record).expect("serialise");
        value
            .as_object_mut()
            .expect("object")
            .remove("environment")
            .expect("environment present");
        let error = serde_json::from_value::<BenchRecord>(value).expect_err("must be rejected");
        assert!(error.to_string().contains("environment"), "{error}");
    }

    #[test]
    fn a_partial_environment_is_rejected_by_the_schema() {
        let record = record(Driver::DockerDirect, Stage::ContainerStart);
        let mut value = serde_json::to_value(&record).expect("serialise");
        value["environment"]
            .as_object_mut()
            .expect("object")
            .remove("docker_storage_driver")
            .expect("field present");
        assert!(serde_json::from_value::<BenchRecord>(value).is_err());
    }

    #[test]
    fn a_container_driver_may_not_claim_broker_latency() {
        let record = record(Driver::DockerDirect, Stage::BrokerDelivery);
        let error = record.validate().expect_err("must be rejected");
        assert_eq!(
            error,
            RecordError::StageOutsideDriverCoverage {
                driver: Driver::DockerDirect,
                stage: Stage::BrokerDelivery,
            }
        );
    }

    #[test]
    fn an_unrunnable_record_may_not_carry_a_driver_or_observations() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.runnability = Runnability::Unrunnable {
            missing: vec![crate::scenario::Requirement::DockerDaemon],
        };
        assert_eq!(
            record.validate(),
            Err(RecordError::DriverRunnabilityMismatch {
                driver: Driver::DockerDirect,
                runnability_driver: None,
            })
        );
    }

    #[test]
    fn a_disabled_image_pull_cannot_claim_a_docker_fallback() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.scenario = "docker/image-pull".to_owned();
        record.runnability = Runnability::Degraded {
            driver: Driver::DockerDirect,
            missing_for_preferred: vec![crate::scenario::Requirement::VelnorJobDriver],
        };
        assert_eq!(
            record.validate(),
            Err(RecordError::DeclaredDriverMismatch {
                runnability_driver: Some(Driver::DockerDirect),
                declared_driver: None,
            })
        );
    }

    #[test]
    fn an_undeclared_scenario_is_rejected() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.scenario = "docker/invented".to_owned();
        assert!(matches!(
            record.validate(),
            Err(RecordError::UnknownScenario(_))
        ));
    }

    #[test]
    fn lane_totals_separate_velnor_from_github() {
        let observation = Observation {
            total_ms: 100,
            stages_ms: BTreeMap::from([
                (Stage::BrokerDelivery, 60),
                (Stage::ContainerStart, 30),
                (Stage::Teardown, 10),
            ]),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources::default(),
            git: GitEvidence::NotMeasured,
        };
        assert_eq!(observation.lane_ms(TelemetryLane::Github), 60);
        assert_eq!(observation.lane_ms(TelemetryLane::Velnor), 40);
    }

    #[test]
    fn summaries_refuse_a_single_observation() {
        let observations = vec![observation(10, Stage::ContainerStart)];
        assert!(Summaries::new(&observations).is_err());
    }

    #[test]
    fn a_record_rejects_summaries_not_derived_from_observations() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.summaries.total_ms.max += 1;
        assert_eq!(record.validate(), Err(RecordError::SummaryMismatch));
    }

    #[test]
    fn a_record_with_too_few_observations_is_rejected() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations.truncate(2);
        assert_eq!(
            record.validate(),
            Err(RecordError::InsufficientObservations {
                samples: 2,
                required: crate::stats::MIN_SAMPLES,
            })
        );
    }
}

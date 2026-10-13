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
    census::DockerCensus,
    env::EnvironmentIdentity,
    fault::FaultOutcome,
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
    /// Per-class Docker invocation census, in the runner's closed vocabulary.
    /// Defaulted so records written before the census existed still parse.
    #[serde(default)]
    pub docker_census: DockerCensus,
    /// Fault outcome. `Some` exactly for the `fault/*` scenarios.
    #[serde(default)]
    pub fault: Option<FaultOutcome>,
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

/// Distribution summaries for one Docker operation class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CensusSummary {
    pub count: Summary,
    pub latency_ms: Summary,
}

/// Distribution summaries over a scenario's observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summaries {
    pub total_ms: Summary,
    pub stages_ms: BTreeMap<Stage, Summary>,
    pub checkout_phases_ms: BTreeMap<CheckoutPhase, Summary>,
    /// Per-class census summaries, keyed by the runner's class label. A class
    /// is summarised when any observation carries it, with zeroes filled in
    /// for the observations that do not: census absence is observed-zero, not
    /// unobserved like a missing stage. Defaulted so records written before
    /// the census existed still parse.
    #[serde(default)]
    pub docker_census: BTreeMap<String, CensusSummary>,
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

        // The census labels are a closed set — the runner's DockerOp
        // vocabulary — so iterate it. Unlike stages, a class missing from some
        // observation is observed-zero for that round, not unobserved: the
        // census only carries classes the round actually invoked. Zero-fill
        // the gaps so an intermittent class is summarised instead of silently
        // dropped.
        let mut docker_census = BTreeMap::new();
        for op in velnor_runner::docker::DockerOp::ALL {
            let label = op.label();
            if observations
                .iter()
                .any(|observation| observation.docker_census.by_class.contains_key(label))
            {
                let counts: Vec<u64> = observations
                    .iter()
                    .map(|observation| {
                        observation
                            .docker_census
                            .by_class
                            .get(label)
                            .map_or(0, |class| class.count)
                    })
                    .collect();
                let latencies: Vec<u64> = observations
                    .iter()
                    .map(|observation| {
                        observation
                            .docker_census
                            .by_class
                            .get(label)
                            .map_or(0, |class| class.latency_ms)
                    })
                    .collect();
                docker_census.insert(
                    label.to_owned(),
                    CensusSummary {
                        count: Summary::new(&counts)?,
                        latency_ms: Summary::new(&latencies)?,
                    },
                );
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
            docker_census,
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
    /// Comparison dimensions: runner identity, trust class, image tag — the
    /// answer to "what differs between the two runs being compared". Defaulted
    /// so records written before it existed still parse.
    #[serde(default)]
    pub context: BTreeMap<String, String>,
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
    GitBytesMismatch {
        evidence: u64,
        resources: u64,
    },
    WrongSchema(String),
    InsufficientObservations {
        samples: usize,
        required: usize,
    },
    SummaryMismatch,
    UnknownCensusLabel {
        label: String,
    },
    CensusCountMismatch {
        census: u64,
        resources: u64,
    },
    MissingFaultOutcome,
    UnexpectedFaultOutcome,
    UnknownFaultClass {
        class: String,
    },
    FaultResidueWhileContained {
        class: String,
    },
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
            Self::GitBytesMismatch {
                evidence,
                resources,
            } => write!(
                formatter,
                "Git evidence reports {evidence} received bytes but resources report {resources}"
            ),
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
            Self::UnknownCensusLabel { label } => write!(
                formatter,
                "census label {label:?} is not in the runner's DockerOp vocabulary"
            ),
            Self::CensusCountMismatch { census, resources } => write!(
                formatter,
                "census counts {census} docker invocations but resources report {resources}"
            ),
            Self::MissingFaultOutcome => write!(
                formatter,
                "a fault scenario observation must carry its fault outcome"
            ),
            Self::UnexpectedFaultOutcome => write!(
                formatter,
                "only fault scenario observations may carry a fault outcome"
            ),
            Self::UnknownFaultClass { class } => write!(
                formatter,
                "fault outcome names unknown catalogue class {class:?}"
            ),
            Self::FaultResidueWhileContained { class } => write!(
                formatter,
                "fault outcome for {class:?} claims containment with residue left behind"
            ),
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
            if !observation.docker_census.labels_are_known() {
                let label = observation
                    .docker_census
                    .by_class
                    .keys()
                    .find(|label| {
                        !velnor_runner::docker::DockerOp::ALL
                            .iter()
                            .any(|op| op.label() == label.as_str())
                    })
                    .cloned()
                    .unwrap_or_default();
                return Err(RecordError::UnknownCensusLabel { label });
            }
            if observation.docker_census.total() != observation.resources.docker_invocations {
                return Err(RecordError::CensusCountMismatch {
                    census: observation.docker_census.total(),
                    resources: observation.resources.docker_invocations,
                });
            }
            let is_fault_row = scenario.family == Family::Fault;
            match &observation.fault {
                None if is_fault_row => return Err(RecordError::MissingFaultOutcome),
                Some(_) if !is_fault_row => return Err(RecordError::UnexpectedFaultOutcome),
                Some(outcome) => {
                    if crate::fault::find(&outcome.class).is_none() {
                        return Err(RecordError::UnknownFaultClass {
                            class: outcome.class.clone(),
                        });
                    }
                    // A never-injected outcome still validates: rejecting it
                    // here would discard the record — and the detail
                    // diagnostics pinning down why the fault never triggered.
                    // `run` writes the record and exits nonzero instead, on
                    // the same path as an uncontained outcome.
                    if outcome.contained && !outcome.residue.is_empty() {
                        return Err(RecordError::FaultResidueWhileContained {
                            class: outcome.class.clone(),
                        });
                    }
                }
                None => {}
            }
            if matches!(
                &observation.git,
                GitEvidence::Observed { .. } | GitEvidence::Mixed { .. }
            ) && observation.git.received_bytes() != observation.resources.bytes_downloaded
            {
                return Err(RecordError::GitBytesMismatch {
                    evidence: observation.git.received_bytes(),
                    resources: observation.resources.bytes_downloaded,
                });
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
            docker_census: DockerCensus {
                by_class: BTreeMap::from([(
                    "query".to_owned(),
                    crate::census::ClassObservation {
                        count: 2,
                        latency_ms: 4,
                    },
                )]),
            },
            fault: None,
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
            context: BTreeMap::new(),
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
    fn no_git_trace_evidence_serializes_with_the_current_v2_discriminator() {
        assert_eq!(
            serde_json::to_value(GitEvidence::NoGitTraceObserved).expect("serialize no-Git state"),
            serde_json::json!({"status": "no_git_trace_observed"})
        );
    }

    #[test]
    fn legacy_v2_no_git_process_evidence_deserializes_to_current_state() {
        let legacy = serde_json::json!({"status": "no_git_process"});

        assert_eq!(
            serde_json::from_value::<GitEvidence>(legacy).expect("deserialize legacy no-Git state"),
            GitEvidence::NoGitTraceObserved
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
    fn a_record_rejects_observed_git_bytes_that_disagree_with_resources() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations[0].git = GitEvidence::Observed {
            counters: crate::gittrace::GitCounters {
                received_bytes: 17,
                processes: 1,
                ..crate::gittrace::GitCounters::default()
            },
            successful: true,
        };
        record.observations[0].resources.bytes_downloaded = 11;
        record.summaries = Summaries::new(&record.observations).expect("summaries");
        assert_eq!(
            record.validate(),
            Err(RecordError::GitBytesMismatch {
                evidence: 17,
                resources: 11,
            })
        );
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
    fn image_pull_accepts_the_declared_isolated_docker_fallback() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.scenario = "docker/image-pull".to_owned();
        record.runnability = Runnability::Degraded {
            driver: Driver::DockerDirect,
            missing_for_preferred: vec![crate::scenario::Requirement::VelnorJobDriver],
        };
        record.validate().expect("declared fallback is valid");
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
            docker_census: DockerCensus::default(),
            fault: None,
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

    #[test]
    fn the_census_is_summarised_per_class() {
        let record = record(Driver::DockerDirect, Stage::ContainerStart);
        let summary = record
            .summaries
            .docker_census
            .get("query")
            .expect("query census summary");
        assert_eq!(summary.count.samples, 4);
        assert_eq!(summary.count.min, 2);
        assert_eq!(summary.count.max, 2);
        assert_eq!(summary.latency_ms.min, 4);
    }

    #[test]
    fn a_census_class_missing_from_some_iterations_is_zero_filled_not_dropped() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        // One round invoked no docker at all: its census is empty and its
        // resource count agrees, so absence is observed-zero.
        record.observations[0].docker_census.by_class.clear();
        record.observations[0].resources.docker_invocations = 0;
        record.summaries = Summaries::new(&record.observations).expect("summaries");
        let summary = record
            .summaries
            .docker_census
            .get("query")
            .expect("an intermittent class is still summarised");
        assert_eq!(summary.count.samples, 4);
        assert_eq!(summary.count.min, 0);
        assert_eq!(summary.count.max, 2);
        assert_eq!(summary.latency_ms.min, 0);
        assert_eq!(summary.latency_ms.max, 4);
        record.validate().expect("zero-filled summaries validate");
    }

    #[test]
    fn a_census_label_outside_the_runner_vocabulary_is_rejected() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations[0].docker_census.by_class.insert(
            "docker-exec".to_owned(),
            crate::census::ClassObservation {
                count: 1,
                latency_ms: 1,
            },
        );
        // Keep the totals consistent so the label is what fails.
        record.observations[0].resources.docker_invocations = 3;
        assert_eq!(
            record.validate(),
            Err(RecordError::UnknownCensusLabel {
                label: "docker-exec".to_owned(),
            })
        );
    }

    #[test]
    fn a_census_that_disagrees_with_resources_is_rejected() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations[0].resources.docker_invocations = 99;
        assert_eq!(
            record.validate(),
            Err(RecordError::CensusCountMismatch {
                census: 2,
                resources: 99,
            })
        );
    }

    fn fault_record() -> BenchRecord {
        let outcome = crate::fault::FaultOutcome {
            class: "docker-kill-mid-step".to_owned(),
            injected: true,
            contained: true,
            residue: Vec::new(),
            detail: "kill delivered; teardown removed the container".to_owned(),
        };
        let observations: Vec<Observation> = (1..=4)
            .map(|index| {
                let mut observation = observation(index * 10, Stage::ContainerStart);
                observation.fault = Some(outcome.clone());
                observation
            })
            .collect();
        BenchRecord {
            schema: RESULT_SCHEMA.to_owned(),
            run_id: "fault-test".to_owned(),
            recorded_at_unix_ms: 1,
            scenario: "fault/container-killed-mid-step".to_owned(),
            family: Family::Fault,
            driver: Driver::DockerDirect,
            runnability: Runnability::Degraded {
                driver: Driver::DockerDirect,
                missing_for_preferred: vec![crate::scenario::Requirement::VelnorJobDriver],
            },
            environment: environment(),
            observations: observations.clone(),
            summaries: Summaries::new(&observations).expect("summaries"),
            notes: Vec::new(),
            context: BTreeMap::new(),
        }
    }

    #[test]
    fn a_fault_record_with_a_catalogued_outcome_validates() {
        fault_record().validate().expect("valid fault record");
    }

    #[test]
    fn a_fault_row_without_an_outcome_is_rejected() {
        let mut record = fault_record();
        record.observations[0].fault = None;
        assert_eq!(record.validate(), Err(RecordError::MissingFaultOutcome));
    }

    #[test]
    fn a_non_fault_row_with_an_outcome_is_rejected() {
        let mut record = record(Driver::DockerDirect, Stage::ContainerStart);
        record.observations[0].fault = Some(crate::fault::FaultOutcome {
            class: "docker-kill-mid-step".to_owned(),
            injected: true,
            contained: true,
            residue: Vec::new(),
            detail: String::new(),
        });
        assert_eq!(record.validate(), Err(RecordError::UnexpectedFaultOutcome));
    }

    #[test]
    fn a_fault_outcome_outside_the_catalogue_is_rejected() {
        let mut record = fault_record();
        record.observations[0]
            .fault
            .as_mut()
            .expect("outcome")
            .class = "docker-invented".to_owned();
        assert_eq!(
            record.validate(),
            Err(RecordError::UnknownFaultClass {
                class: "docker-invented".to_owned(),
            })
        );
    }

    #[test]
    fn a_fault_that_never_triggered_keeps_its_record() {
        // Validation accepts the miss so `run` can write the record — with
        // the detail diagnostics — and exit nonzero on the uncontained path.
        let mut record = fault_record();
        let outcome = record.observations[0].fault.as_mut().expect("outcome");
        outcome.injected = false;
        outcome.contained = false;
        outcome.detail = "running=false kill_exit=0 wait_exit=-1".to_owned();
        record
            .validate()
            .expect("an uninjected run is still a record");
    }

    #[test]
    fn containment_with_residue_is_rejected() {
        let mut record = fault_record();
        let outcome = record.observations[0].fault.as_mut().expect("outcome");
        outcome
            .residue
            .push("container velnor-bench-fault-1".to_owned());
        assert_eq!(
            record.validate(),
            Err(RecordError::FaultResidueWhileContained {
                class: "docker-kill-mid-step".to_owned(),
            })
        );
    }

    #[test]
    fn records_written_before_the_census_still_parse() {
        // The census and fault fields are defaulted: an old v2 line without
        // them deserialises, with an empty census.
        let record = record(Driver::DockerDirect, Stage::ContainerStart);
        let mut value = serde_json::to_value(&record).expect("serialise");
        for observation in value["observations"].as_array_mut().expect("observations") {
            observation
                .as_object_mut()
                .expect("observation")
                .remove("docker_census");
            observation
                .as_object_mut()
                .expect("observation")
                .remove("fault");
        }
        value
            .get_mut("summaries")
            .expect("summaries")
            .as_object_mut()
            .expect("summaries object")
            .remove("docker_census");
        let parsed: BenchRecord = serde_json::from_value(value).expect("old record parses");
        assert!(parsed.summaries.docker_census.is_empty());
        // The census defaults empty while resources still report invocations,
        // so validation reports the mismatch honestly instead of passing.
        assert_eq!(
            parsed.validate(),
            Err(RecordError::CensusCountMismatch {
                census: 0,
                resources: 2,
            })
        );
        let mut reparsed = parsed.clone();
        for observation in &mut reparsed.observations {
            observation.resources.docker_invocations = 0;
        }
        reparsed.summaries = Summaries::new(&reparsed.observations).expect("summaries");
        reparsed
            .validate()
            .expect("zero-docker old record validates");
    }
}

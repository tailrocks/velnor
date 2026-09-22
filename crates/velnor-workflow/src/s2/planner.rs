//! Affected/full planner → three-provider fanout.
//!
//! One planner run selects units, then every selected eligible Linux unit fans
//! out to all three providers with identical source, command, profile,
//! features, fixtures, and test expectations. Display names expose the
//! provider verbatim; official and native lanes route to disjoint dedicated
//! selectors; each lane carries its own typed bootstrap.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::{BTreeMap, BTreeSet};

use crate::s2::provider::{
    plan_digest, unit_job_display_name, unit_job_id, validate_selector_disjointness, Capabilities,
    ExclusionReason, PlanUnitIdentity, Platform, ProviderId, ProviderSet, SelectorMap, TrustReq,
};
use crate::s2::routing::{route_unit, RouteDecision, UnitRequirements};
use crate::s2::GeneratorError;

/// One unit as the planner sees it: identity plus the digest over everything
/// that must be identical on every lane (source, command, profile, features,
/// fixtures, expectations).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedUnit {
    pub(crate) unit_id: String,
    pub(crate) label: String,
    pub(crate) platform: Platform,
    pub(crate) trust: TrustReq,
    pub(crate) capabilities: Capabilities,
    pub(crate) command_digest: String,
}

impl PlannedUnit {
    fn requirements(&self) -> UnitRequirements {
        UnitRequirements {
            unit_id: self.unit_id.clone(),
            platform: self.platform,
            trust: self.trust,
            capabilities: self.capabilities,
        }
    }
}

/// What the planner selected. Diagnostic subsets are labeled reduced coverage,
/// never successful full qualification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Coverage {
    /// Every known unit is selected.
    Full,
    /// Only units affected by the change are selected; the rest are excluded
    /// with [`ExclusionReason::GenuinelyUnaffected`].
    Affected { changed: Vec<String> },
    /// A diagnostic subset: visible reduced coverage.
    Reduced { reason: String },
}

/// One planner-declared exclusion of a (unit, provider) pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Exclusion {
    pub(crate) unit_id: String,
    pub(crate) provider: ProviderId,
    pub(crate) reason: ExclusionReason,
}

/// One provider-specific bootstrap step. Typed: the lane's bootstrap is data,
/// never an arbitrary command string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootstrapStep {
    /// Check out the pinned source SHA. The native lane binds the mediated
    /// workspace instead of running a checkout action.
    Checkout { mediated: bool },
    /// Restore the lane-namespaced cache entry.
    RestoreCache,
    /// Verify the lane's Docker/buildx/services readiness before unit steps.
    VerifyLane,
}

/// The bootstrap for one provider lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BootstrapLane {
    pub(crate) provider: ProviderId,
    pub(crate) steps: Vec<BootstrapStep>,
}

impl BootstrapLane {
    /// The bootstrap each lane runs. Hosted and official lanes check out via
    /// the checkout action; the native lane binds the mediated workspace.
    /// Lanes whose units need Docker-class capabilities verify lane readiness.
    #[must_use]
    pub(crate) fn for_provider(provider: ProviderId, needs_lane: bool) -> Self {
        let mut steps = vec![
            BootstrapStep::Checkout {
                mediated: provider == ProviderId::Velnor,
            },
            BootstrapStep::RestoreCache,
        ];
        if needs_lane {
            steps.push(BootstrapStep::VerifyLane);
        }
        Self { provider, steps }
    }
}

/// One executable (unit, provider) pair: identical inputs on every lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Execution {
    pub(crate) unit_id: String,
    pub(crate) provider: ProviderId,
    pub(crate) job_id: String,
    pub(crate) display_name: String,
    pub(crate) runs_on: Vec<String>,
    pub(crate) platform: Platform,
    pub(crate) trust: TrustReq,
    /// The planned command digest, copied verbatim: the fanout invariant is
    /// that every lane of a unit carries this same digest.
    pub(crate) command_digest: String,
    pub(crate) bootstrap: BootstrapLane,
}

/// The frozen plan: executions plus planner-declared exclusions plus digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Plan {
    pub(crate) coverage: Coverage,
    pub(crate) executions: Vec<Execution>,
    pub(crate) exclusions: Vec<Exclusion>,
    pub(crate) digest: String,
}

impl Plan {
    /// The expected (unit, provider) pairs: exactly the executions.
    #[must_use]
    pub(crate) fn expected_pairs(&self) -> BTreeSet<(String, ProviderId)> {
        self.executions
            .iter()
            .map(|execution| (execution.unit_id.clone(), execution.provider))
            .collect()
    }

    /// The per-unit command digests the strict verdict binds records to.
    #[must_use]
    pub(crate) fn command_digests(&self) -> BTreeMap<String, String> {
        let mut digests = BTreeMap::new();
        for execution in &self.executions {
            digests.insert(execution.unit_id.clone(), execution.command_digest.clone());
        }
        digests
    }
}

/// Whether a unit's capabilities need lane-readiness verification.
fn needs_lane_verification(capabilities: Capabilities) -> bool {
    capabilities.docker
        || capabilities.nested_privileged_docker
        || capabilities.buildx_compose
        || capabilities.testcontainers
        || capabilities.services_with_readiness
}

/// Fan out selected units over the provider universe.
///
/// `affected` selects the coverage: `None` plans full coverage, `Some(ids)`
/// plans affected coverage with every other known unit excluded as genuinely
/// unaffected on every provider in the universe.
///
/// # Errors
/// Returns a usage error for overlapping local selectors, an unsupported
/// capability, or a missing selector — all fail explicitly before expansion.
pub(crate) fn fanout(
    units: &[PlannedUnit],
    affected: Option<&BTreeSet<String>>,
    universe: &ProviderSet,
    selectors: &SelectorMap,
    event_trusted: bool,
) -> Result<Plan, GeneratorError> {
    validate_selector_disjointness(selectors)?;
    let mut units = units.to_vec();
    units.sort_by(|left, right| left.unit_id.cmp(&right.unit_id));
    let coverage = match affected {
        None => Coverage::Full,
        Some(ids) => Coverage::Affected {
            changed: {
                let mut changed: Vec<String> = ids.iter().cloned().collect();
                changed.sort();
                changed
            },
        },
    };
    let mut executions = Vec::new();
    let mut exclusions = Vec::new();
    let mut digest_units: Vec<PlanUnitIdentity> = Vec::new();
    for unit in &units {
        let selected = affected.is_none_or(|ids| ids.contains(&unit.unit_id));
        if !selected {
            for provider in universe {
                exclusions.push(Exclusion {
                    unit_id: unit.unit_id.clone(),
                    provider: *provider,
                    reason: ExclusionReason::GenuinelyUnaffected,
                });
            }
            continue;
        }
        let mut routed = ProviderSet::new();
        for (provider, decision) in
            route_unit(&unit.requirements(), universe, selectors, event_trusted)?
        {
            match decision {
                RouteDecision::Route { runs_on } => {
                    routed.insert(provider);
                    executions.push(Execution {
                        unit_id: unit.unit_id.clone(),
                        provider,
                        job_id: unit_job_id(provider, &unit.unit_id),
                        display_name: unit_job_display_name(&unit.label, provider, &unit.unit_id),
                        runs_on,
                        platform: unit.platform,
                        trust: unit.trust,
                        command_digest: unit.command_digest.clone(),
                        bootstrap: BootstrapLane::for_provider(
                            provider,
                            needs_lane_verification(unit.capabilities),
                        ),
                    });
                }
                RouteDecision::Exclude { reason } => exclusions.push(Exclusion {
                    unit_id: unit.unit_id.clone(),
                    provider,
                    reason,
                }),
            }
        }
        digest_units.push(PlanUnitIdentity {
            unit_id: unit.unit_id.clone(),
            providers: routed,
            platform: unit.platform,
            trust: unit.trust,
            command_digest: unit.command_digest.clone(),
        });
    }
    exclusions.sort_by(|left, right| {
        (&left.unit_id, left.provider).cmp(&(&right.unit_id, right.provider))
    });
    let digest_exclusions: Vec<(String, ProviderId, ExclusionReason)> = exclusions
        .iter()
        .map(|exclusion| {
            (
                exclusion.unit_id.clone(),
                exclusion.provider,
                exclusion.reason,
            )
        })
        .collect();
    let digest = plan_digest(&digest_units, &digest_exclusions);
    Ok(Plan {
        coverage,
        executions,
        exclusions,
        digest,
    })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

    use super::*;
    use crate::s2::provider::ProviderSelector;

    fn selectors() -> SelectorMap {
        SelectorMap::from([
            (
                ProviderId::GithubHosted,
                ProviderSelector {
                    runs_on: vec!["ubuntu-24.04".to_owned()],
                },
            ),
            (
                ProviderId::GithubSelfHosted,
                ProviderSelector {
                    runs_on: vec!["velnor-official".to_owned()],
                },
            ),
            (
                ProviderId::Velnor,
                ProviderSelector {
                    runs_on: vec!["velnor-native".to_owned()],
                },
            ),
        ])
    }

    fn universe() -> ProviderSet {
        ProviderSet::from([
            ProviderId::GithubHosted,
            ProviderId::GithubSelfHosted,
            ProviderId::Velnor,
        ])
    }

    fn unit(id: &str) -> PlannedUnit {
        PlannedUnit {
            unit_id: id.to_owned(),
            label: "Rust".to_owned(),
            platform: Platform::LinuxX64,
            trust: TrustReq::UntrustedOk,
            capabilities: Capabilities::default(),
            command_digest: format!("digest-of-{id}"),
        }
    }

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn full_plan_fans_out_every_unit_to_all_three_with_identical_inputs() {
        let plan = fanout(
            &[unit("rust-a"), unit("rust-b")],
            None,
            &universe(),
            &selectors(),
            true,
        )
        .unwrap();
        assert_eq!(plan.coverage, Coverage::Full);
        assert_eq!(plan.executions.len(), 6);
        assert!(plan.exclusions.is_empty());
        for unit_id in ["rust-a", "rust-b"] {
            let lanes: Vec<&Execution> = plan
                .executions
                .iter()
                .filter(|execution| execution.unit_id == unit_id)
                .collect();
            assert_eq!(lanes.len(), 3);
            for lane in &lanes {
                assert_eq!(
                    lane.command_digest,
                    format!("digest-of-{unit_id}"),
                    "lane {} diverged from the planned inputs",
                    lane.provider
                );
            }
            let mut providers: Vec<ProviderId> = lanes.iter().map(|lane| lane.provider).collect();
            providers.sort();
            assert_eq!(providers, ProviderId::ALL);
        }
    }

    #[test]
    fn display_names_expose_the_provider_verbatim() {
        let plan = fanout(&[unit("rust-a")], None, &universe(), &selectors(), true).unwrap();
        for execution in &plan.executions {
            assert!(
                execution.display_name.contains(execution.provider.as_str()),
                "display name hides the provider: {}",
                execution.display_name
            );
            assert!(
                execution.display_name.contains(&execution.unit_id),
                "display name hides the unit: {}",
                execution.display_name
            );
        }
    }

    #[test]
    fn affected_plan_excludes_unaffected_units_explicitly() {
        let affected = BTreeSet::from(["rust-a".to_owned()]);
        let plan = fanout(
            &[unit("rust-a"), unit("rust-b")],
            Some(&affected),
            &universe(),
            &selectors(),
            true,
        )
        .unwrap();
        assert!(matches!(plan.coverage, Coverage::Affected { .. }));
        assert_eq!(plan.executions.len(), 3);
        assert!(plan
            .executions
            .iter()
            .all(|execution| execution.unit_id == "rust-a"));
        let unaffected: Vec<&Exclusion> = plan
            .exclusions
            .iter()
            .filter(|exclusion| exclusion.unit_id == "rust-b")
            .collect();
        assert_eq!(unaffected.len(), 3);
        assert!(unaffected
            .iter()
            .all(|exclusion| exclusion.reason == ExclusionReason::GenuinelyUnaffected));
    }

    #[test]
    fn overlapping_local_selectors_fail_before_expansion() {
        let mut selectors = selectors();
        selectors.insert(
            ProviderId::Velnor,
            ProviderSelector {
                runs_on: vec!["velnor-official".to_owned()],
            },
        );
        let error = must_fail(
            fanout(&[unit("rust-a")], None, &universe(), &selectors, true),
            "overlapping local selectors",
        );
        assert!(
            error.contains("velnor-official") && error.contains("disjoint"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn bootstrap_differs_per_lane() {
        let docker_unit = PlannedUnit {
            capabilities: Capabilities {
                docker: true,
                ..Capabilities::default()
            },
            ..unit("rust-docker")
        };
        let plan = fanout(
            std::slice::from_ref(&docker_unit),
            None,
            &universe(),
            &selectors(),
            true,
        )
        .unwrap();
        assert_eq!(plan.executions.len(), 3);
        for execution in &plan.executions {
            let mediated = execution.provider == ProviderId::Velnor;
            assert!(
                execution
                    .bootstrap
                    .steps
                    .contains(&BootstrapStep::Checkout { mediated }),
                "{} lane has the wrong checkout shape",
                execution.provider
            );
            assert!(
                execution
                    .bootstrap
                    .steps
                    .contains(&BootstrapStep::VerifyLane),
                "{} lane skipped lane verification",
                execution.provider
            );
        }
        let capability_free =
            fanout(&[unit("rust-a")], None, &universe(), &selectors(), true).unwrap();
        assert!(
            capability_free.executions.iter().all(|execution| !execution
                .bootstrap
                .steps
                .contains(&BootstrapStep::VerifyLane)),
            "capability-free units must not verify lanes"
        );
    }

    #[test]
    fn plan_digest_is_stable_and_covers_inputs_and_exclusions() {
        let units = [unit("rust-a"), unit("rust-b")];
        let first = fanout(&units, None, &universe(), &selectors(), true).unwrap();
        let reordered = fanout(
            &[unit("rust-b"), unit("rust-a")],
            None,
            &universe(),
            &selectors(),
            true,
        )
        .unwrap();
        assert_eq!(first.digest, reordered.digest);
        let changed = fanout(
            &[
                PlannedUnit {
                    command_digest: "changed".to_owned(),
                    ..unit("rust-a")
                },
                unit("rust-b"),
            ],
            None,
            &universe(),
            &selectors(),
            true,
        )
        .unwrap();
        assert_ne!(first.digest, changed.digest);
        let affected = BTreeSet::from(["rust-a".to_owned()]);
        let partial = fanout(&units, Some(&affected), &universe(), &selectors(), true).unwrap();
        assert_ne!(first.digest, partial.digest);
    }
}

//! Generic capability probes: one typed probe per spec §2 capability.
//!
//! Each probe requires exactly one capability and proves real routing (it
//! resolves to concrete selectors on eligible providers) and real execution
//! (it fans out into the frozen expected set with identical inputs on every
//! routed lane). A capability that routes nowhere fails explicitly instead of
//! silently passing unexecuted.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::BTreeMap;

use crate::s2::planner::{fanout, Execution, PlannedUnit};
use crate::s2::provider::{Capabilities, Platform, ProviderId, ProviderSet, SelectorMap, TrustReq};
use crate::s2::routing::{route_unit, RouteDecision, UnitRequirements};
use crate::s2::GeneratorError;

/// One generic capability probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum CapabilityProbe {
    Docker,
    NestedPrivilegedDocker,
    BuildxCompose,
    Testcontainers,
    ServicesWithReadiness,
    BrowserBinaries,
    NativeMacosArm64,
}

impl CapabilityProbe {
    /// All seven probes, in spec §2 order.
    pub(crate) const ALL: [Self; 7] = [
        Self::Docker,
        Self::NestedPrivilegedDocker,
        Self::BuildxCompose,
        Self::Testcontainers,
        Self::ServicesWithReadiness,
        Self::BrowserBinaries,
        Self::NativeMacosArm64,
    ];

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::NestedPrivilegedDocker => "nested-privileged-docker",
            Self::BuildxCompose => "buildx-compose",
            Self::Testcontainers => "testcontainers",
            Self::ServicesWithReadiness => "services-with-readiness",
            Self::BrowserBinaries => "browser-binaries",
            Self::NativeMacosArm64 => "native-macos-arm64",
        }
    }

    /// The single capability this probe requires.
    #[must_use]
    pub(crate) fn required(self) -> Capabilities {
        match self {
            Self::Docker => Capabilities {
                docker: true,
                ..Capabilities::default()
            },
            Self::NestedPrivilegedDocker => Capabilities {
                nested_privileged_docker: true,
                ..Capabilities::default()
            },
            Self::BuildxCompose => Capabilities {
                buildx_compose: true,
                ..Capabilities::default()
            },
            Self::Testcontainers => Capabilities {
                testcontainers: true,
                ..Capabilities::default()
            },
            Self::ServicesWithReadiness => Capabilities {
                services_with_readiness: true,
                ..Capabilities::default()
            },
            Self::BrowserBinaries => Capabilities {
                browser_binaries: true,
                ..Capabilities::default()
            },
            Self::NativeMacosArm64 => Capabilities {
                native_macos_arm64: true,
                ..Capabilities::default()
            },
        }
    }

    /// The platform the probe runs on: macOS for the native-macOS probe, Linux
    /// otherwise.
    #[must_use]
    pub(crate) fn platform(self) -> Platform {
        match self {
            Self::NativeMacosArm64 => Platform::MacosArm64,
            _ => Platform::LinuxX64,
        }
    }

    /// The lanes this probe executes on, declared explicitly. The
    /// `declared_lanes_match_the_matrix` test proves the declaration equals
    /// the static provider matrix, so matrix drift fails loudly instead of
    /// silently narrowing or widening the probe.
    #[must_use]
    pub(crate) fn lanes(self) -> ProviderSet {
        match self {
            Self::NestedPrivilegedDocker => {
                ProviderSet::from([ProviderId::GithubSelfHosted, ProviderId::Velnor])
            }
            Self::NativeMacosArm64 => ProviderSet::from([ProviderId::GithubHosted]),
            _ => ProviderSet::from([
                ProviderId::GithubHosted,
                ProviderId::GithubSelfHosted,
                ProviderId::Velnor,
            ]),
        }
    }

    /// The probe's unit id: `capability-<name>`.
    #[must_use]
    pub(crate) fn unit_id(self) -> String {
        format!("capability-{}", self.as_str())
    }

    /// The probe as a planned unit, so emission renders it like any unit.
    #[must_use]
    pub(crate) fn planned_unit(self) -> PlannedUnit {
        PlannedUnit {
            unit_id: self.unit_id(),
            label: "Capability".to_owned(),
            platform: self.platform(),
            trust: TrustReq::UntrustedOk,
            capabilities: self.required(),
            command_digest: format!("probe-{}", self.as_str()),
        }
    }

    fn requirements(self) -> UnitRequirements {
        UnitRequirements {
            unit_id: self.unit_id(),
            platform: self.platform(),
            trust: TrustReq::UntrustedOk,
            capabilities: self.required(),
        }
    }
}

/// Where one probe routes: provider → concrete labels, plus the recorded
/// reason every other provider in the universe did not route (exclusion
/// reason or explicit capability failure). Empty lanes mean the probe cannot
/// execute anywhere, which is a hard error, never a silent pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbeRouting {
    pub(crate) probe: CapabilityProbe,
    pub(crate) lanes: BTreeMap<ProviderId, Vec<String>>,
    pub(crate) outside: BTreeMap<ProviderId, String>,
}

/// Verify one probe's routing on a trusted event, provider by provider: every
/// declared lane in the universe must route to a concrete selector, and every
/// other provider must fail explicitly (capability hard error) or exclude
/// explicitly (platform/trust) — never route outside the declared lanes and
/// never disappear silently.
///
/// # Errors
/// Returns a usage error when a declared lane does not route, an undeclared
/// lane routes, or the probe routes nowhere.
pub(crate) fn verify_probe_routing(
    probe: CapabilityProbe,
    universe: &ProviderSet,
    selectors: &SelectorMap,
) -> Result<ProbeRouting, GeneratorError> {
    let mut lanes: BTreeMap<ProviderId, Vec<String>> = BTreeMap::new();
    let mut outside: BTreeMap<ProviderId, String> = BTreeMap::new();
    for provider in universe {
        let single = ProviderSet::from([*provider]);
        match route_unit(&probe.requirements(), &single, selectors, true) {
            Ok(decisions) => match decisions.get(provider) {
                Some(RouteDecision::Route { runs_on }) => {
                    if !probe.lanes().contains(provider) {
                        return Err(GeneratorError::usage(format!(
                            "capability probe `{}` routed outside its declared lanes on provider `{provider}`",
                            probe.as_str()
                        )));
                    }
                    if runs_on.is_empty() {
                        return Err(GeneratorError::usage(format!(
                            "capability probe `{}` routed to provider `{provider}` without labels",
                            probe.as_str()
                        )));
                    }
                    lanes.insert(*provider, runs_on.clone());
                }
                Some(RouteDecision::Exclude { reason }) => {
                    outside.insert(*provider, format!("excluded: {}", reason.as_str()));
                }
                None => {
                    return Err(GeneratorError::usage(format!(
                        "router returned no decision for provider `{provider}`"
                    )));
                }
            },
            Err(error) => {
                if probe.lanes().contains(provider) {
                    return Err(error);
                }
                outside.insert(*provider, format!("unsupported: {error}"));
            }
        }
    }
    for provider in probe.lanes().intersection(universe) {
        if !lanes.contains_key(provider) {
            return Err(GeneratorError::usage(format!(
                "capability probe `{}` did not route on declared lane `{provider}`",
                probe.as_str()
            )));
        }
    }
    if lanes.is_empty() {
        return Err(GeneratorError::usage(format!(
            "capability probe `{}` routes to no provider; the capability is untestable in this universe",
            probe.as_str()
        )));
    }
    Ok(ProbeRouting {
        probe,
        lanes,
        outside,
    })
}

/// Verify every probe's routing.
///
/// # Errors
/// Propagates the first [`verify_probe_routing`] failure.
pub(crate) fn verify_all_probes(
    universe: &ProviderSet,
    selectors: &SelectorMap,
) -> Result<Vec<ProbeRouting>, GeneratorError> {
    CapabilityProbe::ALL
        .iter()
        .map(|probe| verify_probe_routing(*probe, universe, selectors))
        .collect()
}

/// Fan every probe out as a planned unit over its declared lanes: the probes
/// become members of the frozen expected set, so each capability is really
/// executed, not just routable.
///
/// # Errors
/// Returns a usage error when a probe's declared lanes miss the universe
/// entirely, and propagates the planner's explicit failures otherwise.
pub(crate) fn fanout_probes(
    universe: &ProviderSet,
    selectors: &SelectorMap,
) -> Result<Vec<Execution>, GeneratorError> {
    let mut executions = Vec::new();
    for probe in CapabilityProbe::ALL {
        let lanes: ProviderSet = probe.lanes().intersection(universe).copied().collect();
        if lanes.is_empty() {
            return Err(GeneratorError::usage(format!(
                "capability probe `{}` has no declared lane in this universe; the capability is untestable here",
                probe.as_str()
            )));
        }
        executions
            .extend(fanout(&[probe.planned_unit()], None, &lanes, selectors, true)?.executions);
    }
    Ok(executions)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

    use super::*;

    fn selectors() -> SelectorMap {
        use crate::s2::provider::ProviderSelector;
        SelectorMap::from([
            (
                ProviderId::GithubHosted,
                ProviderSelector {
                    runs_on: vec!["ubuntu-24.04".to_owned(), "macos-15".to_owned()],
                    arm64_runs_on: Vec::new(),
                },
            ),
            (
                ProviderId::GithubSelfHosted,
                ProviderSelector {
                    runs_on: vec!["velnor-official".to_owned()],
                    arm64_runs_on: Vec::new(),
                },
            ),
            (
                ProviderId::Velnor,
                ProviderSelector {
                    runs_on: vec!["velnor-native".to_owned()],
                    arm64_runs_on: Vec::new(),
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

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn every_probe_routes_to_at_least_one_lane_with_labels() {
        for routing in verify_all_probes(&universe(), &selectors()).unwrap() {
            assert!(
                !routing.lanes.is_empty(),
                "probe {} routed nowhere",
                routing.probe.as_str()
            );
            for (provider, labels) in &routing.lanes {
                assert!(
                    !labels.is_empty(),
                    "probe {} has no labels on {provider}",
                    routing.probe.as_str()
                );
            }
        }
    }

    #[test]
    fn nested_privileged_docker_routes_to_local_lanes_only() {
        let routing = verify_probe_routing(
            CapabilityProbe::NestedPrivilegedDocker,
            &universe(),
            &selectors(),
        )
        .unwrap();
        assert!(!routing.lanes.contains_key(&ProviderId::GithubHosted));
        assert!(routing.lanes.contains_key(&ProviderId::GithubSelfHosted));
        assert!(routing.lanes.contains_key(&ProviderId::Velnor));
    }

    #[test]
    fn native_macos_routes_to_hosted_only() {
        let routing =
            verify_probe_routing(CapabilityProbe::NativeMacosArm64, &universe(), &selectors())
                .unwrap();
        assert_eq!(routing.lanes.len(), 1);
        assert!(routing.lanes.contains_key(&ProviderId::GithubHosted));
    }

    #[test]
    fn each_probe_requires_exactly_one_capability() {
        for probe in CapabilityProbe::ALL {
            let required = probe.required();
            let set = [
                required.docker,
                required.nested_privileged_docker,
                required.buildx_compose,
                required.testcontainers,
                required.services_with_readiness,
                required.browser_binaries,
                required.native_macos_arm64,
            ];
            assert_eq!(
                set.iter().filter(|flag| **flag).count(),
                1,
                "probe {} must isolate one capability",
                probe.as_str()
            );
        }
    }

    #[test]
    fn declared_lanes_match_the_matrix() {
        use crate::s2::provider::{eligibility, provider_caps};
        for probe in CapabilityProbe::ALL {
            for provider in ProviderId::ALL {
                let eligible =
                    eligibility(probe.platform(), TrustReq::UntrustedOk, provider, true).is_ok();
                let supported = probe
                    .required()
                    .missing_in(provider_caps(provider).caps)
                    .is_empty();
                assert_eq!(
                    probe.lanes().contains(&provider),
                    eligible && supported,
                    "probe {} drifted from the matrix on {provider}",
                    probe.as_str()
                );
            }
        }
    }

    #[test]
    fn outside_providers_carry_explicit_reasons() {
        let routing = verify_probe_routing(
            CapabilityProbe::NestedPrivilegedDocker,
            &universe(),
            &selectors(),
        )
        .unwrap();
        let reason = routing.outside.get(&ProviderId::GithubHosted).unwrap();
        assert!(
            reason.contains("nested-privileged-docker"),
            "hosted needs an explicit capability reason: {reason}"
        );
        let routing =
            verify_probe_routing(CapabilityProbe::NativeMacosArm64, &universe(), &selectors())
                .unwrap();
        for provider in ProviderId::LOCAL {
            let reason = routing.outside.get(&provider).unwrap();
            assert!(
                reason.contains("platform"),
                "{provider} needs an explicit platform reason: {reason}"
            );
        }
    }

    #[test]
    fn unroutable_capability_fails_explicitly() {
        let hosted_only = ProviderSet::from([ProviderId::GithubHosted]);
        let error = must_fail(
            verify_probe_routing(
                CapabilityProbe::NestedPrivilegedDocker,
                &hosted_only,
                &selectors(),
            ),
            "nested privileged docker on hosted-only universe",
        );
        assert!(
            error.contains("nested-privileged-docker"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn probes_fan_out_into_executions_with_identical_inputs() {
        let executions = fanout_probes(&universe(), &selectors()).unwrap();
        for probe in CapabilityProbe::ALL {
            let lanes: Vec<&Execution> = executions
                .iter()
                .filter(|execution| execution.unit_id == probe.unit_id())
                .collect();
            assert!(!lanes.is_empty(), "probe {} never executes", probe.as_str());
            for lane in &lanes {
                assert_eq!(
                    lane.command_digest,
                    format!("probe-{}", probe.as_str()),
                    "probe {} diverged across lanes",
                    probe.as_str()
                );
                assert!(
                    lane.display_name.contains(lane.provider.as_str()),
                    "probe lane hides its provider: {}",
                    lane.display_name
                );
            }
        }
    }
}

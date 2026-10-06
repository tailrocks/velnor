//! Unit-to-provider routing: the composition of eligibility and capability checks.
//!
//! [`route_unit`] is the single routing decision point for one
//! (unit, provider) pair: platform/trust eligibility first (a planner-declared
//! exclusion on mismatch), then the capability gate (a hard error naming
//! unit + provider + capability), then selector lookup (a hard error naming
//! the provider). Routing never falls back to another provider: an unroutable
//! pair fails or is excluded explicitly, it is never silently hosted.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::BTreeMap;

use crate::s2::provider::{
    check_capabilities, eligibility, runs_on_for, Capabilities, ExclusionReason, Platform,
    ProviderId, ProviderSet, SelectorMap, TrustReq,
};
use crate::s2::GeneratorError;

/// One unit's routing inputs: platform, trust, and capabilities as separate
/// typed fields. There are no CPU/RAM resource classes anywhere in this shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnitRequirements {
    pub(crate) unit_id: String,
    pub(crate) platform: Platform,
    pub(crate) trust: TrustReq,
    pub(crate) capabilities: Capabilities,
}

/// The routing decision for one (unit, provider) pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteDecision {
    /// The pair executes: these are the concrete `runs-on` labels.
    Route { runs_on: Vec<String> },
    /// The pair is out of scope with a planner-declared reason.
    Exclude { reason: ExclusionReason },
}

/// Route one unit across a provider universe on one event.
///
/// `event_trusted` is the controller-side verdict for the event (see
/// [`crate::s2::trust`]); a requested label or same-repo origin is never
/// passed here. Every provider in `universe` gets an explicit decision: there
/// is no silent hosted default and no silent drop.
///
/// # Errors
/// Returns a usage error for an unsupported capability or a missing selector,
/// naming the unit and the provider.
pub(crate) fn route_unit(
    requirements: &UnitRequirements,
    universe: &ProviderSet,
    selectors: &SelectorMap,
    event_trusted: bool,
) -> Result<BTreeMap<ProviderId, RouteDecision>, GeneratorError> {
    let mut decisions = BTreeMap::new();
    for provider in universe {
        if let Err(reason) = eligibility(
            requirements.platform,
            requirements.trust,
            *provider,
            event_trusted,
        ) {
            decisions.insert(*provider, RouteDecision::Exclude { reason });
            continue;
        }
        check_capabilities(&requirements.unit_id, requirements.capabilities, *provider)?;
        let runs_on = runs_on_for(selectors, *provider)?.to_vec();
        decisions.insert(*provider, RouteDecision::Route { runs_on });
    }
    Ok(decisions)
}

/// Route every unit; the first hard error aborts the table so a partial
/// routing can never render.
///
/// # Errors
/// Propagates the first [`route_unit`] failure.
pub(crate) fn route_all(
    units: &[UnitRequirements],
    universe: &ProviderSet,
    selectors: &SelectorMap,
    event_trusted: bool,
) -> Result<BTreeMap<(String, ProviderId), RouteDecision>, GeneratorError> {
    let mut table = BTreeMap::new();
    for unit in units {
        for (provider, decision) in route_unit(unit, universe, selectors, event_trusted)? {
            table.insert((unit.unit_id.clone(), provider), decision);
        }
    }
    Ok(table)
}

/// Names of the seven spec §2 capabilities, in struct order. The capability
/// vocabulary is exactly this list: any other name is a hard error at the
/// config gate (`deny_unknown_fields`), never a resource class.
#[must_use]
pub(crate) fn capability_names() -> &'static [&'static str] {
    &[
        "docker",
        "nested_privileged_docker",
        "buildx_compose",
        "testcontainers",
        "services_with_readiness",
        "browser_binaries",
        "native_macos_arm64",
    ]
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

    fn plain_unit() -> UnitRequirements {
        UnitRequirements {
            unit_id: "rust-widget".to_owned(),
            platform: Platform::LinuxX64,
            trust: TrustReq::UntrustedOk,
            capabilities: Capabilities::default(),
        }
    }

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn plain_linux_unit_routes_to_all_three_with_concrete_labels() {
        let decisions = route_unit(&plain_unit(), &universe(), &selectors(), true).unwrap();
        assert_eq!(decisions.len(), 3);
        for provider in ProviderId::ALL {
            let decision = decisions.get(&provider).unwrap();
            match decision {
                RouteDecision::Route { runs_on } => {
                    assert!(!runs_on.is_empty(), "{provider} routed without labels");
                }
                RouteDecision::Exclude { reason } => {
                    panic!("{provider} unexpectedly excluded: {}", reason.as_str());
                }
            }
        }
    }

    #[test]
    fn missing_selector_fails_explicitly_and_never_falls_back_to_hosted() {
        let mut partial = selectors();
        partial.remove(&ProviderId::Velnor);
        let error = must_fail(
            route_unit(&plain_unit(), &universe(), &partial, true),
            "missing velnor selector",
        );
        assert!(
            error.contains("[workflow.selectors.velnor]"),
            "unexpected error: {error}"
        );
        // The failure aborts routing: no decision table exists that could
        // silently run the velnor lane on hosted labels.
    }

    #[test]
    fn unknown_selector_key_fails_at_parse_never_routes() {
        let tables = BTreeMap::from([(
            "github".to_owned(),
            ProviderSelector {
                runs_on: vec!["ubuntu-24.04".to_owned()],
            },
        )]);
        let error = must_fail(
            crate::s2::provider::parse_selectors(&tables),
            "unknown selector key",
        );
        assert!(
            error.contains("unknown provider `github`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unsupported_capability_fails_explicitly_per_provider() {
        let unit = UnitRequirements {
            capabilities: Capabilities {
                nested_privileged_docker: true,
                ..Capabilities::default()
            },
            ..plain_unit()
        };
        let error = must_fail(
            route_unit(&unit, &universe(), &selectors(), true),
            "nested privileged docker on hosted",
        );
        assert!(
            error.contains("rust-widget")
                && error.contains("github-hosted")
                && error.contains("nested-privileged-docker"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn untrusted_event_excludes_local_lanes_with_trust_reason() {
        let decisions = route_unit(&plain_unit(), &universe(), &selectors(), false).unwrap();
        assert!(matches!(
            decisions.get(&ProviderId::GithubHosted),
            Some(RouteDecision::Route { .. })
        ));
        for provider in ProviderId::LOCAL {
            assert_eq!(
                decisions.get(&provider),
                Some(&RouteDecision::Exclude {
                    reason: ExclusionReason::Trust
                }),
                "{provider} must be trust-excluded on an untrusted event"
            );
        }
    }

    #[test]
    fn capability_vocabulary_is_exactly_the_seven_spec_fields() {
        // `deny_unknown_fields` plus this key-set assertion: no resource
        // classes can hide in the capability shape.
        let value = serde_json::to_value(Capabilities::default()).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = capability_names().to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        for key in capability_names() {
            assert_eq!(
                object.get(*key),
                Some(&serde_json::Value::Bool(false)),
                "capability `{key}` must default to false"
            );
        }
        let rejected = serde_json::json!({"docker": true, "cpu_class": "xlarge"});
        assert!(
            serde_json::from_value::<Capabilities>(rejected).is_err(),
            "resource-class field must not deserialize into Capabilities"
        );
    }
}

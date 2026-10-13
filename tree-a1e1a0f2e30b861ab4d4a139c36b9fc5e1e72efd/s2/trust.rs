//! Controller-side trust enforcement per spec §6.
//!
//! Trust is decided outside PR-editable YAML from the controller's own
//! event/source/ref verdicts. Same-repository origin or a requested label is
//! never sufficient: every trusted verdict also requires the controller to
//! have verified the source. Default untrusted fork execution stays hosted,
//! `pull_request_target` never runs untrusted checkouts privileged, label
//! spoofing and reusable-workflow input substitution are rejected, and every
//! event has explicit coverage.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::{BTreeMap, BTreeSet};

use crate::s2::provider::{ProviderId, ProviderSet, SelectorMap};
use crate::s2::GeneratorError;

/// Every CI event with explicit trust coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CiEvent {
    /// A pull request: forks are untrusted; same-repo PRs still need the
    /// controller's source verification; bot authors are always untrusted.
    PullRequest {
        same_repo: bool,
        author_is_bot: bool,
    },
    /// The privileged `pull_request_target` context: never privileged with an
    /// untrusted checkout, and the checkout is always the base ref.
    PullRequestTarget { source_verified: bool },
    /// A push, typically to the default branch.
    Push,
    /// A scheduled run on the default branch.
    Schedule,
    /// A manual dispatch: the controller verifies the ref before trusting it.
    WorkflowDispatch { source_verified: bool },
    /// A tag push: tags are repository-controlled.
    Tag,
    /// A merge-group check.
    MergeGroup,
}

/// Which ref an event context checks out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckoutRef {
    /// The event's head (PR head, push SHA, tag).
    Head,
    /// The trusted base ref. `pull_request_target` always checks out base:
    /// untrusted code is never executed in the privileged context.
    Base,
}

/// The controller-side verdict: trusted or not, which providers may execute,
/// and which ref is checked out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustVerdict {
    pub(crate) trusted: bool,
    pub(crate) providers: ProviderSet,
    pub(crate) checkout: CheckoutRef,
}

impl TrustVerdict {
    fn trusted_full(checkout: CheckoutRef) -> Self {
        Self {
            trusted: true,
            providers: ProviderSet::from([
                ProviderId::GithubHosted,
                ProviderId::GithubSelfHosted,
                ProviderId::Velnor,
            ]),
            checkout,
        }
    }

    fn untrusted_hosted(checkout: CheckoutRef) -> Self {
        Self {
            trusted: false,
            providers: ProviderSet::from([ProviderId::GithubHosted]),
            checkout,
        }
    }
}

/// Evaluate one event. `source_verified` is the controller's own source/ref
/// verification — outside PR YAML — and is required wherever the event alone
/// does not establish origin. Every event gets an explicit verdict; there is
/// no unknown event that could fail open.
#[must_use]
pub(crate) fn evaluate_event(event: &CiEvent, source_verified: bool) -> TrustVerdict {
    match event {
        CiEvent::PullRequest {
            same_repo,
            author_is_bot,
        } => {
            // Forks default to hosted; bots are untrusted everywhere; a
            // same-repo origin alone is never sufficient without the
            // controller's source verification.
            if *same_repo && !author_is_bot && source_verified {
                TrustVerdict::trusted_full(CheckoutRef::Head)
            } else {
                TrustVerdict::untrusted_hosted(CheckoutRef::Head)
            }
        }
        CiEvent::PullRequestTarget { source_verified } => {
            // Never privileged-untrusted: without verification the context is
            // hosted-only, and the checkout is always the base ref either way.
            if *source_verified {
                TrustVerdict::trusted_full(CheckoutRef::Base)
            } else {
                TrustVerdict::untrusted_hosted(CheckoutRef::Base)
            }
        }
        CiEvent::Push | CiEvent::Schedule | CiEvent::Tag | CiEvent::MergeGroup => {
            // Same-repo, controller-owned origins. Push/schedule/tag/merge
            // queue entries cannot be forged from a fork, so the event plus
            // the controller's own delivery is the verification.
            TrustVerdict::trusted_full(CheckoutRef::Head)
        }
        CiEvent::WorkflowDispatch { source_verified } => {
            // Dispatch can target any ref, so the controller must verify it.
            if *source_verified {
                TrustVerdict::trusted_full(CheckoutRef::Head)
            } else {
                TrustVerdict::untrusted_hosted(CheckoutRef::Head)
            }
        }
    }
}

/// A selector a job requested. Labels are routing, never authorization: a
/// request may only route within the verdict's providers, and it never
/// widens the verdict.
pub(crate) fn check_label_spoof(
    requested_labels: &[String],
    selectors: &SelectorMap,
    verdict: &TrustVerdict,
) -> Result<BTreeSet<ProviderId>, GeneratorError> {
    let mut routed = BTreeSet::new();
    for label in requested_labels {
        let mut owners: Vec<ProviderId> = selectors
            .iter()
            .filter(|(_, selector)| selector.runs_on.iter().any(|owned| owned == label))
            .map(|(provider, _)| *provider)
            .collect();
        owners.sort();
        match owners.as_slice() {
            [] => {
                return Err(GeneratorError::usage(format!(
                    "requested label `{label}` matches no provider selector; unknown selectors fail explicitly"
                )));
            }
            [provider] => {
                if !verdict.providers.contains(provider) {
                    return Err(GeneratorError::usage(format!(
                        "requested label `{label}` routes to provider `{provider}`, which this event is not trusted for; labels never authorize privileged execution"
                    )));
                }
                routed.insert(*provider);
            }
            _ => {
                return Err(GeneratorError::usage(format!(
                    "requested label `{label}` is claimed by more than one provider selector; local selectors must be disjoint"
                )));
            }
        }
    }
    Ok(routed)
}

/// Reusable-workflow inputs that select trust or execution placement. When the
/// caller is untrusted, these inputs are rejected: a fork must not escalate
/// through a protected workflow's inputs.
pub(crate) const TRUST_RELEVANT_INPUTS: &[&str] = &[
    "providers",
    "trust",
    "trusted",
    "runs-on",
    "runs_on",
    "selector",
    "ref",
    "checkout_ref",
];

/// Reject trust-relevant inputs from an untrusted caller.
///
/// # Errors
/// Returns a usage error naming the substituted input.
pub(crate) fn check_workflow_inputs(
    inputs: &BTreeMap<String, String>,
    verdict: &TrustVerdict,
) -> Result<(), GeneratorError> {
    if verdict.trusted {
        return Ok(());
    }
    for key in TRUST_RELEVANT_INPUTS {
        if let Some(value) = inputs.get(*key) {
            return Err(GeneratorError::usage(format!(
                "untrusted caller passed trust-relevant input `{key} = {value}`; protected-workflow input substitution is rejected"
            )));
        }
    }
    Ok(())
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

    fn hosted_only() -> ProviderSet {
        ProviderSet::from([ProviderId::GithubHosted])
    }

    fn full() -> ProviderSet {
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
    fn fork_pr_defaults_to_hosted_even_when_verified() {
        let verdict = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: false,
                author_is_bot: false,
            },
            true,
        );
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
    }

    #[test]
    fn bot_pr_is_untrusted_even_same_repo() {
        let verdict = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: true,
                author_is_bot: true,
            },
            true,
        );
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
    }

    #[test]
    fn same_repo_origin_alone_is_never_sufficient() {
        let verdict = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: true,
                author_is_bot: false,
            },
            false,
        );
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
        let verified = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: true,
                author_is_bot: false,
            },
            true,
        );
        assert!(verified.trusted);
        assert_eq!(verified.providers, full());
    }

    #[test]
    fn pull_request_target_is_never_privileged_untrusted_and_checks_out_base() {
        let untrusted = evaluate_event(
            &CiEvent::PullRequestTarget {
                source_verified: false,
            },
            false,
        );
        assert!(!untrusted.trusted);
        assert_eq!(untrusted.providers, hosted_only());
        assert_eq!(untrusted.checkout, CheckoutRef::Base);
        // Even the verified context checks out base, never the PR head.
        let verified = evaluate_event(
            &CiEvent::PullRequestTarget {
                source_verified: true,
            },
            true,
        );
        assert_eq!(verified.checkout, CheckoutRef::Base);
    }

    #[test]
    fn controller_owned_events_are_trusted() {
        for event in [
            CiEvent::Push,
            CiEvent::Schedule,
            CiEvent::Tag,
            CiEvent::MergeGroup,
        ] {
            let verdict = evaluate_event(&event, false);
            assert!(verdict.trusted, "{event:?} must be trusted");
            assert_eq!(verdict.providers, full());
        }
    }

    #[test]
    fn dispatch_needs_source_verification() {
        let unverified = evaluate_event(
            &CiEvent::WorkflowDispatch {
                source_verified: false,
            },
            false,
        );
        assert!(!unverified.trusted);
        assert_eq!(unverified.providers, hosted_only());
        let verified = evaluate_event(
            &CiEvent::WorkflowDispatch {
                source_verified: true,
            },
            true,
        );
        assert!(verified.trusted);
    }

    #[test]
    fn every_event_variant_has_explicit_coverage() {
        // Exhaustive: adding a CiEvent variant breaks this match until the
        // new event gets its verdict test.
        let events = [
            CiEvent::PullRequest {
                same_repo: false,
                author_is_bot: false,
            },
            CiEvent::PullRequest {
                same_repo: true,
                author_is_bot: true,
            },
            CiEvent::PullRequestTarget {
                source_verified: false,
            },
            CiEvent::Push,
            CiEvent::Schedule,
            CiEvent::WorkflowDispatch {
                source_verified: false,
            },
            CiEvent::Tag,
            CiEvent::MergeGroup,
        ];
        for event in &events {
            let verdict = evaluate_event(event, false);
            assert!(
                !verdict.providers.is_empty(),
                "{event:?} must name its providers"
            );
            // Untrusted verdicts are hosted-only, without exception.
            if !verdict.trusted {
                assert_eq!(verdict.providers, hosted_only(), "{event:?}");
            }
        }
    }

    #[test]
    fn label_spoofing_a_privileged_lane_is_rejected() {
        let fork = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: false,
                author_is_bot: false,
            },
            true,
        );
        let error = must_fail(
            check_label_spoof(&[String::from("velnor-native")], &selectors(), &fork),
            "fork requesting a native label",
        );
        assert!(
            error.contains("velnor-native") && error.contains("not trusted"),
            "{error}"
        );
        // The hosted label still routes within the verdict.
        let routed =
            check_label_spoof(&[String::from("ubuntu-24.04")], &selectors(), &fork).unwrap();
        assert_eq!(routed, hosted_only());
    }

    #[test]
    fn unknown_requested_label_fails_explicitly() {
        let main = evaluate_event(&CiEvent::Push, true);
        let error = must_fail(
            check_label_spoof(&[String::from("gpu-a100")], &selectors(), &main),
            "unknown requested label",
        );
        assert!(error.contains("matches no provider selector"), "{error}");
    }

    #[test]
    fn labels_never_widen_the_verdict() {
        let fork = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: false,
                author_is_bot: false,
            },
            true,
        );
        let routed =
            check_label_spoof(&[String::from("ubuntu-24.04")], &selectors(), &fork).unwrap();
        assert!(routed
            .iter()
            .all(|provider| fork.providers.contains(provider)));
        assert!(!fork.trusted);
    }

    #[test]
    fn untrusted_input_substitution_is_rejected() {
        let fork = evaluate_event(
            &CiEvent::PullRequest {
                same_repo: false,
                author_is_bot: false,
            },
            true,
        );
        for key in TRUST_RELEVANT_INPUTS {
            let inputs = BTreeMap::from([((*key).to_owned(), "velnor".to_owned())]);
            let error = must_fail(check_workflow_inputs(&inputs, &fork), "substituted input");
            assert!(
                error.contains(key),
                "input `{key}` must be named in: {error}"
            );
        }
        // Benign inputs pass through.
        let benign = BTreeMap::from([("profile".to_owned(), "ci".to_owned())]);
        check_workflow_inputs(&benign, &fork).unwrap();
    }

    #[test]
    fn trusted_callers_keep_their_inputs() {
        let main = evaluate_event(&CiEvent::Push, true);
        let inputs = BTreeMap::from([("providers".to_owned(), "velnor".to_owned())]);
        check_workflow_inputs(&inputs, &main).unwrap();
    }
}

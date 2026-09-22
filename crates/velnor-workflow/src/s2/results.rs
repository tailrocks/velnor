//! Strict expected-result set, fixed before execution.
//!
//! The planner freezes the expected (unit, provider) pairs; the hosted
//! verifier then demands exactly one `success` record with matching identity
//! per pair. Missing, skipped, cancelled, timed-out, failed,
//! duplicate-conflicting, identity-mismatched, stale-attempt, and
//! wrong-provider records all fail. Exclusions excuse nothing unless the
//! planner declared them before expansion, and qualification matrices never
//! fail fast.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::BTreeSet;

use crate::s2::planner::{Exclusion, Execution};
use crate::s2::provider::{
    evaluate_verdict, ObservedResult, ProviderId, RunIdentity, VerdictFailure,
};
use crate::s2::GeneratorError;

/// The frozen expected set: members plus the planner-declared exclusions plus
/// the digest that binds both. Frozen before execution; nothing added later
/// can excuse a member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedSet {
    members: BTreeSet<(String, ProviderId)>,
    exclusions: Vec<Exclusion>,
    digest: String,
}

impl ExpectedSet {
    /// Freeze the planner output. The digest covers members and exclusions so
    /// a post-hoc exclusion cannot masquerade as a planner declaration.
    #[must_use]
    pub(crate) fn freeze(executions: &[Execution], exclusions: Vec<Exclusion>) -> Self {
        let members: BTreeSet<(String, ProviderId)> = executions
            .iter()
            .map(|execution| (execution.unit_id.clone(), execution.provider))
            .collect();
        let mut digest_input = String::new();
        for (unit_id, provider) in &members {
            use std::fmt::Write as _;
            let _ = writeln!(digest_input, "expected:{unit_id}:{provider}");
        }
        let mut exclusions = exclusions;
        exclusions.sort_by(|left, right| {
            (&left.unit_id, left.provider).cmp(&(&right.unit_id, right.provider))
        });
        for exclusion in &exclusions {
            use std::fmt::Write as _;
            let _ = writeln!(
                digest_input,
                "excluded:{}:{}:{}",
                exclusion.unit_id,
                exclusion.provider,
                exclusion.reason.as_str()
            );
        }
        let digest = crate::s2::content_digest_bytes(digest_input.as_bytes());
        Self {
            members,
            exclusions,
            digest: format!("{digest:016x}"),
        }
    }

    #[must_use]
    pub(crate) fn members(&self) -> &BTreeSet<(String, ProviderId)> {
        &self.members
    }

    #[must_use]
    pub(crate) fn exclusions(&self) -> &[Exclusion] {
        &self.exclusions
    }

    #[must_use]
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// The strict verdict: every member must be exactly one matching success.
    #[must_use]
    pub(crate) fn verdict(
        &self,
        observed: &[ObservedResult],
        run: &RunIdentity,
    ) -> Vec<VerdictFailure> {
        evaluate_verdict(&self.members, observed, run)
    }

    /// Whether the verdict passes: no failures at all.
    #[must_use]
    pub(crate) fn passes(&self, observed: &[ObservedResult], run: &RunIdentity) -> bool {
        self.verdict(observed, run).is_empty()
    }
}

/// Matrix policy for qualification: fail-fast is always disabled so every
/// provider's execution stands on its own evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MatrixPolicy {
    fail_fast: bool,
}

impl MatrixPolicy {
    /// The only qualification policy: fail-fast disabled.
    #[must_use]
    pub(crate) fn qualification() -> Self {
        Self { fail_fast: false }
    }

    /// Reject any policy that fails fast: a hosted green must never hide a
    /// local lane that never ran.
    ///
    /// # Errors
    /// Returns a usage error when `fail_fast` is enabled.
    pub(crate) fn require_qualification(policy: Self) -> Result<(), GeneratorError> {
        if policy.fail_fast {
            return Err(GeneratorError::usage(
                "qualification matrices must set fail-fast: false; a fast failure hides the lanes that never ran",
            ));
        }
        Ok(())
    }
}

/// An exclusion claimed after the freeze: never planner-declared, never
/// excusing. The verdict input is the frozen member set only, so late claims
/// like this one cannot reach it; this check fails loudly if one is offered.
pub(crate) fn reject_late_exclusion(
    frozen: &ExpectedSet,
    claimed: &Exclusion,
) -> Result<(), GeneratorError> {
    if frozen.exclusions.contains(claimed) {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "unit `{}` on provider `{}` was not excluded by the planner; post-hoc exclusions never excuse an expected result",
        claimed.unit_id, claimed.provider
    )))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

    use std::collections::BTreeMap;

    use super::*;
    use crate::s2::planner::{fanout, BootstrapLane};
    use crate::s2::provider::ProviderSelector;
    use crate::s2::provider::{
        Capabilities, ExclusionReason, ObservedOutcome, Platform, ProviderSet, ResultIdentity,
        SelectorMap, TrustReq, UnitIdentity,
    };

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

    fn planned(id: &str) -> crate::s2::planner::PlannedUnit {
        crate::s2::planner::PlannedUnit {
            unit_id: id.to_owned(),
            label: "Rust".to_owned(),
            platform: Platform::LinuxX64,
            trust: TrustReq::UntrustedOk,
            capabilities: Capabilities::default(),
            command_digest: format!("digest-of-{id}"),
        }
    }

    fn frozen_single() -> (ExpectedSet, RunIdentity) {
        let plan = fanout(&[planned("rust-a")], None, &universe(), &selectors(), true).unwrap();
        let plan_digest = plan.digest.clone();
        let unit_identities = plan
            .executions
            .iter()
            .map(|execution| {
                (
                    execution.unit_id.clone(),
                    UnitIdentity {
                        platform: execution.platform,
                        trust: execution.trust,
                        command_digest: execution.command_digest.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let frozen = ExpectedSet::freeze(&plan.executions, plan.exclusions);
        let run = RunIdentity {
            repository_id: "123".to_owned(),
            source_sha: "abc".to_owned(),
            audited_sha: "head".to_owned(),
            base_sha: "base".to_owned(),
            run_id: "42".to_owned(),
            run_attempt: "1".to_owned(),
            plan_digest,
            unit_identities,
        };
        (frozen, run)
    }

    fn identity(run: &RunIdentity, unit: &str, provider: ProviderId) -> ResultIdentity {
        let unit_identity = run
            .unit_identities
            .get(unit)
            .expect("test unit identity is frozen");
        ResultIdentity {
            repository_id: run.repository_id.clone(),
            source_sha: run.source_sha.clone(),
            audited_sha: run.audited_sha.clone(),
            base_sha: run.base_sha.clone(),
            run_id: run.run_id.clone(),
            run_attempt: run.run_attempt.clone(),
            plan_digest: run.plan_digest.clone(),
            unit_id: unit.to_owned(),
            provider,
            platform: unit_identity.platform,
            trust: unit_identity.trust,
            command_digest: unit_identity.command_digest.clone(),
        }
    }

    fn success_for(run: &RunIdentity, provider: ProviderId) -> ObservedResult {
        ObservedResult {
            identity: identity(run, "rust-a", provider),
            outcome: ObservedOutcome::Success,
        }
    }

    fn all_green(run: &RunIdentity) -> Vec<ObservedResult> {
        ProviderId::ALL
            .iter()
            .map(|provider| success_for(run, *provider))
            .collect()
    }

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    fn failure_classes(failures: &[VerdictFailure]) -> Vec<&'static str> {
        failures.iter().map(VerdictFailure::class).collect()
    }

    #[test]
    fn all_green_passes() {
        let (frozen, run) = frozen_single();
        assert!(frozen.passes(&all_green(&run), &run));
    }

    #[test]
    fn missing_result_fails() {
        let (frozen, run) = frozen_single();
        let observed = vec![
            success_for(&run, ProviderId::GithubHosted),
            success_for(&run, ProviderId::GithubSelfHosted),
        ];
        let failures = frozen.verdict(&observed, &run);
        assert_eq!(failure_classes(&failures), vec!["missing"]);
        assert!(matches!(
            failures[0],
            VerdictFailure::Missing { provider, .. } if provider == ProviderId::Velnor
        ));
    }

    #[test]
    fn skipped_result_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[2].outcome = ObservedOutcome::Skipped;
        assert_eq!(
            failure_classes(&frozen.verdict(&observed, &run)),
            vec!["skipped"]
        );
    }

    #[test]
    fn cancelled_result_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[1].outcome = ObservedOutcome::Cancelled;
        assert_eq!(
            failure_classes(&frozen.verdict(&observed, &run)),
            vec!["cancelled"]
        );
    }

    #[test]
    fn timed_out_result_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[0].outcome = ObservedOutcome::TimedOut;
        assert_eq!(
            failure_classes(&frozen.verdict(&observed, &run)),
            vec!["timed-out"]
        );
    }

    #[test]
    fn failed_result_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[0].outcome = ObservedOutcome::Failed;
        assert_eq!(
            failure_classes(&frozen.verdict(&observed, &run)),
            vec!["failed"]
        );
    }

    #[test]
    fn duplicate_conflicting_records_fail() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed.push(ObservedResult {
            identity: identity(&run, "rust-a", ProviderId::Velnor),
            outcome: ObservedOutcome::Failed,
        });
        assert_eq!(
            failure_classes(&frozen.verdict(&observed, &run)),
            vec!["duplicate-conflicting"]
        );
    }

    #[test]
    fn identity_mismatch_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[2].identity.command_digest = "forged".to_owned();
        // The forged record mismatches identity; the genuine velnor record is
        // then missing too. Both fail.
        let classes = failure_classes(&frozen.verdict(&observed, &run));
        assert!(classes.contains(&"identity-mismatch"), "{classes:?}");
        assert!(classes.contains(&"missing"), "{classes:?}");
    }

    #[test]
    fn candidate_and_unit_identity_mismatch_fails_closed() {
        let (frozen, run) = frozen_single();

        let mut audited = all_green(&run);
        audited[2].identity.audited_sha = "other-candidate".to_owned();
        let failures = frozen.verdict(&audited, &run);
        assert!(
            failure_classes(&failures).contains(&"identity-mismatch"),
            "{failures:?}"
        );

        let mut based = all_green(&run);
        based[2].identity.base_sha = "other-base".to_owned();
        let failures = frozen.verdict(&based, &run);
        assert!(
            failure_classes(&failures).contains(&"identity-mismatch"),
            "{failures:?}"
        );

        let mut platform = all_green(&run);
        platform[2].identity.platform = Platform::LinuxArm64;
        let failures = frozen.verdict(&platform, &run);
        assert!(
            failure_classes(&failures).contains(&"identity-mismatch"),
            "{failures:?}"
        );

        let mut trust = all_green(&run);
        trust[2].identity.trust = TrustReq::TrustedOnly;
        let failures = frozen.verdict(&trust, &run);
        assert!(
            failure_classes(&failures).contains(&"identity-mismatch"),
            "{failures:?}"
        );
    }

    #[test]
    fn stale_attempt_fails() {
        let (frozen, run) = frozen_single();
        let mut observed = all_green(&run);
        observed[2].identity.run_attempt = "0".to_owned();
        let classes = failure_classes(&frozen.verdict(&observed, &run));
        assert!(classes.contains(&"stale-attempt"), "{classes:?}");
        assert!(classes.contains(&"missing"), "{classes:?}");
    }

    #[test]
    fn wrong_provider_report_fails() {
        let plan = fanout(
            &[planned("rust-a")],
            None,
            &ProviderSet::from([ProviderId::GithubHosted]),
            &selectors(),
            true,
        )
        .unwrap();
        let plan_digest = plan.digest.clone();
        let unit_identities = plan
            .executions
            .iter()
            .map(|execution| {
                (
                    execution.unit_id.clone(),
                    UnitIdentity {
                        platform: execution.platform,
                        trust: execution.trust,
                        command_digest: execution.command_digest.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let frozen = ExpectedSet::freeze(&plan.executions, plan.exclusions);
        let run = RunIdentity {
            repository_id: "123".to_owned(),
            source_sha: "abc".to_owned(),
            audited_sha: "head".to_owned(),
            base_sha: "base".to_owned(),
            run_id: "42".to_owned(),
            run_attempt: "1".to_owned(),
            plan_digest,
            unit_identities,
        };
        // A local lane claims the hosted-only unit: wrong provider, and the
        // expected hosted record is missing.
        let observed = vec![ObservedResult {
            identity: identity(&run, "rust-a", ProviderId::Velnor),
            outcome: ObservedOutcome::Success,
        }];
        let failures = frozen.verdict(&observed, &run);
        assert_eq!(
            failures[0],
            VerdictFailure::WrongProvider {
                unit_id: "rust-a".to_owned(),
                expected: ProviderId::GithubHosted,
                claimed: ProviderId::Velnor,
            }
        );
        assert!(matches!(failures[1], VerdictFailure::Missing { .. }));
    }

    #[test]
    fn failure_classes_cover_all_nine() {
        let classes = [
            VerdictFailure::Missing {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::Skipped {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::Cancelled {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::TimedOut {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::Failed {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::DuplicateConflicting {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
            },
            VerdictFailure::IdentityMismatch {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
                reason: String::new(),
            },
            VerdictFailure::StaleAttempt {
                unit_id: String::new(),
                provider: ProviderId::Velnor,
                attempt: String::new(),
            },
            VerdictFailure::WrongProvider {
                unit_id: "rust-a".to_owned(),
                expected: ProviderId::GithubHosted,
                claimed: ProviderId::Velnor,
            },
        ];
        assert_eq!(
            failure_classes(&classes),
            vec![
                "missing",
                "skipped",
                "cancelled",
                "timed-out",
                "failed",
                "duplicate-conflicting",
                "identity-mismatch",
                "stale-attempt",
                "wrong-provider",
            ]
        );
    }

    #[test]
    fn post_hoc_exclusions_never_excuse() {
        let (frozen, run) = frozen_single();
        let late = Exclusion {
            unit_id: "rust-a".to_owned(),
            provider: ProviderId::Velnor,
            reason: ExclusionReason::Platform,
        };
        let error = must_fail(reject_late_exclusion(&frozen, &late), "late exclusion");
        assert!(error.contains("not excluded by the planner"), "{error}");
        // And the verdict still fails on the missing member.
        let observed = vec![
            success_for(&run, ProviderId::GithubHosted),
            success_for(&run, ProviderId::GithubSelfHosted),
        ];
        assert!(!frozen.passes(&observed, &run));
    }

    #[test]
    fn planner_declared_exclusions_are_accepted() {
        let plan = fanout(&[planned("rust-a")], None, &universe(), &selectors(), false).unwrap();
        assert!(!plan.exclusions.is_empty());
        let frozen = ExpectedSet::freeze(&plan.executions, plan.exclusions.clone());
        for exclusion in &plan.exclusions {
            reject_late_exclusion(&frozen, exclusion).unwrap();
        }
        // The frozen set holds only the hosted execution on an untrusted event.
        assert_eq!(frozen.members().len(), 1);
    }

    #[test]
    fn qualification_rejects_fail_fast() {
        MatrixPolicy::require_qualification(MatrixPolicy::qualification()).unwrap();
        let error = must_fail(
            MatrixPolicy::require_qualification(MatrixPolicy { fail_fast: true }),
            "fail-fast qualification",
        );
        assert!(error.contains("fail-fast: false"), "{error}");
    }

    #[test]
    fn freeze_digest_covers_members_and_exclusions() {
        let (frozen, _) = frozen_single();
        let other = ExpectedSet::freeze(
            &[Execution {
                unit_id: "rust-a".to_owned(),
                provider: ProviderId::GithubHosted,
                job_id: "github-hosted-rust-a".to_owned(),
                display_name: "Rust".to_owned(),
                runs_on: vec!["ubuntu-24.04".to_owned()],
                platform: Platform::LinuxX64,
                trust: TrustReq::UntrustedOk,
                command_digest: "digest-of-rust-a".to_owned(),
                bootstrap: BootstrapLane::for_provider(ProviderId::GithubHosted, false),
            }],
            vec![Exclusion {
                unit_id: "rust-a".to_owned(),
                provider: ProviderId::Velnor,
                reason: ExclusionReason::Trust,
            }],
        );
        assert_ne!(frozen.digest(), other.digest());
    }
}

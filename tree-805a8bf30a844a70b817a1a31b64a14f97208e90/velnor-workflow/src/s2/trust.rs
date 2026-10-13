//! Controller-side trust enforcement per spec §6.
//!
//! Trust is decided outside PR-editable YAML from the controller's own
//! event/source/ref verdicts. Same-repository origin or a requested label is
//! never sufficient: every trusted verdict also requires the controller to
//! have verified the source. Default untrusted fork execution stays hosted,
//! `pull_request_target` never runs untrusted checkouts privileged, label
//! spoofing and reusable-workflow input substitution are rejected, and every
//! event has explicit coverage.

use std::collections::{BTreeMap, BTreeSet};
use std::env;

use crate::s2::provider::{ProviderId, ProviderSet, SelectorMap};
use crate::s2::GeneratorError;

/// Every CI event with explicit trust coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventKind {
    PullRequest,
    PullRequestTarget,
    Push,
    Schedule,
    MergeGroup,
    WorkflowDispatch,
    Tag,
    Local,
}

impl EventKind {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "pull_request" => Ok(Self::PullRequest),
            "pull_request_target" => Ok(Self::PullRequestTarget),
            "push" => Ok(Self::Push),
            "schedule" => Ok(Self::Schedule),
            "merge_group" => Ok(Self::MergeGroup),
            "workflow_dispatch" => Ok(Self::WorkflowDispatch),
            "tag" => Ok(Self::Tag),
            "" => Ok(Self::Local),
            other => Err(GeneratorError::usage(format!(
                "unsupported CI event `{other}`"
            ))),
        }
    }

    pub(crate) fn from_env() -> Result<Self, GeneratorError> {
        let explicit = env::var("EVENT_NAME")
            .ok()
            .filter(|value| !value.is_empty());
        let runner = env::var("GITHUB_EVENT_NAME")
            .ok()
            .filter(|value| !value.is_empty());
        let value = explicit
            .as_deref()
            .or(runner.as_deref())
            .unwrap_or_default();
        if value.is_empty()
            && (env::var_os("GITHUB_ACTIONS").is_some()
                || env::var_os("GITHUB_REPOSITORY").is_some()
                || env::var_os("GITHUB_SHA").is_some())
        {
            return Err(GeneratorError::usage(
                "hosted planning requires explicit EVENT_NAME",
            ));
        }
        classify_event_names(
            explicit.as_deref(),
            runner.as_deref(),
            env::var("GITHUB_REF_TYPE").ok().as_deref(),
        )
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::PullRequestTarget => "pull_request_target",
            Self::Push => "push",
            Self::Schedule => "schedule",
            Self::MergeGroup => "merge_group",
            Self::WorkflowDispatch => "workflow_dispatch",
            Self::Tag => "tag",
            Self::Local => "",
        }
    }

    pub(crate) const fn requires_full_scope(self) -> bool {
        matches!(
            self,
            Self::Push | Self::Schedule | Self::MergeGroup | Self::Tag
        )
    }
}

fn classify_event_names(
    explicit: Option<&str>,
    runner: Option<&str>,
    ref_type: Option<&str>,
) -> Result<EventKind, GeneratorError> {
    if let (Some(explicit), Some(runner)) = (explicit, runner)
        && explicit != runner
        && !(explicit == "tag" && runner == "push")
    {
        return Err(GeneratorError::usage(format!(
            "EVENT_NAME `{explicit}` disagrees with runner event `{runner}`"
        )));
    }
    let value = explicit.or(runner).unwrap_or_default();
    let is_tag = value == "tag" || (value == "push" && ref_type == Some("tag"));
    EventKind::parse(if is_tag { "tag" } else { value })
}

/// Immutable identity facts supplied by the controller at the plan boundary.
/// These fields stay attached to the trust verdict instead of being reduced to
/// a caller-controlled boolean. A PR's source and audited checkout are
/// intentionally separate: the latter is GitHub's synthetic merge revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventIdentity {
    pub(crate) repository: String,
    pub(crate) source_repository: String,
    pub(crate) source_sha: String,
    pub(crate) audited_sha: String,
    pub(crate) base_sha: Option<String>,
    pub(crate) reference: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) source_is_fork: bool,
    pub(crate) author_is_bot: bool,
    pub(crate) source_verified: bool,
    /// `GITHUB_REF_PROTECTED`, when the runner supplied it. Absence is not
    /// proof of a protected dispatch ref and can never widen trust.
    pub(crate) ref_protected: Option<bool>,
    /// The repository default branch from the controller event. Dispatch
    /// trust requires the selected ref to equal this branch.
    pub(crate) default_branch: Option<String>,
    pub(crate) checkout: CheckoutRef,
}

impl EventIdentity {
    pub(crate) fn local(source_sha: String, audited_sha: String, base_sha: Option<String>) -> Self {
        Self {
            repository: "local/local".to_owned(),
            source_repository: "local/local".to_owned(),
            source_sha,
            audited_sha,
            base_sha,
            reference: "local".to_owned(),
            run_id: "1".to_owned(),
            run_attempt: "1".to_owned(),
            source_is_fork: false,
            author_is_bot: false,
            source_verified: true,
            ref_protected: None,
            default_branch: None,
            checkout: CheckoutRef::Head,
        }
    }

    fn validate(&self, kind: EventKind) -> Result<(), GeneratorError> {
        for (name, value) in [
            ("repository", self.repository.as_str()),
            ("source repository", self.source_repository.as_str()),
            ("source SHA", self.source_sha.as_str()),
            ("audited SHA", self.audited_sha.as_str()),
            ("ref", self.reference.as_str()),
            ("run ID", self.run_id.as_str()),
            ("run attempt", self.run_attempt.as_str()),
        ] {
            if value.is_empty() || value.contains(['\n', '\r']) {
                return Err(GeneratorError::usage(format!(
                    "{kind:?} event identity has an invalid {name}"
                )));
            }
        }
        if !valid_repository(&self.repository) || !valid_repository(&self.source_repository) {
            return Err(GeneratorError::usage(
                "event identity repositories must be owner/name values",
            ));
        }
        for (name, value) in [("run ID", &self.run_id), ("run attempt", &self.run_attempt)] {
            if value == "0" || value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(GeneratorError::usage(format!(
                    "{kind:?} event identity {name} must be a positive decimal"
                )));
            }
        }
        if self
            .base_sha
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.contains(['\n', '\r']))
        {
            return Err(GeneratorError::usage(format!(
                "{kind:?} event identity has an invalid base SHA"
            )));
        }
        if self
            .default_branch
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.contains(['\n', '\r']))
        {
            return Err(GeneratorError::usage(format!(
                "{kind:?} event identity has an invalid default branch"
            )));
        }
        let source_repository_differs = self.source_repository != self.repository;
        if self.source_is_fork != source_repository_differs {
            return Err(GeneratorError::usage(format!(
                "{kind:?} event identity has inconsistent fork and source repository facts"
            )));
        }
        let expected_checkout = match kind {
            EventKind::PullRequestTarget => CheckoutRef::Base,
            EventKind::PullRequest
            | EventKind::Push
            | EventKind::Schedule
            | EventKind::MergeGroup
            | EventKind::WorkflowDispatch
            | EventKind::Tag
            | EventKind::Local => CheckoutRef::Head,
        };
        if self.checkout != expected_checkout {
            return Err(GeneratorError::usage(format!(
                "{kind:?} event identity names checkout {:?}, expected {:?}",
                self.checkout, expected_checkout
            )));
        }
        if kind == EventKind::WorkflowDispatch {
            let Some(default_branch) = self.default_branch.as_deref() else {
                return Err(GeneratorError::usage(
                    "workflow_dispatch event identity is missing the default branch",
                ));
            };
            let expected_reference = format!("refs/heads/{default_branch}");
            if self.reference != expected_reference {
                return Err(GeneratorError::usage(format!(
                    "workflow_dispatch ref `{}` is not the default branch `{default_branch}`",
                    self.reference
                )));
            }
        }
        Ok(())
    }

    fn same_repository(&self) -> bool {
        !self.source_is_fork && self.repository == self.source_repository
    }
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !owner.is_empty()
        && !name.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}

/// Every CI event with explicit trust coverage and its immutable controller
/// facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CiEvent {
    /// A pull request: forks are untrusted; same-repo PRs still need the
    /// controller's source verification; bot authors are always untrusted.
    PullRequest(EventIdentity),
    /// The privileged `pull_request_target` context: never privileged with an
    /// untrusted checkout, and the checkout is always the base ref.
    PullRequestTarget(EventIdentity),
    /// A push, typically to the default branch.
    Push(EventIdentity),
    /// A scheduled run on the default branch.
    Schedule(EventIdentity),
    /// A manual dispatch: the controller verifies the ref before trusting it.
    WorkflowDispatch(EventIdentity),
    /// A tag push: tags are repository-controlled.
    Tag(EventIdentity),
    /// A merge-group check.
    MergeGroup(EventIdentity),
    /// A local invocation has no hosted trust boundary and never authorizes
    /// hosted credentials or persistent providers.
    Local(EventIdentity),
}

impl CiEvent {
    pub(crate) fn from_identity(
        kind: EventKind,
        identity: EventIdentity,
    ) -> Result<Self, GeneratorError> {
        identity.validate(kind)?;
        Ok(match kind {
            EventKind::PullRequest => Self::PullRequest(identity),
            EventKind::PullRequestTarget => Self::PullRequestTarget(identity),
            EventKind::Push => Self::Push(identity),
            EventKind::Schedule => Self::Schedule(identity),
            EventKind::MergeGroup => Self::MergeGroup(identity),
            EventKind::WorkflowDispatch => Self::WorkflowDispatch(identity),
            EventKind::Tag => Self::Tag(identity),
            EventKind::Local => Self::Local(identity),
        })
    }

    pub(crate) fn kind(&self) -> EventKind {
        match self {
            Self::PullRequest(_) => EventKind::PullRequest,
            Self::PullRequestTarget(_) => EventKind::PullRequestTarget,
            Self::Push(_) => EventKind::Push,
            Self::Schedule(_) => EventKind::Schedule,
            Self::WorkflowDispatch(_) => EventKind::WorkflowDispatch,
            Self::Tag(_) => EventKind::Tag,
            Self::MergeGroup(_) => EventKind::MergeGroup,
            Self::Local(_) => EventKind::Local,
        }
    }

    pub(crate) fn identity(&self) -> &EventIdentity {
        match self {
            Self::PullRequest(identity)
            | Self::PullRequestTarget(identity)
            | Self::Push(identity)
            | Self::Schedule(identity)
            | Self::WorkflowDispatch(identity)
            | Self::Tag(identity)
            | Self::MergeGroup(identity)
            | Self::Local(identity) => identity,
        }
    }
}

/// Which ref an event context checks out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckoutRef {
    /// The event's audited checkout (the synthetic PR merge, push SHA, or
    /// merge-group revision).
    Head,
    /// The trusted base ref. `pull_request_target` always checks out base:
    /// untrusted code is never executed in the privileged context.
    Base,
}

/// The controller-side verdict: trusted or not, which providers may execute,
/// and which ref is checked out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustVerdict {
    pub(crate) event: EventKind,
    pub(crate) trusted: bool,
    pub(crate) providers: ProviderSet,
    pub(crate) checkout: CheckoutRef,
}

impl TrustVerdict {
    fn trusted_full(event: EventKind, checkout: CheckoutRef) -> Self {
        Self {
            event,
            trusted: true,
            providers: ProviderSet::from([
                ProviderId::GithubHosted,
                ProviderId::GithubSelfHosted,
                ProviderId::Velnor,
            ]),
            checkout,
        }
    }

    fn untrusted_hosted(event: EventKind, checkout: CheckoutRef) -> Self {
        Self {
            event,
            trusted: false,
            providers: ProviderSet::from([ProviderId::GithubHosted]),
            checkout,
        }
    }
}

/// Evaluate one typed event identity. Trust is derived from the complete
/// controller facts; no caller-provided boolean can widen the verdict.
pub(crate) fn evaluate_event(event: &CiEvent) -> Result<TrustVerdict, GeneratorError> {
    let kind = event.kind();
    let identity = event.identity();
    identity.validate(kind)?;
    let same_repo = identity.same_repository();
    match event {
        CiEvent::PullRequest(_) => {
            // Forks default to hosted; bots are untrusted everywhere; a
            // same-repo origin alone is never sufficient without the
            // controller's source verification.
            if same_repo && !identity.author_is_bot && identity.source_verified {
                Ok(TrustVerdict::trusted_full(kind, CheckoutRef::Head))
            } else if same_repo && !identity.source_verified {
                Err(GeneratorError::usage(
                    "same-repository pull request source is not controller-verified",
                ))
            } else {
                Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Head))
            }
        }
        CiEvent::PullRequestTarget(_) => {
            // Never privileged-untrusted: without verification the context is
            // hosted-only, and the checkout is always the base ref either way.
            if same_repo && !identity.author_is_bot && identity.source_verified {
                Ok(TrustVerdict::trusted_full(kind, CheckoutRef::Base))
            } else {
                Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Base))
            }
        }
        CiEvent::Push(_) | CiEvent::Schedule(_) | CiEvent::Tag(_) => {
            // Same-repo, controller-owned origins. Push/schedule/tag/merge
            // queue entries cannot be forged from a fork, so the event plus
            // the controller's own delivery is the verification.
            if !same_repo || !identity.source_verified {
                return Err(GeneratorError::usage(format!(
                    "{kind:?} requires a verified same-repository source"
                )));
            }
            if identity.author_is_bot {
                return Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Head));
            }
            Ok(TrustVerdict::trusted_full(kind, CheckoutRef::Head))
        }
        CiEvent::MergeGroup(_) => {
            if identity.base_sha.is_none() {
                return Err(GeneratorError::usage(
                    "merge_group event identity requires BASE_SHA",
                ));
            }
            // `actions/runner` supplies the merge-group event/ref/SHA, but it
            // does not attest the fork or bot provenance of every constituent
            // pull request. A same-repository merge-group payload is therefore
            // not a trusted source boundary. Keep prospective-main checks on
            // GitHub-hosted runners; only an independently verified admission
            // path may add local providers later.
            Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Head))
        }
        CiEvent::WorkflowDispatch(_) => {
            // Dispatch can target any ref. Privileged providers require the
            // controller-verified source, a non-bot same-repository actor, and
            // both default-branch and protection facts. Missing protection
            // evidence remains hosted-only.
            if same_repo
                && !identity.author_is_bot
                && identity.source_verified
                && identity.ref_protected == Some(true)
            {
                Ok(TrustVerdict::trusted_full(kind, CheckoutRef::Head))
            } else if same_repo {
                if !identity.source_verified {
                    Err(GeneratorError::usage(
                        "workflow_dispatch source is not controller-verified",
                    ))
                } else {
                    Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Head))
                }
            } else {
                Ok(TrustVerdict::untrusted_hosted(kind, CheckoutRef::Head))
            }
        }
        CiEvent::Local(_) => Ok(TrustVerdict::trusted_full(kind, CheckoutRef::Head)),
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

    fn event(
        kind: EventKind,
        source_repository: &str,
        source_is_fork: bool,
        author_is_bot: bool,
        source_verified: bool,
        base_sha: Option<&str>,
    ) -> CiEvent {
        CiEvent::from_identity(
            kind,
            EventIdentity {
                repository: "example/repo".to_owned(),
                source_repository: source_repository.to_owned(),
                source_sha: "source-sha".to_owned(),
                audited_sha: "audited-sha".to_owned(),
                base_sha: base_sha.map(ToOwned::to_owned),
                reference: "refs/heads/main".to_owned(),
                run_id: "42".to_owned(),
                run_attempt: "1".to_owned(),
                source_is_fork,
                author_is_bot,
                source_verified,
                ref_protected: (kind == EventKind::WorkflowDispatch).then_some(true),
                default_branch: (kind == EventKind::WorkflowDispatch).then(|| "main".to_owned()),
                checkout: if kind == EventKind::PullRequestTarget {
                    CheckoutRef::Base
                } else {
                    CheckoutRef::Head
                },
            },
        )
        .unwrap()
    }

    fn verdict(event: &CiEvent) -> TrustVerdict {
        evaluate_event(event).unwrap()
    }

    #[test]
    fn fork_pr_defaults_to_hosted_even_when_verified() {
        let verdict = verdict(&event(
            EventKind::PullRequest,
            "fork/repo",
            true,
            false,
            true,
            Some("base-sha"),
        ));
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
    }

    #[test]
    fn bot_pr_is_untrusted_even_same_repo() {
        let verdict = verdict(&event(
            EventKind::PullRequest,
            "example/repo",
            false,
            true,
            true,
            Some("base-sha"),
        ));
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
    }

    #[test]
    fn same_repo_origin_alone_is_never_sufficient() {
        let error = must_fail(
            evaluate_event(&event(
                EventKind::PullRequest,
                "example/repo",
                false,
                false,
                false,
                Some("base-sha"),
            )),
            "same-repository unverified source",
        );
        assert!(error.contains("not controller-verified"), "{error}");
        let verified = verdict(&event(
            EventKind::PullRequest,
            "example/repo",
            false,
            false,
            true,
            Some("base-sha"),
        ));
        assert!(verified.trusted);
        assert_eq!(verified.providers, full());
    }

    #[test]
    fn pull_request_target_is_never_privileged_untrusted_and_checks_out_base() {
        let untrusted = verdict(&event(
            EventKind::PullRequestTarget,
            "fork/repo",
            true,
            false,
            false,
            Some("base-sha"),
        ));
        assert!(!untrusted.trusted);
        assert_eq!(untrusted.providers, hosted_only());
        assert_eq!(untrusted.checkout, CheckoutRef::Base);
        // Even the verified context checks out base, never the PR head.
        let verified = verdict(&event(
            EventKind::PullRequestTarget,
            "example/repo",
            false,
            false,
            true,
            Some("base-sha"),
        ));
        assert!(verified.trusted);
        assert_eq!(verified.checkout, CheckoutRef::Base);

        let bot = verdict(&event(
            EventKind::PullRequestTarget,
            "example/repo",
            false,
            true,
            true,
            Some("base-sha"),
        ));
        assert!(!bot.trusted);
        assert_eq!(bot.providers, hosted_only());
        assert_eq!(bot.checkout, CheckoutRef::Base);
    }

    #[test]
    fn controller_owned_events_are_trusted() {
        for kind in [EventKind::Push, EventKind::Schedule, EventKind::Tag] {
            let event = event(kind, "example/repo", false, false, true, Some("base-sha"));
            let verdict = verdict(&event);
            assert!(verdict.trusted, "{event:?} must be trusted");
            assert_eq!(verdict.providers, full());
        }
    }

    #[test]
    fn controller_owned_bot_events_are_hosted_only() {
        for kind in [
            EventKind::Push,
            EventKind::Schedule,
            EventKind::Tag,
            EventKind::WorkflowDispatch,
        ] {
            let event = event(kind, "example/repo", false, true, true, None);
            let verdict = verdict(&event);
            assert!(!verdict.trusted, "{kind:?} bot must not be trusted");
            assert_eq!(verdict.providers, hosted_only(), "{kind:?}");
        }
    }

    #[test]
    fn push_tag_classification_requires_runner_tag_fact() {
        assert_eq!(
            classify_event_names(Some("push"), Some("push"), Some("tag")).unwrap(),
            EventKind::Tag
        );
        assert_eq!(
            classify_event_names(Some("push"), Some("push"), Some("branch")).unwrap(),
            EventKind::Push
        );
        assert_eq!(
            classify_event_names(Some("tag"), Some("push"), Some("tag")).unwrap(),
            EventKind::Tag
        );
        let error = must_fail(
            classify_event_names(Some("tag"), Some("pull_request"), Some("tag")),
            "contradictory event names",
        );
        assert!(error.contains("disagrees with runner event"), "{error}");
    }

    #[test]
    fn merge_group_is_hosted_only_even_with_same_repo_verified_facts() {
        for (source_repository, source_is_fork, author_is_bot, source_verified) in [
            ("example/repo", false, false, true),
            ("fork/repo", true, false, true),
            ("example/repo", false, true, true),
            ("example/repo", false, false, false),
        ] {
            let event = event(
                EventKind::MergeGroup,
                source_repository,
                source_is_fork,
                author_is_bot,
                source_verified,
                Some("base-sha"),
            );
            let verdict = verdict(&event);
            assert!(!verdict.trusted, "{event:?} must not gain generic trust");
            assert_eq!(verdict.providers, hosted_only(), "{event:?}");
            assert_eq!(verdict.checkout, CheckoutRef::Head);
        }
    }

    #[test]
    fn dispatch_needs_source_verification() {
        let error = must_fail(
            evaluate_event(&event(
                EventKind::WorkflowDispatch,
                "example/repo",
                false,
                false,
                false,
                None,
            )),
            "unverified dispatch source",
        );
        assert!(error.contains("not controller-verified"), "{error}");
        let verified_event = event(
            EventKind::WorkflowDispatch,
            "example/repo",
            false,
            false,
            true,
            None,
        );
        let verified = verdict(&verified_event);
        assert!(verified.trusted);

        let mut unprotected = verified_event.identity().clone();
        unprotected.ref_protected = Some(false);
        let unprotected = CiEvent::from_identity(EventKind::WorkflowDispatch, unprotected)
            .expect("unprotected dispatch identity remains structurally valid");
        let unprotected_verdict = verdict(&unprotected);
        assert!(!unprotected_verdict.trusted);
        assert_eq!(unprotected_verdict.providers, hosted_only());

        let mut wrong_branch = verified_event.identity().clone();
        wrong_branch.default_branch = Some("trunk".to_owned());
        let error = must_fail(
            CiEvent::from_identity(EventKind::WorkflowDispatch, wrong_branch),
            "dispatch on a non-default branch",
        );
        assert!(error.contains("not the default branch"), "{error}");
    }

    #[test]
    fn typed_identity_survives_verdict_and_merge_group_requires_base() {
        let merge_event = event(
            EventKind::MergeGroup,
            "example/repo",
            false,
            false,
            true,
            Some("base-sha"),
        );
        let verdict = verdict(&merge_event);
        assert_eq!(verdict.event, EventKind::MergeGroup);
        assert!(!verdict.trusted);
        assert_eq!(verdict.providers, hosted_only());
        assert_eq!(verdict.checkout, CheckoutRef::Head);
        assert_eq!(merge_event.identity().source_repository, "example/repo");
        assert_eq!(merge_event.identity().source_sha, "source-sha");
        assert_eq!(merge_event.identity().audited_sha, "audited-sha");
        assert_eq!(merge_event.identity().base_sha.as_deref(), Some("base-sha"));
        assert_eq!(merge_event.identity().run_id, "42");
        assert_eq!(merge_event.identity().run_attempt, "1");

        let missing_base = event(
            EventKind::MergeGroup,
            "example/repo",
            false,
            false,
            true,
            None,
        );
        let error = must_fail(
            evaluate_event(&missing_base),
            "merge-group without base SHA",
        );
        assert!(error.contains("requires BASE_SHA"), "{error}");
    }

    #[test]
    fn malformed_identity_is_rejected_before_trust_evaluation() {
        let error = must_fail(
            CiEvent::from_identity(
                EventKind::PullRequest,
                EventIdentity {
                    repository: "example/repo".to_owned(),
                    source_repository: "example/repo".to_owned(),
                    source_sha: String::new(),
                    audited_sha: "audited-sha".to_owned(),
                    base_sha: Some("base-sha".to_owned()),
                    reference: "refs/pull/1/merge".to_owned(),
                    run_id: "42".to_owned(),
                    run_attempt: "1".to_owned(),
                    source_is_fork: false,
                    author_is_bot: false,
                    source_verified: true,
                    ref_protected: Some(true),
                    default_branch: Some("main".to_owned()),
                    checkout: CheckoutRef::Head,
                },
            ),
            "malformed source identity",
        );
        assert!(error.contains("invalid source SHA"), "{error}");
    }

    #[test]
    fn repository_and_fork_facts_accept_valid_pairs_and_reject_mismatches() {
        for (source_repository, source_is_fork) in [("example/repo", false), ("fork/repo", true)] {
            let identity = EventIdentity {
                repository: "example/repo".to_owned(),
                source_repository: source_repository.to_owned(),
                source_sha: "source-sha".to_owned(),
                audited_sha: "audited-sha".to_owned(),
                base_sha: Some("base-sha".to_owned()),
                reference: "refs/pull/1/merge".to_owned(),
                run_id: "42".to_owned(),
                run_attempt: "1".to_owned(),
                source_is_fork,
                author_is_bot: false,
                source_verified: true,
                ref_protected: None,
                default_branch: None,
                checkout: CheckoutRef::Head,
            };
            CiEvent::from_identity(EventKind::PullRequest, identity).unwrap();
        }

        let mut inconsistent =
            EventIdentity::local("source".to_owned(), "audited".to_owned(), None);
        inconsistent.source_repository = "fork/repo".to_owned();
        let error = must_fail(
            CiEvent::from_identity(EventKind::Local, inconsistent),
            "inconsistent local repository/fork facts",
        );
        assert!(error.contains("inconsistent fork"), "{error}");

        let local = CiEvent::from_identity(
            EventKind::Local,
            EventIdentity::local("source".to_owned(), "audited".to_owned(), None),
        )
        .unwrap();
        assert!(evaluate_event(&local).unwrap().trusted);
    }

    #[test]
    fn every_event_variant_has_explicit_coverage() {
        // Exhaustive: adding a CiEvent variant breaks this match until the
        // new event gets its verdict test.
        let events = [
            event(
                EventKind::PullRequest,
                "fork/repo",
                true,
                false,
                true,
                Some("base-sha"),
            ),
            event(
                EventKind::PullRequest,
                "example/repo",
                false,
                true,
                true,
                Some("base-sha"),
            ),
            event(
                EventKind::PullRequestTarget,
                "fork/repo",
                true,
                false,
                false,
                Some("base-sha"),
            ),
            event(EventKind::Push, "example/repo", false, false, true, None),
            event(
                EventKind::Schedule,
                "example/repo",
                false,
                false,
                true,
                None,
            ),
            event(
                EventKind::WorkflowDispatch,
                "fork/repo",
                true,
                false,
                false,
                None,
            ),
            event(EventKind::Tag, "example/repo", false, false, true, None),
            event(
                EventKind::MergeGroup,
                "example/repo",
                false,
                false,
                true,
                Some("base-sha"),
            ),
        ];
        for event in &events {
            let verdict = verdict(event);
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
        let fork = verdict(&event(
            EventKind::PullRequest,
            "fork/repo",
            true,
            false,
            true,
            Some("base-sha"),
        ));
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
        let main = verdict(&event(
            EventKind::Push,
            "example/repo",
            false,
            false,
            true,
            None,
        ));
        let error = must_fail(
            check_label_spoof(&[String::from("gpu-a100")], &selectors(), &main),
            "unknown requested label",
        );
        assert!(error.contains("matches no provider selector"), "{error}");
    }

    #[test]
    fn labels_never_widen_the_verdict() {
        let fork = verdict(&event(
            EventKind::PullRequest,
            "fork/repo",
            true,
            false,
            true,
            Some("base-sha"),
        ));
        let routed =
            check_label_spoof(&[String::from("ubuntu-24.04")], &selectors(), &fork).unwrap();
        assert!(routed
            .iter()
            .all(|provider| fork.providers.contains(provider)));
        assert!(!fork.trusted);
    }

    #[test]
    fn untrusted_input_substitution_is_rejected() {
        let fork = verdict(&event(
            EventKind::PullRequest,
            "fork/repo",
            true,
            false,
            true,
            Some("base-sha"),
        ));
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
        let main = verdict(&event(
            EventKind::Push,
            "example/repo",
            false,
            false,
            true,
            None,
        ));
        let inputs = BTreeMap::from([("providers".to_owned(), "velnor".to_owned())]);
        check_workflow_inputs(&inputs, &main).unwrap();
    }
}

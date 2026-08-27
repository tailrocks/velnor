//! Sanitized job summary for the operational store.
//!
//! This is the only job-summary shape that may be persisted to the durable
//! database. It is constructed exclusively from already-normalized inputs:
//! typed enums, RFC 3339 [`Timestamp`]s, numeric IDs, and short slugs whose
//! characters are restricted to `[A-Za-z0-9._/-]` and which additionally
//! reject credential-indicating substrings (token, secret, password,
//! bearer, GitHub token prefixes, ...). There is no field for a run-service
//! URL, billing owner, endpoint, environment variable map, or any
//! free-form string, so a secret-bearing value cannot be represented,
//! serialized, or stored through this type.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::sanitized::RepositoryRef;
use crate::time::Timestamp;

/// Longest accepted slug; generous for deep workflow paths and long ref
/// names while still bounding every stored column.
pub const MAX_SLUG_LEN: usize = 512;

/// The single character set any textual summary value may contain.
fn is_slug_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/')
}

/// Substrings no legitimate summary field ever contains. A job summary
/// describes names, refs, and policy labels — never credentials — so any
/// match means raw upstream text reached the constructor. Compared
/// case-insensitively over ASCII.
const DENIED_SUBSTRINGS: [&str; 12] = [
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
    "authorization",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
];

fn denied_substring(raw: &str) -> bool {
    let lowered = raw.to_ascii_lowercase();
    DENIED_SUBSTRINGS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// One validated textual summary component.
///
/// Construction fails closed: an empty value, a value longer than
/// [`MAX_SLUG_LEN`], a value containing any character outside
/// `[A-Za-z0-9._/-]`, or a value containing a credential-indicating
/// substring is rejected with the offending field named. The raw input is
/// never echoed in the error.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Slug(String);

impl Slug {
    /// Validate `raw` as the named summary field.
    ///
    /// # Errors
    /// [`InvalidJobSummaryField`] naming `field`; the reason describes the
    /// violated rule without echoing the rejected value.
    pub fn validate(field: &'static str, raw: &str) -> Result<Self, InvalidJobSummaryField> {
        if raw.is_empty() {
            return Err(InvalidJobSummaryField::rule(field, "must not be empty"));
        }
        if raw.len() > MAX_SLUG_LEN {
            return Err(InvalidJobSummaryField::rule(
                field,
                &format!("length {} exceeds cap {MAX_SLUG_LEN}", raw.len()),
            ));
        }
        if let Some(ch) = raw.chars().find(|ch| !is_slug_char(*ch)) {
            return Err(InvalidJobSummaryField::rule(
                field,
                "contains a character outside [A-Za-z0-9._/-]",
            )
            .with_char_hint(ch));
        }
        if denied_substring(raw) {
            return Err(InvalidJobSummaryField::rule(
                field,
                "contains a credential- or secret-indicating substring",
            ));
        }
        Ok(Self(raw.to_owned()))
    }

    /// The validated text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A construction failure naming the exact summary field that was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidJobSummaryField {
    /// Field that failed validation, for example `"workflow"`.
    pub field: &'static str,
    reason: String,
}

impl InvalidJobSummaryField {
    fn rule(field: &'static str, rule: &str) -> Self {
        Self {
            field,
            reason: rule.to_owned(),
        }
    }

    fn with_char_hint(mut self, ch: char) -> Self {
        // Name the character class, never the surrounding value: the input
        // may itself be the secret being kept out of the store.
        let class = if ch.is_control() {
            "control character".to_owned()
        } else if ch.is_whitespace() {
            "whitespace".to_owned()
        } else {
            format!("character {ch:?}")
        };
        self.reason = format!("contains {class} outside [A-Za-z0-9._/-]");
        self
    }
}

impl fmt::Display for InvalidJobSummaryField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid job summary field '{}': {}",
            self.field, self.reason
        )
    }
}

impl std::error::Error for InvalidJobSummaryField {}

/// GitHub event that queued the job; closed set, fail-closed otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEvent {
    Push,
    PullRequest,
    WorkflowDispatch,
    RepositoryDispatch,
    Schedule,
    Release,
}

impl TriggerEvent {
    /// Canonical GitHub spelling stored in the summary column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::PullRequest => "pull_request",
            Self::WorkflowDispatch => "workflow_dispatch",
            Self::RepositoryDispatch => "repository_dispatch",
            Self::Schedule => "schedule",
            Self::Release => "release",
        }
    }
}

impl TryFrom<&str> for TriggerEvent {
    type Error = InvalidJobSummaryField;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "push" => Ok(Self::Push),
            "pull_request" => Ok(Self::PullRequest),
            "workflow_dispatch" => Ok(Self::WorkflowDispatch),
            "repository_dispatch" => Ok(Self::RepositoryDispatch),
            "schedule" => Ok(Self::Schedule),
            "release" => Ok(Self::Release),
            _ => Err(InvalidJobSummaryField::rule(
                "trigger_event",
                "is not a known trigger event",
            )),
        }
    }
}

/// Lifecycle phase of one persisted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Running,
    Completed,
    Canceled,
    Rejected,
}

impl JobPhase {
    /// Canonical spelling stored in the summary column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Rejected => "rejected",
        }
    }
}

impl TryFrom<&str> for JobPhase {
    type Error = InvalidJobSummaryField;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            "rejected" => Ok(Self::Rejected),
            _ => Err(InvalidJobSummaryField::rule(
                "phase",
                "is not a known job phase",
            )),
        }
    }
}

/// Terminal conclusion reported by GitHub Actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobConclusion {
    Success,
    Failure,
    Cancelled,
    Neutral,
    Skipped,
    TimedOut,
    StartupFailure,
    ActionRequired,
    Stale,
}

impl JobConclusion {
    /// Canonical GitHub spelling stored in the summary column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Neutral => "neutral",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
            Self::StartupFailure => "startup_failure",
            Self::ActionRequired => "action_required",
            Self::Stale => "stale",
        }
    }
}

impl TryFrom<&str> for JobConclusion {
    type Error = InvalidJobSummaryField;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            "cancelled" => Ok(Self::Cancelled),
            "neutral" => Ok(Self::Neutral),
            "skipped" => Ok(Self::Skipped),
            "timed_out" => Ok(Self::TimedOut),
            "startup_failure" => Ok(Self::StartupFailure),
            "action_required" => Ok(Self::ActionRequired),
            "stale" => Ok(Self::Stale),
            _ => Err(InvalidJobSummaryField::rule(
                "conclusion",
                "is not a known job conclusion",
            )),
        }
    }
}

/// Infrastructure failure category; mirrors the runner's closed classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureCategory {
    DockerBindMount,
    DockerEnvironment,
}

impl InfrastructureCategory {
    /// Canonical runner classifier spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DockerBindMount => "docker_bind_mount",
            Self::DockerEnvironment => "docker_environment",
        }
    }
}

impl TryFrom<&str> for InfrastructureCategory {
    type Error = InvalidJobSummaryField;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "docker_bind_mount" => Ok(Self::DockerBindMount),
            "docker_environment" => Ok(Self::DockerEnvironment),
            _ => Err(InvalidJobSummaryField::rule(
                "infrastructure_category",
                "is not a known infrastructure category",
            )),
        }
    }
}

/// Already-normalized inputs for [`JobSummary::from_normalized`].
///
/// Every string is a slug candidate revalidated at construction; there is
/// deliberately no field able to carry an endpoint URL, an authorization
/// header, an environment-variable map, or any unbounded text.
#[derive(Debug, Clone)]
pub struct NormalizedJob {
    pub instance_slug: String,
    /// Daemon-canonical unique job identity within the instance.
    pub job_uid: String,
    pub repository: RepositoryRef,
    /// Workflow path such as `.github/workflows/ci.yml`.
    pub workflow: String,
    pub job_name: String,
    pub run_id: Option<u64>,
    pub attempt: Option<u32>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub trigger_event: Option<TriggerEvent>,
    pub queued_at: Option<Timestamp>,
    pub acquired_at: Option<Timestamp>,
    pub slot_name: Option<String>,
    pub runner_name: Option<String>,
    pub trust_scope: Option<String>,
    pub resource_policy: Option<String>,
    pub phase: JobPhase,
    pub conclusion: Option<JobConclusion>,
    pub infrastructure_category: Option<InfrastructureCategory>,
}

/// Sanitized persistent description of one job.
///
/// Fields mirror the store's summary columns exactly and nothing more: no
/// run-service URL and no billing owner exist here — those live only in the
/// runner's private in-flight record until reconciliation switches over.
/// The only way to obtain a value is [`JobSummary::from_normalized`], which
/// validates every textual field against the slug charset, so nothing can
/// reach the database unvalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    instance_slug: Slug,
    job_uid: Slug,
    repository: RepositoryRef,
    workflow: Slug,
    job_name: Slug,
    run_id: Option<u64>,
    attempt: Option<u32>,
    head_ref: Option<Slug>,
    head_sha: Option<Slug>,
    trigger_event: Option<TriggerEvent>,
    queued_at: Option<Timestamp>,
    acquired_at: Option<Timestamp>,
    slot_name: Option<Slug>,
    runner_name: Option<Slug>,
    trust_scope: Option<Slug>,
    resource_policy: Option<Slug>,
    phase: JobPhase,
    conclusion: Option<JobConclusion>,
    infrastructure_category: Option<InfrastructureCategory>,
}

/// Wire mirror used only to route deserialization back through validation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobSummaryWire {
    instance_slug: String,
    job_uid: String,
    repository: RepositoryRef,
    workflow: String,
    job_name: String,
    run_id: Option<u64>,
    attempt: Option<u32>,
    head_ref: Option<String>,
    head_sha: Option<String>,
    trigger_event: Option<TriggerEvent>,
    queued_at: Option<Timestamp>,
    acquired_at: Option<Timestamp>,
    slot_name: Option<String>,
    runner_name: Option<String>,
    trust_scope: Option<String>,
    resource_policy: Option<String>,
    phase: JobPhase,
    conclusion: Option<JobConclusion>,
    infrastructure_category: Option<InfrastructureCategory>,
}

impl TryFrom<JobSummaryWire> for JobSummary {
    type Error = InvalidJobSummaryField;

    fn try_from(wire: JobSummaryWire) -> Result<Self, Self::Error> {
        Self::from_normalized(NormalizedJob {
            instance_slug: wire.instance_slug,
            job_uid: wire.job_uid,
            repository: wire.repository,
            workflow: wire.workflow,
            job_name: wire.job_name,
            run_id: wire.run_id,
            attempt: wire.attempt,
            head_ref: wire.head_ref,
            head_sha: wire.head_sha,
            trigger_event: wire.trigger_event,
            queued_at: wire.queued_at,
            acquired_at: wire.acquired_at,
            slot_name: wire.slot_name,
            runner_name: wire.runner_name,
            trust_scope: wire.trust_scope,
            resource_policy: wire.resource_policy,
            phase: wire.phase,
            conclusion: wire.conclusion,
            infrastructure_category: wire.infrastructure_category,
        })
    }
}

impl<'de> Deserialize<'de> for JobSummary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        JobSummaryWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl JobSummary {
    /// Build from normalized inputs, validating every textual field.
    ///
    /// Fails closed naming the first rejected field; the error never echoes
    /// the rejected value.
    ///
    /// # Errors
    /// [`InvalidJobSummaryField`] for empty values, values outside the slug
    /// charset, values over [`MAX_SLUG_LEN`], credential-indicating
    /// substrings, or a repository reference whose owner/name violates any
    /// of those rules.
    pub fn from_normalized(input: NormalizedJob) -> Result<Self, InvalidJobSummaryField> {
        let repository = validate_repository(&input.repository)?;
        Ok(Self {
            instance_slug: Slug::validate("instance_slug", &input.instance_slug)?,
            job_uid: Slug::validate("job_uid", &input.job_uid)?,
            repository,
            workflow: Slug::validate("workflow", &input.workflow)?,
            job_name: Slug::validate("job_name", &input.job_name)?,
            run_id: input.run_id,
            attempt: input.attempt,
            head_ref: optional_slug("head_ref", input.head_ref)?,
            head_sha: optional_slug("head_sha", input.head_sha)?,
            trigger_event: input.trigger_event,
            queued_at: input.queued_at,
            acquired_at: input.acquired_at,
            slot_name: optional_slug("slot_name", input.slot_name)?,
            runner_name: optional_slug("runner_name", input.runner_name)?,
            trust_scope: optional_slug("trust_scope", input.trust_scope)?,
            resource_policy: optional_slug("resource_policy", input.resource_policy)?,
            phase: input.phase,
            conclusion: input.conclusion,
            infrastructure_category: input.infrastructure_category,
        })
    }

    #[must_use]
    pub fn instance_slug(&self) -> &str {
        self.instance_slug.as_str()
    }

    #[must_use]
    pub fn job_uid(&self) -> &str {
        self.job_uid.as_str()
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub fn workflow(&self) -> &str {
        self.workflow.as_str()
    }

    #[must_use]
    pub fn job_name(&self) -> &str {
        self.job_name.as_str()
    }

    #[must_use]
    pub const fn run_id(&self) -> Option<u64> {
        self.run_id
    }

    #[must_use]
    pub const fn attempt(&self) -> Option<u32> {
        self.attempt
    }

    #[must_use]
    pub fn head_ref(&self) -> Option<&str> {
        self.head_ref.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub fn head_sha(&self) -> Option<&str> {
        self.head_sha.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub const fn trigger_event(&self) -> Option<TriggerEvent> {
        self.trigger_event
    }

    #[must_use]
    pub const fn queued_at(&self) -> Option<Timestamp> {
        self.queued_at
    }

    #[must_use]
    pub const fn acquired_at(&self) -> Option<Timestamp> {
        self.acquired_at
    }

    #[must_use]
    pub fn runner_name(&self) -> Option<&str> {
        self.runner_name.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub fn slot_name(&self) -> Option<&str> {
        self.slot_name.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub fn trust_scope(&self) -> Option<&str> {
        self.trust_scope.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub fn resource_policy(&self) -> Option<&str> {
        self.resource_policy.as_ref().map(Slug::as_str)
    }

    #[must_use]
    pub const fn phase(&self) -> JobPhase {
        self.phase
    }

    #[must_use]
    pub const fn conclusion(&self) -> Option<JobConclusion> {
        self.conclusion
    }

    #[must_use]
    pub const fn infrastructure_category(&self) -> Option<InfrastructureCategory> {
        self.infrastructure_category
    }
}

fn optional_slug(
    field: &'static str,
    raw: Option<String>,
) -> Result<Option<Slug>, InvalidJobSummaryField> {
    raw.map(|value| Slug::validate(field, &value)).transpose()
}

fn validate_repository(
    repository: &RepositoryRef,
) -> Result<RepositoryRef, InvalidJobSummaryField> {
    for (field, segment) in [
        ("repository.owner", repository.owner.as_str()),
        ("repository.name", repository.name.as_str()),
    ] {
        Slug::validate(field, segment)?;
        // Owner and name are single path segments; a slash inside either
        // would make the stored `owner/name` full name ambiguous.
        if segment.contains('/') {
            return Err(InvalidJobSummaryField::rule(
                field,
                "is a single path segment and must not contain '/'",
            ));
        }
    }
    Ok(repository.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> RepositoryRef {
        RepositoryRef::new("tailrocks", "velnor-actions-fixture")
    }

    fn normalized() -> NormalizedJob {
        NormalizedJob {
            instance_slug: "sentry/main".to_owned(),
            job_uid: "summary-run-42-attempt-1".to_owned(),
            repository: repository(),
            workflow: ".github/workflows/control-plane.yml".to_owned(),
            job_name: "hold".to_owned(),
            run_id: Some(42),
            attempt: Some(1),
            head_ref: Some("velnor-estate-standard".to_owned()),
            head_sha: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned()),
            trigger_event: Some(TriggerEvent::WorkflowDispatch),
            queued_at: Some(Timestamp::parse("2026-08-24T12:30:45Z").unwrap()),
            acquired_at: Some(Timestamp::parse("2026-08-24T12:30:47Z").unwrap()),
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("fixture-runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            phase: JobPhase::Running,
            conclusion: None,
            infrastructure_category: None,
        }
    }

    #[test]
    fn empty_required_fields_fail_closed_naming_field() {
        let cases: [(usize, &str); 4] = [
            (0, "instance_slug"),
            (1, "job_uid"),
            (3, "workflow"),
            (4, "job_name"),
        ];
        for (slot, field) in cases {
            let mut input = normalized();
            match slot {
                0 => input.instance_slug = String::new(),
                1 => input.job_uid = String::new(),
                3 => input.workflow = String::new(),
                _ => input.job_name = String::new(),
            }
            let error = JobSummary::from_normalized(input).unwrap_err();
            assert_eq!(error.field, field);
            assert!(
                error.to_string().contains(field),
                "error must name {field}: {error}"
            );
        }
        // An explicitly empty repository owner fails the same way.
        let mut input = normalized();
        input.repository = RepositoryRef::new("", "fixture");
        assert_eq!(
            JobSummary::from_normalized(input).unwrap_err().field,
            "repository.owner"
        );
    }

    #[test]
    fn secret_marker_values_fail_construction_without_being_echoed() {
        let marker = "SECRET_MARKER_VALUE";
        let mut inputs = [normalized(), normalized(), normalized(), normalized()];
        inputs[0].workflow = marker.to_owned();
        inputs[1].runner_name = Some(marker.to_owned());
        inputs[2].head_ref = Some(marker.to_owned());
        inputs[3].job_uid = marker.to_owned();
        for (field, input) in [
            ("workflow", 0),
            ("runner_name", 1),
            ("head_ref", 2),
            ("job_uid", 3),
        ] {
            let error = JobSummary::from_normalized(inputs[input].clone()).unwrap_err();
            assert_eq!(error.field, field, "{error}");
            assert!(
                !error.to_string().contains(marker) && !format!("{error:?}").contains(marker),
                "error must not echo the rejected value"
            );
        }
    }

    #[test]
    fn credential_bearing_url_fails_construction_naming_field() {
        let raw = "https://build-agent:sup3r-secret-value@github.example.com/endpoint";
        let mut input = normalized();
        input.runner_name = Some(raw.to_owned());
        let error = JobSummary::from_normalized(input).unwrap_err();
        assert_eq!(error.field, "runner_name");
        assert!(
            !error.to_string().contains("sup3r") && !error.to_string().contains("://"),
            "credential must not leak into the error"
        );

        let mut input = normalized();
        input.repository = RepositoryRef::new("tailrocks", "ghp_deadbeefdeadbeef");
        assert_eq!(
            JobSummary::from_normalized(input).unwrap_err().field,
            "repository.name"
        );
    }

    #[test]
    fn control_characters_and_spaces_are_rejected_everywhere() {
        for bad in ["with space", "line\nbreak", "tab\tstop", "semi;colon"] {
            let mut input = normalized();
            input.job_name = bad.to_owned();
            let error = JobSummary::from_normalized(input).unwrap_err();
            assert_eq!(error.field, "job_name");
            assert!(!error.to_string().contains(bad));
        }
    }

    #[test]
    fn slug_length_cap_is_enforced() {
        let mut input = normalized();
        input.head_ref = Some("a".repeat(MAX_SLUG_LEN + 1));
        assert_eq!(
            JobSummary::from_normalized(input).unwrap_err().field,
            "head_ref"
        );
        let mut input = normalized();
        input.head_ref = Some("a".repeat(MAX_SLUG_LEN));
        assert!(JobSummary::from_normalized(input).is_ok());
    }

    #[test]
    fn valid_adversarial_but_ordinary_shapes_are_accepted() {
        let mut input = normalized();
        input.workflow = ".github/workflows/deep.path_v2/build.yml".to_owned();
        input.head_ref = Some("refs/pull/1337/head".to_owned());
        input.head_sha = Some("0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e".to_owned());
        let summary = JobSummary::from_normalized(input).expect("charset-valid shapes pass");
        assert_eq!(summary.head_ref(), Some("refs/pull/1337/head"));
        assert_eq!(summary.phase(), JobPhase::Running);
    }

    #[test]
    fn closed_enums_render_github_spellings_and_fail_closed_on_unknown() {
        for (value, spelling) in [
            (TriggerEvent::PullRequest, "pull_request"),
            (TriggerEvent::WorkflowDispatch, "workflow_dispatch"),
        ] {
            assert_eq!(value.as_str(), spelling);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{spelling}\"")
            );
        }
        for (value, spelling) in [
            (JobConclusion::TimedOut, "timed_out"),
            (JobConclusion::StartupFailure, "startup_failure"),
        ] {
            assert_eq!(value.as_str(), spelling);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{spelling}\"")
            );
        }
        for (value, spelling) in [
            (InfrastructureCategory::DockerBindMount, "docker_bind_mount"),
            (
                InfrastructureCategory::DockerEnvironment,
                "docker_environment",
            ),
        ] {
            assert_eq!(value.as_str(), spelling);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{spelling}\"")
            );
        }
        assert_eq!(JobPhase::Canceled.as_str(), "canceled");
        assert_eq!(
            serde_json::to_string(&JobPhase::Canceled).unwrap(),
            "\"canceled\""
        );
        assert!(TriggerEvent::try_from("dynamic").is_err());
        assert!(JobPhase::try_from("queued ").is_err());
        assert!(JobConclusion::try_from("").is_err());
        assert!(InfrastructureCategory::try_from("docker_network").is_err());
    }

    #[test]
    fn serialized_summary_contains_no_denied_substring() {
        let json =
            serde_json::to_string(&JobSummary::from_normalized(normalized()).unwrap()).unwrap();
        for denied in ["password", "token", "secret", "bearer", "url", "billing"] {
            assert!(
                !json.to_ascii_lowercase().contains(denied),
                "serialized summary leaked denied substring {denied}: {json}"
            );
        }
    }

    #[test]
    fn deserialization_routes_back_through_validation_fail_closed() {
        let good = r#"{
            "instanceSlug": "sentry/main",
            "jobUid": "summary-run-42-attempt-1",
            "repository": {"owner": "tailrocks", "name": "velnor-actions-fixture"},
            "workflow": ".github/workflows/ci.yml",
            "jobName": "build",
            "runId": 42,
            "attempt": 1,
            "headRef": null,
            "headSha": null,
            "triggerEvent": "push",
            "queuedAt": null,
            "acquiredAt": null,
            "runnerName": null,
            "trustScope": "trusted",
            "resourcePolicy": null,
            "phase": "completed",
            "conclusion": "success",
            "infrastructureCategory": null
        }"#;
        let parsed: JobSummary = serde_json::from_str(good).unwrap();
        assert_eq!(parsed.conclusion(), Some(JobConclusion::Success));

        let smuggled = good.replace(".github/workflows/ci.yml", "ghp_smuggledtokenvalue");
        let outcome = serde_json::from_str::<JobSummary>(&smuggled);
        assert!(outcome.is_err(), "secret-bearing wire input must fail");
        assert!(!format!("{outcome:?}").contains("ghp_smuggledtokenvalue"));

        let unknown_phase = good.replace("\"phase\": \"completed\"", "\"phase\": \"mysterious\"");
        assert!(serde_json::from_str::<JobSummary>(&unknown_phase).is_err());

        let extra = good.replace(
            "\"jobName\": \"build\",",
            "\"jobName\": \"build\",\"extra\": 1,",
        );
        assert!(serde_json::from_str::<JobSummary>(&extra).is_err());
    }

    #[test]
    fn serialization_round_trips_byte_identically() {
        let summary = JobSummary::from_normalized(normalized()).unwrap();
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: JobSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, summary);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }
}

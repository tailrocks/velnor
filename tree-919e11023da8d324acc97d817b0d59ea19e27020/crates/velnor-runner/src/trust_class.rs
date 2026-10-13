//! Per-job trust class derived from the job's own event, failing closed.
//!
//! The pool trust scope ([`crate::trust_scope`]) is the operator-selected
//! ceiling. This module derives what the *job* is: [`TrustClass`] is computed
//! from the job message alone — the `github.event_name` variable (or, on a raw
//! pre-hydration message, the `github.event_name` context value), the
//! head/base repository comparison (`github.repository` against the event
//! payload's `pull_request.head.repo` or `workflow_run.head_repository`), the
//! self [`RepositoryResource`]'s name and `cloneUrl` property as
//! corroboration, and the plan scope identifier as a
//! structural-completeness signal.
//!
//! Derivation rules:
//!
//! * A missing, empty, or malformed event signal fails closed to
//!   [`TrustClass::Unknown`]: no event name, no base repository, no plan
//!   reference (scope identifier on V1 orchestration messages, plan id on V2
//!   broker messages), no head repository on a pull-request or `workflow_run`
//!   event, an unparseable event payload, or a repository resource that
//!   contradicts the base repository.
//! * Any event whose name starts with `pull_request` (case-insensitive:
//!   `pull_request`, `pull_request_target`, `pull_request_review`, …) is a
//!   pull-request event. Same-repo (`head == base`, compared
//!   case-insensitively like the rest of the runner) derives
//!   [`TrustClass::Trusted`]; a fork derives [`TrustClass::ForkPR`]. A
//!   `pull_request_target` from a fork is [`TrustClass::ForkPR`]: it runs
//!   base-repo code but operates on fork-controlled inputs.
//! * A `workflow_run` event (case-insensitive) compares the triggering run's
//!   head repository (`workflow_run.head_repository.full_name`) against the
//!   base the same way: a fork head derives [`TrustClass::ForkPR`] — base
//!   workflow code over a fork-controlled head sha and repository — and a
//!   missing or malformed head fails closed to [`TrustClass::Unknown`].
//! * Any other well-formed event name derives [`TrustClass::Trusted`], because
//!   GitHub reserves the `pull_request` prefix for pull-request-scoped events
//!   and `workflow_run` is handled above; every remaining event executes
//!   base-repository code.
//! * [`TrustClass::Unknown`] is treated as untrusted: only [`TrustClass::Trusted`]
//!   reports [`TrustClass::is_trusted`].
//!
//! This module derives the class and maps it against the pool ceiling
//! ([`TrustClass::admitted_scope`]). Enforcing the admitted scope — secrets,
//! the Docker socket, and the store namespace a job runs with — happens at
//! admission (`runner.rs`) and in the execution path it threads the scope to.
//!
//! [`RepositoryResource`]: crate::job_message::RepositoryResource

use crate::job_message::AgentJobRequestMessage;
use serde_json::Value;

/// Per-job trust class, derived from the job's own event.
///
/// Constructible only through [`TrustClass::derive`]. Deliberately not
/// `Default`: there is no trust value that is safe to assume, so every
/// construction site must run the derivation and take its fail-closed answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustClass {
    /// Affirmative same-repository evidence: a non-fork-sensitive event with
    /// complete signals, or a pull-request/`workflow_run` event whose head
    /// repository equals the base repository.
    Trusted,
    /// A pull-request or `workflow_run` event whose head repository differs
    /// from the base repository: base-repo code over fork-controlled inputs.
    /// Untrusted.
    ForkPR,
    /// A signal was missing or unparseable, so no class could be affirmed.
    /// Treated as untrusted.
    Unknown,
}

impl TrustClass {
    /// Derive the job's trust class from its event and repository signals.
    ///
    /// Pure over the message: no I/O, no ambient state, no clock. Reads the
    /// `github.*` variables first and falls back to the `github` context
    /// object, so raw pre-hydration messages and hydrated ones classify
    /// identically.
    #[must_use]
    pub fn derive(job: &AgentJobRequestMessage) -> Self {
        let Some(event) = event_name(job) else {
            return Self::Unknown;
        };
        if !plan_scope_present(job) {
            return Self::Unknown;
        }
        let Some(base) = base_repository(job) else {
            return Self::Unknown;
        };
        if is_pull_request_event(event) {
            let Some(pull) = pull_request_repos(job) else {
                return Self::Unknown;
            };
            if !repository_eq(base, &pull.head_full_name) {
                return Self::ForkPR;
            }
            if pull.numeric_ids_contradict() {
                return Self::Unknown;
            }
        } else if is_workflow_run_event(event) {
            let Some(head) = workflow_run_head_repository(job) else {
                return Self::Unknown;
            };
            if !repository_eq(base, &head) {
                return Self::ForkPR;
            }
        }
        if repository_resources_contradict(job, base) {
            return Self::Unknown;
        }
        Self::Trusted
    }

    /// Whether this class grants trusted treatment. Only [`TrustClass::Trusted`]
    /// does; [`TrustClass::ForkPR`] and [`TrustClass::Unknown`] are untrusted.
    #[must_use]
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted)
    }

    /// The trust scope in effect for a job of this class on a pool whose flag
    /// is `pool_scope`.
    ///
    /// The pool flag is a ceiling, never the job's trust: a [`TrustClass::Trusted`]
    /// job keeps the pool value (including `release` and custom scopes), while
    /// [`TrustClass::ForkPR`] and [`TrustClass::Unknown`] fail closed to the
    /// untrusted floor whatever the pool allows. Admission computes this once
    /// per job and threads it down; no per-job path re-derives trust from the
    /// pool flag alone.
    #[must_use]
    pub fn admitted_scope<'a>(&self, pool_scope: &'a str) -> &'a str {
        if self.is_trusted() {
            pool_scope
        } else {
            crate::trust_scope::FAIL_CLOSED
        }
    }

    /// Stable lowercase label for logs and telemetry.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::ForkPR => "fork-pr",
            Self::Unknown => "unknown",
        }
    }
}

/// A job's admitted trust: the derived class bound to the effective scope
/// it narrows the pool ceiling to.
///
/// Constructible only through [`AdmittedTrust::narrow`] or
/// [`AdmittedTrust::admit`], which run the class narrowing over the raw pool
/// flag. The fields are private, so no call site can pair a class with a
/// scope it forbids (`fork-pr` with `trusted`) — the wrong value is
/// inexpressible, not merely untested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedTrust {
    class: TrustClass,
    effective_scope: String,
}

impl AdmittedTrust {
    /// Bind `class` to the scope it admits on a pool whose flag is
    /// `pool_scope`: the ceiling for [`TrustClass::Trusted`], the untrusted
    /// floor for every other class.
    ///
    /// The ceiling is normalized here (`trust_scope::normalize_scope`), so the
    /// bound scope is always in the canonical spelling — the normalization is
    /// part of the binding, not a step a call site can forget.
    #[must_use]
    pub fn narrow(class: TrustClass, pool_scope: &str) -> Self {
        let effective_scope = class
            .admitted_scope(crate::trust_scope::normalize_scope(pool_scope))
            .to_owned();
        Self {
            class,
            effective_scope,
        }
    }

    /// The production admission binding: derive the job's class from its own
    /// event, then narrow the pool ceiling by it. The single entry point both
    /// `handle_job_request` and its conformance test call, so the narrowed
    /// pair the test asserts is the one production persists — never a
    /// hand-assembled pair production cannot produce.
    #[must_use]
    pub fn admit(job: &AgentJobRequestMessage, pool_scope: &str) -> Self {
        Self::narrow(TrustClass::derive(job), pool_scope)
    }

    /// The derived class this binding narrows.
    #[must_use]
    pub fn class(&self) -> TrustClass {
        self.class
    }

    /// The scope the job runs with: the value admission persists and every
    /// enforcement path threads down.
    #[must_use]
    pub fn effective_scope(&self) -> &str {
        &self.effective_scope
    }
}

/// `pull_request` prefix length: `pull_request`, `pull_request_target`,
/// `pull_request_review`, and `pull_request_review_comment` all share it.
const PULL_REQUEST_PREFIX: &[u8; 12] = b"pull_request";

fn is_pull_request_event(event: &str) -> bool {
    event.len() >= PULL_REQUEST_PREFIX.len()
        && event.as_bytes()[..PULL_REQUEST_PREFIX.len()].eq_ignore_ascii_case(PULL_REQUEST_PREFIX)
}

fn is_workflow_run_event(event: &str) -> bool {
    event.eq_ignore_ascii_case("workflow_run")
}

/// The `github.event_name` variable, else the `github.event_name` context
/// value. Well-formed names are ASCII `[A-Za-z0-9_]`; anything else is an
/// unparseable signal, not a non-PR event.
fn event_name(job: &AgentJobRequestMessage) -> Option<&str> {
    let raw = job_variable(job, "github.event_name")
        .or_else(|| context_string(job, "github", "event_name"))?;
    let event = raw.trim();
    if event.is_empty()
        || !event
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(event)
}

/// The base repository full name (`owner/repo`) from `github.repository`,
/// else the `github.repository` context value. Must carry the full-name shape;
/// a bare word is an unparseable signal.
fn base_repository(job: &AgentJobRequestMessage) -> Option<&str> {
    let raw = job_variable(job, "github.repository")
        .or_else(|| context_string(job, "github", "repository"))?;
    normalize_full_name(raw)
}

/// The plan scope identifier must be present and non-blank. It carries no
/// trust content of its own; it proves the message is structurally complete
/// enough to bind the job to its collection scope.
fn plan_scope_present(job: &AgentJobRequestMessage) -> bool {
    // V1 orchestration messages carry a `ScopeIdentifier` GUID alongside the
    // plan id; V2 broker messages carry only the plan id (`Version: 0`,
    // `ScopeIdentifier` absent). Requiring the scope alone classified every
    // real V2 job as `TrustClass::Unknown`, so the whole estate ran under the
    // untrusted floor. The plan reference is complete when either identity is
    // present; a message with neither is not a real plan and stays fail-closed.
    job.plan
        .scope_identifier
        .as_deref()
        .is_some_and(|scope| !scope.trim().is_empty())
        || !job.plan.plan_id.trim().is_empty()
}

/// `owner/repo` with both sides non-empty and no inner whitespace, trimmed.
fn normalize_full_name(raw: &str) -> Option<&str> {
    let name = raw.trim();
    if name.is_empty() || name.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    let (owner, repo) = name.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(name)
}

/// GitHub repository full names compare case-insensitively, matching the
/// runner's existing `github_string_eq` comparison for event `full_name`
/// values (`executor.rs`).
fn repository_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Head and base repository identities read from the `github.event` payload.
struct PullRequestRepos {
    head_full_name: String,
    head_id: Option<String>,
    base_id: Option<String>,
}

impl PullRequestRepos {
    /// Equal full names with both numeric ids present but disagreeing is a
    /// contradictory payload: refuse to affirm trust. A fork verdict never
    /// reaches this check, and both outcomes below it are untrusted, so the
    /// ids can only ever move `Trusted` to `Unknown`, never the reverse.
    fn numeric_ids_contradict(&self) -> bool {
        match (&self.head_id, &self.base_id) {
            (Some(head), Some(base)) => head != base,
            _ => false,
        }
    }
}

/// Run a projection over the `github.event` payload. The `event` value may be
/// an object or a JSON-encoded string (both arrive on the wire; the
/// executor's event-path writer accepts the same two shapes), and any level
/// may use the V2 broker compact `{"d": [{k, v}]}` form.
fn with_github_event<T>(
    job: &AgentJobRequestMessage,
    project: impl FnOnce(&Value) -> Option<T>,
) -> Option<T> {
    let github = job.context_data.get("github")?;
    let event = context_get(github, "event")?;
    let parsed;
    let event = match event {
        Value::String(encoded) => {
            parsed = serde_json::from_str::<Value>(encoded).ok()?;
            &parsed
        }
        value => value,
    };
    project(event)
}

/// `pull_request.head.repo.full_name` plus the corroborating numeric ids from
/// `pull_request.{head,base}.repo.id`.
fn pull_request_repos(job: &AgentJobRequestMessage) -> Option<PullRequestRepos> {
    with_github_event(job, |event| {
        let pull = context_get(event, "pull_request")?;
        let head_repo = context_get(pull, "head").and_then(|head| context_get(head, "repo"))?;
        let head_full_name = context_get(head_repo, "full_name")?.as_str()?;
        let head_full_name = normalize_full_name(head_full_name)?.to_owned();
        let head_id = context_get(head_repo, "id").and_then(json_id);
        let base_id = context_get(pull, "base")
            .and_then(|base| context_get(base, "repo"))
            .and_then(|repo| context_get(repo, "id"))
            .and_then(json_id);
        Some(PullRequestRepos {
            head_full_name,
            head_id,
            base_id,
        })
    })
}

/// `workflow_run.head_repository.full_name`: the repository that owns the head
/// sha the triggering run executed. A `workflow_run` requested by a fork pull
/// request runs this repository's workflow file but checks out and reports on
/// fork-controlled code, so the head/base comparison is the same fork signal
/// as for `pull_request_target` (and the executor's artifact producer gate
/// treats the same two `full_name` values as fork-sensitive).
fn workflow_run_head_repository(job: &AgentJobRequestMessage) -> Option<String> {
    with_github_event(job, |event| {
        let run = context_get(event, "workflow_run")?;
        let head = context_get(run, "head_repository")?;
        let full_name = context_get(head, "full_name")?.as_str()?;
        normalize_full_name(full_name).map(str::to_owned)
    })
}

/// A numeric GitHub id rendered comparably whether the payload encoded it as a
/// JSON number or a string.
fn json_id(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(raw) => {
            let id = raw.trim();
            (!id.is_empty()).then(|| id.to_owned())
        }
        _ => None,
    }
}

/// Whether the self repository resource contradicts the base repository. The
/// resource block is what the runner actually clones, so a name or `cloneUrl`
/// naming a different repository than the event claims fails the derivation
/// closed. An absent resource block is no contradiction: checkout hydrates it
/// from the same `github.*` signals this derivation already required.
fn repository_resources_contradict(job: &AgentJobRequestMessage, base: &str) -> bool {
    let self_repo = job
        .resources
        .repositories
        .iter()
        .find(|repo| repo.alias.as_deref() == Some("self"))
        .or_else(|| job.resources.repositories.first());
    let Some(self_repo) = self_repo else {
        return false;
    };
    let mut corroborated = false;
    if let Some(name) = self_repo
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        corroborated = true;
        if !repository_eq(name, base) {
            return true;
        }
    }
    if let Some(url) = self_repo
        .properties
        .get("cloneUrl")
        .or(self_repo.url.as_ref())
        .map(|url| url.trim())
        .filter(|url| !url.is_empty())
    {
        // Read exactly the way the checkout planner reads it (`checkout.rs`
        // `self_clone_url`): the same signal, the same key, the same fallback.
        match clone_url_repository(url) {
            Some(repo) => {
                corroborated = true;
                if !repository_eq(&repo, base) {
                    return true;
                }
            }
            None => return true,
        }
    }
    !corroborated
}

/// `owner/repo` parsed from an `https://host/owner/repo(.git)` clone URL. The
/// last two non-empty path segments are the identity; anything else shaped is
/// unparseable.
fn clone_url_repository(url: &str) -> Option<String> {
    let after_scheme = url.rsplit("://").next()?;
    let without_host = after_scheme.split_once('/')?.1;
    let mut segments: Vec<&str> = without_host
        .split('/')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let repo = segments.pop()?;
    let owner = segments.pop()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let full_name = format!("{owner}/{repo}");
    normalize_full_name(&full_name).map(str::to_owned)
}

fn job_variable<'job>(job: &'job AgentJobRequestMessage, name: &str) -> Option<&'job str> {
    job.variables
        .get(name)
        .and_then(|variable| variable.value.as_deref())
}

/// A string member of a top-level context object, for raw messages whose
/// `github.*` variables were never hydrated from `ContextData`.
fn context_string<'job>(
    job: &'job AgentJobRequestMessage,
    object: &str,
    key: &str,
) -> Option<&'job str> {
    let value = job.context_data.get(object)?;
    context_get(value, key)?.as_str()
}

/// One-level context lookup that understands both the plain object form and
/// the V2 broker compact `{"d": [{k, v}]}` form.
fn context_get<'value>(value: &'value Value, key: &str) -> Option<&'value Value> {
    match value {
        Value::Object(object) => {
            if let Some(hit) = object.get(key) {
                return Some(hit);
            }
            object.get("d").and_then(Value::as_array).and_then(|items| {
                items.iter().find_map(|item| {
                    let entry = item.as_object()?;
                    (entry.get("k").and_then(Value::as_str) == Some(key)).then(|| entry.get("v"))?
                })
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A job message assembled from its trust signals: `github.*` variables,
    /// the raw `github` context object, the repository resource block, and the
    /// plan scope. `None` omits that signal entirely.
    fn signal_job(
        variables: serde_json::Value,
        github_context: Option<serde_json::Value>,
        repositories: serde_json::Value,
        scope_identifier: Option<&str>,
    ) -> AgentJobRequestMessage {
        let mut context_data = serde_json::Map::new();
        if let Some(github) = github_context {
            context_data.insert("github".to_string(), github);
        }
        let plan = match scope_identifier {
            Some(scope) => json!({ "planId": "plan", "scopeIdentifier": scope }),
            None => json!({ "planId": "plan" }),
        };
        serde_json::from_value(json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": plan,
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "job",
            "requestId": 1,
            "variables": variables,
            "contextData": context_data,
            "resources": { "repositories": repositories },
        }))
        .expect("trust test job parses")
    }

    fn variables(event: &str, repository: &str) -> serde_json::Value {
        json!({
            "github.event_name": { "value": event },
            "github.repository": { "value": repository },
        })
    }

    fn self_repository(name: &str) -> serde_json::Value {
        json!([{
            "alias": "self",
            "name": name,
            "properties": { "cloneUrl": format!("https://github.com/{name}.git") },
        }])
    }

    fn pull_request_event(head_full_name: &str, head_id: u64, base_id: u64) -> serde_json::Value {
        json!({
            "pull_request": {
                "head": { "repo": { "full_name": head_full_name, "id": head_id } },
                "base": { "repo": { "id": base_id } },
            }
        })
    }

    fn workflow_run_event(head_full_name: &str) -> serde_json::Value {
        json!({
            "workflow_run": {
                "head_repository": { "full_name": head_full_name },
            }
        })
    }

    /// The trusted baseline every fail-closed regression test mutates by
    /// exactly one signal: a `push` job with complete variables, resources,
    /// and plan scope.
    fn trusted_baseline() -> serde_json::Value {
        json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan", "scopeIdentifier": "scope" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "job",
            "requestId": 1,
            "variables": variables("push", "octo/base"),
            "contextData": {},
            "resources": { "repositories": self_repository("octo/base") },
        })
    }

    fn derive_json(body: serde_json::Value) -> TrustClass {
        let job: AgentJobRequestMessage =
            serde_json::from_value(body).expect("trust test job parses");
        TrustClass::derive(&job)
    }

    // Conformance: the classification GitHub's event semantics require.

    #[test]
    fn trust_class_conformance_push_is_trusted() {
        let job = signal_job(
            variables("push", "octo/base"),
            None,
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_non_pr_events_are_trusted() {
        // `workflow_run` is deliberately absent: it carries a fork-controlled
        // head and takes the payload-analysis path below.
        for event in [
            "workflow_dispatch",
            "schedule",
            "release",
            "merge_group",
            "workflow_call",
            "issue_comment",
        ] {
            let job = signal_job(
                variables(event, "octo/base"),
                None,
                self_repository("octo/base"),
                Some("scope"),
            );
            assert_eq!(
                TrustClass::derive(&job),
                TrustClass::Trusted,
                "event {event} executes base-repository code"
            );
        }
    }

    #[test]
    fn trust_class_conformance_same_repo_workflow_run_is_trusted() {
        let job = signal_job(
            variables("workflow_run", "octo/base"),
            Some(json!({
                "event": workflow_run_event("octo/base"),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_fork_workflow_run_is_fork_pr() {
        // A `workflow_run` requested by a fork PR runs base workflow code over
        // a fork-controlled head sha and repository: the same shape as a fork
        // `pull_request_target`.
        let job = signal_job(
            variables("workflow_run", "octo/base"),
            Some(json!({
                "event": workflow_run_event("mallory/base"),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_workflow_run_detection_is_case_insensitive() {
        let job = signal_job(
            variables("Workflow_Run", "octo/base"),
            Some(json!({ "event": workflow_run_event("mallory/base") })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_workflow_run_head_compares_case_insensitively() {
        let job = signal_job(
            variables("workflow_run", "Octo/Base"),
            Some(json!({ "event": workflow_run_event("octo/base") })),
            self_repository("OCTO/BASE"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_workflow_run_string_event_payload_classifies() {
        let encoded = serde_json::to_string(&workflow_run_event("mallory/base"))
            .expect("event payload encodes");
        let job = signal_job(
            variables("workflow_run", "octo/base"),
            Some(json!({ "event": encoded })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_workflow_run_compact_context_classifies() {
        let job = signal_job(
            json!({}),
            Some(json!({
                "d": [
                    { "k": "event_name", "v": "workflow_run" },
                    { "k": "repository", "v": "octo/base" },
                    { "k": "event", "v": {
                        "d": [
                            { "k": "workflow_run", "v": {
                                "d": [{ "k": "head_repository", "v": {
                                    "d": [{ "k": "full_name", "v": "mallory/base" }],
                                } }],
                            } },
                        ],
                    } },
                ],
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_same_repo_pull_request_is_trusted() {
        let job = signal_job(
            variables("pull_request", "octo/base"),
            Some(json!({
                "event_name": "pull_request",
                "repository": "octo/base",
                "event": pull_request_event("octo/base", 1, 1),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_fork_pull_request_is_fork_pr() {
        let job = signal_job(
            variables("pull_request", "octo/base"),
            Some(json!({
                "event": pull_request_event("mallory/base", 2, 1),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_fork_pull_request_target_is_fork_pr() {
        // `pull_request_target` runs base-repo code but operates on
        // fork-controlled inputs, so the fork class still applies.
        let job = signal_job(
            variables("pull_request_target", "octo/base"),
            Some(json!({
                "event": pull_request_event("mallory/base", 2, 1),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_same_repo_pull_request_target_is_trusted() {
        let job = signal_job(
            variables("pull_request_target", "octo/base"),
            Some(json!({
                "event": pull_request_event("octo/base", 1, 1),
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_pull_request_review_events_use_the_head() {
        for event in ["pull_request_review", "pull_request_review_comment"] {
            let fork = signal_job(
                variables(event, "octo/base"),
                Some(json!({ "event": pull_request_event("mallory/base", 2, 1) })),
                self_repository("octo/base"),
                Some("scope"),
            );
            assert_eq!(
                TrustClass::derive(&fork),
                TrustClass::ForkPR,
                "event {event}"
            );
            let same_repo = signal_job(
                variables(event, "octo/base"),
                Some(json!({ "event": pull_request_event("octo/base", 1, 1) })),
                self_repository("octo/base"),
                Some("scope"),
            );
            assert_eq!(
                TrustClass::derive(&same_repo),
                TrustClass::Trusted,
                "event {event}"
            );
        }
    }

    #[test]
    fn trust_class_conformance_pr_detection_is_case_insensitive() {
        // A mixed-case `Pull_Request` must still take the fork-analysis path,
        // never slip through as a trusted non-PR event.
        let job = signal_job(
            variables("Pull_Request", "octo/base"),
            Some(json!({ "event": pull_request_event("mallory/base", 2, 1) })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_repository_names_compare_case_insensitively() {
        let job = signal_job(
            variables("pull_request", "Octo/Base"),
            Some(json!({ "event": pull_request_event("octo/base", 1, 1) })),
            self_repository("OCTO/BASE"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_compact_context_classifies() {
        // V2 broker compact `{"d": [{k, v}]}` github context carries the same
        // signals as the plain object.
        let job = signal_job(
            json!({}),
            Some(json!({
                "d": [
                    { "k": "event_name", "v": "pull_request" },
                    { "k": "repository", "v": "octo/base" },
                    { "k": "event", "v": {
                        "d": [
                            { "k": "pull_request", "v": {
                                "d": [
                                    { "k": "head", "v": {
                                        "d": [{ "k": "repo", "v": {
                                            "d": [
                                                { "k": "full_name", "v": "mallory/base" },
                                                { "k": "id", "v": 2 },
                                            ],
                                        } }],
                                    } },
                                    { "k": "base", "v": {
                                        "d": [{ "k": "repo", "v": {
                                            "d": [{ "k": "id", "v": 1 }],
                                        } }],
                                    } },
                                ],
                            } },
                        ],
                    } },
                ],
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_string_event_payload_classifies() {
        // The `event` value may arrive JSON-encoded as a string.
        let encoded = serde_json::to_string(&pull_request_event("mallory/base", 2, 1))
            .expect("event payload encodes");
        let job = signal_job(
            variables("pull_request", "octo/base"),
            Some(json!({ "event": encoded })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::ForkPR);
    }

    #[test]
    fn trust_class_conformance_pre_hydration_message_classifies() {
        // Raw messages carry the signals in `ContextData` before the runner
        // hydrates them into `github.*` variables; both read identically.
        let job = signal_job(
            json!({}),
            Some(json!({
                "event_name": "push",
                "repository": "octo/base",
            })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_absent_resources_are_no_contradiction() {
        // Checkout hydrates a missing resource block from the same `github.*`
        // signals the derivation already required, so absence alone cannot
        // fail it closed.
        let job = signal_job(
            variables("push", "octo/base"),
            None,
            json!([]),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&job), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_conformance_only_trusted_is_trusted() {
        assert!(TrustClass::Trusted.is_trusted());
        assert!(!TrustClass::ForkPR.is_trusted());
        assert!(!TrustClass::Unknown.is_trusted());
        assert_eq!(TrustClass::Trusted.as_str(), "trusted");
        assert_eq!(TrustClass::ForkPR.as_str(), "fork-pr");
        assert_eq!(TrustClass::Unknown.as_str(), "unknown");
    }

    #[test]
    fn trust_class_conformance_pool_flag_is_a_ceiling() {
        // A trusted job keeps the pool value untouched — the ceiling,
        // including non-docker scopes like `release` and custom pool names.
        for pool in [
            "trusted",
            " trusted ",
            "release",
            "public-forks",
            "untrusted",
        ] {
            assert_eq!(TrustClass::Trusted.admitted_scope(pool), pool);
        }
    }

    #[test]
    fn trust_class_conformance_fork_and_unknown_fail_to_the_untrusted_floor() {
        // Any other class fails closed to the untrusted floor whatever the
        // pool allows: the pool flag can never grant a job trust.
        for class in [TrustClass::ForkPR, TrustClass::Unknown] {
            for pool in ["trusted", "release", "public-forks", "untrusted"] {
                assert_eq!(
                    class.admitted_scope(pool),
                    crate::trust_scope::FAIL_CLOSED,
                    "{class:?} on pool {pool:?}"
                );
            }
        }
    }

    #[test]
    fn admitted_trust_conformance_narrow_binds_class_to_effective_scope() {
        // The constructor production and every test build admissions through:
        // the class is preserved and the scope is the class narrowed over
        // the raw pool flag. No other (class, scope) pair is constructible.
        for (class, pool, scope) in [
            (TrustClass::Trusted, "trusted", "trusted"),
            (TrustClass::Trusted, "release", "release"),
            (TrustClass::Trusted, "public-forks", "public-forks"),
            (TrustClass::ForkPR, "trusted", "untrusted"),
            (TrustClass::ForkPR, "release", "untrusted"),
            (TrustClass::Unknown, "trusted", "untrusted"),
            (TrustClass::Unknown, "public-forks", "untrusted"),
        ] {
            let trust = AdmittedTrust::narrow(class, pool);
            assert_eq!(trust.class(), class, "{class:?} on pool {pool:?}");
            assert_eq!(trust.effective_scope(), scope, "{class:?} on pool {pool:?}");
        }
    }

    // Regression: each test mutates exactly one signal of the trusted
    // baseline (or one signal of a fork-PR job) and proves the derivation
    // fails closed to Unknown rather than affirming either class.

    #[test]
    fn trust_class_regression_missing_event_name_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"] = json!({ "github.repository": { "value": "octo/base" } });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_blank_event_name_is_unknown() {
        for event in ["", "   "] {
            let mut baseline = trusted_baseline();
            baseline["variables"]["github.event_name"]["value"] = json!(event);
            assert_eq!(derive_json(baseline), TrustClass::Unknown);
        }
    }

    #[test]
    fn trust_class_regression_malformed_event_name_is_unknown() {
        for event in [
            "pull request",
            "pu\nsh",
            "püsh",
            "push;true",
            "pull_request-1",
        ] {
            let mut baseline = trusted_baseline();
            baseline["variables"]["github.event_name"]["value"] = json!(event);
            assert_eq!(
                derive_json(baseline),
                TrustClass::Unknown,
                "event name {event:?} is not parseable"
            );
        }
    }

    #[test]
    fn trust_class_regression_missing_base_repository_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"] = json!({ "github.event_name": { "value": "push" } });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_malformed_base_repository_is_unknown() {
        for repository in [
            "",
            "   ",
            "bareword",
            "/repo",
            "owner/",
            "a/b/c",
            "owner/re po",
        ] {
            let mut baseline = trusted_baseline();
            baseline["variables"]["github.repository"]["value"] = json!(repository);
            assert_eq!(
                derive_json(baseline),
                TrustClass::Unknown,
                "repository {repository:?} has no full-name shape"
            );
        }
    }

    #[test]
    fn trust_class_regression_v2_plan_without_scope_identifier_is_trusted() {
        // Real V2 broker messages carry `Version: 0` and no
        // `ScopeIdentifier`; the plan id is the only plan identity. Captured
        // from a live workflow_dispatch job message.
        let mut baseline = trusted_baseline();
        baseline["plan"] = json!({ "planId": "559b5dd3-cee0-47e3-9096-994b510ae4c1", "scopeIdentifier": null, "version": 0 });
        assert_eq!(derive_json(baseline), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_regression_missing_plan_scope_is_unknown() {
        // `PlanId` is schema-required, so the only expressible "no plan
        // reference" message is a blank plan id beside an absent scope.
        let mut baseline = trusted_baseline();
        baseline["plan"] = json!({ "planId": "   ", "scopeIdentifier": null });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_pr_without_event_payload_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"]["github.event_name"]["value"] = json!("pull_request");
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_pr_with_unparseable_string_event_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"]["github.event_name"]["value"] = json!("pull_request");
        baseline["contextData"] = json!({ "github": { "event": "{not json" } });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_pr_without_head_full_name_is_unknown() {
        for event in [
            json!({ "pull_request": { "head": { "repo": { "id": 2 } } } }),
            json!({ "pull_request": { "head": { "repo": { "full_name": "" } } } }),
            json!({ "pull_request": { "head": { "repo": { "full_name": "bareword" } } } }),
            json!({ "pull_request": {} }),
            json!({}),
        ] {
            let mut baseline = trusted_baseline();
            baseline["variables"]["github.event_name"]["value"] = json!("pull_request");
            baseline["contextData"] = json!({ "github": { "event": event } });
            assert_eq!(derive_json(baseline), TrustClass::Unknown);
        }
    }

    #[test]
    fn trust_class_regression_workflow_run_without_event_payload_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"]["github.event_name"]["value"] = json!("workflow_run");
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_workflow_run_with_unparseable_string_event_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["variables"]["github.event_name"]["value"] = json!("workflow_run");
        baseline["contextData"] = json!({ "github": { "event": "{not json" } });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_workflow_run_without_head_full_name_is_unknown() {
        for event in [
            json!({ "workflow_run": { "head_repository": { "id": 2 } } }),
            json!({ "workflow_run": { "head_repository": { "full_name": "" } } }),
            json!({ "workflow_run": { "head_repository": { "full_name": "bareword" } } }),
            json!({ "workflow_run": {} }),
            json!({}),
        ] {
            let mut baseline = trusted_baseline();
            baseline["variables"]["github.event_name"]["value"] = json!("workflow_run");
            baseline["contextData"] = json!({ "github": { "event": event } });
            assert_eq!(derive_json(baseline), TrustClass::Unknown);
        }
    }

    #[test]
    fn trust_class_regression_contradictory_numeric_ids_are_unknown() {
        // Equal full names with both numeric ids present but disagreeing is a
        // contradictory payload: trust cannot be affirmed.
        let mut baseline = trusted_baseline();
        baseline["variables"]["github.event_name"]["value"] = json!("pull_request");
        baseline["contextData"] =
            json!({ "github": { "event": pull_request_event("octo/base", 2, 1) } });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_contradictory_repository_name_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0]["name"] = json!("mallory/base");
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_contradictory_clone_url_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0]["properties"]["cloneUrl"] =
            json!("https://github.com/mallory/base.git");
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_unparseable_clone_url_is_unknown() {
        for clone_url in [
            "https://github.com/onlyone.git",
            "https://github.com/",
            "not a url at all",
            "https://github.com/a/b/c",
        ] {
            let mut baseline = trusted_baseline();
            baseline["resources"]["repositories"][0]["name"] = serde_json::Value::Null;
            baseline["resources"]["repositories"][0]["properties"]["cloneUrl"] = json!(clone_url);
            assert_eq!(
                derive_json(baseline),
                TrustClass::Unknown,
                "clone URL {clone_url:?} names no repository"
            );
        }
    }

    #[test]
    fn trust_class_regression_identity_less_resource_is_unknown() {
        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0] = json!({ "alias": "self" });
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    #[test]
    fn trust_class_regression_clone_url_without_git_suffix_corroborates() {
        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0]["name"] = serde_json::Value::Null;
        baseline["resources"]["repositories"][0]["properties"]["cloneUrl"] =
            json!("https://github.com/octo/base");
        assert_eq!(derive_json(baseline), TrustClass::Trusted);
    }

    #[test]
    fn trust_class_regression_first_repository_corroborates_without_self_alias() {
        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0]["alias"] = serde_json::Value::Null;
        assert_eq!(derive_json(baseline), TrustClass::Trusted);

        let mut baseline = trusted_baseline();
        baseline["resources"]["repositories"][0]["alias"] = serde_json::Value::Null;
        baseline["resources"]["repositories"][0]["name"] = json!("mallory/base");
        assert_eq!(derive_json(baseline), TrustClass::Unknown);
    }

    // Benchmark: derivation sits on the admission path, so its cost is gated.
    // No criterion/divan harness exists in this workspace; the bound below is
    // the benchmark, in the style of the repo's existing timing-gated tests.

    #[test]
    fn trust_class_benchmark_derivation_throughput() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        let push = signal_job(
            variables("push", "octo/base"),
            None,
            self_repository("octo/base"),
            Some("scope"),
        );
        let fork_pr = signal_job(
            variables("pull_request", "octo/base"),
            Some(json!({ "event": pull_request_event("mallory/base", 2, 1) })),
            self_repository("octo/base"),
            Some("scope"),
        );
        let fork_workflow_run = signal_job(
            variables("workflow_run", "octo/base"),
            Some(json!({ "event": workflow_run_event("mallory/base") })),
            self_repository("octo/base"),
            Some("scope"),
        );
        assert_eq!(TrustClass::derive(&push), TrustClass::Trusted);
        assert_eq!(TrustClass::derive(&fork_pr), TrustClass::ForkPR);
        assert_eq!(TrustClass::derive(&fork_workflow_run), TrustClass::ForkPR);

        // 20k derivations per class; a pure JSON walk must stay far below the
        // bound even on a loaded serial-gate runner.
        let started = Instant::now();
        for _ in 0..20_000 {
            assert_eq!(TrustClass::derive(black_box(&push)), TrustClass::Trusted);
            assert_eq!(TrustClass::derive(black_box(&fork_pr)), TrustClass::ForkPR);
            assert_eq!(
                TrustClass::derive(black_box(&fork_workflow_run)),
                TrustClass::ForkPR
            );
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "60k trust derivations took {elapsed:?}",
        );
    }
}

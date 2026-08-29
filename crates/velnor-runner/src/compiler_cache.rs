//! Local compiler-cache backend selection and wrapper ownership.
//!
//! Velnor owns the compiler wrapper for admitted jobs: caches are local,
//! runner-mounted stores, never workflow-controlled remote services. The
//! backend for a job is resolved here from the acceleration policy plus the
//! workflow's explicit setup action. Two invariants: a cache is never
//! silently disabled (a disabled cache is a recorded degradation), and a
//! workflow that fights the wrapper is an admission failure — never a silent
//! override in either direction.

use serde_json::Value;

use crate::acceleration::{AccelerationPolicy, DegradationLog, DegradationRecord};
use crate::job_message::AgentJobRequestMessage;
use crate::manifest::{CapabilityViolation, MANIFEST_VERSION};

/// Kache binary version the job image ships and the native adapter pins via
/// `kache --version`. The `kunobi-ninja/kache-action` step is a marker only;
/// the binary always comes from the image, never a download.
pub const KACHE_BINARY_VERSION: &str = "0.16.0";

/// Default bound of the daemon-shared local compiler-cache store
/// (`KACHE_MAX_SIZE` / the store budget until that workstream lands).
pub const DEFAULT_COMPILER_CACHE_MAX_SIZE: &str = "20GiB";

/// Container path of kache's per-job runtime state (socket, locks, logs).
/// It lives under the `/__t` job-temp mount so concurrent jobs never share a
/// socket or lock file; cache *data* stays in the shared `/var/cache/kache`
/// bind. `KACHE_SERVICE` is not part of kache's env contract and is never set.
pub const KACHE_RUNTIME_DIR_CONTAINER: &str = "/__t/kache-runtime";

/// A local compiler-cache backend. [`Self::Auto`] is a policy value only —
/// the acceleration policy defers to per-job resolution — and can never be a
/// resolved backend for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerCacheBackend {
    /// Policy value: resolve per job (Rust jobs get kache, others none).
    Auto,
    Sccache,
    Kache,
    Off,
}

impl CompilerCacheBackend {
    /// Operator-facing name, also the `[acceleration] compiler_cache` and
    /// `VELNOR_ACCELERATION_COMPILER_CACHE` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sccache => "sccache",
            Self::Kache => "kache",
            Self::Off => "off",
        }
    }

    /// Parse an override/policy choice. Unknown values are `None` so callers
    /// fail closed with the accepted set instead of guessing a backend.
    #[must_use]
    pub fn parse_choice(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "kache" => Some(Self::Kache),
            "sccache" => Some(Self::Sccache),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// The `RUSTC_WRAPPER` binary Velnor exports for this backend. `None` for
    /// [`Self::Auto`] (never resolved) and [`Self::Off`] (Velnor sets no
    /// wrapper, so a workflow-provided one stands).
    #[must_use]
    pub fn wrapper_name(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Sccache => Some("sccache"),
            Self::Kache => Some("kache"),
            Self::Off => None,
        }
    }
}

/// Why a backend was selected for a job. Default is [`Self::Off`]: a fresh
/// job state asserts nothing until resolution runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CompilerCacheOrigin {
    /// The workflow ran the setup action (kache-action / sccache-action).
    WorkflowAction,
    /// Policy `auto` engaged the backend from the job's own shape.
    PolicyAuto,
    /// Policy pinned the backend explicitly (no workflow action).
    PolicyExplicit,
    /// No backend: not applicable, or policy-disabled (see degradation log).
    #[default]
    Off,
}

impl CompilerCacheOrigin {
    /// Machine-readable origin for the job acceleration report.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowAction => "workflow-action",
            Self::PolicyAuto => "policy-auto",
            Self::PolicyExplicit => "policy-explicit",
            Self::Off => "off",
        }
    }
}

/// The compiler-cache decision for one job: the concrete backend (never
/// [`CompilerCacheBackend::Auto`]), why it was chosen, and every degradation
/// recorded on the way there (policy env overrides included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBackend {
    pub backend: CompilerCacheBackend,
    pub origin: CompilerCacheOrigin,
    pub degradation: DegradationLog,
}

impl ResolvedBackend {
    /// A clean selection with no degradations (workflow action or explicit
    /// policy pin).
    #[must_use]
    pub fn selected(backend: CompilerCacheBackend, origin: CompilerCacheOrigin) -> Self {
        Self {
            backend,
            origin,
            degradation: DegradationLog::default(),
        }
    }

    /// No backend, no degradation (not applicable).
    #[must_use]
    pub fn off() -> Self {
        Self::selected(CompilerCacheBackend::Off, CompilerCacheOrigin::Off)
    }
}

/// Resolve the compiler-cache backend for one job.
///
/// Precedence: the workflow's explicit setup action wins over the policy
/// (workflows keep their declared tool); without an action, policy `auto`
/// engages kache for Rust-compiling jobs and nothing for the rest; policy
/// `off` degrades with a record instead of disabling silently. Mixed
/// wrappers are rejected by admission; resolution still degrades visibly
/// instead of guessing, so no bypass of admission can mount both.
#[must_use]
pub fn resolve_backend(
    policy: &AccelerationPolicy,
    job: &AgentJobRequestMessage,
) -> ResolvedBackend {
    let mut degradation = DegradationLog::default();
    degradation.extend(policy.degradations.iter().cloned());
    let mut sccache = false;
    let mut kache = false;
    for step in job.steps.iter().filter(|step| step.enabled) {
        let repository = step
            .reference
            .as_ref()
            .and_then(|reference| reference.name.as_deref());
        sccache |= repository
            .is_some_and(|name| name.eq_ignore_ascii_case("mozilla-actions/sccache-action"));
        kache |=
            repository.is_some_and(|name| name.eq_ignore_ascii_case("kunobi-ninja/kache-action"));
    }
    let resolved = if sccache && kache {
        degradation.record(DegradationRecord::unresolved(
            "acceleration.compiler_cache",
            "mixed cache wrappers resolved to none",
            "workflow declares both sccache-action and kache-action; admission rejects this shape",
        ));
        ResolvedBackend::off()
    } else if kache {
        ResolvedBackend::selected(
            CompilerCacheBackend::Kache,
            CompilerCacheOrigin::WorkflowAction,
        )
    } else if sccache {
        ResolvedBackend::selected(
            CompilerCacheBackend::Sccache,
            CompilerCacheOrigin::WorkflowAction,
        )
    } else {
        match policy.compiler_cache {
            CompilerCacheBackend::Auto => {
                if job_compiles_rust(job) {
                    ResolvedBackend::selected(
                        CompilerCacheBackend::Kache,
                        CompilerCacheOrigin::PolicyAuto,
                    )
                } else {
                    ResolvedBackend::off()
                }
            }
            CompilerCacheBackend::Kache => ResolvedBackend::selected(
                CompilerCacheBackend::Kache,
                CompilerCacheOrigin::PolicyExplicit,
            ),
            CompilerCacheBackend::Sccache => ResolvedBackend::selected(
                CompilerCacheBackend::Sccache,
                CompilerCacheOrigin::PolicyExplicit,
            ),
            CompilerCacheBackend::Off => {
                degradation.record(DegradationRecord::compiler_cache_disabled(
                    "policy [acceleration] compiler_cache = \"off\"",
                ));
                ResolvedBackend::off()
            }
        }
    };
    ResolvedBackend {
        degradation,
        ..resolved
    }
}

/// Whether any enabled run step compiles Rust: the auto policy engages a
/// compiler cache only for jobs that would otherwise pay full rustc cost.
/// Marker words cover the estate's cargo/rustc/nextest/clippy invocations.
#[must_use]
pub fn job_compiles_rust(job: &AgentJobRequestMessage) -> bool {
    job.steps
        .iter()
        .filter(|step| step.enabled)
        .any(|step| step_script(step).is_some_and(rust_marker_in))
}

fn rust_marker_in(script: &str) -> bool {
    let lowered = script.to_ascii_lowercase();
    ["cargo", "rustc", "nextest", "clippy"]
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn step_script(step: &crate::job_message::ActionStep) -> Option<&str> {
    let inputs = step.inputs.as_ref()?.as_object()?;
    let raw = inputs
        .iter()
        .find_map(|(key, value)| key.eq_ignore_ascii_case("script").then_some(value))?;
    raw.as_str().or_else(|| {
        raw.as_object()
            .and_then(|object| object.get("lit").or_else(|| object.get("Lit")))
            .and_then(Value::as_str)
    })
}

/// Admission check: a workflow may not set `RUSTC_WRAPPER` itself while
/// Velnor resolves a cache backend for the job. A foreign wrapper would
/// either bypass the local store or point rustc at a missing binary, so the
/// conflict fails visibly with the exact step, value, and fix — never a
/// silent disable of either side.
pub fn validate_wrapper_ownership(
    job: &AgentJobRequestMessage,
    policy: &AccelerationPolicy,
    violations: &mut Vec<CapabilityViolation>,
) {
    let resolved = resolve_backend(policy, job);
    let Some(expected) = resolved.backend.wrapper_name() else {
        return;
    };
    for (step, name, value) in workflow_environment_entries(job) {
        if !name.eq_ignore_ascii_case("RUSTC_WRAPPER") || value.eq_ignore_ascii_case(expected) {
            continue;
        }
        violations.push(CapabilityViolation {
            step,
            repository: "velnor/compiler-cache".to_string(),
            action_ref: "compiler-cache.ownership".to_string(),
            field: "env.RUSTC_WRAPPER".to_string(),
            received: value.clone(),
            accepted: vec![format!(
                "absent or '{expected}': step sets RUSTC_WRAPPER='{value}' while Velnor \
                 resolves the {} backend and exports RUSTC_WRAPPER itself; remove the \
                 variable from the step/job env",
                resolved.backend.as_str()
            )],
            manifest_version: MANIFEST_VERSION,
        });
    }
}

/// Every workflow-provided env entry as `(step label, name, value)` from the
/// job-level env blocks and each enabled step's env. Mirrors the name
/// collection in `manifest::validate_compiler_cache_topology` but keeps the
/// literal value so ownership checks can quote the offender.
fn workflow_environment_entries(job: &AgentJobRequestMessage) -> Vec<(String, String, String)> {
    let mut entries = Vec::new();
    for value in &job.environment_variables {
        collect_environment_entries(value, "job env", &mut entries);
    }
    for (index, step) in job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.enabled)
    {
        let Some(environment) = &step.environment else {
            continue;
        };
        let label = step
            .display_name_template()
            .or_else(|| step.name.clone())
            .unwrap_or_else(|| format!("step-{index}"));
        collect_environment_entries(environment, &label, &mut entries);
    }
    entries
}

fn collect_environment_entries(
    value: &Value,
    label: &str,
    entries: &mut Vec<(String, String, String)>,
) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("Key")
                .or_else(|| object.get("key"))
                .and_then(crate::manifest::template_literal)
            {
                if let Some(rendered) = object
                    .get("Value")
                    .or_else(|| object.get("value"))
                    .and_then(crate::manifest::template_literal)
                {
                    entries.push((label.to_string(), name.to_string(), rendered.to_string()));
                }
            } else {
                for (name, raw) in object {
                    if matches!(name.as_str(), "type" | "Type" | "map" | "Map") {
                        continue;
                    }
                    if let Some(rendered) = crate::manifest::template_literal(raw) {
                        entries.push((label.to_string(), name.clone(), rendered.to_string()));
                    }
                }
            }
            for nested in object.get("map").or_else(|| object.get("Map")).into_iter() {
                collect_environment_entries(nested, label, entries);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_environment_entries(nested, label, entries);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_job() -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Rust / check",
            "requestId": 1,
            "steps": [
                { "reference": { "type": "Repository", "name": "actions/checkout" } },
                {
                    "reference": { "type": "Script" },
                    "inputs": { "script": "cargo nextest run --profile ci" }
                }
            ]
        }))
        .unwrap()
    }

    fn non_rust_job() -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "JS / test",
            "requestId": 1,
            "steps": [
                {
                    "reference": { "type": "Script" },
                    "inputs": { "script": "npm ci && npm test" }
                }
            ]
        }))
        .unwrap()
    }

    fn kache_action_job() -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Rust / check",
            "requestId": 1,
            "steps": [
                {
                    "reference": {
                        "type": "Repository",
                        "name": "kunobi-ninja/kache-action",
                        "ref": "49398d37113c616fdb61be434cb497e3c2c8f3e6"
                    },
                    "inputs": { "version": "v0.16.0", "github-cache": "false" }
                },
                {
                    "reference": { "type": "Script" },
                    "inputs": { "script": "cargo clippy -- -D warnings" }
                }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn auto_policy_resolves_kache_for_rust_jobs_without_an_action() {
        let resolved = resolve_backend(&AccelerationPolicy::maximum(), &rust_job());
        assert_eq!(resolved.backend, CompilerCacheBackend::Kache);
        assert_eq!(resolved.origin, CompilerCacheOrigin::PolicyAuto);
        assert!(resolved.degradation.is_empty());
    }

    #[test]
    fn auto_policy_resolves_off_for_non_rust_jobs_without_degradation() {
        let resolved = resolve_backend(&AccelerationPolicy::maximum(), &non_rust_job());
        assert_eq!(resolved.backend, CompilerCacheBackend::Off);
        assert_eq!(resolved.origin, CompilerCacheOrigin::Off);
        assert!(
            resolved.degradation.is_empty(),
            "not-applicable is not a degradation"
        );
    }

    #[test]
    fn explicit_workflow_action_wins_over_policy() {
        let resolved = resolve_backend(&AccelerationPolicy::maximum(), &kache_action_job());
        assert_eq!(resolved.backend, CompilerCacheBackend::Kache);
        assert_eq!(resolved.origin, CompilerCacheOrigin::WorkflowAction);

        let mut off_policy = AccelerationPolicy::maximum();
        off_policy.compiler_cache = CompilerCacheBackend::Off;
        let resolved = resolve_backend(&off_policy, &kache_action_job());
        assert_eq!(
            resolved.backend,
            CompilerCacheBackend::Kache,
            "the workflow's declared tool survives an operator-wide disable"
        );
        assert!(resolved.degradation.is_empty());
    }

    #[test]
    fn explicit_policy_off_yields_a_degradation_record_never_silence() {
        let mut policy = AccelerationPolicy::maximum();
        policy.compiler_cache = CompilerCacheBackend::Off;
        let resolved = resolve_backend(&policy, &rust_job());
        assert_eq!(resolved.backend, CompilerCacheBackend::Off);
        let records = resolved.degradation.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].feature, "acceleration.compiler_cache");
        assert!(records[0].reason.contains("cold"));
        assert!(records[0].restore_hint.contains("auto"));
    }

    #[test]
    fn policy_env_override_records_travel_with_the_resolution() {
        let mut policy = AccelerationPolicy::maximum();
        policy.apply_overrides(None, Some("off".to_string()));
        let resolved = resolve_backend(&policy, &rust_job());
        let records = resolved.degradation.records();
        assert_eq!(records.len(), 2, "{records:?}");
        assert!(records[0]
            .cause
            .contains("VELNOR_ACCELERATION_COMPILER_CACHE"));
        assert_eq!(records[1].feature, "acceleration.compiler_cache");
        assert_eq!(resolved.backend, CompilerCacheBackend::Off);
    }

    #[test]
    fn mixed_wrappers_resolve_to_off_with_a_record() {
        let mut job = kache_action_job();
        let sccache_step =
            serde_json::from_value::<crate::job_message::ActionStep>(serde_json::json!({
                "reference": {
                    "type": "Repository",
                    "name": "mozilla-actions/sccache-action",
                    "ref": "9e7fa8a12102821edf02ca5dbea1acd0f89a2696"
                }
            }))
            .unwrap();
        job.steps.push(sccache_step);
        let resolved = resolve_backend(&AccelerationPolicy::maximum(), &job);
        assert_eq!(resolved.backend, CompilerCacheBackend::Off);
        assert!(resolved
            .degradation
            .records()
            .iter()
            .any(|record| record.reason.contains("mixed")));
    }

    #[test]
    fn explicit_policy_sccache_resolves_without_an_action() {
        let mut policy = AccelerationPolicy::maximum();
        policy.compiler_cache = CompilerCacheBackend::Sccache;
        let resolved = resolve_backend(&policy, &rust_job());
        assert_eq!(resolved.backend, CompilerCacheBackend::Sccache);
        assert_eq!(resolved.origin, CompilerCacheOrigin::PolicyExplicit);
    }

    #[test]
    fn disabled_step_does_not_trigger_rust_detection() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "JS",
            "requestId": 1,
            "steps": [
                {
                    "reference": { "type": "Script" },
                    "enabled": false,
                    "inputs": { "script": "cargo build" }
                }
            ]
        }))
        .unwrap();
        assert!(!job_compiles_rust(&job));
    }

    #[test]
    fn wrapper_ownership_rejects_a_foreign_wrapper_with_step_value_and_fix() {
        let mut job = rust_job();
        job.steps[1].environment = Some(serde_json::json!({
            "RUSTC_WRAPPER": "buildcache"
        }));
        let mut violations = Vec::new();
        validate_wrapper_ownership(&job, &AccelerationPolicy::maximum(), &mut violations);
        assert_eq!(violations.len(), 1, "{violations:?}");
        let violation = &violations[0];
        assert_eq!(violation.field, "env.RUSTC_WRAPPER");
        // The display redacts `received`, so the offending value and the fix
        // travel in the accepted list.
        assert!(violation.accepted[0].contains("RUSTC_WRAPPER='buildcache'"));
        assert!(violation.accepted[0].contains("kache"));
        assert!(violation.accepted[0].contains("remove"));
    }

    #[test]
    fn wrapper_ownership_allows_the_matching_wrapper_and_any_wrapper_when_off() {
        let mut job = rust_job();
        job.steps[1].environment = Some(serde_json::json!({ "RUSTC_WRAPPER": "kache" }));
        let mut violations = Vec::new();
        validate_wrapper_ownership(&job, &AccelerationPolicy::maximum(), &mut violations);
        assert!(violations.is_empty(), "{violations:?}");

        let mut policy = AccelerationPolicy::maximum();
        policy.compiler_cache = CompilerCacheBackend::Off;
        let mut job = non_rust_job();
        job.environment_variables = vec![serde_json::json!({ "RUSTC_WRAPPER": "buildcache" })];
        let mut violations = Vec::new();
        validate_wrapper_ownership(&job, &policy, &mut violations);
        assert!(
            violations.is_empty(),
            "no resolved backend means Velnor sets no wrapper"
        );
    }

    #[test]
    fn wrapper_ownership_catches_job_level_and_key_literal_env_forms() {
        let mut job = rust_job();
        job.environment_variables = vec![serde_json::json!({
            "Key": { "lit": "RUSTC_WRAPPER" },
            "Value": { "lit": "sccache" }
        })];
        let mut violations = Vec::new();
        validate_wrapper_ownership(&job, &AccelerationPolicy::maximum(), &mut violations);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].accepted[0].contains("RUSTC_WRAPPER='sccache'"));
    }
}

use crate::job_message::AgentJobRequestMessage;
use anyhow::Result;
use serde_json::{Map, Value};

pub(crate) const RUN_STARTED_AT_ENV: &str = "VELNOR_RUN_STARTED_AT";
pub(crate) const JOB_QUEUED_AT_ENV: &str = "VELNOR_JOB_QUEUED_AT";
pub(crate) const VELNOR_HOST_ENV: &str = "VELNOR_HOST";
pub(crate) const VELNOR_INSTANCE_ENV: &str = "VELNOR_INSTANCE";
pub(crate) const VELNOR_SLOT_ENV: &str = "VELNOR_SLOT";

/// Job-visible Velnor node identity. Non-secret; `runner_name` is the GitHub
/// registration / stored `agent_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRunnerIdentity {
    pub runner_name: String,
    pub host: String,
    pub instance: String,
    pub slot: String,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn job_runtime_env(job: &AgentJobRequestMessage) -> Result<Vec<(String, String)>> {
    job_runtime_env_with_identity(job, None)
}

pub(crate) fn job_runtime_env_with_identity(
    job: &AgentJobRequestMessage,
    identity: Option<&JobRunnerIdentity>,
) -> Result<Vec<(String, String)>> {
    let local_cache_url = std::env::var("VELNOR_ACTIONS_CACHE_URL").ok();
    job_runtime_env_with_identity_and_cache_url(job, identity, local_cache_url.as_deref())
}

fn job_runtime_env_with_identity_and_cache_url(
    job: &AgentJobRequestMessage,
    identity: Option<&JobRunnerIdentity>,
    local_cache_url: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let system_connection = job.system_connection_single_or_default()?;
    let resolved = resolved_identity(identity);
    let mut env = vec![
        ("CI".to_string(), "true".to_string()),
        ("GITHUB_ACTIONS".to_string(), "true".to_string()),
        ("MISE_LOCKFILE".to_string(), "1".to_string()),
        ("MISE_LOCKED".to_string(), "1".to_string()),
        ("MISE_LOCKED_VERIFY_PROVENANCE".to_string(), "1".to_string()),
        ("HOME".to_string(), "/github/home".to_string()),
        ("GITHUB_JOB".to_string(), job.job_name()),
        ("GITHUB_WORKSPACE".to_string(), "/__w".to_string()),
        // Guest jobs run in Linux containers even when the physical host is
        // macOS. Host identity is `VELNOR_HOST`, not `RUNNER_OS`.
        ("RUNNER_OS".to_string(), "Linux".to_string()),
        ("RUNNER_ARCH".to_string(), runner_arch().to_string()),
        ("RUNNER_NAME".to_string(), resolved.runner_name.clone()),
        ("RUNNER_ENVIRONMENT".to_string(), "self-hosted".to_string()),
        ("RUNNER_TEMP".to_string(), "/__t".to_string()),
        ("RUNNER_TOOL_CACHE".to_string(), "/__tool".to_string()),
        ("AGENT_TOOLSDIRECTORY".to_string(), "/__tool".to_string()),
        ("RUNNER_WORKSPACE".to_string(), "/__w".to_string()),
        // Build identity: the release commit and manifest schema the job runs
        // under. Runner-owned; see `is_protected_default_env`.
        (
            "VELNOR_SOURCE_SHA".to_string(),
            env!("VELNOR_SOURCE_SHA").to_string(),
        ),
        (
            "VELNOR_MANIFEST_VERSION".to_string(),
            crate::manifest::MANIFEST_VERSION.to_string(),
        ),
        (VELNOR_HOST_ENV.to_string(), resolved.host),
        (VELNOR_INSTANCE_ENV.to_string(), resolved.instance),
        (VELNOR_SLOT_ENV.to_string(), resolved.slot),
        // Emit runner-owned timing names even when the broker omitted a
        // value, so repository-controlled environment cannot fill a missing
        // value with a spoof.
        (
            RUN_STARTED_AT_ENV.to_string(),
            authoritative_timing_value(job, RUN_STARTED_AT_ENV),
        ),
        (
            JOB_QUEUED_AT_ENV.to_string(),
            authoritative_timing_value(job, JOB_QUEUED_AT_ENV),
        ),
        ("CARGO_INCREMENTAL".to_string(), "0".to_string()),
    ];

    let repository = job.variable("github.repository");
    push_var(&mut env, "GITHUB_REPOSITORY", repository);
    push_var_or_derived(
        &mut env,
        "GITHUB_REPOSITORY_OWNER",
        job.variable("github.repository_owner"),
        repository.and_then(repository_owner),
    );
    push_var(
        &mut env,
        "GITHUB_REPOSITORY_ID",
        job.variable("github.repository_id"),
    );
    push_var(
        &mut env,
        "GITHUB_REPOSITORY_OWNER_ID",
        job.variable("github.repository_owner_id"),
    );
    // The four BuildKit tier signals are always emitted, empty when the
    // broker omits them: container env merges under these authoritative
    // values, so omitting one would let repository-controlled env spoof the
    // trust tier. Empty reads as missing (fail-closed `unknown`) everywhere.
    push_var_or_default(&mut env, "GITHUB_REF", job.variable("github.ref"), "");
    push_var_or_derived(
        &mut env,
        "GITHUB_REF_NAME",
        job.variable("github.ref_name"),
        job.variable("github.ref").map(ref_name),
    );
    push_var_or_default(
        &mut env,
        "GITHUB_REF_TYPE",
        job.variable("github.ref_type"),
        "",
    );
    push_var_or_default(
        &mut env,
        "GITHUB_REF_PROTECTED",
        job.variable("github.ref_protected"),
        "",
    );
    push_var(&mut env, "GITHUB_BASE_REF", job.variable("github.base_ref"));
    push_var(&mut env, "GITHUB_HEAD_REF", job.variable("github.head_ref"));
    push_var(&mut env, "GITHUB_SHA", job.variable("github.sha"));
    push_var(&mut env, "GITHUB_ACTOR", job.variable("github.actor"));
    push_var(&mut env, "GITHUB_ACTOR_ID", job.variable("github.actor_id"));
    push_var(
        &mut env,
        "GITHUB_TRIGGERING_ACTOR",
        job.variable("github.triggering_actor"),
    );
    push_var(&mut env, "GITHUB_WORKFLOW", job.variable("github.workflow"));
    push_var(
        &mut env,
        "GITHUB_WORKFLOW_REF",
        job.variable("github.workflow_ref"),
    );
    push_var(
        &mut env,
        "GITHUB_WORKFLOW_SHA",
        job.variable("github.workflow_sha"),
    );
    push_var_or_default(
        &mut env,
        "GITHUB_EVENT_NAME",
        job.variable("github.event_name"),
        "",
    );
    push_var(&mut env, "GITHUB_RUN_ID", job.variable("github.run_id"));
    push_var(
        &mut env,
        "GITHUB_RUN_NUMBER",
        job.variable("github.run_number"),
    );
    push_var(
        &mut env,
        "GITHUB_RUN_ATTEMPT",
        job.variable("github.run_attempt"),
    );
    push_var(
        &mut env,
        "GITHUB_RETENTION_DAYS",
        job.variable("github.retention_days"),
    );
    push_var_or_default(
        &mut env,
        "GITHUB_SERVER_URL",
        job.variable("github.server_url"),
        "https://github.com",
    );
    push_var_or_default(
        &mut env,
        "GITHUB_API_URL",
        job.variable("github.api_url"),
        "https://api.github.com",
    );
    push_var_or_default(
        &mut env,
        "GITHUB_GRAPHQL_URL",
        job.variable("github.graphql_url"),
        "https://api.github.com/graphql",
    );

    if let Some(endpoint) = system_connection {
        if let Some(url) = endpoint.url.as_deref() {
            set_env(&mut env, "ACTIONS_RUNTIME_URL", url);
        }
        if let Some(token) = endpoint_access_token(endpoint) {
            set_env(&mut env, "ACTIONS_RUNTIME_TOKEN", token);
        }
        push_endpoint_data(&mut env, endpoint, "CacheServerUrl", "ACTIONS_CACHE_URL");
        push_endpoint_data(
            &mut env,
            endpoint,
            "PipelinesServiceUrl",
            "ACTIONS_RUNTIME_URL",
        );
        if push_endpoint_data(
            &mut env,
            endpoint,
            "GenerateIdTokenUrl",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
        ) && let Some(token) = endpoint_access_token(endpoint)
        {
            set_env(&mut env, "ACTIONS_ID_TOKEN_REQUEST_TOKEN", token);
        }
        push_endpoint_data(
            &mut env,
            endpoint,
            "ResultsServiceUrl",
            "ACTIONS_RESULTS_URL",
        );
    }
    // P1: self-hosted job messages never carry a CacheServerUrl, which makes
    // BuildKit's type=gha backend and actions/cache@v4 silently no-op. When
    // the operator enables the daemon-hosted cache service (gha_cache module)
    // by exporting its URL, inject only that endpoint. The job's own runtime
    // token remains the cache bearer and is also retained for Results Service;
    // never replace it with an operator-wide credential.
    if !env.iter().any(|(name, _)| name == "ACTIONS_CACHE_URL") {
        let has_job_token = env
            .iter()
            .any(|(name, value)| name == "ACTIONS_RUNTIME_TOKEN" && !value.is_empty());
        if let Some(url) = configured_cache_url(local_cache_url, has_job_token) {
            set_env(&mut env, "ACTIONS_CACHE_URL", &url);
        }
    }
    if job.variable_bool("actions_uses_cache_service_v2") == Some(true) {
        env.push(("ACTIONS_CACHE_SERVICE_V2".to_string(), "True".to_string()));
    }
    if job.variable_bool("actions_set_orchestration_id_env_for_actions") == Some(true) {
        push_var(
            &mut env,
            "ACTIONS_ORCHESTRATION_ID",
            job.variable("system.orchestrationId"),
        );
    }
    if job.variable_bool("ACTIONS_STEP_DEBUG") == Some(true) {
        env.push(("RUNNER_DEBUG".to_string(), "1".to_string()));
    }

    for (name, value) in job_environment_variables(job) {
        if is_protected_default_env(&name) {
            continue;
        }
        env.push((name, value));
    }

    Ok(env)
}

/// Generated workflows use runner-owned cache state and declare no workflow
/// cache authority. Keep this hook empty while the caller remains shared with
/// older runner paths.
pub(crate) fn cache_authority_env(
    _job: &AgentJobRequestMessage,
    _reserved_bytes: u64,
) -> Result<Vec<(String, String)>> {
    Ok(Vec::new())
}

pub(crate) fn job_environment_variables(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    job.environment_variables
        .iter()
        .flat_map(environment_token_pairs)
        .collect()
}

pub(crate) fn environment_token_pairs(value: &Value) -> Vec<(String, String)> {
    match value {
        Value::Object(object) => environment_object_pairs(object),
        _ => Vec::new(),
    }
}

fn environment_object_pairs(object: &Map<String, Value>) -> Vec<(String, String)> {
    match environment_token_type(object) {
        // JobExtension evaluates each EnvironmentVariables entry as a
        // step-environment TemplateToken, whose converter requires a mapping.
        // Missing `type` means StringToken; malformed, scalar, sequence, and
        // null tokens cannot define environment variable names.
        Some(2) => object_member(object, &["map"])
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(environment_pair_value)
            .collect(),
        Some(_) | None => Vec::new(),
    }
}

fn environment_pair_value(value: &Value) -> Vec<(String, String)> {
    match value {
        Value::Object(object) => {
            if let (Some(key), Some(value)) = (
                object_member(object, &["key"]),
                object_member(object, &["value"]),
            ) && let Some(key) = environment_name(key)
            {
                return vec![(key, environment_value(value))];
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// TemplateToken object members follow CLR's case-insensitive property
/// lookup. This helper is used only for schema members; mapping keys
/// themselves are returned untouched and retain their original spelling.
fn object_member<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    object
        .iter()
        .find(|(member, _)| names.iter().any(|name| member.eq_ignore_ascii_case(name)))
        .map(|(_, value)| value)
}

fn environment_token_type(object: &Map<String, Value>) -> Option<i64> {
    token_discriminator(object, "type", 0)
}

fn token_discriminator(object: &Map<String, Value>, member: &str, default: i64) -> Option<i64> {
    match object_member(object, &[member]) {
        None => Some(default),
        Some(Value::Number(value)) if !value.is_f64() => value.as_i64(),
        Some(_) => None, // The pinned converter returns null for non-integers.
    }
}

fn environment_name(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => {
            match token_discriminator(object, "type", 0) {
                Some(0) => object_member(object, &["lit"]).and_then(token_scalar_string),
                Some(5) => Some(
                    object_member(object, &["bool"])
                        .and_then(token_bool_value)
                        .unwrap_or_default()
                        .to_string(),
                ),
                Some(6) => Some(
                    object_member(object, &["num"])
                        .and_then(token_number_string)
                        .unwrap_or_else(|| crate::expression::value::format_number(0.0)),
                ),
                // Expressions need the job expression context to determine
                // their key; structured, file-table, null, and invalid tokens
                // cannot name an environment variable here.
                Some(_) | None => None,
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => None,
    }
}

fn environment_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(_) => token_number_string(value).unwrap_or_default(),
        Value::Object(object) => {
            match token_discriminator(object, "type", 0) {
                Some(3) => object_member(object, &["expr"])
                    .and_then(token_scalar_string)
                    .filter(|expr| !expr.is_empty())
                    .map(|expr| format!("${{{{ {expr} }}}}"))
                    .unwrap_or_default(),
                Some(5) => object_member(object, &["bool"])
                    .and_then(token_bool_value)
                    .unwrap_or_default()
                    .to_string(),
                Some(6) => object_member(object, &["num"])
                    .and_then(token_number_string)
                    .unwrap_or_else(|| crate::expression::value::format_number(0.0)),
                Some(7) => String::new(),
                Some(0) => object_member(object, &["lit"])
                    .and_then(token_scalar_string)
                    .unwrap_or_default(),
                // Environment values are strings in the runner contract.
                // Structured, file-table, and invalid tokens have no scalar
                // representation here, so retain the fail-closed empty value.
                Some(_) | None => String::new(),
            }
        }
        _ => String::new(),
    }
}

fn token_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(if *value { "True" } else { "False" }.to_owned()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null => None,
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn token_number_string(value: &Value) -> Option<String> {
    let number = match value {
        Value::Number(value) => value.as_f64()?,
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::String(value) => value.trim().replace(',', "").parse::<f64>().ok()?,
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    Some(crate::expression::value::format_number(number))
}

fn token_bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(value) if value.trim().eq_ignore_ascii_case("true") => Some(true),
        Value::String(value) if value.trim().eq_ignore_ascii_case("false") => Some(false),
        Value::Number(value) => value
            .as_i64()
            .map(|value| value != 0)
            .or_else(|| value.as_u64().map(|value| value != 0))
            .or_else(|| value.as_f64().map(|value| value != 0.0)),
        Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

fn is_protected_default_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    name.starts_with("GITHUB_")
        || name.starts_with("RUNNER_")
        || name.starts_with("ACTIONS_")
        || name == "AGENT_TOOLSDIRECTORY"
        || matches!(
            name,
            "MISE_LOCKFILE" | "MISE_LOCKED" | "MISE_LOCKED_VERIFY_PROVENANCE"
        )
        // Exact matches only: jobs legitimately carry VELNOR_APP_ID and
        // VELNOR_APP_PRIVATE_KEY, so no VELNOR_ prefix rule here.
        || matches!(
            name,
            "VELNOR_SOURCE_SHA"
                | "VELNOR_MANIFEST_VERSION"
                | VELNOR_HOST_ENV
                | VELNOR_INSTANCE_ENV
                | VELNOR_SLOT_ENV
                | RUN_STARTED_AT_ENV
                | JOB_QUEUED_AT_ENV
        )
        || (upper.starts_with("MBX_") && upper != "MBX_DISABLE")
}

pub(crate) fn protocol_job_queue_time(job: &AgentJobRequestMessage) -> Option<&str> {
    job.queue_time
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            job.variables
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("system.queueTime"))
                .map(|(_, value)| value)
                .and_then(|value| value.value.as_deref())
                .filter(|value| !value.trim().is_empty())
        })
}

/// When the broker omits queue time, stamp admission so runner-owned
/// `VELNOR_JOB_QUEUED_AT` and the CI report action can compute queue_seconds.
pub(crate) fn stamp_admitted_job_queue_time(job: &mut AgentJobRequestMessage) {
    if protocol_job_queue_time(job).is_some() {
        return;
    }
    let stamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    if stamp.is_empty() {
        return;
    }
    job.queue_time = Some(stamp);
}

fn authoritative_timing_value(job: &AgentJobRequestMessage, name: &str) -> String {
    let value = match name {
        RUN_STARTED_AT_ENV => job.variable("github.run_started_at"),
        JOB_QUEUED_AT_ENV => protocol_job_queue_time(job),
        _ => None,
    };
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

fn configured_cache_url(url: Option<&str>, has_job_token: bool) -> Option<String> {
    url.filter(|url| has_job_token && !url.is_empty())
        .and_then(|url| crate::gha_cache::normalize_public_base(url).ok())
}

/// Extract the cache session binding for this job: the job-scoped runtime
/// token plus the repository identity its cache requests are scoped to,
/// carrying the job's trust class so fork-PR and unknown jobs write an
/// isolated fork namespace instead of the base one. `None` when the job
/// carries no usable token or identity; such jobs keep working, but their
/// cache requests fall back to an isolated per-token namespace (one forensic
/// line per token at request time).
pub(crate) fn job_cache_session(
    job: &AgentJobRequestMessage,
) -> Result<Option<(String, crate::gha_cache::CacheIdentity)>> {
    let system_connection = job.system_connection_single_or_default()?;
    let Some(token) = system_connection
        .and_then(endpoint_access_token)
        .filter(|token| !token.is_empty())
    else {
        return Ok(None);
    };
    let Some(server_url) = job.variable("github.server_url") else {
        return Ok(None);
    };
    let Some(repository_id) = job.variable("github.repository_id") else {
        return Ok(None);
    };
    let Some(git_ref) = job.variable("github.ref") else {
        return Ok(None);
    };
    let base_ref = job
        .variable("github.base_ref")
        .filter(|base| !base.is_empty());
    let trust = crate::trust_class::TrustClass::derive(job);
    let Ok(identity) = crate::gha_cache::CacheIdentity::for_repository(
        server_url,
        repository_id,
        git_ref,
        base_ref,
        trust,
    ) else {
        return Ok(None);
    };
    Ok(Some((token.to_owned(), identity)))
}

fn push_var(env: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        env.push((name.to_string(), value.to_string()));
    }
}

fn push_var_or_default(
    env: &mut Vec<(String, String)>,
    name: &str,
    value: Option<&str>,
    default: &str,
) {
    env.push((name.to_string(), value.unwrap_or(default).to_string()));
}

fn set_env(env: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, current)) = env
        .iter_mut()
        .find(|(current_name, _)| current_name == name)
    {
        *current = value.to_string();
    } else {
        env.push((name.to_string(), value.to_string()));
    }
}

fn push_var_or_derived(
    env: &mut Vec<(String, String)>,
    name: &str,
    value: Option<&str>,
    derived: Option<String>,
) {
    if let Some(value) = value {
        env.push((name.to_string(), value.to_string()));
    } else if let Some(value) = derived {
        env.push((name.to_string(), value));
    }
}

fn ref_name(git_ref: &str) -> String {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .or_else(|| git_ref.strip_prefix("refs/pull/"))
        .unwrap_or(git_ref)
        .to_string()
}

fn repository_owner(repository: &str) -> Option<String> {
    repository
        .split_once('/')
        .map(|(owner, _)| owner.to_string())
        .filter(|owner| !owner.is_empty())
}

fn push_endpoint_data(
    env: &mut Vec<(String, String)>,
    endpoint: &crate::job_message::ServiceEndpoint,
    key: &str,
    env_name: &str,
) -> bool {
    if let Some(value) = endpoint.data_string(key).filter(|value| !value.is_empty()) {
        set_env(env, env_name, value);
        return true;
    }
    false
}

/// The job's runtime credential: the SystemConnection access token, i.e. the
/// exact value injected as `ACTIONS_RUNTIME_TOKEN`. The GHA cache service
/// binds this credential to the job's cache identity at admission, so both
/// sides must read it through this one accessor.
pub(crate) fn job_runtime_token(job: &AgentJobRequestMessage) -> Result<Option<&str>> {
    Ok(job
        .system_connection_single_or_default()?
        .and_then(endpoint_access_token))
}

fn endpoint_access_token(endpoint: &crate::job_message::ServiceEndpoint) -> Option<&str> {
    endpoint
        .authorization
        .as_ref()
        .and_then(|authorization| authorization.parameter_string("AccessToken"))
        .filter(|token| !token.is_empty())
}

fn runner_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "X64",
        "aarch64" => "ARM64",
        "arm" | "armv7" => "ARM",
        arch => arch,
    }
}

fn runner_name() -> String {
    std::env::var("VELNOR_RUNNER_NAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "velnor".to_string())
}

fn resolved_identity(identity: Option<&JobRunnerIdentity>) -> JobRunnerIdentity {
    identity.cloned().unwrap_or_else(|| JobRunnerIdentity {
        runner_name: runner_name(),
        host: crate::runner::github_runner_host_slug(),
        instance: "local".to_string(),
        slot: "0".to_string(),
    })
}

trait JobRuntimeExt {
    fn variable(&self, name: &str) -> Option<&str>;
    fn variable_bool(&self, name: &str) -> Option<bool>;
    fn job_name(&self) -> String;
}

impl JobRuntimeExt for AgentJobRequestMessage {
    fn variable(&self, name: &str) -> Option<&str> {
        self.variables
            .get(name)
            .or_else(|| {
                self.variables
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value)
            })
            .and_then(|value| value.value.as_deref())
    }

    fn variable_bool(&self, name: &str) -> Option<bool> {
        self.variable(name).and_then(|value| {
            if value.trim().eq_ignore_ascii_case("true") {
                Some(true)
            } else if value.trim().eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None
            }
        })
    }

    fn job_name(&self) -> String {
        self.job_name
            .clone()
            .unwrap_or_else(|| self.job_display_name.clone())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    fn acquired_job(value: serde_json::Value) -> anyhow::Result<AgentJobRequestMessage> {
        AgentJobRequestMessage::from_value(value)
    }

    fn environment_mapping(values: Value) -> Value {
        let Value::Object(values) = values else {
            panic!("environment fixture must be a JSON object");
        };
        let map = values
            .into_iter()
            .map(|(key, value)| {
                serde_json::json!({
                    "key": { "type": 0, "lit": key },
                    "value": value
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "type": 2, "map": map })
    }

    #[test]
    fn environment_expr_tokens_render_as_templates_for_runtime_resolution() {
        // Broker sends `env: X: ${{ secrets.Y }}` as an UNevaluated expr token
        // (observed live: {"expr":"secrets.DOCKERHUB_USERNAME","type":3}).
        // Blanking it skipped `if: env.X != ''` steps the GitHub lane runs.
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobName": "__default",
            "jobDisplayName": "validate",
            "requestId": 1,
            "environmentVariables": [{
                "type": 2,
                "map": [
                    { "Key": { "lit": "DOCKERHUB_USERNAME", "type": 0 },
                      "Value": { "expr": "secrets.DOCKERHUB_USERNAME", "type": 3 } },
                    { "Key": { "lit": "PLAIN", "type": 0 },
                      "Value": { "lit": "value", "type": 0 } }
                ]
            }]
        }))
        .unwrap();
        let env = job_environment_variables(&job);
        assert!(env.contains(&(
            "DOCKERHUB_USERNAME".to_string(),
            "${{ secrets.DOCKERHUB_USERNAME }}".to_string()
        )));
        assert!(env.contains(&("PLAIN".to_string(), "value".to_string())));
    }

    #[test]
    fn builds_github_runtime_env_from_job_message() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "jobName": "check",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "acme/repo" },
                "github.repository_id": { "value": "123" },
                "github.repository_owner_id": { "value": "456" },
                "github.ref": { "value": "refs/heads/main" },
                "github.ref_type": { "value": "branch" },
                "github.ref_protected": { "value": "true" },
                "github.sha": { "value": "abc123" },
                "github.actor_id": { "value": "789" },
                "github.triggering_actor": { "value": "octocat" },
                "github.workflow": { "value": "CI" },
                "github.workflow_ref": { "value": "acme/repo/.github/workflows/ci.yml@refs/heads/main" },
                "github.workflow_sha": { "value": "def456" },
                "github.run_attempt": { "value": "2" },
                "github.retention_days": { "value": "90" },
                "ACTIONS_STEP_DEBUG": { "value": "true" },
                "system.github.token": { "value": "ghs_token", "isSecret": true },
                "actions_uses_cache_service_v2": { "value": "true" },
                "actions_set_orchestration_id_env_for_actions": { "value": "true" },
                "system.orchestrationId": { "value": "orch-123" }
            },
            "environmentVariables": [
                environment_mapping(serde_json::json!({
                    "CARGO_TERM_COLOR": "always",
                    "CARGO_INCREMENTAL": 0,
                    "GITHUB_REF": "refs/heads/evil",
                    "ACTIONS_RUNTIME_URL": "https://evil.actions.example",
                    "ACTIONS_CACHE_SERVICE_V2": "false",
                    "MISE_LOCKFILE": "0",
                    "MISE_LOCKED": "0",
                    "MISE_LOCKED_VERIFY_PROVENANCE": "false",
                    "SCCACHE_BASEDIRS": "/untrusted/override"
                })),
                {
                    "type": 2,
                    "map": [
                        {
                            "key": { "type": 0, "lit": "SCCACHE_DIR" },
                            "value": { "type": 0, "lit": "/var/cache/sccache" }
                        },
                        {
                            "key": { "type": 0, "lit": "CARGO_INCREMENTAL" },
                            "value": { "type": 0, "lit": "1" }
                        }
                    ]
                }
            ],
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/abc",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    },
                    "data": {
                        "CacheServerUrl": "https://cache.actions.example",
                        "PipelinesServiceUrl": "https://pipelines-v2.actions.example",
                        "GenerateIdTokenUrl": "https://oidc.actions.example/id-token",
                        "ResultsServiceUrl": "https://results.actions.example"
                    }
                }]
            }
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("GITHUB_ACTIONS".into(), "true".into())));
        assert!(env.contains(&("MISE_LOCKFILE".into(), "1".into())));
        assert!(env.contains(&("MISE_LOCKED".into(), "1".into())));
        assert!(env.contains(&("MISE_LOCKED_VERIFY_PROVENANCE".into(), "1".into())));
        assert!(!env.contains(&("MISE_LOCKFILE".into(), "0".into())));
        assert!(!env.contains(&("MISE_LOCKED".into(), "0".into())));
        assert!(!env.contains(&("MISE_LOCKED_VERIFY_PROVENANCE".into(), "false".into())));
        assert!(env.contains(&("HOME".into(), "/github/home".into())));
        assert!(env.contains(&("RUNNER_ARCH".into(), runner_arch().into())));
        assert!(env.contains(&("RUNNER_NAME".into(), runner_name())));
        assert!(env.contains(&("RUNNER_ENVIRONMENT".into(), "self-hosted".into())));
        assert!(env.contains(&("RUNNER_WORKSPACE".into(), "/__w".into())));
        assert!(env.contains(&("RUNNER_TOOL_CACHE".into(), "/__tool".into())));
        assert!(env.contains(&("AGENT_TOOLSDIRECTORY".into(), "/__tool".into())));
        assert!(env.contains(&("RUNNER_DEBUG".into(), "1".into())));
        assert!(env.contains(&("GITHUB_JOB".into(), "check".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY".into(), "acme/repo".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY_OWNER".into(), "acme".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY_ID".into(), "123".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY_OWNER_ID".into(), "456".into())));
        assert!(env.contains(&("GITHUB_REF_NAME".into(), "main".into())));
        assert!(env.contains(&("GITHUB_REF_TYPE".into(), "branch".into())));
        assert!(env.contains(&("GITHUB_REF_PROTECTED".into(), "true".into())));
        assert!(env.contains(&("GITHUB_WORKFLOW".into(), "CI".into())));
        assert!(env.contains(&("GITHUB_WORKFLOW_SHA".into(), "def456".into())));
        assert!(env.contains(&("GITHUB_ACTOR_ID".into(), "789".into())));
        assert!(env.contains(&("GITHUB_TRIGGERING_ACTOR".into(), "octocat".into())));
        assert!(env.contains(&("GITHUB_RUN_ATTEMPT".into(), "2".into())));
        assert!(env.contains(&("GITHUB_RETENTION_DAYS".into(), "90".into())));
        assert!(env.contains(&("GITHUB_SERVER_URL".into(), "https://github.com".into())));
        assert!(env.contains(&("GITHUB_API_URL".into(), "https://api.github.com".into())));
        assert!(env.contains(&(
            "GITHUB_GRAPHQL_URL".into(),
            "https://api.github.com/graphql".into()
        )));
        // `github.token` remains in the expression context; the pinned
        // GitHubContext runtime env allowlist does not synthesize GITHUB_TOKEN.
        assert!(!env.iter().any(|(name, _)| name == "GITHUB_TOKEN"));
        assert!(env.contains(&("CARGO_TERM_COLOR".into(), "always".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "1".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "0".into())));
        assert!(env.contains(&("GITHUB_REF".into(), "refs/heads/main".into())));
        assert!(!env.contains(&("GITHUB_REF".into(), "refs/heads/evil".into())));
        assert!(env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(!env.contains(&(
            "ACTIONS_RUNTIME_URL".into(),
            "https://evil.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_RUNTIME_URL".into(),
            "https://pipelines-v2.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_CACHE_URL".into(),
            "https://cache.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_RESULTS_URL".into(),
            "https://results.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_ID_TOKEN_REQUEST_URL".into(),
            "https://oidc.actions.example/id-token".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
            "runtime-token".into()
        )));
        assert!(env.contains(&("ACTIONS_CACHE_SERVICE_V2".into(), "True".into())));
        assert!(!env.contains(&("ACTIONS_CACHE_SERVICE_V2".into(), "false".into())));
        assert!(env.contains(&("ACTIONS_ORCHESTRATION_ID".into(), "orch-123".into())));
    }

    #[test]
    fn build_identity_env_is_runner_owned() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "environmentVariables": [environment_mapping(serde_json::json!({
                "VELNOR_SOURCE_SHA": "spoofed",
                "VELNOR_MANIFEST_VERSION": "spoofed",
                "VELNOR_APP_ID": "12345",
                "VELNOR_APP_PRIVATE_KEY": "secret",
            }))],
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("VELNOR_SOURCE_SHA".into(), env!("VELNOR_SOURCE_SHA").into())));
        assert!(env.contains(&(
            "VELNOR_MANIFEST_VERSION".into(),
            crate::manifest::MANIFEST_VERSION.to_string()
        )));
        assert!(!env.contains(&("VELNOR_SOURCE_SHA".into(), "spoofed".into())));
        assert!(!env.contains(&("VELNOR_MANIFEST_VERSION".into(), "spoofed".into())));
        // No VELNOR_ prefix rule: legitimate job-carried vars pass through.
        assert!(env.contains(&("VELNOR_APP_ID".into(), "12345".into())));
        assert!(env.contains(&("VELNOR_APP_PRIVATE_KEY".into(), "secret".into())));
    }

    #[test]
    fn runner_name_matches_registration_identity_when_known() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "environmentVariables": [environment_mapping(serde_json::json!({
                "VELNOR_HOST": "spoofed-host",
                "VELNOR_INSTANCE": "spoofed-instance",
                "VELNOR_SLOT": "99",
                "RUNNER_NAME": "spoofed-runner",
            }))],
        }))
        .unwrap();
        let identity = JobRunnerIdentity {
            runner_name: "velnor-sentry-primary-2".into(),
            host: "sentry".into(),
            instance: "primary".into(),
            slot: "2".into(),
        };

        let env = job_runtime_env_with_identity(&job, Some(&identity)).unwrap();

        assert!(env.contains(&("RUNNER_NAME".into(), "velnor-sentry-primary-2".into())));
        assert!(env.contains(&(VELNOR_HOST_ENV.into(), "sentry".into())));
        assert!(env.contains(&(VELNOR_INSTANCE_ENV.into(), "primary".into())));
        assert!(env.contains(&(VELNOR_SLOT_ENV.into(), "2".into())));
        assert!(!env.contains(&("RUNNER_NAME".into(), "spoofed-runner".into())));
        assert!(!env.contains(&(VELNOR_HOST_ENV.into(), "spoofed-host".into())));
        assert!(!env.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization")
                || value.contains("ghp_")
                || value.contains("github_pat_")
        }));
    }

    #[test]
    fn timing_env_is_runner_owned_and_uses_protocol_queue_time() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "queueTime": "2026-09-15T01:02:03Z",
            "variables": {
                "github.run_started_at": { "value": "2026-09-15T01:01:00Z" }
            },
            "environmentVariables": [environment_mapping(serde_json::json!({
                "VELNOR_RUN_STARTED_AT": "spoofed",
                "VELNOR_JOB_QUEUED_AT": "spoofed"
            }))]
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&(RUN_STARTED_AT_ENV.into(), "2026-09-15T01:01:00Z".into())));
        assert!(env.contains(&(JOB_QUEUED_AT_ENV.into(), "2026-09-15T01:02:03Z".into())));
        assert!(!env.contains(&(RUN_STARTED_AT_ENV.into(), "spoofed".into())));
        assert!(!env.contains(&(JOB_QUEUED_AT_ENV.into(), "spoofed".into())));

        let fallback_job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "55555555-5555-5555-5555-555555555555",
            "jobDisplayName": "Check",
            "requestId": 1,
            "queueTime": " ",
            "variables": {
                "system.queueTime": { "value": "2026-09-15T01:02:03Z" }
            }
        }))
        .unwrap();
        let fallback_env = job_runtime_env(&fallback_job).unwrap();
        assert!(fallback_env.contains(&(JOB_QUEUED_AT_ENV.into(), "2026-09-15T01:02:03Z".into())));
    }

    #[test]
    fn admitted_queue_stamp_wires_velnor_job_queued_at_when_broker_omits_queue_time() {
        let mut job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1
        }))
        .unwrap();

        assert!(protocol_job_queue_time(&job).is_none());
        stamp_admitted_job_queue_time(&mut job);
        let stamped = protocol_job_queue_time(&job)
            .expect("admitted stamp must populate queue time")
            .to_owned();
        assert!(
            time::OffsetDateTime::parse(&stamped, &time::format_description::well_known::Rfc3339)
                .is_ok(),
            "stamp must be RFC3339: {stamped}"
        );

        let env = job_runtime_env(&job).unwrap();
        assert!(env.contains(&(JOB_QUEUED_AT_ENV.into(), stamped)));
    }

    #[test]
    fn admitted_queue_stamp_does_not_override_protocol_queue_time() {
        let mut job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "queueTime": "2026-09-15T01:02:03Z"
        }))
        .unwrap();

        stamp_admitted_job_queue_time(&mut job);
        assert_eq!(protocol_job_queue_time(&job), Some("2026-09-15T01:02:03Z"));
    }

    #[test]
    fn cache_endpoint_requires_the_job_runtime_token() {
        assert_eq!(configured_cache_url(Some("http://cache"), false), None);
        assert_eq!(configured_cache_url(Some(""), true), None);
        assert_eq!(configured_cache_url(None, true), None);
        assert_eq!(
            configured_cache_url(Some("http://cache"), true),
            Some("http://cache".to_owned())
        );
        assert_eq!(
            configured_cache_url(Some("http://cache///"), true),
            Some("http://cache".to_owned())
        );
        assert_eq!(configured_cache_url(Some("ftp://cache"), true), None);
    }

    #[test]
    fn local_cache_fallback_does_not_invent_v2_mode() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    }
                }]
            }
        }))
        .unwrap();

        let env =
            job_runtime_env_with_identity_and_cache_url(&job, None, Some("http://cache.example"))
                .unwrap();

        assert!(env.contains(&("ACTIONS_CACHE_URL".into(), "http://cache.example".into())));
        assert!(!env
            .iter()
            .any(|(name, _)| name == "ACTIONS_CACHE_SERVICE_V2"));
    }

    #[test]
    fn local_cache_fallback_preserves_upstream_results_url_and_explicit_v2_mode() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": {
                "actions_uses_cache_service_v2": { "value": "true" }
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    },
                    "data": {
                        "ResultsServiceUrl": "https://results.actions.example"
                    }
                }]
            }
        }))
        .unwrap();

        let env =
            job_runtime_env_with_identity_and_cache_url(&job, None, Some("http://cache.example"))
                .unwrap();

        assert!(env.contains(&("ACTIONS_CACHE_URL".into(), "http://cache.example".into())));
        assert!(env.contains(&(
            "ACTIONS_RESULTS_URL".into(),
            "https://results.actions.example".into()
        )));
        assert!(env.contains(&("ACTIONS_CACHE_SERVICE_V2".into(), "True".into())));
    }

    #[test]
    fn reads_runtime_endpoint_values_case_insensitively() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/fallback",
                    "authorization": {
                        "parameters": { "accesstoken": "runtime-token" }
                    },
                    "data": {
                        "cacheServerUrl": "https://cache.actions.example",
                        "pipelinesserviceurl": "https://pipelines-v2.actions.example",
                        "generateIdTokenUrl": "https://oidc.actions.example/id-token",
                        "resultsserviceurl": "https://results.actions.example"
                    }
                }]
            }
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(env.contains(&(
            "ACTIONS_RUNTIME_URL".into(),
            "https://pipelines-v2.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_CACHE_URL".into(),
            "https://cache.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_RESULTS_URL".into(),
            "https://results.actions.example".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_ID_TOKEN_REQUEST_URL".into(),
            "https://oidc.actions.example/id-token".into()
        )));
        assert!(env.contains(&(
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
            "runtime-token".into()
        )));
    }

    #[test]
    fn runtime_endpoint_data_skips_missing_null_and_empty_values() {
        let cases = [
            ("missing", serde_json::json!({})),
            (
                "null",
                serde_json::json!({
                    "CacheServerUrl": null,
                    "PipelinesServiceUrl": null,
                    "GenerateIdTokenUrl": null,
                    "ResultsServiceUrl": null
                }),
            ),
            (
                "empty",
                serde_json::json!({
                    "CacheServerUrl": "",
                    "PipelinesServiceUrl": "",
                    "GenerateIdTokenUrl": "",
                    "ResultsServiceUrl": ""
                }),
            ),
        ];

        for (case, data) in cases {
            let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
                "messageType": "PipelineAgentJobRequest",
                "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
                "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
                "jobId": "11111111-1111-1111-1111-111111111111",
                "jobDisplayName": "Check",
                "requestId": 1,
                "resources": {
                    "endpoints": [{
                        "name": "SystemVssConnection",
                        "url": "https://pipelines.actions.example/base",
                        "authorization": {
                            "parameters": { "AccessToken": "runtime-token" }
                        },
                        "data": data
                    }]
                }
            }))
            .unwrap();

            let env = job_runtime_env_with_identity_and_cache_url(&job, None, None).unwrap();

            assert_eq!(
                env.iter()
                    .filter(|(name, _)| name == "ACTIONS_RUNTIME_URL")
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>(),
                ["https://pipelines.actions.example/base"],
                "{case}: missing PipelinesServiceUrl must preserve endpoint.Url"
            );
            for name in [
                "ACTIONS_CACHE_URL",
                "ACTIONS_RESULTS_URL",
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            ] {
                assert!(
                    !env.iter().any(|(env_name, _)| env_name == name),
                    "{case}: {name} must not be emitted"
                );
            }
        }
    }

    #[test]
    fn runtime_endpoint_data_preserves_whitespace_and_rejects_environment_aliases() {
        let whitespace_job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.example/base",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    },
                    "data": {
                        "cAcHeSeRvErUrL": " ",
                        "pIpElInEsSeRvIcEuRl": "\t",
                        "gEnErAtEiDtOkEnUrL": " ",
                        "rEsUlTsSeRvIcEuRl": "\n"
                    }
                }]
            }
        }))
        .unwrap();

        let whitespace_env =
            job_runtime_env_with_identity_and_cache_url(&whitespace_job, None, None).unwrap();
        for (name, expected) in [
            ("ACTIONS_CACHE_URL", " "),
            ("ACTIONS_RUNTIME_URL", "\t"),
            ("ACTIONS_ID_TOKEN_REQUEST_URL", " "),
            ("ACTIONS_RESULTS_URL", "\n"),
        ] {
            assert!(whitespace_env.contains(&(name.into(), expected.into())));
        }
        assert!(whitespace_env.contains(&(
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
            "runtime-token".into()
        )));

        let alias_job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.example/base",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    },
                    "data": {
                        "ACTIONS_CACHE_URL": "https://cache.actions.example/alias",
                        "ACTIONS_RUNTIME_URL": "https://pipelines.actions.example/alias",
                        "ACTIONS_ID_TOKEN_REQUEST_URL": "https://oidc.actions.example/alias",
                        "ACTIONS_RESULTS_URL": "https://results.actions.example/alias"
                    }
                }]
            }
        }))
        .unwrap();

        let alias_env =
            job_runtime_env_with_identity_and_cache_url(&alias_job, None, None).unwrap();
        assert!(alias_env.contains(&(
            "ACTIONS_RUNTIME_URL".into(),
            "https://pipelines.actions.example/base".into()
        )));
        for name in [
            "ACTIONS_CACHE_URL",
            "ACTIONS_RESULTS_URL",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            assert!(
                !alias_env.iter().any(|(env_name, _)| env_name == name),
                "unsupported data alias {name} must not be exported"
            );
        }
    }

    #[test]
    fn null_or_empty_endpoint_access_token_is_absent_but_whitespace_is_preserved() {
        for parameters in [
            serde_json::json!({ "AccessToken": null }),
            serde_json::json!({ "AccessToken": "" }),
            serde_json::Value::Null,
        ] {
            let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
                "messageType": "PipelineAgentJobRequest",
                "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
                "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
                "jobId": "11111111-1111-1111-1111-111111111111",
                "jobDisplayName": "Check",
                "requestId": 1,
                "resources": {
                    "endpoints": [{
                        "name": "SystemVssConnection",
                        "authorization": { "parameters": parameters }
                    }]
                }
            }))
            .unwrap();

            assert_eq!(job_runtime_token(&job).unwrap(), None);
        }

        let whitespace_job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": { "parameters": { "aCcEsStOkEn": " " } }
                }]
            }
        }))
        .unwrap();

        assert_eq!(job_runtime_token(&whitespace_job).unwrap(), Some(" "));
    }

    #[test]
    fn clr_materialized_resource_scalars_and_dictionary_keys_reach_runtime_env() {
        let job = acquired_job(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanId": "22222222-2222-2222-2222-222222222222" },
            "Timeline": { "Id": "33333333-3333-3333-3333-333333333333" },
            "JobId": "11111111-1111-1111-1111-111111111111",
            "JobDisplayName": "Check",
            "RequestId": 1,
            "Variables": {
                "GitHub.Repository": { "Value": "Acme/Repo" },
                "GitHub.Repository_ID": { "Value": 123 },
                "GitHub.REF": { "Value": "refs/heads/main" },
                "actions_step_debug": { "Value": " TRUE " }
            },
            "Resources": {
                "Endpoints": [{
                    "Name": "sYsTeMvSsCoNnEcTiOn",
                    "Authorization": {
                        "Parameters": { "aCcEsStOkEn": "runtime-token" }
                    },
                    "Data": {
                        "cAcHeSeRvErUrL": "https://cache.actions.example",
                        "PipelinesServiceUrl": 123
                    }
                }]
            }
        }))
        .expect("CLR-shaped job message parses");

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("GITHUB_REPOSITORY".into(), "Acme/Repo".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY_ID".into(), "123".into())));
        assert!(env.contains(&("GITHUB_REF".into(), "refs/heads/main".into())));
        assert!(env.contains(&("RUNNER_DEBUG".into(), "1".into())));
        assert!(env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(env.contains(&(
            "ACTIONS_CACHE_URL".into(),
            "https://cache.actions.example".into()
        )));
        assert!(env.contains(&("ACTIONS_RUNTIME_URL".into(), "123".into())));
    }

    #[test]
    fn reads_run_service_typed_job_environment_maps() {
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "environmentVariables": [{
                "type": 2,
                "map": [
                    { "Key": { "lit": "CARGO_TERM_COLOR" }, "Value": { "lit": "always" } },
                    { "Key": { "lit": "CARGO_INCREMENTAL" }, "Value": { "type": 6, "num": 0 } },
                    { "Key": { "lit": "RENOVATE_ONBOARDING" }, "Value": { "type": 5, "bool": false } },
                    { "Key": { "lit": "GITHUB_REF" }, "Value": { "lit": "refs/heads/evil" } },
                    { "Key": { "lit": "MBX_DISABLE" }, "Value": { "lit": "1" } },
                    { "Key": { "lit": "MBX_CACHE_DIR" }, "Value": { "lit": "/untrusted" } }
                ]
            }]
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("CARGO_TERM_COLOR".into(), "always".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "0".into())));
        assert!(env.contains(&("RENOVATE_ONBOARDING".into(), "false".into())));
        assert!(!env.contains(&("GITHUB_REF".into(), "refs/heads/evil".into())));
        assert!(env.contains(&("MBX_DISABLE".into(), "1".into())));
        assert!(!env.iter().any(|(name, _)| name == "MBX_CACHE_DIR"));
    }

    #[test]
    fn typed_environment_tokens_match_clr_fields_and_keep_mapping_key_text() {
        let job = acquired_job(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanId": "22222222-2222-2222-2222-222222222222" },
            "Timeline": { "Id": "33333333-3333-3333-3333-333333333333" },
            "JobId": "11111111-1111-1111-1111-111111111111",
            "JobDisplayName": "Check",
            "RequestId": 1,
            "EnvironmentVariables": [{
                "TyPe": 2,
                "MaP": [
                    {
                        "KEY": { "TYPE": 0, "LIT": "MiXeD.Name" },
                        "VALUE": { "TYPE": 0, "LIT": 123 }
                    },
                    {
                        "Key": { "type": 0, "lit": "BOOLEAN" },
                        "Value": { "type": 5, "bool": false }
                    },
                    {
                        "Key": { "type": 0, "lit": "NUMBER" },
                        "Value": { "type": 6, "num": 1.25 }
                    },
                    {
                        "Key": { "type": 0, "lit": "RAW_NUMBER" },
                        "Value": 1.0
                    },
                    {
                        "Key": { "type": 0, "lit": "NULL" },
                        "Value": { "type": 7 }
                    },
                    {
                        "Key": { "lit": "DEFAULT_STRING" },
                        "Value": { "lit": true }
                    },
                    {
                        "Key": { "type": 0, "lit": "type" },
                        "Value": { "type": 0, "lit": "preserved" }
                    },
                    {
                        "Key": { "type": 0, "lit": "Path" },
                        "Value": { "type": 0, "lit": "upper" }
                    },
                    {
                        "Key": { "type": 0, "lit": "path" },
                        "Value": { "type": 0, "lit": "lower" }
                    }
                ]
            }]
        }))
        .expect("CLR-normalized job message parses");

        let env = job_environment_variables(&job);

        assert!(env.contains(&("MiXeD.Name".into(), "123".into())));
        assert!(env.contains(&("BOOLEAN".into(), "false".into())));
        assert!(env.contains(&("NUMBER".into(), "1.25".into())));
        assert!(env.contains(&("RAW_NUMBER".into(), "1".into())));
        assert!(env.contains(&("NULL".into(), String::new())));
        assert!(env.contains(&("DEFAULT_STRING".into(), "True".into())));
        assert!(env.contains(&("type".into(), "preserved".into())));
        assert!(env.contains(&("Path".into(), "upper".into())));
        assert!(env.contains(&("path".into(), "lower".into())));
    }

    #[test]
    fn environment_tokens_use_clr_defaults_and_require_a_mapping_token() {
        let job = acquired_job(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanId": "22222222-2222-2222-2222-222222222222" },
            "Timeline": { "Id": "33333333-3333-3333-3333-333333333333" },
            "JobId": "11111111-1111-1111-1111-111111111111",
            "JobDisplayName": "Check",
            "RequestId": 1,
            "EnvironmentVariables": [
                {
                    // A missing discriminator defaults to StringToken; its
                    // extension `map` member must not be reinterpreted.
                    "map": [{
                        "key": { "type": 0, "lit": "PLAIN_MAP_LEAK" },
                        "value": { "type": 0, "lit": "value" }
                    }]
                },
                {
                    "type": 1,
                    "seq": [{
                        "type": 2,
                        "map": [{
                            "key": { "type": 0, "lit": "SEQUENCE_LEAK" },
                            "value": { "type": 0, "lit": "value" }
                        }]
                    }]
                },
                {
                    "TYPE": 2,
                    "MAP": [
                        {
                            "Key": { "type": 0, "lit": "DEFAULT_BOOL" },
                            "Value": { "type": 5 }
                        },
                        {
                            "Key": { "type": 0, "lit": "DEFAULT_NUMBER" },
                            "Value": { "type": 6 }
                        },
                        {
                            "Key": { "type": 0, "lit": "DEFAULT_STRING" },
                            "Value": { "type": 0 }
                        },
                        {
                            "Key": { "type": 0, "lit": "NULL" },
                            "Value": null
                        }
                    ]
                },
                null
            ]
        }))
        .expect("CLR-normalized job message parses");

        assert_eq!(
            job_environment_variables(&job),
            vec![
                ("DEFAULT_BOOL".into(), "false".into()),
                ("DEFAULT_NUMBER".into(), "0".into()),
                ("DEFAULT_STRING".into(), String::new()),
                ("NULL".into(), String::new()),
            ]
        );
    }

    #[test]
    fn non_integer_template_token_discriminator_does_not_expose_token_members() {
        let value = serde_json::json!({
            "TYPE": 2.0,
            "MAP": [{
                "KEY": { "TYPE": 0, "LIT": "SHOULD_NOT_APPEAR" },
                "VALUE": { "TYPE": 0, "LIT": "value" }
            }]
        });

        assert!(environment_token_pairs(&value).is_empty());
    }

    #[test]
    fn tier_signals_are_always_emitted_empty_when_the_broker_omits_them() {
        // Container env merges under these authoritative values, so a missing
        // signal must still overwrite — never leave room for a spoofed tier.
        let job: AgentJobRequestMessage = acquired_job(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": {
                "github.ref": { "value": "refs/heads/main" }
            }
        }))
        .unwrap();

        let env = job_runtime_env(&job).unwrap();

        assert!(env.contains(&("GITHUB_REF".into(), "refs/heads/main".into())));
        for name in [
            "GITHUB_REF_TYPE",
            "GITHUB_EVENT_NAME",
            "GITHUB_REF_PROTECTED",
        ] {
            assert!(
                env.contains(&(name.to_string(), String::new())),
                "missing empty default for {name}"
            );
        }
    }

    fn cache_session_job(
        variables: serde_json::Value,
        token: Option<&str>,
    ) -> AgentJobRequestMessage {
        let mut job = serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": variables,
        });
        if let Some(token) = token {
            job["resources"] = serde_json::json!({
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/x",
                    "authorization": {
                        "parameters": { "AccessToken": token }
                    }
                }]
            });
        }
        acquired_job(job).unwrap()
    }

    #[test]
    fn job_cache_session_binds_token_to_repository_identity() {
        let job = cache_session_job(
            serde_json::json!({
                "github.repository": { "value": "Acme/Repo" },
                "github.repository_id": { "value": "123" },
                "github.server_url": { "value": "https://github.com" },
                "github.ref": { "value": "refs/pull/7/merge" },
                "github.base_ref": { "value": "main" },
            }),
            Some("runtime-token"),
        );
        let (token, identity) = job_cache_session(&job).unwrap().unwrap();
        assert_eq!(token, "runtime-token");
        // No event or plan-scope signals: the derivation fails closed to
        // Unknown, and the identity carries it.
        assert_eq!(
            identity,
            crate::gha_cache::CacheIdentity::for_repository(
                "https://github.com",
                "123",
                "refs/pull/7/merge",
                Some("refs/heads/main"),
                crate::trust_class::TrustClass::Unknown,
            )
            .unwrap()
        );

        let renamed_job = cache_session_job(
            serde_json::json!({
                "github.repository": { "value": "renamed/display-only" },
                "github.repository_id": { "value": "123" },
                "github.server_url": { "value": "https://github.com/" },
                "github.ref": { "value": "refs/pull/7/merge" },
                "github.base_ref": { "value": "main" },
            }),
            Some("another-runtime-token"),
        );
        let (_, renamed_identity) = job_cache_session(&renamed_job).unwrap().unwrap();
        assert_eq!(identity, renamed_identity);
    }

    fn cache_session_job_full(body: serde_json::Value) -> AgentJobRequestMessage {
        acquired_job(body).expect("cache session test job parses")
    }

    #[test]
    fn job_cache_session_carries_trusted_for_base_jobs() {
        let job = cache_session_job_full(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222", "scopeIdentifier": "scope" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": {
                "github.event_name": { "value": "push" },
                "github.repository": { "value": "acme/repo" },
                "github.repository_id": { "value": "123" },
                "github.server_url": { "value": "https://github.com" },
                "github.ref": { "value": "refs/heads/main" },
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/x",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    }
                }],
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" },
                }],
            },
        }));
        let (token, identity) = job_cache_session(&job).unwrap().unwrap();
        assert_eq!(token, "runtime-token");
        assert_eq!(
            identity,
            crate::gha_cache::CacheIdentity::for_repository(
                "https://github.com",
                "123",
                "refs/heads/main",
                None,
                crate::trust_class::TrustClass::Trusted,
            )
            .unwrap()
        );
    }

    #[test]
    fn job_cache_session_carries_fork_pr_for_fork_events() {
        let job = cache_session_job_full(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "22222222-2222-2222-2222-222222222222", "scopeIdentifier": "scope" },
            "timeline": { "id": "33333333-3333-3333-3333-333333333333" },
            "jobId": "11111111-1111-1111-1111-111111111111",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": {
                "github.event_name": { "value": "pull_request" },
                "github.repository": { "value": "octo/base" },
                "github.repository_id": { "value": "1" },
                "github.server_url": { "value": "https://github.com" },
                "github.ref": { "value": "refs/pull/7/merge" },
                "github.base_ref": { "value": "main" },
            },
            "contextData": {
                "github": {
                    "event": {
                        "pull_request": {
                            "head": { "repo": { "full_name": "mallory/base", "id": 2 } },
                            "base": { "repo": { "id": 1 } },
                        }
                    }
                }
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/x",
                    "authorization": {
                        "parameters": { "AccessToken": "runtime-token" }
                    }
                }],
                "repositories": [{
                    "alias": "self",
                    "name": "octo/base",
                    "properties": { "cloneUrl": "https://github.com/octo/base.git" },
                }],
            },
        }));
        let (token, identity) = job_cache_session(&job).unwrap().unwrap();
        assert_eq!(token, "runtime-token");
        assert_eq!(
            identity,
            crate::gha_cache::CacheIdentity::for_repository(
                "https://github.com",
                "1",
                "refs/pull/7/merge",
                Some("refs/heads/main"),
                crate::trust_class::TrustClass::ForkPR,
            )
            .unwrap()
        );
    }

    #[test]
    fn job_cache_session_is_none_without_token_or_identity() {
        let repo = serde_json::json!({
            "github.repository": { "value": "acme/repo" },
            "github.ref": { "value": "refs/heads/main" },
        });
        // No endpoint at all.
        assert!(job_cache_session(&cache_session_job(repo.clone(), None))
            .unwrap()
            .is_none());
        // Empty token.
        assert!(
            job_cache_session(&cache_session_job(repo.clone(), Some("")))
                .unwrap()
                .is_none()
        );
        // No repository variable.
        let job = cache_session_job(
            serde_json::json!({
                "github.ref": { "value": "refs/heads/main" },
            }),
            Some("runtime-token"),
        );
        assert!(job_cache_session(&job).unwrap().is_none());
        // Invalid repository ID cannot fall back to the display slug.
        let job = cache_session_job(
            serde_json::json!({
                "github.repository": { "value": "not-a-repo" },
                "github.repository_id": { "value": "0" },
                "github.server_url": { "value": "https://github.com" },
                "github.ref": { "value": "refs/heads/main" },
            }),
            Some("runtime-token"),
        );
        assert!(job_cache_session(&job).unwrap().is_none());
    }
}

use crate::job_message::AgentJobRequestMessage;
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub fn job_runtime_env(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    let mut env = vec![
        ("CI".to_string(), "true".to_string()),
        ("GITHUB_ACTIONS".to_string(), "true".to_string()),
        ("MISE_LOCKFILE".to_string(), "1".to_string()),
        ("MISE_LOCKED".to_string(), "1".to_string()),
        ("MISE_LOCKED_VERIFY_PROVENANCE".to_string(), "1".to_string()),
        ("HOME".to_string(), "/github/home".to_string()),
        ("GITHUB_JOB".to_string(), job.job_name()),
        ("GITHUB_WORKSPACE".to_string(), "/__w".to_string()),
        ("RUNNER_OS".to_string(), "Linux".to_string()),
        ("RUNNER_ARCH".to_string(), runner_arch().to_string()),
        ("RUNNER_NAME".to_string(), runner_name()),
        ("RUNNER_ENVIRONMENT".to_string(), "self-hosted".to_string()),
        ("RUNNER_TEMP".to_string(), "/__t".to_string()),
        ("RUNNER_TOOL_CACHE".to_string(), "/__tool".to_string()),
        ("AGENT_TOOLSDIRECTORY".to_string(), "/__tool".to_string()),
        ("RUNNER_WORKSPACE".to_string(), "/__w".to_string()),
        ("CARGO_INCREMENTAL".to_string(), "0".to_string()),
        (
            "SCCACHE_CACHE_SIZE".to_string(),
            std::env::var("VELNOR_SCCACHE_CACHE_SIZE").unwrap_or_else(|_| "20G".to_string()),
        ),
        (
            "SCCACHE_BASEDIRS".to_string(),
            "/__w:/github/home".to_string(),
        ),
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
    push_var(&mut env, "GITHUB_REF", job.variable("github.ref"));
    push_var_or_derived(
        &mut env,
        "GITHUB_REF_NAME",
        job.variable("github.ref_name"),
        job.variable("github.ref").map(ref_name),
    );
    push_var(&mut env, "GITHUB_REF_TYPE", job.variable("github.ref_type"));
    push_var(
        &mut env,
        "GITHUB_REF_PROTECTED",
        job.variable("github.ref_protected"),
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
    push_var(
        &mut env,
        "GITHUB_EVENT_NAME",
        job.variable("github.event_name"),
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
    push_var(
        &mut env,
        "GITHUB_TOKEN",
        job.variable("system.github.token"),
    );

    if let Some(endpoint) = job.system_connection() {
        if let Some(url) = endpoint.url.as_deref() {
            set_env(&mut env, "ACTIONS_RUNTIME_URL", url);
        }
        if let Some(token) = endpoint_access_token(endpoint) {
            set_env(&mut env, "ACTIONS_RUNTIME_TOKEN", token);
        }
        push_endpoint_data(
            &mut env,
            endpoint,
            &["CacheServerUrl", "cacheServerUrl", "ACTIONS_CACHE_URL"],
            "ACTIONS_CACHE_URL",
        );
        push_endpoint_data(
            &mut env,
            endpoint,
            &[
                "PipelinesServiceUrl",
                "pipelinesServiceUrl",
                "ACTIONS_RUNTIME_URL",
            ],
            "ACTIONS_RUNTIME_URL",
        );
        if push_endpoint_data(
            &mut env,
            endpoint,
            &[
                "GenerateIdTokenUrl",
                "generateIdTokenUrl",
                "ACTIONS_ID_TOKEN_REQUEST_URL",
            ],
            "ACTIONS_ID_TOKEN_REQUEST_URL",
        ) && let Some(token) = endpoint_access_token(endpoint)
        {
            set_env(&mut env, "ACTIONS_ID_TOKEN_REQUEST_TOKEN", token);
        }
        push_endpoint_data(
            &mut env,
            endpoint,
            &[
                "ResultsServiceUrl",
                "resultsServiceUrl",
                "ACTIONS_RESULTS_URL",
            ],
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
        let configured_url = std::env::var("VELNOR_ACTIONS_CACHE_URL").ok();
        let has_job_token = env
            .iter()
            .any(|(name, value)| name == "ACTIONS_RUNTIME_TOKEN" && !value.is_empty());
        if let Some(url) = configured_cache_url(configured_url.as_deref(), has_job_token) {
            set_env(&mut env, "ACTIONS_CACHE_URL", url);
            env.push(("ACTIONS_CACHE_SERVICE_V2".to_string(), "True".to_string()));
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

    env
}

/// Derive the cache-authority handshake declared by admitted generated CI.
///
/// The generated `cache-contract` steps are the closed declaration surface.
/// Admission has already authenticated their repository/ref/input schema before
/// this function runs. Bind those declarations to the host reservation that is
/// held for the complete job; never accept workflow-provided `VELNOR_CACHE_*`
/// overrides as runtime authority.
pub(crate) fn cache_authority_env(
    job: &AgentJobRequestMessage,
    reserved_bytes: u64,
) -> Result<Vec<(String, String)>> {
    let base_env = job_runtime_env(job);
    let context_data = crate::runner::job_context_data(job);
    let mut env = Vec::new();
    let mut ids = BTreeSet::new();
    let mut total_peak = 0_u64;

    for step in &job.steps {
        if !step.enabled || !is_cache_contract_step(step) {
            continue;
        }
        let inputs = crate::action::string_inputs(step).context("read cache-contract inputs")?;
        let value = |name: &str| -> Result<String> {
            let raw = inputs
                .get(name)
                .with_context(|| format!("cache-contract missing input {name}"))?;
            Ok(crate::executor::render_expressions_with_context(
                raw,
                &base_env,
                &context_data,
            ))
        };
        let declaration = value("expected-declaration-sha256")?;
        if declaration.len() != 64
            || !declaration
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("cache-contract declaration identity is not lowercase SHA-256");
        }
        let id = value("expected-cache-id")?;
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !ids.insert(id.clone())
        {
            bail!("cache-contract cache identity is invalid or duplicated: {id}");
        }
        let peak = value("required-peak-bytes")?
            .parse::<u64>()
            .with_context(|| format!("cache-contract peak is not an integer for {id}"))?;
        if peak == 0 {
            bail!("cache-contract peak is zero for {id}");
        }
        total_peak = total_peak
            .checked_add(peak)
            .context("cache-contract total peak overflow")?;

        let prefix = id.replace('-', "_").to_ascii_uppercase();
        let repository = job
            .variable("github.repository")
            .context("cache-contract requires github.repository")?;
        let declared_scope = value("expected-scope")?;
        if declared_scope != "trusted" {
            bail!("cache-contract runtime authority requires trusted scope");
        }
        let declared_owner = value("expected-cache-owner")?;
        if declared_owner != repository {
            bail!("cache-contract owner does not match github.repository");
        }
        let reservation_id = value("expected-reservation-id")?;
        let materialization_id = value("expected-materialization-id")?;
        for (field, value) in [
            ("DECLARATION_SHA256", declaration),
            ("ID", id),
            ("SCOPE", declared_scope),
            ("OWNER", repository.to_string()),
            ("RESERVATION_ID", reservation_id),
            ("RESERVED_BYTES", peak.to_string()),
            ("ATTRIBUTED_BYTES", "0".to_string()),
            ("CLEANUP_STATE", "clean".to_string()),
            ("MATERIALIZATION_ID", materialization_id),
            ("LOCK_WAIT_MS", "0".to_string()),
        ] {
            env.push((format!("VELNOR_CACHE_{prefix}_{field}"), value));
        }
    }

    if total_peak > reserved_bytes {
        bail!(
            "cache-contract total peak {total_peak} exceeds held job reservation {reserved_bytes}"
        );
    }
    Ok(env)
}

fn is_cache_contract_step(step: &crate::job_message::ActionStep) -> bool {
    let Some(reference) = step.reference.as_ref() else {
        return false;
    };
    reference
        .name
        .as_deref()
        .is_some_and(|name| name.ends_with("/velnor-actions"))
        && reference
            .path
            .as_deref()
            .is_some_and(|path| path.trim_matches('/') == "actions/cache-contract")
}

pub(crate) fn job_environment_variables(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    job.environment_variables
        .iter()
        .flat_map(environment_token_pairs)
        .collect()
}

fn environment_token_pairs(value: &Value) -> Vec<(String, String)> {
    match value {
        Value::Object(object) => environment_object_pairs(object),
        Value::Array(values) => values.iter().flat_map(environment_token_pairs).collect(),
        _ => Vec::new(),
    }
}

fn environment_object_pairs(object: &Map<String, Value>) -> Vec<(String, String)> {
    if let (Some(name), Some(value)) = (object.get("name"), object.get("value"))
        && let Some(name) = environment_name(name)
    {
        return vec![(name.to_string(), environment_value(value))];
    }

    for pair_key in ["pairs", "mapping", "map"] {
        if let Some(Value::Array(pairs)) = object.get(pair_key) {
            return pairs.iter().flat_map(environment_pair_value).collect();
        }
    }

    object
        .iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
        .map(|(name, value)| (name.clone(), environment_value(value)))
        .collect()
}

fn environment_pair_value(value: &Value) -> Vec<(String, String)> {
    match value {
        Value::Object(object) => {
            if let (Some(key), Some(value)) = (
                object
                    .get("key")
                    .or_else(|| object.get("name"))
                    .or_else(|| object.get("Key")),
                object.get("value").or_else(|| object.get("Value")),
            ) && let Some(key) = environment_name(key)
            {
                return vec![(key.to_string(), environment_value(value))];
            }
            environment_object_pairs(object)
        }
        Value::Array(pair) if pair.len() == 2 => pair[0]
            .as_str()
            .or_else(|| environment_name(&pair[0]))
            .map(|key| vec![(key.to_string(), environment_value(&pair[1]))])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn environment_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.as_object().and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .and_then(environment_name)
        })
    })
}

fn environment_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Object(object) => {
            // Expression template tokens ({"expr": "...", "type": 3}) arrive
            // UNevaluated from the broker — `env: X: ${{ secrets.Y }}` is an
            // expr token. Render back to `${{ ... }}` so the executor's
            // expression resolver (which holds the secrets context) evaluates
            // it; returning "" here silently blanked every such variable and
            // skipped `if: env.X != ''` steps the GitHub lane runs.
            if let Some(expr) = object
                .get("expr")
                .or_else(|| object.get("Expr"))
                .and_then(Value::as_str)
            {
                return format!("${{{{ {expr} }}}}");
            }
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .map(environment_value)
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn is_protected_default_env(name: &str) -> bool {
    name.starts_with("GITHUB_")
        || name.starts_with("RUNNER_")
        || name.starts_with("ACTIONS_")
        || name == "AGENT_TOOLSDIRECTORY"
        || matches!(
            name,
            "MISE_LOCKFILE" | "MISE_LOCKED" | "MISE_LOCKED_VERIFY_PROVENANCE"
        )
        || name == "SCCACHE_BASEDIRS"
}

fn configured_cache_url(url: Option<&str>, has_job_token: bool) -> Option<&str> {
    url.filter(|url| has_job_token && !url.is_empty())
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
    keys: &[&str],
    env_name: &str,
) -> bool {
    if let Some(value) = map_get_any_case(&endpoint.data, keys).filter(|value| !value.is_empty()) {
        set_env(env, env_name, value);
        return true;
    }
    false
}

fn endpoint_access_token(endpoint: &crate::job_message::ServiceEndpoint) -> Option<&str> {
    endpoint.authorization.as_ref().and_then(|authorization| {
        map_get_any_case(
            &authorization.parameters,
            &["AccessToken", "accessToken", "ACCESSTOKEN"],
        )
    })
}

fn map_get_any_case<'a>(
    map: &'a std::collections::BTreeMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| map.get(*key).map(String::as_str))
        .or_else(|| {
            map.iter().find_map(|(name, value)| {
                keys.iter()
                    .any(|key| name.eq_ignore_ascii_case(key))
                    .then_some(value.as_str())
            })
        })
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
    std::env::var("VELNOR_RUNNER_NAME").unwrap_or_else(|_| "velnor".to_string())
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
            .and_then(|value| value.value.as_deref())
    }

    fn variable_bool(&self, name: &str) -> Option<bool> {
        self.variable(name).and_then(|value| match value {
            "true" | "True" | "TRUE" => Some(true),
            "false" | "False" | "FALSE" => Some(false),
            _ => None,
        })
    }

    fn job_name(&self) -> String {
        self.job_name
            .clone()
            .unwrap_or_else(|| self.job_display_name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_expr_tokens_render_as_templates_for_runtime_resolution() {
        // Broker sends `env: X: ${{ secrets.Y }}` as an UNevaluated expr token
        // (observed live: {"expr":"secrets.DOCKERHUB_USERNAME","type":3}).
        // Blanking it skipped `if: env.X != ''` steps the GitHub lane runs.
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "p" },
            "timeline": { "id": "t" },
            "jobId": "j",
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
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
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
                {
                    "CARGO_TERM_COLOR": "always",
                    "CARGO_INCREMENTAL": 0,
                    "GITHUB_REF": "refs/heads/evil",
                    "ACTIONS_RUNTIME_URL": "https://evil.actions.example",
                    "ACTIONS_CACHE_SERVICE_V2": "false",
                    "MISE_LOCKFILE": "0",
                    "MISE_LOCKED": "0",
                    "MISE_LOCKED_VERIFY_PROVENANCE": "false",
                    "SCCACHE_BASEDIRS": "/untrusted/override"
                },
                {
                    "pairs": [
                        { "key": "SCCACHE_DIR", "value": "/var/cache/sccache" },
                        ["CARGO_INCREMENTAL", "1"]
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

        let env = job_runtime_env(&job);

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
        assert!(env.contains(&("GITHUB_TOKEN".into(), "ghs_token".into())));
        assert!(env.contains(&("CARGO_TERM_COLOR".into(), "always".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "1".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "0".into())));
        assert!(env.contains(&("SCCACHE_CACHE_SIZE".into(), "20G".into())));
        assert!(env.contains(&("SCCACHE_BASEDIRS".into(), "/__w:/github/home".into())));
        assert!(!env.contains(&("SCCACHE_BASEDIRS".into(), "/untrusted/override".into())));
        assert!(env.contains(&("SCCACHE_DIR".into(), "/var/cache/sccache".into())));
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
    fn cache_endpoint_requires_the_job_runtime_token() {
        assert_eq!(configured_cache_url(Some("http://cache"), false), None);
        assert_eq!(configured_cache_url(Some(""), true), None);
        assert_eq!(configured_cache_url(None, true), None);
        assert_eq!(
            configured_cache_url(Some("http://cache"), true),
            Some("http://cache")
        );
    }

    #[test]
    fn derives_cache_authority_from_admitted_contract_and_held_reservation() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "velnor lane",
            "jobName": "velnor-lane",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "tailrocks/example" },
                "github.run_id": { "value": "123" }
            },
            "contextData": {
                "github": {
                    "repository": "tailrocks/example",
                    "run_id": "123",
                    "job": "velnor-lane"
                }
            },
            "steps": [{
                "enabled": true,
                "reference": {
                    "type": "Repository",
                    "name": "tailrocks/velnor-actions",
                    "ref": "3057391f93f3bfc0fe570ee08cfcea9533ea3f92",
                    "path": "actions/cache-contract"
                },
                "inputs": {
                    "expected-declaration-sha256": "4847dfb9b9b3197b7ee91dd917084b2448157a7aa41d9cb12b486a9e8e1493c6",
                    "expected-cache-id": "tools",
                    "expected-scope": "trusted",
                    "expected-cache-owner": "${{ github.repository }}",
                    "expected-reservation-id": "${{ github.run_id }}:${{ github.job }}:tools",
                    "expected-materialization-id": "${{ github.run_id }}-${{ github.job }}-tools",
                    "required-peak-bytes": "2147483648"
                }
            }]
        }))
        .unwrap();

        let env = cache_authority_env(&job, 32_212_254_720).unwrap();
        assert!(env.contains(&(
            "VELNOR_CACHE_TOOLS_DECLARATION_SHA256".into(),
            "4847dfb9b9b3197b7ee91dd917084b2448157a7aa41d9cb12b486a9e8e1493c6".into()
        )));
        assert!(env.contains(&(
            "VELNOR_CACHE_TOOLS_RESERVATION_ID".into(),
            "123:velnor-lane:tools".into()
        )));
        assert!(env.contains(&(
            "VELNOR_CACHE_TOOLS_MATERIALIZATION_ID".into(),
            "123-velnor-lane-tools".into()
        )));
        assert!(env.contains(&(
            "VELNOR_CACHE_TOOLS_RESERVED_BYTES".into(),
            "2147483648".into()
        )));
        assert!(env.contains(&(
            "VELNOR_CACHE_TOOLS_OWNER".into(),
            "tailrocks/example".into()
        )));
    }

    #[test]
    fn rejects_cache_authority_exceeding_held_reservation() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "velnor lane",
            "requestId": 1,
            "variables": { "github.repository": { "value": "tailrocks/example" } },
            "steps": [{
                "enabled": true,
                "reference": {
                    "type": "Repository",
                    "name": "tailrocks/velnor-actions",
                    "path": "actions/cache-contract"
                },
                "inputs": {
                    "expected-declaration-sha256": "4847dfb9b9b3197b7ee91dd917084b2448157a7aa41d9cb12b486a9e8e1493c6",
                    "expected-cache-id": "tools",
                    "expected-scope": "trusted",
                    "expected-cache-owner": "${{ github.repository }}",
                    "expected-reservation-id": "123:velnor-lane:tools",
                    "expected-materialization-id": "123-velnor-lane-tools",
                    "required-peak-bytes": "2147483648"
                }
            }]
        }))
        .unwrap();

        let error = cache_authority_env(&job, 1024).unwrap_err().to_string();
        assert!(error.contains("exceeds held job reservation"));
    }

    #[test]
    fn reads_runtime_endpoint_values_case_insensitively() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
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

        let env = job_runtime_env(&job);

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
    fn reads_run_service_typed_job_environment_maps() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1,
            "environmentVariables": [{
                "type": "map",
                "map": [
                    { "Key": { "lit": "CARGO_TERM_COLOR" }, "Value": { "lit": "always" } },
                    { "Key": { "lit": "CARGO_INCREMENTAL" }, "Value": { "value": 0 } },
                    { "Key": { "lit": "RENOVATE_ONBOARDING" }, "Value": { "value": false } },
                    { "Key": { "lit": "GITHUB_REF" }, "Value": { "lit": "refs/heads/evil" } }
                ]
            }]
        }))
        .unwrap();

        let env = job_runtime_env(&job);

        assert!(env.contains(&("CARGO_TERM_COLOR".into(), "always".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "0".into())));
        assert!(env.contains(&("RENOVATE_ONBOARDING".into(), "false".into())));
        assert!(!env.contains(&("GITHUB_REF".into(), "refs/heads/evil".into())));
    }
}

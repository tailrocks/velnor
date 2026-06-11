use crate::job_message::AgentJobRequestMessage;
use serde_json::{Map, Value};

pub fn job_runtime_env(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    let mut env = vec![
        ("CI".to_string(), "true".to_string()),
        ("GITHUB_ACTIONS".to_string(), "true".to_string()),
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
        ) {
            if let Some(token) = endpoint_access_token(endpoint) {
                set_env(&mut env, "ACTIONS_ID_TOKEN_REQUEST_TOKEN", token);
            }
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
    if let (Some(name), Some(value)) = (object.get("name"), object.get("value")) {
        if let Some(name) = environment_name(name) {
            return vec![(name.to_string(), environment_value(value))];
        }
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
            ) {
                if let Some(key) = environment_name(key) {
                    return vec![(key.to_string(), environment_value(value))];
                }
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
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .map(environment_value)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_protected_default_env(name: &str) -> bool {
    name.starts_with("GITHUB_")
        || name.starts_with("RUNNER_")
        || name.starts_with("ACTIONS_")
        || name == "AGENT_TOOLSDIRECTORY"
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
                    "ACTIONS_CACHE_SERVICE_V2": "false"
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

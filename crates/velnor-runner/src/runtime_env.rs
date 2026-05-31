use crate::job_message::AgentJobRequestMessage;
use serde_json::{Map, Value};

pub fn job_runtime_env(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    let mut env = vec![
        ("CI".to_string(), "true".to_string()),
        ("GITHUB_JOB".to_string(), job.job_name()),
        ("GITHUB_WORKSPACE".to_string(), "/__w".to_string()),
        ("RUNNER_OS".to_string(), "Linux".to_string()),
        (
            "RUNNER_ARCH".to_string(),
            std::env::consts::ARCH.to_string(),
        ),
        ("RUNNER_TEMP".to_string(), "/__t".to_string()),
        ("RUNNER_TOOL_CACHE".to_string(), "/__tool".to_string()),
    ];

    push_var(
        &mut env,
        "GITHUB_REPOSITORY",
        job.variable("github.repository"),
    );
    push_var(&mut env, "GITHUB_REF", job.variable("github.ref"));
    push_var_or_derived(
        &mut env,
        "GITHUB_REF_NAME",
        job.variable("github.ref_name"),
        job.variable("github.ref").map(ref_name),
    );
    push_var(&mut env, "GITHUB_SHA", job.variable("github.sha"));
    push_var(&mut env, "GITHUB_ACTOR", job.variable("github.actor"));
    push_var(&mut env, "GITHUB_WORKFLOW", job.variable("github.workflow"));
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
        "GITHUB_TOKEN",
        job.variable("system.github.token"),
    );

    if let Some(endpoint) = job.system_connection() {
        if let Some(url) = endpoint.url.as_deref() {
            env.push(("ACTIONS_RUNTIME_URL".to_string(), url.to_string()));
        }
        if let Some(token) = endpoint.authorization.as_ref().and_then(|authorization| {
            authorization
                .parameters
                .get("AccessToken")
                .or_else(|| authorization.parameters.get("accessToken"))
        }) {
            env.push(("ACTIONS_RUNTIME_TOKEN".to_string(), token.clone()));
        }
        push_endpoint_data(&mut env, endpoint, "CacheServerUrl", "ACTIONS_CACHE_URL");
        push_endpoint_data(
            &mut env,
            endpoint,
            "ResultsServiceUrl",
            "ACTIONS_RESULTS_URL",
        );
    }

    for (name, value) in job_environment_variables(job) {
        if is_protected_default_env(&name) {
            continue;
        }
        env.push((name, value));
    }

    env
}

fn job_environment_variables(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
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
        if let Some(name) = name.as_str() {
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
                if let Some(key) = key.as_str() {
                    return vec![(key.to_string(), environment_value(value))];
                }
            }
            environment_object_pairs(object)
        }
        Value::Array(pair) if pair.len() == 2 => pair[0]
            .as_str()
            .map(|key| vec![(key.to_string(), environment_value(&pair[1]))])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn environment_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => String::new(),
    }
}

fn is_protected_default_env(name: &str) -> bool {
    name.starts_with("GITHUB_") || name.starts_with("RUNNER_")
}

fn push_var(env: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
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

fn push_endpoint_data(
    env: &mut Vec<(String, String)>,
    endpoint: &crate::job_message::ServiceEndpoint,
    key: &str,
    env_name: &str,
) {
    if let Some(value) = endpoint
        .data
        .get(key)
        .or_else(|| endpoint.data.get(env_name))
    {
        env.push((env_name.to_string(), value.clone()));
    }
}

trait JobRuntimeExt {
    fn variable(&self, name: &str) -> Option<&str>;
    fn job_name(&self) -> String;
}

impl JobRuntimeExt for AgentJobRequestMessage {
    fn variable(&self, name: &str) -> Option<&str> {
        self.variables
            .get(name)
            .and_then(|value| value.value.as_deref())
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
                "github.ref": { "value": "refs/heads/main" },
                "github.sha": { "value": "abc123" },
                "github.workflow": { "value": "CI" },
                "system.github.token": { "value": "ghs_token", "isSecret": true }
            },
            "environmentVariables": [
                {
                    "CARGO_TERM_COLOR": "always",
                    "CARGO_INCREMENTAL": 0,
                    "GITHUB_REF": "refs/heads/evil"
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
                        "ResultsServiceUrl": "https://results.actions.example"
                    }
                }]
            }
        }))
        .unwrap();

        let env = job_runtime_env(&job);

        assert!(env.contains(&("GITHUB_JOB".into(), "check".into())));
        assert!(env.contains(&("GITHUB_REPOSITORY".into(), "acme/repo".into())));
        assert!(env.contains(&("GITHUB_REF_NAME".into(), "main".into())));
        assert!(env.contains(&("GITHUB_WORKFLOW".into(), "CI".into())));
        assert!(env.contains(&("GITHUB_TOKEN".into(), "ghs_token".into())));
        assert!(env.contains(&("CARGO_TERM_COLOR".into(), "always".into())));
        assert!(env.contains(&("CARGO_INCREMENTAL".into(), "1".into())));
        assert!(env.contains(&("SCCACHE_DIR".into(), "/var/cache/sccache".into())));
        assert!(env.contains(&("GITHUB_REF".into(), "refs/heads/main".into())));
        assert!(!env.contains(&("GITHUB_REF".into(), "refs/heads/evil".into())));
        assert!(env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(env.contains(&(
            "ACTIONS_CACHE_URL".into(),
            "https://cache.actions.example".into()
        )));
    }
}

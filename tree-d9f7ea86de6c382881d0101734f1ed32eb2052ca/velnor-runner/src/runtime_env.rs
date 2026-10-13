use crate::job_message::AgentJobRequestMessage;

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
    push_var(&mut env, "GITHUB_SHA", job.variable("github.sha"));
    push_var(&mut env, "GITHUB_ACTOR", job.variable("github.actor"));
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

    env
}

fn push_var(env: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        env.push((name.to_string(), value.to_string()));
    }
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
                "system.github.token": { "value": "ghs_token", "isSecret": true }
            },
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
        assert!(env.contains(&("GITHUB_TOKEN".into(), "ghs_token".into())));
        assert!(env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(env.contains(&(
            "ACTIONS_CACHE_URL".into(),
            "https://cache.actions.example".into()
        )));
    }
}

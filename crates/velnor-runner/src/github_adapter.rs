#![allow(dead_code)]

use crate::{
    container::JobContainerSpec,
    executor::ExecutableStep,
    job_message::{AgentJobRequestMessage, ServiceEndpoint},
    plan::{
        GitHubReportTarget, JobExecutionPlan, JobIdentity, NormalizedJobPlan,
        NormalizedRunDefaults, OutputExpression,
    },
};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn github_normalized_job_plan(
    job: &AgentJobRequestMessage,
    run_service_url: &str,
    billing_owner_id: Option<String>,
    job_container: JobContainerSpec,
    steps: Vec<ExecutableStep>,
    env: Vec<(String, String)>,
    context_data: Vec<(String, Value)>,
) -> NormalizedJobPlan {
    let services = job_container.services.clone();
    NormalizedJobPlan {
        identity: github_job_identity(job),
        github_report: Some(GitHubReportTarget {
            run_service_url: run_service_url.to_string(),
            billing_owner_id,
            system_connection_token: job
                .system_connection()
                .and_then(system_connection_access_token),
            timeline_id: Some(job.timeline.id.clone()),
            mask_values: github_mask_values(job),
        }),
        execution: JobExecutionPlan {
            runner_labels: Vec::new(),
            workspace_container: "/__w".to_string(),
            workspace_host: job_container.workspace_host.clone(),
            temp_host: job_container.temp_host.clone(),
            home_host: job_container.home_host.clone(),
            actions_host: job_container.actions_host.clone(),
            tools_host: job_container.tools_host.clone(),
            job_container,
            services,
            env,
            context_data,
            defaults: github_run_defaults(job),
        },
        steps,
        outputs: github_output_expressions(job.job_outputs.as_ref()),
    }
}

pub fn system_connection_access_token(endpoint: &ServiceEndpoint) -> Option<String> {
    endpoint
        .authorization
        .as_ref()
        .and_then(|authorization| authorization.parameters.get("AccessToken"))
        .cloned()
}

fn github_job_identity(job: &AgentJobRequestMessage) -> JobIdentity {
    JobIdentity {
        plan_id: job.plan.plan_id.clone(),
        job_id: job.job_id.clone(),
        request_id: Some(job.request_id.to_string()),
        name: job
            .job_name
            .clone()
            .unwrap_or_else(|| job.job_display_name.clone()),
        display_name: job.job_display_name.clone(),
        workflow_name: job_variable(job, "github.workflow").map(ToOwned::to_owned),
        repository: job_variable(job, "github.repository").map(ToOwned::to_owned),
        run_id: job_variable(job, "github.run_id").map(ToOwned::to_owned),
        run_attempt: job_variable(job, "github.run_attempt").map(ToOwned::to_owned),
    }
}

fn github_run_defaults(job: &AgentJobRequestMessage) -> NormalizedRunDefaults {
    let mut defaults = NormalizedRunDefaults::default();
    for value in &job.defaults {
        let Some(object) = value.as_object() else {
            continue;
        };
        let Some(run) = object
            .get("run")
            .or_else(|| object.get("Run"))
            .or_else(|| object.get("RUN"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        if let Some(shell) = run
            .get("shell")
            .or_else(|| run.get("Shell"))
            .and_then(Value::as_str)
        {
            defaults.shell = Some(shell.to_string());
        }
        if let Some(working_directory) = run
            .get("workingDirectory")
            .or_else(|| run.get("working-directory"))
            .or_else(|| run.get("WorkingDirectory"))
            .or_else(|| run.get("Working-Directory"))
            .and_then(Value::as_str)
        {
            defaults.working_directory = Some(working_directory.to_string());
        }
    }
    defaults
}

fn github_mask_values(job: &AgentJobRequestMessage) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(
        job.mask
            .iter()
            .filter_map(|mask| mask.value.as_deref())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    );
    values.extend(
        job.variables
            .values()
            .filter(|variable| variable.is_secret)
            .filter_map(|variable| variable.value.as_deref())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    );
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

fn github_output_expressions(job_outputs: Option<&Value>) -> BTreeMap<String, OutputExpression> {
    github_output_pairs(job_outputs)
        .into_iter()
        .map(|(name, value)| (name, OutputExpression { value }))
        .collect()
}

fn github_output_pairs(job_outputs: Option<&Value>) -> Vec<(String, String)> {
    match job_outputs {
        Some(Value::Object(outputs)) => {
            if outputs
                .get("type")
                .or_else(|| outputs.get("Type"))
                .is_some()
                && outputs.get("map").or_else(|| outputs.get("Map")).is_some()
            {
                return github_output_pairs(outputs.get("map").or_else(|| outputs.get("Map")));
            }
            outputs
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
                .filter_map(|(name, value)| {
                    github_output_expression(value).map(|value| (name.clone(), value.to_string()))
                })
                .collect()
        }
        Some(Value::Array(outputs)) => outputs
            .iter()
            .filter_map(github_output_pair_value)
            .collect(),
        _ => Vec::new(),
    }
}

fn github_output_pair_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Object(object) => {
            let key = object.get("Key").or_else(|| object.get("key"))?;
            let value = object.get("Value").or_else(|| object.get("value"))?;
            Some((
                github_output_name(key)?.to_string(),
                github_output_expression(value)?.to_string(),
            ))
        }
        Value::Array(pair) if pair.len() == 2 => Some((
            github_output_name(&pair[0])?.to_string(),
            github_output_expression(&pair[1])?.to_string(),
        )),
        _ => None,
    }
}

fn github_output_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.as_object().and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .and_then(github_output_name)
        })
    })
}

fn github_output_expression(value: &Value) -> Option<&str> {
    if let Some(value) = value.as_str() {
        return Some(value);
    }
    value
        .as_object()
        .and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("expression"))
                .or_else(|| object.get("Expression"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
        })
        .and_then(github_output_expression)
}

fn job_variable<'a>(job: &'a AgentJobRequestMessage, name: &str) -> Option<&'a str> {
    job.variables
        .get(name)
        .and_then(|value| value.value.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_adapter_builds_normalized_plan_metadata() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobName": "check",
            "jobDisplayName": "Check",
            "requestId": 42,
            "variables": {
                "github.workflow": { "value": "CI", "isSecret": false },
                "github.repository": { "value": "ChainArgos/java-monorepo", "isSecret": false },
                "github.run_id": { "value": "100", "isSecret": false },
                "github.run_attempt": { "value": "2", "isSecret": false },
                "system.github.token": { "value": "ghs_secret", "isSecret": true }
            },
            "mask": [{ "value": "mask-hint" }],
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "job-token" }
                    }
                }]
            },
            "defaults": [{ "run": { "shell": "bash", "working-directory": "packages/app" } }],
            "jobOutputs": {
                "image": { "value": "${{ steps.meta.outputs.tags }}" }
            }
        }))
        .unwrap();
        let root = std::env::temp_dir().join("velnor-github-plan-test");
        let container = JobContainerSpec {
            name: "velnor-job-job-1".into(),
            image: "ubuntu:24.04".into(),
            network: "velnor-net-job-1".into(),
            workspace_host: root.join("workspace"),
            temp_host: root.join("temp"),
            home_host: root.join("home"),
            actions_host: root.join("actions"),
            tools_host: root.join("tools"),
            mount_docker_socket: true,
            env: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            verify_bind_mounts: true,
        };
        let plan = github_normalized_job_plan(
            &job,
            "https://run.actions.githubusercontent.com/jobs/1/",
            Some("owner-1".into()),
            container,
            Vec::new(),
            vec![("GITHUB_ACTIONS".into(), "true".into())],
            Vec::new(),
        );

        assert_eq!(plan.identity.plan_id, "plan-1");
        assert_eq!(
            plan.identity.repository.as_deref(),
            Some("ChainArgos/java-monorepo")
        );
        assert_eq!(plan.identity.workflow_name.as_deref(), Some("CI"));
        assert_eq!(
            plan.github_report
                .as_ref()
                .unwrap()
                .billing_owner_id
                .as_deref(),
            Some("owner-1")
        );
        assert_eq!(
            plan.github_report
                .as_ref()
                .unwrap()
                .system_connection_token
                .as_deref(),
            Some("job-token")
        );
        assert!(plan
            .github_report
            .as_ref()
            .unwrap()
            .mask_values
            .contains(&"ghs_secret".to_string()));
        assert_eq!(plan.execution.defaults.shell.as_deref(), Some("bash"));
        assert_eq!(
            plan.outputs
                .get("image")
                .map(|output| output.value.as_str()),
            Some("${{ steps.meta.outputs.tags }}")
        );
    }
}

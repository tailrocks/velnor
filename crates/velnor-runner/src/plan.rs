#![allow(dead_code)]

use crate::{
    container::{JobContainerSpec, ServiceContainerSpec},
    executor::{ExecutableStep, JobExecutionSummary, StepExecutionResult, StepLog},
};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone)]
pub struct NormalizedJobPlan {
    pub identity: JobIdentity,
    pub github_report: Option<GitHubReportTarget>,
    pub execution: JobExecutionPlan,
    pub steps: Vec<ExecutableStep>,
    pub outputs: BTreeMap<String, OutputExpression>,
}

impl NormalizedJobPlan {
    pub fn is_github_reported(&self) -> bool {
        self.github_report.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobIdentity {
    pub plan_id: String,
    pub job_id: String,
    pub request_id: Option<String>,
    pub name: String,
    pub display_name: String,
    pub workflow_name: Option<String>,
    pub repository: Option<String>,
    pub run_id: Option<String>,
    pub run_attempt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubReportTarget {
    pub run_service_url: String,
    pub billing_owner_id: Option<String>,
    pub system_connection_token: Option<String>,
    pub timeline_id: Option<String>,
    pub mask_values: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct JobExecutionPlan {
    pub runner_labels: Vec<String>,
    pub workspace_container: String,
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub job_container: JobContainerSpec,
    pub services: Vec<ServiceContainerSpec>,
    pub env: Vec<(String, String)>,
    pub context_data: Vec<(String, Value)>,
    pub defaults: NormalizedRunDefaults,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedRunDefaults {
    pub shell: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputExpression {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedJobSummary {
    pub step_results: Vec<StepExecutionResult>,
    pub job_outputs: BTreeMap<String, String>,
    pub step_logs: Vec<StepLog>,
}

impl From<JobExecutionSummary> for NormalizedJobSummary {
    fn from(summary: JobExecutionSummary) -> Self {
        Self {
            step_results: summary.step_results,
            job_outputs: summary.job_outputs,
            step_logs: summary.step_logs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Shell;

    fn container_spec(root: &std::path::Path) -> JobContainerSpec {
        JobContainerSpec {
            name: "velnor-job".into(),
            image: "ubuntu:24.04".into(),
            network: "velnor-net".into(),
            workspace_host: root.join("workspace"),
            temp_host: root.join("temp"),
            home_host: root.join("home"),
            actions_host: root.join("actions"),
            tools_host: root.join("tools"),
            mount_docker_socket: false,
            env: Vec::new(),
            resource_options: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: true,
            daemon_id: "test-daemon".into(),
            cargo_target_host: None,
        }
    }

    #[test]
    fn normalized_plan_can_wrap_current_executor_step_surface() {
        let root = std::env::temp_dir().join("velnor-plan-test");
        let step = ExecutableStep::Script(crate::script_step::ScriptStep {
            id: "check".into(),
            display_name: String::new(),
            script: "cargo check".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        });
        let plan = NormalizedJobPlan {
            identity: JobIdentity {
                plan_id: "plan".into(),
                job_id: "job".into(),
                request_id: Some("request".into()),
                name: "check".into(),
                display_name: "Check".into(),
                workflow_name: Some("CI".into()),
                repository: Some("ChainArgos/java-monorepo".into()),
                run_id: Some("1".into()),
                run_attempt: Some("1".into()),
            },
            github_report: Some(GitHubReportTarget {
                run_service_url: "https://run.actions.githubusercontent.com/jobs/1/".into(),
                billing_owner_id: Some("42".into()),
                system_connection_token: Some("token".into()),
                timeline_id: Some("timeline".into()),
                mask_values: vec!["token".into()],
            }),
            execution: JobExecutionPlan {
                runner_labels: vec!["hetzner-sentry-ci".into()],
                workspace_container: "/__w".into(),
                workspace_host: root.join("workspace"),
                temp_host: root.join("temp"),
                home_host: root.join("home"),
                actions_host: root.join("actions"),
                tools_host: root.join("tools"),
                job_container: container_spec(&root),
                services: Vec::new(),
                env: Vec::new(),
                context_data: Vec::new(),
                defaults: NormalizedRunDefaults {
                    shell: Some("bash".into()),
                    working_directory: None,
                },
            },
            steps: vec![step],
            outputs: BTreeMap::new(),
        };

        assert!(plan.is_github_reported());
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(
            plan.identity.repository.as_deref(),
            Some("ChainArgos/java-monorepo")
        );
    }
}

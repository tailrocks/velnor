#![allow(dead_code)]

use crate::{
    container::{split_container_options, JobContainerSpec, ServiceContainerSpec},
    executor::ExecutableStep,
    job_message::{AgentJobRequestMessage, ContainerResource, ServiceEndpoint},
    plan::{
        GitHubReportTarget, JobExecutionPlan, JobIdentity, NormalizedJobPlan,
        NormalizedRunDefaults, OutputExpression,
    },
};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

pub struct GitHubJobContainerPaths {
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub docker_host_work_dir: Option<PathBuf>,
}

pub fn github_job_container_spec(
    job: &AgentJobRequestMessage,
    paths: GitHubJobContainerPaths,
    docker_image: &str,
    resource_options: Vec<String>,
    node_action_image: &str,
    daemon_id: String,
    trust_scope: &str,
) -> JobContainerSpec {
    // Opt-in persistent CARGO_TARGET_DIR. Buckets are scoped by the GitHub
    // trust boundary plus workflow/job class so warm state cannot cross repos
    // or unrelated workflows when an operator enables the speed-up per daemon.
    let cargo_target_host = std::env::var("VELNOR_CARGO_TARGET_PERSIST")
        .ok()
        .filter(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .map(|_| github_cargo_target_store_host(job, &paths.temp_host, trust_scope));
    JobContainerSpec {
        name: job_container_name(job),
        image: job_container_image(job).unwrap_or(docker_image).to_string(),
        network: job_network_name(job),
        workspace_host: paths.workspace_host,
        temp_host: paths.temp_host,
        home_host: paths.home_host,
        actions_host: paths.actions_host,
        tools_host: paths.tools_host,
        mount_docker_socket: github_trust_scope_allows_host_docker(trust_scope),
        env: job_container_env(job),
        resource_options,
        options: job_container_options(job),
        services: service_containers(job),
        node_action_image: node_action_image.to_string(),
        docker_cli_host_path: None,
        docker_cli_plugin_host_dir: None,
        docker_host_work_dir: paths.docker_host_work_dir,
        verify_bind_mounts: true,
        daemon_id,
        repository: job_variable(job, "github.repository").map(ToOwned::to_owned),
        cargo_target_host,
        compiler_cache_backend: crate::manifest::compiler_cache_backend(job),
    }
}

fn github_cargo_target_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
) -> PathBuf {
    let Some(repository) = job_variable(job, "github.repository").filter(|value| !value.is_empty())
    else {
        eprintln!(
            "forensics.lifecycle: persistent target store refused: missing github.repository"
        );
        return temp_host
            .join("_velnor/ephemeral/targets")
            .join(crate::container::sanitize_store_key(&job.job_id));
    };
    let workflow = job_variable(job, "github.workflow_ref")
        .and_then(|value| value.split('@').next())
        .and_then(|value| value.strip_prefix(&format!("{repository}/")))
        .or_else(|| job_variable(job, "github.workflow"))
        .filter(|value| !value.is_empty());
    let Some(workflow) = workflow else {
        eprintln!(
            "forensics.lifecycle: persistent target store refused: missing github.workflow_ref and github.workflow"
        );
        return temp_host
            .join("_velnor/ephemeral/targets")
            .join(crate::container::sanitize_store_key(&job.job_id));
    };
    crate::storage::append_legacy_trust(
        crate::container::cargo_target_store_host(temp_host),
        &cargo_target_trust_scope_from(Some(trust_scope)),
    )
    .join(crate::container::sanitize_store_key(repository))
    .join(crate::container::sanitize_store_key(workflow))
    .join(crate::container::sanitize_store_key(&job.job_display_name))
}

pub(crate) fn cargo_target_trust_scope() -> String {
    cargo_target_trust_scope_from(std::env::var("VELNOR_TRUST_SCOPE").ok().as_deref())
}

fn cargo_target_trust_scope_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("trusted")
        .to_string()
}

pub(crate) fn github_trust_scope_allows_host_docker(trust_scope: &str) -> bool {
    cargo_target_trust_scope_from(Some(trust_scope)).eq_ignore_ascii_case("trusted")
}

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

pub(crate) fn job_variable<'a>(job: &'a AgentJobRequestMessage, name: &str) -> Option<&'a str> {
    job.variables
        .get(name)
        .and_then(|value| value.value.as_deref())
}

pub fn job_container_name(job: &AgentJobRequestMessage) -> String {
    format!("velnor-job-{}", sanitize_path_segment(&job.job_id))
}

fn job_network_name(job: &AgentJobRequestMessage) -> String {
    format!("velnor-net-{}", sanitize_path_segment(&job.job_id))
}

fn job_container_image(job: &AgentJobRequestMessage) -> Option<&str> {
    job.job_container
        .as_ref()
        .and_then(container_image)
        .or_else(|| {
            job.resources
                .containers
                .iter()
                .find(|container| {
                    container
                        .alias
                        .as_deref()
                        .is_some_and(|alias| alias == "__job" || alias.eq_ignore_ascii_case("job"))
                })
                .and_then(|container| container.image.as_deref())
        })
}

fn job_container_env(job: &AgentJobRequestMessage) -> Vec<(String, String)> {
    job.job_container
        .as_ref()
        .into_iter()
        .flat_map(container_env)
        .collect()
}

fn job_container_options(job: &AgentJobRequestMessage) -> Vec<String> {
    let options = job
        .job_container
        .as_ref()
        .and_then(container_options)
        .unwrap_or_default();
    filter_privileged_container_options(options, privileged_container_options_allowed_from_env())
}

fn service_containers(job: &AgentJobRequestMessage) -> Vec<ServiceContainerSpec> {
    let network = job_network_name(job);
    job.resources
        .containers
        .iter()
        .filter_map(|container| {
            let alias = container.alias.as_deref()?;
            if alias == "__job" || alias.eq_ignore_ascii_case("job") {
                return None;
            }
            let image = container.image.as_ref()?.clone();
            Some(ServiceContainerSpec {
                name: format!(
                    "velnor-service-{}-{}",
                    sanitize_path_segment(&job.job_id),
                    sanitize_path_segment(alias)
                ),
                image,
                network_alias: alias.to_string(),
                network: network.clone(),
                env: service_env(container),
                ports: service_ports(container),
                options: container
                    .options
                    .as_deref()
                    .map(split_container_options)
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn service_env(container: &ContainerResource) -> Vec<(String, String)> {
    container
        .environment_variables
        .as_ref()
        .map(container_env_value)
        .unwrap_or_default()
}

fn service_ports(container: &ContainerResource) -> Vec<String> {
    let mut ports = container
        .ports
        .iter()
        .filter_map(|(container_port, host_port)| {
            let container_port = container_port.trim();
            let host_port = host_port.trim();
            if container_port.is_empty() {
                None
            } else if host_port.is_empty() {
                Some(container_port.to_string())
            } else {
                Some(format!("{host_port}:{container_port}"))
            }
        })
        .collect::<Vec<_>>();
    ports.sort();
    ports
}

fn container_image(value: &Value) -> Option<&str> {
    value
        .as_object()
        .and_then(|object| {
            object
                .get("image")
                .or_else(|| object.get("Image"))
                .or_else(|| object.get("containerImage"))
                .or_else(|| object.get("ContainerImage"))
        })
        .and_then(Value::as_str)
        .filter(|image| !image.is_empty())
}

fn container_options(value: &Value) -> Option<Vec<String>> {
    value
        .as_object()
        .and_then(|object| {
            object
                .get("options")
                .or_else(|| object.get("Options"))
                .or_else(|| object.get("createOptions"))
                .or_else(|| object.get("CreateOptions"))
        })
        .and_then(Value::as_str)
        .map(split_container_options)
}

fn privileged_container_options_allowed_from_env() -> bool {
    std::env::var("VELNOR_ALLOW_PRIVILEGED_OPTIONS")
        .ok()
        .is_some_and(|value| env_truthy(&value))
}

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn filter_privileged_container_options(
    options: Vec<String>,
    allow_privileged: bool,
) -> Vec<String> {
    if allow_privileged {
        return options;
    }

    let mut filtered = Vec::with_capacity(options.len());
    let mut index = 0;
    while index < options.len() {
        let option = options[index].as_str();
        match option {
            "--privileged" => {
                log_dropped_container_option(option, "privileged container");
                index += 1;
            }
            "--cap-add" | "--device" | "--security-opt" => {
                let consumed = option_with_optional_value(&options, index);
                log_dropped_container_option(&consumed, "host privilege or security relaxation");
                index += consumed_option_count(&options, index);
            }
            "--pid" => {
                if option_value_is(&options, index, "host") {
                    log_dropped_container_option(
                        &format!("{} {}", options[index], options[index + 1]),
                        "host PID namespace",
                    );
                    index += 2;
                } else {
                    filtered.push(options[index].clone());
                    index += 1;
                }
            }
            "--network" | "--net" => {
                if option_value_is(&options, index, "host") {
                    log_dropped_container_option(
                        &format!("{} {}", options[index], options[index + 1]),
                        "host network namespace",
                    );
                    index += 2;
                } else {
                    filtered.push(options[index].clone());
                    index += 1;
                }
            }
            "-v" | "--volume" => {
                if let Some(value) = options.get(index + 1) {
                    if volume_mount_has_host_source(value) {
                        log_dropped_container_option(
                            &format!("{} {}", options[index], value),
                            "host bind mount",
                        );
                        index += 2;
                    } else {
                        filtered.push(options[index].clone());
                        index += 1;
                    }
                } else {
                    filtered.push(options[index].clone());
                    index += 1;
                }
            }
            "--mount" => {
                if let Some(value) = options.get(index + 1) {
                    if mount_option_has_host_source(value) {
                        log_dropped_container_option(
                            &format!("{} {}", options[index], value),
                            "host bind mount",
                        );
                        index += 2;
                    } else {
                        filtered.push(options[index].clone());
                        index += 1;
                    }
                } else {
                    filtered.push(options[index].clone());
                    index += 1;
                }
            }
            _ if option.starts_with("--cap-add=")
                || option.starts_with("--device=")
                || option.starts_with("--security-opt=") =>
            {
                log_dropped_container_option(option, "host privilege or security relaxation");
                index += 1;
            }
            _ if option == "--pid=host" => {
                log_dropped_container_option(option, "host PID namespace");
                index += 1;
            }
            _ if option == "--network=host" || option == "--net=host" => {
                log_dropped_container_option(option, "host network namespace");
                index += 1;
            }
            _ if option.starts_with("--volume=") => {
                let value = option.trim_start_matches("--volume=");
                if volume_mount_has_host_source(value) {
                    log_dropped_container_option(option, "host bind mount");
                } else {
                    filtered.push(options[index].clone());
                }
                index += 1;
            }
            _ if option.starts_with("-v") && option.len() > 2 => {
                let value = option.trim_start_matches("-v").trim_start_matches('=');
                if volume_mount_has_host_source(value) {
                    log_dropped_container_option(option, "host bind mount");
                } else {
                    filtered.push(options[index].clone());
                }
                index += 1;
            }
            _ if option.starts_with("--mount=") => {
                let value = option.trim_start_matches("--mount=");
                if mount_option_has_host_source(value) {
                    log_dropped_container_option(option, "host bind mount");
                } else {
                    filtered.push(options[index].clone());
                }
                index += 1;
            }
            _ => {
                filtered.push(options[index].clone());
                index += 1;
            }
        }
    }
    filtered
}

fn option_with_optional_value(options: &[String], index: usize) -> String {
    if consumed_option_count(options, index) == 2 {
        format!("{} {}", options[index], options[index + 1])
    } else {
        options[index].clone()
    }
}

fn consumed_option_count(options: &[String], index: usize) -> usize {
    if options
        .get(index + 1)
        .is_some_and(|value| !value.starts_with('-'))
    {
        2
    } else {
        1
    }
}

fn option_value_is(options: &[String], index: usize, expected: &str) -> bool {
    options
        .get(index + 1)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn volume_mount_has_host_source(value: &str) -> bool {
    value
        .split_once(':')
        .map(|(source, _)| source.trim().starts_with('/'))
        .unwrap_or(false)
}

fn mount_option_has_host_source(value: &str) -> bool {
    value.split(',').any(|part| {
        let Some((key, source)) = part.split_once('=') else {
            return false;
        };
        matches!(key.trim(), "source" | "src") && source.trim().starts_with('/')
    })
}

fn log_dropped_container_option(option: &str, reason: &str) {
    eprintln!(
        "Velnor dropped privilege-granting container.options entry `{option}` ({reason}); set VELNOR_ALLOW_PRIVILEGED_OPTIONS=true only for trusted scopes to pass it through."
    );
}

fn container_env(value: &Value) -> Vec<(String, String)> {
    let Some(environment) = value.as_object().and_then(|object| {
        object
            .get("environmentVariables")
            .or_else(|| object.get("EnvironmentVariables"))
            .or_else(|| object.get("env"))
            .or_else(|| object.get("Env"))
    }) else {
        return Vec::new();
    };
    container_env_value(environment)
}

fn container_env_value(environment: &Value) -> Vec<(String, String)> {
    match environment {
        Value::Object(object) => object
            .iter()
            .map(|(name, value)| (name.clone(), scalar_env_value(value)))
            .collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| {
                let object = value.as_object()?;
                let name = object
                    .get("name")
                    .or_else(|| object.get("Name"))
                    .and_then(Value::as_str)?;
                let value = object.get("value").or_else(|| object.get("Value"))?;
                Some((name.to_string(), scalar_env_value(value)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn scalar_env_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => String::new(),
    }
}

fn host_docker_cli_path() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    find_executable_on_path("docker")
}

fn host_docker_cli_plugin_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    if let Some(path) = find_executable_on_path("docker-buildx") {
        return path.parent().map(std::path::Path::to_path_buf);
    }
    [
        "/usr/local/lib/docker/cli-plugins/docker-buildx",
        "/usr/local/libexec/docker/cli-plugins/docker-buildx",
        "/usr/lib/docker/cli-plugins/docker-buildx",
        "/usr/libexec/docker/cli-plugins/docker-buildx",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
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
            resource_options: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: true,
            daemon_id: "test-daemon".into(),
            repository: Some("ChainArgos/java-monorepo".into()),
            cargo_target_host: None,
            compiler_cache_backend: crate::compiler_cache::CompilerCacheBackend::Sccache,
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

    #[test]
    fn github_cargo_target_store_is_scoped_by_trust_repo_workflow_and_job() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobName": "test",
            "jobDisplayName": "Rust / test (ubuntu)",
            "requestId": 42,
            "variables": {
                "github.workflow": { "value": "CI / Preview", "isSecret": false },
                "github.repository": { "value": "ChainArgos/java-monorepo", "isSecret": false }
            }
        }))
        .unwrap();

        let host =
            github_cargo_target_store_host(&job, std::path::Path::new("/velnor/work"), "trusted");

        assert_eq!(
            host,
            std::path::PathBuf::from(
                "/velnor/work/_velnor_targets/trusted/ChainArgos_java-monorepo/CI___Preview/Rust___test__ubuntu_"
            )
        );
    }

    #[test]
    fn target_bucket_refuses_missing_repository_identity() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobDisplayName": "Rust",
            "requestId": 42
        }))
        .unwrap();
        let host = github_cargo_target_store_host(
            &job,
            std::path::Path::new("/velnor/work/job/temp"),
            "trusted",
        );
        assert_eq!(
            host,
            std::path::Path::new("/velnor/work/job/temp/_velnor/ephemeral/targets/job-1")
        );
        assert!(!host.to_string_lossy().contains("_velnor_targets"));
    }

    #[test]
    fn cargo_target_trust_scope_defaults_and_trims() {
        assert_eq!(cargo_target_trust_scope_from(None), "trusted");
        assert_eq!(cargo_target_trust_scope_from(Some("   ")), "trusted");
        assert_eq!(
            cargo_target_trust_scope_from(Some(" public-forks ")),
            "public-forks"
        );
    }

    #[test]
    fn non_trusted_scope_disables_host_docker_socket() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Public fork check",
            "requestId": 1
        }))
        .unwrap();
        let spec = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/tmp/workspace".into(),
                temp_host: "/tmp/temp".into(),
                home_host: "/tmp/home".into(),
                actions_host: "/tmp/actions".into(),
                tools_host: "/tmp/tools".into(),
                docker_host_work_dir: None,
            },
            "ubuntu:24.04",
            Vec::new(),
            "",
            "daemon".into(),
            "public-forks",
        );

        assert!(!spec.mount_docker_socket);
    }

    #[test]
    fn job_container_image_prefers_explicit_job_container() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": {
                "image": "ghcr.io/acme/job:latest"
            },
            "resources": {
                "containers": [{
                    "alias": "__job",
                    "image": "ubuntu:24.04"
                }]
            }
        }))
        .unwrap();

        assert_eq!(job_container_image(&job), Some("ghcr.io/acme/job:latest"));
    }

    #[test]
    fn job_container_image_uses_job_resource_container() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Container",
            "requestId": 1,
            "resources": {
                "containers": [{
                    "alias": "__job",
                    "image": "ghcr.io/acme/resource:latest"
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_image(&job),
            Some("ghcr.io/acme/resource:latest")
        );
    }

    #[test]
    fn job_container_env_reads_object_and_array_shapes() {
        let object_job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": {
                "environmentVariables": {
                    "NODE_OPTIONS": "--max-old-space-size=4096",
                    "CACHE_ENABLED": true,
                    "FETCH_DEPTH": 0,
                    "EMPTY_VALUE": null
                }
            }
        }))
        .unwrap();
        let array_job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": {
                "env": [
                    { "name": "RUST_LOG", "value": "debug" },
                    { "name": "RETRY_COUNT", "value": 3 },
                    { "name": "STRICT_MODE", "value": false }
                ]
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_env(&object_job),
            vec![
                ("CACHE_ENABLED".into(), "true".into()),
                ("EMPTY_VALUE".into(), "".into()),
                ("FETCH_DEPTH".into(), "0".into()),
                ("NODE_OPTIONS".into(), "--max-old-space-size=4096".into()),
            ]
        );
        assert_eq!(
            job_container_env(&array_job),
            vec![
                ("RUST_LOG".into(), "debug".into()),
                ("RETRY_COUNT".into(), "3".into()),
                ("STRICT_MODE".into(), "false".into()),
            ]
        );
    }

    #[test]
    fn job_container_options_read_create_options() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": {
                "createOptions": "--cpus 2 --memory 4g"
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_options(&job),
            vec!["--cpus", "2", "--memory", "4g"]
        );
    }

    #[test]
    fn container_options_drops_privileged() {
        let options = vec![
            "--hostname".to_string(),
            "job-host".to_string(),
            "--privileged".to_string(),
            "-v".to_string(),
            "/:/host".to_string(),
            "--mount".to_string(),
            "type=bind,source=/etc,target=/host-etc".to_string(),
            "--cap-add=ALL".to_string(),
            "--device".to_string(),
            "/dev/kvm".to_string(),
            "--pid".to_string(),
            "host".to_string(),
            "--network=host".to_string(),
            "--security-opt".to_string(),
            "seccomp=unconfined".to_string(),
            "--cpus".to_string(),
            "2".to_string(),
        ];

        assert_eq!(
            filter_privileged_container_options(options, false),
            vec!["--hostname", "job-host", "--cpus", "2"]
        );
    }

    #[test]
    fn container_options_allowed_when_trusted() {
        let options = vec![
            "--privileged".to_string(),
            "-v".to_string(),
            "/:/host".to_string(),
            "--hostname".to_string(),
            "job-host".to_string(),
        ];

        assert_eq!(
            filter_privileged_container_options(options.clone(), true),
            options
        );
    }

    #[test]
    fn service_containers_use_non_job_container_resources() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job/1",
            "jobDisplayName": "Services",
            "requestId": 1,
            "resources": {
                "containers": [
                    { "alias": "__job", "image": "ubuntu:24.04" },
                    {
                        "alias": "postgres",
                        "image": "postgres:16",
                        "options": "--health-cmd \"pg_isready -U postgres\"",
                        "environmentVariables": {
                            "POSTGRES_PASSWORD": "postgres"
                        },
                        "ports": { "5432": "5432" }
                    }
                ]
            }
        }))
        .unwrap();

        assert_eq!(
            service_containers(&job),
            vec![ServiceContainerSpec {
                name: "velnor-service-job_1-postgres".into(),
                image: "postgres:16".into(),
                network_alias: "postgres".into(),
                network: "velnor-net-job_1".into(),
                env: vec![("POSTGRES_PASSWORD".into(), "postgres".into())],
                ports: vec!["5432:5432".into()],
                options: vec!["--health-cmd".into(), "pg_isready -U postgres".into()],
            }]
        );
    }
}

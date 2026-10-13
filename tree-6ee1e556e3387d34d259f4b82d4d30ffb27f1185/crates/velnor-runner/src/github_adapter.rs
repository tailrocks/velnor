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
use std::{collections::BTreeMap, num::NonZeroU32, path::PathBuf};

/// Bump whenever target mounting or compiler-visible path semantics change.
/// Old generations remain inactive, owned cache data and are reclaimed by GC.
const CARGO_TARGET_GENERATION: &str = "workspace-v4-success-only";

pub struct GitHubJobContainerPaths {
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub docker_host_work_dir: Option<PathBuf>,
    pub execution_backend: velnor_model::ExecutionBackendKind,
}

pub fn github_job_container_spec(
    job: &AgentJobRequestMessage,
    paths: GitHubJobContainerPaths,
    docker_image: &str,
    resource_options: Vec<String>,
    slot_count: NonZeroU32,
    node_action_image: &str,
    daemon_id: String,
    trust_scope: &str,
) -> anyhow::Result<JobContainerSpec> {
    if paths.execution_backend == velnor_model::ExecutionBackendKind::MicroVm {
        crate::manifest::validate_microvm_compiler_cache(job)?;
    }
    let explicit_sccache = crate::manifest::declares_sccache(job);
    // Opt-in persistent workspace target directory. Buckets are scoped by the GitHub
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
    Ok(JobContainerSpec {
        name: job_container_name(job),
        image: job_container_image(job).unwrap_or(docker_image).to_string(),
        network: job_network_name(job),
        workspace_host: paths.workspace_host,
        temp_host: paths.temp_host.clone(),
        home_host: paths.home_host,
        actions_host: paths.actions_host,
        tools_host: paths.tools_host,
        mount_docker_socket: github_trust_scope_allows_host_docker(trust_scope)
            && paths.execution_backend.uses_host_docker_socket(),
        slot_count,
        env: backend_advertising_env(job_container_env(job), paths.execution_backend),
        resource_options,
        options: job_container_options(job, trust_scope),
        services: service_containers(job, trust_scope),
        node_action_image: node_action_image.to_string(),
        docker_cli_host_path: None,
        docker_cli_plugin_host_dir: None,
        docker_host_work_dir: paths.docker_host_work_dir,
        verify_bind_mounts: true,
        daemon_id,
        repository: job_variable(job, "github.repository").map(ToOwned::to_owned),
        cargo_target_host,
        store_trust_class: store_trust_class(trust_scope),
        sccache_store_host: (paths.execution_backend == velnor_model::ExecutionBackendKind::Docker
            && explicit_sccache)
            .then(|| github_sccache_store_host(job, &paths.temp_host, trust_scope)),
        mbx_store_host: (paths.execution_backend == velnor_model::ExecutionBackendKind::Docker
            && { !crate::manifest::declares_sccache(job) })
        .then(|| github_mbx_store_host(job, &paths.temp_host, trust_scope)),
    })
}

pub(crate) fn github_mbx_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
) -> PathBuf {
    github_rust_store_host(job, temp_host, trust_scope, "mbx")
}

fn github_sccache_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
) -> PathBuf {
    github_rust_store_host(job, temp_host, trust_scope, "sccache")
}

fn github_rust_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
    store: &str,
) -> PathBuf {
    let ephemeral = || {
        temp_host
            .join("_velnor/ephemeral")
            .join(store)
            .join(crate::container::sanitize_store_key(&job.job_id))
    };
    let Some(repository_id) = job_variable(job, "github.repository_id")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|id| *id != 0)
    else {
        eprintln!(
            "forensics.lifecycle: persistent {store} store refused: missing or invalid github.repository_id"
        );
        return ephemeral();
    };
    crate::storage::cache_class_path_for_trust(
        &crate::container::daemon_store_root(temp_host),
        match store_trust_class(trust_scope) {
            crate::container::StoreTrustClass::Trusted => "trusted",
            crate::container::StoreTrustClass::Release => "release",
            crate::container::StoreTrustClass::Untrusted => "untrusted",
        },
        &format!("compiler/{store}"),
        &format!("_velnor_{store}"),
    )
    .join(repository_id.to_string())
}

pub(crate) fn github_cargo_target_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
) -> PathBuf {
    let ephemeral = || {
        temp_host
            .join("_velnor/ephemeral/targets")
            .join(crate::container::sanitize_store_key(&job.job_id))
    };
    let Some(repository_raw) =
        job_variable(job, "github.repository").filter(|value| !value.trim().is_empty())
    else {
        eprintln!(
            "forensics.lifecycle: persistent target store refused: missing github.repository"
        );
        return ephemeral();
    };
    let Some(repository) = target_path_component(repository_raw) else {
        eprintln!(
            "forensics.lifecycle: persistent target store refused: invalid github.repository"
        );
        return ephemeral();
    };
    let workflow = job_variable(job, "github.workflow_ref")
        .and_then(|value| value.split('@').next())
        .and_then(|value| value.strip_prefix(&format!("{repository_raw}/")))
        .or_else(|| job_variable(job, "github.workflow"))
        .and_then(target_path_component);
    let Some(workflow) = workflow else {
        eprintln!(
            "forensics.lifecycle: persistent target store refused: missing github.workflow_ref and github.workflow"
        );
        return ephemeral();
    };
    let Some(job_name) = target_path_component(&job.job_display_name) else {
        eprintln!("forensics.lifecycle: persistent target store refused: invalid job display name");
        return ephemeral();
    };
    crate::storage::append_legacy_trust(
        crate::container::cargo_target_store_host(temp_host),
        crate::trust_scope::resolve(trust_scope).as_str(),
    )
    .join(CARGO_TARGET_GENERATION)
    .join(repository)
    .join(workflow)
    .join(job_name)
}

/// Convert one external identity into the existing one-directory-name form.
/// Slashes are encoded by the established store-key sanitizer, but traversal
/// components, controls, truncation, and empty identities fail closed instead
/// of silently aliasing another target bucket.
fn target_path_component(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
    {
        return None;
    }
    let key = crate::container::sanitize_store_key(value);
    let mut components = std::path::Path::new(&key).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return None;
    }
    Some(key)
}

/// The pool trust boundary this process resolved at startup.
///
/// There is no ambient read here any more: [`crate::trust_scope`] owns the
/// `VELNOR_TRUST_SCOPE` variable through clap, resolves it against the command
/// line exactly once, and hands the same answer to the capability gates and to
/// every trust-scoped store path.
pub(crate) fn cargo_target_trust_scope() -> String {
    crate::trust_scope::current()
}

pub(crate) fn github_trust_scope_allows_host_docker(trust_scope: &str) -> bool {
    trust_scope
        .trim()
        .eq_ignore_ascii_case(crate::trust_scope::TRUSTED)
}

pub(crate) fn store_trust_class(trust_scope: &str) -> crate::container::StoreTrustClass {
    if trust_scope.trim().eq_ignore_ascii_case("trusted") {
        crate::container::StoreTrustClass::Trusted
    } else if trust_scope.trim().eq_ignore_ascii_case("release") {
        crate::container::StoreTrustClass::Release
    } else {
        crate::container::StoreTrustClass::Untrusted
    }
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

/// Advertise the operator-selected pool backend to jobs as
/// `VELNOR_EXECUTION_BACKEND`. Repository-controlled env of the same name is
/// dropped first: a workflow must not spoof the pool's isolation level.
fn backend_advertising_env(
    mut env: Vec<(String, String)>,
    backend: velnor_model::ExecutionBackendKind,
) -> Vec<(String, String)> {
    env.retain(|(name, _)| name != "VELNOR_EXECUTION_BACKEND" && !is_docker_control_env(name));
    env.push((
        "VELNOR_EXECUTION_BACKEND".to_string(),
        backend.as_str().to_string(),
    ));
    env
}

fn job_container_options(job: &AgentJobRequestMessage, trust_scope: &str) -> Vec<String> {
    let options = job
        .job_container
        .as_ref()
        .and_then(container_options)
        .unwrap_or_default();
    filter_privileged_container_options(
        options,
        github_trust_scope_allows_host_docker(trust_scope)
            && privileged_container_options_allowed_from_env(),
    )
}

fn service_containers(
    job: &AgentJobRequestMessage,
    trust_scope: &str,
) -> Vec<ServiceContainerSpec> {
    let network = job_network_name(job);
    let allow_privileged = github_trust_scope_allows_host_docker(trust_scope)
        && privileged_container_options_allowed_from_env();
    if let Some(services) = job
        .job_service_containers
        .as_ref()
        .map(expand_template_token)
        .and_then(|value| value.as_object().cloned())
    {
        return services
            .into_iter()
            .filter_map(|(alias, container)| {
                let image = container_image(&container)?.to_string();
                Some(ServiceContainerSpec {
                    name: format!(
                        "velnor-service-{}-{}",
                        sanitize_path_segment(&job.job_id),
                        sanitize_path_segment(&alias)
                    ),
                    image,
                    network_alias: alias,
                    network: network.clone(),
                    env: container_env(&container),
                    ports: if github_trust_scope_allows_host_docker(trust_scope) {
                        container_ports(&container)
                    } else {
                        Vec::new()
                    },
                    options: filter_privileged_container_options(
                        container_options(&container).unwrap_or_default(),
                        allow_privileged,
                    ),
                })
            })
            .collect();
    }
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
                ports: if github_trust_scope_allows_host_docker(trust_scope) {
                    service_ports(container)
                } else {
                    Vec::new()
                },
                options: filter_privileged_container_options(
                    container
                        .options
                        .as_deref()
                        .map(split_container_options)
                        .unwrap_or_default(),
                    allow_privileged,
                ),
            })
        })
        .collect()
}

fn container_ports(value: &Value) -> Vec<String> {
    let mut ports = value
        .as_object()
        .and_then(|object| object.get("ports").or_else(|| object.get("Ports")))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    ports.sort();
    ports
}

/// Convert the current V2 broker TemplateToken JSON (`map` entries with
/// `Key`/`Value`) into ordinary JSON. `actions/runner` evaluates
/// `JobServiceContainers` directly; `Resources.Containers` is only the legacy
/// deserialization fallback retained for the old feature flag.
fn expand_template_token(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    if let Some(entries) = object
        .get("map")
        .or_else(|| object.get("Map"))
        .and_then(Value::as_array)
    {
        let mut expanded = serde_json::Map::new();
        for entry in entries {
            let Some(pair) = entry.as_object() else {
                continue;
            };
            let key = pair
                .get("key")
                .or_else(|| pair.get("Key"))
                .map(expand_template_token)
                .and_then(|value| value.as_str().map(ToOwned::to_owned));
            let value = pair
                .get("value")
                .or_else(|| pair.get("Value"))
                .map(expand_template_token);
            if let (Some(key), Some(value)) = (key, value) {
                expanded.insert(key, value);
            }
        }
        return Value::Object(expanded);
    }
    if let Some(sequence) = object
        .get("seq")
        .or_else(|| object.get("Seq"))
        .and_then(Value::as_array)
    {
        return Value::Array(sequence.iter().map(expand_template_token).collect());
    }
    if let Some(scalar) = object
        .get("lit")
        .or_else(|| object.get("Lit"))
        .or_else(|| object.get("value"))
        .or_else(|| object.get("Value"))
    {
        return expand_template_token(scalar);
    }
    Value::Object(
        object
            .iter()
            .map(|(key, value)| (key.clone(), expand_template_token(value)))
            .collect(),
    )
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
    if let Some(image) = value.as_str().filter(|image| !image.is_empty()) {
        return Some(image);
    }
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
        let mut filtered = Vec::with_capacity(options.len());
        let mut options = options.into_iter().peekable();
        while let Some(option) = options.next() {
            let option_name = option.as_str();
            if option_name == "--" {
                log_dropped_container_option(
                    option_name,
                    "Docker option terminator is runner-owned",
                );
            } else if option_name == "--name" {
                log_dropped_container_option(option_name, "container name is runner-owned");
                if options.peek().is_some_and(|value| !value.starts_with('-')) {
                    options.next();
                }
            } else if option_name.starts_with("--name=") {
                log_dropped_container_option(option_name, "container name is runner-owned");
            } else if matches!(option_name, "-e" | "--env") {
                let Some(value) = options.peek() else {
                    log_dropped_container_option(option_name, "missing Docker option value");
                    continue;
                };
                if value.starts_with('-') {
                    log_dropped_container_option(option_name, "missing Docker option value");
                } else if container_env_option_is_control(value) {
                    log_dropped_container_option(
                        option_name,
                        "Docker endpoint environment is runner-owned",
                    );
                    options.next();
                } else {
                    filtered.push(option);
                    if let Some(value) = options.next() {
                        filtered.push(value);
                    }
                }
            } else if option_name.starts_with("--env=")
                || option_name.starts_with("-e") && option_name.len() > 2
            {
                if option_name
                    .split_once('=')
                    .is_some_and(|(_, value)| container_env_option_is_control(value))
                    || option_name
                        .strip_prefix("-e")
                        .is_some_and(container_env_option_is_control)
                {
                    log_dropped_container_option(
                        option_name,
                        "Docker endpoint environment is runner-owned",
                    );
                } else {
                    filtered.push(option);
                }
            } else {
                filtered.push(option);
            }
        }
        return filtered;
    }

    let mut filtered = Vec::with_capacity(options.len());
    let mut index = 0;
    while index < options.len() {
        let option = options[index].as_str();
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        if safe_container_option(name) {
            if container_option_takes_value(name) && !option.contains('=') {
                let Some(value) = options.get(index + 1) else {
                    log_dropped_container_option(option, "missing Docker option value");
                    index += 1;
                    continue;
                };
                filtered.push(options[index].clone());
                filtered.push(value.clone());
                index += 2;
            } else {
                filtered.push(options[index].clone());
                index += 1;
            }
        } else {
            let consumed = option_with_optional_value(&options, index);
            log_dropped_container_option(
                &consumed,
                "Docker option is not in the untrusted allowlist",
            );
            index += consumed_option_count(&options, index);
        }
    }
    filtered
}

fn safe_container_option(name: &str) -> bool {
    matches!(
        name,
        "--cpus"
            | "--cpu-period"
            | "--cpu-quota"
            | "--cpu-shares"
            | "--cpuset-cpus"
            | "--cpuset-mems"
            | "--dns"
            | "--dns-option"
            | "--dns-search"
            | "--domainname"
            | "--entrypoint"
            | "--expose"
            | "--health-cmd"
            | "--health-interval"
            | "--health-retries"
            | "--health-start-interval"
            | "--health-start-period"
            | "--health-timeout"
            | "--hostname"
            | "--init"
            | "--memory"
            | "--memory-reservation"
            | "--memory-swap"
            | "--memory-swappiness"
            | "--no-healthcheck"
            | "--pids-limit"
            | "--read-only"
            | "--shm-size"
            | "--stop-signal"
            | "--stop-timeout"
            | "--ulimit"
            | "--user"
            | "-u"
            | "--workdir"
            | "-w"
    )
}

fn container_option_takes_value(name: &str) -> bool {
    !matches!(name, "--init" | "--no-healthcheck" | "--read-only")
}

fn container_env_option_is_control(value: &str) -> bool {
    is_docker_control_env(value.split_once('=').map_or(value, |(name, _)| name))
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
            .filter(|(name, _)| !is_docker_control_env(name))
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
                if is_docker_control_env(name) {
                    return None;
                }
                let value = object.get("value").or_else(|| object.get("Value"))?;
                Some((name.to_string(), scalar_env_value(value)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn is_docker_control_env(name: &str) -> bool {
    name.eq_ignore_ascii_case("DOCKER_HOST")
        || name.eq_ignore_ascii_case("DOCKER_CONTEXT")
        || name.eq_ignore_ascii_case("DOCKER_CONFIG")
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

    fn microvm_job() -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "MicroVM admission test",
            "requestId": 1
        }))
        .unwrap()
    }

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
            slot_count: NonZeroU32::MIN,
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
            store_trust_class: crate::container::StoreTrustClass::Trusted,
            mbx_store_host: None,
            sccache_store_host: None,
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
                "/velnor/work/_velnor_targets/trusted/workspace-v4-success-only/ChainArgos_java-monorepo/CI___Preview/Rust___test__ubuntu_"
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
    fn target_bucket_refuses_traversal_in_repository_workflow_or_job() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobDisplayName": "Rust",
            "requestId": 42,
            "variables": {
                "github.workflow": { "value": "../escape", "isSecret": false },
                "github.repository": { "value": "tailrocks/../escape", "isSecret": false }
            }
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
    }

    /// The split brain this test exists to keep closed: `VELNOR_TRUST_SCOPE`
    /// says `trusted` (the value the shipped systemd unit sets) while the
    /// operator hardens the pool with `--trust-scope public`. Before the fix
    /// the capability gates read the command line and the store paths read the
    /// variable, so a fork pull request ran without the Docker socket but wrote
    /// into the *trusted* cargo/mise stores that the next job mounts read-write
    /// onto its `PATH`. Every consumer must now observe `public`.
    #[test]
    fn every_consumer_observes_one_resolved_trust_scope() {
        let _serial = crate::trust_scope::test_support::serialized();

        let previous_scope = std::env::var_os("VELNOR_TRUST_SCOPE");
        // SAFETY: this synchronous test owns the process environment for the
        // body below (the trust-scope test guard serializes it against the
        // other tests that touch it) and restores the value before returning.
        // Nothing outside clap reads this variable any more, which is the whole
        // point of the change under test.
        unsafe {
            std::env::set_var("VELNOR_TRUST_SCOPE", "trusted");
        }

        // One parse, one resolution: clap owns the variable and the command
        // line beats it.
        let arg = crate::trust_scope::test_support::parse(&["--trust-scope", "public"]);
        assert_eq!(arg.trust_scope, "public");
        let resolved = arg.resolve();
        assert_eq!(resolved.as_str(), "public");

        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobDisplayName": "Rust",
            "requestId": 42,
            "jobContainer": { "image": "ubuntu:24.04", "options": "--privileged" },
            "jobServiceContainers": {
                "redis": {
                    "image": "redis:7",
                    "ports": ["6379:6379"],
                    "options": "--privileged"
                }
            },
            "variables": {
                "github.workflow": { "value": "CI", "isSecret": false },
                "github.repository": { "value": "ChainArgos/java-monorepo", "isSecret": false },
                "github.repository_id": { "value": "42", "isSecret": false }
            }
        }))
        .unwrap();

        let temp = std::path::Path::new("/velnor/work/job/temp");
        let spec = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/velnor/work/job/workspace".into(),
                temp_host: temp.into(),
                home_host: "/velnor/work/job/home".into(),
                actions_host: "/velnor/work/job/actions".into(),
                tools_host: "/velnor/work/job/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::Docker,
            },
            "ubuntu:24.04",
            Vec::new(),
            NonZeroU32::MIN,
            "",
            "daemon".into(),
            resolved.as_str(),
        )
        .unwrap();

        // The socket gate.
        assert!(!github_trust_scope_allows_host_docker(resolved.as_str()));
        assert!(!spec.mount_docker_socket);
        // Job container options.
        assert!(!spec.options.iter().any(|option| option == "--privileged"));
        // Service container privilege and host port publishing.
        let service = spec.services.first().expect("one service container");
        assert!(!service
            .options
            .iter()
            .any(|option| option == "--privileged"));
        assert!(service.ports.is_empty());
        // Store trust class.
        assert_eq!(
            spec.store_trust_class,
            crate::container::StoreTrustClass::Untrusted
        );

        // Every trust-scoped store path, including the six that used to read
        // the environment variable behind the gate's back. Stores named by the
        // raw scope carry a `public` component; stores named by the derived
        // trust class carry `untrusted`. Neither may ever carry `trusted`.
        assert_eq!(cargo_target_trust_scope(), "public");
        let scoped_by_raw_value = [
            github_cargo_target_store_host(&job, temp, resolved.as_str()),
            crate::container::cargo_executable_store_host(temp, "ChainArgos/java-monorepo"),
            crate::container::mise_executable_store_host(temp, "ChainArgos/java-monorepo"),
            crate::container::mise_binary_store_host(temp, "ChainArgos/java-monorepo"),
            crate::container::playwright_browser_store_host(temp, "ChainArgos/java-monorepo"),
            // The persistent actions cache, exactly as `executor.rs` composes it.
            crate::storage::append_legacy_trust(
                crate::storage::cache_class_path(temp, "caches", "_velnor_caches"),
                &cargo_target_trust_scope(),
            ),
        ];
        let scoped_by_trust_class = [spec.mbx_store_host.clone().expect("mbx store")];

        let has_component = |store: &std::path::Path, wanted: &str| {
            store
                .components()
                .any(|component| component.as_os_str() == wanted)
        };
        for store in scoped_by_raw_value
            .iter()
            .chain(scoped_by_trust_class.iter())
        {
            assert!(
                !has_component(store, "trusted"),
                "store path leaked the ambient VELNOR_TRUST_SCOPE value: {}",
                store.display()
            );
        }
        for store in &scoped_by_raw_value {
            assert!(
                has_component(store, "public"),
                "store path is not scoped to the resolved trust scope: {}",
                store.display()
            );
        }
        for store in &scoped_by_trust_class {
            assert!(
                has_component(store, "untrusted"),
                "store path is not scoped to the resolved trust class: {}",
                store.display()
            );
        }

        // SAFETY: restore the value owned by this synchronous test.
        unsafe {
            match previous_scope {
                Some(value) => std::env::set_var("VELNOR_TRUST_SCOPE", value),
                None => std::env::remove_var("VELNOR_TRUST_SCOPE"),
            }
        }
    }

    #[test]
    fn host_docker_requires_explicit_trusted_scope() {
        assert!(github_trust_scope_allows_host_docker("trusted"));
        assert!(github_trust_scope_allows_host_docker(" Trusted "));
        assert!(!github_trust_scope_allows_host_docker(""));
        assert!(!github_trust_scope_allows_host_docker("   "));
        assert!(!github_trust_scope_allows_host_docker("unknown"));
    }

    #[test]
    fn store_trust_class_mapping_is_explicit_and_fail_closed() {
        assert_eq!(
            store_trust_class("trusted"),
            crate::container::StoreTrustClass::Trusted
        );
        assert_eq!(
            store_trust_class(" release "),
            crate::container::StoreTrustClass::Release
        );
        assert_eq!(
            store_trust_class("public-forks"),
            crate::container::StoreTrustClass::Untrusted
        );
    }

    #[test]
    fn rust_stores_partition_by_repository_id() {
        let job = |repository_id: u64| {
            serde_json::from_value(serde_json::json!({
                "messageType": "RunnerJobRequest",
                "plan": { "planId": "plan" },
                "timeline": { "id": "timeline" },
                "jobId": "job",
                "jobDisplayName": "Rust",
                "requestId": 1,
                "variables": {
                    "github.repository_id": { "value": repository_id.to_string() }
                }
            }))
            .unwrap()
        };
        let temp = std::path::Path::new("/var/lib/velnor/work/slot-1/job/temp");

        assert_eq!(
            github_mbx_store_host(&job(41), temp, "trusted"),
            std::path::Path::new("/var/lib/velnor/work/_velnor_mbx/trusted/41")
        );
        assert_eq!(
            github_sccache_store_host(&job(42), temp, "trusted"),
            std::path::Path::new("/var/lib/velnor/work/_velnor_sccache/trusted/42")
        );
        assert_ne!(
            github_sccache_store_host(&job(41), temp, "trusted"),
            github_sccache_store_host(&job(42), temp, "trusted")
        );
    }

    #[test]
    fn rust_stores_are_ephemeral_without_valid_repository_id() {
        let job = microvm_job();
        let temp = std::path::Path::new("/velnor/work/job/temp");

        assert_eq!(
            github_mbx_store_host(&job, temp, "trusted"),
            temp.join("_velnor/ephemeral/mbx/job")
        );
        assert_eq!(
            github_sccache_store_host(&job, temp, "trusted"),
            temp.join("_velnor/ephemeral/sccache/job")
        );
    }

    #[test]
    fn untrusted_docker_job_gets_only_its_partition_and_no_host_docker() {
        let job = microvm_job();
        let spec = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/tmp/workspace".into(),
                temp_host: "/tmp/temp".into(),
                home_host: "/tmp/home".into(),
                actions_host: "/tmp/actions".into(),
                tools_host: "/tmp/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::Docker,
            },
            "ubuntu:24.04",
            Vec::new(),
            NonZeroU32::MIN,
            "",
            "daemon".into(),
            "public-forks",
        )
        .unwrap();

        assert!(!spec.mount_docker_socket);
        assert_eq!(
            spec.store_trust_class,
            crate::container::StoreTrustClass::Untrusted
        );
        assert!(spec.mbx_store_host.is_some());
        assert!(spec.sccache_store_host.is_none());
    }

    #[test]
    fn microvm_gets_no_host_acceleration_mounts() {
        let job = microvm_job();
        let spec = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/tmp/workspace".into(),
                temp_host: "/tmp/temp".into(),
                home_host: "/tmp/home".into(),
                actions_host: "/tmp/actions".into(),
                tools_host: "/tmp/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::MicroVm,
            },
            "ubuntu:24.04",
            Vec::new(),
            NonZeroU32::MIN,
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap();

        assert!(!spec.mount_docker_socket);
        assert!(spec.mbx_store_host.is_none());
        assert!(spec.sccache_store_host.is_none());
    }

    #[test]
    fn microvm_rejects_explicit_sccache_action() {
        let mut job = microvm_job();
        job.steps = vec![serde_json::from_value(serde_json::json!({
            "type": "Action",
            "reference": {
                "type": "Repository",
                "name": "mozilla-actions/sccache-action",
                "ref": "9e7fa8a12102821edf02ca5dbea1acd0f89a2696"
            }
        }))
        .unwrap()];

        let error = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/tmp/workspace".into(),
                temp_host: "/tmp/temp".into(),
                home_host: "/tmp/home".into(),
                actions_host: "/tmp/actions".into(),
                tools_host: "/tmp/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::MicroVm,
            },
            "ubuntu:24.04",
            Vec::new(),
            NonZeroU32::MIN,
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not support explicit"));
    }

    #[test]
    fn docker_preserves_compiler_cache_environment() {
        let mut job = microvm_job();
        job.job_container = Some(serde_json::json!({
            "environmentVariables": { "RUSTC_WRAPPER": "sccache" }
        }));

        let spec = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/tmp/workspace".into(),
                temp_host: "/tmp/temp".into(),
                home_host: "/tmp/home".into(),
                actions_host: "/tmp/actions".into(),
                tools_host: "/tmp/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::Docker,
            },
            "ubuntu:24.04",
            Vec::new(),
            NonZeroU32::MIN,
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap();

        assert!(spec
            .env
            .iter()
            .any(|(name, value)| name == "RUSTC_WRAPPER" && value == "sccache"));
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
    fn backend_advertising_env_overrides_repo_controlled_value() {
        let env = backend_advertising_env(
            vec![
                ("NODE_OPTIONS".to_string(), "x".to_string()),
                (
                    "VELNOR_EXECUTION_BACKEND".to_string(),
                    "microvm".to_string(),
                ),
            ],
            velnor_model::ExecutionBackendKind::Docker,
        );
        assert_eq!(
            env,
            vec![
                ("NODE_OPTIONS".to_string(), "x".to_string()),
                ("VELNOR_EXECUTION_BACKEND".to_string(), "docker".to_string()),
            ]
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
                    "EMPTY_VALUE": null,
                    "DOCKER_HOST": "tcp://attacker.example:2376"
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
                    { "name": "STRICT_MODE", "value": false },
                    { "name": "DOCKER_CONTEXT", "value": "attacker" }
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
            job_container_options(&job, "trusted"),
            vec!["--cpus", "2", "--memory", "4g"]
        );
    }

    #[test]
    fn container_options_drops_privileged() {
        let options = vec![
            "--hostname".to_string(),
            "job-host".to_string(),
            "--privileged".to_string(),
            "--privileged=true".to_string(),
            "-v".to_string(),
            "/:/host".to_string(),
            "--mount".to_string(),
            "type=bind,source=/etc,target=/host-etc".to_string(),
            "--cap-add=ALL".to_string(),
            "--device".to_string(),
            "/dev/kvm".to_string(),
            "--pid".to_string(),
            "host".to_string(),
            "--ipc=host".to_string(),
            "--cgroupns=host".to_string(),
            "--userns=host".to_string(),
            "--uts=host".to_string(),
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
    fn container_options_drop_runtime_and_host_control_variants() {
        let options = vec![
            "--runtime".to_string(),
            "runsc".to_string(),
            "--sysctl=kernel.unprivileged_userns_clone=1".to_string(),
            "--device-cgroup-rule".to_string(),
            "a *:* rwm".to_string(),
            "--privileged=true".to_string(),
            "--ipc=host".to_string(),
            "--cgroupns=host".to_string(),
            "--userns=host".to_string(),
            "--uts=host".to_string(),
        ];

        assert!(filter_privileged_container_options(options, false).is_empty());
    }

    #[test]
    fn container_options_drop_namespace_joins_relative_binds_and_shared_volumes() {
        let options = vec![
            "--pid".into(),
            "container:other".into(),
            "--ipc=container:other".into(),
            "--network=container:other".into(),
            "--volumes-from".into(),
            "other".into(),
            "--volumes-from=other".into(),
            "--mount".into(),
            "type=bind,source=.,target=/host".into(),
            "-v".into(),
            "./workspace:/host-workspace".into(),
            "--mount=type=volume,source=cache,target=/cache".into(),
        ];

        assert_eq!(
            filter_privileged_container_options(options, false),
            Vec::<String>::new()
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
    fn untrusted_container_options_use_explicit_allowlist() {
        let options = vec![
            "--cpus".into(),
            "2".into(),
            "--memory=4g".into(),
            "--health-cmd".into(),
            "true".into(),
            "--use-api-socket".into(),
            "--gpus=all".into(),
            "--env-file".into(),
            "/host/secrets".into(),
            "--label-file=/host/labels".into(),
            "--cidfile".into(),
            "/host/cid".into(),
            "--restart=always".into(),
            "--link".into(),
            "other:other".into(),
            "--storage-opt".into(),
            "size=100g".into(),
            "--log-driver".into(),
            "journald".into(),
            "--name=attacker-chosen".into(),
            "--volume-driver".into(),
            "nfs".into(),
        ];

        assert_eq!(
            filter_privileged_container_options(options, false),
            vec!["--cpus", "2", "--memory=4g", "--health-cmd", "true"]
        );
    }

    #[test]
    fn trusted_container_options_cannot_replace_runner_name() {
        let options = vec![
            "--name".into(),
            "attacker-chosen".into(),
            "--name=also-attacker-chosen".into(),
            "--env".into(),
            "DOCKER_HOST=tcp://attacker".into(),
            "--env=DOCKER_CONTEXT=remote".into(),
            "-eDOCKER_CONFIG=/host/config".into(),
            "--env".into(),
            "SAFE=value".into(),
            "--hostname".into(),
            "allowed".into(),
        ];

        assert_eq!(
            filter_privileged_container_options(options, true),
            vec!["--env", "SAFE=value", "--hostname", "allowed"]
        );
    }

    #[test]
    fn trusted_container_options_drop_missing_values_without_consuming_options() {
        let options = vec![
            "--name".into(),
            "--hostname".into(),
            "allowed".into(),
            "--name".into(),
            "-malformed-name".into(),
            "--env".into(),
            "--cpus=2".into(),
            "-e".into(),
            "--memory=4g".into(),
            "--env".into(),
        ];

        assert_eq!(
            filter_privileged_container_options(options, true),
            vec![
                "--hostname",
                "allowed",
                "-malformed-name",
                "--cpus=2",
                "--memory=4g"
            ]
        );
    }

    #[test]
    fn container_option_terminator_is_removed_even_when_trusted() {
        let options = vec!["--hostname".into(), "job-host".into(), "--".into()];

        assert_eq!(
            filter_privileged_container_options(options, true),
            vec!["--hostname", "job-host"]
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
                        "options": "--health-cmd \"pg_isready -U postgres\" --use-api-socket --gpus=all",
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
            service_containers(&job, "trusted"),
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

    #[test]
    fn service_containers_prefer_v2_job_service_tokens() {
        // Current V2 TemplateToken literals use `lit`; `value` is retained by
        // the decoder only as a compatibility fallback for primitive JSON.
        let scalar = |value: &str| serde_json::json!({ "type": 0, "lit": value });
        let service = serde_json::json!({
            "type": 2,
            "map": [
                { "Key": scalar("image"), "Value": scalar("postgres:16") },
                { "Key": scalar("ports"), "Value": { "type": 1, "seq": [scalar("5432")] } },
                { "Key": scalar("env"), "Value": { "type": 2, "map": [
                    { "Key": scalar("POSTGRES_PASSWORD"), "Value": scalar("postgres") }
                ] } }
            ]
        });
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Services",
            "requestId": 1,
            "jobServiceContainers": { "type": 2, "map": [
                { "Key": scalar("postgres"), "Value": service }
            ] },
            "resources": { "containers": [
                { "alias": "legacy", "image": "redis:7" }
            ] }
        }))
        .unwrap();

        let services = service_containers(&job, "trusted");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].network_alias, "postgres");
        assert_eq!(services[0].image, "postgres:16");
        assert_eq!(services[0].ports, vec!["5432"]);
        assert_eq!(
            services[0].env,
            vec![("POSTGRES_PASSWORD".into(), "postgres".into())]
        );
    }
}

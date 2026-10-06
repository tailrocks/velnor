#![allow(dead_code)]

use crate::{
    container::{
        has_attached_memory_limit_value, is_quota_flag, split_container_options, JobContainerSpec,
        ServiceContainerSpec,
    },
    executor::ExecutableStep,
    job_message::{template_token_context_value, AgentJobRequestMessage, ServiceEndpoint},
    plan::{
        GitHubReportTarget, JobExecutionPlan, JobIdentity, NormalizedJobPlan,
        NormalizedRunDefaults, OutputExpression,
    },
};
use anyhow::Context;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use velnor_model::{ContextValue, NonFinite};

const PACKAGED_WORKFLOW_CLI_APT: &str = "/usr/bin/velnor-workflow";

/// Host path of the apt-packaged `velnor-workflow` CLI when installed.
///
/// Dev and macOS hosts bootstrap the job image locally; they rely on the
/// image copy at `/usr/local/bin/velnor-workflow` instead of bind-mounting
/// from the host.
fn host_packaged_workflow_cli() -> Option<PathBuf> {
    packaged_workflow_cli_if_present(Path::new(PACKAGED_WORKFLOW_CLI_APT))
}

fn packaged_workflow_cli_if_present(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

pub struct GitHubJobContainerPaths {
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub docker_host_work_dir: Option<PathBuf>,
    pub execution_backend: velnor_model::ExecutionBackendKind,
    /// The owning daemon slot's store key; see
    /// [`JobContainerSpec::slot_store_key`].
    pub slot_store_key: Option<String>,
}

/// Build the job container spec for one admitted job.
///
/// `trust_scope` is the job's admitted scope (the pool ceiling narrowed by
/// the job's trust class at admission), never the raw pool flag: it decides
/// the Docker socket mount, privileged options, port publishing, and every
/// trust-scoped store path in the spec.
#[allow(clippy::too_many_arguments)]
pub fn github_job_container_spec(
    job: &AgentJobRequestMessage,
    paths: GitHubJobContainerPaths,
    docker_image: &str,
    node_action_image: &str,
    daemon_id: String,
    trust_scope: &str,
) -> anyhow::Result<JobContainerSpec> {
    if let Some(host_work_dir) = paths.docker_host_work_dir.as_deref()
        && !host_work_dir.is_absolute()
    {
        anyhow::bail!(
            "docker_host_work_dir must be an absolute path for Docker Desktop/OrbStack; got {}",
            host_work_dir.display()
        );
    }
    if let Some(container) = expanded_job_container(job)? {
        let _ = container_ports(&container)?;
    }
    if paths.execution_backend == velnor_model::ExecutionBackendKind::MicroVm {
        crate::manifest::validate_microvm_compiler_cache(job)?;
    }
    // Single decision point for the explicit-sccache compatibility mode:
    // on, the job gets the sccache store and no mbx; off, sccache is absent
    // from the whole spec (no store, mount, env, or PATH entry).
    let explicit_sccache = crate::sccache_compat::is_explicit(job);
    // The persistent Cargo target layer was deleted with wall-clock checkout
    // (BC-14): wall-clock sources made its mtime-based reuse always dirty,
    // and pinned mtimes made it accept stale artifacts as fresh. An operator
    // who still opts in must hear that loudly instead of silently losing the
    // feature: fail the spec, do not ignore the flag.
    if std::env::var("VELNOR_CARGO_TARGET_PERSIST")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    {
        anyhow::bail!(
            "VELNOR_CARGO_TARGET_PERSIST was removed with the persistent Cargo target layer; \
             unset it — compiler reuse is content-addressed (mbx/sccache) now"
        );
    }
    let name = job_container_name(job);
    let store_trust_scope = crate::trust_scope::normalize_scope(trust_scope).to_owned();
    let sccache_store_host = if paths.execution_backend
        == velnor_model::ExecutionBackendKind::Docker
        && explicit_sccache
    {
        Some(crate::sccache_compat::store_host(
            job,
            &paths.temp_host,
            trust_scope,
        )?)
    } else {
        None
    };
    let mbx_store_host = if paths.execution_backend == velnor_model::ExecutionBackendKind::Docker
        && !explicit_sccache
    {
        Some(github_mbx_store_host(job, &paths.temp_host, trust_scope)?)
    } else {
        None
    };
    Ok(JobContainerSpec {
        name,
        completion_generation: uuid::Uuid::new_v4(),
        image: job_container_image(job)?
            .as_deref()
            .unwrap_or(docker_image)
            .to_string(),
        network: job_network_name(job),
        workspace_host: paths.workspace_host,
        temp_host: paths.temp_host.clone(),
        home_host: paths.home_host,
        actions_host: paths.actions_host,
        tools_host: paths.tools_host,
        mount_docker_socket: github_trust_scope_allows_host_docker(trust_scope)
            && paths.execution_backend.uses_host_docker_socket(),
        slot_store_key: paths.slot_store_key,
        env: backend_advertising_env(job_container_env(job)?, paths.execution_backend),
        options: job_container_options(job, trust_scope)?,
        services: service_containers(job, trust_scope)?,
        node_action_image: node_action_image.to_string(),
        docker_cli_host_path: None,
        docker_cli_plugin_host_dir: None,
        packaged_workflow_cli_host: host_packaged_workflow_cli(),
        docker_host_work_dir: paths.docker_host_work_dir,
        verify_bind_mounts: true,
        daemon_id,
        repository: job_variable(job, "github.repository").map(ToOwned::to_owned),
        repository_store_key: github_repository_store_key(job),
        store_trust_scope,
        sccache_store_host,
        mbx_store_host,
    })
}

pub(crate) fn github_repository_store_key(job: &AgentJobRequestMessage) -> Option<String> {
    // The server URL defaults to the public host when the job carries none,
    // matching the checkout planner and trust derivation: a repository id
    // alone still addresses a persistent, origin-scoped store.
    let server_url = job_variable(job, "github.server_url").unwrap_or("https://github.com");
    crate::store_catalog::repository_store_key(
        server_url,
        job_variable(job, "github.repository_id")?,
    )
}

pub(crate) fn github_mbx_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
) -> anyhow::Result<PathBuf> {
    github_rust_store_host(job, temp_host, trust_scope, "mbx")
}

/// Shared layout for the two compiler-acceleration stores (mbx and the
/// explicit-sccache mode): repository-namespaced under the admitted scope,
/// ephemeral without a valid repository id. Which store a job gets is decided
/// once in [`github_job_container_spec`]; the sccache side resolves through
/// `crate::sccache_compat::store_host`.
pub(crate) fn github_rust_store_host(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
    store: &str,
) -> anyhow::Result<PathBuf> {
    github_rust_store_host_with_layout(job, temp_host, trust_scope, store, None)
}

fn github_rust_store_host_with_layout(
    job: &AgentJobRequestMessage,
    temp_host: &std::path::Path,
    trust_scope: &str,
    store: &str,
    layout: Option<&crate::storage::StorageLayout>,
) -> anyhow::Result<PathBuf> {
    let ephemeral = || {
        temp_host
            .join("_velnor/ephemeral")
            .join(store)
            .join(crate::container::sanitize_store_key(&job.job_id))
    };
    let Some(repository_key) = github_repository_store_key(job) else {
        eprintln!(
            "forensics.lifecycle: persistent {store} store refused: missing or invalid github.server_url or github.repository_id"
        );
        return Ok(ephemeral());
    };
    // Normalize once, without resolving: this is the job's admitted scope,
    // and re-resolving it through the process cell would hand back the pool.
    // The compiler stores are namespaced by that same scope — not collapsed
    // to a fixed class — so the mounts agree with the storage leases.
    let scope = crate::trust_scope::normalize_scope(trust_scope);
    Ok(
        crate::storage::cache_class_path_with_layout(scope, &format!("compiler/{store}"), layout)?
            .join(repository_key),
    )
}

/// Whether the scope in effect for a job unlocks host-level capability.
///
/// The argument is the job's admitted scope (the pool ceiling narrowed by the
/// job's trust class), never the raw pool flag: only the exact value
/// `trusted` passes, case-insensitively. Untrusted jobs — and jobs on
/// untrusted pools — get no host Docker socket, no privileged container
/// options, no host port publishing, and no user secrets.
pub fn github_trust_scope_allows_host_docker(trust_scope: &str) -> bool {
    trust_scope
        .trim()
        .eq_ignore_ascii_case(crate::trust_scope::TRUSTED)
}

pub fn github_normalized_job_plan(
    job: &AgentJobRequestMessage,
    run_service_url: &str,
    billing_owner_id: Option<String>,
    job_container: JobContainerSpec,
    steps: Vec<ExecutableStep>,
    env: Vec<(String, String)>,
    context_data: Vec<(String, ContextValue)>,
) -> anyhow::Result<NormalizedJobPlan> {
    let services = job_container.services.clone();
    Ok(NormalizedJobPlan {
        identity: github_job_identity(job),
        github_report: Some(GitHubReportTarget {
            run_service_url: run_service_url.to_string(),
            billing_owner_id,
            system_connection_token: job
                .system_connection_single_or_default()?
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
    })
}

pub fn system_connection_access_token(endpoint: &ServiceEndpoint) -> Option<String> {
    endpoint
        .authorization
        .as_ref()
        .and_then(|authorization| authorization.parameter_string("AccessToken"))
        .map(ToOwned::to_owned)
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
    for endpoint in &job.resources.endpoints {
        let Some(authorization) = endpoint.authorization.as_ref() else {
            continue;
        };
        values.extend(
            authorization
                .parameters
                .values()
                .filter_map(Option::as_deref)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        );
    }
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

pub(crate) fn job_container_name_for_id(job_id: &str) -> String {
    format!(
        "{}{sanitized}",
        crate::docker_lease::JOB_CONTAINER_NAME_PREFIX,
        sanitized = sanitize_path_segment(job_id)
    )
}

pub fn job_container_name(job: &AgentJobRequestMessage) -> String {
    job_container_name_for_id(&job.job_id)
}

fn job_network_name(job: &AgentJobRequestMessage) -> String {
    format!("velnor-net-{}", sanitize_path_segment(&job.job_id))
}

fn job_container_image(job: &AgentJobRequestMessage) -> anyhow::Result<Option<String>> {
    let Some(container) = expanded_job_container(job)? else {
        return Ok(None);
    };
    container_image(&container)
}

fn job_container_env(job: &AgentJobRequestMessage) -> anyhow::Result<Vec<(String, String)>> {
    let Some(container) = expanded_job_container(job)? else {
        return Ok(Vec::new());
    };
    container_env(&container)
}

fn expanded_job_container(job: &AgentJobRequestMessage) -> anyhow::Result<Option<ContextValue>> {
    let Some(template) = job.job_container.as_ref() else {
        return Ok(None);
    };
    reject_unsupported_template_container_fields(template)?;
    let container = expand_template_token(template)?;
    if matches!(container, ContextValue::Null) {
        return Ok(None);
    }
    validate_container_schema(&container, ContainerSchema::Job)?;
    Ok(Some(container))
}

/// Advertise the operator-selected pool backend to jobs as
/// `VELNOR_EXECUTION_BACKEND`, plus the runner build identity
/// (`VELNOR_SOURCE_SHA`, `VELNOR_MANIFEST_VERSION`). Repository-controlled
/// env of the same names is dropped first: a workflow must not spoof the
/// pool's isolation level or the release it runs under.
/// `GITHUB_*` is dropped too: container env merges under the authoritative
/// job env, so any `GITHUB_*` the broker omits would otherwise survive into
/// the immutable environment that derives the BuildKit trust tier.
fn backend_advertising_env(
    mut env: Vec<(String, String)>,
    backend: velnor_model::ExecutionBackendKind,
) -> Vec<(String, String)> {
    env.retain(|(name, _)| {
        name != "VELNOR_EXECUTION_BACKEND"
            && name != "VELNOR_SOURCE_SHA"
            && name != "VELNOR_MANIFEST_VERSION"
            && name != crate::runtime_env::VELNOR_HOST_ENV
            && name != crate::runtime_env::VELNOR_INSTANCE_ENV
            && name != crate::runtime_env::VELNOR_SLOT_ENV
            && !is_docker_control_env(name)
            && !is_runner_owned_env(name)
    });
    env.push((
        "VELNOR_EXECUTION_BACKEND".to_string(),
        backend.as_str().to_string(),
    ));
    env.push((
        "VELNOR_SOURCE_SHA".to_string(),
        env!("VELNOR_SOURCE_SHA").to_string(),
    ));
    env.push((
        "VELNOR_MANIFEST_VERSION".to_string(),
        crate::manifest::MANIFEST_VERSION.to_string(),
    ));
    env
}

/// Inject host/instance/slot after repository-controlled container env is
/// filtered. Values must already be non-secret composed identity fields.
pub(crate) fn push_runner_identity_env(
    env: &mut Vec<(String, String)>,
    identity: &crate::runtime_env::JobRunnerIdentity,
) {
    env.retain(|(name, _)| {
        name != crate::runtime_env::VELNOR_HOST_ENV
            && name != crate::runtime_env::VELNOR_INSTANCE_ENV
            && name != crate::runtime_env::VELNOR_SLOT_ENV
    });
    env.push((
        crate::runtime_env::VELNOR_HOST_ENV.to_string(),
        identity.host.clone(),
    ));
    env.push((
        crate::runtime_env::VELNOR_INSTANCE_ENV.to_string(),
        identity.instance.clone(),
    ));
    env.push((
        crate::runtime_env::VELNOR_SLOT_ENV.to_string(),
        identity.slot.clone(),
    ));
}

fn job_container_options(
    job: &AgentJobRequestMessage,
    trust_scope: &str,
) -> anyhow::Result<Vec<String>> {
    let options = match expanded_job_container(job)? {
        Some(container) => container_options(&container)?.unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(filter_privileged_container_options(
        options,
        github_trust_scope_allows_host_docker(trust_scope)
            && privileged_container_options_allowed_from_env(),
    ))
}

/// Names of the `services:` containers a job owns, for cancellation fan-out.
///
/// Same authority as the container spec below: a name registered here is the
/// name the spec starts, so the ladder signals a real container.
pub(crate) fn service_container_names(
    job: &AgentJobRequestMessage,
    trust_scope: &str,
) -> Vec<String> {
    match service_containers(job, trust_scope) {
        Ok(services) => services.into_iter().map(|service| service.name).collect(),
        Err(error) => {
            // The job-spec path validates these properties and fails the job
            // before starting containers. This earlier cancellation setup
            // cannot return an error through its existing caller.
            eprintln!("forensics.lifecycle: cannot derive service container names before spec validation: {error:#}");
            Vec::new()
        }
    }
}

fn service_containers(
    job: &AgentJobRequestMessage,
    trust_scope: &str,
) -> anyhow::Result<Vec<ServiceContainerSpec>> {
    // The wire DTO's OnDeserialized callback synthesizes this token from
    // Resources.Containers only when JobSidecarContainers is non-empty and
    // JobServiceContainers is absent/null. Do not enumerate resources here:
    // unrelated resources are not service containers.
    let network = job_network_name(job);
    let allow_privileged = github_trust_scope_allows_host_docker(trust_scope)
        && privileged_container_options_allowed_from_env();
    if let Some(template) = job.job_service_containers.as_ref() {
        reject_unsupported_template_service_fields(template)?;
    }
    let service_container_token = job
        .job_service_containers
        .as_ref()
        .map(expand_template_token)
        .transpose()?;
    let Some(service_container_token) = service_container_token.as_ref() else {
        return Ok(Vec::new());
    };
    if matches!(service_container_token, ContextValue::Null) {
        return Ok(Vec::new());
    }
    let ContextValue::Object {
        entries: services, ..
    } = service_container_token
    else {
        anyhow::bail!("job service containers must be a mapping");
    };

    let mut result = Vec::with_capacity(services.len());
    for (alias, container) in services {
        if alias.is_empty() {
            anyhow::bail!("job service container aliases must be non-empty strings");
        }
        validate_container_schema(container, ContainerSchema::Service)
            .with_context(|| format!("service container {alias:?}"))?;
        let image = container_image(container)?;
        let env = container_env(container)?;
        // Validate every workflow-schema field even if an empty image means
        // the runner will skip this service before launch.
        let ports = container_ports(container)?;
        let options = container_options(container)?.unwrap_or_default();
        let Some(image) = image else {
            // The pinned runner's service converter drops empty images,
            // including an empty `docker://` image, before launch.
            continue;
        };
        result.push(ServiceContainerSpec {
            name: format!(
                "velnor-service-{}-{}",
                sanitize_path_segment(&job.job_id),
                sanitize_path_segment(alias)
            ),
            image,
            network_alias: (*alias).clone(),
            network: network.clone(),
            env,
            ports: if github_trust_scope_allows_host_docker(trust_scope) {
                ports
            } else {
                Vec::new()
            },
            options: filter_privileged_container_options(options, allow_privileged),
        });
    }
    Ok(result)
}

fn container_ports(value: &ContextValue) -> anyhow::Result<Vec<String>> {
    let Some(value) = exact_object_member(value, "ports") else {
        return Ok(Vec::new());
    };
    let ContextValue::Array(values) = value else {
        anyhow::bail!("container ports must be a sequence");
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let port = context_scalar_string(value).with_context(|| {
                format!("container port at index {index} must be a scalar string")
            })?;
            if port.is_empty() {
                anyhow::bail!("container port at index {index} must be non-empty");
            }
            Ok(port)
        })
        .collect()
}

/// Convert V2 TemplateToken JSON into ordered context values. Keeping map
/// entries as vectors matches MappingToken's insertion order through service
/// and environment construction.
fn expand_template_token(value: &Value) -> anyhow::Result<ContextValue> {
    let Some(object) = value.as_object() else {
        return Ok(match value {
            Value::Null => ContextValue::Null,
            Value::Bool(value) => ContextValue::Bool(*value),
            Value::Number(value) => ContextValue::Number(value.clone()),
            Value::String(value) => ContextValue::String(value.clone()),
            Value::Array(values) => ContextValue::Array(
                values
                    .iter()
                    .map(expand_template_token)
                    .collect::<anyhow::Result<_>>()?,
            ),
            Value::Object(_) => anyhow::bail!("TemplateToken object reached scalar expansion"),
        });
    };
    if object_member(object, "type").is_none() {
        // Untagged objects are raw mappings, not string-literal tokens: every
        // template-token envelope carries an explicit integer `type`. Expand
        // member-wise like a mapping token so raw service/container maps keep
        // working without a parse round trip.
        let mut expanded: Vec<(String, ContextValue)> = Vec::with_capacity(object.len());
        for (key, member) in object {
            if key.is_empty() {
                anyhow::bail!("TemplateToken map key must be non-empty");
            }
            if expanded
                .iter()
                .any(|(existing, _)| velnor_model::ordinal_ignore_case_eq(existing, key))
            {
                anyhow::bail!("case-insensitive duplicate TemplateToken map key {key:?}");
            }
            expanded.push((key.clone(), expand_template_token(member)?));
        }
        return Ok(ContextValue::Object {
            case_sensitive: false,
            entries: expanded,
        });
    }
    let token_type = match object_member(object, "type") {
        None => Some(0),
        Some(Value::Number(value)) if !value.is_f64() => Some(
            i32::try_from(
                value
                    .as_i64()
                    .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                    .context("TemplateToken type must be an integer")?,
            )
            .context("TemplateToken type is outside Int32")?,
        ),
        Some(_) => anyhow::bail!("TemplateToken type must be an integer"),
    };
    match token_type {
        Some(0) => {
            let literal = match object_member(object, "lit") {
                Some(value) => template_scalar_string(value)
                    .context("TemplateToken string literal must be scalar")?,
                None => String::new(),
            };
            return Ok(ContextValue::String(literal));
        }
        Some(5) => {
            return match template_token_context_value(value)? {
                ContextValue::Bool(value) => Ok(ContextValue::Bool(value)),
                _ => anyhow::bail!("TemplateToken boolean must be boolean"),
            };
        }
        Some(6) => {
            return match template_token_context_value(value)? {
                ContextValue::Number(number) => Ok(ContextValue::Number(number)),
                ContextValue::NonFinite(value) => Ok(ContextValue::NonFinite(value)),
                _ => anyhow::bail!("TemplateToken number must be numeric"),
            };
        }
        Some(7) => return Ok(ContextValue::Null),
        Some(3 | 4) => anyhow::bail!("TemplateToken expression was not evaluated"),
        Some(1) => {
            let values = match object_member(object, "seq") {
                Some(Value::Array(values)) => values.as_slice(),
                Some(Value::Null) | None => &[],
                Some(_) => anyhow::bail!("TemplateToken sequence must be an array or null"),
            };
            return Ok(ContextValue::Array(
                values
                    .iter()
                    .map(expand_template_token)
                    .collect::<anyhow::Result<_>>()?,
            ));
        }
        Some(2) => {}
        Some(kind) => anyhow::bail!("unknown TemplateToken type {kind}"),
        None => anyhow::bail!("TemplateToken type is missing"),
    }
    if token_type == Some(2) {
        let entries = match object_member(object, "map") {
            Some(Value::Array(entries)) => entries.as_slice(),
            Some(Value::Null) | None => &[],
            Some(_) => anyhow::bail!("TemplateToken map must be an array or null"),
        };
        let mut expanded: Vec<(String, ContextValue)> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let pair = entry.as_object().with_context(|| {
                format!("TemplateToken map item at index {index} must be an object")
            })?;
            let key_token = object_member(pair, "key")
                .with_context(|| format!("TemplateToken map item at index {index} has no key"))?;
            let key_value = expand_template_token(key_token)?;
            if matches!(key_value, ContextValue::Null) {
                anyhow::bail!("TemplateToken map key at index {index} must not be null");
            }
            let key = context_scalar_string(&key_value).with_context(|| {
                format!("TemplateToken map key at index {index} must be a scalar")
            })?;
            if key.is_empty() {
                anyhow::bail!("TemplateToken map key at index {index} must be non-empty");
            }
            if expanded
                .iter()
                .any(|(existing, _)| velnor_model::ordinal_ignore_case_eq(existing, &key))
            {
                anyhow::bail!("case-insensitive duplicate TemplateToken map key {key:?}");
            }
            let mapped_value = object_member(pair, "value")
                .with_context(|| format!("TemplateToken map item at index {index} has no value"))
                .and_then(expand_template_token)?;
            expanded.push((key, mapped_value));
        }
        return Ok(ContextValue::Object {
            case_sensitive: false,
            entries: expanded,
        });
    }
    anyhow::bail!("unsupported TemplateToken shape")
}

#[derive(Clone, Copy)]
enum ContainerSchema {
    Job,
    Service,
}

fn validate_container_schema(value: &ContextValue, schema: ContainerSchema) -> anyhow::Result<()> {
    let ContextValue::Object {
        entries: object, ..
    } = value
    else {
        if matches!(
            value,
            ContextValue::Null
                | ContextValue::Bool(_)
                | ContextValue::Number(_)
                | ContextValue::BigInteger(_)
                | ContextValue::NonFinite(_)
                | ContextValue::String(_)
        ) {
            return Ok(());
        }
        anyhow::bail!("container must be a string-compatible scalar or mapping");
    };

    for (name, value) in object {
        match name.as_str() {
            "image" | "options" => {
                context_scalar_string(value)
                    .with_context(|| format!("container {name} must be a scalar string"))?;
            }
            "env" => {
                container_env_value(value)?;
            }
            "ports" => {
                validate_container_ports_sequence(value)?;
            }
            // TemplateToken preflight rejects these fields before nested
            // values are expanded. Keep this guard for typed ContextValue
            // paths; errors expose only the field name.
            "volumes" | "credentials" => {
                anyhow::bail!("container field {name:?} is not supported");
            }
            "entrypoint" | "command" if matches!(schema, ContainerSchema::Service) => {
                // The pinned runner gates these behind its disabled-by-default
                // ServiceContainerCommand feature. Velnor has no matching
                // feature switch, so fail instead of silently dropping them.
                anyhow::bail!("service container key {name:?} is not enabled");
            }
            _ => anyhow::bail!("unexpected container key {name:?}"),
        }
    }
    Ok(())
}

fn validate_container_ports_sequence(value: &ContextValue) -> anyhow::Result<()> {
    let ContextValue::Array(values) = value else {
        anyhow::bail!("container ports must be a sequence");
    };
    for (index, value) in values.iter().enumerate() {
        let port = context_scalar_string(value)
            .with_context(|| format!("container port at index {index} must be a scalar string"))?;
        if port.is_empty() {
            anyhow::bail!("container port at index {index} must be non-empty");
        }
    }
    Ok(())
}

fn context_object_entries(value: &ContextValue) -> Option<&[(String, ContextValue)]> {
    let ContextValue::Object { entries, .. } = value else {
        return None;
    };
    Some(entries)
}

fn exact_object_member<'a>(value: &'a ContextValue, name: &str) -> Option<&'a ContextValue> {
    context_object_entries(value)?
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
}

fn object_member<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// Reject unsupported service fields from their outer mapping tokens before
/// recursively expanding service/container values.
fn reject_unsupported_template_service_fields(value: &Value) -> anyhow::Result<()> {
    for pair in template_mapping_entries(value).into_iter().flatten() {
        let Some(pair) = pair.as_object() else {
            continue;
        };
        let Some(container) = object_member(pair, "value") else {
            continue;
        };
        reject_unsupported_template_container_fields(container)?;
    }
    Ok(())
}

/// Reject volumes and credentials by field presence, without expanding or
/// formatting their payloads. The schema field names are exact strings.
fn reject_unsupported_template_container_fields(value: &Value) -> anyhow::Result<()> {
    for pair in template_mapping_entries(value).into_iter().flatten() {
        let Some(pair) = pair.as_object() else {
            continue;
        };
        let Some(key) = object_member(pair, "key").and_then(template_string_literal) else {
            continue;
        };
        if matches!(key, "volumes" | "credentials") {
            anyhow::bail!("container field {key:?} is not supported");
        }
    }
    Ok(())
}

fn template_mapping_entries(value: &Value) -> Option<&[Value]> {
    let object = value.as_object()?;
    let Some(Value::Number(token_type)) = object_member(object, "type") else {
        return None;
    };
    if token_type.is_f64() || token_type.as_i64() != Some(2) {
        return None;
    }
    match object_member(object, "map") {
        Some(Value::Array(entries)) => Some(entries),
        _ => None,
    }
}

fn template_string_literal(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Object(object) => {
            if let Some(token_type) = object_member(object, "type")
                && !matches!(token_type, Value::Number(token_type) if !token_type.is_f64() && token_type.as_i64() == Some(0))
            {
                return None;
            }
            object_member(object, "lit")?.as_str()
        }
        _ => None,
    }
}

fn container_image(value: &ContextValue) -> anyhow::Result<Option<String>> {
    let image = if matches!(value, ContextValue::Object { .. }) {
        exact_object_member(value, "image").context("container mapping is missing image")?
    } else {
        value
    };
    let image = context_scalar_string(image).context("container image must be a scalar string")?;
    Ok(normalize_container_image(&image))
}

/// The pinned runner's PipelineTemplateConverter removes this exact prefix
/// from job and service container images before creating container specs.
fn normalize_container_image(image: &str) -> Option<String> {
    let image = image.strip_prefix("docker://").unwrap_or(image);
    (!image.is_empty()).then(|| image.to_owned())
}

fn container_options(value: &ContextValue) -> anyhow::Result<Option<Vec<String>>> {
    let Some(options) = exact_object_member(value, "options") else {
        return Ok(None);
    };
    let options =
        context_scalar_string(options).context("container options must be a scalar string")?;
    Ok(Some(split_container_options(&options)))
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
    let options = strip_quota_container_options(options);
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

/// Drop quota flags (and their values) from admitted workflow options.
/// Runs before the trust split so trusted and untrusted lanes strip
/// identically to emission filtering; every drop is logged, never silent.
fn strip_quota_container_options(options: Vec<String>) -> Vec<String> {
    let mut stripped = Vec::with_capacity(options.len());
    let mut index = 0;
    while index < options.len() {
        let option = options[index].as_str();
        if is_quota_flag(option) {
            // `--flag=value` and `-m<value>` carry values inline; only the
            // bare quota-flag form consumes the following token.
            let value_is_attached = option.contains('=') || has_attached_memory_limit_value(option);
            let consumed = if value_is_attached {
                option.to_owned()
            } else {
                option_with_optional_value(&options, index)
            };
            log_dropped_container_option(&consumed, "CPU/RAM/PID ceilings are not admitted");
            index += if value_is_attached {
                1
            } else {
                consumed_option_count(&options, index)
            };
            continue;
        }
        stripped.push(options[index].clone());
        index += 1;
    }
    stripped
}

fn safe_container_option(name: &str) -> bool {
    matches!(
        name,
        "--dns"
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
            | "--no-healthcheck"
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

fn container_env(value: &ContextValue) -> anyhow::Result<Vec<(String, String)>> {
    let Some(environment) = exact_object_member(value, "env") else {
        return Ok(Vec::new());
    };
    container_env_value(environment)
}

fn container_env_value(environment: &ContextValue) -> anyhow::Result<Vec<(String, String)>> {
    let object = context_object_entries(environment).context("container env must be a mapping")?;
    let mut values = Vec::with_capacity(object.len());
    for (name, value) in object {
        if name.is_empty() {
            anyhow::bail!("container env names must be non-empty strings");
        }
        // TemplateEvaluator stringifies scalar values before the runner drops
        // runner-owned names. Complex values fail StringDefinition validation.
        let value = context_scalar_string(value)
            .with_context(|| format!("container env value for {name:?} must be a scalar string"))?;
        if !is_docker_control_env(name) && !is_runner_owned_env(name) {
            values.push((name.clone(), value));
        }
    }
    Ok(values)
}

/// Runner-owned env names that repository-controlled container env must not
/// set. `GITHUB_*` (including the four BuildKit tier signals `GITHUB_REF`,
/// `GITHUB_REF_TYPE`, `GITHUB_EVENT_NAME`, `GITHUB_REF_PROTECTED`) merges into
/// the immutable job environment under the authoritative job values, so a
/// broker-omitted signal would otherwise let a workflow spoof the tier and
/// share a release daemon's ID-keyed cache mounts.
fn is_runner_owned_env(name: &str) -> bool {
    name.get(.."GITHUB_".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GITHUB_"))
}

fn is_docker_control_env(name: &str) -> bool {
    name.eq_ignore_ascii_case("DOCKER_HOST")
        || name.eq_ignore_ascii_case("DOCKER_CONTEXT")
        || name.eq_ignore_ascii_case("DOCKER_CONFIG")
}

/// Convert a wire scalar literal with the runner's invariant number format.
fn template_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) if value.is_f64() => value.as_f64().map(runner_number_to_string),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// Match TemplateEvaluator's scalar-to-string conversion for StringDefinition.
/// Complex values fail; null becomes the empty string.
fn context_scalar_string(value: &ContextValue) -> Option<String> {
    match value {
        ContextValue::Null => Some(String::new()),
        ContextValue::Bool(value) => Some(value.to_string()),
        ContextValue::Number(value) if value.is_f64() => {
            value.as_f64().map(runner_number_to_string)
        }
        ContextValue::Number(value) => Some(value.to_string()),
        ContextValue::BigInteger(value) => Some(value.clone()),
        ContextValue::NonFinite(NonFinite::NaN) => Some("NaN".to_owned()),
        ContextValue::NonFinite(NonFinite::PositiveInfinity) => Some("Infinity".to_owned()),
        ContextValue::NonFinite(NonFinite::NegativeInfinity) => Some("-Infinity".to_owned()),
        ContextValue::String(value) => Some(value.clone()),
        ContextValue::Undefined
        | ContextValue::Array(_)
        | ContextValue::Constructor { .. }
        | ContextValue::Object { .. } => None,
    }
}

/// Match `NumberToken.ToString()`'s `G15` invariant formatting.
fn runner_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:.14e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return value.to_string();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return value.to_string();
    };
    let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');

    if !(-4..15).contains(&exponent) {
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}E{exponent_sign}{:02}", exponent.abs());
    }

    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let decimal_index = exponent + 1;
    let mut result = if decimal_index <= 0 {
        format!(
            "0.{}{}",
            "0".repeat(decimal_index.unsigned_abs() as usize),
            digits
        )
    } else if decimal_index as usize >= digits.len() {
        format!(
            "{}{}",
            digits,
            "0".repeat(decimal_index as usize - digits.len())
        )
    } else {
        let decimal_index = decimal_index as usize;
        format!("{}.{}", &digits[..decimal_index], &digits[decimal_index..])
    };
    if result.contains('.') {
        while result.ends_with('0') {
            result.pop();
        }
        if result.ends_with('.') {
            result.pop();
        }
    }
    format!("{sign}{result}")
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
    use crate::job_message::ContainerResource;
    use crate::job_message::WireAgentJobRequestMessage;

    #[test]
    fn packaged_workflow_cli_is_omitted_when_apt_file_is_missing() {
        let missing = Path::new("/tmp/velnor-missing-workflow-cli-does-not-exist");
        assert!(!missing.is_file());
        assert_eq!(packaged_workflow_cli_if_present(missing), None);
    }

    #[test]
    fn packaged_workflow_cli_is_used_when_apt_file_exists() {
        let path = std::env::temp_dir().join(format!(
            "velnor-workflow-cli-present-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"cli").unwrap();
        let found = packaged_workflow_cli_if_present(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(found, Some(path));
    }

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

    fn template_string_token(value: &str) -> Value {
        serde_json::json!({ "type": 0, "lit": value })
    }

    fn template_number_token(value: Value) -> Value {
        serde_json::json!({ "type": 6, "num": value })
    }

    fn template_sequence_token(values: Vec<Value>) -> Value {
        serde_json::json!({ "type": 1, "seq": values })
    }

    fn template_map_token(entries: Vec<(Value, Value)>) -> Value {
        serde_json::json!({
            "type": 2,
            "map": entries
                .into_iter()
                .map(|(key, value)| serde_json::json!({ "key": key, "value": value }))
                .collect::<Vec<_>>()
        })
    }

    fn context_value(value: Value) -> ContextValue {
        ContextValue::from_json(value).unwrap()
    }

    fn valid_service_container_token() -> Value {
        template_map_token(vec![(
            template_string_token("image"),
            template_string_token("alpine:3.20"),
        )])
    }

    fn test_container_paths() -> GitHubJobContainerPaths {
        GitHubJobContainerPaths {
            workspace_host: PathBuf::from("/tmp/velnor-test-workspace"),
            temp_host: PathBuf::from("/tmp/velnor-test-temp"),
            home_host: PathBuf::from("/tmp/velnor-test-home"),
            actions_host: PathBuf::from("/tmp/velnor-test-actions"),
            tools_host: PathBuf::from("/tmp/velnor-test-tools"),
            docker_host_work_dir: None,
            execution_backend: velnor_model::ExecutionBackendKind::Docker,
            slot_store_key: None,
        }
    }

    #[test]
    fn github_mask_values_include_endpoint_authorization_values() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": {
                            "AccessToken": "endpoint-only-secret",
                            "Empty": "",
                            "Null": null
                        }
                    }
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            github_mask_values(&job),
            vec!["endpoint-only-secret".to_string()]
        );
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
                        "parameters": { "aCcEsStOkEn": "job-token" }
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
            completion_generation: uuid::Uuid::new_v4(),
            image: "ubuntu:24.04".into(),
            network: "velnor-net-job-1".into(),
            workspace_host: root.join("workspace"),
            temp_host: root.join("temp"),
            home_host: root.join("home"),
            actions_host: root.join("actions"),
            tools_host: root.join("tools"),
            mount_docker_socket: true,
            slot_store_key: None,
            env: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            packaged_workflow_cli_host: None,
            docker_host_work_dir: None,
            verify_bind_mounts: true,
            daemon_id: "test-daemon".into(),
            repository: Some("ChainArgos/java-monorepo".into()),
            repository_store_key: crate::store_catalog::repository_store_key(
                "https://github.com",
                "42",
            ),
            store_trust_scope: "trusted".to_owned(),
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
        )
        .unwrap();

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
    fn removed_cargo_target_persist_flag_fails_loudly() {
        let _serial = crate::trust_scope::test_support::serialized();
        let previous = std::env::var_os("VELNOR_CARGO_TARGET_PERSIST");
        // SAFETY: this synchronous test owns the process environment for the
        // body below (serialized against the other tests that touch it) and
        // restores the value before returning.
        unsafe {
            std::env::set_var("VELNOR_CARGO_TARGET_PERSIST", "1");
        }
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan-1" },
            "timeline": { "id": "timeline-1" },
            "jobId": "job-1",
            "jobDisplayName": "Rust",
            "requestId": 42
        }))
        .unwrap();
        let error = github_job_container_spec(
            &job,
            GitHubJobContainerPaths {
                workspace_host: "/velnor/work/job/workspace".into(),
                temp_host: "/velnor/work/job/temp".into(),
                home_host: "/velnor/work/job/home".into(),
                actions_host: "/velnor/work/job/actions".into(),
                tools_host: "/velnor/work/job/tools".into(),
                docker_host_work_dir: None,
                execution_backend: velnor_model::ExecutionBackendKind::Docker,
                slot_store_key: None,
            },
            "ubuntu:24.04",
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was removed with the persistent Cargo target layer"),
            "{error:#}"
        );
        // SAFETY: restore the value owned by this synchronous test.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("VELNOR_CARGO_TARGET_PERSIST", value),
                None => std::env::remove_var("VELNOR_CARGO_TARGET_PERSIST"),
            }
        }
    }

    /// The split brain this test exists to keep closed: `VELNOR_TRUST_SCOPE`
    /// says `trusted` (the value the shipped systemd unit sets) while the
    /// operator hardens the pool with `--trust-scope public`. Before the fix
    /// the capability gates read the command line and the store paths read the
    /// variable, so a fork pull request ran without the Docker socket but wrote
    /// into the *trusted* cargo/mise stores that the next job mounts read-write
    /// onto its `PATH`. Every consumer takes the resolved scope as a parameter
    /// now — no ambient read remains to disagree — and must observe `public`.
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
            "jobContainer": { "type": 2, "map": [
                { "key": { "type": 0, "lit": "image" }, "value": { "type": 0, "lit": "ubuntu:24.04" } },
                { "key": { "type": 0, "lit": "options" }, "value": { "type": 0, "lit": "--privileged" } }
            ] },
            "jobServiceContainers": { "type": 2, "map": [
                { "key": { "type": 0, "lit": "redis" }, "value": { "type": 2, "map": [
                    { "key": { "type": 0, "lit": "image" }, "value": { "type": 0, "lit": "redis:7" } },
                    { "key": { "type": 0, "lit": "ports" }, "value": { "type": 1, "seq": [{ "type": 0, "lit": "6379:6379" }] } },
                    { "key": { "type": 0, "lit": "options" }, "value": { "type": 0, "lit": "--privileged" } }
                ] } }
            ] },
            "variables": {
                "github.workflow": { "value": "CI", "isSecret": false },
                "github.repository": { "value": "ChainArgos/java-monorepo", "isSecret": false },
                "github.server_url": { "value": "https://github.com", "isSecret": false },
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
                slot_store_key: None,
            },
            "ubuntu:24.04",
            "",
            "daemon".into(),
            resolved.as_str(),
        )
        .unwrap();

        assert_eq!(spec.repository.as_deref(), Some("ChainArgos/java-monorepo"));

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
        // The spec carries the one spelling: the admitted scope itself, not a
        // collapsed class. A custom pool scope must survive into every mount,
        // or the mounts disagree with the storage leases (which use the raw
        // admitted scope) and live stores become GC-reclaimable mid-job.
        assert_eq!(spec.store_trust_scope, "public");

        // Every trust-scoped store path, including the five that used to read
        // the environment variable behind the gate's back. All of them carry
        // the filesystem key for `public` now — the compiler stores used to
        // collapse to `untrusted` here while the leases pinned `public`. None
        // may ever carry the key for `trusted`.
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let scoped_stores = [
            crate::container::cargo_executable_store_host(temp, resolved.as_str(), &repository_key)
                .unwrap(),
            crate::container::mise_executable_store_host(temp, resolved.as_str(), &repository_key)
                .unwrap(),
            crate::container::mise_binary_store_host(temp, resolved.as_str(), &repository_key)
                .unwrap(),
            crate::container::playwright_browser_store_host(
                temp,
                resolved.as_str(),
                &repository_key,
            )
            .unwrap(),
            crate::storage::cache_class_path(resolved.as_str(), "caches").unwrap(),
            spec.mbx_store_host.clone().expect("mbx store"),
        ];

        let has_component = |store: &std::path::Path, wanted: &str| {
            store
                .components()
                .any(|component| component.as_os_str() == wanted)
        };
        let public_key = crate::trust_scope::filesystem_key("public");
        let trusted_key = crate::trust_scope::filesystem_key("trusted");
        for store in &scoped_stores {
            assert!(
                !has_component(store, &trusted_key),
                "store path leaked the ambient VELNOR_TRUST_SCOPE value: {}",
                store.display()
            );
            assert!(
                has_component(store, &public_key),
                "store path is not scoped to the resolved trust scope: {}",
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
        // The single predicate behind every capability gate (F-V5): the exact
        // value `trusted`, case-insensitively, after trimming. Near-misses
        // stay refused.
        for scope in ["trusted", " Trusted ", "TRUSTED", "Trusted"] {
            assert!(
                github_trust_scope_allows_host_docker(scope),
                "scope {scope:?} unlocks host capability"
            );
        }
        for scope in [
            "",
            "   ",
            "unknown",
            "untrusted",
            "release",
            "trustedx",
            "xtrusted",
            "trust ed",
        ] {
            assert!(
                !github_trust_scope_allows_host_docker(scope),
                "scope {scope:?} refuses host capability"
            );
        }
    }

    #[test]
    fn container_spec_preserves_the_admitted_scope_verbatim() {
        // The spec is the mount side of the one-spelling contract: whatever
        // admission derived — a known scope, a custom pool scope, or a case
        // variant — must reach the mounts unchanged (trimmed, empty failing
        // closed), or the mounts disagree with the storage leases.
        let job = microvm_job();
        let spec_for = |scope: &str| {
            github_job_container_spec(
                &job,
                GitHubJobContainerPaths {
                    workspace_host: "/tmp/workspace".into(),
                    temp_host: "/tmp/temp".into(),
                    home_host: "/tmp/home".into(),
                    actions_host: "/tmp/actions".into(),
                    tools_host: "/tmp/tools".into(),
                    docker_host_work_dir: None,
                    execution_backend: velnor_model::ExecutionBackendKind::Docker,
                    slot_store_key: None,
                },
                "ubuntu:24.04",
                "",
                "daemon".into(),
                scope,
            )
            .unwrap()
        };
        assert_eq!(spec_for("trusted").store_trust_scope, "trusted");
        assert_eq!(spec_for(" release ").store_trust_scope, "release");
        assert_eq!(spec_for("public-forks").store_trust_scope, "public-forks");
        assert_eq!(spec_for(" Trusted ").store_trust_scope, "Trusted");
        assert_eq!(
            spec_for("").store_trust_scope,
            crate::trust_scope::FAIL_CLOSED
        );
        assert_eq!(
            spec_for("   ").store_trust_scope,
            crate::trust_scope::FAIL_CLOSED
        );
    }

    /// Leases pin the raw admitted scope while the mounts used to collapse it
    /// to a fixed class: on a custom pool the legacy executable stores were
    /// leased at `bin/<custom>/<repo>` and mounted at `bin/untrusted/<repo>`,
    /// so the live stores were GC-reclaimable mid-job — and in both layouts a
    /// trusted-class job shared mutable untrusted stores with fork jobs. Every
    /// mount-side path below must equal its lease-side twin, in both layouts.
    #[test]
    fn custom_pool_leases_and_mounts_share_one_scope_spelling() {
        use crate::trust_class::TrustClass;

        let root =
            std::env::temp_dir().join(format!("velnor-custom-pool-scope-{}", uuid::Uuid::new_v4()));
        let work = root.join("work");
        let temp = work.join("slot-1").join("job-1").join("temp");
        std::fs::create_dir_all(&temp).unwrap();

        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Custom pool scope test",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "octo/base" },
                "github.server_url": { "value": "https://github.com" },
                "github.repository_id": { "value": "42" }
            }
        }))
        .unwrap();

        for pool in ["public-forks", "Trusted"] {
            // A trusted-class job keeps the pool value byte-identically; the
            // runner normalizes it once and threads it to leases and mounts.
            let admitted =
                crate::trust_scope::normalize_scope(TrustClass::Trusted.admitted_scope(pool));
            let spec = github_job_container_spec(
                &job,
                GitHubJobContainerPaths {
                    workspace_host: work.join("slot-1/job-1/workspace"),
                    temp_host: temp.clone(),
                    home_host: work.join("slot-1/job-1/home"),
                    actions_host: work.join("slot-1/job-1/actions"),
                    tools_host: work.join("slot-1/job-1/tools"),
                    docker_host_work_dir: None,
                    execution_backend: velnor_model::ExecutionBackendKind::Docker,
                    slot_store_key: None,
                },
                "ubuntu:24.04",
                "",
                "daemon".into(),
                admitted,
            )
            .unwrap();
            assert_eq!(spec.store_trust_scope, admitted, "pool={pool}");

            // The daemon roots on both sides resolve identically: the lease
            // side from the work dir, the mount side from the job temp dir.
            let lease_root = crate::container::daemon_shared_root(work.clone());
            let mount_root = crate::container::daemon_store_root(&temp);
            assert_eq!(lease_root, mount_root, "pool={pool}");

            let repository_key =
                crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
            let trust_key = crate::trust_scope::filesystem_key(admitted);
            let untrusted_key = crate::trust_scope::filesystem_key(crate::trust_scope::FAIL_CLOSED);
            // Lease side, exactly as `runner.rs` composes it; mount side from
            // the spec's scope, exactly as `container.rs` composes it.
            for (lease, mount) in [
                (
                    crate::container::cargo_store_host(&lease_root, admitted).unwrap(),
                    crate::container::cargo_store_host(
                        &mount_root,
                        spec.store_trust_scope.as_str(),
                    )
                    .unwrap(),
                ),
                (
                    crate::container::mise_store_host(&lease_root, admitted).unwrap(),
                    crate::container::mise_store_host(&mount_root, spec.store_trust_scope.as_str())
                        .unwrap(),
                ),
            ] {
                // Trust is encoded in the selected class root; the one-spelling
                // proof here is that lease and mount resolve the identical root.
                assert_eq!(lease, mount, "pool={pool}");
            }
            for (lease, mount) in [
                (
                    crate::container::cargo_executable_store_host(
                        &lease_root,
                        admitted,
                        &repository_key,
                    )
                    .unwrap(),
                    crate::container::cargo_executable_store_host(
                        &mount_root,
                        spec.store_trust_scope.as_str(),
                        &repository_key,
                    )
                    .unwrap(),
                ),
                (
                    crate::container::mise_executable_store_host(
                        &lease_root,
                        admitted,
                        &repository_key,
                    )
                    .unwrap(),
                    crate::container::mise_executable_store_host(
                        &mount_root,
                        spec.store_trust_scope.as_str(),
                        &repository_key,
                    )
                    .unwrap(),
                ),
                (
                    crate::container::mise_binary_store_host(
                        &lease_root,
                        admitted,
                        &repository_key,
                    )
                    .unwrap(),
                    crate::container::mise_binary_store_host(
                        &mount_root,
                        spec.store_trust_scope.as_str(),
                        &repository_key,
                    )
                    .unwrap(),
                ),
                (
                    github_mbx_store_host(&job, &lease_root, admitted).unwrap(),
                    spec.mbx_store_host.clone().expect("mbx store"),
                ),
            ] {
                assert_eq!(lease, mount, "pool={pool}");
                assert!(
                    lease
                        .components()
                        .any(|component| component.as_os_str() == std::ffi::OsStr::new(&trust_key)),
                    "store path is not namespaced by the admitted scope {admitted}: {}",
                    lease.display()
                );
                assert!(
                    !lease.components().any(|component| {
                        component.as_os_str() == std::ffi::OsStr::new(&untrusted_key)
                    }),
                    "trusted-class store path collapsed to the untrusted floor: {}",
                    lease.display()
                );
            }

            // Canonical layout: the same scope string selects the same
            // namespace on both sides.
            let layout = crate::storage::StorageLayout::from_prefix(&root);
            let lease_catalog =
                crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work, &layout);
            for (lease, mount) in [
                (
                    crate::storage::cache_class_path_with_layout(admitted, "cargo", Some(&layout))
                        .unwrap(),
                    crate::storage::cache_class_path_with_layout(
                        spec.store_trust_scope.as_str(),
                        "cargo",
                        Some(&layout),
                    )
                    .unwrap(),
                ),
                (
                    lease_catalog.mbx(admitted),
                    lease_catalog.mbx(spec.store_trust_scope.as_str()),
                ),
                (
                    lease_catalog.sccache(admitted),
                    lease_catalog.sccache(spec.store_trust_scope.as_str()),
                ),
            ] {
                assert_eq!(lease, mount, "pool={pool}");
                assert!(
                    lease
                        .components()
                        .any(|component| component.as_os_str() == std::ffi::OsStr::new(&trust_key)),
                    "canonical store path is not namespaced by the admitted scope {admitted}: {}",
                    lease.display()
                );
            }
        }

        std::fs::remove_dir_all(&root).ok();
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
                    "github.server_url": { "value": "https://github.com" },
                    "github.repository_id": { "value": repository_id.to_string() }
                }
            }))
            .unwrap()
        };
        let temp = std::path::Path::new("/var/lib/velnor/work/slot-1/job/temp");
        let layout = crate::storage::StorageLayout::from_prefix(std::path::Path::new("/storage"));

        let repo_41 =
            crate::store_catalog::repository_store_key("https://github.com", "41").unwrap();
        let repo_42 =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        assert_eq!(
            github_rust_store_host_with_layout(&job(41), temp, "trusted", "mbx", Some(&layout))
                .unwrap(),
            layout.cache_class("trusted", "compiler/mbx").join(&repo_41)
        );
        assert_eq!(
            github_rust_store_host_with_layout(
                &job(42),
                temp,
                "trusted",
                "sccache",
                Some(&layout),
            )
            .unwrap(),
            layout
                .cache_class("trusted", "compiler/sccache")
                .join(&repo_42)
        );
        assert_ne!(
            github_rust_store_host_with_layout(&job(41), temp, "trusted", "mbx", Some(&layout))
                .unwrap(),
            github_rust_store_host_with_layout(
                &job(42),
                temp,
                "trusted",
                "sccache",
                Some(&layout),
            )
            .unwrap()
        );
    }

    #[test]
    fn rust_stores_are_ephemeral_without_valid_repository_id() {
        let job = microvm_job();
        let temp = std::path::Path::new("/velnor/work/job/temp");

        assert_eq!(
            github_mbx_store_host(&job, temp, "trusted").unwrap(),
            temp.join("_velnor/ephemeral/mbx/job")
        );
        assert_eq!(
            crate::sccache_compat::store_host(&job, temp, "trusted").unwrap(),
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
                slot_store_key: None,
            },
            "ubuntu:24.04",
            "",
            "daemon".into(),
            "public-forks",
        )
        .unwrap();

        assert!(!spec.mount_docker_socket);
        assert_eq!(spec.store_trust_scope, "public-forks");
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
                slot_store_key: None,
            },
            "ubuntu:24.04",
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
                slot_store_key: None,
            },
            "ubuntu:24.04",
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not support explicit"));
    }

    #[test]
    fn docker_swaps_mbx_for_sccache_only_on_explicit_request() {
        let paths = || GitHubJobContainerPaths {
            workspace_host: "/tmp/workspace".into(),
            temp_host: "/tmp/temp".into(),
            home_host: "/tmp/home".into(),
            actions_host: "/tmp/actions".into(),
            tools_host: "/tmp/tools".into(),
            docker_host_work_dir: None,
            execution_backend: velnor_model::ExecutionBackendKind::Docker,
            slot_store_key: None,
        };
        // Default: mbx store, no sccache presence anywhere in the spec.
        let default = github_job_container_spec(
            &microvm_job(),
            paths(),
            "ubuntu:24.04",
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap();
        assert!(default.mbx_store_host.is_some());
        assert!(default.sccache_store_host.is_none());

        // Explicit request: sccache store instead of mbx, never both.
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
        let explicit = github_job_container_spec(
            &job,
            paths(),
            "ubuntu:24.04",
            "",
            "daemon".into(),
            "trusted",
        )
        .unwrap();
        assert!(explicit.mbx_store_host.is_none());
        assert!(explicit.sccache_store_host.is_some());
    }

    #[test]
    fn docker_preserves_compiler_cache_environment() {
        let mut job = microvm_job();
        job.job_container = Some(template_map_token(vec![
            (
                template_string_token("image"),
                template_string_token("ubuntu:24.04"),
            ),
            (
                template_string_token("env"),
                template_map_token(vec![(
                    template_string_token("RUSTC_WRAPPER"),
                    template_string_token("sccache"),
                )]),
            ),
        ]));

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
                slot_store_key: None,
            },
            "ubuntu:24.04",
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
        let job: AgentJobRequestMessage = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": { "type": 2, "map": [
                { "key": { "type": 0, "lit": "image" }, "value": { "type": 0, "lit": "ghcr.io/acme/job:latest" } }
            ] },
            "resources": {
                "containers": [{
                    "alias": "__job",
                    "properties": { "image": "ubuntu:24.04" }
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_image(&job).unwrap().as_deref(),
            Some("ghcr.io/acme/job:latest")
        );
    }

    #[test]
    fn job_container_image_does_not_enumerate_resources_without_a_string_token() {
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "resources": {
                "containers": [null, {
                    "alias": "__job",
                    "properties": { "image": "ghcr.io/acme/resource:latest" }
                }, {
                    "alias": "job",
                    "properties": { "image": "ghcr.io/acme/duplicate:latest" }
                }]
            }
        }))
        .unwrap();

        assert_eq!(job_container_image(&job).unwrap(), None);

        for job_container in [
            serde_json::Value::Null,
            serde_json::json!({"options": "--init"}),
        ] {
            let job = AgentJobRequestMessage::from_value(serde_json::json!({
                "messageType": "PipelineAgentJobRequest",
                "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
                "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
                "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "jobDisplayName": "Container",
                "requestId": 1,
                "jobContainer": job_container,
                "resources": {
                    "containers": [null, {
                        "alias": "__job",
                        "properties": { "image": "ghcr.io/acme/resource:latest" }
                    }, {
                        "alias": "job",
                        "properties": { "image": "ghcr.io/acme/duplicate:latest" }
                    }]
                }
            }))
            .unwrap();

            assert_eq!(job_container_image(&job).unwrap(), None);
        }
    }

    #[test]
    fn direct_typed_job_container_strips_docker_uri_prefix() {
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": {
                "type": 2,
                "map": [{
                    "key": { "type": 0, "lit": "image" },
                    "value": { "type": 0, "lit": "docker://node:22" }
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_image(&job).unwrap().as_deref(),
            Some("node:22")
        );
    }

    #[test]
    fn scalar_job_and_service_container_values_stringify_and_null_services_skip() {
        let job_for = |container| AgentJobRequestMessage {
            job_container: Some(container),
            ..AgentJobRequestMessage::default()
        };
        assert_eq!(
            job_container_image(&job_for(serde_json::json!({ "type": 5, "bool": true })))
                .unwrap()
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            job_container_image(&job_for(Value::Number(
                serde_json::Number::from_f64(42.0).unwrap(),
            )))
            .unwrap()
            .as_deref(),
            Some("42")
        );
        assert_eq!(
            job_container_image(&job_for(serde_json::json!({ "type": 7 }))).unwrap(),
            None
        );

        let job = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![
                (
                    serde_json::json!({ "type": 5, "bool": true }),
                    serde_json::json!({ "type": 5, "bool": true }),
                ),
                (
                    template_string_token("numeric"),
                    Value::Number(serde_json::Number::from_f64(42.0).unwrap()),
                ),
                (
                    template_string_token("null"),
                    serde_json::json!({ "type": 7 }),
                ),
                (
                    template_string_token("null-literal"),
                    serde_json::json!({ "type": 0, "lit": null }),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };
        let services = service_containers(&job, "trusted").unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].network_alias, "true");
        assert_eq!(services[0].image, "true");
        assert_eq!(services[1].network_alias, "numeric");
        assert_eq!(services[1].image, "42");
    }

    #[test]
    fn resolved_job_container_uses_resource_properties_and_is_not_a_service() {
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": "BUILD",
            "resources": {
                "containers": [{
                    "alias": "build",
                    "properties": {
                        "image": "docker://node:22",
                        "env": { "NODE_ENV": "test" },
                        "options": "--init"
                    }
                }, {
                    "alias": "db",
                    "properties": { "image": "docker://postgres:16" }
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            job_container_image(&job).unwrap().as_deref(),
            Some("node:22")
        );
        assert_eq!(
            job_container_env(&job).unwrap(),
            vec![("NODE_ENV".to_string(), "test".to_string())]
        );
        assert_eq!(
            job_container_options(&job, "trusted").unwrap(),
            vec!["--init"]
        );

        let services = service_containers(&job, "trusted").unwrap();
        assert!(
            services.is_empty(),
            "an unrelated container resource is not a service without JobServiceContainers or JobSidecarContainers"
        );
    }

    #[test]
    fn service_images_require_a_field_but_skip_empty_values_after_prefix_removal() {
        let string_token = |value: &str| serde_json::json!({ "type": 0, "lit": value });
        let mapping_token = |entries: Vec<(&str, Value)>| {
            serde_json::json!({
                "type": 2,
                "map": entries
                    .into_iter()
                    .map(|(name, value)| serde_json::json!({
                        "key": string_token(name),
                        "value": value
                    }))
                    .collect::<Vec<_>>()
            })
        };
        let missing_image = AgentJobRequestMessage {
            job_service_containers: Some(mapping_token(vec![(
                "missing-image",
                mapping_token(Vec::new()),
            )])),
            ..AgentJobRequestMessage::default()
        };
        assert!(service_containers(&missing_image, "trusted").is_err());

        let empty_images = AgentJobRequestMessage {
            job_service_containers: Some(mapping_token(vec![
                (
                    "empty-image",
                    mapping_token(vec![("image", string_token(""))]),
                ),
                (
                    "empty-docker-image",
                    mapping_token(vec![("image", string_token("docker://"))]),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };
        assert!(service_containers(&empty_images, "trusted")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn malformed_container_resource_properties_fail_reads() {
        let value = serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobSidecarContainers": { "postgres": "postgres" },
            "resources": {
                "containers": [{
                    "alias": "postgres",
                    "properties": {
                        "image": "postgres:16",
                        "env": { "POSTGRES_PASSWORD": { "nested": true } }
                    }
                }]
            }
        });

        assert!(
            WireAgentJobRequestMessage::validate_deserialization_callback_from_value(&value)
                .is_err(),
            "the acquisition callback must reject malformed properties on a selected sidecar"
        );
        assert!(
            AgentJobRequestMessage::from_value(value).is_err(),
            "full message deserialization must reject malformed selected-sidecar properties"
        );
    }

    #[test]
    fn malformed_unselected_container_resource_properties_are_ignored() {
        let value = serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobContainer": "build",
            "resources": {
                "containers": [{
                "alias": "build",
                "properties": {
                    "image": "docker://node:22"
                }
                }, {
                    "alias": "postgres",
                    "properties": {
                        "image": "postgres:16",
                        "env": { "POSTGRES_PASSWORD": { "nested": true } }
                    }
                }]
            }
        });

        assert!(
            WireAgentJobRequestMessage::validate_deserialization_callback_from_value(&value)
                .is_ok(),
            "the acquisition callback must ignore malformed properties on an unselected resource"
        );
        let job = AgentJobRequestMessage::from_value(value).unwrap();
        assert_eq!(
            job_container_image(&job).unwrap().as_deref(),
            Some("node:22")
        );
        assert!(service_containers(&job, "trusted").unwrap().is_empty());
    }

    #[test]
    fn service_schema_string_fields_stringify_scalar_tokens() {
        let string_token = |value: &str| serde_json::json!({ "type": 0, "lit": value });
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobServiceContainers": {
                "type": 2,
                "map": [
                    {
                        "key": string_token("boolean-fields"),
                        "value": {
                            "type": 2,
                            "map": [
                                { "key": string_token("image"), "value": { "type": 5, "bool": true } },
                                { "key": string_token("options"), "value": { "type": 6, "num": 23 } },
                                { "key": string_token("env"), "value": {
                                    "type": 2,
                                    "map": [{ "key": string_token("EMPTY_ENV"), "value": { "type": 7 } }]
                                } }
                            ]
                        }
                    },
                    {
                        "key": string_token("numeric-fields"),
                        "value": {
                            "type": 2,
                            "map": [
                                { "key": string_token("image"), "value": { "type": 6, "num": 24 } },
                                { "key": string_token("options"), "value": { "type": 5, "bool": false } }
                            ]
                        }
                    }
                ]
            }
        }))
        .unwrap();
        let services = service_containers(&job, "trusted").unwrap();
        let boolean_fields = services
            .iter()
            .find(|service| service.network_alias == "boolean-fields")
            .unwrap();
        assert_eq!(boolean_fields.image, "true");
        assert_eq!(boolean_fields.options, vec!["23"]);
        assert_eq!(
            boolean_fields.env,
            vec![("EMPTY_ENV".to_owned(), String::new())]
        );
        let numeric_fields = services
            .iter()
            .find(|service| service.network_alias == "numeric-fields")
            .unwrap();
        assert_eq!(numeric_fields.image, "24");
        assert_eq!(numeric_fields.options, vec!["false"]);

        for invalid_value in [
            serde_json::json!({ "type": 1, "seq": [] }),
            template_map_token(Vec::new()),
        ] {
            let job = AgentJobRequestMessage {
                job_service_containers: Some(template_map_token(vec![(
                    template_string_token("postgres"),
                    template_map_token(vec![
                        (
                            template_string_token("image"),
                            template_string_token("postgres:16"),
                        ),
                        (template_string_token("options"), invalid_value),
                    ]),
                )])),
                ..AgentJobRequestMessage::default()
            };
            assert!(service_containers(&job, "trusted").is_err());
        }
    }

    #[test]
    fn selected_legacy_service_with_null_environment_value_uses_empty_string() {
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Container",
            "requestId": 1,
            "jobSidecarContainers": { "postgres": "postgres" },
            "resources": {
                "containers": [{
                    "alias": "postgres",
                    "properties": {
                        "image": "postgres:16",
                        "env": { "POSTGRES_PASSWORD": null }
                    }
                }]
            }
        }))
        .unwrap();
        let services = service_containers(&job, "trusted").unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(
            services[0].env,
            vec![("POSTGRES_PASSWORD".to_string(), String::new())]
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
                ("VELNOR_SOURCE_SHA".to_string(), "spoofed".to_string()),
                ("VELNOR_MANIFEST_VERSION".to_string(), "spoofed".to_string()),
                ("VELNOR_HOST".to_string(), "spoofed-host".to_string()),
                (
                    "VELNOR_INSTANCE".to_string(),
                    "spoofed-instance".to_string(),
                ),
                ("VELNOR_SLOT".to_string(), "99".to_string()),
            ],
            velnor_model::ExecutionBackendKind::Docker,
        );
        assert_eq!(
            env,
            vec![
                ("NODE_OPTIONS".to_string(), "x".to_string()),
                ("VELNOR_EXECUTION_BACKEND".to_string(), "docker".to_string()),
                (
                    "VELNOR_SOURCE_SHA".to_string(),
                    env!("VELNOR_SOURCE_SHA").to_string(),
                ),
                (
                    "VELNOR_MANIFEST_VERSION".to_string(),
                    crate::manifest::MANIFEST_VERSION.to_string(),
                ),
            ]
        );
    }

    #[test]
    fn container_env_strips_runner_owned_github_names() {
        // Repository-controlled container env must not set GITHUB_*: the four
        // tier signals feed the immutable job environment, and a broker-omitted
        // signal would otherwise let a workflow spoof the BuildKit trust tier.
        let object = serde_json::json!({
            "NODE_OPTIONS": "--max-old-space-size=4096",
            "GITHUB_REF": "refs/tags/v9.9.9",
            "GITHUB_REF_TYPE": "tag",
            "GITHUB_EVENT_NAME": "release",
            "GITHUB_REF_PROTECTED": "true",
            "GITHUB_SHA": "evil",
        });
        assert_eq!(
            container_env_value(&context_value(object)).unwrap(),
            vec![(
                "NODE_OPTIONS".to_string(),
                "--max-old-space-size=4096".to_string()
            )]
        );
        let map = serde_json::json!({
            "RUST_LOG": "debug",
            "GITHUB_REF": "refs/tags/v9.9.9"
        });
        assert_eq!(
            container_env_value(&context_value(map)).unwrap(),
            vec![("RUST_LOG".to_string(), "debug".to_string())]
        );
        // Belt and suspenders at the advertising layer: nothing GITHUB_*-
        // shaped survives into the job container spec env.
        let env = backend_advertising_env(
            vec![
                ("NODE_OPTIONS".to_string(), "x".to_string()),
                ("GITHUB_REF_TYPE".to_string(), "tag".to_string()),
            ],
            velnor_model::ExecutionBackendKind::Docker,
        );
        assert_eq!(
            env,
            vec![
                ("NODE_OPTIONS".to_string(), "x".to_string()),
                ("VELNOR_EXECUTION_BACKEND".to_string(), "docker".to_string()),
                (
                    "VELNOR_SOURCE_SHA".to_string(),
                    env!("VELNOR_SOURCE_SHA").to_string(),
                ),
                (
                    "VELNOR_MANIFEST_VERSION".to_string(),
                    crate::manifest::MANIFEST_VERSION.to_string(),
                ),
            ]
        );
    }

    #[test]
    fn job_container_env_reads_mappings_and_rejects_sequences() {
        let object_job = AgentJobRequestMessage {
            job_container: Some(template_map_token(vec![
                (
                    template_string_token("image"),
                    template_string_token("ubuntu:24.04"),
                ),
                (
                    template_string_token("env"),
                    template_map_token(vec![
                        (
                            template_string_token("NODE_OPTIONS"),
                            template_string_token("--max-old-space-size=4096"),
                        ),
                        (
                            template_string_token("CACHE_ENABLED"),
                            serde_json::json!(true),
                        ),
                        (template_string_token("FETCH_DEPTH"), serde_json::json!(0)),
                        (template_string_token("EMPTY_VALUE"), Value::Null),
                        (
                            template_string_token("DOCKER_HOST"),
                            template_string_token("tcp://attacker.example:2376"),
                        ),
                    ]),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };
        let array_job = AgentJobRequestMessage {
            job_container: Some(template_map_token(vec![
                (
                    template_string_token("image"),
                    template_string_token("ubuntu:24.04"),
                ),
                (
                    template_string_token("env"),
                    template_sequence_token(vec![template_string_token("invalid")]),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };

        assert_eq!(
            job_container_env(&object_job).unwrap(),
            vec![
                ("CACHE_ENABLED".into(), "true".into()),
                ("EMPTY_VALUE".into(), "".into()),
                ("FETCH_DEPTH".into(), "0".into()),
                ("NODE_OPTIONS".into(), "--max-old-space-size=4096".into()),
            ]
        );
        assert!(job_container_env(&array_job).is_err());

        let alias_job = AgentJobRequestMessage {
            job_container: Some(template_map_token(vec![
                (
                    template_string_token("image"),
                    template_string_token("ubuntu:24.04"),
                ),
                (
                    template_string_token("environmentVariables"),
                    template_map_token(vec![(
                        template_string_token("NODE_OPTIONS"),
                        template_string_token("--max-old-space-size=4096"),
                    )]),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };
        assert!(job_container_env(&alias_job).is_err());
    }

    #[test]
    fn container_env_stringifies_scalars_and_rejects_complex_values() {
        assert_eq!(
            container_env_value(&context_value(serde_json::json!({
                "STRING": "value",
                "BOOLEAN": false,
                "NUMBER": 0,
                "NULL": null
            })))
            .unwrap(),
            vec![
                ("BOOLEAN".to_string(), "false".to_string()),
                ("NULL".to_string(), String::new()),
                ("NUMBER".to_string(), "0".to_string()),
                ("STRING".to_string(), "value".to_string())
            ]
        );
        assert!(container_env_value(&context_value(serde_json::json!("scalar"))).is_err());
        assert!(container_env_value(&context_value(serde_json::json!(["sequence"]))).is_err());
        assert!(container_env_value(&context_value(serde_json::json!({
            "NESTED": { "value": true }
        })))
        .is_err());
        assert!(container_env(&context_value(serde_json::json!({ "env": "scalar" }))).is_err());
        assert!(container_env(&context_value(serde_json::json!({
            "env": { "NESTED": { "value": true } }
        })))
        .is_err());

        let big_integer = ContextValue::object(vec![(
            "BIG_INTEGER".to_owned(),
            ContextValue::big_integer("123456789012345678901234567890").unwrap(),
        )])
        .unwrap();
        assert_eq!(
            container_env_value(&big_integer).unwrap(),
            vec![(
                "BIG_INTEGER".to_owned(),
                "123456789012345678901234567890".to_owned()
            )]
        );

        let raw_integer = ContextValue::object(vec![(
            "RAW_INTEGER".to_owned(),
            ContextValue::Number(serde_json::Number::from(9_007_199_254_740_993_u64)),
        )])
        .unwrap();
        assert_eq!(
            container_env_value(&raw_integer).unwrap(),
            vec![("RAW_INTEGER".to_owned(), "9007199254740993".to_owned())]
        );
    }

    #[test]
    fn container_scalar_projections_reject_undefined_and_constructor_tokens() {
        for value in [
            ContextValue::Undefined,
            ContextValue::Constructor {
                name: "Date".to_owned(),
                arguments: vec![ContextValue::String("2024-01-01".to_owned())],
            },
        ] {
            assert!(context_scalar_string(&value).is_none());
            assert!(container_image(&value).is_err());

            let options =
                ContextValue::object(vec![("options".to_owned(), value.clone())]).unwrap();
            assert!(container_options(&options).is_err());

            let env = ContextValue::object(vec![("NAME".to_owned(), value.clone())]).unwrap();
            assert!(container_env_value(&env).is_err());

            let ports =
                ContextValue::object(vec![("ports".to_owned(), ContextValue::Array(vec![value]))])
                    .unwrap();
            assert!(container_ports(&ports).is_err());
        }
    }

    #[test]
    fn container_env_and_ports_stringify_scalars_in_source_order() {
        let string_token = |value: &str| serde_json::json!({ "type": 0, "lit": value });
        let valid_container = serde_json::json!({
            "type": 2,
            "map": [
                {
                    "key": string_token("env"),
                    "value": {
                        "type": 2,
                        "map": [
                            { "key": string_token("z-first"), "value": string_token("one") },
                            { "key": string_token("a-second"), "value": string_token("two") }
                        ]
                    }
                },
                {
                    "key": string_token("ports"),
                    "value": {
                        "type": 1,
                        "seq": [
                            string_token("8081:8081"),
                            string_token("8080:8080")
                        ]
                    }
                }
            ]
        });
        let expanded = expand_template_token(&valid_container).unwrap();

        assert_eq!(
            container_env(&expanded).unwrap(),
            vec![
                ("z-first".to_string(), "one".to_string()),
                ("a-second".to_string(), "two".to_string()),
            ]
        );
        assert_eq!(
            container_ports(&expanded).unwrap(),
            vec!["8081:8081".to_string(), "8080:8080".to_string()]
        );

        for invalid in [
            (serde_json::json!(false), Some("false"), Some("false")),
            (serde_json::json!(8080), Some("8080"), Some("8080")),
            (string_token(""), Some(""), None),
            (serde_json::json!(null), Some(""), None),
            (serde_json::json!({ "type": 2, "map": [] }), None, None),
            (
                serde_json::json!({ "type": 5, "bool": true }),
                Some("true"),
                Some("true"),
            ),
            (
                serde_json::json!({ "type": 6, "num": 8081 }),
                Some("8081"),
                Some("8081"),
            ),
            (serde_json::json!({ "type": 7 }), Some(""), None),
        ] {
            let (invalid, env_value, port_value) = invalid;
            let ports = expand_template_token(&serde_json::json!({
                "type": 2,
                "map": [{
                    "key": string_token("ports"),
                    "value": { "type": 1, "seq": [invalid.clone()] }
                }]
            }))
            .unwrap();
            match port_value {
                Some(expected) => {
                    assert_eq!(container_ports(&ports).unwrap(), vec![expected.to_owned()])
                }
                None => assert!(container_ports(&ports).is_err()),
            }

            let env = expand_template_token(&serde_json::json!({
                "type": 2,
                "map": [{
                    "key": string_token("env"),
                    "value": {
                        "type": 2,
                        "map": [{ "key": string_token("VALUE"), "value": invalid }]
                    }
                }]
            }))
            .unwrap();
            match env_value {
                Some(expected) => assert_eq!(
                    container_env(&env).unwrap(),
                    vec![("VALUE".to_owned(), expected.to_owned())]
                ),
                None => assert!(container_env(&env).is_err()),
            }
        }
    }

    #[test]
    fn job_service_container_mapping_shapes_match_runner_expectations() {
        assert!(
            service_containers(&AgentJobRequestMessage::default(), "trusted")
                .unwrap()
                .is_empty()
        );

        for null_token in [
            Value::Null,
            serde_json::json!({ "type": 7 }),
            serde_json::json!({ "type": 2, "map": null }),
        ] {
            let job = AgentJobRequestMessage {
                job_service_containers: Some(null_token),
                ..AgentJobRequestMessage::default()
            };
            assert!(service_containers(&job, "trusted").unwrap().is_empty());
        }

        for invalid_shape in [
            template_string_token("postgres:16"),
            template_sequence_token(Vec::new()),
        ] {
            let job = AgentJobRequestMessage {
                job_service_containers: Some(invalid_shape),
                ..AgentJobRequestMessage::default()
            };
            assert!(service_containers(&job, "trusted").is_err());
        }
    }

    #[test]
    fn container_schema_keys_are_exact_and_unknown_keys_fail() {
        for unsupported_key in [
            "Ports",
            "Image",
            "containerImage",
            "ContainerImage",
            "createOptions",
            "CreateOptions",
            "Options",
            "Env",
            "unknown",
        ] {
            let mut entries = vec![(
                template_string_token(unsupported_key),
                template_string_token("ignored"),
            )];
            if unsupported_key != "Image" {
                entries.push((
                    template_string_token("image"),
                    template_string_token("postgres:16"),
                ));
            }
            let container = template_map_token(entries);
            let job = AgentJobRequestMessage {
                job_service_containers: Some(template_map_token(vec![(
                    template_string_token("postgres"),
                    container,
                )])),
                ..AgentJobRequestMessage::default()
            };
            let error = service_containers(&job, "trusted").unwrap_err();
            assert!(
                error.to_string().contains("unexpected container key"),
                "{unsupported_key}: {error:#}"
            );
        }

        let uppercase_job_image = AgentJobRequestMessage {
            job_container: Some(template_map_token(vec![(
                template_string_token("Image"),
                template_string_token("ubuntu:24.04"),
            )])),
            ..AgentJobRequestMessage::default()
        };
        assert!(expanded_job_container(&uppercase_job_image).is_err());
    }

    #[test]
    fn template_map_conversion_rejects_malformed_pairs_and_discriminators() {
        for malformed in [
            serde_json::json!({ "type": 2, "map": "not-a-sequence" }),
            serde_json::json!({ "type": 2, "map": [null] }),
            serde_json::json!({ "type": 2, "map": [{ "value": "missing key" }] }),
            serde_json::json!({ "type": 2, "map": [{ "key": "missing value" }] }),
            serde_json::json!({ "type": 2.5, "map": [] }),
            serde_json::json!({ "type": "2", "map": [] }),
            serde_json::json!({ "type": 99, "map": [] }),
        ] {
            assert!(expand_template_token(&malformed).is_err(), "{malformed}");
        }
    }

    #[test]
    fn literal_tokens_stringify_boolean_null_and_integer_without_rounding() {
        for (literal, expected) in [
            (serde_json::json!(false), "false"),
            (serde_json::Value::Null, ""),
            (
                serde_json::json!(9_007_199_254_740_993_u64),
                "9007199254740993",
            ),
        ] {
            let token = serde_json::json!({ "type": 0, "lit": literal });
            assert_eq!(
                expand_template_token(&token).unwrap(),
                ContextValue::String(expected.to_owned())
            );
        }

        let typed_number = expand_template_token(&template_number_token(serde_json::json!(
            1.234_567_890_123_456_7_f64
        )))
        .unwrap();
        assert_eq!(
            context_scalar_string(&typed_number).as_deref(),
            Some("1.23456789012346")
        );
    }

    #[test]
    fn template_mapping_keys_are_stringified_and_validated_before_collapse() {
        let numeric_alias = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![(
                template_number_token(serde_json::json!(1)),
                valid_service_container_token(),
            )])),
            ..AgentJobRequestMessage::default()
        };
        let services = service_containers(&numeric_alias, "trusted").unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].network_alias, "1");

        let boolean_alias = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![(
                serde_json::json!({ "type": 5, "bool": true }),
                valid_service_container_token(),
            )])),
            ..AgentJobRequestMessage::default()
        };
        let services = service_containers(&boolean_alias, "trusted").unwrap();
        assert_eq!(services[0].network_alias, "true");

        let duplicate_aliases = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![
                (template_string_token("db"), valid_service_container_token()),
                (template_string_token("DB"), valid_service_container_token()),
            ])),
            ..AgentJobRequestMessage::default()
        };
        assert!(service_containers(&duplicate_aliases, "trusted").is_err());

        for invalid_key in [Value::Null, template_map_token(Vec::new())] {
            let job = AgentJobRequestMessage {
                job_service_containers: Some(template_map_token(vec![(
                    invalid_key,
                    valid_service_container_token(),
                )])),
                ..AgentJobRequestMessage::default()
            };
            assert!(service_containers(&job, "trusted").is_err());
        }

        let empty_alias = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![(
                template_string_token(""),
                valid_service_container_token(),
            )])),
            ..AgentJobRequestMessage::default()
        };
        assert!(service_containers(&empty_alias, "trusted").is_err());

        assert!(expand_template_token(&template_map_token(vec![(
            template_string_token(""),
            template_string_token("value"),
        )]))
        .is_err());
    }

    #[test]
    fn service_and_environment_entries_keep_template_order() {
        let service = |image: &str, first_env: &str, second_env: &str| {
            template_map_token(vec![
                (template_string_token("image"), template_string_token(image)),
                (
                    template_string_token("env"),
                    template_map_token(vec![
                        (template_string_token(first_env), template_string_token("1")),
                        (
                            template_string_token(second_env),
                            template_string_token("2"),
                        ),
                    ]),
                ),
            ])
        };
        let job = AgentJobRequestMessage {
            job_service_containers: Some(template_map_token(vec![
                (
                    template_string_token("z-service"),
                    service("postgres:16", "Z_FIRST", "A_SECOND"),
                ),
                (
                    template_string_token("a-service"),
                    service("redis:7", "Z_FIRST", "A_SECOND"),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };

        let services = service_containers(&job, "trusted").unwrap();
        assert_eq!(
            services
                .iter()
                .map(|service| service.network_alias.as_str())
                .collect::<Vec<_>>(),
            ["z-service", "a-service"]
        );
        assert_eq!(
            services[0].env,
            vec![
                ("Z_FIRST".to_owned(), "1".to_owned()),
                ("A_SECOND".to_owned(), "2".to_owned())
            ]
        );
    }

    #[test]
    fn job_container_ports_reject_null_and_complex_values_through_shared_validator() {
        for malformed_port in [Value::Null, template_map_token(Vec::new())] {
            let job = AgentJobRequestMessage {
                job_container: Some(template_map_token(vec![(
                    template_string_token("ports"),
                    template_sequence_token(vec![malformed_port]),
                )])),
                ..AgentJobRequestMessage::default()
            };
            let error = github_job_container_spec(
                &job,
                test_container_paths(),
                "docker:latest",
                "node:latest",
                "daemon".to_owned(),
                "trusted",
            )
            .unwrap_err();
            assert!(error.to_string().contains("container port"), "{error:#}");
        }
    }

    #[test]
    fn job_and_service_container_fields_reject_unsupported_payloads_without_leaking_values() {
        let credential_secret = "registry-password-secret";
        let unsupported_fields = [
            (
                "volumes",
                template_sequence_token(vec![template_string_token(
                    "/host/private:/container/private",
                )]),
            ),
            (
                "credentials",
                template_map_token(vec![(
                    template_string_token("password"),
                    serde_json::json!({ "type": 3, "expr": credential_secret }),
                )]),
            ),
        ];

        for (field, value) in unsupported_fields {
            let container_token = || {
                template_map_token(vec![
                    (
                        template_string_token("image"),
                        template_string_token("postgres:16"),
                    ),
                    (template_string_token(field), value.clone()),
                ])
            };

            for trust_scope in ["trusted", "untrusted"] {
                for backend in [
                    velnor_model::ExecutionBackendKind::Docker,
                    velnor_model::ExecutionBackendKind::MicroVm,
                ] {
                    let mut paths = test_container_paths();
                    paths.execution_backend = backend;
                    let job = AgentJobRequestMessage {
                        job_container: Some(container_token()),
                        ..AgentJobRequestMessage::default()
                    };
                    let error = github_job_container_spec(
                        &job,
                        paths,
                        "ubuntu:24.04",
                        "node:latest",
                        "daemon".to_owned(),
                        trust_scope,
                    )
                    .unwrap_err();
                    let detail = format!("{error:#}");
                    assert!(detail.contains(field), "{field}: {detail}");
                    assert!(
                        !detail.contains(credential_secret),
                        "container error leaked a credential value: {detail}"
                    );
                }

                let service_job = AgentJobRequestMessage {
                    job_service_containers: Some(template_map_token(vec![(
                        template_string_token("postgres"),
                        container_token(),
                    )])),
                    ..AgentJobRequestMessage::default()
                };
                let error = service_containers(&service_job, trust_scope).unwrap_err();
                let detail = format!("{error:#}");
                assert!(detail.contains(field), "{field}: {detail}");
                assert!(
                    !detail.contains(credential_secret),
                    "service error leaked a credential value: {detail}"
                );
            }
        }
    }

    #[test]
    fn nonfinite_template_numbers_render_when_stringifying_map_keys() {
        for (wire_value, expected, non_finite) in [
            ("NaN", "NaN", NonFinite::NaN),
            ("Infinity", "Infinity", NonFinite::PositiveInfinity),
            ("-Infinity", "-Infinity", NonFinite::NegativeInfinity),
        ] {
            let token = template_number_token(Value::String(wire_value.to_owned()));
            assert_eq!(
                template_token_context_value(&token).unwrap(),
                ContextValue::non_finite(non_finite)
            );
            let expanded = expand_template_token(&template_map_token(vec![(
                token.clone(),
                template_string_token("value"),
            )]))
            .unwrap();
            let entries = context_object_entries(&expanded).unwrap();
            assert_eq!(
                entries
                    .iter()
                    .find(|(key, _)| key == expected)
                    .map(|(_, value)| value),
                Some(&ContextValue::String("value".to_owned()))
            );

            let expanded_env = expand_template_token(&template_map_token(vec![(
                template_string_token("env"),
                template_map_token(vec![(template_string_token("NONFINITE"), token)]),
            )]))
            .unwrap();
            assert_eq!(
                container_env(&expanded_env).unwrap(),
                vec![("NONFINITE".to_owned(), expected.to_owned())]
            );
        }
    }

    #[test]
    fn job_container_options_read_schema_options() {
        let job = AgentJobRequestMessage {
            job_container: Some(template_map_token(vec![
                (
                    template_string_token("image"),
                    template_string_token("ubuntu:24.04"),
                ),
                (
                    template_string_token("options"),
                    template_string_token(
                        "--cpus 2 --memory 4g -m1g -m=8g -m 16g -im1g -itm1g -im 32g",
                    ),
                ),
            ])),
            ..AgentJobRequestMessage::default()
        };

        // Quota flags are stripped at admission on every trust path:
        // no HostConfig ceiling may arrive via workflow container.options.
        assert_eq!(
            job_container_options(&job, "trusted").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            job_container_options(&job, "untrusted").unwrap(),
            Vec::<String>::new()
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
            vec!["--hostname", "job-host"]
        );
    }

    #[test]
    fn container_options_strip_quota_flags_on_every_trust_path() {
        for allow_privileged in [false, true] {
            let options = vec![
                "--cpus".to_string(),
                "2".to_string(),
                "--memory=4g".to_string(),
                "-m".to_string(),
                "4g".to_string(),
                "-m=8g".to_string(),
                "-m1g".to_string(),
                "-im".to_string(),
                "6g".to_string(),
                "-im1g".to_string(),
                "-itm1g".to_string(),
                "--cpu-quota".to_string(),
                "50000".to_string(),
                "--cpuset-cpus".to_string(),
                "0-1".to_string(),
                "--memory-swap".to_string(),
                "8g".to_string(),
                "--pids-limit".to_string(),
                "512".to_string(),
                "--shm-size".to_string(),
                "256m".to_string(),
                "--hostname".to_string(),
                "job-host".to_string(),
            ];
            // Quota flags and their values vanish; --shm-size (not a
            // ceiling) and ordinary options survive.
            assert_eq!(
                filter_privileged_container_options(options, allow_privileged),
                vec!["--shm-size", "256m", "--hostname", "job-host"],
                "allow_privileged={allow_privileged}"
            );
        }
    }

    #[test]
    fn attached_clustered_memory_flag_does_not_consume_following_token() {
        assert_eq!(
            filter_privileged_container_options(
                vec!["-im1g".into(), "preserved-token".into()],
                true,
            ),
            vec!["preserved-token"]
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
            "-m1g".into(),
            "-im".into(),
            "5g".into(),
            "-im1g".into(),
            "-itm1g".into(),
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

        // Quota flags (--cpus, --memory) are stripped before the
        // allowlist runs; hostile flags are dropped by the allowlist.
        assert_eq!(
            filter_privileged_container_options(options, false),
            vec!["--health-cmd", "true"]
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
            "-zmalformed-name".into(),
            "-m1g".into(),
            "--env".into(),
            "--cpus=2".into(),
            "-e".into(),
            "--memory=4g".into(),
            "--env".into(),
        ];

        // Quota flags are stripped on the trusted path too.
        assert_eq!(
            filter_privileged_container_options(options, true),
            vec!["--hostname", "allowed", "-zmalformed-name"]
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
    fn service_containers_use_legacy_sidecar_resources() {
        let payload = serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Services",
            "requestId": 1,
            "jobSidecarContainers": { "network": "postgres" },
            "resources": {
                "containers": [
                    { "alias": "__job", "properties": { "image": "ubuntu:24.04" } },
                    {
                        "alias": "postgres",
                        "properties": {
                            "image": "postgres:16",
                            "options": "--health-cmd \"pg_isready -U postgres\" --use-api-socket --gpus=all",
                            "env": {
                                "EMPTY": null,
                                "POSTGRES_PASSWORD": "postgres",
                                "DOCKER_HOST": "tcp://attacker.example:2376",
                                "GITHUB_REF": "refs/tags/v9.9.9"
                            },
                            "ports": ["5432", "5433"]
                        }
                    },
                    {
                        "alias": "unrelated",
                        "properties": { "image": "redis:7" }
                    }
                ]
            }
        });
        let mut job = AgentJobRequestMessage::from_value(payload.clone()).unwrap();

        let expected = vec![ServiceContainerSpec {
            name: "velnor-service-cccccccc-cccc-cccc-cccc-cccccccccccc-network".into(),
            image: "postgres:16".into(),
            network_alias: "network".into(),
            network: "velnor-net-cccccccc-cccc-cccc-cccc-cccccccccccc".into(),
            env: vec![
                ("EMPTY".into(), "".into()),
                ("POSTGRES_PASSWORD".into(), "postgres".into()),
            ],
            ports: vec!["5432".into(), "5433".into()],
            options: vec!["--health-cmd".into(), "pg_isready -U postgres".into()],
        }];
        assert_eq!(service_containers(&job, "trusted").unwrap(), expected);

        // The wire callback has already resolved the legacy sidecar into the
        // service token. Adapter behavior must not depend on re-reading the
        // resource collection after that point.
        assert!(job.job_service_containers.is_some());
        let duplicate = ContainerResource {
            alias: Some("postgres".into()),
            endpoint: None,
            properties: velnor_model::ContextValue::from_json_root_case_insensitive(
                serde_json::json!({ "image": "wrong:latest" }),
            )
            .unwrap(),
        };
        job.resources.containers.push(None);
        job.resources.containers.push(Some(duplicate));
        assert_eq!(service_containers(&job, "trusted").unwrap(), expected);

        let mut typed_null_payload = payload.clone();
        typed_null_payload["jobServiceContainers"] = serde_json::json!({ "type": 7 });
        let typed_null_job = AgentJobRequestMessage::from_value(typed_null_payload).unwrap();
        assert_eq!(
            service_containers(&typed_null_job, "trusted").unwrap(),
            expected
        );

        let mut actual_null_payload = payload.clone();
        actual_null_payload["jobServiceContainers"] = serde_json::Value::Null;
        let actual_null_job = AgentJobRequestMessage::from_value(actual_null_payload).unwrap();
        assert_eq!(
            service_containers(&actual_null_job, "trusted").unwrap(),
            expected
        );

        let mut non_null_payload = payload;
        non_null_payload["jobServiceContainers"] = serde_json::json!("invalid");
        let non_null_job = AgentJobRequestMessage::from_value(non_null_payload).unwrap();
        assert!(service_containers(&non_null_job, "trusted").is_err());

        let untrusted = service_containers(&job, "untrusted").unwrap();
        assert!(untrusted[0].ports.is_empty());
        assert_eq!(
            untrusted[0].options,
            vec!["--health-cmd", "pg_isready -U postgres"]
        );
    }

    #[test]
    fn service_containers_ignore_resources_without_sidecars() {
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "resources": { "containers": [
                null,
                { "alias": "unrelated", "properties": { "image": "redis:7" } },
                { "alias": "another-unrelated", "properties": { "image": "postgres:16" } }
            ] }
        }))
        .unwrap();

        assert!(job.job_sidecar_containers.is_empty());
        assert!(job.job_service_containers.is_none());
        assert!(service_containers(&job, "trusted").unwrap().is_empty());
    }

    #[test]
    fn service_containers_reject_string_tokens_instead_of_falling_back() {
        for containers in [
            serde_json::json!([
                { "alias": "postgres", "properties": { "image": "postgres:16" } }
            ]),
            serde_json::json!([]),
        ] {
            let job = AgentJobRequestMessage::from_value(serde_json::json!({
                "jobServiceContainers": { "type": 0, "lit": null },
                "jobSidecarContainers": { "network": "postgres" },
                "resources": { "containers": containers }
            }))
            .unwrap();

            assert!(service_containers(&job, "trusted").is_err());
        }
    }

    #[test]
    fn wire_callback_preserves_legacy_sidecar_single_errors() {
        for containers in [
            serde_json::json!([]),
            serde_json::json!([null]),
            serde_json::json!([
                { "alias": "sidecar" },
                { "alias": "sidecar" }
            ]),
        ] {
            assert!(AgentJobRequestMessage::from_value(serde_json::json!({
                "jobSidecarContainers": { "network": "sidecar" },
                "resources": { "containers": containers }
            }))
            .is_err());
        }
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
        let job = AgentJobRequestMessage::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa" },
            "timeline": { "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" },
            "jobId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "jobDisplayName": "Services",
            "requestId": 1,
            "jobServiceContainers": { "type": 2, "map": [
                { "Key": scalar("postgres"), "Value": service }
            ] },
            "resources": { "containers": [
                { "alias": "legacy", "properties": { "image": "redis:7" } }
            ] }
        }))
        .unwrap();

        let services = service_containers(&job, "trusted").unwrap();
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

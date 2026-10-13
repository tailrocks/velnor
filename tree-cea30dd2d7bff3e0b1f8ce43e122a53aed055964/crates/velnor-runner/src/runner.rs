use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fs, path::PathBuf};

use crate::{
    action::{
        composite_action_invocations, composite_repository_action_plans,
        composite_repository_action_plans_from_resolved, download_repository_actions,
        is_local_action_step, local_action_plans_with_context, repository_action_plans,
        resolve_local_action, ActionMetadata, ActionRuntime, CompositeActionInvocation,
        LocalActionPlan, RepositoryActionPlan, ResolvedAction,
    },
    checkout::{checkout_plans, execute_checkouts},
    cli::{ConfigureArgs, RemoveArgs, RunArgs, StatusArgs},
    config::{self, CredentialScheme, RunnerSettings, StoredCredentials, StoredRunnerConfig},
    container::JobContainerSpec,
    executor::{DockerScriptExecutor, ExecutableStep, ProcessCommandRunner},
    job_message::{ActionReferenceType, AgentJobRequestMessage, PIPELINE_AGENT_JOB_REQUEST},
    protocol::{
        DistributedTaskClient, GitHubAuthResult, GitHubScope, JobCompletedEvent, OAuthClient,
        OAuthJwtCredentials, RegistrationClient, RunnerEvent, RunnerKeyPair, RunnerStatus,
        TaskAgent, TaskAgentPool, TaskAgentSession, TaskResult, TimelineRecord,
        TimelineRecordFeedLines,
    },
    runtime_env::job_runtime_env,
    script_step::github_script_steps_with_defaults,
};

pub async fn configure(args: ConfigureArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let scope = GitHubScope::parse(&args.url)?;
    let agent_name = args.name.unwrap_or_else(default_agent_name);
    let labels = normalize_labels(args.labels);
    let key_pair = if args.dry_run {
        None
    } else {
        Some(RunnerKeyPair::generate()?)
    };
    let agent = TaskAgent::new(
        agent_name.clone(),
        labels.clone(),
        key_pair
            .as_ref()
            .map(|key_pair| key_pair.public_key.clone()),
        false,
    );
    let registration = if args.dry_run {
        None
    } else {
        let registration_client = RegistrationClient::new()?;
        let runner_token = runner_token(
            &registration_client,
            &scope,
            args.token.as_ref(),
            args.pat.as_ref(),
            RunnerTokenKind::Registration,
        )
        .await?;
        let auth = registration_client
            .exchange_tenant_credential(&scope, &runner_token, RunnerEvent::Register)
            .await?;
        let client = DistributedTaskClient::new(&auth.server_url, &auth.token)?;
        let pools = client.get_agent_pools(args.pool_name.as_deref()).await?;
        let pool = select_pool(&pools, args.pool_id, args.pool_name.as_deref())?;
        let existing = client.get_agents(pool.id, &agent_name).await?;
        let registered_agent = if let Some(existing_agent) = existing.first() {
            if !args.replace {
                bail!("runner '{}' already exists; pass --replace", agent_name);
            }
            client
                .replace_agent(pool.id, &agent.clone().with_id(existing_agent.id()?))
                .await?
        } else {
            client.add_agent(pool.id, &agent).await?
        };

        Some(RegistrationResult {
            auth,
            pool,
            agent: registered_agent,
        })
    };
    let credentials = match (&registration, &key_pair) {
        (Some(registration), Some(key_pair)) => Some(stored_credentials(
            &registration.auth,
            &registration.agent,
            key_pair,
        )?),
        _ => None,
    };

    let stored = StoredRunnerConfig {
        settings: RunnerSettings {
            github_url: scope.original_url.clone(),
            server_url: registration
                .as_ref()
                .map(|registration| registration.auth.server_url.clone()),
            server_url_v2: None,
            pool_id: registration
                .as_ref()
                .map(|registration| registration.pool.id),
            pool_name: registration
                .as_ref()
                .and_then(|registration| registration.pool.name.clone()),
            agent_id: registration
                .as_ref()
                .and_then(|registration| registration.agent.id),
            agent_name,
            labels,
            use_v2_flow: registration
                .as_ref()
                .is_some_and(|registration| registration.auth.use_v2_flow),
            ephemeral: false,
            disable_update: true,
        },
        credentials,
    };

    config::save(&dir, &stored)?;
    println!("Wrote local runner config to {}", dir.display());
    println!("GitHub scope API: {}", scope.api_base_url);
    println!(
        "Tenant credential endpoint: {}",
        scope.tenant_credential_url
    );
    println!("Runner token endpoint: {}", scope.registration_token_url);
    println!(
        "Prepared TaskAgent payload for '{}' with {} label(s).",
        agent.name,
        agent.labels.len()
    );
    if let Some(registration) = registration {
        println!(
            "Registered agent id {} in pool {}.",
            registration.agent.id.unwrap_or_default(),
            registration.pool.id
        );
        println!("Session create/message poll is the next Milestone 0 step.");
    } else {
        println!("Dry run: skipped tenant credential exchange.");
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum RunnerTokenKind {
    Registration,
    Remove,
}

async fn runner_token(
    client: &RegistrationClient,
    scope: &GitHubScope,
    explicit_token: Option<&String>,
    pat: Option<&String>,
    kind: RunnerTokenKind,
) -> Result<String> {
    if let Some(token) = explicit_token {
        return Ok(token.clone());
    }

    if let Some(pat) = pat {
        let token = match kind {
            RunnerTokenKind::Registration => {
                client.create_runner_registration_token(scope, pat).await?
            }
            RunnerTokenKind::Remove => client.create_runner_remove_token(scope, pat).await?,
        };
        println!(
            "Fetched short-lived runner {:?} token; expires at {}.",
            kind, token.expires_at
        );
        return Ok(token.token);
    }

    bail!("runner token required: pass --token or --pat")
}

struct RegistrationResult {
    auth: GitHubAuthResult,
    pool: TaskAgentPool,
    agent: TaskAgent,
}

fn stored_credentials(
    auth: &GitHubAuthResult,
    agent: &TaskAgent,
    key_pair: &RunnerKeyPair,
) -> Result<StoredCredentials> {
    if let Some(authorization) = &agent.authorization {
        if let (Some(client_id), Some(authorization_url)) =
            (&authorization.client_id, &authorization.authorization_url)
        {
            return Ok(StoredCredentials {
                scheme: CredentialScheme::OAuth,
                data: json!({
                    "clientId": client_id,
                    "authorizationUrl": authorization_url,
                    "privateKeyPem": key_pair.private_key_pem,
                    "requireFipsCryptography": "false",
                }),
            });
        }
    }

    Ok(StoredCredentials {
        scheme: credential_scheme(&auth.token_schema)?,
        data: json!({
            "token": auth.token,
        }),
    })
}

fn credential_scheme(token_schema: &str) -> Result<CredentialScheme> {
    if token_schema.eq_ignore_ascii_case("OAuthAccessToken") {
        Ok(CredentialScheme::OAuthAccessToken)
    } else {
        bail!("unsupported GitHub runner token schema: {token_schema}")
    }
}

fn select_pool(
    pools: &[TaskAgentPool],
    pool_id: Option<i64>,
    pool_name: Option<&str>,
) -> Result<TaskAgentPool> {
    if let Some(pool_id) = pool_id {
        return pools
            .iter()
            .find(|pool| pool.id == pool_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("runner pool id {pool_id} not found"));
    }

    if let Some(pool_name) = pool_name {
        return pools
            .iter()
            .find(|pool| {
                pool.name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(pool_name))
            })
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("runner pool '{pool_name}' not found"));
    }

    pools
        .iter()
        .find(|pool| pool.is_internal && !pool.is_hosted)
        .or_else(|| pools.iter().find(|pool| !pool.is_hosted))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no self-hosted runner pool found"))
}

trait TaskAgentExt {
    fn id(&self) -> Result<i64>;
}

impl TaskAgentExt for TaskAgent {
    fn id(&self) -> Result<i64> {
        self.id
            .ok_or_else(|| anyhow::anyhow!("GitHub returned agent without id"))
    }
}

pub async fn run(args: RunArgs) -> Result<()> {
    if args.complete_noop && args.execute_scripts {
        bail!("--complete-noop and --execute-scripts are mutually exclusive");
    }

    let dir = config::config_dir(args.config_dir)?;
    let stored = config::load(&dir)?;
    let server_url = stored
        .settings
        .server_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("runner is not registered: missing server_url"))?;
    let pool_id = stored
        .settings
        .pool_id
        .ok_or_else(|| anyhow::anyhow!("runner is not registered: missing pool_id"))?;
    let agent_id = stored
        .settings
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("runner is not registered: missing agent_id"))?;
    let token = oauth_access_token(&stored).await?;
    let client = DistributedTaskClient::new(server_url, token)?;
    let owner_name = format!("{} (PID: {})", default_agent_name(), std::process::id());
    let session = TaskAgentSession::new(owner_name, agent_id, stored.settings.agent_name.clone());
    let session = client.create_session(pool_id, &session).await?;
    let session_id = session
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("GitHub returned session without sessionId"))?;

    println!(
        "Runner '{}' ready with labels: {}",
        stored.settings.agent_name,
        stored.settings.labels.join(",")
    );
    println!("Created runner session {session_id}.");

    let message = client
        .get_message(
            pool_id,
            session_id,
            None,
            RunnerStatus::Online,
            stored.settings.disable_update,
        )
        .await?;

    if let Some(message) = message {
        println!(
            "Received message {} type {}.",
            message.message_id, message.message_type
        );
        if message
            .message_type
            .eq_ignore_ascii_case(PIPELINE_AGENT_JOB_REQUEST)
        {
            let job = AgentJobRequestMessage::parse_json(&message.body)?;
            println!(
                "Parsed job request {} for job '{}' ({} step(s), {} endpoint(s)).",
                job.request_id,
                job.job_display_name,
                job.steps.len(),
                job.resources.endpoints.len()
            );
            let script_steps =
                match github_script_steps_with_defaults(&job.steps, "/__w", &job.defaults) {
                    Ok(script_steps) => {
                        println!("Mapped {} script run step(s).", script_steps.len());
                        Some(script_steps)
                    }
                    Err(error) => {
                        println!("Script step mapping is incomplete: {error}.");
                        None
                    }
                };
            if let Some(system_connection) = job.system_connection() {
                println!(
                    "System connection URL: {}",
                    system_connection.url.as_deref().unwrap_or("unknown")
                );
            }
            if args.execute_scripts {
                let Some(script_steps) = script_steps else {
                    bail!("cannot execute scripts because step mapping failed");
                };
                let job_result = execute_script_job(
                    &dir,
                    args.work_dir.clone(),
                    &args.docker_image,
                    &job,
                    &script_steps,
                )?;
                complete_job(
                    &client,
                    pool_id,
                    session_id,
                    message.message_id,
                    &stored.settings.agent_name,
                    &job,
                    job_result.result,
                    job_result.outputs,
                    "Velnor executed supported run and JavaScript action steps in Docker."
                        .to_string(),
                )
                .await?;
                println!(
                    "Job completed with result {:?} and message acknowledged.",
                    job_result.result
                );
            } else if args.complete_noop {
                complete_job(
                    &client,
                    pool_id,
                    session_id,
                    message.message_id,
                    &stored.settings.agent_name,
                    &job,
                    TaskResult::Succeeded,
                    BTreeMap::new(),
                    "Velnor no-op completion probe: user steps were not executed.".to_string(),
                )
                .await?;
                println!("No-op job completed and message acknowledged.");
            } else {
                println!("Job is not acknowledged yet; pass --complete-noop to probe completion.");
            }
        } else {
            println!("Message is not acknowledged because type is not implemented.");
        }
    } else {
        println!("No message received.");
    }

    if args.once {
        client.delete_session(pool_id, session_id).await?;
        println!("Deleted runner session.");
    } else {
        println!("Continuous polling is not implemented yet; use --once for current milestone.");
    }

    Ok(())
}

fn execute_script_job(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    docker_image: &str,
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
) -> Result<ScriptJobResult> {
    let job_dir = job_work_dir(config_dir, work_dir, job);
    let workspace = job_dir.join("workspace");
    let temp = job_dir.join("temp");
    let actions = job_dir.join("actions");
    let tools = job_dir.join("tools");
    for path in [&workspace, &temp, &actions, &tools] {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    let mut command_runner = ProcessCommandRunner;
    let checkout_plans = checkout_plans(job, &workspace)?;
    execute_checkouts(&mut command_runner, &checkout_plans)?;
    let context_data = job_context_data(job);
    let local_action_plans =
        local_action_plans_with_context(&job.steps, &workspace, &context_data)?;
    let local_actions = local_action_plans
        .iter()
        .map(|plan| Ok((plan.clone(), resolve_local_action(plan)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut repository_action_plans = repository_action_plans(&job.steps, &actions)?;
    repository_action_plans.extend(composite_repository_action_plans(&local_actions, &actions)?);
    let resolved_actions = if repository_action_plans.is_empty() {
        Vec::new()
    } else {
        let resolved_actions = download_repository_actions_recursive(
            &mut command_runner,
            &repository_action_plans,
            &actions,
        )?;
        println!(
            "Downloaded and resolved {} repository action(s).",
            resolved_actions.len()
        );
        resolved_actions
    };
    let ordered_steps = ordered_executable_steps(
        job,
        script_steps,
        &resolved_actions,
        &local_actions,
        &actions,
    )?;

    let container = JobContainerSpec {
        name: format!("velnor-job-{}", sanitize_path_segment(&job.job_id)),
        image: docker_image.to_string(),
        network: format!("velnor-net-{}", sanitize_path_segment(&job.job_id)),
        workspace_host: workspace,
        temp_host: temp.clone(),
        actions_host: actions,
        tools_host: tools,
        mount_docker_socket: true,
    };
    let mut executor = DockerScriptExecutor::new(command_runner);
    let base_env = job_runtime_env(job);
    let summary = executor.execute_ordered_steps_with_job_outputs(
        &container,
        &ordered_steps,
        &base_env,
        &context_data,
        job.job_outputs.as_ref(),
        &temp,
    )?;
    if !summary.job_outputs.is_empty() {
        println!("Evaluated {} job output(s).", summary.job_outputs.len());
    }
    let failed = summary
        .step_results
        .iter()
        .any(|result| result.exit_code != 0 && !result.failure_ignored);

    let result = if failed {
        TaskResult::Failed
    } else {
        TaskResult::Succeeded
    };
    Ok(ScriptJobResult {
        result,
        outputs: summary.job_outputs,
    })
}

#[derive(Debug, Clone)]
struct ScriptJobResult {
    result: TaskResult,
    outputs: BTreeMap<String, String>,
}

fn job_context_data(job: &AgentJobRequestMessage) -> Vec<(String, Value)> {
    let mut context_data = job.context_data.clone();
    let mut synthesized_secrets = Map::new();
    for (name, variable) in &job.variables {
        if !variable.is_secret {
            continue;
        }
        let Some(value) = variable.value.as_ref() else {
            continue;
        };
        for secret_name in secret_context_names(name) {
            synthesized_secrets
                .entry(secret_name)
                .or_insert_with(|| Value::String(value.clone()));
        }
    }

    if !synthesized_secrets.is_empty() {
        match context_data.get_mut("secrets") {
            Some(Value::Object(secrets)) => {
                for (name, value) in synthesized_secrets {
                    secrets.entry(name).or_insert(value);
                }
            }
            Some(_) => {}
            None => {
                context_data.insert("secrets".to_string(), Value::Object(synthesized_secrets));
            }
        }
    }

    context_data.into_iter().collect()
}

fn secret_context_names(variable_name: &str) -> Vec<String> {
    if variable_name.eq_ignore_ascii_case("system.github.token") {
        return vec!["GITHUB_TOKEN".to_string()];
    }
    for prefix in ["secrets.", "secret."] {
        if let Some(name) = variable_name.strip_prefix(prefix) {
            if !name.is_empty() {
                return vec![name.to_string()];
            }
        }
    }
    if variable_name.contains('.') {
        Vec::new()
    } else {
        vec![variable_name.to_string()]
    }
}

fn download_repository_actions_recursive<R>(
    runner: &mut R,
    initial_plans: &[RepositoryActionPlan],
    actions_host: &std::path::Path,
) -> Result<Vec<ResolvedAction>>
where
    R: crate::executor::CommandRunner,
{
    let mut resolved = Vec::new();
    let mut pending = initial_plans.to_vec();
    while !pending.is_empty() {
        let next = download_repository_actions(runner, &pending)?;
        resolved.extend(next);

        let nested = composite_repository_action_plans_from_resolved(&resolved, actions_host)?;
        let previous_pending = pending;
        pending = nested
            .into_iter()
            .filter(|plan| {
                !resolved
                    .iter()
                    .any(|action| same_action(&action.plan, plan))
                    && !previous_pending
                        .iter()
                        .any(|existing| same_action(existing, plan))
            })
            .collect();
    }
    Ok(resolved)
}

fn same_action(left: &RepositoryActionPlan, right: &RepositoryActionPlan) -> bool {
    left.step_id == right.step_id
        && left.repository == right.repository
        && left.git_ref == right.git_ref
        && left.source_path == right.source_path
}

fn ordered_executable_steps(
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
    resolved_actions: &[ResolvedAction],
    local_actions: &[(LocalActionPlan, ActionMetadata)],
    actions_host: &std::path::Path,
) -> Result<Vec<ExecutableStep>> {
    let mut ordered = Vec::new();
    let mut script_iter = script_steps.iter();
    let mut local_iter = local_actions.iter();
    for step in job.steps.iter().filter(|step| step.enabled) {
        match step.reference_type() {
            Some(ActionReferenceType::Script) => {
                let script = script_iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("script step mapping count mismatch"))?;
                ordered.push(ExecutableStep::Script(script.clone()));
            }
            Some(ActionReferenceType::Repository) => {
                if is_local_action_step(step) {
                    let (plan, metadata) = local_iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("local action mapping count mismatch"))?;
                    for invocation in
                        composite_action_invocations(plan, metadata, "/__w", actions_host)?
                    {
                        match invocation {
                            CompositeActionInvocation::Script(script) => {
                                ordered.push(ExecutableStep::Script(script));
                            }
                            CompositeActionInvocation::Repository(plan) => {
                                let action = resolved_actions
                                    .iter()
                                    .find(|action| action.plan.step_id == plan.step_id)
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "nested repository action '{}' was not resolved",
                                            plan.step_id
                                        )
                                    })?;
                                append_resolved_action_steps(
                                    &mut ordered,
                                    action,
                                    resolved_actions,
                                    actions_host,
                                    None,
                                )?;
                            }
                            CompositeActionInvocation::Outputs(outputs) => {
                                ordered.push(ExecutableStep::CompositeOutputs {
                                    step_id: outputs.step_id,
                                    outputs: outputs.outputs,
                                    condition: None,
                                });
                            }
                        }
                    }
                    continue;
                }
                let Some(reference) = step.reference.as_ref() else {
                    continue;
                };
                let Some(repository) = reference.name.as_deref() else {
                    continue;
                };
                if repository.eq_ignore_ascii_case("actions/checkout") {
                    continue;
                }
                let git_ref = reference.git_ref.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("repository action '{repository}' missing ref")
                })?;
                let action = resolved_actions
                    .iter()
                    .find(|action| {
                        action.plan.repository == repository
                            && action.plan.git_ref == git_ref
                            && action.plan.source_path.as_deref() == reference.path.as_deref()
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "repository action '{repository}@{git_ref}' was not resolved"
                        )
                    })?;
                append_resolved_action_steps(
                    &mut ordered,
                    action,
                    resolved_actions,
                    actions_host,
                    None,
                )?;
            }
            _ => bail!("unsupported enabled step in job"),
        }
    }
    Ok(ordered)
}

fn append_resolved_action_steps(
    ordered: &mut Vec<ExecutableStep>,
    action: &ResolvedAction,
    resolved_actions: &[ResolvedAction],
    actions_host: &std::path::Path,
    parent_condition: Option<&str>,
) -> Result<()> {
    match &action.runtime {
        ActionRuntime::JavaScript { .. } => ordered.push(ExecutableStep::JavaScript {
            step_id: action.plan.step_id.clone(),
            invocation: action.javascript_invocation(actions_host)?,
            condition: combine_conditions(parent_condition, action.plan.condition.as_deref()),
            continue_on_error: action.plan.continue_on_error,
        }),
        ActionRuntime::Composite => {
            let action_condition =
                combine_conditions(parent_condition, action.plan.condition.as_deref());
            for invocation in action.composite_invocations("/__w", actions_host)? {
                match invocation {
                    CompositeActionInvocation::Script(mut script) => {
                        script.condition = combine_conditions(
                            action_condition.as_deref(),
                            script.condition.as_deref(),
                        );
                        script.continue_on_error |= action.plan.continue_on_error;
                        ordered.push(ExecutableStep::Script(script));
                    }
                    CompositeActionInvocation::Repository(plan) => {
                        let nested = resolved_actions
                            .iter()
                            .find(|resolved| resolved.plan.step_id == plan.step_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "nested repository action '{}' was not resolved",
                                    plan.step_id
                                )
                            })?;
                        append_resolved_action_steps(
                            ordered,
                            nested,
                            resolved_actions,
                            actions_host,
                            action_condition.as_deref(),
                        )?;
                    }
                    CompositeActionInvocation::Outputs(outputs) => {
                        ordered.push(ExecutableStep::CompositeOutputs {
                            step_id: outputs.step_id,
                            outputs: outputs.outputs,
                            condition: action_condition.clone(),
                        });
                    }
                }
            }
        }
        ActionRuntime::Docker { image } => {
            bail!(
                "Docker action '{}' ({image}) is not implemented yet",
                action.plan.repository
            )
        }
    }
    Ok(())
}

fn combine_conditions(parent: Option<&str>, child: Option<&str>) -> Option<String> {
    match (
        parent.filter(|value| !value.trim().is_empty()),
        child.filter(|value| !value.trim().is_empty()),
    ) {
        (Some(parent), Some(child)) => Some(format!(
            "${{{{ ({}) && ({}) }}}}",
            strip_condition_expression(parent),
            strip_condition_expression(child)
        )),
        (Some(parent), None) => Some(parent.to_string()),
        (None, Some(child)) => Some(child.to_string()),
        (None, None) => None,
    }
}

fn strip_condition_expression(condition: &str) -> &str {
    condition
        .trim()
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .unwrap_or_else(|| condition.trim())
}

fn job_work_dir(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    job: &AgentJobRequestMessage,
) -> PathBuf {
    work_dir
        .unwrap_or_else(|| config_dir.join("_work"))
        .join(sanitize_path_segment(&job.job_id))
}

async fn complete_job(
    client: &DistributedTaskClient,
    pool_id: i64,
    session_id: &str,
    message_id: i64,
    worker_name: &str,
    job: &AgentJobRequestMessage,
    result: TaskResult,
    job_outputs: BTreeMap<String, String>,
    feed_line: String,
) -> Result<()> {
    let scope_identifier = job
        .plan
        .scope_identifier
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("job plan missing scopeIdentifier"))?;
    let hub_name = job.plan.plan_type.as_deref().unwrap_or("build");
    let start_time = utc_now_rfc3339()?;
    let record = TimelineRecord::job_pending(
        job.job_id.clone(),
        job.job_display_name.clone(),
        job.job_name.clone(),
        worker_name,
    )
    .in_progress(start_time);

    client
        .renew_agent_request(pool_id, job.request_id, Some(&job.plan.plan_id))
        .await?;
    client
        .update_timeline_records(
            scope_identifier,
            hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            vec![record.clone()],
        )
        .await?;
    client
        .append_timeline_record_feed(
            scope_identifier,
            hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            &job.job_id,
            TimelineRecordFeedLines::new(
                job.job_id.clone(),
                vec![mask_text(&feed_line, &job_mask_values(job))],
                Some(1),
            ),
        )
        .await?;

    let finish_time = utc_now_rfc3339()?;
    client
        .update_timeline_records(
            scope_identifier,
            hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            vec![record.completed(finish_time.clone(), result)],
        )
        .await?;
    client
        .raise_job_completed_event(
            scope_identifier,
            hub_name,
            &job.plan.plan_id,
            &JobCompletedEvent::new(job.request_id, job.job_id.clone(), result, job_outputs),
        )
        .await?;
    client
        .finish_agent_request(pool_id, job.request_id, finish_time, result)
        .await?;
    client
        .delete_message(pool_id, message_id, session_id)
        .await
        .context("acknowledge completed job message")?;

    Ok(())
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

fn job_mask_values(job: &AgentJobRequestMessage) -> Vec<String> {
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

fn mask_text(value: &str, masks: &[String]) -> String {
    masks
        .iter()
        .filter(|mask| !mask.is_empty())
        .fold(value.to_string(), |masked, mask| {
            masked.replace(mask, "***")
        })
}

fn utc_now_rfc3339() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .context("format UTC timestamp")
}

async fn oauth_access_token(stored: &StoredRunnerConfig) -> Result<String> {
    let credentials = stored
        .credentials
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("runner is not registered: missing credentials"))?;

    match credentials.scheme {
        CredentialScheme::OAuthAccessToken => credentials
            .data
            .get("token")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("OAuthAccessToken credentials missing token")),
        CredentialScheme::OAuth => {
            let oauth = OAuthJwtCredentials {
                client_id: credential_str(credentials, "clientId")?,
                authorization_url: credential_str(credentials, "authorizationUrl")?,
                private_key_pem: credential_str(credentials, "privateKeyPem")?,
            };
            OAuthClient::new()?
                .exchange_client_credentials(&oauth)
                .await
        }
    }
}

fn credential_str(credentials: &StoredCredentials, key: &str) -> Result<String> {
    credentials
        .data
        .get(key)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("OAuth credentials missing {key}"))
}

pub async fn remove(args: RemoveArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let stored = config::load(&dir).ok();

    if !args.local_only && (args.token.is_some() || args.pat.is_some()) {
        let stored = stored
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("local runner config is required for remote remove"))?;
        let scope = GitHubScope::parse(&stored.settings.github_url)?;
        let pool_id = stored
            .settings
            .pool_id
            .ok_or_else(|| anyhow::anyhow!("local runner config missing pool_id"))?;
        let agent_id = stored
            .settings
            .agent_id
            .ok_or_else(|| anyhow::anyhow!("local runner config missing agent_id"))?;
        let registration_client = RegistrationClient::new()?;
        let remove_token = runner_token(
            &registration_client,
            &scope,
            args.token.as_ref(),
            args.pat.as_ref(),
            RunnerTokenKind::Remove,
        )
        .await?;
        let auth = registration_client
            .exchange_tenant_credential(&scope, &remove_token, RunnerEvent::Remove)
            .await?;
        let client = DistributedTaskClient::new(&auth.server_url, &auth.token)?;
        client.delete_agent(pool_id, agent_id).await?;
        println!("Removed remote runner agent {agent_id} from pool {pool_id}.");
    } else if !args.local_only {
        println!("Remote remove skipped; pass --pat or --token to unregister from GitHub.");
    }

    if config::remove(&dir)? {
        println!("Removed local runner config from {}", dir.display());
    } else {
        println!("No local runner config at {}", dir.display());
    }
    Ok(())
}

pub async fn status(args: StatusArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let stored = config::load(&dir)?;
    println!("Config dir: {}", dir.display());
    println!("GitHub URL: {}", stored.settings.github_url);
    println!("Runner name: {}", stored.settings.agent_name);
    println!("Labels: {}", stored.settings.labels.join(","));
    println!("Use V2 flow: {}", stored.settings.use_v2_flow);
    println!(
        "Credentials stored: {}",
        if stored.credentials.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    Ok(())
}

fn normalize_labels(mut labels: Vec<String>) -> Vec<String> {
    if labels.is_empty() {
        labels.push("velnor".to_string());
    }
    labels.sort();
    labels.dedup();
    labels
}

fn default_agent_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "velnor-runner".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{
        parse_action_metadata, ActionRuntime, LocalActionPlan, RepositoryActionPlan,
    };
    use std::path::Path;

    #[test]
    fn job_context_data_synthesizes_secrets_from_secret_variables() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Secrets",
            "requestId": 1,
            "variables": {
                "system.github.token": { "value": "ghs_token", "isSecret": true },
                "secrets.DOCKERHUB_TOKEN": { "value": "docker_secret", "isSecret": true },
                "DOCKERHUB_USERNAME": { "value": "docker_user", "isSecret": true },
                "PUBLIC_VALUE": { "value": "visible", "isSecret": false }
            },
            "contextData": {
                "secrets": {
                    "DOCKERHUB_TOKEN": "server_secret"
                },
                "matrix": {
                    "target": "linux"
                }
            }
        }))
        .unwrap();

        let context_data: BTreeMap<_, _> = job_context_data(&job).into_iter().collect();
        let secrets = context_data["secrets"].as_object().unwrap();

        assert_eq!(secrets["GITHUB_TOKEN"], "ghs_token");
        assert_eq!(secrets["DOCKERHUB_TOKEN"], "server_secret");
        assert_eq!(secrets["DOCKERHUB_USERNAME"], "docker_user");
        assert!(!secrets.contains_key("PUBLIC_VALUE"));
        assert_eq!(context_data["matrix"]["target"], "linux");
    }

    #[test]
    fn job_mask_values_include_mask_hints_and_secret_variables() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Mask",
            "requestId": 1,
            "mask": [
                { "type": "Variable", "value": "hint-secret" }
            ],
            "variables": {
                "system.github.token": { "value": "ghs_token", "isSecret": true },
                "PUBLIC_VALUE": { "value": "visible", "isSecret": false }
            }
        }))
        .unwrap();

        let masks = job_mask_values(&job);

        assert!(masks.contains(&"hint-secret".to_string()));
        assert!(masks.contains(&"ghs_token".to_string()));
        assert!(!masks.contains(&"visible".to_string()));
        assert_eq!(
            mask_text("hint-secret ghs_token visible", &masks),
            "*** *** visible"
        );
    }

    #[test]
    fn ordered_steps_expand_local_composite_action() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1,
            "steps": [
                {
                    "id": "run",
                    "reference": { "type": "Script" },
                    "inputs": { "script": "echo before" }
                },
                {
                    "id": "aggregate",
                    "reference": {
                        "type": "Repository",
                        "name": "./.github/actions/aggregate-needs"
                    },
                    "inputs": { "workflow-label": "CI" }
                }
            ]
        }))
        .unwrap();
        let script_steps = crate::script_step::github_script_steps(&job.steps, "/__w").unwrap();
        let local_plan = LocalActionPlan {
            step_id: "aggregate".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/aggregate-needs"),
            inputs: [("workflow-label".to_string(), "CI".to_string())].into(),
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - shell: bash
      run: echo "${{ inputs.workflow-label }}"
"#,
        )
        .unwrap();

        let ordered = ordered_executable_steps(
            &job,
            &script_steps,
            &[],
            &[(local_plan, metadata)],
            Path::new("/tmp/actions"),
        )
        .unwrap();

        assert_eq!(ordered.len(), 2);
        assert!(matches!(ordered[0], ExecutableStep::Script(_)));
        let ExecutableStep::Script(step) = &ordered[1] else {
            panic!("local composite should expand to script step")
        };
        assert_eq!(step.id, "aggregate-1");
        assert!(step.script.contains("echo \"CI\""));
    }

    #[test]
    fn ordered_steps_expand_nested_composite_repository_action() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Docs",
            "requestId": 1,
            "steps": [{
                "id": "docs",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/check-deployed-docs"
                },
                "inputs": { "github-token": "ghs_token" }
            }]
        }))
        .unwrap();
        let local_plan = LocalActionPlan {
            step_id: "docs".into(),
            action_dir: Path::new("/tmp/workspace").join(".github/actions/check-deployed-docs"),
            inputs: [("github-token".to_string(), "ghs_token".to_string())].into(),
        };
        let local_metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - uses: jdx/mise-action@v4
      with:
        github_token: ${{ inputs.github-token }}
"#,
        )
        .unwrap();
        let nested_metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: dist/index.js\n").unwrap();
        let nested_plan = RepositoryActionPlan {
            step_id: "docs-1".into(),
            repository: "jdx/mise-action".into(),
            git_ref: "v4".into(),
            source_path: None,
            repository_dir: Path::new("/tmp/actions").join("_actions/jdx_mise-action/v4"),
            action_dir: Path::new("/tmp/actions").join("_actions/jdx_mise-action/v4"),
            inputs: [("github_token".to_string(), "ghs_token".to_string())].into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let resolved = ResolvedAction {
            plan: nested_plan,
            metadata_path: Path::new("/tmp/actions").join("_actions/jdx_mise-action/v4/action.yml"),
            metadata: nested_metadata,
            runtime: ActionRuntime::JavaScript {
                node: "node20".into(),
                main: "dist/index.js".into(),
            },
        };

        let ordered = ordered_executable_steps(
            &job,
            &[],
            &[resolved],
            &[(local_plan, local_metadata)],
            Path::new("/tmp/actions"),
        )
        .unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::JavaScript {
            step_id,
            invocation,
            ..
        } = &ordered[0]
        else {
            panic!("nested repository action should expand to JavaScript step")
        };
        assert_eq!(step_id, "docs-1");
        assert!(invocation
            .env
            .contains(&("INPUT_GITHUB_TOKEN".into(), "ghs_token".into())));
    }

    #[test]
    fn ordered_steps_expand_repository_composite_action() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Toolchain",
            "requestId": 1,
            "steps": [{
                "id": "rust",
                "reference": {
                    "type": "Repository",
                    "name": "dtolnay/rust-toolchain",
                    "ref": "stable"
                },
                "condition": "runner.os == 'Linux'",
                "inputs": { "toolchain": "stable" }
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "rust".into(),
            repository: "dtolnay/rust-toolchain".into(),
            git_ref: "stable".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/dtolnay_rust-toolchain/stable"),
            action_dir: actions_host.join("_actions/dtolnay_rust-toolchain/stable"),
            inputs: [("toolchain".to_string(), "stable".to_string())].into(),
            env: Vec::new(),
            condition: Some("runner.os == 'Linux'".into()),
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - shell: bash
      if: runner.os != 'Windows'
      run: echo "${{ inputs.toolchain }}"
"#,
        )
        .unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host.join("_actions/dtolnay_rust-toolchain/stable/action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let ordered = ordered_executable_steps(&job, &[], &[resolved], &[], actions_host).unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Script(step) = &ordered[0] else {
            panic!("repository composite should expand to script step")
        };
        assert_eq!(step.id, "rust-1");
        assert_eq!(
            step.condition.as_deref(),
            Some("${{ (runner.os == 'Linux') && (runner.os != 'Windows') }}")
        );
        assert!(step.script.contains("echo \"stable\""));
    }

    #[test]
    fn ordered_steps_match_repository_action_source_path() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Tool",
            "requestId": 1,
            "steps": [{
                "id": "sub",
                "reference": {
                    "type": "Repository",
                    "name": "acme/action",
                    "ref": "v1",
                    "path": "sub/action"
                }
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let root_plan = RepositoryActionPlan {
            step_id: "root".into(),
            repository: "acme/action".into(),
            git_ref: "v1".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/acme_action/v1"),
            action_dir: actions_host.join("_actions/acme_action/v1"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let sub_plan = RepositoryActionPlan {
            step_id: "sub".into(),
            repository: "acme/action".into(),
            git_ref: "v1".into(),
            source_path: Some("sub/action".into()),
            repository_dir: actions_host.join("_actions/acme_action/v1"),
            action_dir: actions_host.join("_actions/acme_action/v1/sub/action"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let root_metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: root.js\n").unwrap();
        let sub_metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: sub.js\n").unwrap();
        let resolved = vec![
            ResolvedAction {
                plan: root_plan,
                metadata_path: actions_host.join("_actions/acme_action/v1/action.yml"),
                runtime: root_metadata.runtime().unwrap(),
                metadata: root_metadata,
            },
            ResolvedAction {
                plan: sub_plan,
                metadata_path: actions_host.join("_actions/acme_action/v1/sub/action/action.yml"),
                runtime: sub_metadata.runtime().unwrap(),
                metadata: sub_metadata,
            },
        ];

        let ordered = ordered_executable_steps(&job, &[], &resolved, &[], actions_host).unwrap();

        let ExecutableStep::JavaScript { invocation, .. } = &ordered[0] else {
            panic!("repository action should expand to JavaScript step")
        };
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/acme_action/v1/sub/action/sub.js"
        );
    }

    #[test]
    fn ordered_steps_materialize_repository_composite_outputs() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Pages",
            "requestId": 1,
            "steps": [{
                "id": "pages",
                "reference": {
                    "type": "Repository",
                    "name": "actions/upload-pages-artifact",
                    "ref": "v5"
                },
                "condition": "runner.os == 'Linux'"
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "pages".into(),
            repository: "actions/upload-pages-artifact".into(),
            git_ref: "v5".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_upload-pages-artifact/v5"),
            action_dir: actions_host.join("_actions/actions_upload-pages-artifact/v5"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: Some("runner.os == 'Linux'".into()),
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
outputs:
  artifact-id:
    value: ${{ steps.upload.outputs.artifact-id }}
runs:
  using: composite
  steps:
    - id: upload
      shell: bash
      run: echo artifact-id=123 >> "$GITHUB_OUTPUT"
"#,
        )
        .unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/actions_upload-pages-artifact/v5/action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let ordered = ordered_executable_steps(&job, &[], &[resolved], &[], actions_host).unwrap();

        assert_eq!(ordered.len(), 2);
        let ExecutableStep::Script(step) = &ordered[0] else {
            panic!("first composite step should be script")
        };
        assert_eq!(step.id, "pages-upload");
        let ExecutableStep::CompositeOutputs {
            step_id,
            outputs,
            condition,
        } = &ordered[1]
        else {
            panic!("second composite step should materialize outputs")
        };
        assert_eq!(step_id, "pages");
        assert_eq!(
            outputs["artifact-id"],
            "${{ steps.pages-upload.outputs.artifact-id }}"
        );
        assert_eq!(condition.as_deref(), Some("runner.os == 'Linux'"));
    }
}

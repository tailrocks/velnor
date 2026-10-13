use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::task::JoinHandle;

use crate::{
    action::{
        composite_action_invocations, composite_repository_action_plans,
        composite_repository_action_plans_from_resolved, download_repository_actions,
        is_local_action_step, local_action_plans_with_context, repository_action_plans,
        resolve_local_action, ActionMetadata, ActionRuntime, CompositeActionInvocation,
        LocalActionPlan, RepositoryActionPlan, ResolvedAction,
    },
    checkout::{
        checkout_plans, checkout_step_id, cleanup_checkout_credentials, configure_safe_directory,
        execute_checkouts, CheckoutPlan,
    },
    cli::{ConfigureArgs, RemoveArgs, RunArgs, StatusArgs},
    config::{self, CredentialScheme, RunnerSettings, StoredCredentials, StoredRunnerConfig},
    container::{split_container_options, JobContainerSpec, ServiceContainerSpec},
    executor::{DockerScriptExecutor, ExecutableStep, ProcessCommandRunner, StepLog},
    job_message::{ActionReferenceType, AgentJobRequestMessage},
    protocol::{
        AcquireJobOutcome, BrokerClient, DistributedTaskClient, GitHubAuthResult, GitHubScope,
        OAuthClient, OAuthJwtCredentials, RegistrationClient, RunServiceClient,
        RunServiceCompleteJob, RunServiceStepResult, RunServiceVariableValue, RunnerEvent,
        RunnerJobRequestRef, RunnerKeyPair, RunnerStatus, TaskAgent, TaskAgentPool,
        TaskAgentSession, TaskResult, TimelineRecordState, RUNNER_JOB_REQUEST,
    },
    runtime_env::job_runtime_env,
    script_step::github_script_steps_with_defaults,
};

const JOB_CANCELLATION_MESSAGE: &str = "JobCancellation";
const BROKER_MIGRATION_MESSAGE: &str = "BrokerMigration";

#[derive(Clone)]
struct RunServiceJobContext {
    client: RunServiceClient,
    run_service_url: String,
    billing_owner_id: Option<String>,
}

#[derive(Clone)]
struct BrokerCancellationContext {
    broker: BrokerClient,
    session_id: String,
    disable_update: bool,
}

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
            server_url_v2: registration
                .as_ref()
                .and_then(|registration| agent_string_property(&registration.agent, "ServerUrlV2")),
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
            use_v2_flow: registration.as_ref().is_some_and(|registration| {
                registration.auth.use_v2_flow
                    || agent_bool_property(&registration.agent, "UseV2Flow") == Some(true)
            }),
            ephemeral: false,
            disable_update: true,
        },
        credentials,
    };
    if registration.is_some()
        && (!stored.settings.use_v2_flow || stored.settings.server_url_v2.is_none())
    {
        bail!(
            "GitHub did not return required V2 runner settings (UseV2Flow/ServerUrlV2); Velnor targets the latest hosted GitHub broker/run-service protocol only"
        );
    }

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

    let dir = config::config_dir(args.config_dir.clone())?;
    let stored = config::load(&dir)?;
    let agent_id = stored
        .settings
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("runner is not registered: missing agent_id"))?;
    let token = oauth_access_token(&stored).await?;
    ensure_v2_runner_settings(&stored)?;
    run_v2(args, dir, stored, agent_id, token).await
}

fn ensure_v2_runner_settings(stored: &StoredRunnerConfig) -> Result<()> {
    if stored.settings.use_v2_flow && stored.settings.server_url_v2.is_some() {
        return Ok(());
    }
    bail!(
        "runner config is missing required V2 settings (UseV2Flow/ServerUrlV2); reconfigure against hosted GitHub with the latest runner registration flow"
    )
}

async fn run_v2(
    args: RunArgs,
    config_dir: PathBuf,
    stored: StoredRunnerConfig,
    agent_id: i64,
    token: String,
) -> Result<()> {
    let server_url_v2 = stored.settings.server_url_v2.as_deref().ok_or_else(|| {
        anyhow::anyhow!("runner config enables V2 flow but missing server_url_v2")
    })?;
    let broker_token = token.clone();
    let mut broker = BrokerClient::new(server_url_v2, broker_token.clone())?;
    let run_service = RunServiceClient::new(token.clone())?;
    let owner_name = format!("{} (PID: {})", default_agent_name(), std::process::id());
    let session = TaskAgentSession::new(owner_name, agent_id, stored.settings.agent_name.clone());
    let session = broker.create_session(&session).await?;
    let session_id = session
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("GitHub broker returned session without sessionId"))?;

    println!(
        "Runner '{}' ready via broker with labels: {}",
        stored.settings.agent_name,
        stored.settings.labels.join(",")
    );
    println!("Created broker runner session {session_id}.");

    loop {
        let message = broker
            .get_runner_message(
                session_id,
                RunnerStatus::Online,
                stored.settings.disable_update,
            )
            .await?;

        let Some(message) = message else {
            println!("No broker message received.");
            if args.once {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };

        if let Some(migration_url) = handle_v2_message(
            &broker,
            &run_service,
            session_id,
            &config_dir,
            &args,
            stored.settings.disable_update,
            message,
        )
        .await?
        {
            broker = BrokerClient::new(&migration_url, broker_token.clone())?;
            println!("Broker migration applied: {migration_url}");
        }
        if args.once {
            break;
        }
    }

    broker.delete_session().await?;
    println!("Deleted broker runner session.");

    Ok(())
}

async fn handle_v2_message(
    broker: &BrokerClient,
    run_service: &RunServiceClient,
    session_id: &str,
    config_dir: &std::path::Path,
    args: &RunArgs,
    disable_update: bool,
    message: crate::protocol::TaskAgentMessage,
) -> Result<Option<String>> {
    println!(
        "Received broker message {} type {}.",
        message.message_id, message.message_type
    );
    if message
        .message_type
        .eq_ignore_ascii_case(BROKER_MIGRATION_MESSAGE)
    {
        let migration_url = broker_migration_url(&message)?;
        println!("Received broker migration to {migration_url}.");
        return Ok(Some(migration_url));
    }
    if !message
        .message_type
        .eq_ignore_ascii_case(RUNNER_JOB_REQUEST)
    {
        println!("Broker message is not acknowledged because type is not implemented.");
        return Ok(None);
    }
    let reference: RunnerJobRequestRef =
        serde_json::from_str(&message.body).context("parse RunnerJobRequestRef")?;
    if reference.should_acknowledge {
        if let Err(error) = broker
            .acknowledge_runner_request(
                session_id,
                &reference.runner_request_id,
                RunnerStatus::Busy,
            )
            .await
        {
            eprintln!(
                "Best-effort broker acknowledge failed for request {}: {error:#}",
                reference.runner_request_id
            );
        }
    }
    let run_service_url = reference
        .run_service_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("V2 runner job request missing run_service_url"))?;
    let job_value = run_service
        .acquire_job(
            run_service_url,
            &reference.runner_request_id,
            std::env::consts::OS,
            reference.billing_owner_id.as_deref(),
        )
        .await?;
    let job_value = match job_value {
        AcquireJobOutcome::Acquired(value) => value,
        AcquireJobOutcome::Skipped {
            status,
            request_id,
            body,
        } => {
            println!(
                "Skipping run-service job request {} after non-retriable acquire response: status={}, request_id={}, body={}",
                reference.runner_request_id,
                status,
                request_id.unwrap_or_else(|| "unknown".to_string()),
                body
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(None);
        }
    };
    let job: AgentJobRequestMessage =
        serde_json::from_value(job_value).context("parse acquired run-service job")?;
    let job_run_service = job
        .system_connection()
        .and_then(system_connection_access_token)
        .map(RunServiceClient::new)
        .transpose()?
        .unwrap_or_else(|| run_service.clone());
    let run_service_job = RunServiceJobContext {
        client: job_run_service,
        run_service_url: run_service_url.to_string(),
        billing_owner_id: reference.billing_owner_id,
    };
    let broker_cancellation = BrokerCancellationContext {
        broker: broker.clone(),
        session_id: session_id.to_string(),
        disable_update,
    };
    handle_job_request(config_dir, args, run_service_job, broker_cancellation, job).await?;
    Ok(None)
}

async fn handle_job_request(
    config_dir: &std::path::Path,
    args: &RunArgs,
    run_service_job: RunServiceJobContext,
    broker_cancellation: BrokerCancellationContext,
    job: AgentJobRequestMessage,
) -> Result<()> {
    println!(
        "Parsed job request {} for job '{}' ({} step(s), {} endpoint(s)).",
        job.request_id,
        job.job_display_name,
        job.steps.len(),
        job.resources.endpoints.len()
    );
    if let Some(path) = &args.dump_job_message {
        let dump_path = write_sanitized_job_message_dump(&job, path)
            .context("write sanitized job message dump")?;
        println!(
            "Wrote sanitized job message dump to {}.",
            dump_path.display()
        );
    }
    let script_steps = match github_script_steps_with_defaults(&job.steps, "/__w", &job.defaults) {
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
    if should_execute_job(args) {
        let Some(script_steps) = script_steps else {
            bail!("cannot execute scripts because step mapping failed");
        };
        let renewal = start_run_service_lock_renewal(
            run_service_job.client.clone(),
            run_service_job.run_service_url.clone(),
            job.plan.plan_id.clone(),
            job.job_id.clone(),
        )
        .await?;
        let canceled = Arc::new(AtomicBool::new(false));
        let cancellation = start_broker_cancellation_poll(
            broker_cancellation.broker,
            broker_cancellation.session_id,
            broker_cancellation.disable_update,
            job.job_id.clone(),
            job_container_name(&job),
            canceled.clone(),
        );
        let config_dir = config_dir.to_path_buf();
        let work_dir = args.work_dir.clone();
        let docker_image = args.docker_image.clone();
        let node_action_image = args.node_action_image.clone();
        let job_to_execute = job.clone();
        let script_steps = script_steps.clone();
        let job_result = tokio::task::spawn_blocking(move || {
            execute_script_job(
                &config_dir,
                work_dir,
                &docker_image,
                &node_action_image,
                &job_to_execute,
                &script_steps,
            )
        })
        .await
        .context("join Docker job execution task")?;
        cancellation.abort();
        renewal.abort();
        let job_result = match job_result {
            Ok(mut job_result) => {
                if canceled.load(Ordering::SeqCst) {
                    job_result.result = TaskResult::Canceled;
                }
                job_result
            }
            Err(error) => {
                if canceled.load(Ordering::SeqCst) {
                    ScriptJobResult {
                        result: TaskResult::Canceled,
                        outputs: BTreeMap::new(),
                        step_logs: Vec::new(),
                    }
                } else {
                    complete_run_service_job(
                        &run_service_job.client,
                        &run_service_job.run_service_url,
                        &job,
                        TaskResult::Failed,
                        BTreeMap::new(),
                        Vec::new(),
                        run_service_job.billing_owner_id.clone(),
                    )
                    .await?;
                    return Err(error);
                }
            }
        };
        let outputs = job_result.outputs;
        let step_logs = job_result.step_logs;
        complete_run_service_job(
            &run_service_job.client,
            &run_service_job.run_service_url,
            &job,
            job_result.result,
            outputs,
            step_logs,
            run_service_job.billing_owner_id,
        )
        .await?;
        println!(
            "Job completed with result {:?} and message acknowledged.",
            job_result.result
        );
    } else if args.complete_noop {
        complete_run_service_job(
            &run_service_job.client,
            &run_service_job.run_service_url,
            &job,
            TaskResult::Succeeded,
            BTreeMap::new(),
            Vec::new(),
            run_service_job.billing_owner_id,
        )
        .await?;
        println!("No-op job completed and message acknowledged.");
    } else {
        println!(
            "Dry-run job inspection only; job was not acknowledged. Omit --dry-run-jobs to execute."
        );
    }
    Ok(())
}

fn should_execute_job(args: &RunArgs) -> bool {
    args.execute_scripts || (!args.complete_noop && !args.dry_run_jobs)
}

fn write_sanitized_job_message_dump(
    job: &AgentJobRequestMessage,
    destination: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let mut value = serde_json::to_value(job).context("serialize job message")?;
    sanitize_job_message_value(&mut value);
    let path = job_dump_path(job, destination);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create job dump directory {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&value).context("render sanitized job message")?;
    std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn job_dump_path(
    job: &AgentJobRequestMessage,
    destination: &std::path::Path,
) -> std::path::PathBuf {
    if destination.extension().is_some() {
        return destination.to_path_buf();
    }
    destination.join(format!(
        "job-{}-{}.json",
        job.request_id,
        sanitize_path_segment(&job.job_id)
    ))
}

fn sanitize_job_message_value(value: &mut Value) {
    sanitize_secret_variables(value);
    sanitize_mask_hints(value);
    sanitize_endpoint_authorization(value);
    sanitize_sensitive_keys(value);
}

fn sanitize_secret_variables(value: &mut Value) {
    let Some(variables) = object_field_mut(value, &["Variables", "variables"]) else {
        return;
    };
    let Some(variables) = variables.as_object_mut() else {
        return;
    };
    for variable in variables.values_mut() {
        let is_secret = object_field(variable, &["IsSecret", "isSecret"])
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_secret {
            if let Some(value) = object_field_mut(variable, &["Value", "value"]) {
                *value = Value::String("***".to_string());
            }
        }
    }
}

fn sanitize_mask_hints(value: &mut Value) {
    let Some(masks) = object_field_mut(value, &["Mask", "mask"]) else {
        return;
    };
    let Some(masks) = masks.as_array_mut() else {
        return;
    };
    for mask in masks {
        if let Some(value) = object_field_mut(mask, &["Value", "value"]) {
            *value = Value::String("***".to_string());
        }
    }
}

fn sanitize_endpoint_authorization(value: &mut Value) {
    let Some(resources) = object_field_mut(value, &["Resources", "resources"]) else {
        return;
    };
    let Some(endpoints) = object_field_mut(resources, &["Endpoints", "endpoints"]) else {
        return;
    };
    let Some(endpoints) = endpoints.as_array_mut() else {
        return;
    };
    for endpoint in endpoints {
        let Some(authorization) = object_field_mut(endpoint, &["Authorization", "authorization"])
        else {
            continue;
        };
        let Some(parameters) = object_field_mut(authorization, &["Parameters", "parameters"])
        else {
            continue;
        };
        let Some(parameters) = parameters.as_object_mut() else {
            continue;
        };
        for value in parameters.values_mut() {
            if value.is_string() {
                *value = Value::String("***".to_string());
            }
        }
    }
}

fn sanitize_sensitive_keys(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) && value.is_string() {
                    *value = Value::String("***".to_string());
                } else {
                    sanitize_sensitive_keys(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_sensitive_keys(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("password")
        || key.contains("secret")
        || key == "authorization"
}

fn object_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    names.iter().find_map(|name| object.get(*name))
}

fn object_field_mut<'a>(value: &'a mut Value, names: &[&str]) -> Option<&'a mut Value> {
    let object = value.as_object_mut()?;
    let key = names.iter().find(|name| object.contains_key(**name))?;
    object.get_mut(*key)
}

fn system_connection_access_token(
    endpoint: &crate::job_message::ServiceEndpoint,
) -> Option<String> {
    endpoint
        .authorization
        .as_ref()
        .and_then(|authorization| authorization.parameters.get("AccessToken"))
        .cloned()
}

async fn start_run_service_lock_renewal(
    client: RunServiceClient,
    run_service_url: String,
    plan_id: String,
    job_id: String,
) -> Result<JoinHandle<()>> {
    match client.renew_job(&run_service_url, &plan_id, &job_id).await {
        Ok(response) => {
            println!(
                "Run-service job {} valid until {}.",
                job_id, response.locked_until
            );
        }
        Err(error) => {
            eprintln!("Initial run-service job lock renewal failed: {error:#}");
        }
    }

    Ok(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            match client.renew_job(&run_service_url, &plan_id, &job_id).await {
                Ok(response) => {
                    println!(
                        "Renewed run-service job {}; valid until {}.",
                        job_id, response.locked_until
                    );
                }
                Err(error) => {
                    eprintln!("Run-service job lock renewal failed: {error:#}");
                }
            }
        }
    }))
}

fn start_broker_cancellation_poll(
    broker: BrokerClient,
    session_id: String,
    disable_update: bool,
    job_id: String,
    job_container_name: String,
    canceled: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let message = match broker
                .get_runner_message(&session_id, RunnerStatus::Busy, disable_update)
                .await
            {
                Ok(message) => message,
                Err(error) => {
                    eprintln!("Broker cancellation poll failed: {error:#}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            let Some(message) = message else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };
            if !is_job_cancellation_for(&message, &job_id) {
                println!(
                    "Busy broker runner received unsupported message {} type {}; ignoring while job runs.",
                    message.message_id, message.message_type
                );
                continue;
            }
            canceled.store(true, Ordering::SeqCst);
            kill_job_container(&job_container_name);
            break;
        }
    })
}

#[derive(Debug, Deserialize)]
struct JobCancelMessage {
    #[serde(default, rename = "JobId", alias = "jobId")]
    job_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BrokerMigrationMessage {
    #[serde(rename = "BrokerBaseUrl", alias = "brokerBaseUrl")]
    broker_base_url: String,
}

fn broker_migration_url(message: &crate::protocol::TaskAgentMessage) -> Result<String> {
    let migration: BrokerMigrationMessage =
        serde_json::from_str(&message.body).context("parse BrokerMigration message")?;
    if migration.broker_base_url.trim().is_empty() {
        bail!("BrokerMigration message missing BrokerBaseUrl");
    }
    Ok(migration.broker_base_url)
}

fn is_job_cancellation_for(message: &crate::protocol::TaskAgentMessage, job_id: &str) -> bool {
    if !message
        .message_type
        .eq_ignore_ascii_case(JOB_CANCELLATION_MESSAGE)
    {
        return false;
    }
    match serde_json::from_str::<JobCancelMessage>(&message.body) {
        Ok(cancel) => match cancel.job_id.as_deref() {
            Some(value) => value == job_id,
            None => true,
        },
        Err(error) => {
            eprintln!(
                "Treating malformed cancellation message {} as job cancellation: {error:#}",
                message.message_id
            );
            true
        }
    }
}

fn kill_job_container(container_name: &str) {
    match Command::new("docker")
        .args(["kill", container_name])
        .output()
    {
        Ok(output) if output.status.success() => {
            println!("Killed Docker job container {container_name} after GitHub cancellation.");
        }
        Ok(output) => {
            eprintln!(
                "Failed to kill Docker job container {container_name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(error) => {
            eprintln!("Failed to run docker kill for {container_name}: {error:#}");
        }
    }
}

fn execute_script_job(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    docker_image: &str,
    node_action_image: &str,
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
) -> Result<ScriptJobResult> {
    let job_dir = job_work_dir(config_dir, work_dir, job);
    let workspace = job_dir.join("workspace");
    let temp = job_dir.join("temp");
    let home = job_dir.join("home");
    let actions = job_dir.join("actions");
    let tools = job_dir.join("tools");
    for path in [&workspace, &temp, &home, &actions, &tools] {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    let context_data = job_context_data(job);
    let mut command_runner = ProcessCommandRunner;
    let checkout_plans = checkout_plans(job, &workspace)?;
    let (runtime_checkout_plans, eager_checkout_plans): (Vec<_>, Vec<_>) = checkout_plans
        .into_iter()
        .partition(CheckoutPlan::requires_runtime_context);
    let base_env = job_runtime_env(job);
    let eager_checkout_plans = eager_checkout_plans
        .into_iter()
        .map(|plan| resolve_checkout_plan_context(plan, &base_env, &context_data))
        .collect::<Vec<_>>();
    execute_checkouts(&mut command_runner, &eager_checkout_plans)?;
    for plan in &eager_checkout_plans {
        configure_safe_directory(&home, &workspace, &plan.destination)?;
    }
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
        &runtime_checkout_plans,
    )?;

    let container = JobContainerSpec {
        name: job_container_name(job),
        image: job_container_image(job).unwrap_or(docker_image).to_string(),
        network: format!("velnor-net-{}", sanitize_path_segment(&job.job_id)),
        workspace_host: workspace,
        temp_host: temp.clone(),
        home_host: home,
        actions_host: actions,
        tools_host: tools,
        mount_docker_socket: true,
        env: job_container_env(job),
        options: job_container_options(job),
        services: service_containers(job),
        node_action_image: node_action_image.to_string(),
        docker_cli_host_path: host_docker_cli_path(),
        docker_cli_plugin_host_dir: host_docker_cli_plugin_dir(),
        verify_bind_mounts: true,
    };
    let cleanup_checkout_plans = eager_checkout_plans
        .iter()
        .chain(runtime_checkout_plans.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mut executor = DockerScriptExecutor::new(command_runner);
    let summary_result = executor.execute_ordered_steps_with_job_outputs(
        &container,
        &ordered_steps,
        &base_env,
        &context_data,
        job.job_outputs.as_ref(),
        &temp,
    );
    let mut command_runner = executor.into_runner();
    let cleanup_result = cleanup_checkout_credentials(&mut command_runner, &cleanup_checkout_plans);
    let summary = match (summary_result, cleanup_result) {
        (Ok(summary), Ok(())) => summary,
        (Ok(_), Err(error)) => return Err(error.context("cleanup checkout credentials")),
        (Err(error), Ok(())) => return Err(error),
        (Err(error), Err(cleanup_error)) => {
            eprintln!("Checkout credential cleanup failed after job error: {cleanup_error:#}");
            return Err(error);
        }
    };
    if !summary.job_outputs.is_empty() {
        println!("Evaluated {} job output(s).", summary.job_outputs.len());
    }
    let failed = summary
        .step_results
        .iter()
        .any(|result| result.exit_code != 0 && !result.failure_ignored);
    for log in summary.step_logs.iter().filter(|log| log.exit_code != 0) {
        println!(
            "Step '{}' failed with exit code {}.",
            log.step_id, log.exit_code
        );
        for line in log.lines.iter().take(20) {
            println!("  {line}");
        }
    }

    let result = if failed {
        TaskResult::Failed
    } else {
        TaskResult::Succeeded
    };
    Ok(ScriptJobResult {
        result,
        outputs: summary.job_outputs,
        step_logs: summary.step_logs,
    })
}

fn resolve_checkout_plan_context(
    mut plan: CheckoutPlan,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> CheckoutPlan {
    if let Some(version) = plan.version.as_mut() {
        *version =
            crate::executor::render_expressions_with_context(version, base_env, context_data);
    }
    plan
}

#[derive(Debug, Clone)]
struct ScriptJobResult {
    result: TaskResult,
    outputs: BTreeMap<String, String>,
    step_logs: Vec<StepLog>,
}

fn job_context_data(job: &AgentJobRequestMessage) -> Vec<(String, Value)> {
    let mut context_data = job.context_data.clone();
    let github_token = job
        .variables
        .get("system.github.token")
        .and_then(|variable| variable.value.clone());
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
    if let Some(token) = github_token {
        match context_data.get_mut("github") {
            Some(Value::Object(github)) => {
                github
                    .entry("token".to_string())
                    .or_insert_with(|| Value::String(token));
            }
            Some(_) => {}
            None => {
                let mut github = Map::new();
                github.insert("token".to_string(), Value::String(token));
                context_data.insert("github".to_string(), Value::Object(github));
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

fn host_docker_cli_path() -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    find_executable_on_path("docker")
}

fn host_docker_cli_plugin_dir() -> Option<std::path::PathBuf> {
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
    .map(std::path::PathBuf::from)
    .find(|path| path.is_file())
    .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
}

fn find_executable_on_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
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

fn job_container_name(job: &AgentJobRequestMessage) -> String {
    format!("velnor-job-{}", sanitize_path_segment(&job.job_id))
}

fn ordered_executable_steps(
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
    resolved_actions: &[ResolvedAction],
    local_actions: &[(LocalActionPlan, ActionMetadata)],
    actions_host: &std::path::Path,
    runtime_checkout_plans: &[CheckoutPlan],
) -> Result<Vec<ExecutableStep>> {
    let mut ordered = Vec::new();
    let mut script_iter = script_steps.iter();
    let mut local_iter = local_actions.iter();
    for (step_index, step) in job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.enabled)
    {
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
                    let parent_condition = step.condition.as_deref();
                    let parent_continue_on_error = crate::script_step::step_continue_on_error(step);
                    ordered.push(ExecutableStep::CompositeStart {
                        step_id: plan.step_id.clone(),
                    });
                    for invocation in
                        composite_action_invocations(plan, metadata, "/__w", actions_host)?
                    {
                        match invocation {
                            CompositeActionInvocation::Script(mut script) => {
                                script.condition = combine_conditions(
                                    parent_condition,
                                    script.condition.as_deref(),
                                );
                                script.continue_on_error |= parent_continue_on_error;
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
                                    parent_condition,
                                    parent_continue_on_error,
                                )?;
                            }
                            CompositeActionInvocation::Outputs(outputs) => {
                                ordered.push(ExecutableStep::CompositeOutputs {
                                    step_id: outputs.step_id,
                                    outputs: outputs.outputs,
                                    condition: parent_condition.map(ToOwned::to_owned),
                                });
                            }
                        }
                    }
                    ordered.push(ExecutableStep::CompositeEnd {
                        step_id: plan.step_id.clone(),
                    });
                    continue;
                }
                let Some(reference) = step.reference.as_ref() else {
                    continue;
                };
                let Some(repository) = reference.name.as_deref() else {
                    continue;
                };
                if repository.eq_ignore_ascii_case("actions/checkout") {
                    if let Some(plan) = runtime_checkout_plans
                        .iter()
                        .find(|plan| plan.step_id == checkout_step_id(step, step_index))
                    {
                        ordered.push(ExecutableStep::Checkout(plan.clone()));
                    }
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
                    false,
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
    parent_continue_on_error: bool,
) -> Result<()> {
    let continue_on_error = parent_continue_on_error || action.plan.continue_on_error;
    match &action.runtime {
        ActionRuntime::JavaScript { .. } => ordered.push(ExecutableStep::JavaScript {
            step_id: action.plan.step_id.clone(),
            invocation: action.javascript_invocation(actions_host)?,
            condition: combine_conditions(parent_condition, action.plan.condition.as_deref()),
            continue_on_error,
        }),
        ActionRuntime::Docker { .. } => ordered.push(ExecutableStep::Docker {
            step_id: action.plan.step_id.clone(),
            invocation: action.docker_invocation(actions_host)?,
            condition: combine_conditions(parent_condition, action.plan.condition.as_deref()),
            continue_on_error,
        }),
        ActionRuntime::Composite => {
            let action_condition =
                combine_conditions(parent_condition, action.plan.condition.as_deref());
            ordered.push(ExecutableStep::CompositeStart {
                step_id: action.plan.step_id.clone(),
            });
            for invocation in action.composite_invocations("/__w", actions_host)? {
                match invocation {
                    CompositeActionInvocation::Script(mut script) => {
                        script.condition = combine_conditions(
                            action_condition.as_deref(),
                            script.condition.as_deref(),
                        );
                        script.continue_on_error |= continue_on_error;
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
                            continue_on_error,
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
            ordered.push(ExecutableStep::CompositeEnd {
                step_id: action.plan.step_id.clone(),
            });
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
    job.job_container
        .as_ref()
        .and_then(container_options)
        .unwrap_or_default()
}

fn service_containers(job: &AgentJobRequestMessage) -> Vec<ServiceContainerSpec> {
    let network = format!("velnor-net-{}", sanitize_path_segment(&job.job_id));
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

fn service_env(container: &crate::job_message::ContainerResource) -> Vec<(String, String)> {
    container
        .environment_variables
        .as_ref()
        .map(container_env_value)
        .unwrap_or_default()
}

fn service_ports(container: &crate::job_message::ContainerResource) -> Vec<String> {
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

async fn complete_run_service_job(
    client: &RunServiceClient,
    run_service_url: &str,
    job: &AgentJobRequestMessage,
    result: TaskResult,
    job_outputs: BTreeMap<String, String>,
    step_logs: Vec<StepLog>,
    billing_owner_id: Option<String>,
) -> Result<()> {
    let step_results = step_logs
        .iter()
        .map(|log| RunServiceStepResult {
            external_id: None,
            name: log.step_id.clone(),
            status: TimelineRecordState::Completed,
            conclusion: step_log_result(log),
            completed_log_lines: log.lines.len() as i64,
        })
        .collect();
    let outputs = job_outputs
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                RunServiceVariableValue {
                    value,
                    is_secret: false,
                },
            )
        })
        .collect();
    let completion = RunServiceCompleteJob {
        plan_id: job.plan.plan_id.clone(),
        job_id: job.job_id.clone(),
        conclusion: result,
        outputs,
        step_results,
        billing_owner_id,
        infrastructure_failure_category: None,
    };
    client
        .complete_job(run_service_url, completion)
        .await
        .context("complete run-service job")
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

fn step_log_result(step_log: &StepLog) -> TaskResult {
    if step_log.skipped {
        TaskResult::Skipped
    } else if step_log.exit_code == 0 || step_log.failure_ignored {
        TaskResult::Succeeded
    } else {
        TaskResult::Failed
    }
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
        "Server URL V2: {}",
        stored.settings.server_url_v2.as_deref().unwrap_or("none")
    );
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

fn agent_string_property(agent: &TaskAgent, name: &str) -> Option<String> {
    agent
        .properties
        .as_ref()
        .and_then(|properties| properties.get(name))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn agent_bool_property(agent: &TaskAgent, name: &str) -> Option<bool> {
    agent
        .properties
        .as_ref()
        .and_then(|properties| properties.get(name))
        .and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{
        parse_action_metadata, resolve_action, ActionRuntime, LocalActionPlan, RepositoryActionPlan,
    };
    use crate::protocol::TaskAgentMessage;
    use std::path::Path;

    fn run_args(complete_noop: bool, execute_scripts: bool, dry_run_jobs: bool) -> RunArgs {
        RunArgs {
            config_dir: None,
            once: false,
            complete_noop,
            execute_scripts,
            dry_run_jobs,
            dump_job_message: None,
            docker_image: "ubuntu:24.04".into(),
            node_action_image: String::new(),
            work_dir: None,
        }
    }

    #[test]
    fn run_executes_jobs_by_default() {
        assert!(should_execute_job(&run_args(false, false, false)));
        assert!(should_execute_job(&run_args(false, true, false)));
        assert!(!should_execute_job(&run_args(true, false, false)));
        assert!(!should_execute_job(&run_args(false, false, true)));
    }

    #[test]
    fn normal_run_requires_v2_settings() {
        let mut stored = stored_config();
        stored.settings.use_v2_flow = false;
        stored.settings.server_url_v2 = None;
        assert!(ensure_v2_runner_settings(&stored).is_err());

        stored.settings.use_v2_flow = true;
        stored.settings.server_url_v2 = None;
        assert!(ensure_v2_runner_settings(&stored).is_err());

        stored.settings.server_url_v2 =
            Some("https://broker.actions.githubusercontent.com/".into());
        assert!(ensure_v2_runner_settings(&stored).is_ok());
    }

    #[test]
    fn sanitized_job_message_dump_redacts_runtime_secrets() {
        let mut value = serde_json::json!({
            "variables": {
                "system.github.token": { "value": "ghs_secret", "isSecret": true },
                "github.repository": { "value": "ChainArgos/java-monorepo", "isSecret": false }
            },
            "mask": [
                { "type": "Regex", "value": "secret-regex" }
            ],
            "resources": {
                "endpoints": [
                    {
                        "name": "SystemVssConnection",
                        "authorization": {
                            "parameters": {
                                "AccessToken": "job-token",
                                "Other": "also-sensitive"
                            }
                        }
                    }
                ]
            },
            "steps": [
                {
                    "inputs": {
                        "token": "step-token",
                        "repository": "owner/repo"
                    }
                }
            ]
        });

        sanitize_job_message_value(&mut value);

        assert_eq!(
            value["variables"]["system.github.token"]["value"],
            serde_json::json!("***")
        );
        assert_eq!(
            value["variables"]["github.repository"]["value"],
            serde_json::json!("ChainArgos/java-monorepo")
        );
        assert_eq!(value["mask"][0]["value"], serde_json::json!("***"));
        assert_eq!(
            value["resources"]["endpoints"][0]["authorization"]["parameters"]["AccessToken"],
            serde_json::json!("***")
        );
        assert_eq!(
            value["resources"]["endpoints"][0]["authorization"]["parameters"]["Other"],
            serde_json::json!("***")
        );
        assert_eq!(
            value["steps"][0]["inputs"]["token"],
            serde_json::json!("***")
        );
        assert_eq!(
            value["steps"][0]["inputs"]["repository"],
            serde_json::json!("owner/repo")
        );
    }

    fn stored_config() -> StoredRunnerConfig {
        StoredRunnerConfig {
            settings: RunnerSettings {
                github_url: "https://github.com/owner/repo".into(),
                server_url: Some("https://pipelines.actions.githubusercontent.com/".into()),
                server_url_v2: Some("https://broker.actions.githubusercontent.com/".into()),
                pool_id: Some(1),
                pool_name: Some("default".into()),
                agent_id: Some(2),
                agent_name: "velnor".into(),
                labels: vec!["self-hosted".into(), "velnor".into()],
                use_v2_flow: true,
                ephemeral: false,
                disable_update: true,
            },
            credentials: None,
        }
    }

    #[test]
    fn recognizes_matching_job_cancellation_message() {
        let message = TaskAgentMessage {
            message_id: 7,
            message_type: "JobCancellation".into(),
            body: serde_json::json!({ "jobId": "job-123", "timeout": "00:05:00" }).to_string(),
            iv_base64: None,
        };

        assert!(is_job_cancellation_for(&message, "job-123"));
        assert!(!is_job_cancellation_for(&message, "job-456"));
    }

    #[test]
    fn ignores_non_cancellation_message_type() {
        let message = TaskAgentMessage {
            message_id: 8,
            message_type: "PipelineAgentJobRequest".into(),
            body: "{}".into(),
            iv_base64: None,
        };

        assert!(!is_job_cancellation_for(&message, "job-123"));
    }

    #[test]
    fn parses_broker_migration_message_url() {
        let message = TaskAgentMessage {
            message_id: 9,
            message_type: "BrokerMigration".into(),
            body: serde_json::json!({
                "BrokerBaseUrl": "https://broker.actions.githubusercontent.com/new/"
            })
            .to_string(),
            iv_base64: None,
        };

        assert_eq!(
            broker_migration_url(&message).unwrap(),
            "https://broker.actions.githubusercontent.com/new/"
        );
    }

    #[test]
    fn reads_v2_settings_from_agent_properties() {
        let mut agent = TaskAgent::new("velnor", vec![], None, false);
        agent.properties = Some(serde_json::json!({
            "ServerUrlV2": "https://broker.actions.githubusercontent.com/tenant/",
            "UseV2Flow": true
        }));

        assert_eq!(
            agent_string_property(&agent, "ServerUrlV2").as_deref(),
            Some("https://broker.actions.githubusercontent.com/tenant/")
        );
        assert_eq!(agent_bool_property(&agent, "UseV2Flow"), Some(true));
    }

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
        assert_eq!(context_data["github"]["token"], "ghs_token");
        assert_eq!(context_data["matrix"]["target"], "linux");
    }

    #[test]
    fn local_action_inputs_can_render_github_token_context() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Docs",
            "requestId": 1,
            "variables": {
                "system.github.token": { "value": "ghs_token", "isSecret": true }
            },
            "steps": [{
                "id": "docs",
                "reference": {
                    "type": "Repository",
                    "name": "./.github/actions/check-deployed-docs"
                },
                "inputs": {
                    "github-token": "${{ github.token }}"
                }
            }]
        }))
        .unwrap();
        let context_data = job_context_data(&job);

        let plans =
            local_action_plans_with_context(&job.steps, Path::new("/tmp/workspace"), &context_data)
                .unwrap();

        assert_eq!(plans[0].inputs["github-token"], "ghs_token");
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
                    "condition": "always()",
                    "continueOnError": true,
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
      if: github.event_name != 'schedule'
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
            &[],
        )
        .unwrap();

        assert_eq!(ordered.len(), 4);
        assert!(matches!(ordered[0], ExecutableStep::Script(_)));
        assert!(matches!(
            &ordered[1],
            ExecutableStep::CompositeStart { step_id } if step_id == "aggregate"
        ));
        let ExecutableStep::Script(step) = &ordered[2] else {
            panic!("local composite should expand to script step")
        };
        assert_eq!(step.id, "aggregate-1");
        assert!(step.script.contains("echo \"CI\""));
        assert_eq!(
            step.condition.as_deref(),
            Some("${{ (always()) && (github.event_name != 'schedule') }}")
        );
        assert!(step.continue_on_error);
        assert!(matches!(
            &ordered[3],
            ExecutableStep::CompositeEnd { step_id } if step_id == "aggregate"
        ));
    }

    #[test]
    fn ordered_steps_keep_runtime_checkout_after_producer_step() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Preview",
            "requestId": 1,
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "jackin-project/jackin",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/jackin-project/jackin.git" }
                }]
            },
            "steps": [
                {
                    "id": "source",
                    "reference": { "type": "Script" },
                    "inputs": { "script": "echo sha=def456 >> \"$GITHUB_OUTPUT\"" }
                },
                {
                    "reference": { "type": "Repository", "name": "actions/checkout" },
                    "inputs": {
                        "ref": "${{ steps.source.outputs.sha }}",
                        "fetch-depth": "0"
                    }
                }
            ]
        }))
        .unwrap();
        let script_steps = crate::script_step::github_script_steps(&job.steps, "/__w").unwrap();
        let runtime_checkout_plans = checkout_plans(&job, Path::new("/tmp/work"))
            .unwrap()
            .into_iter()
            .filter(CheckoutPlan::requires_runtime_context)
            .collect::<Vec<_>>();

        let ordered = ordered_executable_steps(
            &job,
            &script_steps,
            &[],
            &[],
            Path::new("/tmp/actions"),
            &runtime_checkout_plans,
        )
        .unwrap();

        assert_eq!(ordered.len(), 2);
        assert!(matches!(ordered[0], ExecutableStep::Script(_)));
        let ExecutableStep::Checkout(plan) = &ordered[1] else {
            panic!("runtime checkout should remain ordered after its producer step")
        };
        assert_eq!(plan.step_id, "checkout2");
        assert_eq!(
            plan.version.as_deref(),
            Some("${{ steps.source.outputs.sha }}")
        );
    }

    #[test]
    fn eager_checkout_resolves_ref_from_job_context() {
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            clone_url: "https://github.com/jackin-project/jackin.git".into(),
            version: Some("${{ needs.source-changed.outputs.sha }}".into()),
            destination: Path::new("/tmp/work").to_path_buf(),
            token: None,
            fetch_depth: None,
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            condition: None,
            continue_on_error: false,
        };
        let context_data = vec![(
            "needs".to_string(),
            serde_json::json!({
                "source-changed": {
                    "outputs": {
                        "sha": "def456"
                    }
                }
            }),
        )];

        let resolved = resolve_checkout_plan_context(plan, &[], &context_data);

        assert_eq!(resolved.version.as_deref(), Some("def456"));
        assert!(!resolved.requires_runtime_context());
    }

    #[test]
    fn eager_checkout_resolves_ref_from_github_env_context() {
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            clone_url: "https://github.com/jackin-project/jackin.git".into(),
            version: Some("${{ github.sha }}".into()),
            destination: Path::new("/tmp/work").to_path_buf(),
            token: None,
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            condition: None,
            continue_on_error: false,
        };
        let base_env = vec![("GITHUB_SHA".to_string(), "abc123".to_string())];

        let resolved = resolve_checkout_plan_context(plan, &base_env, &[]);

        assert_eq!(resolved.version.as_deref(), Some("abc123"));
        assert!(!resolved.requires_runtime_context());
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
                "condition": "github.event_name == 'push'",
                "continueOnError": true,
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
            &[],
        )
        .unwrap();

        assert_eq!(ordered.len(), 3);
        assert!(matches!(
            &ordered[0],
            ExecutableStep::CompositeStart { step_id } if step_id == "docs"
        ));
        let ExecutableStep::JavaScript {
            step_id,
            invocation,
            condition,
            continue_on_error,
            ..
        } = &ordered[1]
        else {
            panic!("nested repository action should expand to JavaScript step")
        };
        assert_eq!(step_id, "docs-1");
        assert!(invocation
            .env
            .contains(&("INPUT_GITHUB_TOKEN".into(), "ghs_token".into())));
        assert_eq!(condition.as_deref(), Some("github.event_name == 'push'"));
        assert!(*continue_on_error);
        assert!(matches!(
            &ordered[2],
            ExecutableStep::CompositeEnd { step_id } if step_id == "docs"
        ));
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

        let ordered =
            ordered_executable_steps(&job, &[], &[resolved], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 3);
        assert!(matches!(
            &ordered[0],
            ExecutableStep::CompositeStart { step_id } if step_id == "rust"
        ));
        let ExecutableStep::Script(step) = &ordered[1] else {
            panic!("repository composite should expand to script step")
        };
        assert_eq!(step.id, "rust-1");
        assert_eq!(
            step.condition.as_deref(),
            Some("${{ (runner.os == 'Linux') && (runner.os != 'Windows') }}")
        );
        assert!(step.script.contains("echo \"stable\""));
        assert!(matches!(
            &ordered[2],
            ExecutableStep::CompositeEnd { step_id } if step_id == "rust"
        ));
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

        let ordered =
            ordered_executable_steps(&job, &[], &resolved, &[], actions_host, &[]).unwrap();

        let ExecutableStep::JavaScript { invocation, .. } = &ordered[0] else {
            panic!("repository action should expand to JavaScript step")
        };
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/acme_action/v1/sub/action/sub.js"
        );
    }

    #[test]
    fn target_workflow_repository_actions_plan_from_cached_metadata() {
        let actions_host = Path::new("/tmp/velnor-actions");
        let workflow_roots = [
            Path::new("/tmp/velnor-targets/jackin/.github/workflows"),
            Path::new("/tmp/velnor-targets/java-monorepo/.github/workflows"),
        ];
        if !actions_host.exists() || workflow_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let mut references = BTreeMap::new();
        for root in workflow_roots.into_iter().filter(|root| root.exists()) {
            for path in workflow_files(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents)
                    .unwrap_or_else(|error| panic!("parse {}: {error:#}", path.display()));
                collect_repository_uses(&yaml, &mut references);
            }
        }

        let steps = references
            .into_iter()
            .enumerate()
            .map(|(index, (_, reference))| {
                serde_json::json!({
                    "id": format!("target-action-{index}"),
                    "reference": {
                        "type": "Repository",
                        "name": reference.repository,
                        "ref": reference.git_ref,
                        "path": reference.source_path
                    },
                    "inputs": {}
                })
            })
            .collect::<Vec<_>>();
        assert!(steps.len() >= 20, "expected target repository actions");
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "target-actions",
            "jobDisplayName": "Target Actions",
            "requestId": 1,
            "steps": steps
        }))
        .unwrap();
        let plans = repository_action_plans(&job.steps, actions_host).unwrap();
        let resolved = resolve_actions_from_cache(&plans, actions_host);

        let ordered = ordered_executable_steps(&job, &[], &resolved, &[], actions_host, &[])
            .unwrap_or_else(|error| panic!("plan target action inventory: {error:#}"));

        assert!(
            ordered.len() >= plans.len(),
            "expected target action inventory to produce executable steps"
        );
    }

    #[test]
    fn ordered_steps_expand_repository_docker_action() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Renovate",
            "requestId": 1,
            "steps": [{
                "id": "renovate",
                "reference": {
                    "type": "Repository",
                    "name": "renovatebot/github-action",
                    "ref": "v46.1.14"
                },
                "inputs": { "renovate-image": "ghcr.io/renovatebot/renovate" }
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let plan = RepositoryActionPlan {
            step_id: "renovate".into(),
            repository: "renovatebot/github-action".into(),
            git_ref: "v46.1.14".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            action_dir: actions_host.join("_actions/renovatebot_github-action/v46.1.14"),
            inputs: [(
                "renovate-image".to_string(),
                "ghcr.io/renovatebot/renovate".to_string(),
            )]
            .into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let metadata = parse_action_metadata(
            r#"
runs:
  using: docker
  image: docker://alpine:3.20
  args:
    - ${{ inputs.renovate-image }}
"#,
        )
        .unwrap();
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/renovatebot_github-action/v46.1.14/action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let ordered =
            ordered_executable_steps(&job, &[], &[resolved], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Docker {
            step_id,
            invocation,
            ..
        } = &ordered[0]
        else {
            panic!("repository Docker action should expand to Docker step")
        };
        assert_eq!(step_id, "renovate");
        assert_eq!(invocation.image, "alpine:3.20");
        assert_eq!(invocation.args, vec!["ghcr.io/renovatebot/renovate"]);
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

        let ordered =
            ordered_executable_steps(&job, &[], &[resolved], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 4);
        assert!(matches!(
            &ordered[0],
            ExecutableStep::CompositeStart { step_id } if step_id == "pages"
        ));
        let ExecutableStep::Script(step) = &ordered[1] else {
            panic!("first composite step should be script")
        };
        assert_eq!(step.id, "pages-upload");
        let ExecutableStep::CompositeOutputs {
            step_id,
            outputs,
            condition,
        } = &ordered[2]
        else {
            panic!("second composite step should materialize outputs")
        };
        assert_eq!(step_id, "pages");
        assert_eq!(
            outputs["artifact-id"],
            "${{ steps.pages-upload.outputs.artifact-id }}"
        );
        assert_eq!(condition.as_deref(), Some("runner.os == 'Linux'"));
        assert!(matches!(
            &ordered[3],
            ExecutableStep::CompositeEnd { step_id } if step_id == "pages"
        ));
    }

    #[test]
    fn repository_composite_continue_on_error_reaches_nested_actions() {
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
                "continueOnError": true
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let pages_plan = RepositoryActionPlan {
            step_id: "pages".into(),
            repository: "actions/upload-pages-artifact".into(),
            git_ref: "v5".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_upload-pages-artifact/v5"),
            action_dir: actions_host.join("_actions/actions_upload-pages-artifact/v5"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: true,
        };
        let pages_metadata = parse_action_metadata(
            r#"
runs:
  using: composite
  steps:
    - id: upload
      uses: actions/upload-artifact@v7
"#,
        )
        .unwrap();
        let upload_plan = RepositoryActionPlan {
            step_id: "pages-upload".into(),
            repository: "actions/upload-artifact".into(),
            git_ref: "v7".into(),
            source_path: None,
            repository_dir: actions_host.join("_actions/actions_upload-artifact/v7"),
            action_dir: actions_host.join("_actions/actions_upload-artifact/v7"),
            inputs: BTreeMap::new(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let upload_metadata =
            parse_action_metadata("runs:\n  using: node20\n  main: dist/upload/index.js\n")
                .unwrap();
        let pages = ResolvedAction {
            plan: pages_plan,
            metadata_path: actions_host
                .join("_actions/actions_upload-pages-artifact/v5/action.yml"),
            runtime: pages_metadata.runtime().unwrap(),
            metadata: pages_metadata,
        };
        let upload = ResolvedAction {
            plan: upload_plan,
            metadata_path: actions_host.join("_actions/actions_upload-artifact/v7/action.yml"),
            runtime: upload_metadata.runtime().unwrap(),
            metadata: upload_metadata,
        };

        let ordered =
            ordered_executable_steps(&job, &[], &[pages, upload], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 3);
        assert!(matches!(
            &ordered[0],
            ExecutableStep::CompositeStart { step_id } if step_id == "pages"
        ));
        let ExecutableStep::JavaScript {
            step_id,
            continue_on_error,
            ..
        } = &ordered[1]
        else {
            panic!("nested upload action should expand to JavaScript step")
        };
        assert_eq!(step_id, "pages-upload");
        assert!(*continue_on_error);
        assert!(matches!(
            &ordered[2],
            ExecutableStep::CompositeEnd { step_id } if step_id == "pages"
        ));
    }

    #[derive(Clone)]
    struct TargetActionReference {
        repository: String,
        source_path: Option<String>,
        git_ref: String,
    }

    fn collect_repository_uses(
        value: &serde_yaml::Value,
        references: &mut BTreeMap<String, TargetActionReference>,
    ) {
        match value {
            serde_yaml::Value::Mapping(mapping) => {
                for (key, value) in mapping {
                    if key.as_str() == Some("uses") {
                        if let Some(reference) = target_repository_uses(value) {
                            references.insert(
                                format!(
                                    "{}@{}:{}",
                                    reference.repository,
                                    reference.git_ref,
                                    reference.source_path.as_deref().unwrap_or("")
                                ),
                                reference,
                            );
                        }
                    }
                    collect_repository_uses(value, references);
                }
            }
            serde_yaml::Value::Sequence(values) => {
                for value in values {
                    collect_repository_uses(value, references);
                }
            }
            _ => {}
        }
    }

    fn target_repository_uses(value: &serde_yaml::Value) -> Option<TargetActionReference> {
        let uses = value.as_str()?.trim();
        if uses.starts_with('.') || uses.starts_with("docker://") {
            return None;
        }
        let (path, git_ref) = uses.rsplit_once('@')?;
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.len() < 2 {
            return None;
        }
        let repository = format!("{}/{}", parts[0], parts[1]);
        if repository.eq_ignore_ascii_case("actions/checkout") {
            return None;
        }
        let source_path = (parts.len() > 2).then(|| parts[2..].join("/"));
        Some(TargetActionReference {
            repository,
            source_path,
            git_ref: git_ref.to_string(),
        })
    }

    fn workflow_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_workflow_files(root, &mut files);
        files.sort();
        files
    }

    fn collect_workflow_files(root: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_workflow_files(&path, files);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            ) {
                files.push(path);
            }
        }
    }

    fn resolve_actions_from_cache(
        initial_plans: &[RepositoryActionPlan],
        actions_host: &Path,
    ) -> Vec<ResolvedAction> {
        let mut resolved = Vec::new();
        let mut pending = initial_plans.to_vec();
        while !pending.is_empty() {
            for plan in &pending {
                let action = resolve_action(plan)
                    .unwrap_or_else(|error| panic!("resolve cached action {plan:?}: {error:#}"));
                if !resolved
                    .iter()
                    .any(|existing: &ResolvedAction| same_action(&existing.plan, &action.plan))
                {
                    resolved.push(action);
                }
            }
            let previous_pending = pending;
            pending = composite_repository_action_plans_from_resolved(&resolved, actions_host)
                .unwrap()
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
        resolved
    }
}

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{fs, path::PathBuf};

use crate::{
    action::{download_repository_actions, repository_action_plans},
    checkout::{checkout_plans, execute_checkouts, has_unsupported_enabled_action},
    cli::{ConfigureArgs, RemoveArgs, RunArgs, StatusArgs},
    config::{self, CredentialScheme, RunnerSettings, StoredCredentials, StoredRunnerConfig},
    container::JobContainerSpec,
    executor::{DockerScriptExecutor, ProcessCommandRunner},
    job_message::{AgentJobRequestMessage, PIPELINE_AGENT_JOB_REQUEST},
    protocol::{
        DistributedTaskClient, GitHubAuthResult, GitHubScope, OAuthClient, OAuthJwtCredentials,
        RegistrationClient, RunnerEvent, RunnerKeyPair, RunnerStatus, TaskAgent, TaskAgentPool,
        TaskAgentSession, TaskResult, TimelineRecord, TimelineRecordFeedLines,
    },
    script_step::github_script_steps,
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
            let script_steps = match github_script_steps(&job.steps, "/__w") {
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
                if has_unsupported_enabled_action(&job.steps) {
                    probe_repository_actions(&dir, args.work_dir.clone(), &job)?;
                    println!(
                        "Job has enabled unsupported action steps; not executing or acknowledging."
                    );
                } else {
                    let result = execute_script_job(
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
                        result,
                        "Velnor executed supported run steps in Docker.".to_string(),
                    )
                    .await?;
                    println!(
                        "Script job completed with result {result:?} and message acknowledged."
                    );
                }
            } else if args.complete_noop {
                complete_job(
                    &client,
                    pool_id,
                    session_id,
                    message.message_id,
                    &stored.settings.agent_name,
                    &job,
                    TaskResult::Succeeded,
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

fn probe_repository_actions(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    job: &AgentJobRequestMessage,
) -> Result<()> {
    let actions = job_work_dir(config_dir, work_dir, job).join("actions");
    fs::create_dir_all(&actions).with_context(|| format!("create {}", actions.display()))?;
    let plans = repository_action_plans(&job.steps, &actions)?;
    if plans.is_empty() {
        return Ok(());
    }

    let mut command_runner = ProcessCommandRunner;
    let resolved_actions = download_repository_actions(&mut command_runner, &plans)?;
    println!(
        "Downloaded and resolved {} repository action(s). JavaScript execution is not wired yet.",
        resolved_actions.len()
    );
    Ok(())
}

fn execute_script_job(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    docker_image: &str,
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
) -> Result<TaskResult> {
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
    let repository_action_plans = repository_action_plans(&job.steps, &actions)?;
    if !repository_action_plans.is_empty() {
        let resolved_actions =
            download_repository_actions(&mut command_runner, &repository_action_plans)?;
        println!(
            "Downloaded and resolved {} repository action(s). JavaScript execution is not wired yet.",
            resolved_actions.len()
        );
    }

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
    let results = executor.execute_steps(&container, script_steps, &temp)?;
    let failed = results.iter().any(|result| result.exit_code != 0);

    Ok(if failed {
        TaskResult::Failed
    } else {
        TaskResult::Succeeded
    })
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
            TimelineRecordFeedLines::new(job.job_id.clone(), vec![feed_line], Some(1)),
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

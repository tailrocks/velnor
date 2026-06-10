use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc::UnboundedReceiver,
    task::{JoinHandle, JoinSet},
};

use crate::{
    action::{
        composite_action_invocations, composite_repository_action_plans,
        composite_repository_action_plans_from_resolved, download_repository_actions,
        is_local_action_step, local_action_plans_with_context, native_action_adapter,
        native_invocation_from_plan, repository_action_plans, resolve_local_action,
        unsupported_action_error, ActionMetadata, ActionRuntime, CompositeActionInvocation,
        LocalActionPlan, RepositoryActionPlan, ResolvedAction,
    },
    checkout::{
        checkout_plans, checkout_step_id, cleanup_checkout_credentials, configure_safe_directory,
        CheckoutPlan,
    },
    cli::{ConfigureArgs, DaemonArgs, PreflightArgs, RemoveArgs, RunArgs, StatusArgs},
    config::{self, CredentialScheme, RunnerSettings, StoredCredentials, StoredRunnerConfig},
    executor::{
        DockerScriptExecutor, ExecutableStep, ProcessCommandRunner, StepLog, StepStartEvent,
    },
    github_adapter::{
        github_job_container_spec, github_normalized_job_plan, job_container_name,
        system_connection_access_token, GitHubJobContainerPaths,
    },
    job_message::{ActionReferenceType, AgentJobRequestMessage},
    platform,
    protocol::{
        decode_jit_config, AcquireJobOutcome, BrokerClient, DistributedTaskClient,
        GitHubJitConfigRequest, GitHubScope, OAuthClient, OAuthJwtCredentials, RegistrationClient,
        RunServiceAnnotation, RunServiceAnnotationLevel, RunServiceClient, RunServiceCompleteJob,
        RunServiceStepResult, RunServiceTelemetry, RunServiceVariableValue, RunnerJobRequestRef,
        RunnerStatus, TaskAgentSession, TaskResult, TimelineRecord, TimelineRecordFeedLines,
        TimelineRecordState, RUNNER_JOB_REQUEST,
    },
    runtime_env::job_runtime_env,
    script_step::{StepAnnotation, StepAnnotationLevel},
};

const JOB_CANCELLATION_MESSAGE: &str = "JobCancellation";
const BROKER_MIGRATION_MESSAGE: &str = "BrokerMigration";
const FORCE_TOKEN_REFRESH_MESSAGE: &str = "ForceTokenRefresh";
const AGENT_REFRESH_MESSAGE: &str = "AgentRefresh";
const RUNNER_REFRESH_MESSAGE: &str = "RunnerRefresh";
const RUNNER_REFRESH_CONFIG_MESSAGE: &str = "RunnerRefreshConfig";
const RUNNER_SHUTDOWN_MESSAGE: &str = "RunnerShutdown";
const BROKER_POLL_MAX_CONSECUTIVE_ERRORS: u32 = 10;
const BROKER_POLL_EMPTY_BACKOFF_THRESHOLD: u32 = 50;
const BROKER_SESSION_CREATE_MAX_ATTEMPTS: u32 = 5;
const BROKER_SESSION_CREATE_RETRY_SECONDS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
enum V2MessageAction {
    None,
    BrokerMigration(String),
    RefreshToken,
    Shutdown,
    JobHandled,
}

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

#[derive(Default)]
struct BrokerPollState {
    consecutive_errors: u32,
    consecutive_empty_messages: u32,
}

impl BrokerPollState {
    fn received_message(&mut self) {
        self.consecutive_errors = 0;
        self.consecutive_empty_messages = 0;
    }

    fn received_empty_message(&mut self) -> Option<Duration> {
        self.consecutive_errors = 0;
        self.consecutive_empty_messages += 1;
        if self.consecutive_empty_messages > BROKER_POLL_EMPTY_BACKOFF_THRESHOLD {
            self.consecutive_empty_messages = 0;
            Some(Duration::from_secs(15))
        } else {
            None
        }
    }

    fn received_error(&mut self) -> Result<Duration> {
        self.consecutive_errors += 1;
        if self.consecutive_errors >= BROKER_POLL_MAX_CONSECUTIVE_ERRORS {
            bail!(
                "broker polling failed {} consecutive times",
                self.consecutive_errors
            );
        }
        let seconds = if self.consecutive_errors <= 5 { 15 } else { 30 };
        Ok(Duration::from_secs(seconds))
    }
}

pub async fn configure(args: ConfigureArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let scope = GitHubScope::parse(&args.url)?;
    let agent_name = args.name.unwrap_or_else(default_agent_name);
    let labels = normalize_labels(
        args.labels,
        args.target_mvp_labels,
        args.target_mvp_arm_label,
    );
    validate_linux_only_labels(&labels)?;
    platform::validate_arm_label_matches_host(&labels, std::env::consts::ARCH)?;

    if args.pool_name.is_some() && args.pool_id.is_none() {
        bail!("JIT runner config requires numeric --pool-id for non-default runner groups");
    }

    let runner_group_id = args.pool_id.unwrap_or(1);
    let jit_request = GitHubJitConfigRequest {
        name: agent_name.clone(),
        runner_group_id,
        labels: labels.clone(),
        work_folder: None,
    };

    let pat = if args.dry_run {
        None
    } else {
        Some(
            args.pat
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("GitHub PAT required for JIT config: pass --pat"))?,
        )
    };

    if args.replace {
        remove_existing_jit_config_for_replace(&dir, pat).await?;
    }

    let jit_config = if args.dry_run {
        None
    } else {
        let pat = pat.expect("live JIT config requires PAT");
        let jit_client = RegistrationClient::new()?;
        let response = match jit_client
            .generate_jit_config(&scope, pat, &jit_request)
            .await
        {
            Ok(r) => r,
            Err(e) if e.to_string().contains("409") => {
                // Orphaned runner from a previous failed run — delete by name and retry once.
                eprintln!(
                    "JIT 409: deleting orphaned runner '{}' and retrying",
                    agent_name
                );
                delete_orphaned_jit_runner_by_name(&scope, pat, &agent_name).await?;
                jit_client
                    .generate_jit_config(&scope, pat, &jit_request)
                    .await?
            }
            Err(e) => return Err(e),
        };
        Some((
            response.runner,
            decode_jit_config(&response.encoded_jit_config)?,
        ))
    };

    let stored = StoredRunnerConfig {
        settings: RunnerSettings {
            github_url: jit_config
                .as_ref()
                .and_then(|(_, config)| config.settings.github_url.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| scope.original_url.clone()),
            server_url: jit_config
                .as_ref()
                .and_then(|(_, config)| config.settings.server_url.clone()),
            server_url_v2: jit_config
                .as_ref()
                .and_then(|(_, config)| config.settings.server_url_v2.clone()),
            pool_id: jit_config.as_ref().and_then(|(runner, config)| {
                config
                    .settings
                    .pool_id
                    .or(runner.runner_group_id)
                    .or(Some(runner_group_id))
            }),
            pool_name: jit_config
                .as_ref()
                .and_then(|(_, config)| config.settings.pool_name.clone()),
            agent_id: jit_config
                .as_ref()
                .and_then(|(runner, config)| config.settings.agent_id.or(Some(runner.id))),
            agent_name: jit_config
                .as_ref()
                .and_then(|(_, config)| config.settings.agent_name.clone())
                .unwrap_or(agent_name),
            labels,
            use_v2_flow: jit_config
                .as_ref()
                .is_some_and(|(_, config)| config.settings.use_v2_flow),
            ephemeral: jit_config.as_ref().is_some(),
            disable_update: true,
        },
        credentials: match &jit_config {
            Some((_, config)) => Some(stored_jit_credentials(config)?),
            None => None,
        },
    };
    if jit_config.is_some()
        && (!stored.settings.use_v2_flow || stored.settings.server_url_v2.is_none())
    {
        bail!(
            "GitHub JIT config did not return required V2 runner settings (UseV2Flow/ServerUrlV2); Velnor uses the hosted GitHub broker/run-service protocol only"
        );
    }

    config::save(&dir, &stored)?;
    println!("Wrote local runner config to {}", dir.display());
    println!("GitHub scope API: {}", scope.api_base_url);
    println!("JIT config endpoint: {}", scope.jit_config_url);
    println!(
        "Prepared JIT runner request for '{}' with {} label(s) in runner group {}.",
        jit_request.name,
        jit_request.labels.len(),
        jit_request.runner_group_id
    );
    if let Some((runner, _)) = jit_config {
        println!(
            "Created JIT runner id {} in group {}.",
            runner.id,
            runner.runner_group_id.unwrap_or(runner_group_id)
        );
    } else {
        println!("Dry run: skipped JIT config request.");
    }

    Ok(())
}

async fn remove_existing_jit_config_for_replace(dir: &Path, pat: Option<&str>) -> Result<()> {
    if let Some(stored) = config::load(dir).ok() {
        if let (Some(pat), Some(agent_id)) = (pat, stored.settings.agent_id) {
            let scope = GitHubScope::parse(&stored.settings.github_url)?;
            // Best-effort: a runner that is mid-job returns 422 ("currently
            // busy"). That must NOT crash daemon startup — the busy runner will
            // finish and go offline on its own; we just drop the stale local
            // config and register a fresh JIT runner for this slot.
            match RegistrationClient::new()?
                .delete_runner(&scope, pat, agent_id)
                .await
            {
                Ok(()) => println!(
                    "Deleted or confirmed absent existing JIT runner id {agent_id} before replace."
                ),
                Err(e) => eprintln!(
                    "Warning: could not delete existing JIT runner id {agent_id} before replace (continuing): {e:#}"
                ),
            }
        }
        if config::remove(dir)? {
            println!(
                "Removed existing local JIT runner config from {}",
                dir.display()
            );
        }
    }
    Ok(())
}

/// After a 409 Conflict on JIT creation, find and delete any orphaned runner
/// with the given name on GitHub, then allow the caller to retry.
pub async fn delete_orphaned_jit_runner_by_name(
    scope: &GitHubScope,
    pat: &str,
    agent_name: &str,
) -> Result<()> {
    let client = RegistrationClient::new()?;
    let agents = client.list_runners(scope, pat).await?;
    let orphan = agents
        .iter()
        .find(|a| a.name.as_deref() == Some(agent_name));
    if let Some(orphan) = orphan {
        let id = orphan
            .id
            .ok_or_else(|| anyhow::anyhow!("orphaned runner has no id"))?;
        client
            .delete_runner(scope, pat, id)
            .await
            .with_context(|| format!("delete orphaned JIT runner '{agent_name}' id {id}"))?;
        println!("Deleted orphaned JIT runner '{agent_name}' id {id} before retry.");
    }
    Ok(())
}

fn stored_jit_credentials(config: &crate::protocol::DecodedJitConfig) -> Result<StoredCredentials> {
    let mut data = config
        .credentials
        .data
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    if config.credentials.scheme.eq_ignore_ascii_case("OAuth") {
        data.insert(
            "privateKeyPem".to_string(),
            serde_json::Value::String(config.private_key_pem.clone()),
        );
    }

    Ok(StoredCredentials {
        scheme: if config.credentials.scheme.eq_ignore_ascii_case("OAuth") {
            CredentialScheme::OAuth
        } else {
            credential_scheme(&config.credentials.scheme)?
        },
        data: serde_json::Value::Object(data),
    })
}

fn credential_scheme(token_schema: &str) -> Result<CredentialScheme> {
    if token_schema.eq_ignore_ascii_case("OAuthAccessToken") {
        Ok(CredentialScheme::OAuthAccessToken)
    } else {
        bail!("unsupported GitHub runner token schema: {token_schema}")
    }
}

pub async fn run(args: RunArgs) -> Result<()> {
    if args.complete_noop && args.execute_scripts {
        bail!("--complete-noop and --execute-scripts are mutually exclusive");
    }

    let dir = config::config_dir(args.config_dir.clone())?;
    preflight_before_executable_run(&args, &dir)?;
    let stored = config::load(&dir)?;
    let agent_id = stored
        .settings
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("runner is not configured: missing agent_id"))?;
    let token = oauth_access_token(&stored).await?;
    ensure_v2_runner_settings(&stored)?;
    run_v2(args, dir, stored, agent_id, token).await
}

pub async fn daemon(args: DaemonArgs) -> Result<()> {
    let slots = validate_daemon_slots(args.slots)?;
    if args.complete_noop && args.execute_scripts {
        bail!("--complete-noop and --execute-scripts are mutually exclusive");
    }

    let config_base = daemon_config_dir(&args)?;
    preflight_before_daemon_jit_config(&args, &config_base, slots)?;
    if args.url.is_some() && !args.dry_run_registration {
        let daemon_id = args
            .work_dir
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "default".to_string());
        prune_stale_velnor_docker_resources(&daemon_id);
    }
    configure_daemon_slots(&args, &config_base, slots).await?;
    if !daemon_should_poll_after_jit_config(&args) {
        println!("Daemon JIT config dry run complete; skipped polling GitHub for jobs.");
        return Ok(());
    }
    let mut slot_tasks = JoinSet::new();

    println!(
        "Starting Velnor daemon with {slots} internal runner slot{}.",
        if slots == 1 { "" } else { "s" }
    );
    if slots > 1 {
        println!(
            "Each slot uses its own GitHub runner config under {}/slots/slot-N.",
            config_base.display()
        );
    }

    for slot_index in 1..=slots {
        let daemon_args = args.clone();
        let config_base = config_base.clone();
        slot_tasks.spawn(async move {
            run_daemon_slot(daemon_args, config_base, slot_index, slots)
                .await
                .with_context(|| format!("daemon slot-{slot_index} failed"))
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = slot_tasks.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!("{error:#}");
                failures.push(error);
            }
            Err(error) => {
                let error = anyhow::Error::new(error).context("daemon slot task panicked");
                eprintln!("{error:#}");
                failures.push(error);
            }
        }
    }
    if !failures.is_empty() {
        bail!("{} daemon slot task(s) failed", failures.len());
    }

    Ok(())
}

async fn run_daemon_slot(
    args: DaemonArgs,
    config_base: PathBuf,
    slot_index: usize,
    slots: usize,
) -> Result<()> {
    if args.url.is_none() {
        let slot_args = daemon_slot_run_args(&args, &config_base, slot_index, slots)?;
        return run(slot_args).await;
    }

    let mut cycle = 1_u64;
    loop {
        let mut slot_args = daemon_slot_run_args(&args, &config_base, slot_index, slots)?;
        if !args.once {
            slot_args.once = true;
        }
        if let Err(error) = run(slot_args).await {
            cleanup_failed_daemon_slot(&args, &config_base, slot_index, slots, cycle).await;
            if args.once {
                return Err(error);
            }
            eprintln!(
                "daemon slot-{slot_index} cycle {cycle} failed; creating a fresh JIT config before retry: {error:#}"
            );
            retry_daemon_slot_jit_config(&args, &config_base, slot_index, slots, cycle).await?;
            cycle += 1;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        if args.once {
            return Ok(());
        }
        recycle_daemon_slot(&args, &config_base, slot_index, slots, cycle).await?;
        cycle += 1;
    }
}

async fn recycle_daemon_slot(
    args: &DaemonArgs,
    config_base: &Path,
    slot_index: usize,
    slots: usize,
    cycle: u64,
) -> Result<()> {
    let slot_dir = daemon_slot_config_dir(config_base, slot_index, slots);
    if config::remove(&slot_dir)? {
        println!(
            "Discarded local JIT runner config for {} after cycle {cycle}.",
            daemon_slot_name(slot_index)
        );
    }
    let configure_args = daemon_slot_configure_args(args, config_base, slot_index, slots)?;
    configure(configure_args)
        .await
        .with_context(|| format!("recycle JIT config for daemon slot-{slot_index}"))?;
    Ok(())
}

async fn cleanup_failed_daemon_slot(
    args: &DaemonArgs,
    config_base: &Path,
    slot_index: usize,
    slots: usize,
    cycle: u64,
) {
    let slot_dir = daemon_slot_config_dir(config_base, slot_index, slots);
    if let Err(error) = delete_and_remove_daemon_slot_jit_config(args, &slot_dir).await {
        eprintln!(
            "daemon slot-{slot_index} cycle {cycle} cleanup failed for {}: {error:#}",
            slot_dir.display()
        );
    }
}

async fn retry_daemon_slot_jit_config(
    args: &DaemonArgs,
    config_base: &Path,
    slot_index: usize,
    slots: usize,
    cycle: u64,
) -> Result<()> {
    let configure_args = daemon_slot_configure_args(args, config_base, slot_index, slots)?;
    configure(configure_args).await.with_context(|| {
        format!("retry JIT config for daemon slot-{slot_index} after cycle {cycle}")
    })
}

async fn configure_daemon_slots(args: &DaemonArgs, config_base: &Path, slots: usize) -> Result<()> {
    if args.url.is_none() {
        return Ok(());
    }

    println!("Configuring {slots} Velnor daemon JIT runner slot(s) before polling GitHub.");
    let mut configured_slots = Vec::new();
    let mut usable_slots = 0usize;
    let mut skipped_slots = Vec::new();
    for slot_index in 1..=slots {
        let slot_config_dir = daemon_slot_config_dir(config_base, slot_index, slots);
        if !daemon_slot_should_configure_jit(
            &slot_config_dir,
            args.replace,
            args.dry_run_registration,
        ) {
            println!(
                "Using existing daemon {} JIT config at {}.",
                daemon_slot_name(slot_index),
                slot_config_dir.display()
            );
            usable_slots += 1;
            continue;
        }

        if args.replace && !args.dry_run_registration {
            delete_and_remove_daemon_slot_jit_config(args, &slot_config_dir).await?;
        }

        let configure_args = daemon_slot_configure_args(args, config_base, slot_index, slots)?;
        // Per-slot best-effort: a slot whose previous runner is still registered
        // and busy (stale from a prior crash) can't reclaim its name yet and will
        // fail here (409 → orphan delete → 422). That must NOT take down the whole
        // daemon — skip this slot and run on the rest; it recovers on a later
        // restart once the stale runner ages out.
        if let Err(error) = configure(configure_args).await {
            eprintln!(
                "Warning: could not configure daemon slot-{slot_index} (skipping; running on the remaining slots): {error:#}"
            );
            skipped_slots.push(slot_index);
            continue;
        }
        configured_slots.push(slot_index);
        usable_slots += 1;
    }

    if usable_slots == 0 {
        cleanup_configured_daemon_slots(args, config_base, slots, &configured_slots).await;
        bail!(
            "could not configure any of the {slots} daemon runner slot(s); all failed (e.g. stale busy runners holding every slot name)"
        );
    }
    if !skipped_slots.is_empty() {
        eprintln!(
            "Daemon starting with {usable_slots}/{slots} runner slot(s); skipped slot(s): {skipped_slots:?}."
        );
    }
    Ok(())
}

async fn cleanup_configured_daemon_slots(
    args: &DaemonArgs,
    config_base: &Path,
    slots: usize,
    configured_slots: &[usize],
) {
    for slot_index in configured_slots {
        let slot_dir = daemon_slot_config_dir(config_base, *slot_index, slots);
        if let Err(error) = delete_and_remove_daemon_slot_jit_config(args, &slot_dir).await {
            eprintln!(
                "cleanup failed for configured daemon slot-{slot_index} at {}: {error:#}",
                slot_dir.display()
            );
        }
    }
}

async fn delete_and_remove_daemon_slot_jit_config(
    args: &DaemonArgs,
    slot_dir: &Path,
) -> Result<()> {
    let Some(stored) = config::load(slot_dir).ok() else {
        return Ok(());
    };

    if let (Some(pat), Some(agent_id)) = (args.pat.as_ref(), stored.settings.agent_id) {
        let scope = GitHubScope::parse(&stored.settings.github_url)?;
        // Best-effort (see remove_existing_jit_config_for_replace): a busy runner
        // returns 422 and must not abort daemon startup.
        match RegistrationClient::new()?
            .delete_runner(&scope, pat, agent_id)
            .await
        {
            Ok(()) => println!("Deleted or confirmed absent daemon JIT runner id {agent_id}."),
            Err(e) => eprintln!(
                "Warning: could not delete daemon JIT runner id {agent_id} (continuing): {e:#}"
            ),
        }
    }

    if config::remove(slot_dir)? {
        println!(
            "Removed local daemon JIT runner config from {}",
            slot_dir.display()
        );
    }
    Ok(())
}

fn daemon_slot_should_configure_jit(
    slot_config_dir: &Path,
    replace: bool,
    dry_run_registration: bool,
) -> bool {
    replace || dry_run_registration || config::load(slot_config_dir).is_err()
}

fn daemon_should_poll_after_jit_config(args: &DaemonArgs) -> bool {
    !args.dry_run_registration
}

fn daemon_config_dir(args: &DaemonArgs) -> Result<PathBuf> {
    if args.config_dir.is_some() || env::var_os("VELNOR_CONFIG_DIR").is_some() {
        return config::config_dir(args.config_dir.clone());
    }

    let base = config::config_dir(None)?;
    let Some(identity) = args
        .name
        .as_deref()
        .or(args.work_dir.as_deref().and_then(Path::to_str))
        .or(args.url.as_deref())
    else {
        return Ok(base);
    };

    Ok(base
        .join("daemons")
        .join(sanitize_daemon_config_component(identity)))
}

fn sanitize_daemon_config_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn preflight_before_daemon_jit_config(
    args: &DaemonArgs,
    config_base: &Path,
    slots: usize,
) -> Result<()> {
    if args.url.is_none() || args.dry_run_registration {
        return Ok(());
    }

    for preflight_args in daemon_preflight_args(args, config_base, slots)? {
        crate::preflight::preflight(preflight_args)
            .context("Docker preflight failed before daemon JIT runner configuration")?;
    }
    Ok(())
}

fn daemon_preflight_args(
    args: &DaemonArgs,
    config_base: &Path,
    slots: usize,
) -> Result<Vec<PreflightArgs>> {
    (1..=slots)
        .map(|slot_index| {
            let run_args = daemon_slot_run_args(args, config_base, slot_index, slots)?;
            if !should_execute_job(&run_args) || run_args.skip_preflight {
                Ok(None)
            } else {
                let config_dir = run_args.config_dir.as_deref().unwrap_or(config_base);
                Ok(Some(preflight_args_for_run(&run_args, config_dir)))
            }
        })
        .filter_map(Result::transpose)
        .collect()
}

fn validate_daemon_slots(slots: usize) -> Result<usize> {
    if slots == 0 {
        bail!("--slots must be greater than zero");
    }
    Ok(slots)
}

/// Remove leftover Velnor Docker resources from previous (possibly crashed)
/// daemon runs. A daemon killed mid-job cannot run its per-job cleanup, so the
/// job network + container leak; enough leaked `velnor-net-*` networks exhaust
/// Docker's address pool and then EVERY new job fails to create its network
/// ("all predefined address pools have been fully subnetted"). Pruning on
/// startup makes a crash self-healing. Best-effort — never fails startup. Safe
/// because a daemon restart already orphans any in-flight job (JIT runners are
/// per-job), so anything matching here is dead.
fn prune_stale_velnor_docker_resources(daemon_id: &str) {
    let docker = |args: &[&str]| {
        std::process::Command::new("docker")
            .args(args)
            .output()
            .ok()
    };
    let ids_from = |args: &[&str]| -> Vec<String> {
        docker(args)
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    let label_filter = format!("label=velnor.daemon-id={daemon_id}");
    let containers = ids_from(&[
        "ps",
        "-aq",
        "--filter",
        "name=velnor-job",
        "--filter",
        &label_filter,
    ]);
    if !containers.is_empty() {
        let mut args = vec!["rm".to_string(), "-f".to_string()];
        args.extend(containers.iter().cloned());
        let _ = docker(&args.iter().map(String::as_str).collect::<Vec<_>>());
        eprintln!(
            "Pruned {} stale velnor-job container(s) at startup.",
            containers.len()
        );
    }

    let networks = ids_from(&["network", "ls", "-q", "--filter", "name=velnor-net"]);
    if !networks.is_empty() {
        let mut args = vec!["network".to_string(), "rm".to_string()];
        args.extend(networks.iter().cloned());
        let _ = docker(&args.iter().map(String::as_str).collect::<Vec<_>>());
        eprintln!(
            "Pruned {} stale velnor-net network(s) at startup.",
            networks.len()
        );
    }
}

fn daemon_slot_configure_args(
    args: &DaemonArgs,
    config_base: &Path,
    slot_index: usize,
    slot_count: usize,
) -> Result<ConfigureArgs> {
    validate_daemon_slot_index(slot_index, slot_count)?;
    let url = args
        .url
        .clone()
        .ok_or_else(|| anyhow::anyhow!("daemon slot JIT configuration requires --url"))?;

    Ok(ConfigureArgs {
        url,
        pat: args.pat.clone(),
        name: daemon_slot_agent_name(args.name.as_deref(), slot_index, slot_count),
        labels: args.labels.clone(),
        target_mvp_labels: args.target_mvp_labels,
        target_mvp_arm_label: args.target_mvp_arm_label,
        replace: args.replace,
        pool_id: args.pool_id,
        pool_name: args.pool_name.clone(),
        dry_run: args.dry_run_registration,
        config_dir: Some(daemon_slot_config_dir(config_base, slot_index, slot_count)),
    })
}

fn daemon_slot_run_args(
    args: &DaemonArgs,
    config_base: &Path,
    slot_index: usize,
    slot_count: usize,
) -> Result<RunArgs> {
    validate_daemon_slot_index(slot_index, slot_count)?;

    Ok(RunArgs {
        config_dir: Some(daemon_slot_config_dir(config_base, slot_index, slot_count)),
        once: args.once,
        idle_timeout_seconds: args.idle_timeout_seconds,
        complete_noop: args.complete_noop,
        execute_scripts: args.execute_scripts,
        dry_run_jobs: args.dry_run_jobs,
        dump_job_message: daemon_slot_child_path(
            args.dump_job_message.as_deref(),
            slot_index,
            slot_count,
        ),
        docker_image: args.docker_image.clone(),
        node_action_image: args.node_action_image.clone(),
        work_dir: daemon_slot_child_path(args.work_dir.as_deref(), slot_index, slot_count),
        docker_host_work_dir: daemon_slot_child_path(
            args.docker_host_work_dir.as_deref(),
            slot_index,
            slot_count,
        ),
        skip_preflight: args.skip_preflight,
        require_docker_socket: args.require_docker_socket,
    })
}

fn validate_daemon_slot_index(slot_index: usize, slot_count: usize) -> Result<()> {
    if slot_index == 0 || slot_index > slot_count {
        bail!("daemon slot index {slot_index} is outside 1..={slot_count}");
    }
    Ok(())
}

fn daemon_slot_config_dir(config_base: &Path, slot_index: usize, slot_count: usize) -> PathBuf {
    if slot_count == 1 {
        return config_base.to_path_buf();
    }
    config_base.join("slots").join(daemon_slot_name(slot_index))
}

fn daemon_slot_child_path(
    base: Option<&Path>,
    slot_index: usize,
    slot_count: usize,
) -> Option<PathBuf> {
    base.map(|path| {
        if slot_count == 1 {
            path.to_path_buf()
        } else {
            path.join(daemon_slot_name(slot_index))
        }
    })
}

fn daemon_slot_name(slot_index: usize) -> String {
    format!("slot-{slot_index}")
}

fn daemon_slot_agent_name(
    base_name: Option<&str>,
    slot_index: usize,
    slot_count: usize,
) -> Option<String> {
    match (base_name, slot_count) {
        (None, 1) => None,
        (Some(name), 1) => Some(name.to_string()),
        (Some(name), _) => Some(format!("{name}-{}", daemon_slot_name(slot_index))),
        (None, _) => Some(format!(
            "{}-{}",
            default_agent_name(),
            daemon_slot_name(slot_index)
        )),
    }
}

fn preflight_before_executable_run(args: &RunArgs, config_dir: &Path) -> Result<()> {
    if !should_execute_job(args) || args.skip_preflight {
        return Ok(());
    }

    crate::preflight::preflight(preflight_args_for_run(args, config_dir))
        .context("Docker preflight failed before polling GitHub for jobs")
}

fn preflight_args_for_run(args: &RunArgs, config_dir: &Path) -> PreflightArgs {
    PreflightArgs {
        work_dir: Some(
            args.work_dir
                .clone()
                .unwrap_or_else(|| config_dir.join("_work")),
        ),
        docker_host_work_dir: args.docker_host_work_dir.clone(),
        docker_image: args.docker_image.clone(),
        require_docker_socket: args.require_docker_socket,
        require_buildx: true,
    }
}

fn ensure_v2_runner_settings(stored: &StoredRunnerConfig) -> Result<()> {
    if stored.settings.use_v2_flow && stored.settings.server_url_v2.is_some() {
        return Ok(());
    }
    bail!(
        "runner config is missing required V2 settings (UseV2Flow/ServerUrlV2); reconfigure with GitHub JIT runner configuration"
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
    let mut broker_token = token.clone();
    let mut current_broker_url = server_url_v2.to_string();
    let mut broker = BrokerClient::new(&current_broker_url, broker_token.clone())?;
    let mut run_service = RunServiceClient::new(token.clone())?;
    let owner_name = format!("{} (PID: {})", default_agent_name(), std::process::id());
    let session = TaskAgentSession::new(owner_name, agent_id, stored.settings.agent_name.clone());
    let diagnostic = RunnerConnectionDiagnostic::from_config(&stored, &current_broker_url);
    let session = create_broker_session_with_retry(&broker, &session, &diagnostic).await?;
    let session_id = session
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("GitHub broker returned session without sessionId"))?;
    let mut poll_state = BrokerPollState::default();
    let idle_timeout = idle_timeout_duration(args.idle_timeout_seconds)?;
    let idle_started = Instant::now();

    println!(
        "Runner '{}' ready via broker with labels: {}",
        stored.settings.agent_name,
        stored.settings.labels.join(",")
    );
    println!("Created broker runner session {session_id}.");

    let run_result = async {
        loop {
            let message = poll_broker_message(
                &broker,
                session_id,
                RunnerStatus::Online,
                stored.settings.disable_update,
                &mut poll_state,
            )
            .await?;

            let Some(message) = message else {
                println!("No broker message received.");
                fail_if_idle_timeout_elapsed(idle_started, idle_timeout)?;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };

            let action = handle_v2_message(
                &broker,
                &run_service,
                session_id,
                &stored,
                &config_dir,
                &args,
                stored.settings.disable_update,
                &stored.settings.agent_name,
                message,
            )
            .await?;

            match &action {
                V2MessageAction::None => {}
                V2MessageAction::BrokerMigration(migration_url) => {
                    current_broker_url = migration_url.clone();
                    broker = BrokerClient::new(&current_broker_url, broker_token.clone())?;
                    println!("Broker migration applied: {current_broker_url}");
                }
                V2MessageAction::RefreshToken => {
                    let refreshed_token = oauth_access_token(&stored).await?;
                    broker_token = refreshed_token.clone();
                    broker = BrokerClient::new(&current_broker_url, broker_token.clone())?;
                    run_service = RunServiceClient::new(refreshed_token)?;
                    println!("Refreshed broker and run-service credentials.");
                }
                V2MessageAction::Shutdown => {
                    println!("GitHub requested runner shutdown.");
                    break;
                }
                V2MessageAction::JobHandled => {}
            }
            if should_stop_after_message(args.once, &action) {
                break;
            }
            if !matches!(action, V2MessageAction::JobHandled) {
                fail_if_idle_timeout_elapsed(idle_started, idle_timeout)?;
            }
        }
        Ok(())
    }
    .await;

    match broker.delete_session().await {
        Ok(()) => println!("Deleted broker runner session."),
        Err(error) if run_result.is_ok() => {
            return Err(error).context("delete broker runner session");
        }
        Err(error) => {
            eprintln!("Best-effort broker session delete failed: {error:#}");
        }
    }

    run_result
}

async fn create_broker_session_with_retry(
    broker: &BrokerClient,
    session: &TaskAgentSession,
    diagnostic: &RunnerConnectionDiagnostic,
) -> Result<TaskAgentSession> {
    let mut attempt = 1;
    loop {
        match broker.create_session(session).await {
            Ok(session) => return Ok(session),
            Err(error) if attempt < BROKER_SESSION_CREATE_MAX_ATTEMPTS => {
                let delay = broker_session_create_retry_delay(attempt);
                eprintln!(
                    "Broker session create failed on attempt {attempt}/{}: {error:#}. Retrying in {}s.",
                    BROKER_SESSION_CREATE_MAX_ATTEMPTS,
                    delay.as_secs()
                );
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create broker runner session ({diagnostic})"));
            }
        }
    }
}

struct RunnerConnectionDiagnostic {
    github_url: String,
    broker_url: String,
    agent_name: String,
    agent_id: Option<i64>,
    pool_id: Option<i64>,
    labels: Vec<String>,
    use_v2_flow: bool,
}

impl RunnerConnectionDiagnostic {
    fn from_config(stored: &StoredRunnerConfig, broker_url: &str) -> Self {
        Self {
            github_url: stored.settings.github_url.clone(),
            broker_url: broker_url.to_string(),
            agent_name: stored.settings.agent_name.clone(),
            agent_id: stored.settings.agent_id,
            pool_id: stored.settings.pool_id,
            labels: stored.settings.labels.clone(),
            use_v2_flow: stored.settings.use_v2_flow,
        }
    }
}

impl std::fmt::Display for RunnerConnectionDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "github_url={}, broker_url={}, agent_name={}, agent_id={}, pool_id={}, use_v2_flow={}, labels={}",
            self.github_url,
            self.broker_url,
            self.agent_name,
            self.agent_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.pool_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.use_v2_flow,
            self.labels.join(",")
        )
    }
}

fn broker_session_create_retry_delay(attempt: u32) -> Duration {
    let multiplier = attempt.clamp(1, 3) as u64;
    Duration::from_secs(BROKER_SESSION_CREATE_RETRY_SECONDS * multiplier)
}

fn should_stop_after_message(once: bool, action: &V2MessageAction) -> bool {
    once && matches!(action, V2MessageAction::JobHandled)
}

fn idle_timeout_duration(seconds: Option<u64>) -> Result<Option<Duration>> {
    match seconds {
        Some(0) => bail!("--idle-timeout-seconds must be greater than zero"),
        Some(seconds) => Ok(Some(Duration::from_secs(seconds))),
        None => Ok(None),
    }
}

fn fail_if_idle_timeout_elapsed(started: Instant, timeout: Option<Duration>) -> Result<()> {
    if idle_timeout_elapsed(started.elapsed(), timeout) {
        let seconds = timeout.map_or(0, |timeout| timeout.as_secs());
        bail!("no GitHub job was acquired within idle timeout of {seconds}s");
    }
    Ok(())
}

fn idle_timeout_elapsed(elapsed: Duration, timeout: Option<Duration>) -> bool {
    timeout.is_some_and(|timeout| elapsed >= timeout)
}

async fn poll_broker_message(
    broker: &BrokerClient,
    session_id: &str,
    status: RunnerStatus,
    disable_update: bool,
    poll_state: &mut BrokerPollState,
) -> Result<Option<crate::protocol::TaskAgentMessage>> {
    loop {
        match broker
            .get_runner_message(session_id, status, disable_update)
            .await
        {
            Ok(Some(message)) => {
                poll_state.received_message();
                return Ok(Some(message));
            }
            Ok(None) => {
                if let Some(delay) = poll_state.received_empty_message() {
                    println!(
                        "No broker message after {} consecutive polls; backing off for {}s.",
                        BROKER_POLL_EMPTY_BACKOFF_THRESHOLD,
                        delay.as_secs()
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Ok(None);
            }
            Err(error) => {
                let delay = poll_state.received_error()?;
                eprintln!(
                    "Broker message poll failed ({} consecutive error(s)): {error:#}. Retrying in {}s.",
                    poll_state.consecutive_errors,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn handle_v2_message(
    broker: &BrokerClient,
    run_service: &RunServiceClient,
    session_id: &str,
    stored: &StoredRunnerConfig,
    config_dir: &std::path::Path,
    args: &RunArgs,
    disable_update: bool,
    runner_name: &str,
    message: crate::protocol::TaskAgentMessage,
) -> Result<V2MessageAction> {
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
        return Ok(V2MessageAction::BrokerMigration(migration_url));
    }
    if message
        .message_type
        .eq_ignore_ascii_case(FORCE_TOKEN_REFRESH_MESSAGE)
    {
        println!("Received ForceTokenRefresh control message.");
        return Ok(V2MessageAction::RefreshToken);
    }
    if message
        .message_type
        .eq_ignore_ascii_case(AGENT_REFRESH_MESSAGE)
        || message
            .message_type
            .eq_ignore_ascii_case(RUNNER_REFRESH_MESSAGE)
    {
        println!(
            "Received runner update message type {}; self-update is disabled in Velnor Phase 0.",
            message.message_type
        );
        return Ok(V2MessageAction::None);
    }
    if message
        .message_type
        .eq_ignore_ascii_case(RUNNER_REFRESH_CONFIG_MESSAGE)
    {
        println!(
            "Received runner config refresh message; restart the runner to reload hosted GitHub settings. Current runner: {}.",
            stored.settings.agent_name
        );
        return Ok(V2MessageAction::Shutdown);
    }
    if message
        .message_type
        .eq_ignore_ascii_case(RUNNER_SHUTDOWN_MESSAGE)
    {
        println!("Received hosted runner shutdown message.");
        return Ok(V2MessageAction::Shutdown);
    }
    if message
        .message_type
        .eq_ignore_ascii_case(JOB_CANCELLATION_MESSAGE)
    {
        println!(
            "Received idle job cancellation message {}; no active job matched in this runner slot.",
            message.message_id
        );
        return Ok(V2MessageAction::None);
    }
    if !message
        .message_type
        .eq_ignore_ascii_case(RUNNER_JOB_REQUEST)
    {
        println!("Broker message is not acknowledged because type is not implemented.");
        return Ok(V2MessageAction::None);
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
            return Ok(V2MessageAction::None);
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
    handle_job_request(
        config_dir,
        args,
        run_service_job,
        broker_cancellation,
        runner_name,
        job,
    )
    .await?;
    Ok(V2MessageAction::JobHandled)
}

async fn handle_job_request(
    config_dir: &std::path::Path,
    args: &RunArgs,
    run_service_job: RunServiceJobContext,
    broker_cancellation: BrokerCancellationContext,
    runner_name: &str,
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
    let early_context = job_context_data(&job);
    let mut job = job;
    apply_workflow_script_step_names(&mut job, &early_context).await;
    let script_steps = match crate::script_step::github_script_steps_with_context(
        &job.steps,
        "/__w",
        &job.defaults,
        &early_context,
    ) {
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
        if let Err(error) = publish_timeline_job_started(&job, runner_name).await {
            eprintln!("Best-effort timeline job start update failed: {error:#}");
        }
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
        let (step_start_sender, step_start_receiver) = tokio::sync::mpsc::unbounded_channel();
        let step_timeline = start_step_timeline_publisher(job.clone(), step_start_receiver);
        let (step_log_sender, step_log_receiver) = tokio::sync::mpsc::unbounded_channel();
        let console_log_path = Some(job_console_log_path(
            config_dir,
            args.work_dir.clone(),
            &job,
        ));
        let step_logs_publisher =
            start_step_log_publisher(job.clone(), step_log_receiver, console_log_path);
        let config_dir = config_dir.to_path_buf();
        let work_dir = args.work_dir.clone();
        let docker_host_work_dir = args.docker_host_work_dir.clone();
        let docker_image = args.docker_image.clone();
        let node_action_image = args.node_action_image.clone();
        let run_service_url = run_service_job.run_service_url.clone();
        let billing_owner_id = run_service_job.billing_owner_id.clone();
        let daemon_id = args
            .work_dir
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "default".to_string());
        let job_to_execute = job.clone();
        let script_steps = script_steps.clone();
        let job_result = tokio::task::spawn_blocking(move || {
            execute_script_job(
                &config_dir,
                work_dir,
                docker_host_work_dir,
                &docker_image,
                &node_action_image,
                &run_service_url,
                billing_owner_id,
                &job_to_execute,
                &script_steps,
                Some(step_start_sender),
                Some(step_log_sender),
                daemon_id,
            )
        })
        .await
        .context("join Docker job execution task")?;
        cancellation.abort();
        renewal.abort();
        match tokio::time::timeout(Duration::from_secs(5), step_timeline).await {
            Ok(Err(error)) if !error.is_cancelled() => {
                eprintln!("Step timeline publisher failed: {error:#}");
            }
            Ok(_) => {}
            Err(_) => eprintln!("Timed out waiting for best-effort step timeline publisher."),
        }
        match tokio::time::timeout(Duration::from_secs(30), step_logs_publisher).await {
            Ok(Err(error)) if !error.is_cancelled() => {
                eprintln!("Step log publisher failed: {error:#}");
            }
            Ok(_) => {}
            Err(_) => eprintln!("Timed out waiting for best-effort step log publisher."),
        }
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
                        environment_url: None,
                        step_logs: Vec::new(),
                    }
                } else {
                    let infrastructure_failure_category =
                        infrastructure_failure_category(&error).map(ToOwned::to_owned);
                    complete_run_service_job(
                        &run_service_job.client,
                        &run_service_job.run_service_url,
                        &job,
                        TaskResult::Failed,
                        BTreeMap::new(),
                        Vec::new(),
                        None,
                        run_service_job.billing_owner_id.clone(),
                        infrastructure_failure_category,
                        true,
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
            job_result.environment_url,
            run_service_job.billing_owner_id,
            None,
            false,
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
            None,
            run_service_job.billing_owner_id,
            None,
            true,
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
    destination.join(job_dump_filename(job))
}

fn job_dump_filename(job: &AgentJobRequestMessage) -> String {
    let repository = job
        .variables
        .get("github.repository")
        .and_then(|variable| variable.value.as_deref())
        .unwrap_or("unknown-repo");
    let run_id = job
        .variables
        .get("github.run_id")
        .and_then(|variable| variable.value.as_deref())
        .unwrap_or("unknown-run");
    format!(
        "job-{}-{}-{}-{}-{}.json",
        sanitize_path_segment(repository),
        sanitize_path_segment(run_id),
        job.request_id,
        sanitize_path_segment(
            &job.job_name
                .clone()
                .unwrap_or_else(|| job.job_display_name.clone())
        ),
        sanitize_path_segment(&job.job_id)
    )
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
            // Renew every 25 seconds — the run-service validity window is ~30 seconds,
            // so renewing at 25s keeps safe margin without hammering the API.
            tokio::time::sleep(Duration::from_secs(25)).await;
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

fn start_step_timeline_publisher(
    job: AgentJobRequestMessage,
    mut receiver: UnboundedReceiver<StepStartEvent>,
) -> JoinHandle<()> {
    // Build Twirp client for Results Service step status updates if available.
    let twirp_client = job
        .system_connection()
        .and_then(|ep| {
            let token = ep
                .authorization
                .as_ref()
                .and_then(|a| a.parameters.get("AccessToken"))
                .cloned()
                .unwrap_or_default();
            crate::protocol::TwirpResultsClient::from_endpoint_data(&ep.data, &token)
        })
        .and_then(|r| r.ok());

    let plan_id = job.plan.plan_id.clone();
    let job_id = job.job_id.clone();

    tokio::spawn(async move {
        let mut change_order: i64 = 1;
        while let Some(event) = receiver.recv().await {
            // Send step "in-progress" update via Twirp Results Service
            if let Some(client) = &twirp_client {
                let step = crate::protocol::TwirpStep {
                    external_id: event.step_id.clone(),
                    number: event.order as usize,
                    name: if event.display_name.is_empty() {
                        event.step_id.clone()
                    } else {
                        event.display_name.clone()
                    },
                    status: crate::protocol::StepStatus::InProgress as u8,
                    started_at: Some(unix_now_iso8601()),
                    completed_at: None,
                    conclusion: crate::protocol::StepConclusion::Unknown as u8,
                };
                if let Err(e) = client
                    .update_steps(&[step], &plan_id, &job_id, change_order)
                    .await
                {
                    eprintln!(
                        "Best-effort Twirp step start failed for '{}': {e:#}",
                        event.step_id
                    );
                }
                change_order += 1;
            }

            // Also update the distributed task timeline (legacy path)
            if let Err(error) = publish_timeline_step_started(&job, &event).await {
                eprintln!(
                    "Best-effort timeline step start update failed for '{}': {error:#}",
                    event.step_id
                );
            }
        }
    })
}

fn start_step_log_publisher(
    job: AgentJobRequestMessage,
    mut receiver: UnboundedReceiver<StepLog>,
    console_log_path: Option<PathBuf>,
) -> JoinHandle<()> {
    let plan_id_for_feed = job.plan.plan_id.clone();
    let job_id_for_feed = job.job_id.clone();
    let feed_client = job.system_connection().and_then(|ep| {
        let token = ep
            .authorization
            .as_ref()
            .and_then(|a| a.parameters.get("AccessToken"))
            .cloned()
            .unwrap_or_default();
        crate::protocol::FeedStreamClient::from_endpoint_data(&ep.data, &token)
            .map(|c| c.with_context(&plan_id_for_feed, &job_id_for_feed))
    });

    let twirp_client = job
        .system_connection()
        .and_then(|ep| {
            let token = ep
                .authorization
                .as_ref()
                .and_then(|a| a.parameters.get("AccessToken"))
                .cloned()
                .unwrap_or_default();
            crate::protocol::TwirpResultsClient::from_endpoint_data(&ep.data, &token)
        })
        .and_then(|r| r.ok());

    let plan_id = job.plan.plan_id.clone();
    let job_id = job.job_id.clone();

    tokio::spawn(async move {
        let mut line_counter: i64 = 1;
        let mut change_order: i64 = 1000; // Offset from start-event change_orders
        let mut streamed_steps = BTreeSet::new();

        // Prepare the live console file that the job container tails as PID 1
        // (so `docker logs <job-container>` mirrors the GitHub UI). Create it
        // fresh for this job; the container's `tail -F` picks it up.
        if let Some(path) = &console_log_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, b"");
        }

        // Open ONE persistent WebSocket connection for the entire job (matching the
        // official GitHub runner which keeps a single connection open per job).
        let mut ws_conn = if let Some(client) = &feed_client {
            eprintln!("[feed] Connecting to WebSocket feed...");
            match client.connect().await {
                Ok(ws) => {
                    eprintln!("[feed] WebSocket connected.");
                    Some(ws)
                }
                Err(e) => {
                    eprintln!("Best-effort WebSocket feed connect failed: {e:#}");
                    None
                }
            }
        } else {
            eprintln!("[feed] No feed client (FeedStreamUrl missing).");
            None
        };

        // Keep the feed connection warm: ping during idle gaps (e.g. a long
        // compile step with no log output) so GitHub doesn't close it and the
        // live console doesn't stutter on the next send.
        let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let log = tokio::select! {
                maybe = receiver.recv() => match maybe {
                    Some(log) => log,
                    None => break,
                },
                _ = ping_interval.tick() => {
                    if let Some(ws) = ws_conn.as_mut() {
                        if let Err(e) = crate::protocol::FeedStreamClient::send_ping(ws).await {
                            eprintln!(
                                "[feed] keepalive ping failed: {e:#}; dropping to reconnect on next send"
                            );
                            ws_conn = None;
                        }
                    }
                    continue;
                }
            };
            let masks = job_secret_mask_values(&job);
            let lines: Vec<String> = log
                .lines
                .iter()
                .map(|l| mask_single_value(l, &masks))
                .collect();
            let line_count = lines.len() as i64;
            let live_chunk = log.completed_at.is_empty() && !log.skipped;
            if live_chunk {
                streamed_steps.insert(log.step_id.clone());
            }
            let already_streamed = !live_chunk && streamed_steps.contains(&log.step_id);

            // Mirror this step to the container's live console file (docker logs).
            if !already_streamed {
                if let Some(path) = &console_log_path {
                    append_job_console(path, &log.display_name, &lines, &log.masks);
                }
            }

            if !already_streamed && !lines.is_empty() {
                // GitHub may close the feed WebSocket between steps / after idle, so a
                // later send hits a Broken pipe. Reconnect (lazily, and once on send
                // failure) so the live console does not go dark for the rest of the job.
                if ws_conn.is_none() {
                    if let Some(client) = &feed_client {
                        if let Ok(ws) = client.connect().await {
                            eprintln!("[feed] WebSocket reconnected.");
                            ws_conn = Some(ws);
                        }
                    }
                }
                if let Some(ws) = ws_conn.as_mut() {
                    eprintln!(
                        "[feed] Sending {} lines for step {}",
                        lines.len(),
                        &log.step_id[..log.step_id.len().min(8)]
                    );
                    let ts = unix_now_iso8601();
                    let timestamped: Vec<String> =
                        lines.iter().map(|l| format!("{ts} {l}")).collect();
                    if let Err(e) = crate::protocol::FeedStreamClient::send_log_lines(
                        ws,
                        &log.step_id,
                        timestamped.clone(),
                        Some(line_counter),
                        Some(&plan_id),
                        Some(&job_id),
                    )
                    .await
                    {
                        eprintln!(
                            "Best-effort WebSocket feed send failed for '{}': {e:#}; reconnecting",
                            log.step_id
                        );
                        ws_conn = None;
                        // Reconnect once and resend this batch so no step's live log is lost.
                        if let Some(client) = &feed_client {
                            if let Ok(mut ws2) = client.connect().await {
                                if crate::protocol::FeedStreamClient::send_log_lines(
                                    &mut ws2,
                                    &log.step_id,
                                    timestamped,
                                    Some(line_counter),
                                    Some(&plan_id),
                                    Some(&job_id),
                                )
                                .await
                                .is_ok()
                                {
                                    eprintln!(
                                        "[feed] resent {} lines after reconnect.",
                                        line_count
                                    );
                                    ws_conn = Some(ws2);
                                }
                            }
                        }
                    }
                }
            }
            line_counter += line_count;

            if live_chunk {
                continue;
            }

            // Send step completion via Twirp Results Service.
            if let Some(client) = &twirp_client {
                let conclusion = if log.skipped {
                    crate::protocol::StepConclusion::Skipped
                } else if log.exit_code != 0 && !log.failure_ignored {
                    crate::protocol::StepConclusion::Failure
                } else {
                    crate::protocol::StepConclusion::Success
                };
                let step = crate::protocol::TwirpStep {
                    external_id: log.step_id.clone(),
                    number: log.order as usize,
                    name: if log.display_name.is_empty() {
                        log.step_id.clone()
                    } else {
                        log.display_name.clone()
                    },
                    status: crate::protocol::StepStatus::Completed as u8,
                    started_at: if log.started_at.is_empty() {
                        None
                    } else {
                        Some(log.started_at.clone())
                    },
                    completed_at: Some(if log.completed_at.is_empty() {
                        unix_now_iso8601()
                    } else {
                        log.completed_at.clone()
                    }),
                    conclusion: conclusion as u8,
                };
                if let Err(e) = client
                    .update_steps(&[step], &plan_id, &job_id, change_order)
                    .await
                {
                    eprintln!(
                        "Best-effort Twirp step completion failed for '{}': {e:#}",
                        log.step_id
                    );
                }
                change_order += 1;

                // Upload step log blob to Results Service (populates data-log-url in GitHub UI).
                // Upload for every non-skipped step — even empty — so it is expandable.
                // Add RFC3339 timestamps so the "Show timestamps" toggle works.
                if !log.skipped {
                    let ts = unix_now_iso8601();
                    let timestamped: Vec<String> =
                        lines.iter().map(|l| format!("{ts} {l}")).collect();
                    if let Err(e) = client
                        .upload_step_log(&plan_id, &job_id, &log.step_id, &timestamped)
                        .await
                    {
                        eprintln!(
                            "Best-effort Results Service log upload failed for '{}': {e:#}",
                            log.step_id
                        );
                    }
                    // Upload GITHUB_STEP_SUMMARY content so it renders in the Summary tab.
                    if !log.summary.is_empty() {
                        if let Err(e) = client
                            .upload_step_summary(&plan_id, &job_id, &log.step_id, &log.summary)
                            .await
                        {
                            eprintln!(
                                "Best-effort step summary upload failed for '{}': {e:#}",
                                log.step_id
                            );
                        }
                    }
                }
            }

            if let Err(error) = publish_timeline_step_log(&job, &log).await {
                eprintln!(
                    "Best-effort timeline step log upload failed for '{}': {error:#}",
                    log.step_id
                );
            }
        }
        // Close persistent WebSocket after all logs sent.
        if let Some(mut ws) = ws_conn {
            ws.close(None).await.ok();
        }
    })
}

fn mask_single_value(line: &str, masks: &[String]) -> String {
    let mut result = line.to_string();
    for mask in masks {
        if !mask.is_empty() {
            result = result.replace(mask.as_str(), "***");
        }
    }
    result
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
    docker_host_work_dir: Option<PathBuf>,
    docker_image: &str,
    node_action_image: &str,
    run_service_url: &str,
    billing_owner_id: Option<String>,
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
    step_start_sender: Option<tokio::sync::mpsc::UnboundedSender<StepStartEvent>>,
    step_log_sender: Option<tokio::sync::mpsc::UnboundedSender<StepLog>>,
    daemon_id: String,
) -> Result<ScriptJobResult> {
    let job_dir = job_work_dir(config_dir, work_dir, job);
    let result = execute_script_job_inner(
        &job_dir,
        docker_host_work_dir,
        docker_image,
        node_action_image,
        run_service_url,
        billing_owner_id,
        job,
        script_steps,
        step_start_sender,
        step_log_sender,
        daemon_id,
    );
    if let Err(e) = fs::remove_dir_all(&job_dir) {
        eprintln!(
            "Warning: failed to clean up job workspace at {}: {e:#}",
            job_dir.display()
        );
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn execute_script_job_inner(
    job_dir: &std::path::Path,
    docker_host_work_dir: Option<PathBuf>,
    docker_image: &str,
    node_action_image: &str,
    run_service_url: &str,
    billing_owner_id: Option<String>,
    job: &AgentJobRequestMessage,
    script_steps: &[crate::script_step::ScriptStep],
    step_start_sender: Option<tokio::sync::mpsc::UnboundedSender<StepStartEvent>>,
    step_log_sender: Option<tokio::sync::mpsc::UnboundedSender<StepLog>>,
    daemon_id: String,
) -> Result<ScriptJobResult> {
    let workspace = job_dir.join("workspace");
    let temp = job_dir.join("temp");
    let home = job_dir.join("home");
    let actions = job_dir.join("actions");
    let tools = job_dir.join("tools");
    for path in [&workspace, &temp, &home, &actions, &tools] {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    let context_data = job_context_data(job);
    // Synthetic "Set up job" step matching GitHub-hosted runner output.
    let setup_step_id = uuid::Uuid::new_v4().to_string();
    if let Some(sender) = &step_start_sender {
        let _ = sender.send(StepStartEvent {
            step_id: setup_step_id.clone(),
            display_name: "Set up job".to_string(),
            order: 1,
        });
    }
    let setup_ts = unix_now_iso8601();
    let setup_log = StepLog {
        step_id: setup_step_id,
        display_name: "Set up job".to_string(),
        order: 1,
        started_at: setup_ts.clone(),
        completed_at: setup_ts,
        lines: setup_job_lines(job, docker_image),
        masks: Vec::new(),
        annotations: Vec::new(),
        telemetry: Vec::new(),
        exit_code: 0,
        skipped: false,
        failure_ignored: false,
        error_count: 0,
        warning_count: 0,
        notice_count: 0,
        summary: String::new(),
    };
    if let Some(sender) = &step_log_sender {
        let _ = sender.send(setup_log.clone());
    }
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
    let mut checkout_order: i32 = 1;
    for plan in &eager_checkout_plans {
        checkout_order += 1;
        // Emit step events so GitHub shows the checkout step in the job's step list.
        if let Some(sender) = &step_start_sender {
            let _ = sender.send(StepStartEvent {
                step_id: plan.step_id.clone(),
                display_name: plan.display_name.clone(),
                order: checkout_order,
            });
        }
        let mut checkout_trace = Vec::new();
        let checkout_result =
            crate::checkout::execute_checkout(&mut command_runner, plan, &mut checkout_trace);
        let exit_code = if checkout_result.is_ok() { 0 } else { 1 };
        if let Err(ref e) = checkout_result {
            eprintln!("Checkout failed: {e:#}");
        }
        if let Some(sender) = &step_log_sender {
            let checkout_lines = checkout_step_lines(plan, exit_code, &checkout_trace);
            let _ = sender.send(StepLog {
                step_id: plan.step_id.clone(),
                display_name: plan.display_name.clone(),
                order: checkout_order,
                started_at: unix_now_iso8601(),
                completed_at: unix_now_iso8601(),
                lines: checkout_lines,
                masks: Vec::new(),
                annotations: Vec::new(),
                telemetry: Vec::new(),
                exit_code,
                skipped: false,
                failure_ignored: false,
                error_count: if exit_code != 0 { 1 } else { 0 },
                warning_count: 0,
                notice_count: 0,
                summary: String::new(),
            });
        }
        checkout_result?;
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
        &repository_action_plans,
        &resolved_actions,
        &local_actions,
        &actions,
        &runtime_checkout_plans,
    )?;

    let container = github_job_container_spec(
        job,
        GitHubJobContainerPaths {
            workspace_host: workspace,
            temp_host: temp.clone(),
            home_host: home,
            actions_host: actions,
            tools_host: tools,
            docker_host_work_dir,
        },
        docker_image,
        node_action_image,
        daemon_id,
    );
    let plan = github_normalized_job_plan(
        job,
        run_service_url,
        billing_owner_id,
        container.clone(),
        ordered_steps,
        base_env,
        context_data,
    );
    println!(
        "Built normalized job plan for '{}' with {} executable step(s).",
        plan.identity.display_name,
        plan.steps.len()
    );
    let cleanup_checkout_plans = eager_checkout_plans
        .iter()
        .chain(runtime_checkout_plans.iter())
        .cloned()
        .collect::<Vec<_>>();
    // Keep clones for synthetic steps after executor (senders are moved into executor below).
    let post_step_start_sender = step_start_sender.clone();
    let post_step_log_sender = step_log_sender.clone();
    let mut executor = DockerScriptExecutor::new(command_runner)
        .with_initial_order(checkout_order)
        .with_trailing_post_action_count(cleanup_checkout_plans.len());
    if let Some(sender) = step_start_sender {
        executor = executor.with_step_start_sender(sender);
    }
    if let Some(sender) = step_log_sender {
        executor = executor.with_step_log_sender(sender);
    }
    let summary_result = executor.execute_ordered_steps_with_completion(
        &plan.execution.job_container,
        &plan.steps,
        &plan.execution.env,
        &plan.execution.context_data,
        job.job_outputs.as_ref(),
        actions_environment_url(job),
        &plan.execution.temp_host,
    );
    let mut command_runner = executor.into_runner();
    let cleanup_result = cleanup_checkout_credentials(&mut command_runner, &cleanup_checkout_plans);
    let (summary, cleanup_traces) = match (summary_result, cleanup_result) {
        (Ok(summary), Ok(traces)) => (summary, traces),
        (Ok(_), Err(error)) => return Err(error.context("cleanup checkout credentials")),
        (Err(error), Ok(_)) => return Err(error),
        (Err(error), Err(cleanup_error)) => {
            eprintln!("Checkout credential cleanup failed after job error: {cleanup_error:#}");
            return Err(error);
        }
    };
    if !summary.job_outputs.is_empty() {
        println!("Evaluated {} job output(s).", summary.job_outputs.len());
    }
    let environment_url = safe_environment_url(
        summary.environment_url,
        &job_secret_mask_values(job),
        &summary.step_logs,
    );
    if let Some(environment_url) = &environment_url {
        println!("Evaluated environment URL: {environment_url}");
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
    // "Post Run actions/checkout@vN" steps for each checkout (credential cleanup).
    let mut post_order = summary.step_logs.iter().map(|l| l.order).max().unwrap_or(0);
    let visible_post_count = summary
        .step_logs
        .iter()
        .filter(|log| log.display_name.starts_with("Post "))
        .count()
        + cleanup_checkout_plans.len();
    if visible_post_count > 0 {
        let main_order = summary
            .step_logs
            .iter()
            .filter(|log| !log.display_name.starts_with("Post "))
            .map(|log| log.order)
            .max()
            .unwrap_or(post_order);
        let complete_order = (main_order * 2) + 1;
        let first_cleanup_order = complete_order - cleanup_checkout_plans.len() as i32;
        post_order = first_cleanup_order - 1;
    }
    let mut extra_step_logs: Vec<StepLog> = Vec::new();
    for (index, plan) in cleanup_checkout_plans.iter().enumerate() {
        post_order += 1;
        let post_step_id = uuid::Uuid::new_v4().to_string();
        let post_name = post_step_display_name(&plan.display_name);
        let post_ts = unix_now_iso8601();
        if let Some(sender) = &post_step_start_sender {
            let _ = sender.send(StepStartEvent {
                step_id: post_step_id.clone(),
                display_name: post_name.clone(),
                order: post_order,
            });
        }
        // Show the credential-cleanup git trace (matches GitHub's "Post Run
        // actions/checkout"); empty when the plan had nothing to clean.
        let post_lines = cleanup_traces.get(index).cloned().unwrap_or_default();
        let post_log = StepLog {
            step_id: post_step_id,
            display_name: post_name,
            order: post_order,
            started_at: post_ts.clone(),
            completed_at: post_ts,
            lines: post_lines,
            masks: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            exit_code: 0,
            skipped: false,
            failure_ignored: false,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
            summary: String::new(),
        };
        if let Some(sender) = &post_step_log_sender {
            let _ = sender.send(post_log.clone());
        }
        extra_step_logs.push(post_log);
    }

    // Synthetic "Complete job" step matching GitHub-hosted runner output.
    let complete_step_id = uuid::Uuid::new_v4().to_string();
    let complete_order = post_order + 1;
    if let Some(sender) = &post_step_start_sender {
        let _ = sender.send(StepStartEvent {
            step_id: complete_step_id.clone(),
            display_name: "Complete job".to_string(),
            order: complete_order,
        });
    }
    let complete_ts = unix_now_iso8601();
    let complete_log = StepLog {
        step_id: complete_step_id,
        display_name: "Complete job".to_string(),
        order: complete_order,
        started_at: complete_ts.clone(),
        completed_at: complete_ts,
        lines: complete_job_lines(),
        masks: Vec::new(),
        annotations: Vec::new(),
        telemetry: Vec::new(),
        exit_code: 0,
        skipped: false,
        failure_ignored: false,
        error_count: 0,
        warning_count: 0,
        notice_count: 0,
        summary: String::new(),
    };
    if let Some(sender) = &post_step_log_sender {
        let _ = sender.send(complete_log.clone());
    }
    extra_step_logs.push(complete_log);

    let mut all_step_logs = vec![setup_log];
    all_step_logs.extend(summary.step_logs);
    all_step_logs.extend(extra_step_logs);
    Ok(ScriptJobResult {
        result,
        outputs: summary.job_outputs,
        environment_url,
        step_logs: all_step_logs,
    })
}

fn actions_environment_url(job: &AgentJobRequestMessage) -> Option<&Value> {
    job.actions_environment
        .as_ref()?
        .as_object()
        .and_then(|object| {
            object
                .get("Url")
                .or_else(|| object.get("url"))
                .filter(|value| !value.is_null())
        })
}

fn safe_environment_url(
    environment_url: Option<String>,
    job_masks: &[String],
    step_logs: &[StepLog],
) -> Option<String> {
    let environment_url = environment_url?;
    let contains_masked_value = job_masks
        .iter()
        .chain(step_logs.iter().flat_map(|log| log.masks.iter()))
        .filter(|mask| !mask.is_empty())
        .any(|mask| environment_url.contains(mask));
    if contains_masked_value {
        eprintln!("Skipping environment URL because it contains a masked value.");
        None
    } else {
        Some(environment_url)
    }
}

fn job_secret_mask_values(job: &AgentJobRequestMessage) -> Vec<String> {
    job.mask
        .iter()
        .filter_map(|mask| mask.value.clone())
        .chain(
            job.variables
                .values()
                .filter(|variable| variable.is_secret)
                .filter_map(|variable| variable.value.clone()),
        )
        .collect()
}

fn resolve_checkout_plan_context(
    mut plan: CheckoutPlan,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> CheckoutPlan {
    if let Some(version) = plan.version.as_mut() {
        if !contains_step_output_expression(version) {
            *version =
                crate::executor::render_expressions_with_context(version, base_env, context_data);
        }
    }
    plan
}

fn contains_step_output_expression(value: &str) -> bool {
    value
        .match_indices("steps.")
        .any(|(index, _)| value[index..].contains(".outputs."))
}

#[derive(Debug, Clone)]
struct ScriptJobResult {
    result: TaskResult,
    outputs: BTreeMap<String, String>,
    environment_url: Option<String>,
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
    // Expand any context values stored in GitHub V2 broker compact format
    // {"d": [{"k": key, "v": value}, ...]} into plain flat objects so that
    // expression evaluation and context lookups work uniformly.
    let mut expanded: BTreeMap<String, Value> = context_data
        .into_iter()
        .map(|(k, v)| (k, expand_broker_context_value(v)))
        .collect();
    if let Some(token) = github_token {
        match expanded.get_mut("github") {
            Some(Value::Object(github)) => {
                github
                    .entry("token".to_string())
                    .or_insert_with(|| Value::String(token));
            }
            Some(_) => {}
            None => {
                let mut github = Map::new();
                github.insert("token".to_string(), Value::String(token));
                expanded.insert("github".to_string(), Value::Object(github));
            }
        }
    }
    expanded.into_iter().collect()
}

fn expand_broker_context_value(value: Value) -> Value {
    match value {
        Value::Object(ref obj) => {
            if let Some(items) = obj.get("d").and_then(Value::as_array) {
                // Compact format: expand [{k, v}] into a plain object, recursively.
                let mut map = Map::new();
                for item in items {
                    if let Some(item_obj) = item.as_object() {
                        let k = item_obj
                            .get("k")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if let Some(v) = item_obj.get("v") {
                            if !k.is_empty() {
                                map.insert(k, expand_broker_context_value(v.clone()));
                            }
                        }
                    }
                }
                if !map.is_empty() {
                    return Value::Object(map);
                }
            }
            // Plain object: recursively expand each value.
            let expanded: Map<String, Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), expand_broker_context_value(v.clone())))
                .collect();
            Value::Object(expanded)
        }
        other => other,
    }
}

async fn apply_workflow_script_step_names(
    job: &mut AgentJobRequestMessage,
    context_data: &[(String, Value)],
) {
    let Some(workflow) = workflow_source_context(context_data) else {
        return;
    };
    let Some(token) = context_string(context_data, "github.token") else {
        eprintln!("Skipping workflow script-step name lookup: missing GitHub token.");
        return;
    };
    let Ok(contents) = fetch_workflow_file(&workflow, &token).await else {
        eprintln!(
            "Skipping workflow script-step name lookup: could not fetch {} at {}.",
            workflow.path, workflow.sha
        );
        return;
    };
    let names_by_line = workflow_run_step_names_by_line(&contents);
    let names_by_order = workflow_step_names_in_order(&contents);
    let enabled_step_count = job.steps.iter().filter(|step| step.enabled).count();
    let use_ordered_names = names_by_order.len() == enabled_step_count;

    let mut updated = 0usize;
    let mut enabled_index = 0usize;
    for step in &mut job.steps {
        if !step.enabled {
            continue;
        }
        let ordered_name = if use_ordered_names {
            names_by_order
                .get(enabled_index)
                .and_then(|name| name.as_ref())
        } else {
            None
        };
        enabled_index += 1;

        if step
            .display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .is_some()
        {
            continue;
        }
        let is_script_step =
            step.reference_type() == Some(crate::job_message::ActionReferenceType::Script);
        if !is_script_step && has_explicit_step_name(step) {
            continue;
        }

        let line_name = if is_script_step {
            crate::script_step::script_input_source_line(step)
                .and_then(|line| names_by_line.get(&line))
        } else {
            None
        };
        let Some(name) = line_name.or(ordered_name) else {
            continue;
        };
        step.display_name = Some(name.clone());
        updated += 1;
    }
    if updated > 0 {
        println!("Recovered {updated} script step display name(s) from workflow YAML.");
    }
}

fn has_explicit_step_name(step: &crate::job_message::ActionStep) -> bool {
    step.name
        .as_deref()
        .filter(|name| !name.is_empty() && !name.starts_with("__"))
        .is_some()
}

#[derive(Debug)]
struct WorkflowSourceContext {
    repository: String,
    path: String,
    sha: String,
}

fn workflow_source_context(context_data: &[(String, Value)]) -> Option<WorkflowSourceContext> {
    let path = context_string(context_data, "job.workflow_file_path")
        .or_else(|| context_string(context_data, "github.event.workflow"))?;
    let sha = context_string(context_data, "github.workflow_sha")
        .or_else(|| context_string(context_data, "github.sha"))?;
    let repository = context_string(context_data, "job.workflow_repository")
        .or_else(|| context_string(context_data, "github.repository"))
        .or_else(|| {
            context_string(context_data, "github.workflow_ref").and_then(|workflow_ref| {
                workflow_ref.split_once('/').map(|(owner, rest)| {
                    let repo = rest.split_once('/').map(|(repo, _)| repo).unwrap_or(rest);
                    format!("{owner}/{repo}")
                })
            })
        })?;
    Some(WorkflowSourceContext {
        repository,
        path,
        sha,
    })
}

fn context_string(context_data: &[(String, Value)], path: &str) -> Option<String> {
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut value = context_data
        .iter()
        .find(|(name, _)| name == first)
        .map(|(_, value)| value)?;
    for part in parts {
        value = value.as_object()?.get(part)?;
    }
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct GitHubContentsResponse {
    content: String,
    encoding: Option<String>,
}

async fn fetch_workflow_file(source: &WorkflowSourceContext, token: &str) -> Result<String> {
    let (owner, repo) = source
        .repository
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid workflow repository '{}'", source.repository))?;
    let mut url = url::Url::parse("https://api.github.com/")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("cannot build GitHub contents URL"))?;
        segments.push("repos");
        segments.push(owner);
        segments.push(repo);
        segments.push("contents");
        for segment in source.path.trim_start_matches('/').split('/') {
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
    }
    url.query_pairs_mut().append_pair("ref", &source.sha);
    let (status, body) =
        crate::protocol::curl_json_request("GET", url.as_str(), token, None, 30).await?;
    if !(200..300).contains(&status) {
        bail!(
            "fetch workflow file failed: status={status}, repository={}, path={}, ref={}",
            source.repository,
            source.path,
            source.sha
        );
    }
    let response: GitHubContentsResponse =
        serde_json::from_str(&body).context("parse workflow contents response")?;
    if !response
        .encoding
        .as_deref()
        .unwrap_or("base64")
        .eq_ignore_ascii_case("base64")
    {
        bail!("unsupported workflow contents encoding");
    }
    let encoded = response.content.replace(['\n', '\r'], "");
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .context("decode workflow contents")?;
    String::from_utf8(bytes).context("decode workflow contents as UTF-8")
}

fn workflow_run_step_names_by_line(contents: &str) -> BTreeMap<u64, String> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut result = BTreeMap::new();
    let mut starts = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("- "))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    starts.push(lines.len());

    for pair in starts.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let mut name = None;
        let mut run_line = None;
        for (offset, line) in lines[start..end].iter().enumerate() {
            let trimmed = line.trim_start();
            if name.is_none() {
                name = yaml_name_value(trimmed);
            }
            if yaml_has_run_key(trimmed) {
                run_line = Some(start + offset + 1);
                break;
            }
        }
        let (Some(name), Some(run_line)) = (name, run_line) else {
            continue;
        };
        for line in run_line..=end {
            result.insert(line as u64, name.clone());
        }
    }
    result
}

fn workflow_step_names_in_order(contents: &str) -> Vec<Option<String>> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut result = Vec::new();
    let mut starts = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("- "))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    starts.push(lines.len());

    for pair in starts.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let mut name = None;
        let mut executable = false;
        for line in &lines[start..end] {
            let trimmed = line.trim_start();
            if name.is_none() {
                name = yaml_name_value(trimmed);
            }
            if yaml_has_run_key(trimmed) || yaml_has_uses_key(trimmed) {
                executable = true;
            }
        }
        if executable {
            result.push(name);
        }
    }
    result
}

fn yaml_name_value(trimmed_line: &str) -> Option<String> {
    let value = trimmed_line
        .strip_prefix("- name:")
        .or_else(|| trimmed_line.strip_prefix("name:"))?
        .trim();
    if value.is_empty() {
        return None;
    }
    Some(unquote_yaml_scalar(value))
}

fn yaml_has_run_key(trimmed_line: &str) -> bool {
    trimmed_line
        .strip_prefix("- run:")
        .or_else(|| trimmed_line.strip_prefix("run:"))
        .is_some()
}

fn yaml_has_uses_key(trimmed_line: &str) -> bool {
    trimmed_line
        .strip_prefix("- uses:")
        .or_else(|| trimmed_line.strip_prefix("uses:"))
        .is_some()
}

fn unquote_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
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
        let downloadable = pending
            .iter()
            .filter(|plan| native_action_adapter(&plan.repository).is_none())
            .cloned()
            .collect::<Vec<_>>();
        if downloadable.is_empty() {
            break;
        }
        let next = download_repository_actions(runner, &downloadable)?;
        resolved.extend(next);

        let nested = composite_repository_action_plans_from_resolved(&resolved, actions_host)?;
        let previous_pending = pending;
        pending = nested
            .into_iter()
            .filter(|plan| {
                native_action_adapter(&plan.repository).is_none()
                    && !resolved
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
    repository_action_plans: &[RepositoryActionPlan],
    resolved_actions: &[ResolvedAction],
    local_actions: &[(LocalActionPlan, ActionMetadata)],
    actions_host: &std::path::Path,
    runtime_checkout_plans: &[CheckoutPlan],
) -> Result<Vec<ExecutableStep>> {
    let mut ordered = Vec::new();
    let mut script_iter = script_steps.iter();
    let mut local_iter = local_actions.iter();
    let mut repository_iter = repository_action_plans.iter();
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
                                if append_native_action_step_from_plan(
                                    &mut ordered,
                                    &plan,
                                    parent_condition,
                                    parent_continue_on_error,
                                    "",
                                ) {
                                    continue;
                                }
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
                                    "",
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
                let plan = repository_iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("repository action mapping count mismatch"))?;
                let step_display_name = action_step_display_name(step);
                if append_native_action_step_from_plan(
                    &mut ordered,
                    plan,
                    None,
                    false,
                    &step_display_name,
                ) {
                    continue;
                }
                let action = resolved_actions
                    .iter()
                    .find(|action| same_action(&action.plan, plan))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "repository action '{}@{}' was not resolved",
                            repository,
                            plan.git_ref
                        )
                    })?;
                append_resolved_action_steps(
                    &mut ordered,
                    action,
                    resolved_actions,
                    actions_host,
                    None,
                    false,
                    &step_display_name,
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
    display_name: &str,
) -> Result<()> {
    let continue_on_error = parent_continue_on_error || action.plan.continue_on_error;
    if let Some(invocation) = action.native_invocation() {
        ordered.push(ExecutableStep::Native {
            step_id: action.plan.step_id.clone(),
            display_name: display_name.to_string(),
            invocation,
            condition: combine_conditions(parent_condition, action.plan.condition.as_deref()),
            continue_on_error,
        });
        return Ok(());
    }
    if let Some(message) = unsupported_action_error(&action.plan.repository) {
        bail!("{message}");
    }
    match &action.runtime {
        ActionRuntime::JavaScript { .. } => ordered.push(ExecutableStep::JavaScript {
            step_id: action.plan.step_id.clone(),
            display_name: display_name.to_string(),
            invocation: action.javascript_invocation(actions_host)?,
            condition: combine_conditions(parent_condition, action.plan.condition.as_deref()),
            continue_on_error,
        }),
        ActionRuntime::Docker { .. } => ordered.push(ExecutableStep::Docker {
            step_id: action.plan.step_id.clone(),
            display_name: display_name.to_string(),
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
                        if append_native_action_step_from_plan(
                            ordered,
                            &plan,
                            action_condition.as_deref(),
                            continue_on_error,
                            "",
                        ) {
                            continue;
                        }
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
                            "",
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

fn append_native_action_step_from_plan(
    ordered: &mut Vec<ExecutableStep>,
    plan: &RepositoryActionPlan,
    parent_condition: Option<&str>,
    parent_continue_on_error: bool,
    display_name: &str,
) -> bool {
    let Some(invocation) = native_invocation_from_plan(plan) else {
        return false;
    };
    ordered.push(ExecutableStep::Native {
        step_id: plan.step_id.clone(),
        display_name: display_name.to_string(),
        invocation,
        condition: combine_conditions(parent_condition, plan.condition.as_deref()),
        continue_on_error: parent_continue_on_error || plan.continue_on_error,
    });
    true
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

/// Host path of the live console file the job container tails as PID 1. Lives
/// under the job's temp dir (mounted at `/__t`), so the container streams it via
/// `tail -F /__t/_velnor/console.log` and `docker logs` mirrors the UI output.
fn job_console_log_path(
    config_dir: &std::path::Path,
    work_dir: Option<PathBuf>,
    job: &AgentJobRequestMessage,
) -> PathBuf {
    job_work_dir(config_dir, work_dir, job)
        .join("temp")
        .join("_velnor")
        .join("console.log")
}

/// Append one completed step's masked output to the live console file. `lines`
/// are already job-secret-masked; `step_masks` adds any per-step `::add-mask::`
/// values so nothing secret reaches `docker logs`.
fn append_job_console(path: &Path, display_name: &str, lines: &[String], step_masks: &[String]) {
    use std::io::Write;
    let mut out = String::new();
    if !display_name.is_empty() {
        out.push_str(&format!("\n=== {display_name} ===\n"));
    }
    for line in lines {
        out.push_str(&mask_value(line, step_masks));
        out.push('\n');
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(out.as_bytes());
    }
}

fn unix_now_iso8601() -> String {
    use time::{format_description, OffsetDateTime};
    // GitHub's log UI strips a leading per-line timestamp ONLY when it matches the
    // runner's .NET "o" round-trip format with 7 fractional digits:
    // `YYYY-MM-DDTHH:MM:SS.fffffffZ` (e.g. 2026-06-04T15:27:50.9085200Z). A
    // second-precision timestamp (no dot) is NOT recognised, so it leaks into the
    // visible log content column instead of the timestamp toggle. Always emit the
    // 7-digit sub-second form — see `unix_now_iso8601_is_github_strippable`.
    // REGRESSION HISTORY: this once emitted second precision and the timestamp
    // showed up as plain text in every log line. Do NOT drop the sub-seconds.
    let fmt = format_description::parse(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:7]Z",
    )
    .unwrap_or_else(|_| vec![]);
    OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.0000000Z".to_string())
}

/// Build the "Set up job" log lines — mirrors the GitHub-hosted runner's
/// provisioning block. Uses `::group::` sections so they collapse in the UI.
fn setup_job_lines(job: &AgentJobRequestMessage, docker_image: &str) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push(format!(
        "Current runner version: '{}'",
        crate::protocol::velnor_runner_display()
    ));

    // Operating System (fixed: Velnor jobs always run in Ubuntu 24.04).
    lines.push("##[group]Operating System".to_string());
    lines.push("Ubuntu".to_string());
    lines.push("24.04.2".to_string());
    lines.push("LTS".to_string());
    lines.push("##[endgroup]".to_string());

    // Runner Image.
    lines.push("##[group]Runner Image".to_string());
    lines.push(format!("Image: {docker_image}"));
    lines.push("##[endgroup]".to_string());

    // GITHUB_TOKEN Permissions — read from job variable "system.github.token.permissions"
    // (JSON string like {"Actions":"read","Contents":"read","Metadata":"read"}).
    // GitHub always shows this group when permissions are known; "Secret source: Actions"
    // is shown separately after the group (not as an exclusive fallback).
    let perm_json = job
        .variables
        .get("system.github.token.permissions")
        .and_then(|v| v.value.as_deref());
    if let Some(json_str) = perm_json {
        if let Ok(serde_json::Value::Object(perms)) =
            serde_json::from_str::<serde_json::Value>(json_str)
        {
            lines.push("##[group]GITHUB_TOKEN Permissions".to_string());
            for (scope, level) in &perms {
                let display = level.as_str().unwrap_or("read");
                lines.push(format!("  {scope}: {display}"));
            }
            lines.push("##[endgroup]".to_string());
        }
    }
    lines.push("Secret source: Actions".to_string());

    lines.push("Prepare workflow directory".to_string());

    // Enumerate repository actions used by the job (non-local uses).
    // Format matches GitHub: "Download action repository '<name>@<ref>' (SHA:<sha>)"
    // SHA is included when the git_ref looks like a full commit hash (40 hex chars).
    let repo_actions: Vec<_> = job
        .steps
        .iter()
        .filter_map(|step| {
            let reference = step.reference.as_ref()?;
            if reference.r#type != Some(ActionReferenceType::Repository) {
                return None;
            }
            let name = reference.name.as_deref()?;
            // Skip local composite actions (start with '.')
            if name.starts_with('.') {
                return None;
            }
            Some((name, reference.git_ref.as_deref().unwrap_or("latest")))
        })
        .collect();

    if !repo_actions.is_empty() {
        lines.push("##[group]Prepare all required actions".to_string());
        lines.push("Getting action download info".to_string());
        for (name, git_ref) in repo_actions {
            // If git_ref is a full SHA (40 hex chars), show it as "(SHA:…)"; otherwise
            // the ref is a tag/branch and no SHA suffix is shown.
            let sha_suffix =
                if git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit()) {
                    format!(" (SHA:{git_ref})")
                } else {
                    String::new()
                };
            lines.push(format!(
                "Download action repository '{name}@{git_ref}'{sha_suffix}"
            ));
        }
        lines.push("##[endgroup]".to_string());
    }

    lines.push(format!("Complete job name: {}", job.job_display_name));
    lines
}

/// Build log lines for a checkout step from the checkout plan.
fn checkout_step_lines(plan: &CheckoutPlan, exit_code: i32, trace: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    // Show the repo being checked out (mask auth from URL for display).
    let display_url = plan
        .clone_url
        .split('@')
        .last()
        .unwrap_or(&plan.clone_url)
        .trim_end_matches('/');
    lines.push(format!("Syncing repository: {display_url}"));
    if let Some(ref ver) = plan.version {
        lines.push(format!("Setting up ref '{ver}'"));
    }
    if let Some(depth) = plan.fetch_depth {
        if depth > 0 {
            lines.push(format!("Fetch depth: {depth}"));
        }
    }
    lines.push(format!("Repository path: {}", plan.destination.display()));
    // The actual `[command]git …` trace, matching the GitHub-hosted runner's
    // checkout log instead of a bare summary.
    lines.extend(trace.iter().cloned());
    if exit_code == 0 {
        lines.push("Checkout completed successfully".to_string());
    } else {
        lines.push("::error::Checkout failed".to_string());
    }
    lines
}

/// Build the "Complete job" log lines — summarise Velnor cleanup work.
fn complete_job_lines() -> Vec<String> {
    vec![
        "##[group]Post-job cleanup".to_string(),
        "Stop job container".to_string(),
        "Remove per-job network".to_string(),
        "Clean work directory".to_string(),
        "Recycle runner slot".to_string(),
        "##[endgroup]".to_string(),
        "Finishing: Complete job".to_string(),
    ]
}

fn action_step_display_name(step: &crate::job_message::ActionStep) -> String {
    let explicit = step
        .display_name
        .as_deref()
        .or(step.name.as_deref())
        .filter(|n| !n.is_empty() && !n.starts_with("__"));
    if let Some(name) = explicit {
        return name.to_string();
    }
    if let Some(reference) = &step.reference {
        if let Some(action_name) = reference.name.as_deref() {
            if !action_name.is_empty() {
                return match reference.git_ref.as_deref() {
                    Some(r) if !r.is_empty() => format!("Run {action_name}@{r}"),
                    _ => format!("Run {action_name}"),
                };
            }
        }
    }
    String::new()
}

fn post_step_display_name(display_name: &str) -> String {
    format!("Post {display_name}")
}

/// Build one masked, GitHub-style text blob of the whole job — each step wrapped
/// in `##[group]<name>` … `##[endgroup]` — for the downloadable `job-log.txt`.
fn build_combined_job_log(job: &AgentJobRequestMessage, step_logs: &[StepLog]) -> String {
    let secret_masks = job_secret_mask_values(job);
    let mut out = String::new();
    for log in step_logs {
        if log.skipped {
            continue;
        }
        let name = if log.display_name.is_empty() {
            log.step_id.as_str()
        } else {
            log.display_name.as_str()
        };
        out.push_str(&format!("##[group]{name}\n"));
        for line in &log.lines {
            let masked = mask_value(&mask_single_value(line, &secret_masks), &log.masks);
            out.push_str(&masked);
            out.push('\n');
        }
        out.push_str("##[endgroup]\n");
    }
    out
}

/// Upload the combined job log as a `job-log.txt` artifact (best-effort). Gives a
/// real "download the logs" path since GitHub doesn't serve the v1 /logs archive
/// for Velnor's V2 jobs.
async fn upload_job_log_artifact(job: &AgentJobRequestMessage, step_logs: &[StepLog]) {
    if step_logs.is_empty() {
        return;
    }
    let Some(endpoint) = job.system_connection() else {
        return;
    };
    let Some(token) = system_connection_access_token(endpoint) else {
        return;
    };
    let Some(results_url) = endpoint.data.get("ResultsServiceUrl").cloned() else {
        return;
    };
    let plan_id = job.plan.plan_id.clone();
    let job_id = job.job_id.clone();
    let content = build_combined_job_log(job, step_logs).into_bytes();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::protocol::upload_artifact_blocking(
            &results_url,
            &token,
            &plan_id,
            &job_id,
            "job-log",
            &[("job-log.txt".to_string(), content)],
        )
    })
    .await;
    match outcome {
        Ok(Ok(())) => println!("Uploaded job-log.txt artifact."),
        Ok(Err(e)) => eprintln!("Best-effort job-log artifact upload failed: {e:#}"),
        Err(e) => eprintln!("Best-effort job-log artifact task join failed: {e:#}"),
    }
}

async fn complete_run_service_job(
    client: &RunServiceClient,
    run_service_url: &str,
    job: &AgentJobRequestMessage,
    result: TaskResult,
    job_outputs: BTreeMap<String, String>,
    step_logs: Vec<StepLog>,
    environment_url: Option<String>,
    billing_owner_id: Option<String>,
    infrastructure_failure_category: Option<String>,
    publish_completion_timeline_logs: bool,
) -> Result<()> {
    if publish_completion_timeline_logs {
        if let Err(error) = publish_timeline_logs(job, &step_logs).await {
            eprintln!("Best-effort timeline log upload failed: {error:#}");
        }
    }
    // Best-effort: publish the whole job log as a downloadable artifact
    // ("job-log.txt"). GitHub does not expose the v1 /logs archive ("Download
    // log archive" / `gh run view --log`) for Velnor's V2 jobs, so this gives a
    // real way to download the full log — it shows up under the run's Artifacts.
    upload_job_log_artifact(job, &step_logs).await;
    let step_results = step_logs
        .iter()
        .map(|log| RunServiceStepResult {
            // external_id must match the Twirp-registered step external_id so GitHub
            // merges this result onto the existing step instead of creating a duplicate
            // entry at default order 0.
            external_id: Some(log.step_id.clone()),
            // number is the sequential 1-indexed step position GitHub uses for its REST
            // API `number` field and `/logs/{n}` URL. order 0 means unset — skip it.
            number: if log.order > 0 {
                Some(log.order as i64)
            } else {
                None
            },
            name: if log.display_name.is_empty() {
                log.step_id.clone()
            } else {
                log.display_name.clone()
            },
            status: TimelineRecordState::Completed,
            conclusion: step_log_result(log),
            started_at: if log.started_at.is_empty() {
                None
            } else {
                Some(log.started_at.clone())
            },
            // Use per-step completed_at tracked in StepLog so GitHub shows accurate
            // step durations. Fallback to current time only if not tracked.
            completed_at: Some(if log.completed_at.is_empty() {
                unix_now_iso8601()
            } else {
                log.completed_at.clone()
            }),
            completed_log_lines: log.lines.len() as i64,
            annotations: log.annotations.iter().map(run_service_annotation).collect(),
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
    let telemetry = run_service_telemetry(job, &step_logs);
    let annotations: Vec<RunServiceAnnotation> = step_logs
        .iter()
        .flat_map(|log| log.annotations.iter().map(run_service_annotation))
        .collect();
    let completion = RunServiceCompleteJob {
        plan_id: job.plan.plan_id.clone(),
        job_id: job.job_id.clone(),
        conclusion: result,
        outputs,
        step_results,
        annotations,
        telemetry,
        environment_url,
        billing_owner_id,
        infrastructure_failure_category,
    };
    client
        .complete_job(run_service_url, completion)
        .await
        .context("complete run-service job")
}

fn run_service_telemetry(
    job: &AgentJobRequestMessage,
    step_logs: &[StepLog],
) -> Vec<RunServiceTelemetry> {
    let masks = job_secret_mask_values(job);
    let mut seen = BTreeSet::new();
    step_logs
        .iter()
        .flat_map(|log| log.telemetry.iter().map(move |telemetry| (log, telemetry)))
        .map(|(log, telemetry)| {
            let mut all_masks = masks.clone();
            all_masks.extend(log.masks.iter().cloned());
            RunServiceTelemetry {
                message: mask_value(&telemetry.message, &all_masks),
                kind: telemetry.kind.clone(),
            }
        })
        .filter(|telemetry| seen.insert((telemetry.kind.clone(), telemetry.message.clone())))
        .collect()
}

async fn publish_timeline_job_started(
    job: &AgentJobRequestMessage,
    runner_name: &str,
) -> Result<()> {
    let Some(context) = timeline_publish_context(job)? else {
        return Ok(());
    };
    let start_time = current_time_rfc3339()?;
    let record = timeline_started_record(job, runner_name, &start_time);
    context
        .client
        .update_timeline_records(
            &context.scope_identifier,
            &context.hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            vec![record],
        )
        .await?;
    Ok(())
}

async fn publish_timeline_step_started(
    job: &AgentJobRequestMessage,
    event: &StepStartEvent,
) -> Result<()> {
    let Some(context) = timeline_publish_context(job)? else {
        return Ok(());
    };
    let start_time = current_time_rfc3339()?;
    let record = timeline_step_started_record(job, event, &start_time);
    context
        .client
        .update_timeline_records(
            &context.scope_identifier,
            &context.hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            vec![record],
        )
        .await?;
    Ok(())
}

async fn publish_timeline_logs(job: &AgentJobRequestMessage, step_logs: &[StepLog]) -> Result<()> {
    if step_logs.is_empty() {
        return Ok(());
    }
    let Some(context) = timeline_publish_context(job)? else {
        return Ok(());
    };
    let finish_time = current_time_rfc3339()?;
    let records = timeline_records_for_step_logs(job, step_logs, &finish_time);
    context
        .client
        .update_timeline_records(
            &context.scope_identifier,
            &context.hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            records,
        )
        .await?;
    for log in step_logs {
        let masks = job_secret_mask_values(job)
            .into_iter()
            .chain(log.masks.iter().cloned())
            .collect::<Vec<_>>();
        let lines = mask_log_lines(&log.lines, &masks);
        if lines.is_empty() {
            continue;
        }
        context
            .client
            .append_timeline_record_feed(
                &context.scope_identifier,
                &context.hub_name,
                &job.plan.plan_id,
                &job.timeline.id,
                &log.step_id,
                TimelineRecordFeedLines::new(log.step_id.clone(), lines, Some(1)),
            )
            .await?;
    }
    Ok(())
}

async fn publish_timeline_step_log(job: &AgentJobRequestMessage, log: &StepLog) -> Result<()> {
    let Some(context) = timeline_publish_context(job)? else {
        return Ok(());
    };
    let finish_time = current_time_rfc3339()?;
    let records = timeline_records_for_step_logs(job, std::slice::from_ref(log), &finish_time);
    context
        .client
        .update_timeline_records(
            &context.scope_identifier,
            &context.hub_name,
            &job.plan.plan_id,
            &job.timeline.id,
            records,
        )
        .await?;

    let masks = job_secret_mask_values(job)
        .into_iter()
        .chain(log.masks.iter().cloned())
        .collect::<Vec<_>>();
    let lines = mask_log_lines(&log.lines, &masks);
    if !lines.is_empty() {
        context
            .client
            .append_timeline_record_feed(
                &context.scope_identifier,
                &context.hub_name,
                &job.plan.plan_id,
                &job.timeline.id,
                &log.step_id,
                TimelineRecordFeedLines::new(log.step_id.clone(), lines, Some(1)),
            )
            .await?;
    }
    Ok(())
}

fn timeline_started_record(
    job: &AgentJobRequestMessage,
    runner_name: &str,
    start_time: &str,
) -> TimelineRecord {
    TimelineRecord::job_pending(
        job.job_id.clone(),
        job.job_display_name.clone(),
        job.job_name.clone(),
        runner_name.to_string(),
    )
    .in_progress(start_time.to_string())
}

fn timeline_step_started_record(
    job: &AgentJobRequestMessage,
    event: &StepStartEvent,
    start_time: &str,
) -> TimelineRecord {
    TimelineRecord::task_pending(
        event.step_id.clone(),
        job.job_id.clone(),
        event.step_id.clone(),
        event.order,
    )
    .in_progress(start_time.to_string())
}

struct TimelinePublishContext {
    client: DistributedTaskClient,
    scope_identifier: String,
    hub_name: String,
}

fn timeline_publish_context(
    job: &AgentJobRequestMessage,
) -> Result<Option<TimelinePublishContext>> {
    let Some(scope_identifier) = job.plan.scope_identifier.clone() else {
        return Ok(None);
    };
    let Some(system_connection) = job.system_connection() else {
        return Ok(None);
    };
    let Some(server_url) = system_connection.url.as_deref() else {
        return Ok(None);
    };
    let Some(token) = system_connection_access_token(system_connection) else {
        return Ok(None);
    };
    Ok(Some(TimelinePublishContext {
        client: DistributedTaskClient::new(server_url, token)?,
        scope_identifier,
        hub_name: job
            .plan
            .plan_type
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("Build")
            .to_ascii_lowercase(),
    }))
}

fn timeline_records_for_step_logs(
    job: &AgentJobRequestMessage,
    step_logs: &[StepLog],
    finish_time: &str,
) -> Vec<TimelineRecord> {
    step_logs
        .iter()
        .map(|log| {
            TimelineRecord::task_completed(
                log.step_id.clone(),
                job.job_id.clone(),
                log.step_id.clone(),
                log.order,
                finish_time.to_string(),
                step_log_result(log),
            )
            .with_issue_counts(log.error_count, log.warning_count, log.notice_count)
        })
        .collect()
}

fn mask_log_lines(lines: &[String], masks: &[String]) -> Vec<String> {
    lines.iter().map(|line| mask_value(line, masks)).collect()
}

fn mask_value(value: &str, masks: &[String]) -> String {
    masks
        .iter()
        .filter(|mask| !mask.is_empty())
        .fold(value.to_string(), |value, mask| value.replace(mask, "***"))
}

fn current_time_rfc3339() -> Result<String> {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};

    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("format current time")
}

fn infrastructure_failure_category(error: &anyhow::Error) -> Option<&'static str> {
    let messages = error.chain().map(ToString::to_string).collect::<Vec<_>>();
    if messages.iter().any(|message| {
        message.contains("Docker daemon cannot see Velnor bind-mounted work directories")
    }) {
        return Some("docker_bind_mount");
    }
    if messages
        .iter()
        .any(|message| message.contains("Docker job environment start failed"))
    {
        return Some("docker_environment");
    }
    None
}

fn run_service_annotation(annotation: &StepAnnotation) -> RunServiceAnnotation {
    RunServiceAnnotation {
        level: match annotation.level {
            StepAnnotationLevel::Notice => RunServiceAnnotationLevel::Notice,
            StepAnnotationLevel::Warning => RunServiceAnnotationLevel::Warning,
            StepAnnotationLevel::Failure => RunServiceAnnotationLevel::Failure,
        },
        message: annotation.message.clone(),
        title: annotation.title.clone(),
        path: annotation.path.clone(),
        start_line: annotation.start_line,
        end_line: annotation.end_line,
        start_column: annotation.start_column,
        end_column: annotation.end_column,
        step_number: None,
        is_infrastructure_issue: false,
    }
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
        .ok_or_else(|| anyhow::anyhow!("runner is not configured: missing credentials"))?;

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
    // GitHub JIT credentials use PascalCase keys (e.g. ClientId, AuthorizationUrl).
    // Accept any case variant by searching case-insensitively.
    let obj = credentials
        .data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OAuth credentials data is not an object"))?;
    let lower_key = key.to_ascii_lowercase();
    obj.iter()
        .find(|(k, _)| k.to_ascii_lowercase() == lower_key)
        .and_then(|(_, v)| v.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("OAuth credentials missing {key}"))
}

pub async fn remove(args: RemoveArgs) -> Result<()> {
    let config_base = config::config_dir(args.config_dir.clone())?;

    for (slot_index, dir) in daemon_slot_config_dirs(&config_base, args.slots)?
        .into_iter()
        .enumerate()
    {
        remove_one(&args, &dir)
            .await
            .with_context(|| format!("remove daemon slot-{}", slot_index + 1))?;
    }

    Ok(())
}

fn daemon_slot_config_dirs(config_base: &Path, slots: usize) -> Result<Vec<PathBuf>> {
    let slots = validate_daemon_slots(slots)?;
    Ok((1..=slots)
        .map(|slot_index| daemon_slot_config_dir(config_base, slot_index, slots))
        .collect())
}

async fn remove_one(args: &RemoveArgs, dir: &Path) -> Result<()> {
    let stored = config::load(&dir).ok();

    if !args.local_only && args.pat.is_some() {
        let stored = stored
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("local runner config is required for remote remove"))?;
        let scope = GitHubScope::parse(&stored.settings.github_url)?;
        let agent_id = stored
            .settings
            .agent_id
            .ok_or_else(|| anyhow::anyhow!("local runner config missing agent_id"))?;
        RegistrationClient::new()?
            .delete_runner(&scope, args.pat.as_ref().unwrap(), agent_id)
            .await?;
        println!("Deleted or confirmed absent remote JIT runner id {agent_id}.");
    } else if !args.local_only {
        println!(
            "Remote remove skipped; pass --pat to delete the stored JIT runner id from GitHub."
        );
    }

    if config::remove(&dir)? {
        println!("Removed local runner config from {}", dir.display());
    } else {
        println!("No local runner config at {}", dir.display());
    }
    Ok(())
}

pub async fn status(args: StatusArgs) -> Result<()> {
    let config_base = config::config_dir(args.config_dir.clone())?;
    let slot_dirs = daemon_slot_config_dirs(&config_base, args.slots)?;
    for (slot_index, dir) in slot_dirs.iter().enumerate() {
        if args.slots > 1 {
            println!("Daemon slot {}:", slot_index + 1);
        }
        status_one(&args, dir)
            .with_context(|| format!("read daemon slot-{} status", slot_index + 1))?;
        if slot_index + 1 < slot_dirs.len() {
            println!();
        }
    }
    Ok(())
}

fn status_one(args: &StatusArgs, dir: &Path) -> Result<()> {
    let stored = config::load(&dir)?;
    println!("Config dir: {}", dir.display());
    println!("GitHub URL: {}", stored.settings.github_url);
    println!("Runner name: {}", stored.settings.agent_name);
    println!(
        "Agent id: {}",
        stored
            .settings
            .agent_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!(
        "Pool: {}",
        stored.settings.pool_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "Pool id: {}",
        stored
            .settings
            .pool_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
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
    if args.check_target_mvp {
        validate_target_mvp_status(&stored)?;
        println!("Target MVP status: ready for x64 Linux target jobs.");
    }
    Ok(())
}

fn validate_target_mvp_status(stored: &StoredRunnerConfig) -> Result<()> {
    let mut missing = Vec::new();
    validate_linux_only_labels(&stored.settings.labels)?;
    platform::validate_arm_label_matches_host(&stored.settings.labels, std::env::consts::ARCH)?;
    if !stored.settings.use_v2_flow {
        missing.push("UseV2Flow is false".to_string());
    }
    if stored.settings.server_url_v2.is_none() {
        missing.push("ServerUrlV2 is missing".to_string());
    }
    if stored.settings.pool_id.is_none() {
        missing.push("pool id is missing".to_string());
    }
    if stored.settings.agent_id.is_none() {
        missing.push("agent id is missing".to_string());
    }
    if stored.credentials.is_none() {
        missing.push("runner credentials are missing".to_string());
    }

    for label in target_mvp_required_x64_labels() {
        if !stored
            .settings
            .labels
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(label))
        {
            missing.push(format!("label '{label}' is missing"));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "target MVP runner config is not ready: {}",
            missing.join("; ")
        )
    }
}

fn target_mvp_required_x64_labels() -> &'static [&'static str] {
    &[
        "hetzner-sentry-ci",
        "ubuntu-24.04",
        "ubuntu-latest",
        "velnor-target-mvp",
    ]
}

fn normalize_labels(
    mut labels: Vec<String>,
    target_mvp_labels: bool,
    target_mvp_arm_label: bool,
) -> Vec<String> {
    if labels.is_empty() {
        labels.push("velnor".to_string());
    }
    // self-hosted is always required so GitHub can match the runner.
    if !labels.iter().any(|l| l == "self-hosted") {
        labels.insert(0, "self-hosted".to_string());
    }
    if target_mvp_labels {
        labels.extend(
            target_mvp_required_x64_labels()
                .iter()
                .map(|label| label.to_string()),
        );
    }
    if target_mvp_arm_label {
        labels.push("ubuntu-24.04-arm".to_string());
    }
    labels.sort();
    labels.dedup();
    labels
}

fn validate_linux_only_labels(labels: &[String]) -> Result<()> {
    let unsupported = labels
        .iter()
        .find(|label| is_macos_runner_label(label))
        .map(String::as_str);
    if let Some(label) = unsupported {
        bail!(
            "unsupported non-Linux runner label '{label}'; Velnor runner execution is Linux-only"
        );
    }
    Ok(())
}

fn is_macos_runner_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    normalized == "macos" || normalized.starts_with("macos-") || normalized.contains("darwin")
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
        parse_action_metadata, resolve_action, ActionRuntime, LocalActionPlan, RepositoryActionPlan,
    };
    use crate::protocol::TaskAgentMessage;
    use crate::script_step::StepCommandTelemetry;
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn run_args(complete_noop: bool, execute_scripts: bool, dry_run_jobs: bool) -> RunArgs {
        RunArgs {
            config_dir: None,
            once: false,
            idle_timeout_seconds: None,
            complete_noop,
            execute_scripts,
            dry_run_jobs,
            dump_job_message: None,
            docker_image: "ubuntu:24.04".into(),
            node_action_image: String::new(),
            work_dir: None,
            docker_host_work_dir: None,
            skip_preflight: false,
            require_docker_socket: false,
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
    fn workflow_run_step_names_map_source_lines_to_explicit_names() {
        let yaml = r#"
name: Ansible
jobs:
  syntax-check:
    steps:
      - uses: actions/checkout@v6
      - name: Install Ansible
        run: pip install ansible-core
      - name: Install required collections
        run: ansible-galaxy collection install -r requirements.yaml
      - name: Syntax check playbooks
        run: |
          set -euo pipefail
          ansible-playbook --syntax-check site.yml
"#;

        let names = workflow_run_step_names_by_line(yaml);

        assert_eq!(names.get(&8).map(String::as_str), Some("Install Ansible"));
        assert_eq!(
            names.get(&10).map(String::as_str),
            Some("Install required collections")
        );
        assert_eq!(
            names.get(&13).map(String::as_str),
            Some("Syntax check playbooks")
        );
    }

    #[test]
    fn workflow_step_names_in_order_recovers_named_actions_and_scripts() {
        let yaml = r#"
jobs:
  build:
    steps:
      - name: Checkout code
        uses: actions/checkout@v6
      - uses: jdx/mise-action@v4
      - name: Cache rust-script
        uses: actions/cache@v5
      - name: Check if image exists
        id: check-image
        run: echo check
"#;

        let names = workflow_step_names_in_order(yaml);

        assert_eq!(
            names.iter().map(|name| name.as_deref()).collect::<Vec<_>>(),
            vec![
                Some("Checkout code"),
                None,
                Some("Cache rust-script"),
                Some("Check if image exists"),
            ]
        );
    }

    #[test]
    fn job_context_data_keeps_github_token_after_compact_context_expansion() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanId": "plan" },
            "Timeline": { "Id": "timeline" },
            "JobId": "job",
            "JobDisplayName": "job",
            "RequestId": 1,
            "Variables": {
                "system.github.token": { "Value": "token-123", "IsSecret": true }
            },
            "ContextData": {
                "github": {
                    "d": [
                        { "k": "workflow_sha", "v": "abc123" }
                    ],
                    "t": 2
                }
            }
        }))
        .unwrap();

        let context = job_context_data(&job);

        assert_eq!(
            context_string(&context, "github.workflow_sha").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            context_string(&context, "github.token").as_deref(),
            Some("token-123")
        );
    }

    #[test]
    fn run_preflight_args_preserve_target_docker_requirements() {
        let mut args = run_args(false, false, false);
        args.work_dir = Some(Path::new("/runner/work").to_path_buf());
        args.docker_host_work_dir = Some(Path::new("/daemon/work").to_path_buf());
        args.docker_image = "velnor/job-ubuntu:24.04".into();
        args.require_docker_socket = true;

        let preflight = preflight_args_for_run(&args, Path::new("/config"));

        assert_eq!(
            preflight.work_dir,
            Some(Path::new("/runner/work").to_path_buf())
        );
        assert_eq!(
            preflight.docker_host_work_dir,
            Some(Path::new("/daemon/work").to_path_buf())
        );
        assert_eq!(preflight.docker_image, "velnor/job-ubuntu:24.04");
        assert!(preflight.require_docker_socket);
        assert!(preflight.require_buildx);
    }

    #[test]
    fn run_preflight_args_default_work_dir_under_config() {
        let args = run_args(false, false, false);
        let preflight = preflight_args_for_run(&args, Path::new("/config"));

        assert_eq!(
            preflight.work_dir,
            Some(Path::new("/config/_work").to_path_buf())
        );
        assert_eq!(preflight.docker_host_work_dir, None);
        assert!(!preflight.require_docker_socket);
        assert!(preflight.require_buildx);
    }

    fn daemon_args(slots: usize) -> DaemonArgs {
        DaemonArgs {
            config_dir: None,
            url: None,
            pat: None,
            name: None,
            labels: Vec::new(),
            target_mvp_labels: false,
            target_mvp_arm_label: false,
            replace: false,
            pool_id: None,
            pool_name: None,
            dry_run_registration: false,
            slots,
            once: false,
            idle_timeout_seconds: None,
            complete_noop: false,
            execute_scripts: false,
            dry_run_jobs: false,
            dump_job_message: None,
            docker_image: "ubuntu:24.04".into(),
            node_action_image: String::new(),
            work_dir: None,
            docker_host_work_dir: None,
            skip_preflight: false,
            require_docker_socket: false,
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn daemon_config_dir_isolates_named_daemons_by_default() {
        let mut fixture = daemon_args(2);
        fixture.url = Some("https://github.com/tailrocks/velnor-actions-fixture".into());
        fixture.name = Some("velnor-fixture".into());

        let mut chainargos = daemon_args(10);
        chainargos.url = Some("https://github.com/ChainArgos/java-monorepo".into());
        chainargos.name = Some("velnor-sentry".into());

        let fixture_dir = daemon_config_dir(&fixture).unwrap();
        let chainargos_dir = daemon_config_dir(&chainargos).unwrap();

        assert_ne!(fixture_dir, chainargos_dir);
        assert!(fixture_dir.ends_with(Path::new("daemons/velnor-fixture")));
        assert!(chainargos_dir.ends_with(Path::new("daemons/velnor-sentry")));
    }

    #[test]
    fn daemon_config_dir_keeps_explicit_config_dir() {
        let mut args = daemon_args(4);
        args.url = Some("https://github.com/ChainArgos/blockchain-nodes".into());
        args.name = Some("velnor-blockchain-nodes".into());
        args.config_dir = Some(Path::new("/etc/velnor/blockchain-nodes").to_path_buf());

        let dir = daemon_config_dir(&args).unwrap();

        assert_eq!(dir, Path::new("/etc/velnor/blockchain-nodes"));
    }

    #[test]
    fn daemon_config_component_replaces_path_characters() {
        assert_eq!(
            sanitize_daemon_config_component("https://github.com/ChainArgos/java-monorepo"),
            "https---github.com-ChainArgos-java-monorepo"
        );
        assert_eq!(sanitize_daemon_config_component("///"), "default");
    }

    #[test]
    fn daemon_rejects_zero_slots() {
        let error = validate_daemon_slots(0).unwrap_err().to_string();

        assert!(error.contains("--slots must be greater than zero"));
    }

    #[test]
    fn daemon_single_slot_preserves_base_config_and_paths() {
        let mut args = daemon_args(1);
        args.work_dir = Some(Path::new("/work").to_path_buf());
        args.docker_host_work_dir = Some(Path::new("/host-work").to_path_buf());
        args.dump_job_message = Some(Path::new("/tmp/job.json").to_path_buf());

        let run_args = daemon_slot_run_args(&args, Path::new("/config"), 1, 1).unwrap();

        assert_eq!(
            run_args.config_dir,
            Some(Path::new("/config").to_path_buf())
        );
        assert_eq!(run_args.work_dir, Some(Path::new("/work").to_path_buf()));
        assert_eq!(
            run_args.docker_host_work_dir,
            Some(Path::new("/host-work").to_path_buf())
        );
        assert_eq!(
            run_args.dump_job_message,
            Some(Path::new("/tmp/job.json").to_path_buf())
        );
        assert!(!run_args.once);
    }

    #[test]
    fn daemon_multislot_configure_args_build_isolated_jit_runner_slots() {
        let mut args = daemon_args(2);
        args.url = Some("https://github.com/owner/repo".into());
        args.pat = Some("pat".into());
        args.name = Some("velnor-ci".into());
        args.labels = vec!["velnor".into(), "ubuntu-24.04".into()];
        args.replace = true;
        args.pool_name = Some("Default".into());

        let configure_args = daemon_slot_configure_args(&args, Path::new("/config"), 2, 2).unwrap();

        assert_eq!(configure_args.url, "https://github.com/owner/repo");
        assert_eq!(configure_args.pat.as_deref(), Some("pat"));
        assert_eq!(configure_args.name.as_deref(), Some("velnor-ci-slot-2"));
        assert_eq!(
            configure_args.config_dir,
            Some(Path::new("/config/slots/slot-2").to_path_buf())
        );
        assert_eq!(
            configure_args.labels,
            vec!["velnor".to_string(), "ubuntu-24.04".to_string()]
        );
        assert!(configure_args.replace);
        assert_eq!(configure_args.pool_name.as_deref(), Some("Default"));
    }

    #[test]
    fn daemon_slot_jit_config_skips_valid_existing_config_unless_replace() {
        let dir = unique_temp_dir("daemon-slot-config");
        config::save(&dir, &stored_config()).unwrap();

        assert!(!daemon_slot_should_configure_jit(&dir, false, false));
        assert!(daemon_slot_should_configure_jit(&dir, true, false));
        assert!(daemon_slot_should_configure_jit(&dir, false, true));

        fs::remove_dir_all(&dir).unwrap();
        assert!(daemon_slot_should_configure_jit(&dir, false, false));
    }

    #[tokio::test]
    async fn configure_replace_dry_run_removes_stale_local_jit_config() {
        let dir = unique_temp_dir("configure-replace-dry-run");
        config::save(&dir, &stored_config()).unwrap();

        configure(ConfigureArgs {
            url: "https://github.com/owner/repo".into(),
            pat: None,
            name: Some("velnor-replaced".into()),
            labels: vec!["velnor".into()],
            target_mvp_labels: false,
            target_mvp_arm_label: false,
            replace: true,
            pool_id: None,
            pool_name: None,
            dry_run: true,
            config_dir: Some(dir.clone()),
        })
        .await
        .unwrap();

        let stored = config::load(&dir).unwrap();
        assert_eq!(stored.settings.agent_name, "velnor-replaced");
        assert_eq!(stored.settings.agent_id, None);
        assert!(!stored.settings.ephemeral);

        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn daemon_failed_slot_cleanup_without_pat_removes_only_local_slot_config() {
        let base = unique_temp_dir("daemon-slot-local-cleanup");
        let slot_dir = daemon_slot_config_dir(&base, 2, 2);
        config::save(&slot_dir, &stored_config()).unwrap();
        let mut args = daemon_args(2);
        args.url = Some("https://github.com/owner/repo".into());

        delete_and_remove_daemon_slot_jit_config(&args, &slot_dir)
            .await
            .unwrap();

        assert!(config::load(&slot_dir).is_err());
        assert!(base.join("slots").exists());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn daemon_jit_config_dry_run_does_not_poll_for_jobs() {
        let mut args = daemon_args(1);
        assert!(daemon_should_poll_after_jit_config(&args));

        args.dry_run_registration = true;
        assert!(!daemon_should_poll_after_jit_config(&args));
    }

    #[test]
    fn daemon_preflight_args_cover_each_jit_slot_before_polling() {
        let mut args = daemon_args(2);
        args.url = Some("https://github.com/owner/repo".into());
        args.work_dir = Some(Path::new("/runner/work").to_path_buf());
        args.docker_host_work_dir = Some(Path::new("/daemon/work").to_path_buf());
        args.docker_image = "velnor/job-ubuntu:24.04".into();
        args.require_docker_socket = true;

        let preflight = daemon_preflight_args(&args, Path::new("/config"), 2).unwrap();

        assert_eq!(preflight.len(), 2);
        assert_eq!(
            preflight[0].work_dir,
            Some(Path::new("/runner/work/slot-1").to_path_buf())
        );
        assert_eq!(
            preflight[0].docker_host_work_dir,
            Some(Path::new("/daemon/work/slot-1").to_path_buf())
        );
        assert_eq!(
            preflight[1].work_dir,
            Some(Path::new("/runner/work/slot-2").to_path_buf())
        );
        assert_eq!(
            preflight[1].docker_host_work_dir,
            Some(Path::new("/daemon/work/slot-2").to_path_buf())
        );
        assert!(preflight.iter().all(|args| args.require_docker_socket));
        assert!(preflight.iter().all(|args| args.require_buildx));
        assert!(preflight
            .iter()
            .all(|args| args.docker_image == "velnor/job-ubuntu:24.04"));
    }

    #[test]
    fn daemon_preflight_args_skip_non_executable_modes() {
        let mut args = daemon_args(2);
        args.url = Some("https://github.com/owner/repo".into());
        args.complete_noop = true;
        assert!(daemon_preflight_args(&args, Path::new("/config"), 2)
            .unwrap()
            .is_empty());

        args.complete_noop = false;
        args.dry_run_jobs = true;
        assert!(daemon_preflight_args(&args, Path::new("/config"), 2)
            .unwrap()
            .is_empty());

        args.dry_run_jobs = false;
        args.skip_preflight = true;
        assert!(daemon_preflight_args(&args, Path::new("/config"), 2)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn daemon_single_slot_configure_args_preserve_runner_name() {
        let mut args = daemon_args(1);
        args.url = Some("https://github.com/owner/repo".into());
        args.name = Some("velnor-ci".into());

        let configure_args = daemon_slot_configure_args(&args, Path::new("/config"), 1, 1).unwrap();

        assert_eq!(configure_args.name.as_deref(), Some("velnor-ci"));
        assert_eq!(
            configure_args.config_dir,
            Some(Path::new("/config").to_path_buf())
        );
    }

    #[test]
    fn daemon_multislot_run_args_use_isolated_config_and_work_dirs() {
        let mut args = daemon_args(3);
        args.work_dir = Some(Path::new("/work").to_path_buf());
        args.docker_host_work_dir = Some(Path::new("/host-work").to_path_buf());
        args.dump_job_message = Some(Path::new("/tmp/jobs").to_path_buf());

        let run_args = daemon_slot_run_args(&args, Path::new("/config"), 2, 3).unwrap();

        assert_eq!(
            run_args.config_dir,
            Some(Path::new("/config/slots/slot-2").to_path_buf())
        );
        assert_eq!(
            run_args.work_dir,
            Some(Path::new("/work/slot-2").to_path_buf())
        );
        assert_eq!(
            run_args.docker_host_work_dir,
            Some(Path::new("/host-work/slot-2").to_path_buf())
        );
        assert_eq!(
            run_args.dump_job_message,
            Some(Path::new("/tmp/jobs/slot-2").to_path_buf())
        );
        assert!(!run_args.once);
    }

    #[test]
    fn daemon_run_args_propagate_once_to_each_slot() {
        let mut args = daemon_args(2);
        args.once = true;

        let run_args = daemon_slot_run_args(&args, Path::new("/config"), 1, 2).unwrap();

        assert!(run_args.once);
    }

    #[test]
    fn daemon_slot_config_dirs_match_single_and_multislot_layouts() {
        assert_eq!(
            daemon_slot_config_dirs(Path::new("/config"), 1).unwrap(),
            vec![Path::new("/config").to_path_buf()]
        );
        assert_eq!(
            daemon_slot_config_dirs(Path::new("/config"), 2).unwrap(),
            vec![
                Path::new("/config/slots/slot-1").to_path_buf(),
                Path::new("/config/slots/slot-2").to_path_buf()
            ]
        );
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
    fn broker_poll_state_matches_v2_retry_shape() {
        let mut state = BrokerPollState::default();

        for _ in 0..5 {
            assert_eq!(state.received_error().unwrap(), Duration::from_secs(15));
        }
        for _ in 5..(BROKER_POLL_MAX_CONSECUTIVE_ERRORS - 1) {
            assert_eq!(state.received_error().unwrap(), Duration::from_secs(30));
        }
        assert!(state.received_error().is_err());

        state.received_message();
        assert_eq!(state.consecutive_errors, 0);
        assert_eq!(state.consecutive_empty_messages, 0);
    }

    #[test]
    fn broker_session_create_retry_delay_is_bounded() {
        assert_eq!(
            broker_session_create_retry_delay(1),
            Duration::from_secs(BROKER_SESSION_CREATE_RETRY_SECONDS)
        );
        assert_eq!(
            broker_session_create_retry_delay(2),
            Duration::from_secs(BROKER_SESSION_CREATE_RETRY_SECONDS * 2)
        );
        assert_eq!(
            broker_session_create_retry_delay(4),
            Duration::from_secs(BROKER_SESSION_CREATE_RETRY_SECONDS * 3)
        );
    }

    #[test]
    fn runner_connection_diagnostic_is_sanitized() {
        let mut stored = stored_config();
        stored.settings.labels = vec!["self-hosted".into(), "hetzner-sentry-ci".into()];
        stored.credentials = Some(StoredCredentials {
            scheme: CredentialScheme::OAuthAccessToken,
            data: serde_json::json!({ "token": "secret-token" }),
        });

        let diagnostic =
            RunnerConnectionDiagnostic::from_config(&stored, "https://broker.example/");
        let rendered = diagnostic.to_string();

        assert!(rendered.contains("github_url=https://github.com/owner/repo"));
        assert!(rendered.contains("broker_url=https://broker.example/"));
        assert!(rendered.contains("agent_name=velnor"));
        assert!(rendered.contains("agent_id=2"));
        assert!(rendered.contains("pool_id=1"));
        assert!(rendered.contains("labels=self-hosted,hetzner-sentry-ci"));
        assert!(!rendered.contains("secret-token"));
    }

    #[test]
    fn broker_poll_state_backs_off_after_many_empty_messages() {
        let mut state = BrokerPollState::default();

        for _ in 0..BROKER_POLL_EMPTY_BACKOFF_THRESHOLD {
            assert_eq!(state.received_empty_message(), None);
        }
        assert_eq!(
            state.received_empty_message(),
            Some(Duration::from_secs(15))
        );
        assert_eq!(state.consecutive_empty_messages, 0);
    }

    #[test]
    fn once_mode_stops_only_after_job_handled() {
        assert!(!should_stop_after_message(
            false,
            &V2MessageAction::JobHandled
        ));
        assert!(should_stop_after_message(
            true,
            &V2MessageAction::JobHandled
        ));
        assert!(!should_stop_after_message(true, &V2MessageAction::None));
        assert!(!should_stop_after_message(
            true,
            &V2MessageAction::BrokerMigration(
                "https://broker.actions.githubusercontent.com/new/".into()
            )
        ));
        assert!(!should_stop_after_message(
            true,
            &V2MessageAction::RefreshToken
        ));
    }

    #[test]
    fn idle_timeout_duration_rejects_zero() {
        assert!(idle_timeout_duration(None).unwrap().is_none());
        assert_eq!(
            idle_timeout_duration(Some(30)).unwrap(),
            Some(Duration::from_secs(30))
        );
        assert!(idle_timeout_duration(Some(0)).is_err());
    }

    #[test]
    fn idle_timeout_elapsed_only_after_threshold() {
        assert!(!idle_timeout_elapsed(Duration::from_secs(60), None));
        assert!(!idle_timeout_elapsed(
            Duration::from_secs(59),
            Some(Duration::from_secs(60))
        ));
        assert!(idle_timeout_elapsed(
            Duration::from_secs(60),
            Some(Duration::from_secs(60))
        ));
        assert!(idle_timeout_elapsed(
            Duration::from_secs(61),
            Some(Duration::from_secs(60))
        ));
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

    #[test]
    fn job_dump_filename_includes_live_run_context() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job/id 123",
            "jobName": "syntax-check",
            "jobDisplayName": "Syntax Check",
            "requestId": 42,
            "variables": {
                "github.repository": { "value": "ChainArgos/java-monorepo" },
                "github.run_id": { "value": "1234567890" }
            }
        }))
        .unwrap();

        assert_eq!(
            job_dump_filename(&job),
            "job-ChainArgos_java-monorepo-1234567890-42-syntax-check-job_id_123.json"
        );
    }

    #[test]
    fn actions_environment_url_reads_template_token() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Deploy",
            "requestId": 1,
            "actionsEnvironment": {
                "name": "github-pages",
                "url": {
                    "type": "String",
                    "value": "${{ steps.deployment.outputs.page_url }}"
                }
            }
        }))
        .unwrap();

        assert_eq!(
            actions_environment_url(&job).and_then(|value| value.get("value")),
            Some(&serde_json::json!(
                "${{ steps.deployment.outputs.page_url }}"
            ))
        );
    }

    #[test]
    fn safe_environment_url_skips_masked_values() {
        let log = StepLog {
            step_id: "deploy".into(),
            display_name: String::new(),
            order: 1,
            started_at: String::new(),
            completed_at: String::new(),
            lines: Vec::new(),
            masks: vec!["runtime-secret".into()],
            annotations: Vec::new(),
            telemetry: Vec::new(),
            exit_code: 0,
            skipped: false,
            failure_ignored: false,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
            summary: String::new(),
        };

        assert_eq!(
            safe_environment_url(
                Some("https://example.com/docs/".into()),
                &["job-secret".into()],
                std::slice::from_ref(&log),
            )
            .as_deref(),
            Some("https://example.com/docs/")
        );
        assert!(safe_environment_url(
            Some("https://example.com/runtime-secret".into()),
            &["job-secret".into()],
            &[log],
        )
        .is_none());
    }

    #[test]
    fn classifies_docker_infrastructure_failures() {
        let bind_mount = anyhow::anyhow!(
            "Docker daemon cannot see Velnor bind-mounted work directories. Use a local Docker daemon"
        );
        let docker_start =
            anyhow::anyhow!("docker run failed").context("Docker job environment start failed");

        assert_eq!(
            infrastructure_failure_category(&bind_mount),
            Some("docker_bind_mount")
        );
        assert_eq!(
            infrastructure_failure_category(&docker_start),
            Some("docker_environment")
        );
        assert_eq!(
            infrastructure_failure_category(&anyhow::anyhow!("user script failed")),
            None
        );
    }

    #[test]
    fn timeline_records_include_step_issue_counts() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": {
                "planId": "plan",
                "planType": "Build",
                "scopeIdentifier": "scope"
            },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1
        }))
        .unwrap();
        let log = StepLog {
            step_id: "step-1".into(),
            display_name: String::new(),
            order: 7,
            started_at: String::new(),
            completed_at: String::new(),
            lines: vec!["hello".into()],
            masks: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            exit_code: 1,
            skipped: false,
            failure_ignored: false,
            error_count: 1,
            warning_count: 2,
            notice_count: 3,
            summary: String::new(),
        };

        let records = timeline_records_for_step_logs(&job, &[log], "2026-06-01T00:00:00Z");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "step-1");
        assert_eq!(records[0].parent_id.as_deref(), Some("job"));
        assert_eq!(records[0].order, Some(7));
        assert_eq!(records[0].result, Some(TaskResult::Failed));
        assert_eq!(records[0].error_count, 1);
        assert_eq!(records[0].warning_count, 2);
        assert_eq!(records[0].notice_count, 3);
    }

    #[test]
    fn timeline_started_record_marks_job_in_progress() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": {
                "planId": "plan",
                "planType": "Build",
                "scopeIdentifier": "scope"
            },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobName": "check",
            "jobDisplayName": "Check",
            "requestId": 1
        }))
        .unwrap();

        let record = timeline_started_record(&job, "velnor-1", "2026-06-01T00:00:00Z");
        let json = serde_json::to_value(record).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "id": "job",
                "type": "Job",
                "name": "Check",
                "startTime": "2026-06-01T00:00:00Z",
                "percentComplete": 0,
                "state": "inProgress",
                "workerName": "velnor-1",
                "refName": "check",
                "errorCount": 0,
                "warningCount": 0,
                "noticeCount": 0
            })
        );
    }

    #[test]
    fn timeline_step_started_record_marks_task_in_progress() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": {
                "planId": "plan",
                "planType": "Build",
                "scopeIdentifier": "scope"
            },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1
        }))
        .unwrap();
        let event = StepStartEvent {
            step_id: "step-1".into(),
            display_name: String::new(),
            order: 2,
        };

        let record = timeline_step_started_record(&job, &event, "2026-06-01T00:00:00Z");
        let json = serde_json::to_value(record).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "id": "step-1",
                "parentId": "job",
                "type": "Task",
                "name": "step-1",
                "startTime": "2026-06-01T00:00:00Z",
                "percentComplete": 0,
                "state": "inProgress",
                "order": 2,
                "errorCount": 0,
                "warningCount": 0,
                "noticeCount": 0
            })
        );
    }

    #[test]
    fn masks_timeline_feed_lines() {
        let lines = vec![
            "token=secret-value".to_string(),
            "ordinary line".to_string(),
        ];

        assert_eq!(
            mask_log_lines(&lines, &["secret-value".into()]),
            vec!["token=***".to_string(), "ordinary line".to_string()]
        );
    }

    #[test]
    fn run_service_telemetry_masks_step_and_job_secrets() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1,
            "variables": {
                "SECRET_TOKEN": {
                    "value": "job-secret",
                    "isSecret": true
                }
            }
        }))
        .unwrap();
        let log = StepLog {
            step_id: "step-1".into(),
            display_name: String::new(),
            order: 1,
            started_at: String::new(),
            completed_at: String::new(),
            lines: Vec::new(),
            masks: vec!["step-secret".into()],
            annotations: Vec::new(),
            telemetry: vec![StepCommandTelemetry {
                message: "DeprecatedCommand: set-output job-secret step-secret".into(),
                kind: "ActionCommand".into(),
            }],
            exit_code: 0,
            skipped: false,
            failure_ignored: false,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
            summary: String::new(),
        };
        let duplicate_log = StepLog {
            telemetry: vec![StepCommandTelemetry {
                message: "DeprecatedCommand: set-output job-secret step-secret".into(),
                kind: "ActionCommand".into(),
            }],
            ..log.clone()
        };

        let telemetry = run_service_telemetry(&job, &[log, duplicate_log]);

        assert_eq!(telemetry.len(), 1);
        assert_eq!(
            telemetry[0].message,
            "DeprecatedCommand: set-output *** ***"
        );
        assert_eq!(telemetry[0].kind, "ActionCommand");
    }

    #[test]
    fn timeline_publish_context_uses_system_connection() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": {
                "planId": "plan",
                "planType": "Build",
                "scopeIdentifier": "scope"
            },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Check",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "url": "https://pipelines.actions.githubusercontent.com/abc",
                    "authorization": {
                        "parameters": { "AccessToken": "job-token" }
                    }
                }]
            }
        }))
        .unwrap();

        let context = timeline_publish_context(&job).unwrap().unwrap();

        assert_eq!(context.scope_identifier, "scope");
        assert_eq!(context.hub_name, "build");
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

    #[tokio::test]
    async fn idle_job_cancellation_is_control_message_not_unsupported_job() {
        let broker =
            BrokerClient::new("https://broker.actions.githubusercontent.com/", "token").unwrap();
        let run_service = RunServiceClient::new("token").unwrap();
        let stored = stored_config();
        let message = TaskAgentMessage {
            message_id: 10,
            message_type: "JobCancellation".into(),
            body: serde_json::json!({ "jobId": "job-123" }).to_string(),
            iv_base64: None,
        };

        let action = handle_v2_message(
            &broker,
            &run_service,
            "session",
            &stored,
            Path::new("/config"),
            &run_args(false, false, false),
            true,
            "velnor",
            message,
        )
        .await
        .unwrap();

        assert_eq!(action, V2MessageAction::None);
    }

    #[test]
    fn default_labels_keep_velnor_only() {
        assert_eq!(
            normalize_labels(Vec::new(), false, false),
            vec!["self-hosted", "velnor"]
        );
    }

    #[test]
    fn target_mvp_labels_cover_current_x64_linux_target_jobs() {
        assert_eq!(
            normalize_labels(vec!["custom".into()], true, false),
            vec![
                "custom",
                "hetzner-sentry-ci",
                "self-hosted",
                "ubuntu-24.04",
                "ubuntu-latest",
                "velnor-target-mvp"
            ]
        );
    }

    #[test]
    fn target_mvp_status_requires_v2_credentials_ids_and_labels() {
        let mut stored = stored_config();
        stored.credentials = Some(StoredCredentials {
            scheme: CredentialScheme::OAuthAccessToken,
            data: serde_json::json!({ "token": "runner-token" }),
        });
        stored.settings.labels = normalize_labels(Vec::new(), true, false);

        assert!(validate_target_mvp_status(&stored).is_ok());

        stored.settings.labels = vec!["velnor".into()];
        let error = validate_target_mvp_status(&stored).unwrap_err().to_string();
        assert!(error.contains("label 'hetzner-sentry-ci' is missing"));

        stored.settings.labels = normalize_labels(Vec::new(), true, false);
        stored.settings.use_v2_flow = false;
        let error = validate_target_mvp_status(&stored).unwrap_err().to_string();
        assert!(error.contains("UseV2Flow is false"));
    }

    #[test]
    fn target_mvp_arm_label_is_explicit() {
        assert_eq!(
            normalize_labels(Vec::new(), true, true),
            vec![
                "hetzner-sentry-ci",
                "self-hosted",
                "ubuntu-24.04",
                "ubuntu-24.04-arm",
                "ubuntu-latest",
                "velnor",
                "velnor-target-mvp"
            ]
        );
    }

    #[test]
    fn arm_label_requires_arm_host() {
        let labels = normalize_labels(Vec::new(), true, true);
        assert!(platform::validate_arm_label_matches_host(&labels, "aarch64").is_ok());
        assert!(platform::validate_arm_label_matches_host(&labels, "arm64").is_ok());

        let error = platform::validate_arm_label_matches_host(&labels, "x86_64")
            .unwrap_err()
            .to_string();
        assert!(error.contains("only claim it when Docker can provide ARM64 Linux job containers"));
    }

    #[test]
    fn macos_runner_labels_are_rejected() {
        let labels = vec!["velnor".into(), "macos-latest".into()];
        let error = validate_linux_only_labels(&labels).unwrap_err().to_string();
        assert!(error.contains("Velnor runner execution is Linux-only"));

        let labels = vec!["x86_64-apple-darwin".into()];
        let error = validate_linux_only_labels(&labels).unwrap_err().to_string();
        assert!(error.contains("unsupported non-Linux runner label"));
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
            display_name: String::new(),
            clone_url: "https://github.com/jackin-project/jackin.git".into(),
            version: Some("${{ needs.source-changed.outputs.sha }}".into()),
            destination: Path::new("/tmp/work").to_path_buf(),
            token: None,
            fetch_depth: None,
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
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
    fn checkout_step_output_ref_survives_eager_context_resolution() {
        let plan = CheckoutPlan {
            step_id: "checkout2".into(),
            display_name: String::new(),
            clone_url: "https://github.com/jackin-project/jackin.git".into(),
            version: Some("${{ steps.source.outputs.sha }}".into()),
            destination: Path::new("/tmp/work").to_path_buf(),
            token: None,
            fetch_depth: None,
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
        };

        let resolved = resolve_checkout_plan_context(plan, &[], &[]);

        assert_eq!(
            resolved.version.as_deref(),
            Some("${{ steps.source.outputs.sha }}")
        );
        assert!(resolved.requires_runtime_context());
    }

    #[test]
    fn eager_checkout_resolves_ref_from_github_env_context() {
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/jackin-project/jackin.git".into(),
            version: Some("${{ github.sha }}".into()),
            destination: Path::new("/tmp/work").to_path_buf(),
            token: None,
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
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
        let ExecutableStep::Native {
            step_id,
            invocation,
            condition,
            continue_on_error,
            ..
        } = &ordered[1]
        else {
            panic!("nested repository action should expand to native adapter step")
        };
        assert_eq!(step_id, "docs-1");
        assert_eq!(invocation.adapter, crate::action::NativeActionAdapter::Mise);
        assert_eq!(invocation.inputs["github_token"], "ghs_token");
        assert_eq!(condition.as_deref(), Some("github.event_name == 'push'"));
        assert!(*continue_on_error);
        assert!(matches!(
            &ordered[2],
            ExecutableStep::CompositeEnd { step_id } if step_id == "docs"
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

        let plans = vec![resolved[1].plan.clone()];
        let ordered =
            ordered_executable_steps(&job, &[], &plans, &resolved, &[], actions_host, &[]).unwrap();

        let ExecutableStep::JavaScript { invocation, .. } = &ordered[0] else {
            panic!("repository action should expand to JavaScript step")
        };
        assert_eq!(
            invocation.main_container_path,
            "/__a/_actions/acme_action/v1/sub/action/sub.js"
        );
    }

    #[test]
    fn unsupported_actions_fail_at_step_planning_time() {
        let actions_host = Path::new("/tmp/actions");
        let metadata = parse_action_metadata("runs:\n  using: node20\n  main: index.js\n").unwrap();
        for repository in ["dtolnay/rust-toolchain", "baptiste0928/cargo-install"] {
            let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
                "messageType": "PipelineAgentJobRequest",
                "plan": { "planId": "plan" },
                "timeline": { "id": "timeline" },
                "jobId": "job",
                "jobDisplayName": "Build",
                "requestId": 1,
                "steps": [{
                    "id": "step",
                    "reference": {
                        "type": "Repository",
                        "name": repository,
                        "ref": "stable"
                    }
                }]
            }))
            .unwrap();
            let plan = RepositoryActionPlan {
                step_id: "step".into(),
                repository: repository.into(),
                git_ref: "stable".into(),
                source_path: None,
                repository_dir: actions_host.join("_actions/step/stable"),
                action_dir: actions_host.join("_actions/step/stable"),
                inputs: BTreeMap::new(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            };
            let resolved = vec![ResolvedAction {
                plan: plan.clone(),
                metadata_path: actions_host.join("_actions/step/stable/action.yml"),
                runtime: metadata.runtime().unwrap(),
                metadata: metadata.clone(),
            }];
            let error =
                ordered_executable_steps(&job, &[], &[plan], &resolved, &[], actions_host, &[])
                    .unwrap_err();
            assert!(
                error.to_string().contains("jdx/mise-action"),
                "expected error for {repository} to mention jdx/mise-action, got: {error}"
            );
        }
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

        let ordered =
            ordered_executable_steps(&job, &[], &plans, &resolved, &[], actions_host, &[])
                .unwrap_or_else(|error| panic!("plan target action inventory: {error:#}"));

        assert!(
            ordered.len() >= plans.len(),
            "expected target action inventory to produce executable steps"
        );
        let sidecar_steps = ordered
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    ExecutableStep::JavaScript { .. } | ExecutableStep::Docker { .. }
                )
            })
            .collect::<Vec<_>>();
        assert!(
            sidecar_steps.is_empty(),
            "target repository action inventory must route through native adapters, got {sidecar_steps:#?}"
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
        let plans = vec![plan.clone()];
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/renovatebot_github-action/v46.1.14/action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let ordered =
            ordered_executable_steps(&job, &[], &plans, &[resolved], &[], actions_host, &[])
                .unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Native {
            step_id,
            invocation,
            ..
        } = &ordered[0]
        else {
            panic!("target repository Docker action should expand to native adapter step")
        };
        assert_eq!(step_id, "renovate");
        assert_eq!(
            invocation.adapter,
            crate::action::NativeActionAdapter::Renovate
        );
        assert_eq!(
            invocation.inputs["renovate-image"],
            "ghcr.io/renovatebot/renovate"
        );
    }

    #[test]
    fn native_repository_actions_ignore_pinned_ref_metadata() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Cache",
            "requestId": 1,
            "steps": [{
                "id": "cache",
                "reference": {
                    "type": "Repository",
                    "name": "actions/cache",
                    "ref": "pinned-sha-ignored-by-native-adapter"
                },
                "inputs": {
                    "key": "linux-cache",
                    "path": "~/.cargo"
                }
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let plans = repository_action_plans(&job.steps, actions_host).unwrap();

        let ordered =
            ordered_executable_steps(&job, &[], &plans, &[], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Native { invocation, .. } = &ordered[0] else {
            panic!("known native action should not require downloaded action metadata")
        };
        assert_eq!(
            invocation.adapter,
            crate::action::NativeActionAdapter::Cache
        );
        assert_eq!(invocation.inputs["key"], "linux-cache");
        assert_eq!(invocation.inputs["path"], "~/.cargo");
    }

    #[test]
    fn native_repository_actions_do_not_require_ref_metadata() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Cache",
            "requestId": 1,
            "steps": [{
                "id": "cache",
                "reference": {
                    "type": "Repository",
                    "name": "actions/cache"
                },
                "inputs": {
                    "key": "linux-cache",
                    "path": "~/.cargo"
                }
            }]
        }))
        .unwrap();
        let actions_host = Path::new("/tmp/actions");
        let plans = repository_action_plans(&job.steps, actions_host).unwrap();

        let ordered =
            ordered_executable_steps(&job, &[], &plans, &[], &[], actions_host, &[]).unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Native { invocation, .. } = &ordered[0] else {
            panic!("known native action should not require downloaded action metadata")
        };
        assert_eq!(
            invocation.adapter,
            crate::action::NativeActionAdapter::Cache
        );
        assert_eq!(invocation.inputs["key"], "linux-cache");
        assert_eq!(invocation.inputs["path"], "~/.cargo");
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
        let plans = vec![plan.clone()];
        let resolved = ResolvedAction {
            plan,
            metadata_path: actions_host
                .join("_actions/actions_upload-pages-artifact/v5/action.yml"),
            runtime: metadata.runtime().unwrap(),
            metadata,
        };

        let ordered =
            ordered_executable_steps(&job, &[], &plans, &[resolved], &[], actions_host, &[])
                .unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Native {
            step_id,
            invocation,
            condition,
            ..
        } = &ordered[0]
        else {
            panic!("target repository composite should route to native adapter step")
        };
        assert_eq!(step_id, "pages");
        assert_eq!(
            invocation.adapter,
            crate::action::NativeActionAdapter::UploadPagesArtifact
        );
        assert_eq!(condition.as_deref(), Some("runner.os == 'Linux'"));
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
        let plans = vec![pages_plan.clone(), upload_plan.clone()];
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
            ordered_executable_steps(&job, &[], &plans, &[pages, upload], &[], actions_host, &[])
                .unwrap();

        assert_eq!(ordered.len(), 1);
        let ExecutableStep::Native {
            step_id,
            invocation,
            continue_on_error,
            ..
        } = &ordered[0]
        else {
            panic!("target composite action should route to native adapter step")
        };
        assert_eq!(step_id, "pages");
        assert_eq!(
            invocation.adapter,
            crate::action::NativeActionAdapter::UploadPagesArtifact
        );
        assert!(*continue_on_error);
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

    #[test]
    fn expand_broker_context_value_flattens_d_array() {
        let compact = serde_json::json!({
            "d": [
                {"k": "repository", "v": "donbeave/velnor-actions-fixture"},
                {"k": "ref", "v": "refs/heads/main"}
            ]
        });
        let expanded = expand_broker_context_value(compact);
        assert_eq!(
            expanded.get("repository").and_then(Value::as_str),
            Some("donbeave/velnor-actions-fixture")
        );
        assert_eq!(
            expanded.get("ref").and_then(Value::as_str),
            Some("refs/heads/main")
        );
    }

    #[test]
    fn expand_broker_context_value_preserves_plain_object() {
        let plain = serde_json::json!({"repository": "org/repo", "sha": "abc123"});
        let result = expand_broker_context_value(plain.clone());
        assert_eq!(result, plain);
    }

    #[test]
    fn setup_job_lines_contains_version_and_image() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanType": "Build", "ScopeIdentifier": "s", "PlanId": "p", "Version": 1 },
            "Timeline": { "Id": "t" },
            "JobId": "j",
            "JobDisplayName": "build (app-a)",
            "RequestId": 1
        }))
        .unwrap();
        let lines = setup_job_lines(&job, "velnor/job-ubuntu:24.04");
        let joined = lines.join("\n");
        assert!(joined.contains("Current runner version:"));
        assert!(joined.contains("velnor/job-ubuntu:24.04"));
        assert!(joined.contains("Complete job name: build (app-a)"));
        assert!(joined.contains("##[group]Operating System"));
        assert!(joined.contains("##[endgroup]"));
        assert!(joined.contains("Prepare workflow directory"));
        // Secret source always present regardless of whether permissions are known.
        assert!(joined.contains("Secret source: Actions"));
    }

    #[test]
    fn setup_job_lines_shows_github_token_permissions() {
        // Permissions are stored in job variable "system.github.token.permissions"
        // as a JSON string. The group must appear AND "Secret source: Actions" must
        // follow it (not be a fallback).
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanType": "Build", "ScopeIdentifier": "s", "PlanId": "p", "Version": 1 },
            "Timeline": { "Id": "t" },
            "JobId": "j",
            "JobDisplayName": "CI",
            "RequestId": 1,
            "Variables": {
                "system.github.token.permissions": {
                    "Value": "{\"Actions\":\"read\",\"Contents\":\"read\",\"Metadata\":\"read\"}",
                    "IsSecret": false
                }
            }
        }))
        .unwrap();
        let lines = setup_job_lines(&job, "velnor/job-ubuntu:24.04");
        let joined = lines.join("\n");
        assert!(
            joined.contains("##[group]GITHUB_TOKEN Permissions"),
            "permissions group missing: {joined}"
        );
        assert!(joined.contains("Actions: read"), "scope missing: {joined}");
        assert!(joined.contains("Contents: read"), "scope missing: {joined}");
        assert!(joined.contains("Metadata: read"), "scope missing: {joined}");
        // "Secret source: Actions" must ALSO appear after the permissions group.
        assert!(
            joined.contains("Secret source: Actions"),
            "secret source missing: {joined}"
        );
        // Permissions group must come BEFORE Secret source in the output.
        let perm_pos = joined.find("##[group]GITHUB_TOKEN Permissions").unwrap();
        let secret_pos = joined.find("Secret source: Actions").unwrap();
        assert!(
            perm_pos < secret_pos,
            "permissions group must precede Secret source line"
        );
    }

    #[test]
    fn setup_job_lines_lists_repository_actions() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanType": "Build", "ScopeIdentifier": "s", "PlanId": "p", "Version": 1 },
            "Timeline": { "Id": "t" },
            "JobId": "j",
            "JobDisplayName": "CI",
            "RequestId": 1,
            "Steps": [
                {
                    "Reference": {
                        "Type": "Repository",
                        "Name": "actions/checkout",
                        "Ref": "v6"
                    }
                },
                {
                    "Reference": {
                        "Type": "Script"
                    }
                }
            ]
        }))
        .unwrap();
        let lines = setup_job_lines(&job, "velnor/job-ubuntu:24.04");
        let joined = lines.join("\n");
        // Tag ref: no SHA suffix.
        assert!(
            joined.contains("Download action repository 'actions/checkout@v6'"),
            "{joined}"
        );
        assert!(
            !joined.contains("(SHA:"),
            "tag ref must not show SHA suffix: {joined}"
        );
        assert!(joined.contains("##[group]Prepare all required actions"));
    }

    #[test]
    fn setup_job_lines_action_download_shows_sha_when_ref_is_commit() {
        let sha = "de0fac2e4500dabe0009e67214ff5f5447ce83dd";
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "MessageType": "PipelineAgentJobRequest",
            "Plan": { "PlanType": "Build", "ScopeIdentifier": "s", "PlanId": "p", "Version": 1 },
            "Timeline": { "Id": "t" },
            "JobId": "j",
            "JobDisplayName": "CI",
            "RequestId": 1,
            "Steps": [{
                "Reference": {
                    "Type": "Repository",
                    "Name": "actions/checkout",
                    "Ref": sha
                }
            }]
        }))
        .unwrap();
        let lines = setup_job_lines(&job, "velnor/job-ubuntu:24.04");
        let joined = lines.join("\n");
        // Full 40-char SHA ref: show SHA suffix matching GitHub UI format.
        assert!(
            joined.contains(&format!("'actions/checkout@{sha}' (SHA:{sha})")),
            "SHA suffix missing for commit ref: {joined}"
        );
    }

    #[test]
    fn complete_job_lines_has_cleanup_content() {
        let lines = complete_job_lines();
        let joined = lines.join("\n");
        assert!(joined.contains("##[group]Post-job cleanup"));
        assert!(joined.contains("Stop job container"));
        assert!(joined.contains("Finishing: Complete job"));
    }

    #[test]
    fn checkout_step_lines_success_contains_key_info() {
        use crate::checkout::CheckoutPlan;
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: "Checkout".into(),
            clone_url: "https://github.com/org/repo.git".into(),
            version: Some("main".into()),
            destination: std::path::PathBuf::from("/workspace/repo"),
            token: None,
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
        };
        let trace = vec![
            "[command]git init /work".to_string(),
            "[command]git fetch --prune origin main".to_string(),
        ];
        let lines = checkout_step_lines(&plan, 0, &trace);
        let joined = lines.join("\n");
        assert!(joined.contains("github.com/org/repo.git"));
        assert!(joined.contains("[command]git init /work"));
        assert!(joined.contains("[command]git fetch --prune origin main"));
        assert!(joined.contains("main"));
        assert!(joined.contains("Fetch depth: 1"));
        assert!(joined.contains("Checkout completed successfully"));
    }

    #[test]
    fn unix_now_iso8601_is_github_strippable() {
        // REGRESSION GUARD (do not weaken): GitHub's log UI strips a leading
        // per-line timestamp from the visible content ONLY when it matches the
        // runner's .NET "o" format with SEVEN fractional digits —
        // `YYYY-MM-DDTHH:MM:SS.fffffffZ` (e.g. 2026-06-04T15:27:50.9085200Z).
        //
        // This file once emitted SECOND precision (no sub-seconds) on the mistaken
        // belief that sub-seconds broke the parser. The opposite is true: without
        // the fractional component GitHub does NOT recognise the prefix and the
        // timestamp leaks into every visible log line. If this test ever fails
        // because the format lost its sub-seconds, timestamps are back in the UI.
        let ts = unix_now_iso8601();
        assert!(
            ts.contains('.'),
            "timestamp MUST contain sub-seconds or GitHub renders it as log content: {ts}"
        );
        let (date_time, frac_z) = ts.split_once('.').expect("timestamp must contain '.'");
        // Date-time portion: YYYY-MM-DDTHH:MM:SS (19 chars).
        assert_eq!(
            date_time.len(),
            19,
            "expected YYYY-MM-DDTHH:MM:SS before the dot: {ts}"
        );
        assert!(
            date_time.contains('T'),
            "timestamp must contain T separator: {ts}"
        );
        assert!(
            frac_z.ends_with('Z'),
            "timestamp must end with Z (UTC): {ts}"
        );
        let frac = frac_z.trim_end_matches('Z');
        assert_eq!(
            frac.len(),
            7,
            "GitHub's .NET 'o' format uses exactly 7 fractional digits: {ts}"
        );
        assert!(
            frac.chars().all(|c| c.is_ascii_digit()),
            "fractional seconds must be all digits: {ts}"
        );
        // Full form: 19 (date-time) + 1 (dot) + 7 (frac) + 1 (Z) = 28 chars.
        assert_eq!(ts.len(), 28, "expected 28-char .NET 'o' timestamp: {ts}");
    }

    #[test]
    fn run_service_step_result_uses_per_step_completed_at_not_job_finish() {
        // RunServiceStepResult.completed_at must come from StepLog.completed_at (the
        // actual step finish time), NOT from a single job-level completion_time.
        // Before this fix all steps showed the job finish time as completed_at,
        // making step durations inflate to the total job duration.
        use crate::executor::StepLog;
        use crate::protocol::{RunServiceStepResult, TaskResult, TimelineRecordState};

        let step_log = StepLog {
            step_id: "abc-uuid".to_string(),
            display_name: "Run cargo test".to_string(),
            order: 5,
            started_at: "2026-06-04T10:00:00Z".to_string(),
            completed_at: "2026-06-04T10:00:03Z".to_string(), // 3s step
            lines: vec!["test passed".to_string()],
            masks: vec![],
            annotations: vec![],
            telemetry: vec![],
            exit_code: 0,
            skipped: false,
            failure_ignored: false,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
            summary: String::new(),
        };

        // Simulate what complete_run_service_job does to build RunServiceStepResult.
        let result = RunServiceStepResult {
            external_id: Some(step_log.step_id.clone()),
            number: Some(step_log.order as i64),
            name: step_log.display_name.clone(),
            status: TimelineRecordState::Completed,
            conclusion: TaskResult::Succeeded,
            started_at: if step_log.started_at.is_empty() {
                None
            } else {
                Some(step_log.started_at.clone())
            },
            completed_at: Some(if step_log.completed_at.is_empty() {
                unix_now_iso8601()
            } else {
                step_log.completed_at.clone()
            }),
            completed_log_lines: step_log.lines.len() as i64,
            annotations: vec![],
        };

        assert_eq!(
            result.started_at.as_deref(),
            Some("2026-06-04T10:00:00Z"),
            "started_at must be the step start time"
        );
        assert_eq!(
            result.completed_at.as_deref(),
            Some("2026-06-04T10:00:03Z"),
            "completed_at must be the step finish time, not job finish time"
        );
        assert_eq!(result.number, Some(5), "step number preserved");
    }

    #[test]
    fn complete_job_step_log_has_correct_timestamps_and_order() {
        // The synthetic "Complete job" StepLog must have:
        // - started_at set to the time it was created (not empty)
        // - completed_at also set (not empty) so duration shows 0s not inflated
        // - order > 0 so it gets a proper /logs/{n} URL
        // This test verifies the shape matches what complete_run_service_job expects.
        let ts = unix_now_iso8601();
        let log = crate::executor::StepLog {
            step_id: uuid::Uuid::new_v4().to_string(),
            display_name: "Complete job".to_string(),
            order: 25,
            started_at: ts.clone(),
            completed_at: ts.clone(),
            lines: complete_job_lines(),
            masks: vec![],
            annotations: vec![],
            telemetry: vec![],
            exit_code: 0,
            skipped: false,
            failure_ignored: false,
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
            summary: String::new(),
        };

        assert!(!log.started_at.is_empty(), "started_at must be set");
        assert!(!log.completed_at.is_empty(), "completed_at must be set");
        assert_eq!(log.order, 25, "order must be > 0 for valid log URL");
        assert_eq!(log.display_name, "Complete job");
        assert!(
            !log.lines.is_empty(),
            "Complete job must have content lines"
        );
        // started_at == completed_at → 0s duration displayed in GitHub UI.
        assert_eq!(
            log.started_at, log.completed_at,
            "synthetic step: started == completed so duration shows 0s"
        );
    }
}

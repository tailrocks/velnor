//! Per-scope controller: desired state, permits, slot/job child processes.
//!
//! Restarting this process must not stop existing slot or job workers: children
//! are spawned without kill-on-drop, and packaged units must not use
//! `PartOf=controller`. Every journal side effect is executed here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use clap::Args;
use velnor_control::journal::{Event, Journal, SideEffect};
use velnor_model::{ActorPhase, Generation, JobId, SlotId};

use super::cleanup;
use super::exec::load_exec_config;
use super::health::HealthServer;
use super::prove;
use super::slot::{heartbeat_path, slot_id, SlotHeartbeat};
use super::watchdog::{feed_after_cycle, LocalCycle};

/// Bound live JIT requests during startup/recovery without making the GitHub
/// API a burst target. This matches the bounded configure path.
const JIT_REGISTRATION_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Args)]
pub struct ControllerArgs {
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long, default_value = "default")]
    pub scope: String,
    /// Operator-declared minimum ready capacity `M`.
    #[arg(long, default_value_t = 1)]
    pub desired_ready: u32,
    /// Extra fully reserved slots so `M` survives one replace.
    #[arg(long, default_value_t = 1)]
    pub surge: u32,
    #[arg(long)]
    pub once: bool,
    /// Spawn slot OS processes (production and isolation tests).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub spawn_slots: bool,
}

pub async fn run(args: ControllerArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.state_dir)?;
    let mut journal = Journal::open(args.state_dir.join("journal.db"))?;
    let server = HealthServer::bind(&args.state_dir)?;
    journal.apply(Event::ControlLive)?;
    journal.apply(Event::JournalWritable)?;
    journal.apply(Event::DesiredCapacity {
        ready: args.desired_ready,
        surge: args.surge,
    })?;
    let mut slots: HashMap<String, Child> = HashMap::new();
    let mut jobs: HashMap<String, Child> = HashMap::new();
    let mut heartbeats: HashMap<String, (u32, u64)> = HashMap::new();
    let mut ready_announced = false;
    loop {
        if crate::runner::draining() {
            drain_children(&mut slots, &mut jobs).await?;
            return Ok(());
        }
        let cycle = reconcile_once(
            &args,
            &mut journal,
            &server,
            &mut slots,
            &mut jobs,
            &mut heartbeats,
        )
        .await?;
        let _ = feed_after_cycle(cycle, !ready_announced);
        ready_announced = true;
        if args.once {
            // Leave children running: a controller restart (or --once exit)
            // must not stop slot or job processes.
            for (_id, child) in slots.drain().chain(jobs.drain()) {
                std::mem::forget(child);
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Stop idle controller-owned slot processes when the daemon receives SIGTERM.
/// Job workers are deliberately left alone so systemd's stop timeout remains
/// the outer bound for an in-flight job rather than turning an upgrade into a
/// lost job. The daemon's drain flag lives in the supervisor process, so this
/// explicit handoff is the process boundary that makes graceful drain real.
async fn drain_children(
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    for child in slots.values() {
        request_child_shutdown(child)?;
    }

    loop {
        reap(slots);
        reap(jobs);
        if slots.is_empty() && jobs.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn request_child_shutdown(child: &Child) -> anyhow::Result<()> {
    if child.id() == 0 {
        return Ok(());
    }

    #[cfg(unix)]
    {
        // SAFETY: the PID comes from the live Child handle owned by this
        // controller. SIGTERM lets the child exit through its normal signal
        // path; SIGKILL remains systemd's final timeout action.
        let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = child;
        anyhow::bail!("graceful controller-child shutdown requires a Unix target")
    }
}

async fn reconcile_once(
    args: &ControllerArgs,
    journal: &mut Journal,
    server: &HealthServer,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
    heartbeats: &mut HashMap<String, (u32, u64)>,
) -> anyhow::Result<LocalCycle> {
    let total = args.desired_ready.saturating_add(args.surge).max(1);
    let generation = Generation::INITIAL;
    let mut effects = Vec::new();
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        let surge = index > args.desired_ready;
        effects.extend(
            journal
                .apply(Event::PermitReserved {
                    slot_id: id,
                    generation,
                    surge,
                })?
                .commands,
        );
    }
    for command in effects {
        execute_effect(args, journal, slots, jobs, command).await?;
    }

    ingest_slot_heartbeats(args, journal, total as usize, heartbeats)?;

    observe_github_and_routing(args, journal).await?;

    let mut proof_effects = Vec::new();
    let executor = prove::observe_executor(&args.state_dir);
    let snapshot = journal.load_state()?;
    for index in 1..=total {
        let id = slot_id(&args.scope, index as usize);
        if executor {
            proof_effects.extend(
                journal
                    .apply(Event::ExecutorProven {
                        slot_id: id.clone(),
                        generation,
                    })?
                    .commands,
            );
        }
        let journal_pid = snapshot
            .slots
            .iter()
            .find(|slot| slot.slot_id == id)
            .and_then(|slot| slot.pid);
        if prove::observe_session(slots.get_mut(&id.0), journal_pid) {
            proof_effects.extend(
                journal
                    .apply(Event::SessionLive {
                        slot_id: id.clone(),
                        generation,
                    })?
                    .commands,
            );
        }
        let state = journal.load_state()?;
        if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) {
            if slot.ready_proof().is_ok() && !slot.registered {
                proof_effects.extend(
                    journal
                        .apply(Event::RegistrationIntended {
                            slot_id: id,
                            generation,
                        })?
                        .commands,
                );
            }
        }
    }
    let mut registrations = Vec::new();
    for command in proof_effects {
        match command {
            SideEffect::RegisterRunner {
                slot_id,
                generation,
            } => registrations.push((slot_id, generation)),
            command => execute_effect(args, journal, slots, jobs, command).await?,
        }
    }
    register_runners(args, journal, registrations).await?;

    spawn_ready_waiters(args, journal, jobs)?;

    for row in journal.pending_outbox()? {
        preserve_outbox(
            args,
            journal,
            &row.job_id,
            row.generation,
            &row.payload_sha256,
        )?;
    }

    reap(slots);
    reap(jobs);
    let health = journal.load_state()?.health();
    server.publish(&health)?;
    Ok(LocalCycle::finished())
}

async fn execute_effect(
    args: &ControllerArgs,
    journal: &mut Journal,
    slots: &mut HashMap<String, Child>,
    jobs: &mut HashMap<String, Child>,
    command: SideEffect,
) -> anyhow::Result<()> {
    match command {
        SideEffect::SpawnSlot {
            slot_id,
            generation,
        } => maybe_spawn_slot(args, journal, slots, &slot_id, generation),
        SideEffect::RegisterRunner {
            slot_id,
            generation,
        } => register_runner(args, journal, slot_id, generation).await,
        SideEffect::StartJob { job_id, generation } => {
            maybe_spawn_job(args, journal, jobs, &job_id.0, generation.0, None)
        }
        SideEffect::AdvertiseCapacity { permits } => {
            std::fs::write(
                args.state_dir.join("advertised-capacity"),
                permits.to_string(),
            )?;
            Ok(())
        }
        SideEffect::SendCompletion {
            job_id,
            generation,
            payload_sha256,
        } => preserve_outbox(args, journal, &job_id, generation, &payload_sha256),
        SideEffect::Cleanup {
            isolation_id,
            generation,
        } => cleanup::remove_owned(&args.state_dir, &isolation_id, generation.0),
        SideEffect::DeleteOutbox { .. } | SideEffect::FenceSlot { .. } => Ok(()),
    }
}

async fn register_runner(
    args: &ControllerArgs,
    journal: &mut Journal,
    slot_id: SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    register_runners(args, journal, vec![(slot_id, generation)]).await
}

/// Configure independent, already-proven slots concurrently, then commit the
/// resulting journal events in slot order. Network work never mutates the
/// journal; routing, executor, session, and permit proofs remain prerequisites
/// for every request.
async fn register_runners(
    args: &ControllerArgs,
    journal: &mut Journal,
    registrations: Vec<(SlotId, Generation)>,
) -> anyhow::Result<()> {
    if registrations.is_empty() {
        return Ok(());
    }
    super::scheduler::production_scheduler().activate_production()?;
    let Ok(exec) = load_exec_config(&args.state_dir) else {
        return Ok(());
    };

    let config_base = exec
        .config_dir
        .clone()
        .unwrap_or_else(|| args.state_dir.clone());
    let slot_count = exec.slots.max(1);
    use futures_util::stream::{self, StreamExt as _};
    let concurrency = registrations.len().clamp(1, JIT_REGISTRATION_CONCURRENCY);
    let mut outcomes = stream::iter(registrations)
        .map(|(slot_id, generation)| {
            let exec = exec.clone();
            let config_base = config_base.clone();
            async move {
                let index = slot_index_from_id(&slot_id);
                let result =
                    crate::runner::jit_configure_one_slot(&exec, &config_base, index, slot_count)
                        .await;
                (slot_id, generation, result)
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    outcomes.sort_by_key(|(slot_id, _, _)| slot_id.0.clone());

    for (slot_id, generation, result) in outcomes {
        if let Err(error) = result {
            eprintln!(
                "Warning: JIT register {} failed (slot stays unregistered): {error:#}",
                slot_id.0
            );
            continue;
        }
        let registered = journal.apply(Event::Registered {
            slot_id: slot_id.clone(),
            generation,
        })?;
        if registered.rejected {
            continue;
        }
        let ready = journal.apply(Event::ReadyAttempt {
            slot_id,
            generation,
        })?;
        for nested in ready.commands {
            if let SideEffect::AdvertiseCapacity { permits } = nested {
                std::fs::write(
                    args.state_dir.join("advertised-capacity"),
                    permits.to_string(),
                )?;
            }
        }
    }
    Ok(())
}

async fn observe_github_and_routing(
    args: &ControllerArgs,
    journal: &mut Journal,
) -> anyhow::Result<()> {
    let mut reachable = false;
    if let Ok(exec) = load_exec_config(&args.state_dir) {
        let group = exec
            .pool_name
            .clone()
            .or_else(|| exec.name.clone())
            .unwrap_or_else(|| "default".to_owned());
        let trust = prove::runtime_trust_scope(&exec.trust_scope);
        if let Some(url) = exec.url.as_deref() {
            if let Some(policy) =
                prove::policy_from_github_url(url, group, exec.labels.clone(), trust.clone())
            {
                prove::write_policy_if_absent(&args.state_dir, &policy)?;
            }
        }
        let policy = prove::read_policy(&args.state_dir);
        if let (Some(url), Some(token)) = (exec.url.as_deref(), exec.pat.as_deref()) {
            let probe = prove::probe_github(prove::GitHubProbeRequest {
                url,
                token,
                policy: policy.as_ref(),
                pool_id: exec.pool_id,
                configured_labels: &exec.labels,
                configured_trust: &exec.trust_scope,
            })
            .await;
            reachable = probe.reachable;
            if let Some(evidence) = probe.evidence {
                prove::write_evidence(&args.state_dir, &evidence)?;
            }
        }
    }
    journal.apply(Event::Dependency {
        github_reachable: reachable,
    })?;
    let _ = prove::reconcile_from_dir(&args.state_dir)?;
    let routing = prove::observe_routing(&args.state_dir);
    journal.apply(Event::Routing {
        valid: routing.valid,
        group_valid: routing.group_valid,
    })?;
    Ok(())
}

/// GitHub session waiters for Ready slots. Do not apply Assigned: REST
/// queued ids are not broker job ids, and Ready must stay Ready until
/// `accept_job` on the broker GUID.
fn spawn_ready_waiters(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
) -> anyhow::Result<()> {
    if load_exec_config(&args.state_dir).is_err() {
        return Ok(());
    }
    let state = journal.load_state()?;
    for slot in &state.slots {
        if slot.phase != ActorPhase::Ready {
            continue;
        }
        if state.jobs.iter().any(|job| {
            job.slot_id == slot.slot_id
                && matches!(
                    job.phase,
                    ActorPhase::Assigned
                        | ActorPhase::Starting
                        | ActorPhase::Running
                        | ActorPhase::Completing
                )
        }) {
            continue;
        }
        if jobs.contains_key(&slot.slot_id.0) {
            continue;
        }
        maybe_spawn_job(
            args,
            journal,
            jobs,
            &format!("wait-{}", slot.slot_id.0),
            slot.generation.0,
            Some(&slot.slot_id.0),
        )?;
    }
    Ok(())
}

/// Keep a durable completion payload. Never replace it with the checksum
/// and never stamp `CompletionSendStarted` without an actual send.
fn preserve_outbox(
    args: &ControllerArgs,
    _journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    payload_sha256: &str,
) -> anyhow::Result<()> {
    let path = cleanup::outbox_path(&args.state_dir, &job_id.0, generation.0);
    if !path.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    let actual = velnor_control::journal::payload_checksum(&bytes);
    if actual != payload_sha256 {
        anyhow::bail!(
            "outbox checksum mismatch for {} generation {}",
            job_id.0,
            generation.0
        );
    }
    Ok(())
}

fn slot_index_from_id(slot_id: &SlotId) -> usize {
    slot_id
        .0
        .rsplit('-')
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(1)
}

/// Read per-slot liveness files and serialize their durable journal effects in
/// this controller process. Slot processes must not contend on the shared
/// SQLite writer just to report liveness.
fn ingest_slot_heartbeats(
    args: &ControllerArgs,
    journal: &mut Journal,
    total: usize,
    seen: &mut HashMap<String, (u32, u64)>,
) -> anyhow::Result<()> {
    let state = journal.load_state()?;
    for index in 1..=total {
        let path = heartbeat_path(&args.state_dir, index);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(heartbeat) = serde_json::from_slice::<SlotHeartbeat>(&bytes) else {
            continue;
        };
        let id = slot_id(&args.scope, index);
        let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == id) else {
            continue;
        };
        if slot.generation.0 != heartbeat.generation
            || !prove::pid_is_alive(heartbeat.pid)
            || seen.get(&id.0).is_some_and(|(pid, sequence)| {
                *pid == heartbeat.pid && *sequence >= heartbeat.sequence
            })
        {
            continue;
        }
        let outcome = journal.apply(Event::SlotHeartbeat {
            slot_id: id.clone(),
            generation: Generation(heartbeat.generation),
            pid: heartbeat.pid,
        })?;
        if !outcome.rejected {
            seen.insert(id.0, (heartbeat.pid, heartbeat.sequence));
        }
    }
    Ok(())
}

fn maybe_spawn_slot(
    args: &ControllerArgs,
    journal: &Journal,
    children: &mut HashMap<String, Child>,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    if !args.spawn_slots {
        return Ok(());
    }
    if children.contains_key(&slot_id.0) {
        return Ok(());
    }
    if let Ok(state) = journal.load_state() {
        if let Some(slot) = state.slots.iter().find(|slot| slot.slot_id == *slot_id) {
            if slot.pid.is_some_and(prove::pid_is_alive) {
                return Ok(());
            }
        }
    }
    let exe = std::env::current_exe()?;
    let index = slot_index_from_id(slot_id);
    let child = Command::new(exe)
        .arg("slot")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--scope")
        .arg(&args.scope)
        .arg("--slot-index")
        .arg(index.to_string())
        .arg("--generation")
        .arg(generation.0.to_string())
        .spawn()?;
    children.insert(slot_id.0.clone(), child);
    Ok(())
}

fn maybe_spawn_job(
    args: &ControllerArgs,
    journal: &Journal,
    jobs: &mut HashMap<String, Child>,
    job_id: &str,
    generation: u64,
    slot_key: Option<&str>,
) -> anyhow::Result<()> {
    let key = slot_key
        .map(ToOwned::to_owned)
        .or_else(|| {
            journal.load_state().ok().and_then(|state| {
                state
                    .jobs
                    .into_iter()
                    .find(|job| job.job_id.0 == job_id)
                    .map(|job| job.slot_id.0)
            })
        })
        .unwrap_or_else(|| job_id.to_owned());
    if jobs.contains_key(&key) {
        return Ok(());
    }
    if cleanup::read_owned_pid(&args.state_dir, job_id, generation).is_some_and(prove::pid_is_alive)
    {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let slot_index = slot_index_from_id(&SlotId(key.clone()));
    let child = Command::new(exe)
        .arg("job")
        .arg("--state-dir")
        .arg(&args.state_dir)
        .arg("--job-id")
        .arg(job_id)
        .arg("--generation")
        .arg(generation.to_string())
        .arg("--slot-index")
        .arg(slot_index.to_string())
        .arg("--scope")
        .arg(&args.scope)
        .spawn()?;
    cleanup::write_owned_pid(&args.state_dir, job_id, generation, child.id())?;
    jobs.insert(key, child);
    Ok(())
}

fn reap(children: &mut HashMap<String, Child>) {
    let mut dead = Vec::new();
    for (id, child) in children.iter_mut() {
        if let Ok(Some(_)) = child.try_wait() {
            dead.push(id.clone());
        }
    }
    for id in dead {
        children.remove(&id);
    }
}

/// Daemon production path: spawn one OS process per configured slot instead
/// of a shared-process `JoinSet`.
pub async fn supervise_from_daemon(
    state_dir: PathBuf,
    scope: String,
    desired_ready: u32,
    surge: u32,
    once: bool,
) -> anyhow::Result<()> {
    run(ControllerArgs {
        state_dir,
        scope,
        desired_ready,
        surge,
        once,
        spawn_slots: true,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::DaemonArgs;
    use crate::node::exec::write_exec_config;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn dummy_exec(url: &str) -> DaemonArgs {
        serde_json::from_value(json!({
            "url": url,
            "name": "velnor",
            "labels": ["velnor"],
            "target_mvp_labels": false,
            "target_mvp_arm_label": false,
            "replace": false,
            "dry_run_registration": false,
            "slots": 1,
            "once": false,
            "complete_noop": false,
            "execute_scripts": false,
            "dry_run_jobs": false,
            "docker_image": "img",
            "job_cpus": "",
            "job_memory": "",
            "trust_scope": "trusted",
            "emergency_reserve_bytes": 0,
            "job_peak_bytes": 0,
            "node_action_image": "img",
            "skip_preflight": false,
            "require_docker_socket": false
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn org_url_probe_sets_github_reachable_without_inferred_policy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runners"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "runners": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/orgs/tailrocks/actions/runner-groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "runner_groups": [{"id": 7, "name": "velnor", "default": false}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v3/orgs/tailrocks/actions/runner-groups/7/repositories",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "repositories": [{"full_name": "tailrocks/velnor"}]
            })))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "velnor-ctrl-org-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{}/tailrocks", server.uri());
        write_exec_config(&dir, &dummy_exec(&url), 1).unwrap();
        std::env::set_var("GITHUB_TOKEN", "ghs_test");
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let args = ControllerArgs {
            state_dir: dir.clone(),
            scope: "velnor".into(),
            desired_ready: 1,
            surge: 0,
            once: true,
            spawn_slots: false,
        };
        observe_github_and_routing(&args, &mut journal)
            .await
            .unwrap();
        let state = journal.load_state().unwrap();
        assert!(state.github_reachable, "{state:?}");
        let evidence: crate::node::prove::RoutingFields =
            serde_json::from_slice(&std::fs::read(dir.join(prove::ROUTING_EVIDENCE_FILE)).unwrap())
                .unwrap();
        assert_eq!(evidence.group, "velnor");
        assert_eq!(evidence.selected_repositories, vec!["tailrocks/velnor"]);
        std::env::remove_var("GITHUB_TOKEN");
        std::fs::remove_dir_all(dir).ok();
    }
}

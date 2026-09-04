use crate::{
    executor::{CommandRunner, StepLogicFailure},
    job_message::{
        ActionReferenceType, ActionStep, AgentJobRequestMessage, RepositoryResource,
        ServiceEndpoint,
    },
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rustix::fs::{flock, FlockOperation};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    pub step_id: String,
    pub display_name: String,
    pub clone_url: String,
    pub version: Option<String>,
    pub destination: PathBuf,
    pub token: Option<String>,
    pub fetch_depth: Option<u32>,
    pub fetch_tags: bool,
    pub persist_credentials: bool,
    pub clean: bool,
    /// `lfs: true` input — download Git LFS objects during checkout. Default false
    /// (matches actions/checkout: leave LFS pointers, do not fetch blobs).
    pub lfs: bool,
    pub condition: Option<String>,
    pub continue_on_error: bool,
    pub timeout_minutes: Option<u64>,
}

impl CheckoutPlan {
    pub fn requires_runtime_context(&self) -> bool {
        // GitHub's job message sets `Condition: "success()"` on every step, so
        // a trivial default condition must not force the checkout onto the
        // runtime-deferred path — local composite actions are resolved from the
        // workspace right after the eager checkouts, and a job whose only
        // checkout is deferred can never resolve them (observed live on the
        // fixture: "action metadata not found in …/.github/actions/…").
        if self
            .condition
            .as_deref()
            .map(str::trim)
            .is_some_and(|condition| {
                !condition.is_empty()
                    && !condition.eq_ignore_ascii_case("success()")
                    && !condition.eq_ignore_ascii_case("always()")
            })
        {
            return true;
        }
        self.version
            .as_deref()
            .is_some_and(contains_step_context_expression)
            || self
                .token
                .as_deref()
                .is_some_and(contains_step_context_expression)
    }
}

pub fn checkout_plans(
    job: &AgentJobRequestMessage,
    workspace_host: &Path,
) -> Result<Vec<CheckoutPlan>> {
    let mut plans = Vec::new();
    for (index, step) in job.steps.iter().enumerate() {
        if !step.enabled || !is_checkout_step(step) {
            continue;
        }
        plans.push(checkout_plan(job, workspace_host, step, index)?);
    }
    Ok(plans)
}

pub(crate) fn checkout_plan(
    job: &AgentJobRequestMessage,
    workspace_host: &Path,
    step: &ActionStep,
    index: usize,
) -> Result<CheckoutPlan> {
    let self_repository = self_repository(job)?;
    let checkout_repository = checkout_repository(step);
    let server_url = checkout_server_url(job, &self_repository);
    let clone_url = checkout_clone_url(
        checkout_repository.as_deref(),
        &self_repository,
        &server_url,
    )?;
    let destination = workspace_host.join(checkout_path(step)?);
    let reference_name = step
        .reference
        .as_ref()
        .and_then(|r| r.name.as_deref())
        .unwrap_or("");
    let reference_ref = step
        .reference
        .as_ref()
        .and_then(|r| r.git_ref.as_deref())
        .unwrap_or("");
    let display_name = step.display_name_template().unwrap_or_else(|| {
        if reference_name.is_empty() {
            String::new()
        } else if reference_ref.is_empty() {
            format!("Run {reference_name}")
        } else {
            format!("Run {reference_name}@{reference_ref}")
        }
    });
    let token = checkout_token(step, job)?.or_else(|| {
        // Prefer system.github.token (the GITHUB_TOKEN with repo access) over
        // SystemVssConnection's AccessToken (runner OAuth token, no repo scope).
        job.variables
            .get("system.github.token")
            .and_then(|v| v.value.clone())
            .filter(|v| !v.is_empty())
            .or_else(|| system_access_token(job.system_connection()))
    });
    Ok(CheckoutPlan {
        step_id: checkout_step_id(step, index),
        display_name,
        clone_url,
        version: checkout_version(job, step, checkout_repository.as_deref(), &self_repository),
        destination,
        token,
        fetch_depth: checkout_fetch_depth(step)?,
        fetch_tags: checkout_fetch_tags(step),
        persist_credentials: checkout_persist_credentials(step),
        clean: checkout_clean(step),
        lfs: checkout_lfs(step),
        condition: step.condition.clone(),
        continue_on_error: crate::script_step::step_continue_on_error(step),
        timeout_minutes: crate::script_step::step_timeout_minutes(step),
    })
}

#[cfg(test)]
fn has_unsupported_enabled_action(steps: &[ActionStep]) -> bool {
    steps.iter().any(|step| {
        step.enabled
            && step.reference_type() != Some(ActionReferenceType::Script)
            && !is_checkout_step(step)
    })
}

#[cfg(test)]
pub fn execute_checkout<R>(runner: &mut R, plan: &CheckoutPlan, log: &mut Vec<String>) -> Result<()>
where
    R: CommandRunner,
{
    execute_checkout_with_mirror(runner, plan, log, None)
}

pub fn execute_checkout_with_mirror<R>(
    runner: &mut R,
    plan: &CheckoutPlan,
    log: &mut Vec<String>,
    mirror_store: Option<&Path>,
) -> Result<()>
where
    R: CommandRunner,
{
    // Any credential an aborted job left on this host is unowned once its
    // process is gone; take it out before adding another one.
    if let Err(error) = reap_stale_checkout_credentials() {
        eprintln!("Warning: could not reap stale checkout credentials: {error:#}");
    }
    let mirror = match mirror_store {
        // A mirror failure used to warn and fall back to a direct checkout,
        // which turned one broken mirror into a permanent, silent slowdown for
        // every later job. `ensure_mirror` repairs what it can; what is left is
        // a real failure and fails the checkout.
        Some(store) => match crate::git_mirror::ensure_mirror(
            runner,
            store,
            &plan.clone_url,
            plan.token.as_deref(),
            &mirror_want(plan),
        ) {
            Ok(mirror) => Some(mirror),
            Err(error) => {
                // The mirror is part of this step, so its git output belongs in
                // the step log and its exit code in the step result.
                for line in format!("{error:#}").lines() {
                    log.push(line.to_string());
                }
                return Err(error.context(format!("prepare git mirror for {}", plan.clone_url)));
            }
        },
        None => None,
    };
    fetch_git_ref(
        runner,
        &plan.clone_url,
        plan.version.as_deref().unwrap_or("HEAD"),
        &plan.destination,
        plan.token.as_deref(),
        plan.fetch_depth,
        plan.fetch_tags,
        plan.persist_credentials,
        plan.clean,
        plan.lfs,
        mirror.as_ref(),
        log,
    )?;
    normalize_checkout_mtimes(runner, &plan.destination, log);
    Ok(())
}

fn mirror_want(plan: &CheckoutPlan) -> crate::git_mirror::MirrorWant {
    crate::git_mirror::MirrorWant {
        git_ref: plan.version.clone().unwrap_or_else(|| "HEAD".to_string()),
        full_history: plan.fetch_depth.is_none(),
        tags: plan.fetch_tags,
    }
}

/// Pin every checked-out file's mtime to the commit timestamp, so two jobs
/// checking out the SAME commit see identical mtimes. Cargo fingerprints path
/// dependencies by mtime: with fresh per-job checkouts every workspace crate
/// looked dirty on every job, recompiling the whole workspace even when
/// sccache and the persistent target dir were warm. Best-effort: any failure
/// (no git, odd fs) leaves mtimes as checked out.
fn normalize_checkout_mtimes<R>(runner: &mut R, destination: &Path, log: &mut Vec<String>)
where
    R: CommandRunner,
{
    let args = [
        "-C".to_string(),
        path_arg(destination),
        "log".to_string(),
        "-1".to_string(),
        "--format=%ct".to_string(),
    ];
    let Ok(result) = runner.run("git", &args) else {
        return;
    };
    if result.code != 0 {
        return;
    }
    let Ok(commit_secs) = result.stdout.trim().parse::<u64>() else {
        return;
    };
    let commit_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(commit_secs);
    let mut pending = vec![destination.to_path_buf()];
    let mut touched = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(path);
                }
            } else if file_type.is_file()
                && let Ok(file) = std::fs::File::options().append(true).open(&path)
                && file.set_modified(commit_time).is_ok()
            {
                touched += 1;
            }
        }
        // Directories too: cargo's `rerun-if-changed=<dir>` fingerprints the
        // directory mtime, so an unpinned dir re-runs build scripts (and
        // recompiles their crates) on every fresh checkout — observed live as
        // blockchain-explorer rebuilding 28s of clippy per warm run because
        // its build.rs tracks the proto include directories. Setting file
        // times does not touch the parent dir, so order is irrelevant.
        if let Ok(handle) = std::fs::File::open(&dir)
            && handle.set_modified(commit_time).is_ok()
        {
            touched += 1;
        }
    }
    if touched > 0 {
        log.push(format!(
            "Pinned {touched} file and directory mtimes to the commit timestamp (stable cargo fingerprints across jobs)"
        ));
    }
}

/// Run the post-checkout credential cleanup for each plan, returning the
/// GitHub-style git-command trace for each (aligned with `plans` by index) so
/// the "Post Run actions/checkout" step log shows the cleanup instead of being
/// empty. A plan that has nothing to clean yields an empty trace.
pub fn cleanup_checkout_credentials<R>(
    runner: &mut R,
    plans: &[CheckoutPlan],
) -> Result<Vec<Vec<String>>>
where
    R: CommandRunner,
{
    let mut traces = Vec::with_capacity(plans.len());
    for plan in plans {
        let mut log = Vec::new();
        cleanup_checkout_credential(runner, plan, &mut log)?;
        traces.push(log);
    }
    Ok(traces)
}

fn cleanup_checkout_credential<R>(
    runner: &mut R,
    plan: &CheckoutPlan,
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    // Retire the crash journal entries for this workspace first: from here on
    // this function owns removing the credential, and a stale entry would let a
    // later reaper scrub a workspace that has already been handed back.
    let registered = release_registered_credentials(&plan.destination);
    let git_dir = plan.destination.join(".git");
    if !git_dir.exists() {
        return Ok(());
    }
    let config_path = git_dir.join("config");
    // Cleanup is unconditional. Keying it off `persist_credentials` assumed the
    // only writer of the credential was the persist path, which left every
    // other write (an interrupted run, an lfs checkout, a reused workspace)
    // permanently on disk.
    let args = [
        "-C".to_string(),
        path_arg(&plan.destination),
        "config".to_string(),
        "--local".to_string(),
        "--unset-all".to_string(),
        git_extraheader_key(&plan.clone_url),
    ];
    log.push(format!("[command]git {}", format_git_args(&args)));
    let result = runner.run("git", &args)?;
    for line in result.stdout.lines().chain(result.stderr.lines()) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            log.push(trimmed.to_string());
        }
    }
    // Exit 5 is "the key was not set", the expected state for a checkout that
    // never persisted anything. Anything else is a real failure and must fail
    // the job: a warning on stderr let a credentialed workspace survive.
    if result.code != 0 && result.code != GIT_CONFIG_KEY_NOT_FOUND {
        bail!(
            "cleanup of checkout credentials in {} failed with code {}: {}",
            plan.destination.display(),
            result.code,
            result.stderr.trim()
        );
    }
    // Verify rather than trust. `git config --unset-all` removes one key; the
    // invariant this function has to hold is that no credential of any scope
    // survives in the workspace at all.
    if scrub_config_credentials(&config_path)? {
        log.push(format!(
            "Removed a residual credential from {}",
            config_path.display()
        ));
    }
    if config_has_credential(&config_path) {
        bail!(
            "checkout credential still present in {} after cleanup",
            config_path.display()
        );
    }
    if registered > 0 {
        log.push(format!(
            "Released {registered} tracked checkout credential(s)"
        ));
    }
    Ok(())
}

/// `git config --unset-all` exit code for "the key does not exist".
const GIT_CONFIG_KEY_NOT_FOUND: i32 = 5;

/// One persisted credential, tracked for as long as it exists on disk.
///
/// The journal file is held under an exclusive `flock` for the whole lifetime
/// of the credential. The kernel drops that lock when the owning process dies,
/// however it dies, so a later reaper can tell a live credential from one an
/// aborted runner left behind without pid liveness guesswork.
struct CredentialRegistration {
    destination: PathBuf,
    journal_path: PathBuf,
    _journal: File,
}

fn active_credentials() -> &'static Mutex<Vec<CredentialRegistration>> {
    static ACTIVE: OnceLock<Mutex<Vec<CredentialRegistration>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

fn credential_journal_dir() -> PathBuf {
    crate::storage::StorageLayout::resolve()
        .or_else(|| crate::storage::StorageLayout::user_cli().ok())
        .map(|layout| layout.run_root)
        .unwrap_or_else(|| std::env::temp_dir().join("velnor"))
        .join("checkout-credentials")
}

/// Record that `destination` is about to hold a credential, before it does.
fn register_credential(destination: &Path) -> Result<()> {
    register_credential_in(&credential_journal_dir(), destination)
}

fn register_credential_in(dir: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("create checkout credential journal {}", dir.display()))?;
    let journal_path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    let mut journal = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&journal_path)
        .with_context(|| format!("open checkout credential journal {journal_path:?}"))?;
    flock(&journal, FlockOperation::NonBlockingLockExclusive)
        .with_context(|| format!("lock checkout credential journal {journal_path:?}"))?;
    let record = serde_json::json!({
        "config": destination.join(".git/config").display().to_string(),
        "workspace": destination.display().to_string(),
        "pid": std::process::id(),
    });
    journal
        .write_all(record.to_string().as_bytes())
        .and_then(|()| journal.sync_all())
        .with_context(|| format!("write checkout credential journal {journal_path:?}"))?;
    active_credentials()
        .lock()
        .map_err(|_| anyhow::anyhow!("checkout credential registry poisoned"))?
        .push(CredentialRegistration {
            destination: destination.to_path_buf(),
            journal_path,
            _journal: journal,
        });
    Ok(())
}

/// Drop the journal entries for a workspace whose credential has been removed.
fn release_registered_credentials(destination: &Path) -> usize {
    let Ok(mut active) = active_credentials().lock() else {
        return 0;
    };
    let mut released = 0;
    active.retain(|registration| {
        if registration.destination != destination {
            return true;
        }
        fs::remove_file(&registration.journal_path).ok();
        released += 1;
        false
    });
    released
}

/// Scrub every credential an aborted runner left behind.
///
/// A journal entry whose exclusive lock can be taken has no live owner, so the
/// credential it names is unowned and is removed. Entries of running jobs — in
/// this process or any other on the host — stay locked and are left alone.
///
/// # Errors
/// The journal directory exists but cannot be read.
pub fn reap_stale_checkout_credentials() -> Result<usize> {
    reap_stale_credentials_in(&credential_journal_dir())
}

fn reap_stale_credentials_in(dir: &Path) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read checkout credential journal {}", dir.display()))
        }
    };
    let mut reaped = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
            // A live job owns this credential.
            continue;
        }
        let config = fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
            .and_then(|record| {
                record
                    .get("config")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            });
        if let Some(config) = config
            && scrub_config_credentials(&config).unwrap_or(false)
        {
            eprintln!(
                "Removed a checkout credential left behind by an aborted job: {}",
                config.display()
            );
            reaped += 1;
        }
        fs::remove_file(&path).ok();
    }
    Ok(reaped)
}

/// Remove every `extraheader` entry from a git config file, returning whether
/// anything was removed. Velnor is the only writer of `http.*.extraheader` in a
/// checkout, and every value it writes is a bearer credential.
fn scrub_config_credentials(config_path: &Path) -> Result<bool> {
    let Ok(content) = fs::read_to_string(config_path) else {
        return Ok(false);
    };
    let (scrubbed, changed) = strip_extraheaders(&content);
    if !changed {
        return Ok(false);
    }
    let temporary = config_path.with_extension(format!("velnor-scrub-{}", std::process::id()));
    fs::write(&temporary, scrubbed)
        .with_context(|| format!("write scrubbed git config {}", temporary.display()))?;
    fs::rename(&temporary, config_path)
        .with_context(|| format!("replace git config {}", config_path.display()))?;
    Ok(true)
}

fn config_has_credential(config_path: &Path) -> bool {
    fs::read_to_string(config_path).is_ok_and(|content| {
        content.lines().any(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("extraheader")
        })
    })
}

/// Drop `extraheader` keys, and any `[http …]` section left empty by that.
fn strip_extraheaders(content: &str) -> (String, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut pending_http_section: Option<&str> = None;
    let mut in_http_section = false;
    let mut changed = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if let Some(header) = pending_http_section.take() {
                lines.push(header.to_string());
            }
            in_http_section = trimmed.to_ascii_lowercase().starts_with("[http");
            if in_http_section {
                pending_http_section = Some(line);
            } else {
                lines.push(line.to_string());
            }
            continue;
        }
        if in_http_section && trimmed.to_ascii_lowercase().starts_with("extraheader") {
            changed = true;
            continue;
        }
        if !trimmed.is_empty()
            && let Some(header) = pending_http_section.take()
        {
            lines.push(header.to_string());
        }
        lines.push(line.to_string());
    }
    let mut scrubbed = lines.join("\n");
    if !scrubbed.is_empty() {
        scrubbed.push('\n');
    }
    (scrubbed, changed)
}

pub fn configure_safe_directory(
    home_host: &Path,
    workspace_host: &Path,
    destination: &Path,
) -> Result<()> {
    let Some(safe_directory) = checkout_container_path(workspace_host, destination) else {
        return Ok(());
    };
    fs::create_dir_all(home_host).with_context(|| format!("create {}", home_host.display()))?;
    let config_path = home_host.join(".gitconfig");
    let mut config = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_path)
        .with_context(|| format!("open {}", config_path.display()))?;
    writeln!(config, "[safe]\n\tdirectory = {safe_directory}")
        .with_context(|| format!("write {}", config_path.display()))?;
    Ok(())
}

fn checkout_container_path(workspace_host: &Path, destination: &Path) -> Option<String> {
    let relative = destination.strip_prefix(workspace_host).ok()?;
    if relative.as_os_str().is_empty() {
        return Some("/__w".to_string());
    }
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("/__w/{relative}"))
}

#[allow(clippy::too_many_arguments)]
pub fn fetch_git_ref<R>(
    runner: &mut R,
    clone_url: &str,
    git_ref: &str,
    destination: &Path,
    token: Option<&str>,
    fetch_depth: Option<u32>,
    fetch_tags: bool,
    persist_credentials: bool,
    clean: bool,
    lfs: bool,
    mirror: Option<&crate::git_mirror::MirrorCheckout>,
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;

    run_git(runner, &["init".to_string(), path_arg(destination)], log)?;
    // Remove a stale origin only when one exists: on a fresh workspace
    // `git remote remove origin` exits non-zero and prints
    // "error: No such remote: 'origin'" into the job log (noise the
    // GitHub-hosted lane never emits). Probe quietly first; stay tolerant
    // of a removal failure (`|| true`, matching the guest checkout idiom).
    let origin_exists = runner
        .run(
            "git",
            &[
                "-C".to_string(),
                path_arg(destination),
                "remote".to_string(),
                "get-url".to_string(),
                "origin".to_string(),
            ],
        )
        .map(|result| result.code == 0)
        .unwrap_or(false);
    if origin_exists {
        run_git(
            runner,
            &[
                "-C".to_string(),
                path_arg(destination),
                "remote".to_string(),
                "remove".to_string(),
                "origin".to_string(),
            ],
            log,
        )
        .ok();
    }
    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(destination),
            "remote".to_string(),
            "add".to_string(),
            "origin".to_string(),
            clone_url.to_string(),
        ],
        log,
    )?;

    let fetch_env = git_auth_env(clone_url, token);
    if let Some(mirror) = mirror {
        hydrate_from_mirror(
            runner,
            mirror,
            destination,
            clone_url,
            git_ref,
            fetch_depth.is_none(),
            fetch_tags,
            log,
        )?;
    } else {
        run_network_fetch(
            runner,
            destination,
            git_ref,
            fetch_depth,
            fetch_tags,
            &fetch_env,
            log,
        )?;
    }

    // For lfs:true, the git-lfs smudge filter runs during checkout and downloads
    // LFS blobs. It authenticates through the same header the fetch used, which
    // is passed per command rather than written into the workspace config, so
    // no credential outlives the checkout unless the job asked for one.
    let mut checkout = vec!["-C".to_string(), path_arg(destination)];
    if !lfs {
        checkout.extend(lfs_skip_smudge_args());
    }
    checkout.extend([
        "checkout".to_string(),
        "--force".to_string(),
        "FETCH_HEAD".to_string(),
    ]);
    if lfs {
        run_git_with_env_and_display(runner, &checkout, &checkout, &fetch_env, log)?;
    } else {
        run_git(runner, &checkout, log)?;
    }

    if clean {
        let mut reset = vec!["-C".to_string(), path_arg(destination)];
        if !lfs {
            reset.extend(lfs_skip_smudge_args());
        }
        reset.extend([
            "reset".to_string(),
            "--hard".to_string(),
            "HEAD".to_string(),
        ]);
        if lfs {
            run_git_with_env_and_display(runner, &reset, &reset, &fetch_env, log)?;
        } else {
            run_git(runner, &reset, log)?;
        }
        let preserve_workspace_target = std::env::var("VELNOR_CARGO_TARGET_PERSIST")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        run_git(
            runner,
            &checkout_clean_args(destination, preserve_workspace_target),
            log,
        )?;
    }

    if persist_credentials && let Some(token) = token {
        persist_git_credentials(runner, destination, clone_url, token, log)?;
    }

    Ok(())
}

fn run_network_fetch<R>(
    runner: &mut R,
    destination: &Path,
    git_ref: &str,
    fetch_depth: Option<u32>,
    fetch_tags: bool,
    fetch_env: &[(String, String)],
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    let mut fetch = vec![
        "-C".to_string(),
        path_arg(destination),
        "-c".to_string(),
        "protocol.version=2".to_string(),
    ];
    fetch.extend(["fetch".to_string(), "--prune".to_string()]);
    match fetch_depth {
        Some(depth) => {
            if fetch_tags {
                fetch.push("--tags".to_string());
            } else {
                fetch.push("--no-tags".to_string());
            }
            fetch.push(format!("--depth={depth}"));
            fetch.extend(["origin".to_string(), git_ref.to_string()]);
        }
        None => {
            fetch.extend([
                "--tags".to_string(),
                "origin".to_string(),
                // FETCH_HEAD is ordered by requested refspec. Keep the exact
                // workflow ref first: checkout uses FETCH_HEAD below, while
                // the remaining refspecs only populate full history/tags.
                // Putting the wildcard first made FETCH_HEAD resolve to the
                // lexicographically first branch (observed on v0.1.122).
                git_ref.to_string(),
                "+refs/heads/*:refs/remotes/origin/*".to_string(),
                "+refs/tags/*:refs/tags/*".to_string(),
            ]);
        }
    }
    run_git_with_env_and_display(runner, &fetch, &fetch, fetch_env, log)
}

/// Make the mirror's objects available to the workspace without copying them.
///
/// Object files are hard-linked out of the mirror. Git object files are
/// immutable and are only ever added to a repository, so a link is a complete,
/// self-contained copy of the byte content for zero bytes written:
///
/// * A concurrent mirror fetch only adds new object files; it never rewrites
///   the ones already linked here, and files still being written carry a
///   `tmp_` name and are skipped.
/// * `git gc` in the mirror cannot take the bytes away — a hard link keeps the
///   inode alive after the mirror unlinks its own name — and cannot even reach
///   the wanted objects, which the mirror pins under `refs/velnor/*` and never
///   deletes. Auto gc is disabled there regardless.
/// * Deleting the workspace unlinks names only; the mirror is untouched.
/// * The workspace ends up self-contained, unlike `objects/info/alternates`,
///   which would break the moment the container (which does not mount the
///   mirror store) or a rebuild of a corrupt mirror removed the pointee.
fn hydrate_from_mirror<R>(
    runner: &mut R,
    mirror: &crate::git_mirror::MirrorCheckout,
    destination: &Path,
    clone_url: &str,
    git_ref: &str,
    full_history: bool,
    tags: bool,
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    let git_dir = destination.join(".git");
    fs::create_dir_all(&git_dir)
        .with_context(|| format!("create git directory {}", git_dir.display()))?;
    let (linked, bytes) =
        link_object_store(&mirror.path.join("objects"), &git_dir.join("objects"))?;

    let mut refs = 0usize;
    if full_history || tags {
        for (refname, object) in crate::git_mirror::mirror_refs(runner, &mirror.path)? {
            let Some(local) = workspace_ref_name(&refname, full_history, tags) else {
                continue;
            };
            write_loose_ref(&git_dir, &local, &object)?;
            refs += 1;
        }
    }

    // `checkout FETCH_HEAD` stays the checkout command on both paths, so the
    // step log and the resulting worktree do not depend on which path ran.
    write_fetch_head(&git_dir, &mirror.sha, git_ref, clone_url)?;

    log.push(format!(
        "Linked {linked} object file(s) ({bytes} bytes) and {refs} ref(s) from the shared mirror; no objects were copied and no network fetch was needed"
    ));
    Ok(())
}

fn link_object_store(source: &Path, destination: &Path) -> Result<(usize, u64)> {
    let mut linked = 0usize;
    let mut bytes = 0u64;
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let Ok(entries) = fs::read_dir(source.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let child = relative.join(&name);
            if file_type.is_dir() {
                // `info/` holds regenerable caches, and an `alternates` file
                // there would point outside this store.
                if name == "info" {
                    continue;
                }
                pending.push(child);
                continue;
            }
            if !file_type.is_file() || name.to_string_lossy().starts_with("tmp_") {
                continue;
            }
            let target = destination.join(&child);
            if target.exists() {
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create object directory {}", parent.display()))?;
            }
            let origin = source.join(&child);
            match fs::hard_link(&origin, &target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                // A different filesystem (or a filesystem without links) is the
                // only case that still pays for the bytes.
                Err(_) => {
                    fs::copy(&origin, &target).with_context(|| {
                        format!(
                            "copy git object {} to {}",
                            origin.display(),
                            target.display()
                        )
                    })?;
                }
            }
            linked += 1;
            bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        }
    }
    Ok((linked, bytes))
}

fn workspace_ref_name(refname: &str, full_history: bool, tags: bool) -> Option<String> {
    if let Some(branch) = refname.strip_prefix("refs/heads/") {
        return full_history.then(|| format!("refs/remotes/origin/{branch}"));
    }
    if refname.starts_with("refs/tags/") {
        return (full_history || tags).then(|| refname.to_string());
    }
    None
}

fn write_loose_ref(git_dir: &Path, refname: &str, object: &str) -> Result<()> {
    if !is_safe_ref_name(refname) || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("refusing to write unsafe ref '{refname}'")
    }
    let path = git_dir.join(refname);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create ref directory {}", parent.display()))?;
    }
    fs::write(&path, format!("{object}\n")).with_context(|| format!("write ref {}", path.display()))
}

/// Ref names are mirror data, and they become path components here.
fn is_safe_ref_name(refname: &str) -> bool {
    refname.starts_with("refs/")
        && !refname.ends_with('/')
        && refname.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && component.chars().all(|character| {
                    !character.is_control()
                        && !matches!(character, '\\' | ':' | '?' | '*' | '[' | '~' | '^' | ' ')
                })
        })
}

fn write_fetch_head(git_dir: &Path, sha: &str, git_ref: &str, clone_url: &str) -> Result<()> {
    let kind = if git_ref.len() == 40 && git_ref.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        "commit"
    } else {
        "branch"
    };
    let path = git_dir.join("FETCH_HEAD");
    fs::write(
        &path,
        format!("{sha}\t\t{kind} '{git_ref}' of {clone_url}\n"),
    )
    .with_context(|| format!("write {}", path.display()))
}

fn checkout_clean_args(destination: &Path, preserve_workspace_target: bool) -> Vec<String> {
    let mut args = vec![
        "-C".to_string(),
        path_arg(destination),
        "clean".to_string(),
        "-ffdx".to_string(),
    ];
    // The persistent Cargo bucket is bind-mounted at the workflow-visible
    // `target/` path before checkout. Native checkout must keep that one
    // runner-owned cache mount; otherwise `git clean -ffdx` empties the bucket
    // at the beginning of every job and defeats no-change reruns. All other
    // ignored and untracked workspace content retains actions/checkout's clean
    // semantics.
    if preserve_workspace_target {
        args.extend(["-e".to_string(), "target/".to_string()]);
    }
    args
}

/// `git -c` args that make the git-lfs smudge/process filters skip downloading
/// LFS objects, leaving the pointer files in place. This matches the default
/// behavior of `actions/checkout` (`lfs: false`): a repo that uses Git LFS is
/// checked out without fetching LFS blobs, so no LFS credentials are needed.
///
/// Without this, the job image's globally-installed git-lfs runs its smudge
/// filter during `git checkout` and makes its own authenticated request to the
/// LFS endpoint, which fails ("could not read Username for https://github.com")
/// because the credential helper is not configured for the lfs subprocess.
///
/// (LFS download — the `lfs: true` opt-in — is a separate feature; ChainArgos and
/// the fixture both use the default, so skipping is the correct match today.)
fn lfs_skip_smudge_args() -> [String; 6] {
    [
        "-c".to_string(),
        "filter.lfs.smudge=git-lfs smudge --skip -- %f".to_string(),
        "-c".to_string(),
        "filter.lfs.process=git-lfs filter-process --skip".to_string(),
        "-c".to_string(),
        "filter.lfs.required=false".to_string(),
    ]
}

fn persist_git_credentials<R>(
    runner: &mut R,
    destination: &Path,
    clone_url: &str,
    token: &str,
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    let key = git_extraheader_key(clone_url);
    let _ = runner.run(
        "git",
        &[
            "-C".to_string(),
            path_arg(destination),
            "config".to_string(),
            "--local".to_string(),
            "--unset-all".to_string(),
            key.clone(),
        ],
    );
    log.push(format!(
        "[command]git -C {} config --local {} AUTHORIZATION: ***",
        path_arg(destination),
        key
    ));
    let config_path = destination.join(".git/config");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create checkout config dir {}", parent.display()))?;
    }
    // Register before the write, never after: an entry that names a credential
    // which was never written scrubs nothing, while a credential written before
    // its entry exists is untracked if the process dies in between.
    register_credential(destination)?;
    let mut config = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_path)
        .with_context(|| format!("open checkout config {}", config_path.display()))?;
    writeln!(
        config,
        "\n[http \"{}\"]\n\textraheader = {}",
        git_extraheader_scope(clone_url),
        git_basic_auth_value(token)
    )
    .with_context(|| format!("write checkout credential {}", config_path.display()))
}

fn run_git<R>(runner: &mut R, args: &[String], log: &mut Vec<String>) -> Result<()>
where
    R: CommandRunner,
{
    // Echo the command (token masked) the way actions/checkout does — a
    // `[command]git …` line followed by the command's own output — so the
    // checkout step log reads like the GitHub-hosted runner's git trace.
    log.push(format!("[command]git {}", format_git_args(args)));
    let result = runner.run("git", args)?;
    for line in result.stdout.lines().chain(result.stderr.lines()) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            log.push(trimmed.to_string());
        }
    }
    if result.code != 0 {
        return Err(StepLogicFailure::new(
            result.code,
            "",
            format!(
                "git {} failed with code {}: {}",
                format_git_args(args),
                result.code,
                result.stderr
            ),
        )
        .into());
    }
    Ok(())
}

fn run_git_with_env_and_display<R>(
    runner: &mut R,
    args: &[String],
    display_args: &[String],
    env: &[(String, String)],
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    log.push(format!("[command]git {}", format_git_args(display_args)));
    let result = runner.run_with_env("git", args, env)?;
    for line in result.stdout.lines().chain(result.stderr.lines()) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            log.push(trimmed.to_string());
        }
    }
    if result.code != 0 {
        return Err(StepLogicFailure::new(
            result.code,
            "",
            format!(
                "git {} failed with code {}: {}",
                format_git_args(display_args),
                result.code,
                result.stderr
            ),
        )
        .into());
    }
    Ok(())
}

pub(crate) fn is_checkout_step(step: &ActionStep) -> bool {
    step.reference_type() == Some(ActionReferenceType::Repository)
        && step
            .reference
            .as_ref()
            .and_then(|reference| reference.name.as_deref())
            .is_some_and(|name| name.eq_ignore_ascii_case("actions/checkout"))
}

pub(crate) fn checkout_step_id(step: &ActionStep, index: usize) -> String {
    // Prefer context_name (YAML id:) over internal UUID for expression lookup.
    step.context_name
        .as_deref()
        .filter(|n| !n.is_empty() && !n.starts_with("__"))
        .or(step.id.as_deref())
        .or(step.name.as_deref())
        .map(sanitize_segment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("checkout{}", index + 1))
}

fn sanitize_segment(value: &str) -> String {
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

fn contains_step_context_expression(value: &str) -> bool {
    value.contains("${{") && value.contains("steps.")
}

fn self_repository(job: &AgentJobRequestMessage) -> Result<RepositoryResource> {
    if let Some(repository) = job
        .resources
        .repositories
        .iter()
        .find(|repository| repository.alias.as_deref() == Some("self"))
        .or_else(|| job.resources.repositories.first())
    {
        return Ok(repository.clone());
    }

    let name = job_string(job, "github.repository")
        .filter(|name| is_repository_name(name))
        .ok_or_else(|| {
            anyhow::anyhow!("job has no repository resources and no github.repository context")
        })?;
    let server_url = job_string(job, "github.server_url").unwrap_or("https://github.com");
    let clone_url = format!(
        "{}/{}.git",
        server_url.trim_end_matches('/'),
        name.trim_start_matches('/')
    );
    let mut properties = BTreeMap::new();
    properties.insert("cloneUrl".to_string(), clone_url);
    Ok(RepositoryResource {
        alias: Some("self".to_string()),
        name: Some(name.to_string()),
        git_ref: job_string(job, "github.ref").map(ToOwned::to_owned),
        version: job_string(job, "github.sha").map(ToOwned::to_owned),
        url: None,
        properties,
    })
}

fn checkout_path(step: &ActionStep) -> Result<PathBuf> {
    let path = step
        .inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["path", "Path"]))
        .unwrap_or(".");
    if path.starts_with('/') || path.contains("..") {
        bail!("unsupported checkout path '{path}'")
    }
    Ok(PathBuf::from(path))
}

fn checkout_ref(step: &ActionStep) -> Option<String> {
    step.inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["ref", "Ref"]))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn checkout_version(
    job: &AgentJobRequestMessage,
    step: &ActionStep,
    checkout_repository: Option<&str>,
    self_repository: &RepositoryResource,
) -> Option<String> {
    if let Some(reference) = checkout_ref(step) {
        return Some(reference);
    }

    let is_self_checkout = checkout_repository.is_none_or(|repository| {
        self_repository
            .name
            .as_deref()
            .is_some_and(|self_name| repository.eq_ignore_ascii_case(self_name))
    });
    if !is_self_checkout {
        return None;
    }

    // RepositoryResource.version is the server-selected immutable revision.
    // PR merge/head refs are mutable pointers and remain only as a fallback
    // for incomplete job messages that lack the immutable version.
    self_repository.version.clone().or_else(|| {
        self_repository
            .git_ref
            .as_deref()
            .or_else(|| job_string(job, "github.ref"))
            .filter(|reference| is_pull_request_ref(reference))
            .map(ToOwned::to_owned)
    })
}

fn is_pull_request_ref(reference: &str) -> bool {
    let mut parts = reference.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("refs"), Some("pull"), Some(number), Some(kind), None)
            if !number.is_empty()
                && number.bytes().all(|byte| byte.is_ascii_digit())
                && matches!(kind, "merge" | "head")
    )
}

fn checkout_repository(step: &ActionStep) -> Option<String> {
    step.inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["repository", "Repository"]))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn checkout_clone_url(
    requested_repository: Option<&str>,
    self_repository: &RepositoryResource,
    server_url: &str,
) -> Result<String> {
    match requested_repository {
        Some(repository) if !is_repository_name(repository) => {
            bail!("unsupported checkout repository '{repository}'")
        }
        Some(repository)
            if self_repository
                .name
                .as_deref()
                .is_some_and(|self_name| repository.eq_ignore_ascii_case(self_name)) =>
        {
            self_clone_url(self_repository)
        }
        // The server was hardcoded to github.com, so an external-repository
        // checkout on a GHES instance silently fetched a different repository
        // from the public site. The host comes from the job's own repository.
        Some(repository) => Ok(format!(
            "{}/{repository}.git",
            server_url.trim_end_matches('/')
        )),
        None => self_clone_url(self_repository),
    }
}

/// The git server this job belongs to: the origin of the self repository's
/// clone URL, falling back to the `github.server_url` context.
fn checkout_server_url(
    job: &AgentJobRequestMessage,
    self_repository: &RepositoryResource,
) -> String {
    self_clone_url(self_repository)
        .ok()
        .and_then(|url| {
            let parsed = Url::parse(&url).ok()?;
            let host = parsed.host_str()?;
            Some(match parsed.port() {
                Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
                None => format!("{}://{host}", parsed.scheme()),
            })
        })
        .or_else(|| job_string(job, "github.server_url").map(ToOwned::to_owned))
        .unwrap_or_else(|| "https://github.com".to_string())
}

fn self_clone_url(repository: &RepositoryResource) -> Result<String> {
    repository
        .properties
        .get("cloneUrl")
        .or(repository.url.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("self repository missing clone URL"))
}

fn is_repository_name(repository: &str) -> bool {
    let mut parts = repository.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [owner, name].iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
        })
}

fn checkout_fetch_depth(step: &ActionStep) -> Result<Option<u32>> {
    let Some(value) = step
        .inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["fetch-depth", "fetchDepth", "FetchDepth"]))
    else {
        return Ok(Some(1));
    };
    let Some(value) = Some(value).filter(|value| !value.is_empty()) else {
        return Ok(Some(1));
    };
    let depth = value
        .parse::<u32>()
        .with_context(|| format!("parse checkout fetch-depth '{value}'"))?;
    if depth == 0 {
        Ok(None)
    } else {
        Ok(Some(depth))
    }
}

fn checkout_fetch_tags(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["fetch-tags", "fetchTags", "FetchTags"]))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// `lfs` input (actions/checkout). Default false: leave LFS pointers, do not
/// fetch blobs. `true`: download LFS objects during checkout (needs auth).
fn checkout_lfs(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["lfs", "Lfs", "LFS"]))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn checkout_persist_credentials(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| {
            input_string(
                inputs,
                &[
                    "persist-credentials",
                    "persistCredentials",
                    "PersistCredentials",
                ],
            )
        })
        .map(|value| !value.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn checkout_clean(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["clean", "Clean"]))
        .map(|value| !value.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn checkout_token(step: &ActionStep, job: &AgentJobRequestMessage) -> Result<Option<String>> {
    let Some(token) = step
        .inputs
        .as_ref()
        .and_then(|inputs| input_string_or_expression(inputs, &["token", "Token"]))
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if contains_step_context_expression(&token) {
        return Ok(Some(token));
    }
    let Some(resolved) = resolve_token_expression(&token, job).filter(|value| !value.is_empty())
    else {
        bail!("explicit checkout token expression did not resolve");
    };
    Ok(Some(resolved))
}

fn resolve_token_expression(token: &str, job: &AgentJobRequestMessage) -> Option<String> {
    let expression = token
        .trim()
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim);
    let Some(expression) = expression else {
        return Some(token.to_string());
    };
    if expression.eq_ignore_ascii_case("github.token")
        || expression.eq_ignore_ascii_case("secrets.GITHUB_TOKEN")
    {
        // system.github.token is the GITHUB_TOKEN for workflow git/API auth.
        // The SystemVssConnection AccessToken is for the Actions service API — not git auth.
        return job
            .variables
            .get("system.github.token")
            .and_then(|v| v.value.clone())
            .filter(|v| !v.is_empty())
            .or_else(|| system_access_token(job.system_connection()));
    }
    for prefix in ["secrets.", "secret."] {
        if let Some(name) = expression.strip_prefix(prefix) {
            return job
                .variables
                .get(&format!("secrets.{name}"))
                .or_else(|| job.variables.get(&format!("secret.{name}")))
                .or_else(|| job.variables.get(name))
                .and_then(|value| value.value.clone());
        }
    }
    Some(token.to_string())
}

fn system_access_token(endpoint: Option<&ServiceEndpoint>) -> Option<String> {
    endpoint
        .and_then(|endpoint| endpoint.authorization.as_ref())
        .and_then(|authorization| {
            authorization
                .parameters
                .get("AccessToken")
                .or_else(|| authorization.parameters.get("accessToken"))
        })
        .cloned()
}

fn input_string<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    let object = value.as_object()?;
    direct_input_string(object, names).or_else(|| nested_map_input_string(object, names))
}

fn direct_input_string<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(input_value_as_str))
}

fn nested_map_input_string<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    let map = object.get("map").or_else(|| object.get("Map"))?;
    if let Some(map) = map.as_object() {
        return direct_input_string(map, names);
    }
    map.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            let item = item.as_object()?;
            let name = input_name_field(item)?;
            if !names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            item.get("value")
                .or_else(|| item.get("Value"))
                .and_then(input_value_as_str)
        })
    })
}

fn input_name_field(object: &Map<String, Value>) -> Option<&str> {
    ["name", "Name", "key", "Key"]
        .iter()
        .find_map(|name| object.get(*name).and_then(input_value_as_str))
}

fn input_value_as_str(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_object()
            .and_then(|object| direct_input_string(object, &["value", "Value", "lit", "Lit"]))
    })
}

fn input_string_or_expression(value: &Value, names: &[&str]) -> Option<String> {
    input_string(value, names)
        .map(ToOwned::to_owned)
        .or_else(|| input_expression(value, names).map(|expr| format!("${{{{ {expr} }}}}")))
}

fn input_expression<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    let object = value.as_object()?;
    if let Some(expression) = names
        .iter()
        .filter_map(|name| object.get(*name))
        .find_map(expression_value_as_str)
    {
        return Some(expression);
    }
    let map = object.get("map").or_else(|| object.get("Map"))?;
    map.as_array()?.iter().find_map(|item| {
        let item = item.as_object()?;
        let name = input_name_field(item)?;
        if !names
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
        {
            return None;
        }
        item.get("value")
            .or_else(|| item.get("Value"))
            .and_then(expression_value_as_str)
    })
}

fn expression_value_as_str(value: &Value) -> Option<&str> {
    value
        .as_object()?
        .get("expr")
        .or_else(|| value.as_object()?.get("Expr"))
        .and_then(Value::as_str)
}

fn job_string<'a>(job: &'a AgentJobRequestMessage, name: &str) -> Option<&'a str> {
    job.variables
        .get(name)
        .and_then(|value| value.value.as_deref())
        .or_else(|| context_string(&job.context_data, name))
}

fn context_string<'a>(context_data: &'a BTreeMap<String, Value>, path: &str) -> Option<&'a str> {
    let mut parts = path.split('.');
    let root = parts.next()?;
    let mut value = context_data.get(root)?;
    for part in parts {
        value = context_object_get(value, part)?;
    }
    value.as_str()
}

/// Navigate a context value by key, handling both plain objects and
/// the GitHub V2 broker format `{"d": [{"k": key, "v": value}, ...]}`.
fn context_object_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(obj) = value.as_object() {
        // Plain object lookup
        if let Some(v) = obj.get(key) {
            return Some(v);
        }
        // GitHub broker compact format: {"d": [{"k": "...", "v": ...}, ...]}
        if let Some(items) = obj.get("d").and_then(Value::as_array) {
            for item in items {
                if let Some(item_obj) = item.as_object() {
                    let k = item_obj
                        .get("k")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if k.eq_ignore_ascii_case(key) {
                        return item_obj.get("v");
                    }
                }
            }
        }
    }
    None
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

/// Build the git http.extraheader value for basic auth (matches actions/runner format).
/// GitHub's git service expects: AUTHORIZATION: basic base64("x-access-token:<token>")
fn git_basic_auth_value(token: &str) -> String {
    let encoded = STANDARD.encode(format!("x-access-token:{token}"));
    format!("AUTHORIZATION: basic {encoded}")
}

pub(crate) fn git_auth_env(clone_url: &str, token: Option<&str>) -> Vec<(String, String)> {
    token.map_or_else(Vec::new, |token| {
        vec![
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            ("GIT_CONFIG_KEY_0".into(), git_extraheader_key(clone_url)),
            ("GIT_CONFIG_VALUE_0".into(), git_basic_auth_value(token)),
        ]
    })
}

pub(crate) fn git_extraheader_key(clone_url: &str) -> String {
    Url::parse(clone_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(format!("http.{}://{host}/.extraheader", url.scheme()))
        })
        .unwrap_or_else(|| "http.https://github.com/.extraheader".to_string())
}

fn git_extraheader_scope(clone_url: &str) -> String {
    Url::parse(clone_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(format!("{}://{host}/", url.scheme()))
        })
        .unwrap_or_else(|| "https://github.com/".to_string())
}

fn format_git_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.starts_with("http.extraheader=AUTHORIZATION:")
                || arg.starts_with("AUTHORIZATION: bearer ")
                || arg.starts_with("AUTHORIZATION: basic ")
            {
                "http.extraheader=AUTHORIZATION: ***".to_string()
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{CommandResult, ProcessCommandRunner};

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
        env_calls: Vec<Vec<(String, String)>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_with_env(
            &mut self,
            program: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandResult> {
            self.env_calls.push(env.to_vec());
            self.run(program, args)
        }
    }

    #[test]
    fn detects_supported_and_unsupported_actions() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            { "reference": { "type": "Repository", "name": "actions/checkout" } },
            { "reference": { "type": "Script" }, "inputs": { "script": "echo ok" } }
        ]))
        .unwrap();

        assert!(!has_unsupported_enabled_action(&steps));

        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            { "reference": { "type": "Repository", "name": "actions/cache" } }
        ]))
        .unwrap();

        assert!(has_unsupported_enabled_action(&steps));
    }

    #[test]
    fn executes_checkout_with_fetch_head() {
        let temp =
            std::env::temp_dir().join(format!("velnor-checkout-test-{}", std::process::id()));
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: temp.clone(),
            token: Some("token".into()),
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut runner = RecordingRunner::default();

        execute_checkout(&mut runner, &plan, &mut Vec::new()).unwrap();

        assert_eq!(runner.calls[0].0, "git");
        assert_eq!(runner.calls[0].1[0], "init");
        assert!(runner
            .calls
            .iter()
            .any(|(_, args)| args.contains(&"fetch".to_string())
                && args.contains(&"abc123".to_string())
                && args.contains(&"--depth=1".to_string())));
        assert!(runner.env_calls.iter().any(|env| env
            .iter()
            .any(|(name, value)| name == "GIT_CONFIG_VALUE_0"
                && value.starts_with("AUTHORIZATION: basic "))));
        assert!(!runner
            .calls
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg.contains("AUTHORIZATION: basic "))));
        assert!(runner.calls.iter().any(|(_, args)| args.ends_with(&[
            "checkout".into(),
            "--force".into(),
            "FETCH_HEAD".into()
        ])));
        assert!(runner.calls.iter().any(|(_, args)| args.ends_with(&[
            "reset".into(),
            "--hard".into(),
            "HEAD".into()
        ])));
        assert!(runner
            .calls
            .iter()
            .any(|(_, args)| args.ends_with(&["clean".into(), "-ffdx".into()])));
        let config = std::fs::read_to_string(temp.join(".git/config")).unwrap();
        assert!(config.contains("extraheader = AUTHORIZATION: basic "));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn an_external_repository_is_cloned_from_the_job_own_server() {
        let mut properties = BTreeMap::new();
        properties.insert(
            "cloneUrl".to_string(),
            "https://ghe.acme.test/acme/repo.git".to_string(),
        );
        let self_repository = RepositoryResource {
            alias: Some("self".to_string()),
            name: Some("acme/repo".to_string()),
            git_ref: None,
            version: None,
            url: None,
            properties,
        };
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Build",
            "requestId": 1,
            "variables": {},
            "resources": { "repositories": [] },
            "steps": []
        }))
        .unwrap();
        let server = checkout_server_url(&job, &self_repository);
        assert_eq!(server, "https://ghe.acme.test");
        // Hardcoding github.com here made a GHES job fetch a public repository
        // of the same name instead of its own server's.
        assert_eq!(
            checkout_clone_url(Some("other/tool"), &self_repository, &server).unwrap(),
            "https://ghe.acme.test/other/tool.git"
        );
    }

    /// Simulate the process that owns a persisted credential dying: the
    /// registration leaves the journal file behind, and dropping it releases
    /// the kernel lock exactly as process death would.
    fn simulate_owner_death(destination: &Path) {
        let mut active = active_credentials().lock().unwrap();
        active.retain(|registration| registration.destination != destination);
    }

    fn write_credentialed_config(destination: &Path) -> PathBuf {
        let git_dir = destination.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let config = git_dir.join("config");
        std::fs::write(
            &config,
            "[core]\n\trepositoryformatversion = 0\n[http \"https://github.com/\"]\n\textraheader = AUTHORIZATION: basic c2VjcmV0\n",
        )
        .unwrap();
        config
    }

    #[test]
    fn an_aborted_job_leaves_no_credential_behind() {
        let root = std::env::temp_dir().join(format!("velnor-credential-{}", uuid::Uuid::new_v4()));
        let journal = root.join("journal");
        let workspace = root.join("workspace");
        let config = write_credentialed_config(&workspace);

        register_credential_in(&journal, &workspace).unwrap();
        // A live owner's credential is never touched.
        assert_eq!(reap_stale_credentials_in(&journal).unwrap(), 0);
        assert!(config_has_credential(&config));

        // The runner dies between the last step and cleanup. Nothing in the
        // process runs; the next checkout on this host reaps the credential.
        simulate_owner_death(&workspace);
        eprintln!(
            "DEBUG entries={:?} config={:?}",
            std::fs::read_dir(&journal)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect::<Vec<_>>(),
            std::fs::read_to_string(&config)
        );
        assert_eq!(reap_stale_credentials_in(&journal).unwrap(), 1);
        assert!(!config_has_credential(&config));
        assert!(!std::fs::read_to_string(&config).unwrap().contains("basic"));
        assert!(std::fs::read_to_string(&config)
            .unwrap()
            .contains("repositoryformatversion"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cleanup_fails_the_job_when_the_credential_cannot_be_removed() {
        struct RefusingRunner;
        impl CommandRunner for RefusingRunner {
            fn run(&mut self, _program: &str, _args: &[String]) -> Result<CommandResult> {
                Ok(CommandResult {
                    code: 1,
                    stdout: String::new(),
                    stderr: "error: could not lock config file".into(),
                })
            }
        }

        let root = std::env::temp_dir().join(format!("velnor-credential-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        write_credentialed_config(&workspace);
        let mut plan = test_checkout_plan(workspace);
        plan.token = Some("token".into());
        plan.persist_credentials = true;

        // This used to print a warning to stderr and report success, leaving a
        // credentialed workspace on the host.
        let error = cleanup_checkout_credentials(&mut RefusingRunner, &[plan]).unwrap_err();
        assert!(
            format!("{error:#}").contains("cleanup of checkout credentials"),
            "{error:#}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cleanup_removes_a_credential_even_when_persistence_was_disabled() {
        struct MissingKeyRunner;
        impl CommandRunner for MissingKeyRunner {
            fn run(&mut self, _program: &str, _args: &[String]) -> Result<CommandResult> {
                // `git config --unset-all` on an absent key.
                Ok(CommandResult {
                    code: GIT_CONFIG_KEY_NOT_FOUND,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        let root = std::env::temp_dir().join(format!("velnor-credential-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let config = write_credentialed_config(&workspace);
        let mut plan = test_checkout_plan(workspace);
        plan.persist_credentials = false;

        // Cleanup used to return early whenever the plan said it had not
        // persisted anything, so a credential written by any other path stayed.
        cleanup_checkout_credentials(&mut MissingKeyRunner, &[plan]).unwrap();
        assert!(!config_has_credential(&config));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_persisted_credential_is_registered_before_it_reaches_disk() {
        let root = std::env::temp_dir().join(format!("velnor-credential-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        let mut runner = RecordingRunner::default();

        persist_git_credentials(
            &mut runner,
            &workspace,
            "https://github.com/acme/repo.git",
            "token",
            &mut Vec::new(),
        )
        .unwrap();

        let tracked = active_credentials()
            .lock()
            .unwrap()
            .iter()
            .any(|registration| registration.destination == workspace);
        assert!(tracked, "the persisted credential is untracked");
        assert!(config_has_credential(&workspace.join(".git/config")));

        cleanup_checkout_credentials(
            &mut runner,
            &[{
                let mut plan = test_checkout_plan(workspace.clone());
                plan.token = Some("token".into());
                plan.persist_credentials = true;
                plan
            }],
        )
        .unwrap();
        assert!(!config_has_credential(&workspace.join(".git/config")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_lfs_checkout_does_not_write_the_token_into_the_workspace() {
        let root = std::env::temp_dir().join(format!("velnor-credential-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let mut runner = RecordingRunner::default();

        // lfs used to persist the credential unconditionally, before the
        // checkout, so `persist-credentials: false` still left one on disk.
        fetch_git_ref(
            &mut runner,
            "https://github.com/acme/repo.git",
            "abc123",
            &workspace,
            Some("token"),
            Some(1),
            false,
            false,
            false,
            true,
            None,
            &mut Vec::new(),
        )
        .unwrap();

        assert!(!config_has_credential(&workspace.join(".git/config")));
        assert!(runner
            .env_calls
            .iter()
            .any(|env| env.iter().any(|(name, _)| name == "GIT_CONFIG_KEY_0")));
        std::fs::remove_dir_all(root).ok();
    }

    /// A real repository served over `file://`, so the mirror and the workspace
    /// hydration run the actual git commands rather than a mock.
    struct RepoFixture {
        root: PathBuf,
        origin: PathBuf,
        sha: String,
    }

    impl RepoFixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("velnor-checkout-{}", uuid::Uuid::new_v4()));
            let origin = root.join("origin/repo.git");
            let work = root.join("seed");
            std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
            let mut runner = ProcessCommandRunner;
            let mut git = |args: Vec<String>| {
                let result = runner.run("git", &args).unwrap();
                assert_eq!(result.code, 0, "git {args:?}: {}", result.stderr);
                result.stdout.trim().to_string()
            };
            git(vec!["init".into(), "--bare".into(), path_arg(&origin)]);
            git(vec![
                "-C".into(),
                path_arg(&origin),
                "config".into(),
                "uploadpack.allowAnySHA1InWant".into(),
                "true".into(),
            ]);
            git(vec!["init".into(), path_arg(&work)]);
            for (key, value) in [("user.email", "test@example.com"), ("user.name", "Test")] {
                git(vec![
                    "-C".into(),
                    path_arg(&work),
                    "config".into(),
                    key.into(),
                    value.into(),
                ]);
            }
            std::fs::write(work.join("value"), "one").unwrap();
            git(vec!["-C".into(), path_arg(&work), "add".into(), ".".into()]);
            git(vec![
                "-C".into(),
                path_arg(&work),
                "commit".into(),
                "-m".into(),
                "one".into(),
            ]);
            git(vec![
                "-C".into(),
                path_arg(&work),
                "push".into(),
                path_arg(&origin),
                "HEAD:master".into(),
            ]);
            let sha = git(vec![
                "-C".into(),
                path_arg(&work),
                "rev-parse".into(),
                "HEAD".into(),
            ]);
            Self { root, origin, sha }
        }

        fn plan(&self, destination: PathBuf) -> CheckoutPlan {
            CheckoutPlan {
                step_id: "checkout".into(),
                display_name: "Checkout".into(),
                clone_url: format!("file://{}", self.origin.display()),
                version: Some(self.sha.clone()),
                destination,
                token: None,
                fetch_depth: Some(1),
                fetch_tags: false,
                persist_credentials: false,
                clean: false,
                lfs: false,
                condition: None,
                continue_on_error: false,
                timeout_minutes: None,
            }
        }

        fn store(&self) -> PathBuf {
            self.root.join("mirrors")
        }
    }

    impl Drop for RepoFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn checkout_hydrates_the_workspace_without_copying_object_bytes() {
        let fixture = RepoFixture::new();
        let workspace = fixture.root.join("workspace");
        let plan = fixture.plan(workspace.clone());
        let mut runner = ProcessCommandRunner;
        let mut log = Vec::new();

        execute_checkout_with_mirror(&mut runner, &plan, &mut log, Some(&fixture.store())).unwrap();

        let head = runner
            .run(
                "git",
                &[
                    "-C".into(),
                    path_arg(&workspace),
                    "rev-parse".into(),
                    "HEAD".into(),
                ],
            )
            .unwrap();
        assert_eq!(head.stdout.trim(), fixture.sha);
        assert_eq!(
            std::fs::read_to_string(workspace.join("value")).unwrap(),
            "one"
        );

        // Every object in the workspace is a hard link to the mirror's copy:
        // more than one link, and the same inode.
        use std::os::unix::fs::MetadataExt;
        let mirror = fixture
            .store()
            .join(format!("{}__origin__repo.git", "localhost"));
        let mirror = if mirror.exists() {
            mirror
        } else {
            std::fs::read_dir(fixture.store())
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .find(|path| path.extension().is_some_and(|extension| extension == "git"))
                .expect("mirror directory")
        };
        let mut checked = 0;
        let mut pending = vec![workspace.join(".git/objects")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let workspace_object = std::fs::metadata(&path).unwrap();
                let relative = path.strip_prefix(workspace.join(".git/objects")).unwrap();
                let mirrored = std::fs::metadata(mirror.join("objects").join(relative)).unwrap();
                assert_eq!(
                    workspace_object.ino(),
                    mirrored.ino(),
                    "{} was copied out of the mirror instead of linked",
                    relative.display()
                );
                assert!(workspace_object.nlink() >= 2);
                checked += 1;
            }
        }
        assert!(checked > 0, "workspace has no objects from the mirror");
    }

    #[test]
    fn checkout_fails_when_the_mirror_cannot_be_prepared() {
        let fixture = RepoFixture::new();
        let mut plan = fixture.plan(fixture.root.join("workspace"));
        plan.clone_url = format!("file://{}", fixture.root.join("gone.git").display());
        let mut runner = ProcessCommandRunner;

        // A mirror that cannot serve the job used to warn and fall through to a
        // direct fetch, hiding the failure and every later degradation with it.
        let error = execute_checkout_with_mirror(
            &mut runner,
            &plan,
            &mut Vec::new(),
            Some(&fixture.store()),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("git mirror"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn concurrent_checkouts_of_one_commit_fetch_the_mirror_once() {
        #[derive(Default)]
        struct CountingRunner {
            network_fetches: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl CommandRunner for CountingRunner {
            fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
                ProcessCommandRunner.run(program, args)
            }
            fn run_with_env(
                &mut self,
                program: &str,
                args: &[String],
                env: &[(String, String)],
            ) -> Result<CommandResult> {
                if args.iter().any(|arg| arg.starts_with("file://")) {
                    self.network_fetches
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                ProcessCommandRunner.run_with_env(program, args, env)
            }
        }

        let fixture = RepoFixture::new();
        let store = fixture.store();
        let fetches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // The first leg of a matrix warms the mirror.
        execute_checkout_with_mirror(
            &mut CountingRunner {
                network_fetches: std::sync::Arc::clone(&fetches),
            },
            &fixture.plan(fixture.root.join("warm")),
            &mut Vec::new(),
            Some(&store),
        )
        .unwrap();
        assert_eq!(fetches.swap(0, std::sync::atomic::Ordering::SeqCst), 1);
        std::thread::scope(|scope| {
            for index in 0..6 {
                let plan = fixture.plan(fixture.root.join(format!("matrix-{index}")));
                let store = store.clone();
                let fetches = std::sync::Arc::clone(&fetches);
                scope.spawn(move || {
                    let mut runner = CountingRunner {
                        network_fetches: fetches,
                    };
                    execute_checkout_with_mirror(&mut runner, &plan, &mut Vec::new(), Some(&store))
                        .unwrap();
                });
            }
        });

        // Every remaining leg wants the same commit, and the mirror already has
        // it: no lock upgrade, no fetch, no origin traffic. This used to be six
        // full `+refs/*:refs/*` fetches serialized behind one exclusive lock.
        let observed = fetches.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            observed, 0,
            "a mirrored commit still cost {observed} fetch(es)"
        );
        for index in 0..6 {
            let workspace = fixture.root.join(format!("matrix-{index}"));
            assert_eq!(
                std::fs::read_to_string(workspace.join("value")).unwrap(),
                "one"
            );
        }
    }

    fn test_checkout_plan(destination: PathBuf) -> CheckoutPlan {
        CheckoutPlan {
            step_id: "checkout".into(),
            display_name: "Checkout".into(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination,
            token: None,
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: false,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }
    }

    #[test]
    fn cleanup_unsets_persisted_checkout_credentials() {
        let temp = std::env::temp_dir().join(format!(
            "velnor-checkout-cleanup-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(temp.join(".git")).unwrap();
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: temp.clone(),
            token: Some("token".into()),
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut runner = RecordingRunner::default();

        cleanup_checkout_credentials(&mut runner, &[plan]).unwrap();

        assert!(runner.calls.iter().any(|(_, args)| args.ends_with(&[
            "config".into(),
            "--local".into(),
            "--unset-all".into(),
            "http.https://github.com/.extraheader".into()
        ])));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn cleanup_skips_disabled_persistence() {
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: PathBuf::from("/tmp/nonexistent-velnor-cleanup-test"),
            token: Some("token".into()),
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: false,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut runner = RecordingRunner::default();

        cleanup_checkout_credentials(&mut runner, &[plan]).unwrap();

        assert!(runner.calls.is_empty());
    }

    #[test]
    fn full_fetch_checkout_omits_depth_arg() {
        let temp = std::env::temp_dir().join(format!(
            "velnor-checkout-full-fetch-test-{}",
            std::process::id()
        ));
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("main".into()),
            destination: temp.clone(),
            token: None,
            fetch_depth: None,
            fetch_tags: false,
            persist_credentials: true,
            clean: true,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let mut runner = RecordingRunner::default();

        execute_checkout(&mut runner, &plan, &mut Vec::new()).unwrap();

        let fetch = runner
            .calls
            .iter()
            .find(|(_, args)| args.contains(&"fetch".to_string()))
            .unwrap();
        assert!(!fetch.1.iter().any(|arg| arg.starts_with("--depth=")));
        assert!(fetch
            .1
            .contains(&"+refs/heads/*:refs/remotes/origin/*".to_string()));
        assert!(fetch.1.contains(&"+refs/tags/*:refs/tags/*".to_string()));
        assert!(fetch.1.contains(&"--tags".to_string()));
        let origin = fetch.1.iter().position(|arg| arg == "origin").unwrap();
        let requested = fetch.1.iter().position(|arg| arg == "main").unwrap();
        let wildcard = fetch
            .1
            .iter()
            .position(|arg| arg == "+refs/heads/*:refs/remotes/origin/*")
            .unwrap();
        assert_eq!(requested, origin + 1);
        assert!(requested < wildcard, "requested ref must own FETCH_HEAD");

        std::fs::remove_dir_all(temp).ok();
    }

    /// Fresh-workspace probe: `git remote get-url origin` fails with
    /// "error: No such remote: 'origin'".
    #[derive(Default)]
    struct FreshWorkspaceRunner {
        calls: Vec<Vec<String>>,
    }

    impl CommandRunner for FreshWorkspaceRunner {
        fn run(&mut self, _program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push(args.to_vec());
            let get_url = args.iter().any(|arg| arg == "get-url");
            Ok(CommandResult {
                code: if get_url { 2 } else { 0 },
                stdout: String::new(),
                stderr: if get_url {
                    "error: No such remote: 'origin'".to_string()
                } else {
                    String::new()
                },
            })
        }

        fn run_with_env(
            &mut self,
            _program: &str,
            args: &[String],
            _env: &[(String, String)],
        ) -> Result<CommandResult> {
            self.calls.push(args.to_vec());
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn checkout_skips_origin_removal_on_fresh_workspace() {
        // A fresh workspace has no origin remote: removing it prints
        // "error: No such remote: 'origin'" into the job log. The removal
        // must be probed quietly and skipped instead.
        let temp =
            std::env::temp_dir().join(format!("velnor-checkout-fresh-{}", uuid::Uuid::new_v4()));
        let mut runner = FreshWorkspaceRunner::default();
        let mut log = Vec::new();

        fetch_git_ref(
            &mut runner,
            "https://github.com/acme/repo.git",
            "abc123",
            &temp,
            None,
            Some(1),
            false,
            false,
            true,
            false,
            None,
            &mut log,
        )
        .unwrap();

        assert!(
            !runner.calls.iter().any(|args| args
                .windows(2)
                .any(|pair| pair[0] == "remote" && pair[1] == "remove")),
            "fresh workspace must not run git remote remove: {:?}",
            runner.calls
        );
        assert!(
            !log.iter().any(|line| line.contains("No such remote")),
            "fresh workspace log must not contain the git error: {log:?}"
        );

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn checkout_removes_stale_origin_remote() {
        // With an existing origin (probe succeeds), the removal still runs
        // so re-checkout of a reused workspace stays idempotent.
        let temp =
            std::env::temp_dir().join(format!("velnor-checkout-stale-{}", uuid::Uuid::new_v4()));
        let mut runner = RecordingRunner::default();
        let mut log = Vec::new();

        fetch_git_ref(
            &mut runner,
            "https://github.com/acme/repo.git",
            "abc123",
            &temp,
            None,
            Some(1),
            false,
            false,
            true,
            false,
            None,
            &mut log,
        )
        .unwrap();

        assert!(runner.calls.iter().any(|(_, args)| args
            .windows(2)
            .any(|pair| pair[0] == "remote" && pair[1] == "remove")));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn full_fetch_checkout_keeps_exact_requested_commit_in_fetch_head() {
        let root = std::env::temp_dir().join(format!(
            "velnor-checkout-fetch-head-{}",
            uuid::Uuid::new_v4()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-b", "main", source.to_str().unwrap()]);
        git(&[
            "-C",
            source.to_str().unwrap(),
            "config",
            "user.email",
            "test@example.invalid",
        ]);
        git(&[
            "-C",
            source.to_str().unwrap(),
            "config",
            "user.name",
            "Velnor Test",
        ]);
        std::fs::write(source.join("state"), "requested\n").unwrap();
        git(&["-C", source.to_str().unwrap(), "add", "state"]);
        git(&["-C", source.to_str().unwrap(), "commit", "-m", "requested"]);
        let requested = git(&["-C", source.to_str().unwrap(), "rev-parse", "HEAD"]);
        git(&[
            "-C",
            source.to_str().unwrap(),
            "checkout",
            "-b",
            "aaa-unrelated",
        ]);
        std::fs::write(source.join("state"), "unrelated\n").unwrap();
        git(&["-C", source.to_str().unwrap(), "commit", "-am", "unrelated"]);

        fetch_git_ref(
            &mut ProcessCommandRunner,
            source.to_str().unwrap(),
            &requested,
            &destination,
            None,
            None,
            true,
            false,
            true,
            false,
            None,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(
            git(&["-C", destination.to_str().unwrap(), "rev-parse", "HEAD"]),
            requested
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn plans_external_checkout_with_path_ref_token_and_full_fetch() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Release",
            "requestId": 1,
            "variables": {
                "secrets.HOMEBREW_TAP_TOKEN": {
                    "value": "tap-token",
                    "isSecret": true
                }
            },
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "jackin-project/jackin",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/jackin-project/jackin.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": {
                    "repository": "jackin-project/homebrew-tap",
                    "ref": "main",
                    "path": "homebrew-tap",
                    "token": "${{ secrets.HOMEBREW_TAP_TOKEN }}",
                    "fetch-depth": "0"
                }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].clone_url,
            "https://github.com/jackin-project/homebrew-tap.git"
        );
        assert_eq!(plans[0].version.as_deref(), Some("main"));
        assert_eq!(plans[0].destination, Path::new("/tmp/work/homebrew-tap"));
        assert_eq!(plans[0].token.as_deref(), Some("tap-token"));
        assert_eq!(plans[0].fetch_depth, None);
        assert!(!plans[0].fetch_tags);
        assert!(plans[0].persist_credentials);
        assert!(plans[0].clean);
    }

    #[test]
    fn plans_self_checkout_defaults() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "CI",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "ghs-token" }
                    }
                }],
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].clone_url, "https://github.com/acme/repo.git");
        assert_eq!(plans[0].version.as_deref(), Some("abc123"));
        assert_eq!(plans[0].destination, Path::new("/tmp/work"));
        assert_eq!(plans[0].token.as_deref(), Some("ghs-token"));
        assert_eq!(plans[0].fetch_depth, Some(1));
        assert!(!plans[0].fetch_tags);
        assert!(plans[0].persist_credentials);
        assert!(plans[0].clean);
    }

    #[test]
    fn plans_self_pull_request_checkout_from_immutable_repository_version() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "PR",
            "requestId": 1,
            "variables": {
                "github.ref": { "value": "refs/pull/408/merge" }
            },
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "ref": "refs/pull/408/merge",
                    "version": "immutable-merge-sha",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].version.as_deref(), Some("immutable-merge-sha"));
    }

    #[test]
    fn plans_self_pull_request_checkout_from_remote_ref_without_version() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "PR",
            "requestId": 1,
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "ref": "refs/pull/408/merge",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].version.as_deref(), Some("refs/pull/408/merge"));
    }

    #[test]
    fn plans_self_push_checkout_from_exact_sha() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Push",
            "requestId": 1,
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "ref": "refs/heads/main",
                    "version": "push-commit-sha",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].version.as_deref(), Some("push-commit-sha"));
    }

    #[test]
    fn explicit_checkout_ref_overrides_pull_request_remote_ref() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "PR",
            "requestId": 1,
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "ref": "refs/pull/408/merge",
                    "version": "ephemeral-merge-sha",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": { "ref": "refs/tags/v1.2.3" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].version.as_deref(), Some("refs/tags/v1.2.3"));
    }

    #[test]
    fn plans_self_checkout_from_github_context_without_repository_resources() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "CI",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "acme/repo" },
                "github.sha": { "value": "abc123" },
                "github.ref": { "value": "refs/heads/main" },
                "github.server_url": { "value": "https://github.com" }
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "ghs-token" }
                    }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans[0].clone_url, "https://github.com/acme/repo.git");
        assert_eq!(plans[0].version.as_deref(), Some("abc123"));
        assert_eq!(plans[0].token.as_deref(), Some("ghs-token"));
    }

    #[test]
    fn plans_checkout_from_run_service_typed_inputs() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "CI",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "acme/repo" },
                "github.sha": { "value": "abc123" },
                "secrets.HOMEBREW_TAP_TOKEN": { "value": "tap-token", "isSecret": true }
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "repository", "type": 0 }, "Value": { "lit": "acme/homebrew-tap", "type": 0 } },
                        { "Key": { "lit": "ref", "type": 0 }, "Value": { "lit": "main", "type": 0 } },
                        { "Key": { "lit": "path", "type": 0 }, "Value": { "lit": "homebrew-tap", "type": 0 } },
                        { "Key": { "lit": "token", "type": 0 }, "Value": { "expr": "secrets.HOMEBREW_TAP_TOKEN", "type": 3 } },
                        { "Key": { "lit": "fetch-depth", "type": 0 }, "Value": { "lit": "0", "type": 0 } },
                        { "Key": { "lit": "persist-credentials", "type": 0 }, "Value": { "lit": "false", "type": 0 } },
                        { "Key": { "lit": "clean", "type": 0 }, "Value": { "lit": "false", "type": 0 } },
                        { "Key": { "lit": "fetch-tags", "type": 0 }, "Value": { "lit": "true", "type": 0 } }
                    ]
                }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(
            plans[0].clone_url,
            "https://github.com/acme/homebrew-tap.git"
        );
        assert_eq!(plans[0].version.as_deref(), Some("main"));
        assert_eq!(plans[0].destination, Path::new("/tmp/work/homebrew-tap"));
        assert_eq!(plans[0].token.as_deref(), Some("tap-token"));
        assert_eq!(plans[0].fetch_depth, None);
        assert!(plans[0].fetch_tags);
        assert!(!plans[0].persist_credentials);
        assert!(!plans[0].clean);
    }

    #[test]
    fn explicit_unresolved_checkout_secret_does_not_fall_back_to_github_token() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "CI",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "acme/repo" },
                "github.sha": { "value": "abc123" },
                "system.github.token": { "value": "repo-token", "isSecret": true }
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "repository", "type": 0 }, "Value": { "lit": "acme/homebrew-tap", "type": 0 } },
                        { "Key": { "lit": "token", "type": 0 }, "Value": { "expr": "secrets.MISSING_TOKEN", "type": 3 } }
                    ]
                }
            }]
        }))
        .unwrap();

        let error = checkout_plans(&job, Path::new("/tmp/work")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "explicit checkout token expression did not resolve"
        );
        assert!(!error.to_string().contains("MISSING_TOKEN"));
        assert!(!error.to_string().contains("repo-token"));
    }

    #[test]
    fn checkout_can_disable_credential_persistence_and_clean() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "CI",
            "requestId": 1,
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "ghs-token" }
                    }
                }],
                "repositories": [{
                    "alias": "self",
                    "name": "acme/repo",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/acme/repo.git" }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": {
                    "persist-credentials": "false",
                    "clean": "false",
                    "fetch-tags": "true"
                }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert!(plans[0].fetch_tags);
        assert!(!plans[0].persist_credentials);
        assert!(!plans[0].clean);
    }

    #[test]
    fn writes_safe_directory_for_workspace_checkout() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "velnor-checkout-safe-dir-test-{}-{nonce}",
            std::process::id(),
        ));
        let home = temp.join("home");
        let workspace = temp.join("work");

        configure_safe_directory(&home, &workspace, &workspace).unwrap();
        configure_safe_directory(&home, &workspace, &workspace.join("homebrew-tap")).unwrap();

        let config = std::fs::read_to_string(home.join(".gitconfig")).unwrap();
        assert!(config.contains("directory = /__w\n"));
        assert!(config.contains("directory = /__w/homebrew-tap\n"));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn checkout_ref_from_previous_step_requires_runtime_context() {
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

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].step_id, "checkout2");
        assert_eq!(
            plans[0].version.as_deref(),
            Some("${{ steps.source.outputs.sha }}")
        );
        assert!(plans[0].requires_runtime_context());
        assert_eq!(plans[0].fetch_depth, None);
    }

    #[test]
    fn checkout_with_condition_requires_runtime_context() {
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
                    "name": "ChainArgos/java-monorepo",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/ChainArgos/java-monorepo.git" }
                }]
            },
            "steps": [
                {
                    "id": "plan",
                    "reference": { "type": "Script" },
                    "inputs": { "script": "echo run-tests=false >> \"$GITHUB_OUTPUT\"" }
                },
                {
                    "reference": { "type": "Repository", "name": "actions/checkout" },
                    "condition": "${{ steps.plan.outputs.run-tests == 'true' }}"
                }
            ]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans.len(), 1);
        assert!(
            plans[0].requires_runtime_context(),
            "conditional checkout must stay in normal step order"
        );
    }

    #[test]
    fn checkout_with_step_output_token_requires_runtime_context() {
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Package update",
            "requestId": 1,
            "variables": {
                "github.repository": { "value": "acme/repo" },
                "github.sha": { "value": "abc123" }
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "authorization": {
                        "parameters": { "AccessToken": "service-token" }
                    }
                }]
            },
            "steps": [{
                "reference": { "type": "Repository", "name": "actions/checkout" },
                "inputs": {
                    "type": "map",
                    "map": [{
                        "Key": { "lit": "token", "type": 0 },
                        "Value": { "expr": "steps.app-token.outputs.token", "type": 3 }
                    }]
                }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(
            plans[0].token.as_deref(),
            Some("${{ steps.app-token.outputs.token }}")
        );
        assert!(plans[0].requires_runtime_context());
        assert_ne!(plans[0].token.as_deref(), Some("service-token"));
    }

    #[test]
    fn checkout_with_default_success_condition_is_eager() {
        // GitHub sets `Condition: "success()"` on every step in the job
        // message; the trivial default must not defer the checkout, or jobs
        // using local composite actions can never resolve them.
        let job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Compat",
            "requestId": 1,
            "resources": {
                "repositories": [{
                    "alias": "self",
                    "name": "tailrocks/velnor-actions-fixture",
                    "version": "abc123",
                    "properties": { "cloneUrl": "https://github.com/tailrocks/velnor-actions-fixture.git" }
                }]
            },
            "steps": [
                {
                    "reference": { "type": "Repository", "name": "actions/checkout" },
                    "condition": "success()"
                }
            ]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

        assert_eq!(plans.len(), 1);
        assert!(
            !plans[0].requires_runtime_context(),
            "default success() condition must keep the checkout eager"
        );
    }

    #[test]
    fn rejects_malformed_checkout_repository() {
        let repository = RepositoryResource {
            alias: Some("self".into()),
            name: Some("acme/repo".into()),
            git_ref: None,
            version: None,
            url: None,
            properties: Default::default(),
        };

        let error =
            checkout_clone_url(Some("../bad"), &repository, "https://github.com").unwrap_err();

        assert!(error
            .to_string()
            .contains("unsupported checkout repository"));
    }

    #[test]
    fn masks_checkout_token_in_git_error_args() {
        let args = vec![
            "-c".to_string(),
            "http.extraheader=AUTHORIZATION: bearer secret".to_string(),
            "fetch".to_string(),
        ];

        let formatted = format_git_args(&args);

        assert!(formatted.contains("AUTHORIZATION: ***"));
        assert!(!formatted.contains("secret"));
    }

    #[test]
    fn masks_persisted_checkout_token_in_git_error_args() {
        let args = vec![
            "config".to_string(),
            "--local".to_string(),
            "http.https://github.com/.extraheader".to_string(),
            "AUTHORIZATION: bearer secret".to_string(),
        ];

        let formatted = format_git_args(&args);

        assert!(formatted.contains("AUTHORIZATION: ***"));
        assert!(!formatted.contains("secret"));
    }

    #[test]
    fn checkout_clean_preserves_only_runner_owned_target_when_persistent() {
        assert_eq!(
            checkout_clean_args(Path::new("/__w"), true),
            ["-C", "/__w", "clean", "-ffdx", "-e", "target/"]
        );
        assert_eq!(
            checkout_clean_args(Path::new("/__w"), false),
            ["-C", "/__w", "clean", "-ffdx"]
        );
    }
}

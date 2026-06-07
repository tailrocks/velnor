use crate::{
    executor::CommandRunner,
    job_message::{
        ActionReferenceType, ActionStep, AgentJobRequestMessage, RepositoryResource,
        ServiceEndpoint,
    },
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
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
}

impl CheckoutPlan {
    pub fn requires_runtime_context(&self) -> bool {
        self.version
            .as_deref()
            .is_some_and(contains_step_context_expression)
            || self
                .condition
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

        let self_repository = self_repository(job)?;
        let checkout_repository = checkout_repository(step);
        let clone_url = checkout_clone_url(checkout_repository.as_deref(), &self_repository)?;
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
        let display_name = step
            .display_name
            .as_deref()
            .filter(|n| !n.is_empty() && !n.starts_with("__"))
            .map(|n| n.to_string())
            .unwrap_or_else(|| {
                if reference_name.is_empty() {
                    String::new()
                } else if reference_ref.is_empty() {
                    format!("Run {reference_name}")
                } else {
                    format!("Run {reference_name}@{reference_ref}")
                }
            });
        plans.push(CheckoutPlan {
            step_id: checkout_step_id(step, index),
            display_name,
            clone_url,
            version: checkout_ref(step).or_else(|| {
                checkout_repository
                    .as_deref()
                    .filter(|repository| {
                        self_repository
                            .name
                            .as_deref()
                            .is_some_and(|self_name| repository.eq_ignore_ascii_case(self_name))
                    })
                    .and_then(|_| self_repository.version.clone())
                    .or_else(|| {
                        if checkout_repository.is_none() {
                            self_repository.version.clone()
                        } else {
                            None
                        }
                    })
            }),
            destination,
            token: checkout_token(step, job)
                // Prefer system.github.token (the GITHUB_TOKEN with repo access) over
                // SystemVssConnection's AccessToken (runner OAuth token, no repo scope).
                .or_else(|| {
                    job.variables
                        .get("system.github.token")
                        .and_then(|v| v.value.clone())
                        .filter(|v| !v.is_empty())
                })
                .or_else(|| system_access_token(job.system_connection())),
            fetch_depth: checkout_fetch_depth(step)?,
            fetch_tags: checkout_fetch_tags(step),
            persist_credentials: checkout_persist_credentials(step),
            clean: checkout_clean(step),
            lfs: checkout_lfs(step),
            condition: step.condition.clone(),
            continue_on_error: crate::script_step::step_continue_on_error(step),
        });
    }
    Ok(plans)
}

#[cfg(test)]
fn has_unsupported_enabled_action(steps: &[ActionStep]) -> bool {
    steps.iter().any(|step| {
        step.enabled
            && step.reference_type() != Some(ActionReferenceType::Script)
            && !is_checkout_step(step)
    })
}

pub fn execute_checkout<R>(runner: &mut R, plan: &CheckoutPlan, log: &mut Vec<String>) -> Result<()>
where
    R: CommandRunner,
{
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
        log,
    )
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
    if !plan.persist_credentials || plan.token.is_none() || !plan.destination.join(".git").exists()
    {
        return Ok(());
    }
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
    if result.code != 0 {
        eprintln!(
            "Failed to cleanup checkout credentials in {}: {}",
            plan.destination.display(),
            result.stderr.trim()
        );
    }
    Ok(())
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
    log: &mut Vec<String>,
) -> Result<()>
where
    R: CommandRunner,
{
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;

    run_git(runner, &["init".to_string(), path_arg(destination)], log)?;
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

    let mut fetch = vec![
        "-C".to_string(),
        path_arg(destination),
        "-c".to_string(),
        "protocol.version=2".to_string(),
    ];
    if let Some(token) = token {
        fetch.extend(["-c".to_string(), git_basic_auth_header(&token)]);
    }
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
                "+refs/heads/*:refs/remotes/origin/*".to_string(),
                "+refs/tags/*:refs/tags/*".to_string(),
                git_ref.to_string(),
            ]);
        }
    }
    run_git(runner, &fetch, log)?;

    // For lfs:true, the git-lfs smudge filter runs during checkout and downloads
    // LFS blobs — it authenticates via the persisted http.<host>.extraheader, so
    // set credentials up BEFORE checkout. For lfs:false (default) we instead skip
    // the smudge entirely (no LFS fetch, no creds needed) via lfs_skip_smudge_args.
    if lfs {
        if let Some(token) = token {
            persist_git_credentials(runner, destination, clone_url, token, log)?;
        }
    }

    let mut checkout = vec!["-C".to_string(), path_arg(destination)];
    if !lfs {
        checkout.extend(lfs_skip_smudge_args());
    }
    checkout.extend([
        "checkout".to_string(),
        "--force".to_string(),
        "FETCH_HEAD".to_string(),
    ]);
    run_git(runner, &checkout, log)?;

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
        run_git(runner, &reset, log)?;
        run_git(
            runner,
            &[
                "-C".to_string(),
                path_arg(destination),
                "clean".to_string(),
                "-ffdx".to_string(),
            ],
            log,
        )?;
    }

    if persist_credentials {
        if let Some(token) = token {
            persist_git_credentials(runner, destination, clone_url, token, log)?;
        }
    }

    Ok(())
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
    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(destination),
            "config".to_string(),
            "--local".to_string(),
            git_extraheader_key(clone_url),
            git_basic_auth_value(&token),
        ],
        log,
    )
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
        bail!(
            "git {} failed with code {}: {}",
            format_git_args(args),
            result.code,
            result.stderr
        );
    }
    Ok(())
}

fn is_checkout_step(step: &ActionStep) -> bool {
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
        .or_else(|| step.id.as_deref())
        .or_else(|| step.name.as_deref())
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
        Some(repository) => Ok(format!("https://github.com/{repository}.git")),
        None => self_clone_url(self_repository),
    }
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

fn checkout_token(step: &ActionStep, job: &AgentJobRequestMessage) -> Option<String> {
    let token = step
        .inputs
        .as_ref()
        .and_then(|inputs| input_string(inputs, &["token", "Token"]))
        .filter(|value| !value.is_empty())?;
    resolve_token_expression(token, job)
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

/// Build the full git -c argument for the http.extraheader using basic auth.
fn git_basic_auth_header(token: &str) -> String {
    format!("http.extraheader={}", git_basic_auth_value(token))
}

fn git_extraheader_key(clone_url: &str) -> String {
    Url::parse(clone_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(format!("http.{}://{host}/.extraheader", url.scheme()))
        })
        .unwrap_or_else(|| "http.https://github.com/.extraheader".to_string())
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
    use crate::executor::CommandResult;

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
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
                && args.contains(&"--depth=1".to_string())
                && args.iter().any(|arg| arg.contains("AUTHORIZATION: basic "))));
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
        assert!(runner.calls.iter().any(|(_, args)| {
            args.len() >= 2
                && args[args.len() - 2] == "http.https://github.com/.extraheader"
                && args[args.len() - 1].starts_with("AUTHORIZATION: basic ")
        }));

        std::fs::remove_dir_all(temp).ok();
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

        std::fs::remove_dir_all(temp).ok();
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
                        { "Key": { "lit": "token", "type": 0 }, "Value": { "lit": "${{ secrets.HOMEBREW_TAP_TOKEN }}", "type": 0 } },
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
    fn rejects_malformed_checkout_repository() {
        let repository = RepositoryResource {
            alias: Some("self".into()),
            name: Some("acme/repo".into()),
            git_ref: None,
            version: None,
            url: None,
            properties: Default::default(),
        };

        let error = checkout_clone_url(Some("../bad"), &repository).unwrap_err();

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
}

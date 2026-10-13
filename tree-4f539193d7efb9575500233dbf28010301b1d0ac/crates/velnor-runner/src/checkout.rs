use crate::{
    executor::CommandRunner,
    job_message::{
        ActionReferenceType, ActionStep, AgentJobRequestMessage, RepositoryResource,
        ServiceEndpoint,
    },
};
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    pub step_id: String,
    pub clone_url: String,
    pub version: Option<String>,
    pub destination: PathBuf,
    pub token: Option<String>,
    pub fetch_depth: Option<u32>,
    pub persist_credentials: bool,
    pub clean: bool,
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

        let self_repository = self_repository(&job.resources.repositories)?;
        let checkout_repository = checkout_repository(step);
        let clone_url = checkout_clone_url(checkout_repository.as_deref(), self_repository)?;
        let destination = workspace_host.join(checkout_path(step)?);
        plans.push(CheckoutPlan {
            step_id: checkout_step_id(step, index),
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
                .or_else(|| system_access_token(job.system_connection())),
            fetch_depth: checkout_fetch_depth(step)?,
            persist_credentials: checkout_persist_credentials(step),
            clean: checkout_clean(step),
            condition: step.condition.clone(),
            continue_on_error: crate::script_step::step_continue_on_error(step),
        });
    }
    Ok(plans)
}

pub fn has_unsupported_enabled_action(steps: &[ActionStep]) -> bool {
    steps.iter().any(|step| {
        step.enabled
            && step.reference_type() != Some(ActionReferenceType::Script)
            && !is_checkout_step(step)
    })
}

pub fn execute_checkouts<R>(runner: &mut R, plans: &[CheckoutPlan]) -> Result<()>
where
    R: CommandRunner,
{
    for plan in plans {
        execute_checkout(runner, plan)?;
    }
    Ok(())
}

pub fn execute_checkout<R>(runner: &mut R, plan: &CheckoutPlan) -> Result<()>
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
        plan.persist_credentials,
        plan.clean,
    )
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
    persist_credentials: bool,
    clean: bool,
) -> Result<()>
where
    R: CommandRunner,
{
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;

    run_git(runner, &["init".to_string(), path_arg(destination)])?;
    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(destination),
            "remote".to_string(),
            "remove".to_string(),
            "origin".to_string(),
        ],
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
    )?;

    let mut fetch = vec![
        "-C".to_string(),
        path_arg(destination),
        "-c".to_string(),
        "protocol.version=2".to_string(),
    ];
    if let Some(token) = token {
        fetch.extend([
            "-c".to_string(),
            format!("http.extraheader=AUTHORIZATION: bearer {token}"),
        ]);
    }
    fetch.extend([
        "fetch".to_string(),
        "--no-tags".to_string(),
        "--prune".to_string(),
    ]);
    if let Some(depth) = fetch_depth {
        fetch.push(format!("--depth={depth}"));
    }
    fetch.extend(["origin".to_string(), git_ref.to_string()]);
    run_git(runner, &fetch)?;

    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(destination),
            "checkout".to_string(),
            "--force".to_string(),
            "FETCH_HEAD".to_string(),
        ],
    )?;

    if clean {
        run_git(
            runner,
            &[
                "-C".to_string(),
                path_arg(destination),
                "reset".to_string(),
                "--hard".to_string(),
                "HEAD".to_string(),
            ],
        )?;
        run_git(
            runner,
            &[
                "-C".to_string(),
                path_arg(destination),
                "clean".to_string(),
                "-ffdx".to_string(),
            ],
        )?;
    }

    if persist_credentials {
        if let Some(token) = token {
            persist_git_credentials(runner, destination, clone_url, token)?;
        }
    }

    Ok(())
}

fn persist_git_credentials<R>(
    runner: &mut R,
    destination: &Path,
    clone_url: &str,
    token: &str,
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
            format!("AUTHORIZATION: bearer {token}"),
        ],
    )
}

fn run_git<R>(runner: &mut R, args: &[String]) -> Result<()>
where
    R: CommandRunner,
{
    let result = runner.run("git", args)?;
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
    step.id
        .as_deref()
        .or(step.context_name.as_deref())
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

fn self_repository(repositories: &[RepositoryResource]) -> Result<&RepositoryResource> {
    repositories
        .iter()
        .find(|repository| repository.alias.as_deref() == Some("self"))
        .or_else(|| repositories.first())
        .ok_or_else(|| anyhow::anyhow!("job has no repository resources"))
}

fn checkout_path(step: &ActionStep) -> Result<PathBuf> {
    let path = step
        .inputs
        .as_ref()
        .and_then(|inputs| inputs.get("path"))
        .and_then(|path| path.as_str())
        .unwrap_or(".");
    if path.starts_with('/') || path.contains("..") {
        bail!("unsupported checkout path '{path}'")
    }
    Ok(PathBuf::from(path))
}

fn checkout_ref(step: &ActionStep) -> Option<String> {
    step.inputs
        .as_ref()
        .and_then(|inputs| inputs.get("ref"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn checkout_repository(step: &ActionStep) -> Option<String> {
    step.inputs
        .as_ref()
        .and_then(|inputs| inputs.get("repository"))
        .and_then(|value| value.as_str())
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
        .and_then(|inputs| inputs.get("fetch-depth"))
    else {
        return Ok(Some(1));
    };
    let Some(value) = value.as_str().filter(|value| !value.is_empty()) else {
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

fn checkout_persist_credentials(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| inputs.get("persist-credentials"))
        .and_then(|value| value.as_str())
        .map(|value| !value.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn checkout_clean(step: &ActionStep) -> bool {
    step.inputs
        .as_ref()
        .and_then(|inputs| inputs.get("clean"))
        .and_then(|value| value.as_str())
        .map(|value| !value.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn checkout_token(step: &ActionStep, job: &AgentJobRequestMessage) -> Option<String> {
    let token = step
        .inputs
        .as_ref()
        .and_then(|inputs| inputs.get("token"))
        .and_then(|value| value.as_str())
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
        return system_access_token(job.system_connection());
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

fn path_arg(path: &Path) -> String {
    path.display().to_string()
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
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: temp.clone(),
            token: Some("token".into()),
            fetch_depth: Some(1),
            persist_credentials: true,
            clean: true,
            condition: None,
            continue_on_error: false,
        };
        let mut runner = RecordingRunner::default();

        execute_checkout(&mut runner, &plan).unwrap();

        assert_eq!(runner.calls[0].0, "git");
        assert_eq!(runner.calls[0].1[0], "init");
        assert!(runner
            .calls
            .iter()
            .any(|(_, args)| args.contains(&"fetch".to_string())
                && args.contains(&"abc123".to_string())
                && args.contains(&"--depth=1".to_string())
                && args
                    .iter()
                    .any(|arg| arg.contains("AUTHORIZATION: bearer token"))));
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
        assert!(runner.calls.iter().any(|(_, args)| args.ends_with(&[
            "http.https://github.com/.extraheader".into(),
            "AUTHORIZATION: bearer token".into()
        ])));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn full_fetch_checkout_omits_depth_arg() {
        let temp = std::env::temp_dir().join(format!(
            "velnor-checkout-full-fetch-test-{}",
            std::process::id()
        ));
        let plan = CheckoutPlan {
            step_id: "checkout".into(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("main".into()),
            destination: temp.clone(),
            token: None,
            fetch_depth: None,
            persist_credentials: true,
            clean: true,
            condition: None,
            continue_on_error: false,
        };
        let mut runner = RecordingRunner::default();

        execute_checkout(&mut runner, &plan).unwrap();

        let fetch = runner
            .calls
            .iter()
            .find(|(_, args)| args.contains(&"fetch".to_string()))
            .unwrap();
        assert!(!fetch.1.iter().any(|arg| arg.starts_with("--depth=")));

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
        assert!(plans[0].persist_credentials);
        assert!(plans[0].clean);
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
                    "clean": "false"
                }
            }]
        }))
        .unwrap();

        let plans = checkout_plans(&job, Path::new("/tmp/work")).unwrap();

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

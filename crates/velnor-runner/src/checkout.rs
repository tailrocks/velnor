use crate::{
    executor::CommandRunner,
    job_message::{
        ActionReferenceType, ActionStep, AgentJobRequestMessage, RepositoryResource,
        ServiceEndpoint,
    },
};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    pub clone_url: String,
    pub version: Option<String>,
    pub destination: PathBuf,
    pub token: Option<String>,
}

pub fn checkout_plans(
    job: &AgentJobRequestMessage,
    workspace_host: &Path,
) -> Result<Vec<CheckoutPlan>> {
    let mut plans = Vec::new();
    for step in &job.steps {
        if !step.enabled || !is_checkout_step(step) {
            continue;
        }

        let repository = self_repository(&job.resources.repositories)?;
        ensure_self_checkout(step, repository)?;
        let clone_url = repository
            .properties
            .get("cloneUrl")
            .or(repository.url.as_ref())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("self repository missing clone URL"))?;
        let destination = workspace_host.join(checkout_path(step)?);
        plans.push(CheckoutPlan {
            clone_url,
            version: checkout_ref(step).or_else(|| repository.version.clone()),
            destination,
            token: system_access_token(job.system_connection()),
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

fn execute_checkout<R>(runner: &mut R, plan: &CheckoutPlan) -> Result<()>
where
    R: CommandRunner,
{
    std::fs::create_dir_all(&plan.destination)
        .with_context(|| format!("create {}", plan.destination.display()))?;

    run_git(runner, &["init".to_string(), path_arg(&plan.destination)])?;
    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(&plan.destination),
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
            path_arg(&plan.destination),
            "remote".to_string(),
            "add".to_string(),
            "origin".to_string(),
            plan.clone_url.clone(),
        ],
    )?;

    let mut fetch = vec![
        "-C".to_string(),
        path_arg(&plan.destination),
        "-c".to_string(),
        "protocol.version=2".to_string(),
    ];
    if let Some(token) = &plan.token {
        fetch.extend([
            "-c".to_string(),
            format!("http.extraheader=AUTHORIZATION: bearer {token}"),
        ]);
    }
    fetch.extend([
        "fetch".to_string(),
        "--no-tags".to_string(),
        "--prune".to_string(),
        "--depth=1".to_string(),
        "origin".to_string(),
        plan.version.clone().unwrap_or_else(|| "HEAD".to_string()),
    ]);
    run_git(runner, &fetch)?;

    run_git(
        runner,
        &[
            "-C".to_string(),
            path_arg(&plan.destination),
            "checkout".to_string(),
            "--force".to_string(),
            "FETCH_HEAD".to_string(),
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

fn ensure_self_checkout(step: &ActionStep, repository: &RepositoryResource) -> Result<()> {
    let Some(requested_repository) = step
        .inputs
        .as_ref()
        .and_then(|inputs| inputs.get("repository"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let Some(self_name) = repository.name.as_deref() else {
        bail!("checkout repository input is unsupported without self repository name")
    };
    if requested_repository.eq_ignore_ascii_case(self_name) {
        Ok(())
    } else {
        bail!("unsupported checkout repository '{requested_repository}'")
    }
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

fn format_git_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.starts_with("http.extraheader=AUTHORIZATION:") {
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
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: temp.clone(),
            token: Some("token".into()),
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
                && args
                    .iter()
                    .any(|arg| arg.contains("AUTHORIZATION: bearer token"))));
        assert!(runner.calls.iter().any(|(_, args)| args.ends_with(&[
            "checkout".into(),
            "--force".into(),
            "FETCH_HEAD".into()
        ])));

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn rejects_checkout_of_different_repository() {
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "reference": { "type": "Repository", "name": "actions/checkout" },
            "inputs": { "repository": "other/repo" }
        }))
        .unwrap();
        let repository = RepositoryResource {
            alias: Some("self".into()),
            name: Some("acme/repo".into()),
            git_ref: None,
            version: None,
            url: None,
            properties: Default::default(),
        };

        let error = ensure_self_checkout(&step, &repository).unwrap_err();

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
}

//! Self-hosted Renovate workflows: scheduled dependency updates and optional
//! configuration validation.

use super::{
    json_string, lanes_dispatch_inputs, lanes_labels_json, Args, Primitive, RenderCtx, Rendered,
};
use crate::{
    velnor_runner, velnor_runner_group, yaml_scalar, ActionPin, GeneratorError, ProjectConfig,
    RenovateSpec, RunnerMode, GENERATED_HEADER,
};

/// The pinned Renovate OSS version rendered into `renovate-version`.
#[cfg(test)]
pub(crate) use crate::renovate_renderer::RENOVATE_OSS_VERSION;

/// Sidecar workflow families the generator may emit for Renovate.
pub(crate) const RENOVATE_SIDE_FILES: &[(&str, &str)] = &[
    ("renovate.yml", super::RENOVATE),
    ("renovate-validate.yml", super::RENOVATE_VALIDATE),
];

/// Whether `primitive` renders a Renovate side workflow.
pub(crate) fn is_renovate_side(primitive: &str) -> bool {
    RENOVATE_SIDE_FILES
        .iter()
        .any(|(_, family)| *family == primitive)
}

/// The canonical filename a Renovate primitive renders when pinned to one name.
pub(crate) fn canonical_renovate_side_file(primitive: &str) -> Option<&'static str> {
    RENOVATE_SIDE_FILES
        .iter()
        .find(|(_, family)| *family == primitive)
        .map(|(file, _)| *file)
}

/// The `renovate.yml` content for a config, or `None` when Renovate is disabled.
pub(crate) fn renovate_content(config: &ProjectConfig) -> Option<String> {
    config
        .renovate
        .as_ref()
        .map(|spec| format!("{GENERATED_HEADER}{}", render_renovate(config, spec)))
}

/// The `renovate-validate.yml` content, or `None` when validation is disabled.
pub(crate) fn renovate_validate_content(config: &ProjectConfig) -> Option<String> {
    config
        .renovate
        .as_ref()
        .filter(|spec| spec.validate)
        .map(|spec| {
            format!(
                "{GENERATED_HEADER}{}",
                render_renovate_validate(config, spec)
            )
        })
}

/// The declared `renovate.yml` writer workflow.
pub(crate) struct Renovate;

impl Primitive for Renovate {
    fn id(&self) -> &'static str {
        super::RENOVATE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = configured_spec(ctx)?;
        let content = format!("{GENERATED_HEADER}{}", render_renovate(ctx.config, &spec));
        render_file(ctx, "renovate.yml", content)
    }
}

/// The declared `renovate-validate.yml` configuration validator.
pub(crate) struct RenovateValidate;

impl Primitive for RenovateValidate {
    fn id(&self) -> &'static str {
        super::RENOVATE_VALIDATE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = configured_spec(ctx)?;
        if !spec.validate {
            return Err(GeneratorError::usage(format!(
                "`{}` renders `renovate-validate.yml` only when `[renovate] validate = true`",
                ctx.family
            )));
        }
        let content = format!(
            "{GENERATED_HEADER}{}",
            render_renovate_validate(ctx.config, &spec)
        );
        render_file(ctx, "renovate-validate.yml", content)
    }
}

fn configured_spec(ctx: &RenderCtx<'_>) -> Result<RenovateSpec, GeneratorError> {
    ctx.config.renovate.clone().ok_or_else(|| {
        GeneratorError::usage(format!(
            "`{}` renders only for a repository with `[renovate] enabled = true` and a complete contract",
            ctx.family
        ))
    })
}

fn render_file(
    ctx: &RenderCtx<'_>,
    canonical: &str,
    content: String,
) -> Result<Rendered, GeneratorError> {
    let file = ctx
        .file
        .filter(|file| !file.is_empty())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders `{canonical}` and needs `file`",
                ctx.family
            ))
        })?
        .to_owned();
    Ok(Rendered {
        files: std::iter::once((
            std::path::PathBuf::from(".github/workflows").join(file),
            content,
        ))
        .collect(),
        ..Rendered::default()
    })
}

/// Velnor labels for the writer job: the declared lane labels plus the trusted
/// label exactly once. Both `runs-on` spellings (static and `fromJSON`)
/// share this so they cannot route different runners.
fn writer_velnor_labels(config: &ProjectConfig) -> Vec<String> {
    let mut labels = config.velnor_labels.clone();
    if let Some(trusted) = config.velnor_trusted_label.as_deref()
        && !labels.iter().any(|label| label == trusted)
    {
        labels.push(trusted.to_owned());
    }
    labels
}

fn renovate_runner(config: &ProjectConfig) -> String {
    velnor_runner(&writer_velnor_labels(config), velnor_runner_group(config))
}

/// Writer `runs-on` for the declared lanes: static labels for a single lane,
/// a dispatch-choice expression for `both` (scheduled runs stay on Velnor;
/// only a manual dispatch can select GitHub, and only when declared).
fn writer_runs_on(config: &ProjectConfig, spec: &RenovateSpec) -> String {
    match spec.lanes {
        RunnerMode::Velnor => renovate_runner(config),
        RunnerMode::Github => yaml_scalar(&config.github_runner),
        RunnerMode::Both => format!(
            "${{{{ (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && {} || fromJSON('{}') }}}}",
            json_string(&config.github_runner),
            velnor_labels_json(config),
        ),
    }
}

/// The `workflow_dispatch` inputs for the declared lanes: a lane choice only
/// exists when both lanes are declared, so a dispatch can never escalate to
/// an undeclared lane.
fn writer_dispatch_inputs(spec: &RenovateSpec) -> &'static str {
    if spec.lanes == RunnerMode::Both {
        lanes_dispatch_inputs(RunnerMode::Velnor)
    } else {
        ""
    }
}

fn velnor_labels_json(config: &ProjectConfig) -> String {
    lanes_labels_json(&writer_velnor_labels(config))
}

fn render_renovate(config: &ProjectConfig, spec: &RenovateSpec) -> String {
    let runner = writer_runs_on(config, spec);
    let dispatch_inputs = writer_dispatch_inputs(spec);
    crate::renovate_renderer::render_writer(&crate::renovate_renderer::WriterInput {
        checkout: ActionPin::Checkout.reference(),
        cache_restore: ActionPin::CacheRestore.reference(),
        cache_save: ActionPin::CacheSave.reference(),
        renovate_action: ActionPin::Renovate.reference(),
        runner: &runner,
        dispatch_inputs,
        default_branch: &config.default_branch,
        token: &spec.token,
        config_path: &spec.config_path,
        schedule: &spec.schedule,
        schedules: &spec.schedules,
        repositories: &spec.repositories,
        host_rules_secret: spec.host_rules_secret.as_deref(),
        author: spec.author.as_deref(),
        signoff: spec.signoff,
        allowed_commands: &spec.allowed_commands,
        cache: spec.cache,
    })
}

fn render_renovate_validate(config: &ProjectConfig, spec: &RenovateSpec) -> String {
    let runner = yaml_scalar(&config.github_runner);
    let default_branch = yaml_scalar(&config.default_branch);
    crate::renovate_renderer::render_validate(&crate::renovate_renderer::ValidateInput {
        checkout: ActionPin::Checkout.reference(),
        runner: &runner,
        // The v1 validate job always runs on the GitHub-hosted lane runner,
        // which needs no trusted-event gate.
        gate: None,
        default_branch: &default_branch,
        config_path: &spec.config_path,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::{config, ProjectConfig};

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn renovate_config() -> ProjectConfig {
        let mut config = ProjectConfig {
            repository: String::new(),
            workflow_revision: crate::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::AnalysisSummary {
                method: "test".to_owned(),
                detected: vec!["renovate-configuration".to_owned()],
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["renovate.yml".to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            runners: crate::RunnerMode::Velnor,
            automatic: crate::RunnerMode::Velnor,
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned(), "example-lane".to_owned()],
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: true,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: Vec::new(),
            maintenance: crate::MaintenanceSpec::default(),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: Some("example-trusted".to_owned()),
            velnor_trusted_runner_available: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: crate::DEFAULT_DISPATCH_RUNNER.to_owned(),
            automatic_lanes: crate::DEFAULT_AUTOMATIC_LANES.to_owned(),
            velnor_rust_needs: crate::VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            reviewers: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            mise_lock_backends: BTreeMap::new(),
            mise_install_deps: crate::s2::MiseInstallDeps::default(),
            github_cache: config::CacheGithubSection::default(),
            velnor_host_cache: config::CacheVelnorSection::default(),
        };
        config.renovate = Some(RenovateSpec {
            enabled: true,
            reason: "Repository-local Renovate via GH_RENOVATE_TOKEN".to_owned(),
            schedule: "0 6 * * *".to_owned(),
            schedules: Vec::new(),
            token: "GH_RENOVATE_TOKEN".to_owned(),
            config_path: "renovate.json".to_owned(),
            validate: true,
            cache: true,
            lanes: RunnerMode::Velnor,
            repositories: Vec::new(),
            host_rules_secret: None,
            author: None,
            signoff: false,
            allowed_commands: Vec::new(),
        });
        config
    }

    #[test]
    fn renovate_writer_references_token_and_action_pin() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        assert!(workflow.contains("secrets.GH_RENOVATE_TOKEN"));
        assert!(workflow.contains(ActionPin::Renovate.reference()));
        assert!(workflow.contains("renovate-version: \"44.93.6\""));
        assert!(workflow.contains("workflow_dispatch"));
        assert!(workflow.contains("0 6 * * *"));
        assert!(!workflow.contains("pull_request"));
        assert!(
            workflow.contains("/tmp/renovate/cache/${{ github.repository }}/renovate/repository")
        );
        assert!(workflow.contains("velnor-renovate-${{ github.repository }}-"));
    }

    #[test]
    fn renovate_writer_has_no_trailing_whitespace() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        let offenders = workflow
            .lines()
            .enumerate()
            .filter_map(|(line, content)| (content.trim_end() != content).then_some(line + 1))
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "trailing whitespace at lines {offenders:?}"
        );
    }

    #[test]
    fn renovate_writer_gates_token_via_job_env_without_secrets_in_if() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        // actionlint rejects the secrets context in `if:` in every
        // spelling, so the gate reads a job-level flag instead.
        assert!(
            workflow.contains("RENOVATE_HAS_TOKEN: ${{ secrets.GH_RENOVATE_TOKEN != '' }}"),
            "{workflow}"
        );
        assert!(
            workflow.contains("if: env.RENOVATE_HAS_TOKEN == 'true'"),
            "{workflow}"
        );
        assert!(
            workflow.contains("if: env.RENOVATE_HAS_TOKEN != 'true'"),
            "{workflow}"
        );
        for line in workflow.lines() {
            let trimmed = line.trim_start();
            assert!(
                !(trimmed.starts_with("if:") && line.contains("secrets.")),
                "no step gate may read secrets directly: {line}"
            );
        }
        assert!(
            !workflow.contains('`'),
            "run blocks must not use legacy backticks (command substitution): {workflow}"
        );
    }

    #[test]
    fn renovate_writer_uses_trusted_runner_labels() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        assert!(workflow.contains("example-trusted"));
        assert!(workflow.contains("self-hosted"));
    }

    #[test]
    fn renovate_writer_velnor_lane_renders_static_labels_without_dispatch_inputs() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        assert_eq!(spec.lanes, RunnerMode::Velnor);
        let workflow = render_renovate(&config, spec);
        assert!(
            workflow.contains("    runs-on: [self-hosted, example-lane, example-trusted]\n"),
            "{workflow}"
        );
        assert!(
            workflow.contains("  workflow_dispatch:\n\npermissions:"),
            "{workflow}"
        );
        assert!(!workflow.contains("fromJSON"), "{workflow}");
        assert!(!workflow.contains("inputs.lanes"), "{workflow}");
    }

    #[test]
    fn renovate_writer_github_lane_runs_on_github_runner() {
        let mut config = renovate_config();
        {
            let spec = must_some(
                config.renovate.as_mut(),
                "renovate_config must include a renovate spec",
            );
            spec.lanes = RunnerMode::Github;
        }
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        assert!(
            workflow.contains("    runs-on: ubuntu-24.04\n"),
            "{workflow}"
        );
        assert!(!workflow.contains("fromJSON"), "{workflow}");
        assert!(!workflow.contains("inputs.lanes"), "{workflow}");
    }

    #[test]
    fn renovate_writer_both_lanes_renders_dispatch_choice_defaulting_to_velnor() {
        let mut config = renovate_config();
        {
            let spec = must_some(
                config.renovate.as_mut(),
                "renovate_config must include a renovate spec",
            );
            spec.lanes = RunnerMode::Both;
        }
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        assert!(workflow.contains("inputs.lanes == 'github'"), "{workflow}");
        assert!(
            workflow.contains("fromJSON('[\"self-hosted\",\"example-lane\",\"example-trusted\"]')"),
            "{workflow}"
        );
        assert!(workflow.contains("options: [velnor, github]"), "{workflow}");
        assert!(!workflow.contains("pull_request ||"), "{workflow}");
    }

    #[test]
    fn renovate_writer_both_lanes_escapes_quotes_inside_from_json() {
        let mut config = renovate_config();
        config.velnor_labels = vec!["self-hosted".to_owned(), "o'brien".to_owned()];
        {
            let spec = must_some(
                config.renovate.as_mut(),
                "renovate_config must include a renovate spec",
            );
            spec.lanes = RunnerMode::Both;
        }
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        assert!(
            workflow
                .contains("fromJSON('[\"self-hosted\",\"o\\u0027brien\",\"example-trusted\"]')"),
            "{workflow}"
        );
        assert!(!workflow.contains("o'brien"), "{workflow}");
    }

    #[test]
    fn renovate_validate_uses_docker_validator_without_secrets() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate_validate(&config, spec);
        assert!(workflow.contains("renovate-config-validator --strict --no-global renovate.json"));
        assert!(workflow.contains("ghcr.io/renovatebot/renovate:44.93.6"));
        assert!(!workflow.contains("GH_RENOVATE_TOKEN"));
        assert!(!workflow.contains("RENOVATE_TOKEN"));
    }

    fn full_spec() -> RenovateSpec {
        RenovateSpec {
            enabled: true,
            reason: "Repository-local Renovate via GH_RENOVATE_TOKEN".to_owned(),
            schedule: "0 6 * * *".to_owned(),
            schedules: vec!["0 18 * * *".to_owned()],
            token: "GH_RENOVATE_TOKEN".to_owned(),
            config_path: "renovate.json".to_owned(),
            validate: true,
            cache: true,
            lanes: RunnerMode::Velnor,
            repositories: vec!["example/one".to_owned(), "example/two".to_owned()],
            host_rules_secret: Some("RENOVATE_HOST_RULES_JSON".to_owned()),
            author: Some("Renovate Bot <bot@example.com>".to_owned()),
            signoff: true,
            allowed_commands: vec!["^npm install --package-lock-only$".to_owned()],
        }
    }

    #[test]
    fn renovate_writer_renders_declared_targets_credentials_author_and_allowances() {
        let config = renovate_config();
        let workflow = render_renovate(&config, &full_spec());
        assert!(workflow.contains("- cron: \"0 6 * * *\""), "{workflow}");
        assert!(workflow.contains("- cron: \"0 18 * * *\""), "{workflow}");
        assert!(
            workflow.contains("RENOVATE_AUTODISCOVER: \"false\""),
            "{workflow}"
        );
        assert!(
            workflow.contains("RENOVATE_REPOSITORIES: \"example/one,example/two\""),
            "{workflow}"
        );
        assert!(
            workflow.contains("RENOVATE_HOST_RULES: ${{ secrets.RENOVATE_HOST_RULES_JSON }}"),
            "{workflow}"
        );
        assert!(
            workflow.contains("RENOVATE_GIT_AUTHOR: \"Renovate Bot <bot@example.com>\""),
            "{workflow}"
        );
        assert!(
            workflow.contains(
                "RENOVATE_COMMIT_BODY: \"Signed-off-by: Renovate Bot <bot@example.com>\""
            ),
            "{workflow}"
        );
        assert!(
            workflow.contains(
                "RENOVATE_ALLOWED_COMMANDS: \"[\\\"^npm install --package-lock-only$\\\"]\""
            ),
            "{workflow}"
        );
    }

    #[test]
    fn renovate_writer_omits_undeclared_contract_env() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = render_renovate(&config, spec);
        for absent in [
            "RENOVATE_AUTODISCOVER",
            "RENOVATE_REPOSITORIES",
            "RENOVATE_HOST_RULES",
            "RENOVATE_GIT_AUTHOR",
            "RENOVATE_COMMIT_BODY",
            "RENOVATE_ALLOWED_COMMANDS",
        ] {
            assert!(!workflow.contains(absent), "{workflow}");
        }
        assert_eq!(workflow.matches("- cron:").count(), 1, "{workflow}");
    }

    #[test]
    fn renovate_writer_and_validator_agree_on_the_renovate_release() {
        let config = renovate_config();
        let writer = render_renovate(&config, &full_spec());
        let validator = render_renovate_validate(&config, &full_spec());
        assert!(
            writer.contains(&format!("renovate-version: \"{RENOVATE_OSS_VERSION}\"")),
            "{writer}"
        );
        assert!(
            validator.contains(&format!(
                "ghcr.io/renovatebot/renovate:{RENOVATE_OSS_VERSION}"
            )),
            "{validator}"
        );
    }

    /// A throwaway tree carrying rendered side workflows for the policy audit.
    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn audited_tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-renovate-{name}-{}",
            crate::unique_suffix()
        ));
        match std::fs::create_dir_all(root.join(".github/workflows")) {
            Ok(()) => {}
            Err(error) => panic!("create audited tree: {error}"),
        }
        for (file, content) in files {
            match std::fs::write(root.join(".github/workflows").join(file), content) {
                Ok(()) => {}
                Err(error) => panic!("write audited {file}: {error}"),
            }
        }
        root
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn renovate_workflows_pass_trusted_policy_audit() {
        let config = renovate_config();
        let writer = render_renovate(&config, &full_spec());
        let validator = render_renovate_validate(&config, &full_spec());
        let root = audited_tree(
            "audit",
            &[
                ("renovate.yml", &writer),
                ("renovate-validate.yml", &validator),
            ],
        );
        let audit = match crate::policy::audit_workflows(&root) {
            Ok(audit) => audit,
            Err(error) => panic!("audit renovate workflows: {error}"),
        };
        assert!(
            audit.pull_request_target.is_empty(),
            "{:?}",
            audit.pull_request_target
        );
        assert!(audit.runners.is_empty(), "{:?}", audit.runners);
        assert!(audit.actions.is_empty(), "{:?}", audit.actions);
        assert!(audit.structure.is_empty(), "{:?}", audit.structure);
        let _ = std::fs::remove_dir_all(root);
    }
}

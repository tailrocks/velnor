//! Self-hosted Renovate workflows: scheduled dependency updates and optional
//! configuration validation.

use std::fmt::Write as _;

use super::{trusted_cache_save_expression, Args, Primitive, RenderCtx, Rendered};
use crate::s2::provider::ProviderId;
use crate::s2::{
    selector_runs_on_yaml, yaml_scalar, ActionPin, GeneratorError, ProjectConfig, RenovateSpec,
    GENERATED_HEADER,
};

/// The pinned Renovate OSS version rendered into `renovate-version`.
pub(crate) const RENOVATE_OSS_VERSION: &str = "44.93.6";

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
pub(crate) fn renovate_content(config: &ProjectConfig) -> Result<Option<String>, GeneratorError> {
    config
        .renovate
        .as_ref()
        .map(|spec| {
            render_renovate(config, spec).map(|content| format!("{GENERATED_HEADER}{content}"))
        })
        .transpose()
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
        let content = format!("{GENERATED_HEADER}{}", render_renovate(ctx.config, &spec)?);
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

fn renovate_runner(config: &ProjectConfig) -> Result<String, GeneratorError> {
    crate::s2::provider::runs_on_for(&config.selectors, ProviderId::Velnor)
        .map(crate::s2::runs_on_labels_yaml)
}

fn trusted_renovate_gate(default_branch: &str) -> String {
    format!(
        "github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}

fn renovate_repository_cache_path() -> &'static str {
    "/tmp/renovate/cache/${{ github.repository }}/renovate/repository"
}

fn renovate_config_env(spec: &RenovateSpec) -> String {
    if spec.config_path == "renovate.json" {
        String::new()
    } else {
        format!(
            "          RENOVATE_CONFIG_FILE: {}\n",
            yaml_scalar(&spec.config_path)
        )
    }
}

/// The `on.schedule` cron list: the primary schedule plus every declared
/// extra, each once.
fn renovate_schedules(spec: &RenovateSpec) -> String {
    let mut schedules = format!("    - cron: {}\n", yaml_scalar(&spec.schedule));
    for extra in &spec.schedules {
        if extra != &spec.schedule {
            let _ = writeln!(schedules, "    - cron: {}", yaml_scalar(extra));
        }
    }
    schedules
}

/// Repository targets: explicit targets disable autodiscovery, so the writer
/// renovates exactly the declared slugs. Empty keeps autodiscovery.
fn renovate_target_env(spec: &RenovateSpec) -> String {
    if spec.repositories.is_empty() {
        String::new()
    } else {
        format!(
            "          RENOVATE_AUTODISCOVER: \"false\"\n          RENOVATE_REPOSITORIES: {}\n",
            yaml_scalar(&spec.repositories.join(","))
        )
    }
}

/// Private-registry credentials: the secret holds the JSON `hostRules`
/// array, so credentials travel by reference and never as workflow text.
fn renovate_host_rules_env(spec: &RenovateSpec) -> String {
    spec.host_rules_secret
        .as_deref()
        .map_or_else(String::new, |secret| {
            format!("          RENOVATE_HOST_RULES: ${{{{ secrets.{secret} }}}}\n")
        })
}

/// The git author Renovate commits as.
fn renovate_author_env(spec: &RenovateSpec) -> String {
    spec.author.as_deref().map_or_else(String::new, |author| {
        format!("          RENOVATE_GIT_AUTHOR: {}\n", yaml_scalar(author))
    })
}

/// DCO sign-off: the commit body carries a `Signed-off-by` trailer for the
/// declared author. The repository's DCO check stays the enforcement; this
/// only makes the writer produce commits that pass it.
fn renovate_signoff_env(spec: &RenovateSpec) -> String {
    if !spec.signoff {
        return String::new();
    }
    spec.author.as_deref().map_or_else(String::new, |author| {
        format!(
            "          RENOVATE_COMMIT_BODY: {}\n",
            yaml_scalar(&format!("Signed-off-by: {author}"))
        )
    })
}

/// Execution allowances: the `allowedCommands` regex allowlist for
/// post-upgrade commands, as the JSON array Renovate parses it from.
fn renovate_allowed_commands_env(spec: &RenovateSpec) -> String {
    if spec.allowed_commands.is_empty() {
        return String::new();
    }
    let json = serde_json::to_string(&spec.allowed_commands).unwrap_or_else(|_| "[]".to_owned());
    format!(
        "          RENOVATE_ALLOWED_COMMANDS: {}\n",
        yaml_scalar(&json)
    )
}

fn cache_hash_files(spec: &RenovateSpec) -> String {
    format!(
        "${{{{ hashFiles('{}') }}}}",
        spec.config_path.replace('\'', "''")
    )
}

fn render_renovate(config: &ProjectConfig, spec: &RenovateSpec) -> Result<String, GeneratorError> {
    let checkout = ActionPin::Checkout.reference();
    let cache_restore = ActionPin::CacheRestore.reference();
    let cache_save = ActionPin::CacheSave.reference();
    let renovate_action = ActionPin::Renovate.reference();
    let runner = renovate_runner(config)?;
    let gate = trusted_renovate_gate(&config.default_branch);
    let token_secret = format!("secrets.{}", spec.token);
    let cache_save_gate = trusted_cache_save_expression(&config.default_branch);
    let config_env = renovate_config_env(spec);
    let schedules = renovate_schedules(spec);
    let target_env = renovate_target_env(spec);
    let host_rules_env = renovate_host_rules_env(spec);
    let author_env = renovate_author_env(spec);
    let signoff_env = renovate_signoff_env(spec);
    let allowed_commands_env = renovate_allowed_commands_env(spec);
    let hash_files = cache_hash_files(spec);
    let cache_path = renovate_repository_cache_path();

    let mut cache_steps = String::new();
    if spec.cache {
        let _ = write!(
            cache_steps,
            r"      - name: Restore Renovate repository cache
        id: renovate-cache
        uses: {cache_restore}
        with:
          path: {cache_path}
          key: velnor-renovate-${{{{ github.repository }}}}-{hash_files}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}
          restore-keys: |
            velnor-renovate-${{{{ github.repository }}}}-{hash_files}-
            velnor-renovate-${{{{ github.repository }}}}-
      - name: Fix Renovate cache ownership
        if: steps.renovate-cache.outputs.cache-matched-key != ''
        run: sudo chown -R 12021:0 /tmp/renovate/
"
        );
    }

    let mut cache_save_step = String::new();
    if spec.cache {
        let _ = write!(
            cache_save_step,
            r"      - name: Save Renovate repository cache
        if: always() && ({cache_save_gate}) && steps.renovate-cache.outputs.cache-hit != 'true'
        uses: {cache_save}
        with:
          path: {cache_path}
          key: velnor-renovate-${{{{ github.repository }}}}-{hash_files}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}
"
        );
    }

    Ok(format!(
        r#"name: Renovate
run-name: Renovate · ${{{{ github.event_name }}}}

on:
  schedule:
{schedules}  workflow_dispatch:

permissions:
  contents: read
  actions: read

concurrency:
  group: renovate-${{{{ github.repository }}}}-${{{{ github.ref }}}}
  cancel-in-progress: true

jobs:
  renovate:
    name: Renovate dependencies
    if: ${{{{ {gate} }}}}
    runs-on: {runner}
    timeout-minutes: 120
    env:
      RENOVATE_HAS_TOKEN: ${{{{ {token_secret} != '' }}}}
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Prepare Renovate workspace
        run: |
          set -euo pipefail
          install -d -m 0755 /tmp/renovate /tmp/renovate/cache /tmp/renovate/repos
          install -d -m 0755 "/tmp/renovate/cache/${{{{ github.repository }}}}/renovate/repository"
{cache_steps}      - name: Run Renovate
        if: env.RENOVATE_HAS_TOKEN == 'true'
        uses: {renovate_action}
        with:
          token: ${{{{ {token_secret} }}}}
          renovate-version: {version}
        env:
          RENOVATE_REPOSITORY_CACHE: enabled
          RENOVATE_BASE_DIR: /tmp/renovate
          RENOVATE_ONBOARDING: "false"
{config_env}{target_env}{host_rules_env}{author_env}{signoff_env}{allowed_commands_env}      - name: Skip Renovate without token
        if: env.RENOVATE_HAS_TOKEN != 'true'
        run: |
          echo "::notice::Renovate skipped because '{token}' is not configured for this repository" >> "$GITHUB_STEP_SUMMARY"
{cache_save_step}"#,
        version = yaml_scalar(RENOVATE_OSS_VERSION),
        token = spec.token,
    ))
}

fn render_renovate_validate(config: &ProjectConfig, spec: &RenovateSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let runner = config
        .selectors
        .get(&ProviderId::GithubHosted)
        .map(selector_runs_on_yaml)
        .unwrap_or_default();
    let config_arg = shell_escape(&spec.config_path);

    format!(
        r#"name: Renovate validate
run-name: Renovate validate · ${{{{ github.event_name }}}}

on:
  push:
    branches: [{default_branch}]
    paths:
      - renovate.json
      - renovate.json5
      - .github/renovate.json
      - .github/renovate.json5
  pull_request:
    paths:
      - renovate.json
      - renovate.json5
      - .github/renovate.json
      - .github/renovate.json5
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: renovate-validate-${{{{ github.repository }}}}-${{{{ github.ref }}}}
  cancel-in-progress: ${{{{ github.event_name == 'pull_request' }}}}

jobs:
  validate:
    name: Validate Renovate configuration
    runs-on: {runner}
    timeout-minutes: 10
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Validate Renovate configuration
        run: |
          set -euo pipefail
          docker run --rm \
            -v "$GITHUB_WORKSPACE:/repo" \
            -w /repo \
            ghcr.io/renovatebot/renovate:{version} \
            renovate-config-validator --strict --no-global {config_arg}
"#,
        default_branch = yaml_scalar(&config.default_branch),
        version = RENOVATE_OSS_VERSION,
    )
}

fn shell_escape(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::s2::{config, ProjectConfig};

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

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn renovate_config() -> ProjectConfig {
        let mut config = ProjectConfig {
            repository: String::new(),
            workflow_revision: crate::s2::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::s2::AnalysisSummary {
                method: "test".to_owned(),
                detected: vec!["renovate-configuration".to_owned()],
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["renovate.yml".to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            providers: std::collections::BTreeSet::from([crate::s2::provider::ProviderId::Velnor]),
            automatic_providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::Velnor,
            ]),
            selectors: crate::s2::scan::default_selectors(),
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
            maintenance: crate::s2::MaintenanceSpec::default(),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            default_dispatch_providers: crate::s2::provider::ProviderId::ALL.into_iter().collect(),
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
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
        let workflow = must(render_renovate(&config, spec), "render renovate workflow");
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
        let workflow = must(render_renovate(&config, spec), "render renovate workflow");
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
        let workflow = must(render_renovate(&config, spec), "render renovate workflow");
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
    fn renovate_writer_uses_the_velnor_selector() {
        let config = renovate_config();
        let spec = must_some(
            config.renovate.as_ref(),
            "renovate_config must include a renovate spec",
        );
        let workflow = must(render_renovate(&config, spec), "render renovate workflow");
        assert!(workflow.contains("velnor-native"));
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
        let workflow = must(
            render_renovate(&config, &full_spec()),
            "render renovate workflow",
        );
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
        let workflow = must(render_renovate(&config, spec), "render renovate workflow");
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
        let writer = must(
            render_renovate(&config, &full_spec()),
            "render renovate workflow",
        );
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
            crate::s2::unique_suffix()
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
        let writer = must(
            render_renovate(&config, &full_spec()),
            "render renovate workflow",
        );
        let validator = render_renovate_validate(&config, &full_spec());
        let root = audited_tree(
            "audit",
            &[
                ("renovate.yml", &writer),
                ("renovate-validate.yml", &validator),
            ],
        );
        // The selector audit matches runs-on against the declared
        // selectors, so the audited tree carries the generation config
        // the writer rendered from.
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config dir",
        );
        must(
            std::fs::write(
                root.join(".github-gen/velnor-workflow.toml"),
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [workflow]\nproviders = [\"velnor\"]\n\n\
                 [workflow.selectors.velnor]\nruns_on = [\"velnor-native\"]\n",
            ),
            "write generation config",
        );
        let audit = match crate::s2::policy::audit_workflows(&root) {
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

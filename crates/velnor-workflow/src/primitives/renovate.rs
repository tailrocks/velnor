//! Self-hosted Renovate workflows: scheduled dependency updates and optional
//! configuration validation.

use std::fmt::Write as _;

use super::{trusted_cache_save_expression, Args, Primitive, RenderCtx, Rendered};
use crate::provider::ProviderId;
use crate::{
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
    crate::provider::runs_on_for(&config.selectors, ProviderId::Velnor)
        .map(crate::runs_on_labels_yaml)
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
    - cron: {schedule}
  workflow_dispatch:

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
        if: ${{{{ {token_secret} }} != '' }}
        uses: {renovate_action}
        with:
          token: ${{{{ {token_secret} }}}}
          renovate-version: {version}
        env:
          RENOVATE_REPOSITORY_CACHE: enabled
          RENOVATE_BASE_DIR: /tmp/renovate
          RENOVATE_ONBOARDING: "false"
{config_env}      - name: Skip Renovate without token
        if: ${{{{ {token_secret} }} == '' }}
        run: |
          echo "::notice::Renovate skipped because `{token}` is not configured for this repository" >> "$GITHUB_STEP_SUMMARY"
{cache_save_step}"#,
        schedule = yaml_scalar(&spec.schedule),
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
            providers: std::collections::BTreeSet::from([crate::provider::ProviderId::Velnor]),
            automatic_providers: std::collections::BTreeSet::from([
                crate::provider::ProviderId::Velnor,
            ]),
            selectors: crate::scan::default_selectors(),
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: true,
            renovate_reason: String::new(),
            renovate: None,
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            default_dispatch_providers: crate::provider::ProviderId::ALL.into_iter().collect(),
            rust_needs: crate::RustNeeds::Parallel,
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
            token: "GH_RENOVATE_TOKEN".to_owned(),
            config_path: "renovate.json".to_owned(),
            validate: true,
            cache: true,
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
}

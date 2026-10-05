//! Composable documentation-site pipeline: build, link checks, spelling,
//! Pages deployment, and post-deployment verification from one consumer contract.
//!
//! One `docs.yml` workflow renders from the repository's `[docs]` section.
//! Local checks (source links, built-site links, spelling) run on pull
//! requests, pushes, and dispatches; the live external-link check runs on
//! schedule only; Pages deployment runs on the default branch with a bounded
//! retry scoped to the deployment handoff; and a post-deployment job proves
//! the deployed site answers before the aggregator records the result.
//!
//! Every consumer-owned value — the site address, the built output directory,
//! the reuse-digest path filters, and the build and check commands — arrives
//! through [`DocsSpec`](crate::DocsSpec). The renderer owns only structure:
//! gates, reuse lookups, the retry ladder, and failure reporting.

use std::fmt::Write as _;

use super::{Args, Primitive, RenderCtx, Rendered};
use crate::{
    velnor_runner, yaml_scalar, ActionPin, DocsSpec, GeneratorError, ProjectConfig, RunnerMode,
    GENERATED_HEADER,
};

/// The workflow file the pipeline renders into.
pub(crate) const DOCS_SITE_FILE: &str = "docs.yml";

/// The side-file family and the canonical file it renders.
pub(crate) const DOCS_SITE_SIDE_FILES: &[(&str, &str)] = &[(DOCS_SITE_FILE, super::DOCS_SITE)];

/// Whether `primitive` renders the documentation-site pipeline workflow.
pub(crate) fn is_docs_site_side(primitive: &str) -> bool {
    DOCS_SITE_SIDE_FILES
        .iter()
        .any(|(_, family)| *family == primitive)
}

/// The canonical filename the documentation-site primitive renders.
pub(crate) fn canonical_docs_site_side_file(primitive: &str) -> Option<&'static str> {
    DOCS_SITE_SIDE_FILES
        .iter()
        .find(|(_, family)| *family == primitive)
        .map(|(file, _)| *file)
}

/// The `docs.yml` content for a config, or `None` when the repository declares
/// no docs contract.
pub(crate) fn docs_site_content(config: &ProjectConfig) -> Option<String> {
    let spec = config.docs.as_ref()?;
    let runner = docs_runner(config).ok()?;
    Some(format!(
        "{GENERATED_HEADER}{}",
        render_docs_site(config, spec, &runner)
    ))
}

/// The declared `docs.yml` documentation-site pipeline.
pub(crate) struct DocsSite;

impl Primitive for DocsSite {
    fn id(&self) -> &'static str {
        super::DOCS_SITE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = ctx.config.docs.clone().ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders only for a repository with `[docs] enabled = true` and a complete contract",
                ctx.family
            ))
        })?;
        let runner = docs_runner(ctx.config)?;
        let content = format!(
            "{GENERATED_HEADER}{}",
            render_docs_site(ctx.config, &spec, &runner)
        );
        render_file(ctx, DOCS_SITE_FILE, content)
    }
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

/// The lane the pipeline runs on: the repository's own lane selection. Pages
/// deployment and artifact reuse are GitHub-platform operations, so a
/// Velnor-only repository runs them on its declared self-hosted labels.
fn docs_runner(config: &ProjectConfig) -> Result<String, GeneratorError> {
    match config.runners {
        RunnerMode::Github | RunnerMode::Both => Ok(yaml_scalar(&config.github_runner)),
        RunnerMode::Velnor => {
            if config.velnor_labels.is_empty() {
                return Err(GeneratorError::usage(
                    "`docs-site` renders a Velnor-lane pipeline but [workflow] velnor_labels is empty; declare the self-hosted labels",
                ));
            }
            Ok(velnor_runner(
                &config.velnor_labels,
                config.velnor_runner_group.as_deref(),
            ))
        }
    }
}

/// The `hashFiles` call over the reuse-digest path filters.
fn hash_files_call(paths: &[String]) -> String {
    let quoted = paths
        .iter()
        .map(|pattern| format!("'{}'", pattern.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("hashFiles({quoted})")
}

/// Escape a path for a bash double-quoted string. Commands render verbatim —
/// the consumer owns their shell semantics — but paths the renderer splices
/// into scripts must not break out of their quotes.
fn bash_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

/// One workflow step per consumer command. Every command runs under `set -euo
/// pipefail` in its own step so a failure names the command that failed.
fn command_steps(
    commands: &[String],
    label: &str,
    condition: Option<&str>,
    env: Option<&str>,
) -> String {
    let mut steps = String::new();
    for (index, command) in commands.iter().enumerate() {
        let name = if commands.len() == 1 {
            label.to_owned()
        } else {
            format!("{label} {}/{}", index + 1, commands.len())
        };
        let condition = condition.map_or(String::new(), |gate| format!("        if: {gate}\n"));
        let env = env.map_or(String::new(), |vars| format!("        env:\n{vars}"));
        let _ = writeln!(
            steps,
            "      - name: {name}\n{condition}{env}        shell: bash\n        run: |\n          set -euo pipefail\n          {command}"
        );
    }
    steps
}

/// Events that deploy: a push or a branch dispatch on the default branch. The
/// schedule never deploys: it verifies the live site only.
fn main_deploy_gate(default_branch: &str) -> String {
    format!(
        "github.event_name == 'push' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}

/// Runs trusted enough to publish shared reuse artifacts: the default branch,
/// a dispatch, or a same-repository pull request. Fork pull requests read
/// reuse but never publish it, so untrusted runs cannot poison trusted ones.
fn publish_guard(default_branch: &str) -> String {
    format!(
        "github.ref == 'refs/heads/{default_branch}' || github.event_name == 'workflow_dispatch' || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository)"
    )
}

/// The local check jobs the contract declares, as `(job id, commands, step
/// label)` triples. Empty stages render no job at all.
fn local_check_jobs(spec: &DocsSpec) -> Vec<(&'static str, &[String], &'static str)> {
    let mut jobs = Vec::new();
    if !spec.source_link_commands.is_empty() {
        jobs.push((
            "source-links",
            spec.source_link_commands.as_slice(),
            "Check source links",
        ));
    }
    if !spec.spell_commands.is_empty() {
        jobs.push(("spell", spec.spell_commands.as_slice(), "Check spelling"));
    }
    jobs
}

fn render_triggers(config: &ProjectConfig, spec: &DocsSpec) -> String {
    let mut triggers = format!(
        "on:\n  push:\n    branches: [{}]\n  pull_request:\n  workflow_dispatch:\n",
        yaml_scalar(&config.default_branch)
    );
    if let Some(schedule) = spec.schedule.as_deref() {
        let _ = writeln!(
            triggers,
            "  schedule:\n    - cron: {}",
            yaml_scalar(schedule)
        );
    }
    triggers
}

/// The reuse gate: look up a prior successful result for byte-identical docs
/// inputs. A hit lets pull-request runs skip every local job; default-branch
/// runs still build and deploy through the `site` job.
fn render_gate_job(runner: &str, spec: &DocsSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let hash_files = hash_files_call(&spec.docs_paths);
    format!(
        r#"  gate:
    if: github.event_name != 'schedule'
    runs-on: {runner}
    timeout-minutes: 10
    outputs:
      result-reuse: ${{{{ steps.result.outputs.hit }}}}
      result-name: ${{{{ steps.result.outputs.name }}}}
    permissions:
      contents: read
      actions: read
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Look up a prior successful docs result
        id: result
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          REPOSITORY: ${{{{ github.repository }}}}
        shell: bash
        run: |
          set -euo pipefail
          digest="${{{{ {hash_files} }}}}"
          name="docs-result-v1-${{digest:-none}}"
          artifact_id=""
          if ! artifact_id="$(gh api "repos/${{REPOSITORY}}/actions/artifacts?name=${{name}}&per_page=1" --jq '.artifacts | map(select(.expired == false)) | first | .id // empty')"; then
            echo "::warning::Docs result lookup failed; running the full pipeline"
            artifact_id=""
          fi
          if [ -n "$artifact_id" ]; then
            echo "::notice::Reusing a prior successful docs result for identical inputs"
            hit=true
          else
            hit=false
          fi
          {{
            echo "hit=$hit"
            echo "name=$name"
          }} >> "$GITHUB_OUTPUT"
"#,
    )
}

/// One always-on local check job: source links catch renames the docs path
/// filter would miss, and spelling covers prose the build never validates.
fn render_local_check_job(
    runner: &str,
    job_id: &str,
    job_name: &str,
    commands: &[String],
    label: &str,
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let steps = command_steps(commands, label, None, None);
    format!(
        r"  {job_id}:
    name: {job_name}
    if: github.event_name != 'schedule' && needs.gate.outputs.result-reuse != 'true'
    needs: gate
    runs-on: {runner}
    timeout-minutes: 15
    permissions:
      contents: read
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
{steps}",
    )
}

/// The built-site lookup: restore the stored site for identical inputs, or
/// report a miss so the build steps run. A stored archive without pages is a
/// miss, never a deployable site.
fn render_built_site_lookup(site_dir: &str, hash_files: &str) -> String {
    let escaped = bash_escape(site_dir);
    format!(
        r#"      - name: Restore the built site for identical inputs
        id: built-site
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          REPOSITORY: ${{{{ github.repository }}}}
        shell: bash
        run: |
          set -euo pipefail
          digest="${{{{ {hash_files} }}}}"
          name="docs-site-v1-${{RUNNER_OS}}-${{digest:-none}}"
          artifact_id=""
          if ! artifact_id="$(gh api "repos/${{REPOSITORY}}/actions/artifacts?name=${{name}}&per_page=1" --jq '.artifacts | map(select(.expired == false)) | first | .id // empty')"; then
            echo "::warning::Built-site lookup failed; rebuilding the site"
            artifact_id=""
          fi
          hit=false
          if [ -n "$artifact_id" ]; then
            mkdir -p "{escaped}"
            gh api "repos/${{REPOSITORY}}/actions/artifacts/${{artifact_id}}/zip" > site-archive.zip
            python3 -m zipfile -e site-archive.zip "{escaped}"
            rm site-archive.zip
            if [ -n "$(find "{escaped}" -name '*.html' -print -quit)" ]; then
              echo "::notice::Reusing the built site for identical inputs"
              hit=true
            else
              echo "::warning::Stored site archive holds no pages; rebuilding"
              rm -rf "{escaped}"
            fi
          fi
          {{
            echo "hit=$hit"
            echo "name=$name"
          }} >> "$GITHUB_OUTPUT"
"#,
    )
}

/// The build job: reuse or rebuild the site, always re-check its links, then
/// publish the built site for reuse and stage the Pages artifact for deploy.
fn render_site_job(config: &ProjectConfig, runner: &str, spec: &DocsSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let upload_artifact = ActionPin::UploadArtifact.reference();
    let upload_pages = ActionPin::UploadPages.reference();
    let hash_files = hash_files_call(&spec.docs_paths);
    let lookup = render_built_site_lookup(&spec.site_dir, &hash_files);
    let build = command_steps(
        &spec.build_commands,
        "Build the documentation site",
        Some("steps.built-site.outputs.hit != 'true'"),
        None,
    );
    let links = command_steps(
        &spec.site_link_commands,
        "Check built-site links",
        None,
        None,
    );
    let deploy_gate = main_deploy_gate(&config.default_branch);
    let publish = publish_guard(&config.default_branch);
    let site_dir = yaml_scalar(&spec.site_dir);
    format!(
        r"  site:
    name: Build and check the site
    if: github.event_name != 'schedule' && (needs.gate.outputs.result-reuse != 'true' || ({deploy_gate}))
    needs: gate
    runs-on: {runner}
    timeout-minutes: 30
    permissions:
      contents: read
      actions: write
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
{lookup}{build}{links}      - name: Publish the built site for identical inputs
        if: steps.built-site.outputs.hit != 'true' && ({publish})
        uses: {upload_artifact}
        with:
          name: ${{{{ steps.built-site.outputs.name }}}}
          path: {site_dir}
          compression-level: 0
          retention-days: 7
      - name: Stage the Pages artifact
        if: {deploy_gate}
        uses: {upload_pages}
        with:
          path: {site_dir}
",
    )
}

/// The Pages deployment: three bounded attempts over the transient handoff,
/// then a fail-closed report. Validation never retries — only this handoff.
fn render_deploy_job(config: &ProjectConfig, runner: &str, spec: &DocsSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let deploy_pages = ActionPin::DeployPages.reference();
    let deploy_gate = main_deploy_gate(&config.default_branch);
    let mut needs = vec!["site"];
    let mut checks = vec!["needs.site.result == 'success'".to_owned()];
    for (job_id, _, _) in local_check_jobs(spec) {
        needs.push(job_id);
        checks.push(format!(
            "(needs.{job_id}.result == 'success' || needs.{job_id}.result == 'skipped')"
        ));
    }
    let needs = needs.join(", ");
    let checks = checks.join(" && ");
    format!(
        r#"  deploy:
    name: Deploy to GitHub Pages
    if: always() && ({deploy_gate}) && {checks}
    needs: [{needs}]
    runs-on: {runner}
    timeout-minutes: 20
    outputs:
      page_url: ${{{{ steps.deploy-1.outputs.page_url || steps.deploy-2.outputs.page_url || steps.deploy-3.outputs.page_url }}}}
    permissions:
      contents: read
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{{{ steps.deploy-1.outputs.page_url || steps.deploy-2.outputs.page_url || steps.deploy-3.outputs.page_url }}}}
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Deploy to GitHub Pages
        id: deploy-1
        continue-on-error: true
        uses: {deploy_pages}
      - name: Wait before the Pages retry
        if: steps.deploy-1.outcome == 'failure'
        shell: bash
        run: |
          echo "::notice::Pages handoff failed on attempt 1 of 3; retrying in 30 seconds"
          sleep 30
      - name: Deploy to GitHub Pages (attempt 2 of 3)
        id: deploy-2
        if: steps.deploy-1.outcome == 'failure'
        continue-on-error: true
        uses: {deploy_pages}
      - name: Wait before the final Pages attempt
        if: steps.deploy-1.outcome == 'failure' && steps.deploy-2.outcome == 'failure'
        shell: bash
        run: |
          echo "::notice::Pages handoff failed on attempt 2 of 3; retrying in 60 seconds"
          sleep 60
      - name: Deploy to GitHub Pages (attempt 3 of 3)
        id: deploy-3
        if: steps.deploy-1.outcome == 'failure' && steps.deploy-2.outcome == 'failure'
        continue-on-error: true
        uses: {deploy_pages}
      - name: Require a successful Pages deployment
        if: steps.deploy-1.outcome != 'success' && steps.deploy-2.outcome != 'success' && steps.deploy-3.outcome != 'success'
        shell: bash
        run: |
          echo "::error::GitHub Pages refused the artifact on all 3 attempts"
          exit 1
"#,
    )
}

/// Post-deployment verification: prove the deployed sitemap answers, with a
/// bounded wait for propagation, then run the consumer-owned verify commands.
fn render_verify_job(config: &ProjectConfig, runner: &str, spec: &DocsSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let deploy_gate = main_deploy_gate(&config.default_branch);
    let sitemap = bash_escape(&spec.sitemap_path);
    let env = "          DEPLOYED_URL: ${{ needs.deploy.outputs.page_url }}\n";
    let verify = command_steps(
        &spec.verify_commands,
        "Verify the deployed site",
        None,
        Some(env),
    );
    format!(
        r#"  verify-deployed:
    name: Verify the deployed site
    if: always() && needs.deploy.result == 'success' && ({deploy_gate})
    needs: deploy
    runs-on: {runner}
    timeout-minutes: 10
    permissions:
      contents: read
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Prove the deployed sitemap answers
        env:
          DEPLOYED_URL: ${{{{ needs.deploy.outputs.page_url }}}}
        shell: bash
        run: |
          set -euo pipefail
          target="${{DEPLOYED_URL%/}}/{sitemap}"
          answered=false
          for _ in 1 2 3 4 5; do
            if curl --fail --silent --show-error --max-time 30 "$target" -o /dev/null; then
              answered=true
              break
            fi
            sleep 10
          done
          if [ "$answered" != true ]; then
            echo "::error::Deployed site never answered $target"
            exit 1
          fi
          echo "Deployed sitemap answers: $target"
{verify}"#,
    )
}

/// The scheduled-external live-link check: the only job that runs on schedule,
/// against the deployed site rather than the checkout.
fn render_live_job(runner: &str, spec: &DocsSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let steps = command_steps(
        &spec.external_link_commands,
        "Check live external links",
        None,
        None,
    );
    format!(
        r"  check-live:
    name: Check the live site
    if: github.event_name == 'schedule'
    runs-on: {runner}
    timeout-minutes: 15
    permissions:
      contents: read
    steps:
      - name: Checkout repository
        uses: {checkout}
        with:
          persist-credentials: false
{steps}",
    )
}

/// The required aggregator: fail the pipeline when any composed job failed,
/// and record the successful result for reuse.
fn render_required_job(config: &ProjectConfig, runner: &str, spec: &DocsSpec) -> String {
    let upload_artifact = ActionPin::UploadArtifact.reference();
    let publish = publish_guard(&config.default_branch);
    let mut needs = vec!["gate"];
    for (job_id, _, _) in local_check_jobs(spec) {
        needs.push(job_id);
    }
    needs.extend(["site", "deploy", "verify-deployed"]);
    let failed = needs
        .iter()
        .map(|job_id| {
            format!("(needs.{job_id}.result == 'failure' || needs.{job_id}.result == 'cancelled')")
        })
        .collect::<Vec<_>>()
        .join(" || ");
    let needs = needs.join(", ");
    format!(
        r#"  docs-required:
    name: Docs required
    if: always() && github.event_name != 'schedule'
    needs: [{needs}]
    runs-on: {runner}
    timeout-minutes: 5
    permissions:
      contents: read
      actions: write
    steps:
      - name: Require the docs pipeline to succeed
        if: {failed}
        shell: bash
        run: |
          echo "::error::Docs pipeline failed; see the failed job above"
          exit 1
      - name: Record the successful docs result
        if: needs.gate.outputs.result-reuse != 'true' && ({publish})
        shell: bash
        run: |
          printf '%s\n' "source-sha=${{GITHUB_SHA}}" > docs-result.txt
      - name: Publish the successful docs result
        if: needs.gate.outputs.result-reuse != 'true' && ({publish})
        uses: {upload_artifact}
        with:
          name: ${{{{ needs.gate.outputs.result-name }}}}
          path: docs-result.txt
          compression-level: 0
          retention-days: 7
"#,
    )
}

fn render_docs_site(config: &ProjectConfig, spec: &DocsSpec, runner: &str) -> String {
    let mut output = format!(
        "name: Docs\nrun-name: Docs \u{b7} ${{{{ github.event_name }}}}\n\n{}\npermissions:\n  contents: read\n\nconcurrency:\n  group: docs-${{{{ github.repository }}}}-${{{{ github.ref }}}}\n  cancel-in-progress: ${{{{ github.event_name == 'pull_request' }}}}\n\nenv:\n  DOCS_SITE_URL: {}\n\njobs:\n",
        render_triggers(config, spec),
        yaml_scalar(&spec.site_url),
    );
    output.push_str(&render_gate_job(runner, spec));
    for (job_id, commands, label) in local_check_jobs(spec) {
        let name = match job_id {
            "source-links" => "Check source links",
            "spell" => "Check spelling",
            _ => label,
        };
        output.push_str(&render_local_check_job(
            runner, job_id, name, commands, label,
        ));
    }
    output.push_str(&render_site_job(config, runner, spec));
    output.push_str(&render_deploy_job(config, runner, spec));
    output.push_str(&render_verify_job(config, runner, spec));
    if !spec.external_link_commands.is_empty() {
        output.push_str(&render_live_job(runner, spec));
    }
    output.push_str(&render_required_job(config, runner, spec));
    output
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
    fn must_ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}"),
            Err(error) => error,
        }
    }

    fn docs_spec() -> DocsSpec {
        DocsSpec {
            reason: "Example docs site".to_owned(),
            site_url: "https://docs.example.com".to_owned(),
            site_dir: "site".to_owned(),
            sitemap_path: "sitemap.xml".to_owned(),
            schedule: Some("17 4 * * *".to_owned()),
            build_commands: vec!["mise run docs:build".to_owned()],
            source_link_commands: vec!["mise run docs:check-source-links".to_owned()],
            site_link_commands: vec!["mise run docs:check-site-links".to_owned()],
            spell_commands: vec!["mise run docs:spell".to_owned()],
            verify_commands: vec!["mise run docs:verify-deployed".to_owned()],
            external_link_commands: vec!["mise run docs:check-live".to_owned()],
            docs_paths: vec!["**/*.md".to_owned(), "docs/**".to_owned()],
        }
    }

    fn docs_config(spec: DocsSpec) -> ProjectConfig {
        ProjectConfig {
            repository: "example/docs-fixture".to_owned(),
            workflow_revision: crate::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec![DOCS_SITE_FILE.to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            runners: crate::RunnerMode::Github,
            automatic: crate::RunnerMode::Github,
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: Vec::new(),
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: true,
            docs_reason: spec.reason.clone(),
            docs: Some(spec),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            velnor_trusted_runner_available: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: crate::DEFAULT_DISPATCH_RUNNER.to_owned(),
            automatic_lanes: crate::DEFAULT_AUTOMATIC_LANES.to_owned(),
            velnor_rust_needs: crate::VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: config::CacheGithubSection::default(),
            velnor_host_cache: config::CacheVelnorSection::default(),
        }
    }

    fn render(spec: &DocsSpec) -> String {
        let config = docs_config(spec.clone());
        let runner = must_ok(docs_runner(&config), "docs test runner resolves");
        render_docs_site(&config, spec, &runner)
    }

    #[test]
    fn local_and_scheduled_checks_split_by_event() {
        let workflow = render(&docs_spec());
        assert!(workflow.contains("branches: [main]"), "{workflow}");
        assert!(workflow.contains("- cron: \"17 4 * * *\""), "{workflow}");
        for job in ["gate:", "source-links:", "  site:", "  spell:"] {
            let block = must_some(workflow.split(job).nth(1), "the local job renders");
            let gate = must_some(
                block.lines().find(|line| line.contains("if:")),
                "the local job gates on the event",
            );
            assert!(
                gate.contains("github.event_name != 'schedule'"),
                "local job {job} skips the schedule: {gate}"
            );
        }
        let live = must_some(workflow.split("check-live:").nth(1), "the live job renders");
        let live = must_some(
            live.split("docs-required:").next(),
            "the live job ends before the aggregator",
        );
        let gate = must_some(
            live.lines().find(|line| line.contains("if:")),
            "the live job gates on the event",
        );
        assert!(
            gate.contains("github.event_name == 'schedule'"),
            "live job runs on the schedule only: {gate}"
        );
        assert!(
            !live.contains("!= 'schedule'"),
            "live job never runs beside local checks: {live}"
        );
    }

    #[test]
    fn scheduled_external_omitted_without_contract() {
        let mut spec = docs_spec();
        spec.schedule = None;
        spec.external_link_commands.clear();
        let workflow = render(&spec);
        assert!(!workflow.contains("schedule:"), "{workflow}");
        assert!(!workflow.contains("check-live"), "{workflow}");
        assert!(
            !workflow.contains("Check live external links"),
            "{workflow}"
        );
    }

    #[test]
    fn deploy_retries_the_pages_handoff_three_times() {
        let workflow = render(&docs_spec());
        for attempt in ["deploy-1", "deploy-2", "deploy-3"] {
            assert!(workflow.contains(attempt), "{workflow}");
        }
        assert!(workflow.contains("attempt 1 of 3"), "{workflow}");
        assert!(workflow.contains("attempt 2 of 3"), "{workflow}");
        assert!(workflow.contains("attempt 3 of 3"), "{workflow}");
        assert!(workflow.contains("sleep 30"), "{workflow}");
        assert!(workflow.contains("sleep 60"), "{workflow}");
        assert!(
            workflow.contains("Require a successful Pages deployment"),
            "{workflow}"
        );
        assert!(
            workflow.contains("refused the artifact on all 3 attempts"),
            "{workflow}"
        );
        assert!(workflow.contains("page_url:"), "{workflow}");
        assert!(
            workflow.contains(ActionPin::DeployPages.reference()),
            "{workflow}"
        );
        assert!(
            workflow.contains(ActionPin::UploadPages.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn post_deploy_verification_fails_closed() {
        let workflow = render(&docs_spec());
        let verify = must_some(
            workflow.split("verify-deployed:").nth(1),
            "the verify job renders",
        );
        let verify = must_some(
            verify.split("check-live:").next(),
            "the verify job ends before the live job",
        );
        assert!(
            verify.contains("needs.deploy.result == 'success'"),
            "{verify}"
        );
        assert!(
            verify.contains("Prove the deployed sitemap answers"),
            "{verify}"
        );
        assert!(verify.contains("curl --fail"), "{verify}");
        assert!(verify.contains("Deployed site never answered"), "{verify}");
        assert!(verify.contains("mise run docs:verify-deployed"), "{verify}");
        assert!(verify.contains("DEPLOYED_URL"), "{verify}");
    }

    #[test]
    fn artifact_reuse_round_trip_uses_guarded_names() {
        let workflow = render(&docs_spec());
        assert!(workflow.contains("docs-result-v1-"), "{workflow}");
        assert!(workflow.contains("docs-site-v1-"), "{workflow}");
        assert!(
            workflow.contains("Look up a prior successful docs result"),
            "{workflow}"
        );
        assert!(
            workflow.contains("Restore the built site for identical inputs"),
            "{workflow}"
        );
        assert!(
            workflow.contains("Publish the built site for identical inputs"),
            "{workflow}"
        );
        assert!(
            workflow.contains("Publish the successful docs result"),
            "{workflow}"
        );
        assert!(
            workflow.contains("github.event.pull_request.head.repo.full_name == github.repository"),
            "fork runs never publish reuse: {workflow}"
        );
        assert!(
            workflow.contains(ActionPin::UploadArtifact.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn empty_stages_render_no_job() {
        let mut spec = docs_spec();
        spec.source_link_commands.clear();
        spec.spell_commands.clear();
        spec.external_link_commands.clear();
        spec.schedule = None;
        let workflow = render(&spec);
        assert!(!workflow.contains("source-links"), "{workflow}");
        assert!(!workflow.contains("spell"), "{workflow}");
        assert!(!workflow.contains("check-live"), "{workflow}");
        assert!(
            workflow.contains("needs: [gate, site, deploy, verify-deployed]"),
            "{workflow}"
        );
    }

    #[test]
    fn consumer_contract_stays_verbatim() {
        let workflow = render(&docs_spec());
        assert!(workflow.contains("https://docs.example.com"), "{workflow}");
        assert!(workflow.contains("path: site"), "{workflow}");
        for command in [
            "mise run docs:build",
            "mise run docs:check-source-links",
            "mise run docs:check-site-links",
            "mise run docs:spell",
            "mise run docs:verify-deployed",
            "mise run docs:check-live",
        ] {
            assert!(workflow.contains(command), "{workflow}");
        }
        assert!(
            workflow.contains("hashFiles('**/*.md', 'docs/**')"),
            "{workflow}"
        );
        assert!(
            !workflow.contains("scripts/generate-docs.ts"),
            "no hardcoded generator script path: {workflow}"
        );
    }

    #[test]
    fn velnor_lane_without_labels_fails_closed() {
        let mut config = docs_config(docs_spec());
        config.runners = crate::RunnerMode::Velnor;
        let error = must_fail(
            docs_runner(&config),
            "a Velnor lane without labels is a usage error",
        );
        assert!(error.to_string().contains("velnor_labels"), "{error}");
    }

    #[test]
    fn side_file_predicates_match_the_registry() {
        assert!(is_docs_site_side(super::super::DOCS_SITE));
        assert!(!is_docs_site_side(super::super::RENOVATE));
        assert_eq!(
            canonical_docs_site_side_file(super::super::DOCS_SITE),
            Some(DOCS_SITE_FILE)
        );
        assert_eq!(canonical_docs_site_side_file("not-a-family"), None);
        let config = docs_config(docs_spec());
        let content = must_some(
            docs_site_content(&config),
            "a docs contract renders content",
        );
        assert!(content.contains("name: Docs"), "{content}");
        let mut bare = config;
        bare.docs = None;
        assert!(docs_site_content(&bare).is_none());
    }
}

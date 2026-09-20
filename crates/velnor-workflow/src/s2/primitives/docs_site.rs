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
//! through [`DocsSpec`](crate::s2::DocsSpec). The renderer owns only structure:
//! gates, reuse lookups, the retry ladder, and failure reporting.

use std::fmt::Write as _;

use super::{Args, Primitive, ProviderAdmission, RenderCtx, Rendered, WorkflowIr};
use crate::s2::provider::{runs_on_for, ProviderId};
use crate::s2::{
    runs_on_labels_yaml, yaml_scalar, ActionPin, DocsSpec, GeneratorError, ProjectConfig,
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
    let (provider, runner) = docs_runner(config).ok()?;
    Some(format!(
        "{GENERATED_HEADER}{}",
        render_docs_site(config, spec, provider, &runner)
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

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = ctx.config.docs.clone().ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders only for a repository with `[docs] enabled = true` and a complete contract",
                ctx.family
            ))
        })?;
        let _ = args;
        // Hosted stays preferred. A local fallback is eligible only when it
        // appears in automatic_providers, and its jobs carry the canonical
        // provider admission gate. The pipeline is a single-provider job
        // family like the renovate writer, so it has no dispatch provider input.
        let (provider, runner) = docs_runner(ctx.config)?;
        let content = format!(
            "{GENERATED_HEADER}{}",
            render_docs_site(ctx.config, &spec, provider, &runner)
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

/// The provider the pipeline runs on: hosted when configured, otherwise the
/// first enabled local provider in canonical order. A local provider outside
/// `automatic_providers` is never chosen as a fallback.
fn docs_runner(config: &ProjectConfig) -> Result<(ProviderId, String), GeneratorError> {
    let provider = ProviderId::ALL
        .iter()
        .find(|provider| {
            config.providers.contains(provider)
                && (!provider.is_local() || config.automatic_providers.contains(provider))
        })
        .copied()
        .ok_or_else(|| {
            GeneratorError::usage(
                "`docs-site` needs a configured hosted runner or a local provider enabled in [workflow] automatic_providers",
            )
        })?;
    runs_on_for(&config.selectors, provider)
        .map(runs_on_labels_yaml)
        .map(|runner| (provider, runner))
}

/// Add the selected local provider's admission predicate to every job. Hosted
/// docs workflows keep their existing event gates unchanged.
fn admitted_condition(condition: &str, admission_gate: Option<&str>) -> String {
    admission_gate.map_or_else(
        || condition.to_owned(),
        |gate| format!("({gate}) && ({condition})"),
    )
}

/// The reuse-recipe contract version. It feeds the recipe fingerprint and the
/// artifact names, so bumping it invalidates every previously published reuse
/// artifact. Bump it whenever the pipeline semantics behind a reused result
/// change: new digest inputs, new verification, or a new trust rule.
const DOCS_REUSE_RECIPE_VERSION: u32 = 2;

/// Toolchain lockfiles folded into the reuse digest after the consumer's
/// `docs_paths`. A toolchain change rebuilds and re-checks even when the prose
/// is byte-identical. `hashFiles` skips patterns that match nothing, so a
/// repository carries only the lockfiles it owns.
const DOCS_TOOLCHAIN_DIGEST_PATHS: &[&str] = &[
    "mise.lock",
    "mise.toml",
    ".mise.toml",
    ".tool-versions",
    "rust-toolchain.toml",
    "rust-toolchain",
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];

/// The `hashFiles` call over the reuse-digest path filters: the consumer's
/// docs inputs first, then the toolchain lockfiles.
fn hash_files_call(paths: &[String]) -> String {
    let quoted = paths
        .iter()
        .map(String::as_str)
        .chain(DOCS_TOOLCHAIN_DIGEST_PATHS.iter().copied())
        .map(|pattern| format!("'{}'", pattern.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("hashFiles({quoted})")
}

/// Stable FNV-1a over bytes. The recipe fingerprint needs determinism across
/// runs without a dependency, not cryptographic strength: the lookup
/// re-verifies the downloaded artifact bytes before any hit.
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The reuse-recipe fingerprint: the pipeline version plus every consumer
/// command and site path that a reused result stands in for. It renders into
/// the artifact names and the published manifests, so a command-only change
/// misses reuse and runs the full pipeline. Fields are NUL-separated —
/// commands reject control characters, so the separator is unambiguous — and
/// every stage contributes a separator even when empty, so commands cannot
/// blur across stage boundaries.
fn docs_reuse_recipe(spec: &DocsSpec) -> String {
    let mut fingerprint = format!("docs-reuse-recipe/{DOCS_REUSE_RECIPE_VERSION}\0");
    for commands in [
        &spec.build_commands,
        &spec.source_link_commands,
        &spec.site_link_commands,
        &spec.spell_commands,
        &spec.verify_commands,
        &spec.external_link_commands,
    ] {
        for command in commands {
            fingerprint.push_str(command);
            fingerprint.push('\0');
        }
        fingerprint.push('\0');
    }
    fingerprint.push_str(&spec.site_dir);
    fingerprint.push('\0');
    fingerprint.push_str(&spec.sitemap_path);
    format!("{:016x}", fnv1a64(fingerprint.as_bytes()))
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

/// Runs trusted enough to publish shared reuse artifacts: the default branch
/// only.
///
/// Trust rule (trusted-producer): an artifact is published only by a run on
/// the default branch, and a lookup accepts only an artifact whose producing
/// workflow run targeted the default branch (`head_branch`, checked over the
/// API) *and* whose downloaded bytes verify against this run's recipe and
/// digest. Consumers on any ref — pull requests included — may consume such
/// a verified trusted artifact, because it proves the exact recipe and inputs
/// already passed on the trusted ref. Nothing a pull-request branch publishes
/// is ever published or accepted, so same-repo and fork pull requests alike
/// can neither poison trusted runs nor smuggle bytes into a Pages deploy.
fn publish_guard(default_branch: &str) -> String {
    format!("github.ref == 'refs/heads/{default_branch}'")
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

/// The reuse gate: look up a prior successful result for an identical recipe
/// and identical inputs. A hit lets pull-request runs skip every local job;
/// default-branch runs still build and deploy through the `site` job.
///
/// The lookup never trusts an artifact name: a candidate must come from a
/// default-branch run, download intact, hold exactly `docs-result.txt`, and
/// byte-match this run's recipe and digest before `hit` can turn true.
fn render_gate_job(
    config: &ProjectConfig,
    runner: &str,
    spec: &DocsSpec,
    admission_gate: Option<&str>,
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let hash_files = hash_files_call(&spec.docs_paths);
    let recipe = docs_reuse_recipe(spec);
    let branch = bash_escape(&config.default_branch);
    let condition = admitted_condition("github.event_name != 'schedule'", admission_gate);
    format!(
        r#"  gate:
    if: {condition}
    runs-on: {runner}
    timeout-minutes: 10
    outputs:
      result-reuse: ${{{{ steps.result.outputs.hit }}}}
      result-name: ${{{{ steps.result.outputs.name }}}}
      result-digest: ${{{{ steps.result.outputs.digest }}}}
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
          name="docs-result-v{DOCS_REUSE_RECIPE_VERSION}-{recipe}-${{digest:-none}}"
          hit=false
          candidates=""
          if ! candidates="$(gh api "repos/${{REPOSITORY}}/actions/artifacts?name=${{name}}&per_page=10" --jq '[.artifacts[] | select(.expired == false) | "\(.id) \(.workflow_run.id)"] | join("\n")')"; then
            echo "::warning::Docs result lookup failed; running the full pipeline"
            candidates=""
          fi
          while IFS= read -r candidate; do
            [ -n "$candidate" ] || continue
            artifact_id="${{candidate%% *}}"
            run_id="${{candidate##* }}"
            if [ -z "$artifact_id" ] || [ -z "$run_id" ]; then
              continue
            fi
            branch="$(gh api "repos/${{REPOSITORY}}/actions/runs/${{run_id}}" --jq '.head_branch // empty' 2>/dev/null || true)"
            if [ "$branch" != "{branch}" ]; then
              continue
            fi
            rm -rf .docs-result-restore
            mkdir -p .docs-result-restore
            if ! gh api "repos/${{REPOSITORY}}/actions/artifacts/${{artifact_id}}/zip" > result-archive.zip 2>/dev/null; then
              continue
            fi
            python3 -m zipfile -e result-archive.zip .docs-result-restore
            rm -f result-archive.zip
            if [ ! -f .docs-result-restore/docs-result.txt ]; then
              continue
            fi
            if [ "$(find .docs-result-restore -mindepth 1 | wc -l | tr -d ' ')" != "1" ]; then
              echo "::warning::Docs result artifact holds unexpected files; ignoring it"
              continue
            fi
            if printf 'recipe=%s\ninputs=%s\n' "{recipe}" "$digest" | cmp -s - .docs-result-restore/docs-result.txt; then
              echo "::notice::Reusing a verified docs result for an identical recipe and inputs"
              hit=true
              break
            else
              echo "::warning::Docs result bytes do not match this recipe and inputs; ignoring them"
            fi
          done <<< "$candidates"
          rm -rf .docs-result-restore result-archive.zip
          {{
            echo "hit=$hit"
            echo "name=$name"
            echo "digest=$digest"
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
    admission_gate: Option<&str>,
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let steps = command_steps(commands, label, None, None);
    let condition = admitted_condition(
        "github.event_name != 'schedule' && needs.gate.outputs.result-reuse != 'true'",
        admission_gate,
    );
    format!(
        r"  {job_id}:
    name: {job_name}
    if: {condition}
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

/// The built-site lookup: restore the stored site for an identical recipe and
/// identical inputs, or report a miss so the build steps run. Like the gate,
/// it never trusts an artifact name: the candidate must come from a
/// default-branch run and its reuse receipt must byte-match this run's recipe
/// and digest. A stored archive without a matching receipt — or without
/// pages — is a miss, never a deployable site.
fn render_built_site_lookup(spec: &DocsSpec, hash_files: &str, default_branch: &str) -> String {
    let escaped = bash_escape(&spec.site_dir);
    let recipe = docs_reuse_recipe(spec);
    let branch = bash_escape(default_branch);
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
          name="docs-site-v{DOCS_REUSE_RECIPE_VERSION}-{recipe}-${{RUNNER_OS}}-${{digest:-none}}"
          hit=false
          candidates=""
          if ! candidates="$(gh api "repos/${{REPOSITORY}}/actions/artifacts?name=${{name}}&per_page=10" --jq '[.artifacts[] | select(.expired == false) | "\(.id) \(.workflow_run.id)"] | join("\n")')"; then
            echo "::warning::Built-site lookup failed; rebuilding the site"
            candidates=""
          fi
          while IFS= read -r candidate; do
            [ -n "$candidate" ] || continue
            artifact_id="${{candidate%% *}}"
            run_id="${{candidate##* }}"
            if [ -z "$artifact_id" ] || [ -z "$run_id" ]; then
              continue
            fi
            branch="$(gh api "repos/${{REPOSITORY}}/actions/runs/${{run_id}}" --jq '.head_branch // empty' 2>/dev/null || true)"
            if [ "$branch" != "{branch}" ]; then
              continue
            fi
            rm -rf "{escaped}"
            mkdir -p "{escaped}"
            if ! gh api "repos/${{REPOSITORY}}/actions/artifacts/${{artifact_id}}/zip" > site-archive.zip 2>/dev/null; then
              rm -rf "{escaped}"
              continue
            fi
            python3 -m zipfile -e site-archive.zip "{escaped}"
            rm -f site-archive.zip
            receipt="{escaped}/docs-site-receipt.txt"
            if [ ! -f "$receipt" ]; then
              echo "::warning::Stored site archive holds no reuse receipt; rebuilding"
              rm -rf "{escaped}"
              continue
            fi
            if printf 'recipe=%s\ninputs=%s\n' "{recipe}" "$digest" | cmp -s - "$receipt"; then
              rm -f "$receipt"
            else
              echo "::warning::Stored site bytes do not match this recipe and inputs; rebuilding"
              rm -rf "{escaped}"
              continue
            fi
            if [ -n "$(find "{escaped}" -name '*.html' -print -quit)" ]; then
              echo "::notice::Reusing the verified built site for an identical recipe and inputs"
              hit=true
              break
            else
              echo "::warning::Stored site archive holds no pages; rebuilding"
              rm -rf "{escaped}"
            fi
          done <<< "$candidates"
          rm -f site-archive.zip
          {{
            echo "hit=$hit"
            echo "name=$name"
            echo "digest=$digest"
          }} >> "$GITHUB_OUTPUT"
"#,
    )
}

/// The build job: reuse or rebuild the site, always re-check its links, then
/// publish the built site for reuse and stage the Pages artifact for deploy.
fn render_site_job(
    config: &ProjectConfig,
    runner: &str,
    spec: &DocsSpec,
    admission_gate: Option<&str>,
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let upload_artifact = ActionPin::UploadArtifact.reference();
    let upload_pages = ActionPin::UploadPages.reference();
    let hash_files = hash_files_call(&spec.docs_paths);
    let lookup = render_built_site_lookup(spec, &hash_files, &config.default_branch);
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
    let escaped = bash_escape(&spec.site_dir);
    let recipe = docs_reuse_recipe(spec);
    let condition = admitted_condition(
        &format!(
            "github.event_name != 'schedule' && (needs.gate.outputs.result-reuse != 'true' || ({deploy_gate}))"
        ),
        admission_gate,
    );
    // The receipt rides inside the uploaded site directory so the lookup can
    // verify the exact bytes it restores. All three receipt steps share one
    // condition: the receipt is written if and only if the publish runs, and
    // removed right after, so it never reaches the Pages artifact — and a
    // failed publish stops the job before staging.
    format!(
        r#"  site:
    name: Build and check the site
    if: {condition}
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
{lookup}{build}{links}      - name: Write the reuse receipt for this recipe and inputs
        if: steps.built-site.outputs.hit != 'true' && ({publish})
        shell: bash
        run: |
          set -euo pipefail
          {{
            echo "recipe={recipe}"
            echo "inputs=${{{{ steps.built-site.outputs.digest }}}}"
          }} > "{escaped}/docs-site-receipt.txt"
      - name: Publish the built site for identical inputs
        if: steps.built-site.outputs.hit != 'true' && ({publish})
        uses: {upload_artifact}
        with:
          name: ${{{{ steps.built-site.outputs.name }}}}
          path: {site_dir}
          compression-level: 0
          retention-days: 7
      - name: Remove the reuse receipt before staging
        if: steps.built-site.outputs.hit != 'true' && ({publish})
        shell: bash
        run: |
          rm -f "{escaped}/docs-site-receipt.txt"
      - name: Stage the Pages artifact
        if: {deploy_gate}
        uses: {upload_pages}
        with:
          path: {site_dir}
"#,
    )
}

/// The Pages deployment: three bounded attempts over the transient handoff,
/// then a fail-closed report. Validation never retries — only this handoff.
fn render_deploy_job(
    config: &ProjectConfig,
    runner: &str,
    spec: &DocsSpec,
    admission_gate: Option<&str>,
) -> String {
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
    let condition = admitted_condition(
        &format!("always() && ({deploy_gate}) && {checks}"),
        admission_gate,
    );
    format!(
        r#"  deploy:
    name: Deploy to GitHub Pages
    if: {condition}
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
fn render_verify_job(
    config: &ProjectConfig,
    runner: &str,
    spec: &DocsSpec,
    admission_gate: Option<&str>,
) -> String {
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
    let condition = admitted_condition(
        &format!("always() && needs.deploy.result == 'success' && ({deploy_gate})"),
        admission_gate,
    );
    format!(
        r#"  verify-deployed:
    name: Verify the deployed site
    if: {condition}
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
fn render_live_job(runner: &str, spec: &DocsSpec, admission_gate: Option<&str>) -> String {
    let checkout = ActionPin::Checkout.reference();
    let steps = command_steps(
        &spec.external_link_commands,
        "Check live external links",
        None,
        None,
    );
    let condition = admitted_condition("github.event_name == 'schedule'", admission_gate);
    format!(
        r"  check-live:
    name: Check the live site
    if: {condition}
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
/// and record the successful result for reuse. The manifest stamps the exact
/// recipe and digest the gate verified, so a future lookup compares bytes,
/// never a name.
fn render_required_job(
    config: &ProjectConfig,
    runner: &str,
    spec: &DocsSpec,
    admission_gate: Option<&str>,
) -> String {
    let upload_artifact = ActionPin::UploadArtifact.reference();
    let publish = publish_guard(&config.default_branch);
    let recipe = docs_reuse_recipe(spec);
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
    let condition = admitted_condition(
        "always() && github.event_name != 'schedule'",
        admission_gate,
    );
    format!(
        r#"  docs-required:
    name: Docs required
    if: {condition}
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
          printf '%s\n' "recipe={recipe}" "inputs=${{{{ needs.gate.outputs.result-digest }}}}" > docs-result.txt
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

fn render_docs_site(
    config: &ProjectConfig,
    spec: &DocsSpec,
    provider: ProviderId,
    runner: &str,
) -> String {
    let admission_gate = provider.is_local().then(|| {
        WorkflowIr::from_config(config)
            .provider_admission_expression(ProviderAdmission::ProviderTrusted(provider))
    });
    let admission_gate = admission_gate.as_deref();
    let mut output = format!(
        "name: Docs\nrun-name: Docs \u{b7} ${{{{ github.event_name }}}}\n\n{}\npermissions:\n  contents: read\n\nconcurrency:\n  group: docs-${{{{ github.repository }}}}-${{{{ github.ref }}}}\n  cancel-in-progress: ${{{{ github.event_name == 'pull_request' }}}}\n\nenv:\n  DOCS_SITE_URL: {}\n\njobs:\n",
        render_triggers(config, spec),
        yaml_scalar(&spec.site_url),
    );
    output.push_str(&render_gate_job(config, runner, spec, admission_gate));
    for (job_id, commands, label) in local_check_jobs(spec) {
        let name = match job_id {
            "source-links" => "Check source links",
            "spell" => "Check spelling",
            _ => label,
        };
        output.push_str(&render_local_check_job(
            runner,
            job_id,
            name,
            commands,
            label,
            admission_gate,
        ));
    }
    output.push_str(&render_site_job(config, runner, spec, admission_gate));
    output.push_str(&render_deploy_job(config, runner, spec, admission_gate));
    output.push_str(&render_verify_job(config, runner, spec, admission_gate));
    if !spec.external_link_commands.is_empty() {
        output.push_str(&render_live_job(runner, spec, admission_gate));
    }
    output.push_str(&render_required_job(config, runner, spec, admission_gate));
    output
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
            workflow_revision: crate::s2::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::s2::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec![DOCS_SITE_FILE.to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::GithubHosted,
            ]),
            automatic_providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::GithubHosted,
            ]),
            default_dispatch_providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::GithubHosted,
            ]),
            selectors: std::collections::BTreeMap::from([(
                crate::s2::provider::ProviderId::GithubHosted,
                crate::s2::provider::ProviderSelector {
                    runs_on: vec!["ubuntu-24.04".to_owned()],
                },
            )]),
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: true,
            docs_reason: spec.reason.clone(),
            docs: Some(spec),
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
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: config::CacheGithubSection::default(),
            velnor_host_cache: config::CacheVelnorSection::default(),
        }
    }

    fn render(spec: &DocsSpec) -> String {
        let config = docs_config(spec.clone());
        let (provider, runner) = must_ok(docs_runner(&config), "docs test runner resolves");
        render_docs_site(&config, spec, provider, &runner)
    }

    fn velnor_config(spec: DocsSpec) -> ProjectConfig {
        let mut config = docs_config(spec);
        config.providers =
            std::collections::BTreeSet::from([crate::s2::provider::ProviderId::Velnor]);
        config.automatic_providers = config.providers.clone();
        config.default_dispatch_providers.clear();
        config.selectors = std::collections::BTreeMap::from([(
            crate::s2::provider::ProviderId::Velnor,
            crate::s2::provider::ProviderSelector {
                runs_on: vec!["self-hosted".to_owned(), "example-fleet".to_owned()],
            },
        )]);
        config
    }

    #[test]
    fn local_and_scheduled_checks_split_by_event() {
        let workflow = render(&docs_spec());
        assert!(workflow.contains("branches: [main]"), "{workflow}");
        assert!(workflow.contains("- cron: \"17 4 * * *\""), "{workflow}");
        assert!(
            !workflow.contains("github.repository =="),
            "hosted docs jobs keep their existing event gates: {workflow}"
        );
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
        assert!(workflow.contains("docs-result-v2-"), "{workflow}");
        assert!(workflow.contains("docs-site-v2-"), "{workflow}");
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
            workflow.contains(ActionPin::UploadArtifact.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn reuse_recipe_covers_commands_toolchain_and_version() {
        let spec = docs_spec();
        let recipe = docs_reuse_recipe(&spec);
        assert_eq!(recipe, docs_reuse_recipe(&spec.clone()));
        assert!(
            recipe.len() == 16
                && recipe
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "the recipe is a 16-character lowercase hex fingerprint: {recipe}"
        );
        // A command-only change in any stage misses reuse: the fingerprint —
        // and therefore the artifact names — must move.
        let mut changed = spec.clone();
        changed.build_commands = vec!["mise run docs:build --strict".to_owned()];
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "build commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.source_link_commands = vec![
            "mise run docs:check-source-links".to_owned(),
            "extra".to_owned(),
        ];
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "source-link commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.site_link_commands.clear();
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "site-link commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.spell_commands = vec!["mise run docs:spell --fix".to_owned()];
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "spell commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.verify_commands.clear();
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "verify commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.external_link_commands.clear();
        changed.schedule = None;
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "external-link commands feed the recipe"
        );
        let mut changed = spec.clone();
        changed.site_dir = "public".to_owned();
        assert_ne!(
            recipe,
            docs_reuse_recipe(&changed),
            "the site directory feeds the recipe"
        );
        // Commands cannot blur across stage boundaries: the same words in two
        // stages fingerprint differently than joined in one.
        let mut joined = spec.clone();
        let second = joined.site_link_commands.clone();
        joined.build_commands.extend(second);
        joined.site_link_commands.clear();
        assert_ne!(
            recipe,
            docs_reuse_recipe(&joined),
            "stage boundaries feed the recipe"
        );
        let workflow = render(&spec);
        assert!(
            workflow.contains(&format!("docs-result-v2-{recipe}-")),
            "the result name keys on the recipe: {workflow}"
        );
        assert!(
            workflow.contains(&format!("docs-site-v2-{recipe}-")),
            "the site name keys on the recipe: {workflow}"
        );
    }

    #[test]
    fn reuse_hit_requires_verified_trusted_bytes() {
        let workflow = render(&docs_spec());
        let gate = must_some(
            workflow
                .split("Look up a prior successful docs result")
                .nth(1),
            "the gate lookup renders",
        );
        let gate = must_some(gate.split("source-links:").next(), "the gate lookup ends");
        for marker in [
            ".head_branch // empty",
            "/artifacts/${artifact_id}/zip",
            "docs-result.txt",
            "cmp -s",
            "hit=true",
        ] {
            assert!(gate.contains(marker), "the gate verifies {marker}: {gate}");
        }
        let position =
            |marker: &str| must_some(gate.find(marker), "the gate lookup verifies before it hits");
        assert!(
            position("hit=true") > position("cmp -s"),
            "bytes verify before the hit: {gate}"
        );
        assert!(
            position("cmp -s") > position("/artifacts/${artifact_id}/zip"),
            "bytes download before they verify: {gate}"
        );
        assert!(
            gate.contains("holds unexpected files"),
            "the gate enforces the expected file set: {gate}"
        );
        let site = must_some(
            workflow
                .split("Restore the built site for identical inputs")
                .nth(1),
            "the site lookup renders",
        );
        let site = must_some(site.split("  deploy:").next(), "the site job ends");
        for marker in [
            ".head_branch // empty",
            "docs-site-receipt.txt",
            "cmp -s",
            "hit=true",
        ] {
            assert!(
                site.contains(marker),
                "the site lookup verifies {marker}: {site}"
            );
        }
        assert!(
            must_some(site.find("hit=true"), "the site lookup hits")
                > must_some(site.find("cmp -s"), "the site lookup verifies"),
            "site bytes verify before the hit: {site}"
        );
        assert!(
            must_some(
                site.find("Remove the reuse receipt before staging"),
                "the receipt is removed"
            ) < must_some(site.find("Stage the Pages artifact"), "the site is staged"),
            "the receipt never reaches the Pages artifact: {site}"
        );
        assert!(
            workflow.contains("Write the reuse receipt for this recipe and inputs"),
            "{workflow}"
        );
        let required = must_some(
            workflow.split("docs-required:").nth(1),
            "the aggregator renders",
        );
        assert!(
            required.contains("needs.gate.outputs.result-digest"),
            "the manifest stamps the verified digest: {required}"
        );
    }

    #[test]
    fn pull_request_runs_never_publish_or_satisfy_trusted_reuse() {
        let workflow = render(&docs_spec());
        for line in workflow.lines().filter(|line| {
            line.contains("needs.gate.outputs.result-reuse != 'true' && (")
                || line.contains("steps.built-site.outputs.hit != 'true' && (")
        }) {
            assert!(
                line.contains("github.ref == 'refs/heads/main'"),
                "reuse publishes only on the default branch: {line}"
            );
            assert!(
                !line.contains("pull_request"),
                "pull requests never publish reuse: {line}"
            );
        }
        assert!(
            !workflow.contains("head.repo.full_name"),
            "no pull-request branch clause remains in the reuse path: {workflow}"
        );
        let trusted_checks = workflow
            .lines()
            .filter(|line| line.contains(r#"[ "$branch" != "main" ]"#))
            .count();
        assert!(
            trusted_checks >= 2,
            "both lookups refuse artifacts from other branches: {workflow}"
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
            workflow.contains("hashFiles('**/*.md', 'docs/**', 'mise.lock'"),
            "consumer paths lead the reuse digest: {workflow}"
        );
        for lockfile in DOCS_TOOLCHAIN_DIGEST_PATHS {
            assert!(
                workflow.contains(&format!("'{lockfile}'")),
                "the toolchain pins feed the reuse digest: {workflow}"
            );
        }
        assert!(
            !workflow.contains("scripts/generate-docs.ts"),
            "no hardcoded generator script path: {workflow}"
        );
    }

    #[test]
    fn velnor_only_universe_without_a_selector_fails_closed() {
        let mut config = docs_config(docs_spec());
        config.providers =
            std::collections::BTreeSet::from([crate::s2::provider::ProviderId::Velnor]);
        config.automatic_providers = config.providers.clone();
        config.selectors = std::collections::BTreeMap::new();
        let error = must_fail(
            docs_runner(&config),
            "a velnor-only universe without a selector is a usage error",
        );
        assert!(error.to_string().contains("velnor"), "{error}");
    }

    #[test]
    fn velnor_only_universe_runs_on_the_velnor_selector() {
        let config = velnor_config(docs_spec());
        let (provider, runner) = must_ok(docs_runner(&config), "docs test runner resolves");
        assert_eq!(provider, crate::s2::provider::ProviderId::Velnor);
        assert!(runner.contains("self-hosted"), "{runner}");
    }

    #[test]
    fn local_provider_jobs_use_the_canonical_admission_gate() {
        let spec = docs_spec();
        let config = velnor_config(spec.clone());
        let (provider, runner) = must_ok(docs_runner(&config), "docs test runner resolves");
        let admission = WorkflowIr::from_config(&config).provider_admission_expression(
            ProviderAdmission::ProviderTrusted(crate::s2::provider::ProviderId::Velnor),
        );
        assert_eq!(provider, crate::s2::provider::ProviderId::Velnor);
        let workflow = render_docs_site(&config, &spec, provider, &runner);
        for job in [
            "gate:",
            "source-links:",
            "spell:",
            "  site:",
            "  deploy:",
            "verify-deployed:",
            "check-live:",
            "docs-required:",
        ] {
            let block = must_some(workflow.split(job).nth(1), "the local job renders");
            let condition = must_some(
                block.lines().find(|line| line.contains("if:")),
                "the local job has an admission condition",
            );
            assert!(
                condition.contains(&admission),
                "local job {job} uses the canonical provider gate: {condition}"
            );
        }
        assert!(
            admission.contains("github.repository == 'example/docs-fixture'"),
            "local admission binds to the configured repository: {admission}"
        );
        assert!(
            admission.contains("github.event_name == 'push' && github.ref == 'refs/heads/main'"),
            "local admission is restricted to the default-branch push: {admission}"
        );
    }

    #[test]
    fn disabled_local_provider_is_not_selected_as_a_docs_fallback() {
        let mut config = velnor_config(docs_spec());
        config.automatic_providers.clear();
        let error = must_fail(
            docs_runner(&config),
            "a disabled local provider cannot serve as the docs fallback",
        );
        assert!(error.to_string().contains("automatic_providers"), "{error}");
        assert!(
            docs_site_content(&config).is_none(),
            "no workflow renders without an eligible provider"
        );
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

    #[test]
    fn default_render_carries_no_providers_surface() {
        let workflow = render(&docs_spec());
        assert!(
            workflow.contains("  workflow_dispatch:\n"),
            "the dispatch trigger stays bare: {workflow}"
        );
        for marker in ["providers:", "inputs.providers", "fromJSON", "CI_LANE"] {
            assert!(
                !workflow.contains(marker),
                "an undeclared providers input renders nothing: {marker} in {workflow}"
            );
        }
        let runs_on: Vec<&str> = workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("runs-on:"))
            .collect();
        assert_eq!(runs_on.len(), 8, "{workflow}");
        for line in runs_on {
            assert_eq!(
                line.trim(),
                "runs-on: ubuntu-24.04",
                "every job stays on the static runner: {workflow}"
            );
        }
    }
}

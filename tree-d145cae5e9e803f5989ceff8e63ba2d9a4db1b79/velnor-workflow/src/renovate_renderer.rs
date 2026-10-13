//! Canonical Renovate workflow rendering shared by schema 1 and schema 2.
//!
//! The schema modules adapt their runner and dispatch contracts into these
//! inputs. Keeping the workflow templates and cache-key construction here
//! prevents one schema from silently retaining a stale renderer.

use std::fmt::Write as _;

/// The pinned Renovate OSS version rendered into `renovate-version`.
pub(crate) const RENOVATE_OSS_VERSION: &str = "44.93.6";

/// Schema-specific runner and dispatch values plus the shared Renovate
/// contract consumed by the canonical writer renderer.
pub(crate) struct WriterInput<'a> {
    pub(crate) checkout: &'a str,
    pub(crate) cache_restore: &'a str,
    pub(crate) cache_save: &'a str,
    pub(crate) renovate_action: &'a str,
    pub(crate) runner: &'a str,
    pub(crate) dispatch_inputs: &'a str,
    pub(crate) default_branch: &'a str,
    pub(crate) token: &'a str,
    pub(crate) config_path: &'a str,
    pub(crate) schedule: &'a str,
    pub(crate) schedules: &'a [String],
    pub(crate) repositories: &'a [String],
    pub(crate) host_rules_secret: Option<&'a str>,
    pub(crate) author: Option<&'a str>,
    pub(crate) signoff: bool,
    pub(crate) allowed_commands: &'a [String],
    pub(crate) cache: bool,
}

/// Schema-specific runner values plus the shared Renovate validator contract.
pub(crate) struct ValidateInput<'a> {
    pub(crate) checkout: &'a str,
    pub(crate) runner: &'a str,
    /// The job-level trusted-event gate. `None` on hosted runners (which
    /// need no gate); `Some` on local runners, where the validator mounts
    /// the pull-request checkout and must skip fork and bot pull requests.
    pub(crate) gate: Option<&'a str>,
    pub(crate) default_branch: &'a str,
    pub(crate) config_path: &'a str,
}

/// Render the Renovate writer workflow for both schema generations.
#[expect(
    clippy::too_many_lines,
    reason = "the canonical workflow template stays contiguous for byte-level review"
)]
pub(crate) fn render_writer(input: &WriterInput<'_>) -> String {
    let gate = trusted_renovate_gate(input.default_branch);
    let token_secret = format!("secrets.{}", input.token);
    let cache_save_gate = crate::primitives::trusted_cache_save_expression(input.default_branch);
    let config_env = renovate_config_env(input.config_path);
    let schedules = renovate_schedules(input.schedule, input.schedules);
    let target_env = renovate_target_env(input.repositories);
    let host_rules_env = renovate_host_rules_env(input.host_rules_secret);
    let author_env = renovate_author_env(input.author);
    let signoff_env = renovate_signoff_env(input.author, input.signoff);
    let allowed_commands_env = renovate_allowed_commands_env(input.allowed_commands);
    let hash_files = cache_hash_files(input.config_path);
    let cache_path = renovate_repository_cache_path();

    let mut cache_steps = String::new();
    if input.cache {
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
",
            cache_restore = input.cache_restore,
            cache_path = cache_path,
            hash_files = hash_files,
        );
    }

    let mut cache_save_step = String::new();
    if input.cache {
        let _ = write!(
            cache_save_step,
            r"      - name: Save Renovate repository cache
        if: always() && ({cache_save_gate}) && steps.renovate-cache.outputs.cache-hit != 'true'
        uses: {cache_save}
        with:
          path: {cache_path}
          key: velnor-renovate-${{{{ github.repository }}}}-{hash_files}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}
",
            cache_save_gate = cache_save_gate,
            cache_save = input.cache_save,
            cache_path = cache_path,
            hash_files = hash_files,
        );
    }

    format!(
        r#"name: Renovate
run-name: Renovate · ${{{{ github.event_name }}}}

on:
  schedule:
{schedules}  workflow_dispatch:{dispatch_inputs}

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
        dispatch_inputs = input.dispatch_inputs,
        schedules = schedules,
        gate = gate,
        runner = input.runner,
        token_secret = token_secret,
        checkout = input.checkout,
        cache_steps = cache_steps,
        renovate_action = input.renovate_action,
        version = crate::yaml_scalar(RENOVATE_OSS_VERSION),
        token = input.token,
        config_env = config_env,
        target_env = target_env,
        host_rules_env = host_rules_env,
        author_env = author_env,
        signoff_env = signoff_env,
        allowed_commands_env = allowed_commands_env,
        cache_save_step = cache_save_step,
    )
}

/// Render the Renovate configuration validator for both schema generations.
pub(crate) fn render_validate(input: &ValidateInput<'_>) -> String {
    let config_arg = shell_escape(input.config_path);
    let gate = input
        .gate
        .map_or_else(String::new, |gate| format!("    if: ${{{{ ({gate}) }}}}\n"));

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
{gate}    runs-on: {runner}
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
        default_branch = input.default_branch,
        runner = input.runner,
        checkout = input.checkout,
        version = RENOVATE_OSS_VERSION,
    )
}

fn trusted_renovate_gate(default_branch: &str) -> String {
    format!(
        "github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}

fn renovate_repository_cache_path() -> &'static str {
    "/tmp/renovate/cache/${{ github.repository }}/renovate/repository"
}

fn renovate_config_env(config_path: &str) -> String {
    if config_path == "renovate.json" {
        String::new()
    } else {
        format!(
            "          RENOVATE_CONFIG_FILE: {}\n",
            crate::yaml_scalar(config_path)
        )
    }
}

/// The `on.schedule` cron list: the primary schedule plus every declared
/// extra, each once.
fn renovate_schedules(schedule: &str, extras: &[String]) -> String {
    let mut schedules = format!("    - cron: {}\n", crate::yaml_scalar(schedule));
    for extra in extras {
        if extra != schedule {
            let _ = writeln!(schedules, "    - cron: {}", crate::yaml_scalar(extra));
        }
    }
    schedules
}

/// Repository targets: explicit targets disable autodiscovery, so the writer
/// renovates exactly the declared slugs. Empty keeps autodiscovery.
fn renovate_target_env(repositories: &[String]) -> String {
    if repositories.is_empty() {
        String::new()
    } else {
        format!(
            "          RENOVATE_AUTODISCOVER: \"false\"\n          RENOVATE_REPOSITORIES: {}\n",
            crate::yaml_scalar(&repositories.join(","))
        )
    }
}

/// Private-registry credentials: the secret holds the JSON `hostRules`
/// array, so credentials travel by reference and never as workflow text.
fn renovate_host_rules_env(secret: Option<&str>) -> String {
    secret.map_or_else(String::new, |secret| {
        format!("          RENOVATE_HOST_RULES: ${{{{ secrets.{secret} }}}}\n")
    })
}

/// The git author Renovate commits as.
fn renovate_author_env(author: Option<&str>) -> String {
    author.map_or_else(String::new, |author| {
        format!(
            "          RENOVATE_GIT_AUTHOR: {}\n",
            crate::yaml_scalar(author)
        )
    })
}

/// DCO sign-off: the commit body carries a `Signed-off-by` trailer for the
/// declared author. The repository's DCO check stays the enforcement; this
/// only makes the writer produce commits that pass it.
fn renovate_signoff_env(author: Option<&str>, signoff: bool) -> String {
    if !signoff {
        return String::new();
    }
    author.map_or_else(String::new, |author| {
        format!(
            "          RENOVATE_COMMIT_BODY: {}\n",
            crate::yaml_scalar(&format!("Signed-off-by: {author}"))
        )
    })
}

/// Execution allowances: the `allowedCommands` regex allowlist for
/// post-upgrade commands, as the JSON array Renovate parses it from.
fn renovate_allowed_commands_env(allowed_commands: &[String]) -> String {
    if allowed_commands.is_empty() {
        return String::new();
    }
    let json = serde_json::to_string(allowed_commands).unwrap_or_else(|_| "[]".to_owned());
    format!(
        "          RENOVATE_ALLOWED_COMMANDS: {}\n",
        crate::yaml_scalar(&json)
    )
}

fn cache_hash_files(config_path: &str) -> String {
    format!(
        "${{{{ hashFiles('{}') }}}}",
        config_path.replace('\'', "''")
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

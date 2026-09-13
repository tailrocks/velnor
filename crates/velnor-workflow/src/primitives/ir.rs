//! The resolved render environment for the declared CI surface.
//!
//! `WorkflowIr` is the lane and toolchain context every primitive renders
//! against: which lanes exist, which runner each lane uses, and which tools a
//! unit needs provisioned. It carries no repository knowledge of its own; every
//! value comes from the scanned shape and the repo-owned generation config.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::{
    CacheBackend, GraphNode, LaneJob, Pins, UnitContract, DEFAULT_UNIT_TIMEOUT_MINUTES,
    MUTABLE_MOUNT_HOST_DIR,
};
use crate::{
    github_expression, hosted_mold_setup, lane_supports_unit, nested_unit_workflow_file,
    rendered_cache_values, sidebar_group_name, stack_group_job_id, unit_group, unit_group_job_id,
    unit_job_id, unit_needs, velnor_runner, velnor_runner_group, workflow_runtime_artifact_upload,
    workflow_runtime_download, workflow_runtime_setup, workflow_selection_artifact_download,
    workflow_selection_artifact_upload, yaml_scalar, CachePurpose, CacheSpec, ProjectConfig,
    RunnerMode, RustToolchain, Unit, UnitKind, GENERATED_HEADER, MR_BOXINGTON_CACHE_GENERATION,
    MR_BOXINGTON_VERSION, OPEN_TOFU_VERSION, VELNOR_POLICY_WORKFLOW_REV,
    VELNOR_WORKFLOW_SETUP_ACTION, VELNOR_WORKFLOW_SOURCE_REV,
};

/// Shell body of the post-checks report step.
///
/// Classifies the captured unit log into the three outcome classes the
/// performance contract is measured against — package-origin downloads,
/// compiler-cache outcomes (mbx counters, Cargo compile segments), and queue
/// time — then emits one machine-readable `VELNOR_CI_REPORT` line plus a
/// step-summary table. The step never fails the job.
const CHECKS_REPORT_SCRIPT: &str = r#"set -uo pipefail
log="$RUNNER_TEMP/velnor-unit-log.txt"
log_present=false
if [[ -s "$log" ]]; then log_present=true; fi
started="${VELNOR_CHECKS_STARTED_EPOCH:-}"
ended="${VELNOR_CHECKS_ENDED_EPOCH:-}"
wall=""
if [[ "$started" =~ ^[0-9]+$ && "$ended" =~ ^[0-9]+$ ]]; then
  wall=$((ended - started))
fi
queue=""
job_name=""
expected_job="$VELNOR_WORKFLOW_NAME / $VELNOR_LANE_NAME"
if command -v gh >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
  run_started="$(gh api "repos/$GH_REPO/actions/runs/$GITHUB_RUN_ID" --jq '.run_started_at // empty' 2>/dev/null || true)"
  jobs_json="$(gh api --paginate "repos/$GH_REPO/actions/runs/$GITHUB_RUN_ID/jobs?per_page=100" 2>/dev/null || true)"
  job_name="$(printf '%s' "$jobs_json" | jq -r --arg wf "$VELNOR_WORKFLOW_NAME" --arg lane "$VELNOR_LANE_NAME" '[.jobs[]? | select(.name == ($wf + " / " + $lane))][0].name // empty' 2>/dev/null | head -n1 || true)"
  if [[ -z "$job_name" ]]; then
    job_name="$(printf '%s' "$jobs_json" | jq -r --arg lane "$VELNOR_LANE_NAME" '[.jobs[]? | select(.name | endswith(" / " + $lane))][0].name // empty' 2>/dev/null | head -n1 || true)"
  fi
  if [[ -n "$job_name" && -n "$run_started" ]]; then
    job_started="$(printf '%s' "$jobs_json" | jq -r --arg name "$job_name" '.jobs[]? | select(.name == $name) | .started_at' 2>/dev/null | head -n1 || true)"
    if [[ -n "$job_started" ]]; then
      queue="$(jq -n --arg job "$job_started" --arg run "$run_started" '($job | fromdateiso8601) - ($run | fromdateiso8601)' 2>/dev/null || true)"
    fi
  fi
fi
if [[ -z "$job_name" ]]; then
  job_name="$expected_job"
fi
updating_index=0
updating_git=0
downloading_crates=0
compiling_count=0
downloaded_lines=""
mbx_lines=""
finished_lines=""
if [[ -s "$log" ]]; then
  updating_index="$(grep -c 'Updating crates.io index' "$log" || true)"
  updating_git="$(grep -cE 'Updating git (repository|submodule)' "$log" || true)"
  downloading_crates="$(grep -c 'Downloading crates' "$log" || true)"
  compiling_count="$(grep -c 'Compiling ' "$log" || true)"
  downloaded_lines="$(grep -E 'Downloaded [0-9]+ crate' "$log" || true)"
  mbx_lines="$(grep -E 'mbx(\[cache\])?:' "$log" | grep -E 'hits|misses' || true)"
  finished_lines="$(grep -E 'Finished .* in ' "$log" || true)"
fi
positive_int() {
  local value="$1"
  [[ "$value" =~ ^[0-9]+$ ]] || value=0
  printf '%s' "$value"
}
report_json="$(jq -nc \
  --arg job "$job_name" \
  --arg queue "$queue" \
  --arg wall "$wall" \
  --argjson log_present "$log_present" \
  --argjson updating_index "$(positive_int "$updating_index")" \
  --argjson updating_git "$(positive_int "$updating_git")" \
  --argjson downloading_crates "$(positive_int "$downloading_crates")" \
  --argjson compiling_count "$(positive_int "$compiling_count")" \
  --argjson downloaded_lines "$(printf '%s' "$downloaded_lines" | jq -Rsc 'split("\n") | map(select(length > 0))' || true)" \
  --argjson mbx "$(printf '%s' "$mbx_lines" | jq -Rsc 'split("\n") | map(select(length > 0))' || true)" \
  --argjson finished "$(printf '%s' "$finished_lines" | jq -Rsc 'split("\n") | map(select(length > 0))' || true)" \
  '{
    job: $job,
    log_present: $log_present,
    queue_seconds: ($queue | if test("^[0-9]+$") then tonumber else null end),
    checks_wall_seconds: ($wall | if test("^[0-9]+$") then tonumber else null end),
    origin_downloads: {
      updating_crates_io_index: $updating_index,
      updating_git: $updating_git,
      downloading_crates: $downloading_crates,
      downloaded_lines: $downloaded_lines
    },
    compiler: {
      mbx_outcomes: $mbx,
      finished_segments: $finished,
      compiling_lines: $compiling_count
    }
  }' 2>/dev/null || true)"
if [[ -z "$report_json" ]]; then
  report_json="VELNOR_CI_REPORT_FALLBACK job=$job_name"
fi
echo "VELNOR_CI_REPORT $report_json"
{
  echo ""
  echo '### Phase timings and cache outcomes'
  echo ""
  echo '```json'
  echo "$report_json"
  echo '```'
  echo ""
  echo "| metric | value |"
  echo "| --- | --- |"
  echo "| queue_seconds | ${queue:-unknown} |"
  echo "| checks_wall_seconds | ${wall:-unknown} |"
  echo "| updating_crates_io_index | $updating_index |"
  echo "| updating_git | $updating_git |"
  echo "| downloading_crates | $downloading_crates |"
  echo "| compiling_lines | $compiling_count |"
} >> "$GITHUB_STEP_SUMMARY" 2>/dev/null || true
exit 0
"#;

/// Emit the post-checks report step shared by both render paths.
///
/// `workflow_name` and `lane_name` compose the job name the Actions API
/// reports for this job (`<workflow name> / <job name>` for a `workflow_call`
/// callee), which the queue-time lookup matches on.
pub(crate) fn render_phase_report_step(output: &mut String, workflow_name: &str, lane_name: &str) {
    output.push_str("      - name: Report phase timings and cache outcomes\n");
    output.push_str("        if: always()\n");
    output.push_str("        env:\n");
    let _ = writeln!(output, "          VELNOR_WORKFLOW_NAME: {workflow_name}");
    let _ = writeln!(output, "          VELNOR_LANE_NAME: {lane_name}");
    output.push_str("          GH_TOKEN: ${{ github.token }}\n");
    output.push_str("          GH_REPO: ${{ github.repository }}\n");
    output.push_str("        run: |\n");
    for line in CHECKS_REPORT_SCRIPT.lines() {
        output.push_str("          ");
        output.push_str(line);
        output.push('\n');
    }
}

/// The Cargo subcommands whose inputs are resolved by the command itself:
/// advisory databases, publish registries, and similar tool-owned data are
/// fetched at run time regardless of any `Cargo.lock` pin.
fn command_resolves_its_own_inputs(command: &str) -> bool {
    // Environment assignments (`FOO=bar cargo audit`) do not change the tool;
    // `skip_while` stops at the first non-assignment token and keeps it. The
    // tool is then located wherever it sits, so a quoted env value that split
    // into extra tokens cannot push it out of head position.
    let mut tokens = command
        .split_whitespace()
        .skip_while(|token| token.contains('=') && !token.starts_with('-'));
    while let Some(tool) = tokens.next() {
        if !matches!(tool, "cargo" | "mbx") {
            continue;
        }
        // Tool modifiers such as `+nightly` do not change the subcommand.
        if let Some(subcommand) = tokens.by_ref().find(|token| !token.starts_with('+')) {
            return matches!(subcommand, "deny" | "audit" | "publish");
        }
    }
    false
}

/// Whether the unit's Cargo verification runs with the network restricted to
/// the source-preparation step. Restriction requires a root `Cargo.lock` so
/// `cargo fetch --locked` is well-defined, and exempts units whose commands
/// resolve their own inputs (deny, audit, publish).
fn cargo_network_is_restricted(unit: &Unit) -> bool {
    if !unit.pinned_lockfile {
        return false;
    }
    let mut commands = unit
        .pr_commands
        .iter()
        .chain(&unit.full_commands)
        .chain(unit.github_pr_commands.iter().flatten())
        .chain(unit.github_full_commands.iter().flatten())
        .chain(unit.velnor_pr_commands.iter().flatten())
        .chain(unit.velnor_full_commands.iter().flatten());
    !commands.any(|command| command_resolves_its_own_inputs(command))
}

/// Env for the steps that run Cargo and repository commands, on both lanes:
/// the `CARGO_NET_OFFLINE` restriction for lockfile-pinned units, and the
/// suppression of every mise auto-install path. Verification must consume only
/// what the provision steps put in place; a task runner that silently installs
/// a missing tool would widen each job's dependency surface and re-introduce
/// the whole-configured-toolset materialisation a minimal provision list
/// exists to prevent.
pub(crate) fn checks_env(unit: &Unit) -> String {
    let mut env = String::new();
    if cargo_network_is_restricted(unit) {
        env.push_str("\n          CARGO_NET_OFFLINE: \"true\"");
    }
    env.push_str("\n          MISE_AUTO_INSTALL: \"false\"");
    env.push_str("\n          MISE_EXEC_AUTO_INSTALL: \"false\"");
    env.push_str("\n          MISE_NOT_FOUND_AUTO_INSTALL: \"false\"");
    env
}

/// Every command the unit runs, on either lane: the scan-derived base
/// commands plus every lane-specific override a repo-owned config declared.
/// A unit may pin commands per lane only — its base vectors then stay empty —
/// so a predicate that skips the overrides would conclude the unit runs
/// nothing at all.
fn unit_commands(unit: &Unit) -> impl Iterator<Item = &String> {
    unit.pr_commands
        .iter()
        .chain(&unit.full_commands)
        .chain(unit.github_pr_commands.iter().flatten())
        .chain(unit.github_full_commands.iter().flatten())
        .chain(unit.velnor_pr_commands.iter().flatten())
        .chain(unit.velnor_full_commands.iter().flatten())
}

/// Whether any of the unit's commands drive the test runner through Cargo or
/// Mr. Boxington. One predicate feeds both the `ToolRequirement` selection and
/// the mise install list, so the two can never disagree about what a unit
/// needs.
fn needs_nextest(unit: &Unit) -> bool {
    unit_commands(unit)
        .any(|command| command.contains("cargo nextest") || command.contains("mbx nextest"))
}

/// The restore/provision/save steps every hosted Rust job needs before it may
/// run a Cargo command: restore the cached `~/.rustup` keyed by the
/// repository's pin, install exactly that pin, and — when `save_gate` carries
/// the step's `if:` body — save the result for the next run. Every rendering
/// path (unit lanes, the release publisher, the rolling preview) funnels
/// through here, so no Rust job can skip the toolchain contract.
pub(crate) fn render_pinned_toolchain_steps(
    output: &mut String,
    cache_restore: &str,
    cache_save: &str,
    toolchain: &RustToolchain,
    save_gate: Option<&str>,
) {
    let (paths, key_files) = rendered_cache_values(&CacheSpec {
        key_files: vec![
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
        ],
        paths: vec!["~/.rustup".to_owned()],
        purpose: CachePurpose::Toolchains,
        mbx_output_cache_justification: None,
        mutable_mount_seed: false,
    });
    let key = format!(
        "velnor-rustup-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-${{{{ hashFiles({key_files}) }}}}"
    );
    let _ = writeln!(
        output,
        "      - name: Restore Rust toolchain\n        id: rustup-toolchain\n        uses: {cache_restore}\n        with:\n          path: |\n{paths}\n          key: {key}"
    );
    // The channel is not passed explicitly: the checkout put the
    // repository's toolchain file at the workspace root, and a file-driven
    // install also applies the components and targets the file declares,
    // which an explicitly named channel would not.
    let install = match &toolchain.profile {
        Some(profile) => format!("rustup toolchain install --profile {profile}"),
        None => "rustup toolchain install".to_owned(),
    };
    let _ = writeln!(
        output,
        "      - name: Provision Rust toolchain\n        shell: bash\n        run: |\n          set -euo pipefail\n          {install}"
    );
    if !toolchain.targets.is_empty() {
        let targets = toolchain
            .targets
            .iter()
            .map(|target| crate::shell_quote(target))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(output, "          rustup target add {targets}");
    }
    if let Some(save_gate) = save_gate {
        let _ = writeln!(
            output,
            "      - name: Save Rust toolchain\n        if: {save_gate}\n        uses: {cache_save}\n        with:\n          path: |\n{paths}\n          key: {key}"
        );
    }
}

/// The mise tool ids a unit's own commands require. The language toolchain is
/// deliberately absent — rustup provisions it from the repository's pin — and
/// so is every tool a policy step installs through its own action. Each
/// additional id widens the supply chain of every job that runs it, so a unit
/// that invokes none of these gets nothing.
pub(crate) fn mise_tool_ids(unit: &Unit) -> Vec<&'static str> {
    let mut tools = Vec::new();
    if needs_nextest(unit) {
        tools.push("aqua:nextest-rs/nextest/cargo-nextest");
    }
    tools
}

/// Whether the unit's commands hand work to the mise task runner itself, which
/// needs the mise binary on `PATH` even when no tool is installed through it.
pub(crate) fn commands_invoke_mise(unit: &Unit) -> bool {
    unit_commands(unit).any(|command| {
        command
            .split_whitespace()
            .any(|token| token == "mise" || token.starts_with("mise:"))
    })
}

/// Fetch every declared Cargo source up front, after the cache restore and
/// before verification. Verification then runs without package-origin
/// downloads: registry archives and Git dependencies arrive here, once, where
/// the fetch is visible and measurable on its own.
pub(crate) fn render_cargo_source_preparation(output: &mut String, unit: &Unit) {
    if !cargo_network_is_restricted(unit) {
        return;
    }
    let change_dir = crate::shell_change_dir(&unit.root);
    let _ = writeln!(
        output,
        "      - name: Prepare Cargo sources\n        env:{}
        run: |
          set -euo pipefail
          {change_dir}cargo fetch --locked",
        checks_env(unit)
    );
}

/// The files a Docker mutable mount seed exchange carries: what the image's
/// export target writes out after a trusted full build, and what the next
/// build's seed context holds. The image consumes them by these names.
const MUTABLE_MOUNT_SEED_FILES: &[&str] =
    &["cargo-registry.tar", "cargo-git.tar", "mbx-closure.tar"];

/// Restore the Docker mutable mount seed on the hosted lane and stage the
/// seed context directory the image build overrides the empty
/// `velnor-cache-seed` stage with. The image copies out of the context into
/// its cache mounts only when a mount is empty, so a retained builder keeps
/// its warm state and a fresh builder starts from the restored one.
fn render_mutable_mount_seed_restore(output: &mut String, ir: &WorkflowIr, unit: &Unit) {
    let Some(cache) = unit.cache.as_ref() else {
        return;
    };
    let (paths, key_files) = rendered_cache_values(cache);
    let key = format!(
        "velnor-docker-seed-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-${{{{ hashFiles({key_files}) }}}}"
    );
    let _ = writeln!(
        output,
        "      - name: Restore Docker build seed\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}\n          restore-keys: |\n            velnor-docker-seed-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-",
        ir.pins.cache_restore
    );
    let _ = writeln!(
        output,
        "      - name: Prepare Docker build seed context\n        env:{}
        run: |
          set -euo pipefail
          mkdir -p {MUTABLE_MOUNT_HOST_DIR}/seed
          if compgen -G \"{MUTABLE_MOUNT_HOST_DIR}/seed/*\" > /dev/null; then
            echo \"Docker build seed restored:\"
            du -sh {MUTABLE_MOUNT_HOST_DIR}/seed/*
          else
            echo \"Docker build seed empty: the build starts from cold cache mounts\"
          fi",
        checks_env(unit)
    );
}

/// Collect the mutable-cache state a trusted full build exported out of the
/// image's cache mounts, and save it for the next builder. Collection runs
/// only on trusted events (untrusted pull-request code never writes seed
/// state) and only when the declared full commands actually produced the
/// export; a partial export fails here instead of seeding the next build with
/// a lie.
fn render_mutable_mount_seed_collection(
    output: &mut String,
    ir: &WorkflowIr,
    unit: &Unit,
    cache_save: bool,
) {
    let trusted_cache = format!(
        "(github.event_name == 'push' && github.ref == 'refs/heads/{}') || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}')",
        ir.default_branch, ir.default_branch
    );
    let required_files = MUTABLE_MOUNT_SEED_FILES
        .iter()
        .map(|file| {
            format!(
                "          test -s \"{MUTABLE_MOUNT_HOST_DIR}/export/{file}\" \
                 || {{ echo \"::error::Docker cache export is missing {file}\" >&2; exit 1; }}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = writeln!(
        output,
        "      - name: Collect Docker mutable-cache export\n        if: {trusted_cache}\n        env:{}
        run: |
          set -euo pipefail
          if [ ! -f \"{MUTABLE_MOUNT_HOST_DIR}/export/{}\" ]; then
            echo \"no Docker cache export: this run did not execute the full build commands\"
            exit 0
          fi
{required_files}
          rm -rf \"{MUTABLE_MOUNT_HOST_DIR}/seed.next\"
          mkdir -p \"{MUTABLE_MOUNT_HOST_DIR}/seed.next\"
          mv \"{MUTABLE_MOUNT_HOST_DIR}\"/export/* \"{MUTABLE_MOUNT_HOST_DIR}/seed.next/\"
          rm -rf \"{MUTABLE_MOUNT_HOST_DIR}/seed\"
          mv \"{MUTABLE_MOUNT_HOST_DIR}/seed.next\" \"{MUTABLE_MOUNT_HOST_DIR}/seed\"
          rm -rf \"{MUTABLE_MOUNT_HOST_DIR}/export\"
          echo \"Docker build seed updated:\"
          du -sh \"{MUTABLE_MOUNT_HOST_DIR}\"/seed/*",
        checks_env(unit),
        MUTABLE_MOUNT_SEED_FILES[MUTABLE_MOUNT_SEED_FILES.len() - 1]
    );
    if cache_save && let Some(cache) = unit.cache.as_ref() {
        let (paths, key_files) = rendered_cache_values(cache);
        let key = format!(
            "velnor-docker-seed-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-${{{{ hashFiles({key_files}) }}}}"
        );
        let _ = writeln!(
            output,
            "      - name: Save Docker build seed\n        if: ({trusted_cache}) && steps.cache.outputs.cache-hit != 'true'\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}",
            ir.pins.cache_save
        );
    }
}

/// Record why a raw output cache rides alongside the Mr. Boxington object
/// transport. Without a justification the generator refuses the combination.
pub(crate) fn render_retained_output_cache_note(
    output: &mut String,
    ir: &WorkflowIr,
    unit: &Unit,
    cache: &crate::CacheSpec,
) {
    if ir.uses_mr_boxington(unit) && cache.justified_output_alongside_mr_boxington() {
        let justification = cache
            .mbx_output_cache_justification
            .as_deref()
            .unwrap_or_default();
        let _ = writeln!(
            output,
            "      # output cache retained alongside mbx: {justification}"
        );
    }
}

/// The resolved render environment: lanes, runners, and unit tool needs.
///
/// Rendering is kept separate from scanning so output policy is inspectable
/// and unit-tested.
#[derive(Debug)]
pub(crate) struct WorkflowIr {
    pub(crate) default_branch: String,
    pub(crate) github_runner: String,
    pub(crate) velnor_labels: Vec<String>,
    pub(crate) ci_required: bool,
    pub(crate) velnor_runner_group: Option<String>,
    pub(crate) runners: RunnerMode,
    pub(crate) tools: BTreeSet<ToolRequirement>,
    /// The repository drives its Rust units through mise. Naming matters: mise
    /// never provides the Rust toolchain (rustup owns that), it only
    /// contributes the task runner and the tools units declare.
    pub(crate) mise_present: bool,
    pub(crate) mr_boxington: bool,
    pub(crate) units: Vec<Unit>,
    pub(crate) pins: Pins,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ToolRequirement {
    OpenTofu,
    Homebrew,
    Bun,
    Node,
    Nextest,
    CargoDeny,
    CargoAudit,
    Gradle,
    Sccache,
    MrBoxington,
    Mold,
    DockerBuildx,
    Mise,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowKind {
    PullRequest,
    Main,
    Nightly,
}

#[allow(dead_code)]
impl WorkflowIr {
    pub(crate) fn from_config(config: &ProjectConfig) -> Self {
        let mut tools = BTreeSet::new();
        let mr_boxington = config.units.iter().any(|unit| unit.kind == UnitKind::Rust);
        if config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::OpenTofu)
        {
            tools.insert(ToolRequirement::OpenTofu);
        }
        if config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Homebrew)
        {
            tools.insert(ToolRequirement::Homebrew);
        }
        if config.units.iter().any(|unit| unit.kind == UnitKind::Bun) {
            tools.insert(ToolRequirement::Bun);
        }
        if config.units.iter().any(|unit| unit.kind == UnitKind::Node) {
            tools.insert(ToolRequirement::Node);
        }
        if config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Gradle)
        {
            tools.insert(ToolRequirement::Gradle);
        }
        if config.units.iter().any(|unit| unit.kind == UnitKind::Rust) {
            if mr_boxington {
                tools.insert(ToolRequirement::MrBoxington);
            } else {
                tools.insert(ToolRequirement::Sccache);
            }
            tools.insert(ToolRequirement::Mold);
        }
        // The detection fact only says mise is configured; the mise surface
        // matters here only where Rust units exist to run through it.
        let mise_present = config
            .analysis
            .detected
            .iter()
            .any(|item| item == "mise-present")
            && config.units.iter().any(|unit| unit.kind == UnitKind::Rust);
        if mise_present {
            tools.insert(ToolRequirement::Mise);
        }
        if config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Docker)
        {
            tools.insert(ToolRequirement::DockerBuildx);
        }
        Self {
            default_branch: config.default_branch.clone(),
            github_runner: config.github_runner.clone(),
            velnor_labels: config.velnor_labels.clone(),
            ci_required: config.ci_required,
            velnor_runner_group: velnor_runner_group(config).map(str::to_owned),
            runners: config.runners,
            tools,
            mise_present,
            mr_boxington,
            units: config.units.clone(),
            pins: Pins::resolved(),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the legacy renderer remains a single auditable workflow pass"
    )]
    pub(crate) fn render(&self, kind: WorkflowKind) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, cancel_in_progress) = match kind {
            WorkflowKind::PullRequest => (
                "CI",
                "CI / PR",
                "on:\n  pull_request:".to_owned(),
                "true",
            ),
            WorkflowKind::Main => (
                "CI",
                "CI / main",
                format!(
                    "on:\n  push:\n    branches: [{}]\n  workflow_dispatch:",
                    yaml_scalar(&self.default_branch)
                ),
                "true",
            ),
            WorkflowKind::Nightly => (
                "Nightly",
                "Nightly",
                "on:\n  schedule:\n    - cron: '17 3 * * *'\n  workflow_dispatch:\n    inputs:\n      simulate_failure:\n        description: Force the red-to-signal test path\n        required: false\n        default: false\n        type: boolean"
                    .to_owned(),
                "true",
            ),
        };
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\nconcurrency:\n  group: ci-${{{{ github.workflow }}}}-${{{{ github.event.pull_request.number || github.ref }}}}\n  cancel-in-progress: {cancel_in_progress}\n\npermissions:\n  actions: read\n  contents: read\n\n"
        );
        if self.tools.contains(&ToolRequirement::Sccache)
            || self.tools.contains(&ToolRequirement::OpenTofu)
            || self.mise_present
        {
            output.push_str("env:\n");
            if self.tools.contains(&ToolRequirement::Sccache) {
                output.push_str(
                    "  CARGO_INCREMENTAL: \"0\"\n  RUSTC_WRAPPER: sccache\n  SCCACHE_GHA_ENABLED: \"true\"\n",
                );
            }
            if self.mise_present {
                output.push_str("  RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"\n");
            }
            if self.tools.contains(&ToolRequirement::OpenTofu) {
                output.push_str("  TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache\n");
            }
            output.push('\n');
        }
        output.push_str("jobs:\n");
        // Runner mode is global; only trusted-event jobs receive the extra
        // default-branch gate needed for Velnor execution.
        let trusted_event = kind != WorkflowKind::PullRequest;
        let runners = self.runners;
        // Planning and policy execute on a GitHub-hosted runner even when the
        // selected verification lane is self-hosted. They execute checked-in
        // shell files and must never expose untrusted PR code to that runner.
        self.render_plan(&mut output, RunnerMode::Github, false);
        if kind != WorkflowKind::PullRequest {
            Self::render_policy(&mut output, RunnerMode::Github, false);
        }
        self.render_hierarchy_groups(&mut output, kind != WorkflowKind::PullRequest);
        let cache_save = kind != WorkflowKind::PullRequest;
        match runners {
            RunnerMode::Github => self.render_verify_github(
                &mut output,
                None,
                cache_save,
                kind != WorkflowKind::PullRequest,
            ),
            RunnerMode::Velnor => self.render_verify_velnor(
                &mut output,
                cache_save,
                kind != WorkflowKind::PullRequest,
            ),
            RunnerMode::Both => {
                self.render_verify_github(
                    &mut output,
                    None,
                    cache_save,
                    kind != WorkflowKind::PullRequest,
                );
                self.render_verify_velnor(&mut output, false, kind != WorkflowKind::PullRequest);
            }
        }
        // Auxiliary schedules must not create or satisfy the branch-protection
        // check. Only the PR and main workflows own the stable `ci-required`
        // check that repository rulesets gate on.
        if kind != WorkflowKind::Nightly && self.ci_required {
            self.render_required(
                &mut output,
                runners,
                kind == WorkflowKind::PullRequest,
                trusted_event,
                kind != WorkflowKind::PullRequest,
            );
        }
        while output.ends_with("\n\n") {
            output.pop();
        }
        output = output
            .lines()
            .map(|line| {
                line.split_once(" · Runner:")
                    .filter(|_| line.trim_start().starts_with("name:"))
                    .map_or_else(
                        || line.to_owned(),
                        |(name, _)| {
                            if name.contains("name: \"") {
                                format!("{name}\"")
                            } else {
                                name.to_owned()
                            }
                        },
                    )
            })
            .collect::<Vec<_>>()
            .join("\n");
        output
    }

    /// Render one reusable-workflow call per sidebar group. GitHub flattens
    /// nested reusable workflows, so an intermediate stack workflow would hide
    /// unit names and place every terminal runner job directly under the stack.
    /// Render one aggregate workflow from the CI graph nodes the declared
    /// primitives contributed.
    ///
    /// The aggregate owns only composition: the plan job, the advisory policy
    /// job, one reusable-workflow caller per contributed unit node, and the
    /// required check that validates every one of them. Rendering is a pure
    /// function of the nodes, so a unit the declared surface does not cover
    /// simply has no caller and no required-check branch.
    pub(crate) fn render_nested(&self, kind: WorkflowKind, nodes: &[GraphNode]) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, cancel_in_progress) = match kind {
            WorkflowKind::PullRequest => (
                "CI",
                "CI / PR",
                "on:\n  pull_request:".to_owned(),
                "true",
            ),
            WorkflowKind::Main => (
                "CI",
                "CI / main",
                format!(
                    "on:\n  push:\n    branches: [{}]\n  workflow_dispatch:",
                    yaml_scalar(&self.default_branch)
                ),
                "true",
            ),
            WorkflowKind::Nightly => (
                "Nightly",
                "Nightly",
                "on:\n  schedule:\n    - cron: '17 3 * * *'\n  workflow_dispatch:\n    inputs:\n      simulate_failure:\n        description: Force the red-to-signal test path\n        required: false\n        default: false\n        type: boolean"
                    .to_owned(),
                "true",
            ),
        };
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\nconcurrency:\n  group: ci-${{{{ github.workflow }}}}-${{{{ github.event.pull_request.number || github.ref }}}}\n  cancel-in-progress: {cancel_in_progress}\n\npermissions:\n  actions: read\n  contents: read\n\njobs:"
        );
        // The plan job is a contributed node: the aggregate composes the graph
        // the declared primitives built and renders no job of its own.
        let plan = nodes
            .iter()
            .find_map(GraphNode::plan_job)
            .unwrap_or_default();
        output.push_str(plan);
        if kind != WorkflowKind::PullRequest {
            Self::render_policy(&mut output, RunnerMode::Github, false);
        }
        Self::render_node_callers(nodes, &mut output, kind != WorkflowKind::PullRequest);
        if kind == WorkflowKind::Nightly {
            self.render_nodes_required(nodes, &mut output, true, "nightly-required", true);
            self.render_nightly_alert(&mut output);
        } else if self.ci_required {
            self.render_nodes_required(
                nodes,
                &mut output,
                kind != WorkflowKind::PullRequest,
                "ci-required",
                false,
            );
        }
        while output.ends_with("\n\n") {
            output.pop();
        }
        output
    }

    pub(crate) fn render_nested_unit_callers(&self, output: &mut String, include_policy: bool) {
        for unit in &self.units {
            let mut needs = vec!["plan".to_owned()];
            if include_policy {
                needs.push("policy".to_owned());
            }
            let id = unit_group_job_id(unit);
            let name = sidebar_group_name(unit);
            let mut conditions = vec![
                "always()".to_owned(),
                "needs.plan.result == 'success'".to_owned(),
            ];
            if include_policy {
                conditions.push("needs.policy.result == 'success'".to_owned());
            }
            conditions.push(format!(
                "contains(format(',{{0}},', needs.plan.outputs.units), ',{},')",
                unit.id
            ));
            let _ = writeln!(
                output,
                "  {id}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{}\n    with:\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      selection-artifact: velnor-ci-selection",
                yaml_scalar(&name),
                conditions.join(" && "),
                needs.join(", "),
                nested_unit_workflow_file(unit),
            );
        }
    }

    /// One reusable-workflow caller per contributed unit node, in the order the
    /// nodes were contributed (canonical unit order).
    pub(crate) fn render_node_callers(
        nodes: &[GraphNode],
        output: &mut String,
        include_policy: bool,
    ) {
        for (unit_id, job_id, name, file) in nodes.iter().filter_map(GraphNode::as_unit) {
            let mut needs = vec!["plan".to_owned()];
            if include_policy {
                needs.push("policy".to_owned());
            }
            let id = job_id;
            let mut conditions = vec![
                "always()".to_owned(),
                "needs.plan.result == 'success'".to_owned(),
            ];
            if include_policy {
                conditions.push("needs.policy.result == 'success'".to_owned());
            }
            conditions.push(format!(
                "contains(format(',{{0}},', needs.plan.outputs.units), ',{unit_id},')"
            ));
            let _ = writeln!(
                output,
                "  {id}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{}\n    with:\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      selection-artifact: velnor-ci-selection",
                yaml_scalar(name),
                conditions.join(" && "),
                needs.join(", "),
                file,
            );
        }
    }

    /// The aggregate required check over every contributed unit node.
    pub(crate) fn render_nodes_required(
        &self,
        nodes: &[GraphNode],
        output: &mut String,
        include_policy: bool,
        check_name: &str,
        simulate_failure: bool,
    ) {
        let units = nodes
            .iter()
            .filter_map(|node| node.as_unit().map(|(_, job_id, _, _)| job_id.to_owned()))
            .collect::<Vec<_>>();
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        needs.extend(units);
        let _ = writeln!(
            output,
            "  {check_name}:\n    name: {check_name}\n    if: ${{{{ always() }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated stack results\n        shell: bash\n        run: |",
            needs.join(", "),
            yaml_scalar(&self.github_runner)
        );
        if simulate_failure {
            let simulate = github_expression("inputs.simulate_failure");
            let _ = writeln!(
                output,
                "          if [[ \"{simulate}\" == true ]]; then\n            echo \"nightly red-to-signal simulation requested\" >&2\n            exit 1\n          fi"
            );
        }
        for job in needs
            .iter()
            .filter(|job| matches!(job.as_str(), "plan" | "policy"))
        {
            let result = github_expression(&format!("needs['{job}'].result"));
            let _ = writeln!(
                output,
                "          result=\"{result}\"\n          if [[ \"$result\" != success ]]; then\n            echo \"required CI prerequisite {job} did not pass: $result\" >&2\n            exit 1\n          fi"
            );
        }
        let selected = github_expression("needs.plan.outputs.units");
        let _ = writeln!(output, "          selected=\",{selected},\"");
        for (unit_id, job_id, _, _) in nodes.iter().filter_map(GraphNode::as_unit) {
            let job = job_id;
            let result = github_expression(&format!("needs['{job}'].result"));
            let _ = writeln!(
                output,
            "          if [[ \"$selected\" == *\",{unit_id},\"* ]]; then\n            result=\"{result}\"\n            case \"$result\" in\n              success) ;;\n              *) echo \"selected CI unit {unit_id} did not pass: $result\" >&2; exit 1 ;;\n            esac\n          else\n            case \"$result\" in\n              success|skipped) ;;\n              *) echo \"unselected CI unit {unit_id} failed unexpectedly: $result\" >&2; exit 1 ;;\n            esac\n          fi",
            );
        }
    }

    pub(crate) fn render_nightly_alert(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "  nightly-alert:\n    name: Nightly red-to-signal\n    if: ${{{{ always() }}}}\n    needs: [nightly-required]\n    runs-on: {}\n    permissions:\n      contents: read\n      issues: write\n    steps:\n      - name: Open or update nightly failure signal\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          NIGHTLY_RESULT: ${{{{ needs.nightly-required.result }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [[ \"$NIGHTLY_RESULT\" == success ]]; then\n            exit 0\n          fi\n          echo \"::error::nightly-required failed: $NIGHTLY_RESULT\"\n          existing=\"$(gh api \"repos/$GITHUB_REPOSITORY/issues?state=open\" --jq '.[] | select(.title == \"Nightly CI red\") | .number' | sed -n '1p')\"\n          body=\"nightly-required result: $NIGHTLY_RESULT\nRun: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID\"\n          if [[ -n \"$existing\" ]]; then\n            gh api --method PATCH \"repos/$GITHUB_REPOSITORY/issues/$existing\" -f body=\"$body\" >/dev/null\n          else\n            gh api --method POST \"repos/$GITHUB_REPOSITORY/issues\" -f title='Nightly CI red' -f body=\"$body\" >/dev/null\n          fi",
            yaml_scalar(&self.github_runner)
        );
        let bad_body = r#"          body="nightly-required result: $NIGHTLY_RESULT
Run: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID""#;
        let good_body = r#"          body="$(printf '%s\n%s\n' \
            "nightly-required result: $NIGHTLY_RESULT" \
            "Run: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID")""#;
        *output = output.replace(bad_body, good_body);
    }

    /// The legacy default unit contract: every supported lane, the default
    /// timeout, the detected cache backend, and the unit's own declared cache
    /// contract. A unit that declares a Docker mutable mount seed gets the
    /// seed lifecycle rendered for it exactly as a declared pipeline would —
    /// the default contract defaults the lane surface, never the cache
    /// transport a unit declared.
    pub(crate) fn default_unit_contract(&self, unit: &Unit, cache_save: bool) -> UnitContract {
        UnitContract {
            lanes: self.default_lane_jobs(cache_save),
            timeout_minutes: DEFAULT_UNIT_TIMEOUT_MINUTES,
            cache: CacheBackend::Detected,
            cache_save,
            mutable_mount_seed: unit
                .cache
                .as_ref()
                .is_some_and(|cache| cache.mutable_mount_seed),
        }
    }

    /// The lane jobs a nested unit workflow emits: the hosted lane saves the
    /// cache and is untrusted, the self-hosted lane is the trusted event lane.
    pub(crate) fn default_lane_jobs(&self, cache_save: bool) -> Vec<LaneJob> {
        let mut lanes = Vec::new();
        if matches!(self.runners, RunnerMode::Github | RunnerMode::Both) {
            lanes.push(LaneJob {
                lane: RunnerMode::Github,
                cache_save,
                trusted: false,
            });
        }
        if matches!(self.runners, RunnerMode::Velnor | RunnerMode::Both) {
            lanes.push(LaneJob {
                lane: RunnerMode::Velnor,
                cache_save: false,
                trusted: true,
            });
        }
        lanes
    }

    /// Render one unit's reusable workflow surface.
    ///
    /// The unit contract carries everything a declaring primitive may tune: the
    /// lane jobs to emit, the job timeout, and the cache backend. With the
    /// default contract the output is the surface the generator has always
    /// emitted for a unit.
    pub(crate) fn render_unit_surface(&self, unit: &Unit, contract: &UnitContract) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let _ = writeln!(
            output,
            "name: {}\non:\n  workflow_call:\n    inputs:\n      scope:\n        required: true\n        type: string\n      selection-artifact:\n        required: true\n        type: string",
            yaml_scalar(&sidebar_group_name(unit))
        );
        self.render_workflow_env(&mut output, unit);
        output.push_str("\njobs:\n");
        for job in &contract.lanes {
            if lane_supports_unit(job.lane, unit) {
                self.render_lane_job(&mut output, *job, unit, contract);
            }
        }
        output
    }

    /// Legacy entry point: the nested unit workflow for one unit under the
    /// default contract.
    pub(crate) fn render_nested_unit(&self, unit: &Unit, kind: WorkflowKind) -> String {
        self.render_unit_surface(
            unit,
            &self.default_unit_contract(unit, kind != WorkflowKind::PullRequest),
        )
    }

    pub(crate) fn render_workflow_env(&self, output: &mut String, unit: &Unit) {
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        if tools.contains(&ToolRequirement::Sccache)
            || tools.contains(&ToolRequirement::OpenTofu)
            || self.mise_present
        {
            output.push_str("\nenv:\n");
            if tools.contains(&ToolRequirement::Sccache) {
                output.push_str(
                    "  CARGO_INCREMENTAL: \"0\"\n  RUSTC_WRAPPER: sccache\n  SCCACHE_GHA_ENABLED: \"true\"\n",
                );
            }
            if self.mise_present {
                output.push_str("  RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"\n");
            }
            if tools.contains(&ToolRequirement::OpenTofu) {
                output.push_str("  TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache\n");
            }
        }
    }

    /// Render one lane job of a nested unit workflow from the unit contract.
    pub(crate) fn render_lane_job(
        &self,
        output: &mut String,
        job: LaneJob,
        unit: &Unit,
        contract: &UnitContract,
    ) {
        let lane = job.lane;
        let cache_save = job.cache_save && contract.cache_save;
        let trusted = job.trusted;
        let id = lane.as_str();
        let _ = writeln!(output, "  {id}:\n    name: {}", lane.display_name());
        if trusted {
            let _ = writeln!(
                output,
                "    if: ${{{{ github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}}}",
                self.default_branch
            );
        }
        let _ = writeln!(
            output,
            "    runs-on: {}\n    timeout-minutes: {}\n    steps:",
            self.runner_for_unit(lane, unit),
            contract.timeout_minutes
        );
        let _ = writeln!(
            output,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false",
            self.pins.checkout
        );
        Self::render_workflow_runtime_download(output, lane);
        output.push_str(&workflow_selection_artifact_download(Some(
            "${{ inputs.selection-artifact }}",
        )));
        self.render_tool_provisioning(output, lane, unit, cache_save);
        let seed = contract.mutable_mount_seed;
        if seed && lane == RunnerMode::Github {
            render_mutable_mount_seed_restore(output, self, unit);
        } else if contract.cache.enables_actions_cache(self, unit)
            && let Some(cache) = &unit.cache
        {
            render_retained_output_cache_note(output, self, unit, cache);
            let (paths, key) = rendered_cache_values(cache);
            let cache_key = format!(
                "ci-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                unit.id
            );
            let _ = writeln!(
                output,
                "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            ci-${{{{ runner.os }}}}-{}-",
                yaml_scalar(&unit.label),
                self.pins.cache_restore,
                unit.id
            );
        }
        render_cargo_source_preparation(output, unit);
        let _ = writeln!(
            output,
            "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ github.event.pull_request.base.sha || github.event.before }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}\n        run: |\n          set -o pipefail\n          echo \"VELNOR_CHECKS_STARTED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {} 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n          echo \"VELNOR_CHECKS_ENDED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          exit $rc",
            yaml_scalar(&unit.label),
            yaml_scalar(&unit.id),
            checks_env(unit),
            yaml_scalar(&unit.id)
        );
        render_phase_report_step(
            output,
            &yaml_scalar(&sidebar_group_name(unit)),
            yaml_scalar(lane.display_name()).as_str(),
        );
        if seed && lane == RunnerMode::Github {
            render_mutable_mount_seed_collection(output, self, unit, cache_save);
        } else if cache_save
            && lane == RunnerMode::Github
            && contract.cache.enables_actions_cache(self, unit)
            && let Some(cache) = unit.cache.as_ref()
        {
            let trusted_cache = format!(
                "(github.event_name == 'push' && github.ref == 'refs/heads/{}') || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}')",
                self.default_branch, self.default_branch
            );
            let (paths, key) = rendered_cache_values(cache);
            let cache_key = format!(
                "ci-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                unit.id
            );
            let _ = writeln!(
                output,
                "      - name: Save {} cache\n        if: ({trusted_cache}) && steps.cache.outputs.cache-hit != 'true'\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}",
                yaml_scalar(&unit.label),
                self.pins.cache_save
            );
        }
        output.push('\n');
    }

    pub(crate) fn runner_for(&self, lane: RunnerMode) -> String {
        match lane {
            RunnerMode::Github | RunnerMode::Both => yaml_scalar(&self.github_runner),
            RunnerMode::Velnor => {
                velnor_runner(&self.velnor_labels, self.velnor_runner_group.as_deref())
            }
        }
    }

    pub(crate) fn render_workflow_runtime_setup(output: &mut String, lane: RunnerMode) {
        output.push_str(&workflow_runtime_setup(lane));
    }

    pub(crate) fn render_workflow_runtime_download(output: &mut String, lane: RunnerMode) {
        output.push_str(&workflow_runtime_download(lane));
    }

    pub(crate) fn render_plan(&self, output: &mut String, runners: RunnerMode, trusted: bool) {
        let gate = self.trusted_runner_gate(runners, trusted);
        let _ = writeln!(
            output,
            "  plan:\n    name: Planning\n{gate}    runs-on: {}\n    outputs:\n      scope: ${{{{ steps.plan.outputs.scope }}}}\n      units: ${{{{ steps.plan.outputs.units }}}}\n      full_units: ${{{{ steps.plan.outputs.full_units }}}}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Set up Velnor workflow runtime\n        if: ${{{{ runner.environment == 'github-hosted' }}}}\n        uses: {}@{}\n        with:\n          rev: {}\n      - name: Select affected units\n        id: plan\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ github.event.pull_request.base.sha || github.event.before }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: ${{{{ runner.temp }}}}/velnor-ci-selection\n        run: velnor-workflow plan --config .github/ci/project.toml\n",
            self.runner_for(runners),
            self.pins.checkout,
            VELNOR_WORKFLOW_SETUP_ACTION,
            VELNOR_WORKFLOW_SOURCE_REV,
            VELNOR_WORKFLOW_SOURCE_REV,
        );
        output.push_str(&workflow_runtime_artifact_upload());
        output.push_str(&workflow_selection_artifact_upload());
    }

    pub(crate) fn render_policy(output: &mut String, runners: RunnerMode, trusted: bool) {
        let _ = (runners, trusted);
        output.push_str(&crate::inline_policy_job(
            "Advisory policy",
            VELNOR_POLICY_WORKFLOW_REV,
        ));
    }

    pub(crate) fn trusted_runner_gate(&self, runners: RunnerMode, trusted: bool) -> String {
        if runners == RunnerMode::Velnor && trusted {
            format!(
                "    if: ${{{{ github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}}}\n",
                self.default_branch
            )
        } else {
            String::new()
        }
    }

    pub(crate) fn render_verify_github(
        &self,
        output: &mut String,
        condition: Option<&str>,
        cache_save: bool,
        include_policy: bool,
    ) {
        self.render_verify_lane(
            output,
            RunnerMode::Github,
            condition,
            cache_save,
            include_policy,
        );
    }

    pub(crate) fn render_verify_velnor(
        &self,
        output: &mut String,
        cache_save: bool,
        include_policy: bool,
    ) {
        // Self-hosted runners never receive untrusted pull-request code.
        let condition = Some(format!(
            "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
            self.default_branch
        ));
        self.render_verify_lane(
            output,
            RunnerMode::Velnor,
            condition.as_deref(),
            cache_save,
            include_policy,
        );
    }

    pub(crate) fn render_hierarchy_groups(&self, output: &mut String, include_policy: bool) {
        let kinds = self
            .units
            .iter()
            .map(|unit| unit.kind)
            .collect::<BTreeSet<_>>();
        for kind in kinds {
            let group_id = stack_group_job_id(kind);
            let group_name = yaml_scalar(unit_group(kind));
            let _ = writeln!(output, "  {group_id}:\n    name: {group_name}");
            let needs = if include_policy {
                "[plan, policy]"
            } else {
                "[plan]"
            };
            let _ = writeln!(
                output,
                "    needs: {needs}\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit {} jobs\n        run: echo 'group ready'\n",
                self.github_runner,
                unit_group(kind),
            );
            for unit in self.units.iter().filter(|unit| unit.kind == kind) {
                let child_id = unit_group_job_id(unit);
                let child_label = unit
                    .label
                    .strip_prefix("Rust crate (")
                    .and_then(|value| value.strip_suffix(')'))
                    .unwrap_or(&unit.label);
                let child_name = yaml_scalar(child_label);
                let _ = writeln!(
                    output,
                    "  {child_id}:\n    name: {child_name}\n    needs: [{group_id}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit runner jobs\n        run: echo 'crate group ready'\n",
                    self.github_runner,
                );
            }
        }
    }

    pub(crate) fn render_verify_lane(
        &self,
        output: &mut String,
        lane: RunnerMode,
        condition: Option<&str>,
        cache_save: bool,
        include_policy: bool,
    ) {
        for unit in self
            .units
            .iter()
            .filter(|unit| lane_supports_unit(lane, unit))
        {
            let id = unit_job_id(lane, &unit.id);
            let lane_label = lane.display_name();
            let needs = unit_needs(lane, unit, include_policy);
            let runner = self.runner_for_unit(lane, unit);
            let group = unit_group(unit.kind);
            let label = unit
                .label
                .strip_prefix("Rust crate (")
                .and_then(|value| value.strip_suffix(')'))
                .unwrap_or(&unit.label);
            let job_name = yaml_scalar(&format!("{lane_label} / {group} / {label}"));
            let verify_name = yaml_scalar(&unit.label);
            let _ = writeln!(output, "  {id}:\n    name: {job_name}");
            if let Some(condition) = condition {
                let _ = writeln!(output, "    if: ${{{{ {condition} }}}}");
            }
            let _ = writeln!(output, "    needs: [{}]", needs.join(", "));
            let _ = writeln!(
                output,
                "    runs-on: {runner}\n    timeout-minutes: 45\n    steps:",
            );
            let _ = writeln!(
                output,
                "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false",
                self.pins.checkout,
            );
            Self::render_workflow_runtime_download(output, lane);
            output.push_str(&workflow_selection_artifact_download(None));
            self.render_tool_provisioning(output, lane, unit, cache_save);
            if CacheBackend::Detected.enables_actions_cache(self, unit)
                && let Some(cache) = &unit.cache
            {
                render_retained_output_cache_note(output, self, unit, cache);
                let (paths, key) = rendered_cache_values(cache);
                let cache_key = format!(
                    "ci-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                    unit.id
                );
                let _ = writeln!(
                    output,
                    "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            ci-${{{{ runner.os }}}}-{}-",
                    verify_name,
                    self.pins.cache_restore,
                    unit.id,
                );
            }
            render_cargo_source_preparation(output, unit);
            let _ = writeln!(
                output,
                "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ needs.plan.outputs.scope }}}}\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ github.event.pull_request.base.sha || github.event.before }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}\n        run: |\n          set -o pipefail\n          echo \"VELNOR_CHECKS_STARTED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {} 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n          echo \"VELNOR_CHECKS_ENDED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          exit $rc",
                verify_name,
                yaml_scalar(&unit.id),
                checks_env(unit),
                yaml_scalar(&unit.id),
            );
            render_phase_report_step(
                output,
                &yaml_scalar(&format!("{lane_label} / {group}")),
                yaml_scalar(label).as_str(),
            );
            if cache_save
                && lane == RunnerMode::Github
                && CacheBackend::Detected.enables_actions_cache(self, unit)
                && let Some(cache) = &unit.cache
            {
                let (paths, key) = rendered_cache_values(cache);
                let trusted_cache = format!(
                    "(github.event_name == 'push' && github.ref == 'refs/heads/{}') || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}')",
                    self.default_branch, self.default_branch
                );
                let cache_key = format!(
                    "ci-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                    unit.id
                );
                let _ = writeln!(
                    output,
                    "      - name: Save {} cache\n        if: ({trusted_cache}) && steps.cache.outputs.cache-hit != 'true'\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}",
                    verify_name,
                    self.pins.cache_save,
                );
            }
            output.push('\n');
        }
    }

    pub(crate) fn runner_for_unit(&self, lane: RunnerMode, unit: &Unit) -> String {
        if unit.kind == UnitKind::Swift && lane == RunnerMode::Github {
            return "macos-15".to_owned();
        }
        self.runner_for(lane)
    }

    pub(crate) fn uses_mr_boxington(&self, unit: &Unit) -> bool {
        self.mr_boxington && unit.kind == UnitKind::Rust
    }

    pub(crate) fn tools_for_unit(
        unit: &Unit,
        mise_present: bool,
        mr_boxington: bool,
    ) -> BTreeSet<ToolRequirement> {
        let mut tools = BTreeSet::new();
        match unit.kind {
            UnitKind::OpenTofu => {
                tools.insert(ToolRequirement::OpenTofu);
            }
            UnitKind::Homebrew => {
                tools.insert(ToolRequirement::Homebrew);
            }
            UnitKind::Bun => {
                tools.insert(ToolRequirement::Bun);
            }
            UnitKind::Node => {
                tools.insert(ToolRequirement::Node);
            }
            UnitKind::Gradle => {
                tools.insert(ToolRequirement::Gradle);
            }
            UnitKind::Rust => {
                if mise_present {
                    tools.insert(ToolRequirement::Mise);
                }
                if mr_boxington {
                    tools.insert(ToolRequirement::MrBoxington);
                } else {
                    tools.insert(ToolRequirement::Sccache);
                }
                tools.insert(ToolRequirement::Mold);
                if needs_nextest(unit) {
                    tools.insert(ToolRequirement::Nextest);
                }
                if unit_commands(unit)
                    .any(|command| command.contains("cargo deny") || command.contains("mbx deny"))
                {
                    tools.insert(ToolRequirement::CargoDeny);
                }
                if unit_commands(unit)
                    .any(|command| command.contains("cargo audit") || command.contains("mbx audit"))
                {
                    tools.insert(ToolRequirement::CargoAudit);
                }
            }
            UnitKind::Docker => {
                tools.insert(ToolRequirement::DockerBuildx);
            }
            UnitKind::Swift | UnitKind::Docs => {}
        }
        tools
    }

    /// Render the hosted-lane Rust toolchain contract: restore the cached
    /// `~/.rustup` state, provision exactly the toolchain the repository pins,
    /// and save the result on trusted events. Provisioning always runs — on an
    /// exact cache hit `rustup toolchain install` is a fast no-op, so the hit
    /// and miss paths converge on the same installed state without the
    /// renderer having to model rustup's resolution rules in an `if:`.
    ///
    /// The repository's `rust-toolchain.toml` is the single source of the
    /// channel, components, and targets; the cache key hashes it, so a pin
    /// change mints a new entry instead of silently reusing the old toolchain.
    fn render_rust_toolchain_steps(
        &self,
        output: &mut String,
        toolchain: &RustToolchain,
        cache_save: bool,
    ) {
        // Unit lanes run on push, schedule, and workflow_dispatch, so the
        // trusted gate is the full default-branch set. Surfaces with a
        // narrower trigger set pass their own gate.
        let trusted_cache = format!(
            "(github.event_name == 'push' && github.ref == 'refs/heads/{}') || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}')",
            self.default_branch, self.default_branch
        );
        let save_gate = cache_save.then(|| {
            format!("({trusted_cache}) && steps.rustup-toolchain.outputs.cache-hit != 'true'")
        });
        render_pinned_toolchain_steps(
            output,
            self.pins.cache_restore,
            self.pins.cache_save,
            toolchain,
            save_gate.as_deref(),
        );
    }

    pub(crate) fn render_tool_provisioning(
        &self,
        output: &mut String,
        lane: RunnerMode,
        unit: &Unit,
        cache_save: bool,
    ) {
        // The Velnor job image is the toolchain boundary for self-hosted jobs.
        // Hosted setup actions either are not admitted by Velnor or would
        // redundantly download tools already pinned in that image. Keep
        // transport/setup actions lane-specific instead of rendering one
        // action surface and hoping the runner can ignore the other lane.
        let github_lane = lane == RunnerMode::Github;
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        if github_lane && let Some(toolchain) = &unit.toolchain {
            self.render_rust_toolchain_steps(output, toolchain, cache_save);
        }
        if github_lane && tools.contains(&ToolRequirement::Mise) {
            // The Rust toolchain is never a mise tool: the scan refuses a
            // Rust repository without a pin, and rustup provisions exactly
            // that pin in the steps above. Mise contributes only the tools
            // the unit's own commands name.
            let mise_tools = mise_tool_ids(unit);
            let invokes_mise = commands_invoke_mise(unit);
            if !mise_tools.is_empty() {
                let _ = writeln!(
                    output,
                    "      - name: Set up Mise tools\n        uses: {}\n        with:\n          install_args: {}\n          cache: true",
                    self.pins.mise,
                    mise_tools.join(" ")
                );
            } else if invokes_mise {
                // The unit runs repository tasks through the mise task
                // runner. It gets the runner binary and nothing else: the
                // tools those tasks need are provisioned by the steps above,
                // and auto-install is switched off on the checks steps.
                let _ = writeln!(
                    output,
                    "      - name: Set up Mise\n        uses: {}\n        with:\n          install: false",
                    self.pins.mise
                );
            }
        }
        if tools.contains(&ToolRequirement::MrBoxington) {
            if lane == RunnerMode::Github {
                let (_, key_files) = unit.cache.as_ref().map_or_else(
                    || {
                        rendered_cache_values(&CacheSpec {
                            key_files: vec![
                                "Cargo.lock".to_owned(),
                                "rust-toolchain.toml".to_owned(),
                                "rust-toolchain".to_owned(),
                                "mise.toml".to_owned(),
                                "mise.lock".to_owned(),
                            ],
                            paths: Vec::new(),
                            purpose: CachePurpose::Generic,
                            mbx_output_cache_justification: None,
                            mutable_mount_seed: false,
                        })
                    },
                    rendered_cache_values,
                );
                // Stable per-unit key: exact hits reuse the cache on every run
                // with unchanged dependency inputs, and a new immutable entry
                // is saved only when those inputs change. A per-commit suffix
                // would mint one entry per unit per push, flood the 10 GiB
                // GitHub cache, evict warm entries, and force cold
                // `Downloading crates` / `Compiling` builds.
                let cache_key = format!(
                    "velnor-mbx-{MR_BOXINGTON_CACHE_GENERATION}-{MR_BOXINGTON_VERSION}-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{}-${{{{ hashFiles({key_files}) }}}}",
                    unit.id,
                );
                // Unit-level prefix (no dependency hash): after a lockfile or
                // toolchain bump the previous tree still warms unchanged
                // dependencies instead of rebuilding the world cold.
                let restore_key = format!(
                    "velnor-mbx-{MR_BOXINGTON_CACHE_GENERATION}-{MR_BOXINGTON_VERSION}-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{}-",
                    unit.id,
                );
                let _ = writeln!(
                    output,
                    "      - name: Set up Mr. Boxington\n        uses: {}\n        with:\n          backend: github\n          github-cache-mode: objects\n          version: {MR_BOXINGTON_VERSION}\n          cache-key: {cache_key}\n          restore-keys: |\n            {restore_key}\n          save-on-workflow-dispatch: true",
                    self.pins.mr_boxington
                );
            } else {
                // Velnor lane: the job image pins Mr. Boxington and the runner
                // mounts its host-persistent local store, so the local backend
                // reuses it with no download and no cache transport. The
                // version is deliberately omitted: pinning one would force a
                // release download on every job instead of reusing PATH mbx.
                let _ = writeln!(
                    output,
                    "      - name: Set up Mr. Boxington\n        uses: {}\n        with:\n          backend: local",
                    self.pins.mr_boxington
                );
            }
        }
        if github_lane && tools.contains(&ToolRequirement::Bun) {
            if let Some(bun_version) = unit.tool_version.as_deref() {
                let _ = writeln!(
                    output,
                    "      - name: Set up Bun\n        uses: {}\n        with:\n          bun-version: {bun_version}",
                    self.pins.bun
                );
            } else {
                let _ = writeln!(
                    output,
                    "      - name: Set up Bun\n        uses: {}",
                    self.pins.bun
                );
            }
        }
        if github_lane && tools.contains(&ToolRequirement::Node) {
            let cache_manager = "npm";
            let cache_dependency_path = unit.cache.as_ref().and_then(|cache| {
                cache
                    .key_files
                    .iter()
                    .find(|path| path.ends_with("package-lock.json"))
            });
            let cache_options = cache_dependency_path.map_or_else(
                || "\n          package-manager-cache: false".to_owned(),
                |path| {
                    format!(
                        "\n          cache: {cache_manager}\n          cache-dependency-path: {}",
                        yaml_scalar(path)
                    )
                },
            );
            let _ = writeln!(
                output,
                "      - name: Set up Node.js\n        uses: {}\n        with:\n          node-version: lts/*{cache_options}",
                self.pins.node
            );
        }
        if github_lane && tools.contains(&ToolRequirement::Nextest) && !self.mise_present {
            let _ = writeln!(
                output,
                "      - name: Set up cargo-nextest\n        uses: {}\n        with:\n          tool: nextest\n          fallback: none",
                self.pins.rust_tool
            );
        }
        if github_lane && tools.contains(&ToolRequirement::CargoDeny) {
            let _ = writeln!(
                output,
                "      - name: Set up cargo-deny\n        uses: {}\n        with:\n          tool: cargo-deny\n          fallback: none",
                self.pins.rust_tool
            );
        }
        if github_lane && tools.contains(&ToolRequirement::CargoAudit) {
            let _ = writeln!(
                output,
                "      - name: Set up cargo-audit\n        uses: {}\n        with:\n          tool: cargo-audit\n          fallback: none",
                self.pins.rust_tool
            );
        }
        if github_lane && tools.contains(&ToolRequirement::Gradle) {
            let _ = writeln!(
                output,
                "      - name: Set up Gradle\n        uses: {}",
                self.pins.gradle
            );
        }
        if github_lane && tools.contains(&ToolRequirement::Sccache) {
            let _ = writeln!(
                output,
                "      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0",
                self.pins.sccache
            );
        }
        if github_lane && tools.contains(&ToolRequirement::Mold) {
            output.push_str(&hosted_mold_setup(&self.default_branch, cache_save));
        }
        if github_lane && tools.contains(&ToolRequirement::DockerBuildx) {
            let _ = writeln!(
                output,
                "      - name: Expose GitHub Actions runtime\n        uses: {}",
                self.pins.github_runtime
            );
            let _ = writeln!(
                output,
                "      - name: Set up Docker Buildx\n        uses: {}",
                self.pins.docker_buildx
            );
        }
        if github_lane && tools.contains(&ToolRequirement::OpenTofu) {
            let _ = writeln!(
                output,
                "      - name: Set up OpenTofu\n        uses: {}\n        with:\n          tofu_version: {}\n          tofu_wrapper: false",
                self.pins.opentofu_setup,
                OPEN_TOFU_VERSION,
            );
        }
        if github_lane && tools.contains(&ToolRequirement::Homebrew) {
            output.push_str(
                "      - name: Prepare Linuxbrew path\n        shell: bash\n        run: |\n          set -euo pipefail\n          if command -v brew >/dev/null 2>&1; then\n            exit 0\n          fi\n          linuxbrew_bin=/home/linuxbrew/.linuxbrew/bin\n          linuxbrew_sbin=/home/linuxbrew/.linuxbrew/sbin\n          if [[ ! -x \"$linuxbrew_bin/brew\" ]]; then\n            printf '%s\\n' 'Homebrew unavailable: install brew or expose it on PATH' >&2\n            exit 1\n          fi\n          [[ -n \"${GITHUB_PATH:-}\" ]] || { printf '%s\\n' 'GITHUB_PATH is unavailable' >&2; exit 1; }\n          printf '%s\\n%s\\n' \"$linuxbrew_bin\" \"$linuxbrew_sbin\" >> \"$GITHUB_PATH\"\n",
            );
        }
    }

    pub(crate) fn render_required(
        &self,
        output: &mut String,
        runners: RunnerMode,
        _stable: bool,
        trusted: bool,
        include_policy: bool,
    ) {
        let check_name = "ci-required";
        let lanes = match runners {
            RunnerMode::Github => vec![RunnerMode::Github],
            RunnerMode::Velnor => vec![RunnerMode::Velnor],
            RunnerMode::Both => vec![RunnerMode::Github, RunnerMode::Velnor],
        };
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        let mut job_checks = Vec::new();
        for lane in lanes {
            for unit in self
                .units
                .iter()
                .filter(|unit| lane_supports_unit(lane, unit))
            {
                let id = unit_job_id(lane, &unit.id);
                needs.push(id.clone());
                job_checks.push((unit.id.clone(), id, lane == RunnerMode::Velnor && !trusted));
            }
        }
        let gate = if runners == RunnerMode::Velnor && trusted {
            format!(
                "always() && github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
                self.default_branch
            )
        } else {
            "always()".to_owned()
        };
        let _ = writeln!(
            output,
            "  ci-required:\n    name: {check_name}\n    if: ${{{{ {gate} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated unit results\n        shell: bash\n        run: |",
            needs.join(", "),
            yaml_scalar(&self.github_runner),
        );
        for job in needs
            .iter()
            .filter(|job| matches!(job.as_str(), "plan" | "policy"))
        {
            let result = github_expression(&format!("needs['{job}'].result"));
            let _ = writeln!(
                output,
                "          result=\"{result}\"\n          if [[ \"$result\" != success ]]; then\n            echo \"required CI prerequisite {job} did not pass: $result\" >&2\n            exit 1\n          fi"
            );
        }
        let selected = github_expression("needs.plan.outputs.units");
        let _ = writeln!(output, "          selected=\",{selected},\"");
        for (unit, job, allow_selected_skip) in job_checks {
            let result = github_expression(&format!("needs['{job}'].result"));
            let selected_case = if allow_selected_skip {
                "success|skipped"
            } else {
                "success"
            };
            let _ = writeln!(
                output,
                "          if [[ \"$selected\" == *\",{unit},\"* ]]; then\n            result=\"{result}\"\n            case \"$result\" in\n              {selected_case}) ;;\n              *) echo \"selected CI job {job} did not pass: $result\" >&2; exit 1 ;;\n            esac\n          else\n            case \"$result\" in\n              success|skipped) ;;\n              *) echo \"unselected CI job {job} failed unexpectedly: $result\" >&2; exit 1 ;;\n            esac\n          fi"
            );
        }
    }
}

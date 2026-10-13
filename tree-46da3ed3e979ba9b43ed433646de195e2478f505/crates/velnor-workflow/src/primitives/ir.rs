//! The resolved render environment for the declared CI surface.
//!
//! `WorkflowIr` is the lane and toolchain context every primitive renders
//! against: which lanes exist, which runner each lane uses, and which tools a
//! unit needs provisioned. It carries no repository knowledge of its own; every
//! value comes from the scanned shape and the repo-owned generation config.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::snapshot::{
    freshness_expression, snapshot_class_prefix, snapshot_key, snapshot_restore_keys,
    CompatibilityFacts, SNAPSHOT_SCHEMA,
};
use super::{
    CacheBackend, GraphNode, LaneJob, Pins, UnitContract, DEFAULT_UNIT_TIMEOUT_MINUTES,
    MUTABLE_MOUNT_HOST_DIR,
};
use crate::{
    config_rust_toolchain, github_expression, hosted_mold_setup, kind_unit_workflow_shard_file,
    lane_supports_unit, nested_unit_workflow_file,
    rendered_cache_values, sidebar_group_name, stack_group_job_id, unit_group, unit_group_job_id,
    unit_job_id, unit_needs, velnor_runner, velnor_runner_group, workflow_runtime_artifact_upload,
    workflow_runtime_download, workflow_runtime_setup, workflow_selection_artifact_download,
    workflow_selection_artifact_upload, yaml_scalar, CachePurpose, CacheSpec, GeneratorError,
    ProjectConfig, RunnerMode, RustToolchain, Unit, UnitKind, GENERATED_HEADER,
    MR_BOXINGTON_VERSION, OPEN_TOFU_VERSION, VELNOR_POLICY_WORKFLOW_REV,
};

/// GitHub rejects reusable workflow files above this size.
pub(crate) const GITHUB_WORKFLOW_BYTE_LIMIT: usize = 500_000;
/// Leave headroom for parser overhead and future pin growth.
const KIND_WORKFLOW_SHARD_BUDGET: usize = 480_000;

/// The snapshot namespace the unit-lane compiler snapshots live in.
const UNIT_SNAPSHOT_NAMESPACE: &str = "velnor-mbx";
/// The snapshot namespace the Docker mutable-mount seed bundle lives in. Kept
/// out of the `-mbx-` namespace so retention classifies it as the protected
/// baseline it is, never as a rolling compiler snapshot.
const DOCKER_SEED_SNAPSHOT_NAMESPACE: &str = "velnor-docker-seed";

/// Dependency-closure inputs a unit snapshot hashes at restore time: the Cargo
/// configuration, manifests, and lockfiles of the unit and of every workspace
/// unit it depends on. A change to any of them mints a new dependency segment,
/// so the previous snapshot stays reachable as a fallback instead of a stale
/// exact hit.
fn snapshot_dependency_inputs(members: &[&Unit]) -> Vec<String> {
    let mut patterns = Vec::new();
    for member in members {
        patterns.extend(member.cache.as_ref().map_or_else(
            || {
                vec![
                    "Cargo.lock".to_owned(),
                    "rust-toolchain.toml".to_owned(),
                    "rust-toolchain".to_owned(),
                    "mise.toml".to_owned(),
                    "mise.lock".to_owned(),
                ]
            },
            |cache| cache.key_files.clone(),
        ));
    }
    patterns.sort();
    patterns.dedup();
    patterns
}

/// The units a snapshot-carrying unit compiles: the unit itself plus the
/// transitive workspace dependency closure its `depends_on` names.
fn closure_members<'a>(unit: &'a Unit, units: &'a [Unit]) -> Vec<&'a Unit> {
    let mut members = vec![unit];
    let mut cursor = 0;
    while cursor < members.len() {
        let current = members[cursor];
        cursor += 1;
        for dependency in &current.depends_on {
            if members.iter().any(|member| member.id == *dependency) {
                continue;
            }
            if let Some(member) = units.iter().find(|candidate| candidate.id == *dependency) {
                members.push(member);
            }
        }
    }
    members
}

/// Source-state inputs a unit snapshot hashes into its freshness segment: the
/// compiled sources of the dependency closure plus every watched file a build
/// embeds or copies (a Rust unit's `include_str!` data, a Docker unit's build
/// context). Manifests, lockfiles, toolchain pins, and Cargo configuration are
/// dependency inputs and ride the other segment; hashing them again here would
/// cold-restart the closure on a metadata-only edit without advancing state.
fn snapshot_state_files(members: &[&Unit], unit: &Unit) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for member in members {
        if member.kind == UnitKind::Rust {
            files.push(format!("{}/**/*.rs", member.root));
        }
        for watched in &member.watch {
            let compiled_source = watched.ends_with("*.rs");
            let dependency_input = std::path::Path::new(watched)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
                || watched == "Cargo.lock"
                || watched == "rust-toolchain"
                || watched == ".cargo/**";
            if !compiled_source && !dependency_input {
                files.push(watched.clone());
            }
        }
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        // A unit with no derivable source input must still hash something:
        // a freshness segment that hashes nothing would freeze the class at
        // its first save, which is the defect this grammar exists to remove.
        files.push(format!("{}/**", unit.root));
    }
    files
}

/// The snapshot identity a static template consumes: the compatibility token
/// (`<schema>-<digest>`) a template renders into its keys and restore
/// prefixes, and the freshness expression it appends to its primary keys.
///
/// Static templates own their dependency hashFiles and their per-job variant
/// names — those are lane-specific facts a repo contract states explicitly —
/// so the marker covers only the two halves the generator owns: who may
/// import, and what counts as new state.
pub(crate) fn config_snapshot_identity(config: &ProjectConfig) -> (String, String) {
    let rust_units: Vec<&Unit> = config
        .units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Rust)
        .collect();
    let mise_present = config
        .analysis
        .detected
        .iter()
        .any(|item| item == "mise-present")
        && !rust_units.is_empty();
    let facts = CompatibilityFacts {
        schema: SNAPSHOT_SCHEMA,
        payload: "mbx",
        mbx_version: MR_BOXINGTON_VERSION.to_owned(),
        toolchain: config_rust_toolchain(config),
        host_image: config.github_runner.clone(),
        linker: "mold".to_owned(),
        rustflags: if mise_present {
            "-C link-arg=-fuse-ld=mold".to_owned()
        } else {
            String::new()
        },
        cargo_inputs: Vec::new(),
        recipe: Vec::new(),
    };
    let mut state_files: Vec<String> = Vec::new();
    for unit in &rust_units {
        let members = closure_members(unit, &config.units);
        state_files.extend(snapshot_state_files(&members, unit));
    }
    state_files.sort();
    state_files.dedup();
    (
        format!("{}-{}", SNAPSHOT_SCHEMA, facts.digest()),
        freshness_expression(&state_files),
    )
}

/// The compatibility identity of a unit snapshot: every fact that decides
/// whether a saved snapshot may be imported at all. Freshness is deliberately
/// absent here — it is the second, source-state half of the key.
fn snapshot_compatibility(
    ir: &WorkflowIr,
    unit: &Unit,
    dependency_inputs: &[String],
) -> CompatibilityFacts {
    CompatibilityFacts {
        schema: SNAPSHOT_SCHEMA,
        payload: if unit.kind == UnitKind::Docker {
            "docker-seed"
        } else {
            "mbx"
        },
        mbx_version: MR_BOXINGTON_VERSION.to_owned(),
        toolchain: unit.toolchain.clone(),
        host_image: ir.github_runner.clone(),
        linker: if ir.tools.contains(&ToolRequirement::Mold) {
            "mold".to_owned()
        } else {
            String::new()
        },
        rustflags: if ir.mise_present {
            "-C link-arg=-fuse-ld=mold".to_owned()
        } else {
            String::new()
        },
        cargo_inputs: dependency_inputs.to_vec(),
        recipe: unit_commands(unit).cloned().collect(),
    }
}

/// A rendered snapshot key and its restore prefixes for one unit on one
/// namespace. The key carries the compatibility digest, the runtime segments,
/// the dependency inputs GitHub hashes at restore time, and the freshness
/// segment that lets a state-advancing run save; the restore prefixes stay
/// prefixes, so the most recently saved compatible generation wins and an
/// exact old generation can never shadow it.
fn unit_snapshot(ir: &WorkflowIr, unit: &Unit, namespace: &str) -> (String, String) {
    let members = closure_members(unit, &ir.units);
    let dependency_inputs = snapshot_dependency_inputs(&members);
    let dependency = freshness_expression(&dependency_inputs);
    let facts = snapshot_compatibility(ir, unit, &dependency_inputs);
    let class_prefix = snapshot_class_prefix(namespace, &facts.digest());
    let state = freshness_expression(&snapshot_state_files(&members, unit));
    (
        snapshot_key(&class_prefix, &unit.id, &dependency, &state),
        snapshot_restore_keys(&class_prefix, &unit.id, &dependency),
    )
}

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

/// Env for the source-preparation step: the mise auto-install suppression
/// only. Preparation is the one Cargo step allowed to reach the network — it
/// establishes the local source state that `CARGO_NET_OFFLINE` verification
/// consumes afterwards, and it is the visible recovery when a cache restore
/// missed. Restricting the fetch itself (release-preview runs failed exactly
/// this way: `can't checkout from … you are in the offline mode` with no
/// cached Git database to serve it) turns every cache miss into an
/// unrecoverable failure.
pub(crate) fn preparation_env() -> String {
    let mut env = String::new();
    env.push_str("\n          MISE_AUTO_INSTALL: \"false\"");
    env.push_str("\n          MISE_EXEC_AUTO_INSTALL: \"false\"");
    env.push_str("\n          MISE_NOT_FOUND_AUTO_INSTALL: \"false\"");
    env
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
pub(crate) fn needs_nextest(unit: &Unit) -> bool {
    unit_commands(unit)
        .any(|command| command.contains("cargo nextest") || command.contains("mbx nextest"))
}

/// The two spellings a lock may pin the nextest runner under. Locks mix bare
/// keys (`cargo-nextest`) and backend-qualified keys
/// (`aqua:nextest-rs/nextest/cargo-nextest`), and `mise --locked` requires the
/// install args to equal the lock keys byte for byte, so the scanner resolves
/// the spelling from the lock instead of mandating one.
pub(crate) const QUALIFIED_NEXTEST_TOOL: &str = "aqua:nextest-rs/nextest/cargo-nextest";
pub(crate) const BARE_NEXTEST_TOOL: &str = "cargo-nextest";

/// The nextest tool id to install for a unit that needs it, resolved against
/// the root lock keys: the lock's own spelling wins, and an absent lock keeps
/// the historical qualified id. A lock that pins neither spelling resolves to
/// the qualified id as well; the scan refuses that state before rendering (see
/// `validate_nextest_tools_are_locked`), so the fallback only serves
/// hand-built configs that never passed through a scan.
pub(crate) fn nextest_tool_id(lock_keys: &BTreeSet<String>) -> &'static str {
    if !lock_keys.is_empty()
        && !lock_keys.contains(QUALIFIED_NEXTEST_TOOL)
        && lock_keys.contains(BARE_NEXTEST_TOOL)
    {
        BARE_NEXTEST_TOOL
    } else {
        QUALIFIED_NEXTEST_TOOL
    }
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

/// The mise tool ids a unit's jobs install: what the scan detects from the
/// unit's own commands, plus what the repository declares for tools the scan
/// cannot see. The language toolchain is deliberately absent — rustup
/// provisions it from the repository's pin — and so is every tool a policy
/// step installs through its own action. Each additional id widens the supply
/// chain of every job that runs it. Detected ids resolve against the root lock
/// keys; declared ids were already matched to the lock by validation.
pub(crate) fn mise_tool_ids(unit: &Unit, lock_keys: &BTreeSet<String>) -> Vec<String> {
    let mut tools = Vec::new();
    if needs_nextest(unit) {
        tools.push(nextest_tool_id(lock_keys).to_owned());
    }
    for declared in &unit.mise_tools {
        if !tools.iter().any(|tool| tool == declared) {
            tools.push(declared.clone());
        }
    }
    tools
}

/// Refuse a unit that needs nextest while the root lock pins neither spelling
/// of the runner: rendering either id would emit `install_args` the runner's
/// own lock check rejects. An absent lock (empty keys) cannot prove the gap,
/// so it passes and rendering keeps the historical qualified id.
///
/// # Errors
/// Returns a usage error naming the first unit whose nextest need the lock
/// does not pin, with every key the lock does pin.
pub(crate) fn validate_nextest_tools_are_locked(
    units: &[Unit],
    lock_keys: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if lock_keys.is_empty()
        || lock_keys.contains(QUALIFIED_NEXTEST_TOOL)
        || lock_keys.contains(BARE_NEXTEST_TOOL)
    {
        return Ok(());
    }
    for unit in units {
        if needs_nextest(unit) {
            let known = lock_keys.iter().cloned().collect::<Vec<_>>().join(", ");
            return Err(GeneratorError::usage(format!(
                "unit {} runs cargo-nextest but mise.lock pins neither {QUALIFIED_NEXTEST_TOOL} nor {BARE_NEXTEST_TOOL}; install one and re-lock so install_args match the lock, known keys: {known}",
                unit.id
            )));
        }
    }
    Ok(())
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
/// the fetch is visible and measurable on its own. The fetch itself runs
/// online (`preparation_env`, never `checks_env`) — it is the recovery path
/// the offline restriction presumes already happened.
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
        preparation_env()
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
    let (paths, _) = rendered_cache_values(cache);
    let (key, restore_keys) = unit_snapshot(ir, unit, DOCKER_SEED_SNAPSHOT_NAMESPACE);
    let _ = writeln!(
        output,
        "      - name: Restore Docker build seed\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}\n          restore-keys: |\n            {restore_keys}",
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
        let (paths, _) = rendered_cache_values(cache);
        let (key, _) = unit_snapshot(ir, unit, DOCKER_SEED_SNAPSHOT_NAMESPACE);
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
    pub(crate) macos_runner: String,
    pub(crate) velnor_labels: Vec<String>,
    pub(crate) ci_required: bool,
    pub(crate) velnor_runner_group: Option<String>,
    pub(crate) pull_request_on_velnor: VelnorPullRequest,
    pub(crate) default_dispatch_runner: String,
    pub(crate) runners: RunnerMode,
    pub(crate) tools: BTreeSet<ToolRequirement>,
    /// The repository drives its Rust units through mise. Naming matters: mise
    /// never provides the Rust toolchain (rustup owns that), it only
    /// contributes the task runner and the tools units declare.
    pub(crate) mise_present: bool,
    pub(crate) mr_boxington: bool,
    pub(crate) units: Vec<Unit>,
    pub(crate) pins: Pins,
    /// Tool keys the root `mise.lock` pins. Detected `install_args` resolve
    /// their spelling from these; empty when the scan root has no lock.
    pub(crate) mise_lock_keys: BTreeSet<String>,
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
pub(crate) enum VelnorPullRequest {
    TrustedOnly,
    Automatic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowKind {
    PullRequest,
    Main,
    Nightly,
}

fn workflow_dispatch_inputs(
    default_scope: &str,
    default_branch: &str,
    extra_inputs: &str,
    runners: RunnerMode,
    default_dispatch_runner: &str,
) -> String {
    let options = crate::dispatch_runner_options(runners)
        .iter()
        .map(|option| format!("          - {option}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "  workflow_dispatch:\n    inputs:\n      runner:\n        description: Execution backend\n        required: true\n        default: {default_dispatch_runner}\n        type: choice\n        options:\n{options}\n      scope:\n        description: Verification scope\n        required: true\n        default: {default_scope}\n        type: choice\n        options:\n          - affected\n          - full\n      base_sha:\n        description: Git ref or SHA used as the affected-selection base\n        required: false\n        default: refs/heads/{default_branch}\n        type: string\n{extra_inputs}"
    )
}

fn aggregate_triggers(
    kind: WorkflowKind,
    default_branch: &str,
    runners: RunnerMode,
    default_dispatch_runner: &str,
) -> (&'static str, &'static str, String, &'static str) {
    match kind {
        WorkflowKind::PullRequest => {
            // Merge-queue validation runs on the GitHub lane only; a
            // velnor-only surface has no lane that can run it.
            let merge_group = if runners == RunnerMode::Velnor {
                ""
            } else {
                "  merge_group:\n"
            };
            (
                "CI",
                "CI / PR",
                format!(
                    "on:\n  pull_request:\n{merge_group}{}",
                    workflow_dispatch_inputs(
                        "affected",
                        default_branch,
                        "",
                        runners,
                        default_dispatch_runner,
                    )
                ),
                "true",
            )
        }
        WorkflowKind::Main => (
            "CI",
            "CI / main",
            format!(
                "on:\n  push:\n    branches: [{}]\n{}",
                yaml_scalar(default_branch),
                workflow_dispatch_inputs(
                    "full",
                    default_branch,
                    "",
                    runners,
                    default_dispatch_runner,
                )
            ),
            "true",
        ),
        WorkflowKind::Nightly => (
            "Nightly",
            "Nightly",
            format!(
                "on:\n  schedule:\n    - cron: '17 3 * * *'\n{}",
                workflow_dispatch_inputs(
                    "full",
                    default_branch,
                    "      simulate_failure:\n        description: Force the red-to-signal test path\n        required: false\n        default: false\n        type: boolean\n",
                    runners,
                    default_dispatch_runner,
                )
            ),
            "true",
        ),
    }
}

fn kind_matrix_output(kind: UnitKind) -> String {
    format!("{}_matrix", kind.id_prefix())
}

fn kind_matrix_output_from_file(file: &str) -> String {
    let stem = file
        .strip_prefix("ci-unit-")
        .and_then(|value| value.strip_suffix(".yml"))
        .unwrap_or("unit");
    format!("{stem}_matrix")
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
            macos_runner: config.macos_runner.clone(),
            velnor_labels: config.velnor_labels.clone(),
            ci_required: config.ci_required,
            velnor_runner_group: velnor_runner_group(config).map(str::to_owned),
            pull_request_on_velnor: if config.pull_request_on_velnor {
                VelnorPullRequest::Automatic
            } else {
                VelnorPullRequest::TrustedOnly
            },
            default_dispatch_runner: config.default_dispatch_runner.clone(),
            runners: config.runners,
            tools,
            mise_present,
            mr_boxington,
            units: config.units.clone(),
            pins: Pins::resolved(),
            mise_lock_keys: config.mise_lock_keys.clone(),
        }
    }

    pub(crate) fn render(&self, kind: WorkflowKind) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, cancel_in_progress) = aggregate_triggers(
            kind,
            &self.default_branch,
            self.runners,
            &self.default_dispatch_runner,
        );
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
        // Runner mode is global; every self-hosted job receives the
        // default-branch trusted-event gate needed for Velnor execution.
        let trusted_event = kind != WorkflowKind::PullRequest;
        let runners = self.runners;
        self.render_plan(&mut output, runners, runners == RunnerMode::Velnor);
        if kind != WorkflowKind::PullRequest {
            self.render_policy(&mut output, runners, runners == RunnerMode::Velnor);
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
                trusted_event || runners == RunnerMode::Velnor,
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
        let (workflow_name, run_name, triggers, cancel_in_progress) = aggregate_triggers(
            kind,
            &self.default_branch,
            self.runners,
            &self.default_dispatch_runner,
        );
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
            self.render_policy(
                &mut output,
                self.runners,
                self.runners == RunnerMode::Velnor,
            );
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

    /// One reusable-workflow caller per unit kind. Selected units of that kind
    /// fan out through `strategy.matrix.include` from the plan artifact so
    /// unselected units are never scheduled.
    pub(crate) fn render_node_callers(
        nodes: &[GraphNode],
        output: &mut String,
        include_policy: bool,
    ) {
        let mut groups: BTreeMap<(&str, &str), Vec<(&str, &str)>> = BTreeMap::new();
        for (unit_id, job_id, name, file) in nodes.iter().filter_map(GraphNode::as_unit) {
            groups
                .entry((job_id, file))
                .or_default()
                .push((unit_id, name));
        }
        for ((job_id, file), _units) in groups {
            let mut needs = vec!["plan".to_owned()];
            if include_policy {
                needs.push("policy".to_owned());
            }
            let matrix_output = kind_matrix_output_from_file(file);
            let mut conditions = vec![
                "always()".to_owned(),
                "needs.plan.result == 'success'".to_owned(),
            ];
            if include_policy {
                conditions.push("needs.policy.result == 'success'".to_owned());
            }
            conditions.push(format!("needs.plan.outputs.{matrix_output} != '[]'"));
            let _ = writeln!(
                output,
                "  {job_id}:\n    name: ${{{{ matrix.label }}}}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    strategy:\n      fail-fast: false\n      matrix:\n        include: ${{{{ fromJSON(needs.plan.outputs.{matrix_output}) }}}}\n    uses: ./.github/workflows/{file}\n    with:\n      unit: ${{{{ matrix.unit }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      selection-artifact: velnor-ci-selection",
                conditions.join(" && "),
                needs.join(", "),
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
        let mut units = Vec::new();
        for job_id in nodes
            .iter()
            .filter_map(|node| node.as_unit().map(|(_, job_id, _, _)| job_id.to_owned()))
        {
            if !units.contains(&job_id) {
                units.push(job_id);
            }
        }
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        needs.extend(units);
        let if_condition = if self.runners == RunnerMode::Velnor {
            format!("always() && {}", self.velnor_control_plane_expression())
        } else {
            "always()".to_owned()
        };
        let needs_json = github_expression("toJSON(needs)");
        let selected_units = github_expression("needs.plan.outputs.units");
        let _ = writeln!(
            output,
            "  {check_name}:\n    name: {check_name}\n    if: ${{{{ {if_condition} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated stack results\n        env:\n          NEEDS_JSON: {needs_json}\n          SELECTED_UNITS: {selected_units}",
            needs.join(", "),
            self.runner_for(self.runners)
        );
        if simulate_failure {
            let simulate = github_expression("inputs.simulate_failure");
            let _ = writeln!(output, "          SIMULATE_FAILURE: {simulate}");
        }
        output.push_str("        shell: bash\n        run: |\n          set -euo pipefail\n");
        if simulate_failure {
            output.push_str(
                "          if [[ \"$SIMULATE_FAILURE\" == true ]]; then\n            echo \"nightly red-to-signal simulation requested\" >&2\n            exit 1\n          fi\n",
            );
        }
        output.push_str(
            "          result_for_job() {\n            jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"\n          }\n",
        );
        for job in needs
            .iter()
            .filter(|job| matches!(job.as_str(), "plan" | "policy"))
        {
            let _ = writeln!(
                output,
                "          result=\"$(result_for_job {job})\"\n          if [[ \"$result\" != success ]]; then\n            echo \"required CI prerequisite {job} did not pass: $result\" >&2\n            exit 1\n          fi"
            );
        }
        output.push_str("          selected=\",$SELECTED_UNITS,\"\n");
        for (unit_id, job_id, _, _) in nodes.iter().filter_map(GraphNode::as_unit) {
            let job = job_id;
            let _ = writeln!(
                output,
                "          if [[ \"$selected\" == *\",{unit_id},\"* ]]; then\n            result=\"$(result_for_job {job})\"\n            case \"$result\" in\n              success) ;;\n              *) echo \"selected CI unit {unit_id} did not pass: $result\" >&2; exit 1 ;;\n            esac\n          else\n            result=\"$(result_for_job {job})\"\n            case \"$result\" in\n              success|skipped) ;;\n              *) echo \"unselected CI unit {unit_id} failed unexpectedly: $result\" >&2; exit 1 ;;\n            esac\n          fi",
            );
        }
    }

    pub(crate) fn render_nightly_alert(&self, output: &mut String) {
        let if_condition = if self.runners == RunnerMode::Velnor {
            format!(
                "always() && github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
                self.default_branch
            )
        } else {
            "always()".to_owned()
        };
        let _ = writeln!(
            output,
            "  nightly-alert:\n    name: Nightly red-to-signal\n    if: ${{{{ {if_condition} }}}}\n    needs: [nightly-required]\n    runs-on: {}\n    permissions:\n      contents: read\n      issues: write\n    steps:\n      - name: Open or update nightly failure signal\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          NIGHTLY_RESULT: ${{{{ needs.nightly-required.result }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [[ \"$NIGHTLY_RESULT\" == success ]]; then\n            exit 0\n          fi\n          echo \"::error::nightly-required failed: $NIGHTLY_RESULT\"\n          existing=\"$(gh api \"repos/$GITHUB_REPOSITORY/issues?state=open\" --jq '.[] | select(.title == \"Nightly CI red\") | .number' | sed -n '1p')\"\n          body=\"nightly-required result: $NIGHTLY_RESULT\nRun: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID\"\n          if [[ -n \"$existing\" ]]; then\n            gh api --method PATCH \"repos/$GITHUB_REPOSITORY/issues/$existing\" -f body=\"$body\" >/dev/null\n          else\n            gh api --method POST \"repos/$GITHUB_REPOSITORY/issues\" -f title='Nightly CI red' -f body=\"$body\" >/dev/null\n          fi",
            self.runner_for(self.runners)
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
    #[expect(
        clippy::unused_self,
        reason = "unit contracts stay on WorkflowIr so callers keep one render environment"
    )]
    pub(crate) fn default_unit_contract(&self, unit: &Unit, cache_save: bool) -> UnitContract {
        UnitContract {
            lanes: Self::default_lane_jobs(cache_save),
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
    /// cache and is untrusted. Velnor jobs are trusted-event-only because they
    /// execute on the self-hosted runner pool.
    pub(crate) fn default_lane_jobs(cache_save: bool) -> Vec<LaneJob> {
        vec![
            LaneJob {
                lane: RunnerMode::Github,
                cache_save,
                trusted: false,
            },
            LaneJob {
                lane: RunnerMode::Velnor,
                cache_save: false,
                trusted: true,
            },
        ]
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
            "name: {}\non:\n  workflow_call:\n    inputs:\n      unit:\n        required: true\n        type: string\n      scope:\n        required: true\n        type: string\n      selection-artifact:\n        required: true\n        type: string",
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

    fn render_kind_units_header(&self, kind: UnitKind) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let _ = writeln!(
            output,
            "name: {}\non:\n  workflow_call:\n    inputs:\n      unit:\n        required: true\n        type: string\n      scope:\n        required: true\n        type: string\n      selection-artifact:\n        required: true\n        type: string\n\njobs:",
            yaml_scalar(unit_group(kind))
        );
        output
    }

    /// One reusable workflow for every unit of `kind`. The caller supplies the
    /// unit id through `workflow_call` inputs so a monorepo does not emit one
    /// unique reusable file per unit. When the rendered surface exceeds
    /// GitHub's byte limit, units are packed into additional shard files.
    pub(crate) fn render_kind_unit_workflows(
        &self,
        kind: UnitKind,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let members = self
            .units
            .iter()
            .filter(|unit| unit.kind == kind)
            .collect::<Vec<_>>();
        let mut files = BTreeMap::new();
        let mut assignments = BTreeMap::new();
        if members.is_empty() {
            return (files, assignments);
        }
        let header = self.render_kind_units_header(kind);
        let mut shard_index = 0_usize;
        let mut current_file = kind_unit_workflow_shard_file(kind, shard_index);
        let mut current = header.clone();

        for unit in members {
            let contract = contracts
                .and_then(|contracts| contracts.get(&unit.id))
                .cloned()
                .unwrap_or_else(|| self.default_unit_contract(unit, true));
            let mut unit_body = String::new();
            for job in &contract.lanes {
                if lane_supports_unit(job.lane, unit) {
                    let job_id = unit_job_id(job.lane, &unit.id);
                    self.render_lane_job_for_input(
                        &mut unit_body,
                        *job,
                        unit,
                        &contract,
                        &job_id,
                        Some(&unit.id),
                    );
                }
            }
            if current.len() > header.len()
                && current.len() + unit_body.len() > KIND_WORKFLOW_SHARD_BUDGET
            {
                files.insert(current_file.clone(), current);
                shard_index += 1;
                current_file = kind_unit_workflow_shard_file(kind, shard_index);
                current = header.clone();
            }
            current.push_str(&unit_body);
            assignments.insert(unit.id.clone(), current_file.clone());
        }
        if current.len() > header.len() {
            files.insert(current_file, current);
        }
        (files, assignments)
    }

    /// Legacy entry point returning one unsplit kind workflow.
    #[cfg(test)]
    pub(crate) fn render_kind_units(
        &self,
        kind: UnitKind,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> String {
        self.render_kind_unit_workflows(kind, contracts)
            .0
            .into_values()
            .next()
            .unwrap_or_default()
    }

    pub(crate) fn render_workflow_env(&self, output: &mut String, unit: &Unit) {
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let mut entries = Vec::new();
        if tools.contains(&ToolRequirement::Sccache) {
            entries.push("  CARGO_INCREMENTAL: \"0\"");
            entries.push("  RUSTC_WRAPPER: sccache");
            entries.push("  SCCACHE_GHA_ENABLED: \"true\"");
        }
        if self.mise_present && unit.kind != UnitKind::Swift {
            entries.push("  RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"");
        }
        if tools.contains(&ToolRequirement::OpenTofu) {
            entries.push("  TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache");
        }
        if entries.is_empty() {
            return;
        }
        output.push_str("\nenv:\n");
        for entry in entries {
            output.push_str(entry);
            output.push('\n');
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
        let id = job.lane.as_str();
        self.render_lane_job_for_input(output, job, unit, contract, id, None);
    }

    fn render_lane_job_for_input(
        &self,
        output: &mut String,
        job: LaneJob,
        unit: &Unit,
        contract: &UnitContract,
        id: &str,
        input_unit: Option<&str>,
    ) {
        let lane = job.lane;
        let cache_save = job.cache_save && contract.cache_save;
        let name = input_unit.map_or_else(
            || lane.display_name().to_owned(),
            |unit_id| format!("{} / {unit_id}", lane.display_name()),
        );
        let _ = writeln!(output, "  {id}:\n    name: {}", yaml_scalar(&name));
        let lane_gate = self.lane_event_expression(lane);
        let gate = input_unit.map_or(lane_gate.clone(), |unit_id| {
            format!("inputs.unit == '{unit_id}' && ({lane_gate})")
        });
        let _ = writeln!(output, "    if: ${{{{ {gate} }}}}");
        let _ = writeln!(output, "    runs-on: {}", self.runner_for_unit(lane, unit));
        if input_unit.is_some() {
            self.render_job_env(output, lane, unit);
        }
        let _ = writeln!(output, "    timeout-minutes: {}", contract.timeout_minutes);
        Self::render_job_services(output, unit);
        output.push_str("    steps:\n");
        let _ = writeln!(
            output,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false",
            self.pins.checkout
        );
        self.render_unit_runtime(output, lane, unit);
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
                unit.kind.id_prefix()
            );
            let _ = writeln!(
                output,
                "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            ci-${{{{ runner.os }}}}-{}-",
                yaml_scalar(&unit.label),
                self.pins.cache_restore,
                unit.kind.id_prefix()
            );
        }
        render_cargo_source_preparation(output, unit);
        let base_sha = self.base_sha_expression();
        let _ = writeln!(
            output,
            "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ {} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}\n        run: |\n          set -o pipefail\n          echo \"VELNOR_CHECKS_STARTED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n          echo \"VELNOR_CHECKS_ENDED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          exit $rc",
            yaml_scalar(&unit.label),
            base_sha,
            checks_env(unit),
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
                unit.kind.id_prefix()
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

    fn render_job_env(&self, output: &mut String, lane: RunnerMode, unit: &Unit) {
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let postgres = unit
            .services
            .iter()
            .find(|service| service.name == "postgres");
        let mut entries = Vec::<String>::new();
        if tools.contains(&ToolRequirement::Sccache) {
            entries.push("      CARGO_INCREMENTAL: \"0\"".to_owned());
            entries.push("      RUSTC_WRAPPER: sccache".to_owned());
            entries.push("      SCCACHE_GHA_ENABLED: \"true\"".to_owned());
        }
        if self.mise_present && unit.kind != UnitKind::Swift {
            entries.push("      RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"".to_owned());
        }
        if tools.contains(&ToolRequirement::OpenTofu) {
            entries.push("      TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache".to_owned());
        }
        if lane == RunnerMode::Velnor
            && let Some(service) = postgres
        {
            entries.push("      POSTGRESQL_DB_HOST: postgres".to_owned());
            let container_port = service
                .ports
                .first()
                .and_then(|mapping| mapping.split_once(':'))
                .map(|(_, container)| container)
                .filter(|port| !port.is_empty())
                .unwrap_or("5432");
            entries.push(format!(
                "      POSTGRESQL_DB_PORT: {}",
                yaml_scalar(container_port)
            ));
        }
        if entries.is_empty() {
            return;
        }
        output.push_str("    env:\n");
        for entry in entries {
            output.push_str(&entry);
            output.push('\n');
        }
    }

    fn render_job_services(output: &mut String, unit: &Unit) {
        if unit.services.is_empty() {
            return;
        }
        output.push_str("    services:\n");
        for service in &unit.services {
            let _ = writeln!(output, "      {}:", service.name);
            let _ = writeln!(output, "        image: {}", yaml_scalar(&service.image));
            if !service.env.is_empty() {
                output.push_str("        env:\n");
                for (name, value) in &service.env {
                    let _ = writeln!(output, "          {name}: {}", yaml_scalar(value));
                }
            }
            if !service.ports.is_empty() {
                output.push_str("        ports:\n");
                for port in &service.ports {
                    let _ = writeln!(output, "          - {}", yaml_scalar(port));
                }
            }
            if !service.options.is_empty() {
                let _ = writeln!(output, "        options: {}", yaml_scalar(&service.options));
            }
        }
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

    fn render_unit_runtime(&self, output: &mut String, lane: RunnerMode, unit: &Unit) {
        if lane != RunnerMode::Github {
            return;
        }
        // Velnor Planning does not publish a SOURCE_REV product. Manual GitHub
        // dispatch jobs bootstrap the pinned runtime themselves. Apple jobs
        // cannot consume a Linux-built plan artifact even when Planning is hosted.
        if self.runners == RunnerMode::Velnor || unit.kind == UnitKind::Swift {
            Self::render_workflow_runtime_setup(output, lane);
        } else {
            Self::render_workflow_runtime_download(output, lane);
        }
    }

    pub(crate) fn render_plan(&self, output: &mut String, runners: RunnerMode, trusted: bool) {
        // Planning follows the configured lane. Velnor uses the image-provided
        // runtime, while an explicit GitHub lane bootstraps the pinned runtime.
        let runners = if self.runners == RunnerMode::Velnor {
            RunnerMode::Velnor
        } else {
            runners
        };
        let gate = if runners == RunnerMode::Velnor {
            format!(
                "    if: ${{{{ {} }}}}\n",
                self.velnor_control_plane_expression()
            )
        } else {
            self.trusted_runner_gate(runners, trusted)
        };
        let runtime_setup = if runners == RunnerMode::Velnor {
            String::new()
        } else {
            workflow_runtime_setup(RunnerMode::Github)
        };
        let mut outputs = vec![
            "      scope: ${{ steps.plan.outputs.scope }}".to_owned(),
            "      units: ${{ steps.plan.outputs.units }}".to_owned(),
            "      full_units: ${{ steps.plan.outputs.full_units }}".to_owned(),
        ];
        let mut matrix_outputs = BTreeSet::new();
        for unit in &self.units {
            matrix_outputs.insert(kind_matrix_output_from_file(&nested_unit_workflow_file(
                unit,
            )));
        }
        for name in matrix_outputs {
            outputs.push(format!(
                "      {name}: ${{{{ steps.plan.outputs.{name} }}}}"
            ));
        }
        let base_sha = self.base_sha_expression();
        // The plan step consumes the dispatch runner input: a velnor-only
        // dispatch plans a velnor-only selection, so units the Velnor lane
        // cannot run stay unselected (and green under the required gate)
        // instead of failing a selection they can never satisfy. Automatic
        // events carry no runner input, so the empty value plans unfiltered.
        let _ = writeln!(
            output,
            "  plan:\n    name: Planning\n{gate}    runs-on: {}\n    outputs:\n{}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Select affected units\n        id: plan\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          CI_SCOPE_OVERRIDE: ${{{{ github.event.inputs.scope || '' }}}}\n          BASE_SHA: ${{{{ {base_sha} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: ${{{{ runner.temp }}}}/velnor-ci-selection\n          VELNOR_RUNNER: ${{{{ github.event.inputs.runner || '' }}}}\n        run: |\n          set -euo pipefail\n          if [[ -z \"${{CI_SCOPE_OVERRIDE:-}}\" ]]; then unset CI_SCOPE_OVERRIDE; fi\n          velnor-workflow plan --config .github/ci/project.toml\n",
            self.runner_for(runners),
            outputs.join("\n"),
            self.pins.checkout,
            base_sha = base_sha,
        );
        if runners != RunnerMode::Velnor {
            output.push_str(&workflow_runtime_artifact_upload());
        }
        output.push_str(&workflow_selection_artifact_upload());
    }

    pub(crate) fn render_policy(&self, output: &mut String, runners: RunnerMode, trusted: bool) {
        if runners == RunnerMode::Velnor {
            let gate = self.trusted_runner_gate(runners, trusted);
            output.push_str(&crate::inline_policy_job_for_lane(
                "Advisory policy",
                VELNOR_POLICY_WORKFLOW_REV,
                &self.runner_for(runners),
                "local",
                Some(&gate),
            ));
        } else {
            output.push_str(&crate::inline_policy_job(
                "Advisory policy",
                VELNOR_POLICY_WORKFLOW_REV,
            ));
        }
    }

    pub(crate) fn trusted_runner_gate(&self, runners: RunnerMode, trusted: bool) -> String {
        if runners == RunnerMode::Velnor && trusted {
            format!("    if: ${{{{ {} }}}}\n", self.trusted_event_expression())
        } else {
            String::new()
        }
    }

    fn trusted_event_expression(&self) -> String {
        format!(
            "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
            self.default_branch
        )
    }

    fn automatic_event_expression(&self) -> String {
        format!(
            "github.event_name == 'pull_request' || (github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule'))",
            self.default_branch
        )
    }

    fn base_sha_expression(&self) -> String {
        format!(
            "github.event.pull_request.base.sha || github.event.inputs.base_sha || github.event.before || 'refs/heads/{}'",
            self.default_branch
        )
    }

    fn lane_event_expression(&self, lane: RunnerMode) -> String {
        let dispatch = match lane {
            RunnerMode::Velnor => {
                "github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')"
            }
            RunnerMode::Github => {
                "github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'github' || github.event.inputs.runner == 'both')"
            }
            RunnerMode::Both => {
                "github.event_name == 'workflow_dispatch'"
            }
        };
        let automatic = matches!(
            (self.runners, lane),
            (RunnerMode::Velnor, RunnerMode::Velnor)
                | (RunnerMode::Github, RunnerMode::Github)
                | (RunnerMode::Both, RunnerMode::Velnor | RunnerMode::Github)
        );
        if automatic {
            if lane == RunnerMode::Velnor {
                self.velnor_lane_event_expression(dispatch)
            } else {
                // Merge-queue validation runs the GitHub lane only, mirroring
                // the pull_request gate; the Velnor lane keeps its trusted
                // push/schedule/dispatch gate and skips merge_group runs.
                format!(
                    "{} || github.event_name == 'merge_group' || ({dispatch})",
                    self.automatic_event_expression()
                )
            }
        } else {
            dispatch.to_owned()
        }
    }

    fn velnor_control_plane_expression(&self) -> String {
        if self.pull_request_on_velnor == VelnorPullRequest::Automatic {
            format!(
                "github.event_name == 'pull_request' || github.event_name == 'workflow_dispatch' || (github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule'))",
                self.default_branch
            )
        } else {
            self.trusted_event_expression()
        }
    }

    fn velnor_lane_event_expression(&self, dispatch: &str) -> String {
        if self.pull_request_on_velnor == VelnorPullRequest::Automatic {
            format!("{} || ({dispatch})", self.automatic_event_expression())
        } else {
            format!(
                "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || ({dispatch}))",
                self.default_branch
            )
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
        let condition = Some(self.trusted_event_expression());
        self.render_verify_lane(
            output,
            RunnerMode::Velnor,
            condition.as_deref(),
            cache_save,
            include_policy,
        );
    }

    pub(crate) fn render_hierarchy_groups(&self, output: &mut String, include_policy: bool) {
        let gate = if self.runners == RunnerMode::Velnor {
            format!(
                "    if: ${{{{ {} }}}}\n",
                self.velnor_control_plane_expression()
            )
        } else {
            String::new()
        };
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
                "{gate}    needs: {needs}\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit {} jobs\n        run: echo 'group ready'\n",
                self.runner_for(self.runners),
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
                    "  {child_id}:\n    name: {child_name}\n{gate}    needs: [{group_id}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit runner jobs\n        run: echo 'crate group ready'\n",
                    self.runner_for(self.runners),
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
            self.render_unit_runtime(output, lane, unit);
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
            let base_sha = self.base_sha_expression();
            let _ = writeln!(
                output,
                "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ needs.plan.outputs.scope }}}}\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ {} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}\n        run: |\n          set -o pipefail\n          echo \"VELNOR_CHECKS_STARTED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {} 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n          echo \"VELNOR_CHECKS_ENDED_EPOCH=$(date +%s)\" >> \"$GITHUB_ENV\"\n          exit $rc",
                verify_name,
                yaml_scalar(&unit.id),
                base_sha,
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
            return yaml_scalar(&self.macos_runner);
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
            UnitKind::Swift => {
                if !unit.mise_tools.is_empty() || commands_invoke_mise(unit) {
                    tools.insert(ToolRequirement::Mise);
                }
            }
            UnitKind::Docs => {}
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
        if !github_lane && self.mise_present {
            // Hosted mise-action is not admitted on Velnor. Auto-install is
            // off on the checks step, so declared lockfile tools must be
            // installed explicitly or shims fail closed.
            output.push_str(
                "      - name: Install declared Mise tools\n        run: |\n          set -euo pipefail\n          mise --yes install\n",
            );
        }
        if github_lane && let Some(toolchain) = &unit.toolchain {
            self.render_rust_toolchain_steps(output, toolchain, cache_save);
        }
        if github_lane && tools.contains(&ToolRequirement::Mise) {
            // The Rust toolchain is never a mise tool: the scan refuses a
            // Rust repository without a pin, and rustup provisions exactly
            // that pin in the steps above. Mise contributes only the tools
            // the unit's own commands name or the repository declares.
            let mise_tools = mise_tool_ids(unit, &self.mise_lock_keys);
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
                // Snapshot identity is two different facts, and the key
                // names both: the compatibility digest (toolchain pin, hosted
                // image, linker recipe, `RUSTFLAGS`, Cargo inputs, recipe)
                // decides whether a snapshot may be imported at all, and the
                // freshness segment (the closure's compiled sources plus
                // embedded data) decides whether the run holds state worth
                // saving. A source change mints a new key, so the run that
                // produced the new state is never refused a save by an
                // immutable exact hit — and an exact hit (same sources,
                // snapshot already saved) writes nothing.
                let (cache_key, restore_keys) = unit_snapshot(self, unit, UNIT_SNAPSHOT_NAMESPACE);
                let _ = writeln!(
                    output,
                    "      - name: Set up Mr. Boxington\n        uses: {}\n        with:\n          backend: github\n          github-cache-mode: objects\n          version: {MR_BOXINGTON_VERSION}\n          cache-key: {cache_key}\n          restore-keys: |\n            {restore_keys}\n          save-on-workflow-dispatch: true",
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
            format!("always() && {}", self.velnor_control_plane_expression())
        } else {
            "always()".to_owned()
        };
        let needs_json = github_expression("toJSON(needs)");
        let selected_units = github_expression("needs.plan.outputs.units");
        let _ = writeln!(
            output,
            "  ci-required:\n    name: {check_name}\n    if: ${{{{ {gate} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated unit results\n        env:\n          NEEDS_JSON: {needs_json}\n          SELECTED_UNITS: {selected_units}\n        shell: bash\n        run: |\n          set -euo pipefail\n          result_for_job() {{\n            jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"\n          }}",
            needs.join(", "),
            self.runner_for(runners),
        );
        for job in needs
            .iter()
            .filter(|job| matches!(job.as_str(), "plan" | "policy"))
        {
            let _ = writeln!(
                output,
                "          result=\"$(result_for_job {job})\"\n          if [[ \"$result\" != success ]]; then\n            echo \"required CI prerequisite {job} did not pass: $result\" >&2\n            exit 1\n          fi"
            );
        }
        output.push_str("          selected=\",$SELECTED_UNITS,\"\n");
        for (unit, job, allow_selected_skip) in job_checks {
            let selected_case = if allow_selected_skip {
                "success|skipped"
            } else {
                "success"
            };
            let _ = writeln!(
                output,
                "          if [[ \"$selected\" == *\",{unit},\"* ]]; then\n            result=\"$(result_for_job {job})\"\n            case \"$result\" in\n              {selected_case}) ;;\n              *) echo \"selected CI job {job} did not pass: $result\" >&2; exit 1 ;;\n            esac\n          else\n            result=\"$(result_for_job {job})\"\n            case \"$result\" in\n              success|skipped) ;;\n              *) echo \"unselected CI job {job} failed unexpectedly: $result\" >&2; exit 1 ;;\n            esac\n          fi"
            );
        }
    }
}

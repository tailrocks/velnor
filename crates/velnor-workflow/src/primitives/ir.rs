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
    cache::{cache_is_velnor_host_persistent, velnor_skips_pinned_rust_toolchain},
    CacheBackend, GraphNode, LaneJob, Pins, UnitContract, DEFAULT_UNIT_TIMEOUT_MINUTES,
    MUTABLE_MOUNT_HOST_DIR,
};
use crate::reuse::REQUIRED_CHECK;
use crate::{
    config_rust_toolchain, github_expression, hosted_cargo_bin_toolchain_restore,
    hosted_cargo_bin_toolchain_save, hosted_cargo_bin_toolchain_verify, hosted_mold_setup,
    kind_unit_workflow_file, lane_supports_unit, nested_unit_workflow_file,
    prepare_cargo_caller_job_id, render_mr_boxington_store_budget_step, rendered_cache_values,
    sidebar_group_name, stack_group_job_id, unit_group, unit_group_job_id, unit_job_display_name,
    unit_job_id, unit_needs, velnor_runner, velnor_runner_group, velnor_rust_dependency_needs,
    workflow_runtime_artifact_upload, workflow_runtime_download, workflow_runtime_setup,
    workflow_selection_file_materialize, yaml_scalar, CachePurpose, CacheSpec, GeneratorError,
    ProjectConfig, RunnerMode, RustToolchain, SelectionFieldSources, Unit, UnitKind,
    VelnorRustNeeds, GENERATED_HEADER, MR_BOXINGTON_VERSION, OPEN_TOFU_VERSION,
};

/// GitHub rejects reusable workflow files above this size.
pub(crate) const GITHUB_WORKFLOW_BYTE_LIMIT: usize = 500_000;

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::process::Command;

    use super::{
        automatic_event_selects_lane, dispatch_choice_selects_lane, dispatch_lane_expression,
        lane_input, unit_owns_workflow_crate, AutomaticEvent, DispatchChoice::*, GraphNode,
        LaneAdmission, Pins, RunnerMode, Unit, UnitKind, VelnorPullRequest, VelnorRustNeeds,
        WorkflowIr, WorkflowKind,
    };
    use crate::{
        nested_unit_workflow_file, sidebar_group_name, stack_group_job_id,
        workflow_setup_action_repository,
    };

    #[test]
    fn manual_dispatch_reachability_matches_the_backend_table() {
        let cases = [
            (Github, true, false),
            (Velnor, false, true),
            (Both, true, true),
            (Omitted, true, true),
        ];
        for (choice, github, velnor) in cases {
            assert_eq!(
                dispatch_choice_selects_lane(choice, RunnerMode::Github),
                github,
                "GitHub reachability for {choice:?}"
            );
            assert_eq!(
                dispatch_choice_selects_lane(choice, RunnerMode::Velnor),
                velnor,
                "Velnor reachability for {choice:?}"
            );
        }

        let github = dispatch_lane_expression(RunnerMode::Github, true);
        let velnor = dispatch_lane_expression(RunnerMode::Velnor, true);
        assert!(github.contains("runner == 'github'"));
        assert!(!github.contains("runner == 'velnor'"));
        assert!(velnor.contains("runner == 'velnor'"));
        assert!(!velnor.contains("runner == 'github'"));
    }

    #[test]
    fn automatic_reachability_keeps_untrusted_events_off_velnor() {
        use AutomaticEvent::{
            MergeGroup, PullRequestFork, PullRequestSameRepository, Push, Schedule,
        };

        let cases = [
            (PullRequestSameRepository, true, true),
            (PullRequestFork, true, false),
            (Push, true, true),
            (Schedule, true, true),
            (MergeGroup, true, true),
        ];
        for (event, github, velnor) in cases {
            assert_eq!(
                automatic_event_selects_lane(
                    event,
                    RunnerMode::Github,
                    VelnorPullRequest::Automatic,
                ),
                github,
                "GitHub reachability for {event:?}"
            );
            assert_eq!(
                automatic_event_selects_lane(
                    event,
                    RunnerMode::Velnor,
                    VelnorPullRequest::Automatic,
                ),
                velnor,
                "Velnor reachability for {event:?}"
            );
        }
        assert!(!automatic_event_selects_lane(
            PullRequestSameRepository,
            RunnerMode::Velnor,
            VelnorPullRequest::TrustedOnly,
        ));
    }

    /// One hand-built Rust unit: the id is fixture-local, the root decides
    /// ownership of the generator crate.
    fn rust_unit(id: &str, root: &str) -> Unit {
        Unit {
            id: id.to_owned(),
            label: format!("Rust crate ({id})"),
            kind: UnitKind::Rust,
            root: root.to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: vec!["cargo test --locked".to_owned()],
            full_commands: vec!["cargo test --locked".to_owned()],
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            requires_trusted: false,
            workspace_check: false,
            platform: crate::platform::PlatformRequirement::portable(),
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
    }

    fn owner_test_ir(repository: &str, units: Vec<Unit>) -> WorkflowIr {
        WorkflowIr {
            default_branch: "main".to_owned(),
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned()],
            ci_required: true,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            velnor_trusted_runner_online: true,
            velnor_trusted_runner_skip_reason: None,
            pull_request_on_velnor: VelnorPullRequest::TrustedOnly,
            repository: repository.to_owned(),
            workflow_revision: "0".repeat(40),
            default_dispatch_runner: "github".to_owned(),
            runners: RunnerMode::Both,
            automatic: RunnerMode::Both,
            velnor_rust_needs: VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            tools: BTreeSet::new(),
            mise_present: false,
            mr_boxington: false,
            units,
            pins: Pins::resolved(),
            mise_lock_keys: BTreeSet::new(),
            declared_ruleset_contexts: String::new(),
        }
    }

    fn aggregate_fixture_nodes(ir: &WorkflowIr) -> Vec<GraphNode> {
        ir.units
            .iter()
            .map(|unit| GraphNode::Unit {
                unit_id: unit.id.clone(),
                job_id: stack_group_job_id(unit.kind),
                name: sidebar_group_name(unit),
                file: nested_unit_workflow_file(unit),
            })
            .collect()
    }

    #[test]
    fn pull_request_aggregate_cancellation_guard_preserves_status_contract() {
        let mut leaf = rust_unit("rust-leaf", "crates/leaf");
        leaf.depends_on = vec!["rust-root".to_owned()];
        let mut ir = owner_test_ir(
            "example/cancellation",
            vec![rust_unit("rust-root", "crates/root"), leaf],
        );
        ir.velnor_rust_needs = VelnorRustNeeds::DependencyClosure;
        let nodes = aggregate_fixture_nodes(&ir);
        let pr = ir.render_nested(WorkflowKind::PullRequest, &nodes, None);
        let expected_callers = ir.required_callers(&nodes, None).len();
        assert_eq!(
            pr.matches("if: ${{ !cancelled() &&").count(),
            expected_callers,
            "every rendered PR caller carries the cancellation guard"
        );
        assert!(
            pr.contains("if: ${{ !cancelled() && needs.plan.result == 'success'"),
            "PR callers keep the explicit plan-success prerequisite after the cancellation guard"
        );
        assert!(
            pr.contains("result == 'skipped'"),
            "dependency skips remain an explicit caller condition"
        );
        assert!(
            pr.contains("selected CI job") && pr.contains("case \"$result\" in"),
            "the required gate still evaluates each selected caller result"
        );
        assert!(
            pr.contains("cancel-in-progress: true")
                && pr.contains("ci-required:\n    name:")
                && pr.contains("if: ${{ !cancelled() }}"),
            "PR concurrency and required checks use the cancellation-aware aggregate guard"
        );
        let direct_pr = ir.render(WorkflowKind::PullRequest);
        assert!(
            direct_pr.contains("cancel-in-progress: true")
                && direct_pr.contains("if: ${{ !cancelled() }}"),
            "the legacy direct renderer carries the same PR guard"
        );

        for kind in [WorkflowKind::Main, WorkflowKind::Nightly] {
            let stable = ir.render_nested(kind, &nodes, None);
            assert!(
                stable.contains("cancel-in-progress: false"),
                "{kind:?} keeps cancellation disabled"
            );
            assert!(
                !stable.contains("if: ${{ !cancelled()"),
                "{kind:?} keeps publishing/alert jobs on the always-running path"
            );
            assert!(
                stable.contains("if: ${{ always()"),
                "{kind:?} retains its always-running aggregate checks"
            );
        }
    }

    #[test]
    fn required_gate_rejects_failed_skipped_and_cancelled_selected_callers() {
        let ir = owner_test_ir(
            "example/cancellation-results",
            vec![rust_unit("rust", "crates/rust")],
        );
        let nodes = aggregate_fixture_nodes(&ir);
        let callers = ir.required_callers(&nodes, None);
        let mut rendered = String::new();
        ir.render_nodes_required(
            &nodes,
            None,
            &mut rendered,
            false,
            super::REQUIRED_CHECK,
            false,
            false,
        );
        let script = must_some(
            rendered
                .split_once("        run: |\n")
                .and_then(|(_, body)| body.split_once("\n  required:\n").map(|(body, _)| body)),
            "required gate shell fixture",
        )
        .lines()
        .map(|line| line.strip_prefix("          ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");

        for result in ["success", "failure", "skipped", "cancelled"] {
            let mut needs = serde_json::Map::new();
            needs.insert("plan".to_owned(), serde_json::json!({"result": "success"}));
            if ir.emits_velnor_lane_admission() {
                needs.insert(
                    "velnor-lane-admission".to_owned(),
                    serde_json::json!({"result": "skipped"}),
                );
            }
            for caller in &callers {
                needs.insert(caller.job_id.clone(), serde_json::json!({"result": result}));
            }
            let mut command = Command::new("bash");
            command
                .args(["-euo", "pipefail", "-c", &script])
                .env("NEEDS_JSON", serde_json::Value::Object(needs).to_string())
                .env("SELECTED_UNITS", "rust")
                .env("LANE_ADMITTED_GITHUB", "true")
                .env("LANE_ADMITTED_VELNOR", "true")
                .env("LANE_ADMITTED_VELNOR_TRUSTED", "true");
            let output = must_ok(command.output(), "bash executes required gate fixture");
            assert_eq!(
                output.status.success(),
                result == "success",
                "selected caller result {result}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn tool_provisioning_renders_declared_prepared_tools() {
        let mut unit = rust_unit("rust", ".");
        unit.prepared_tools = vec![crate::primitives::prepared_tools::PreparedToolNeed {
            tool_id: "test-runner".to_owned(),
            authorized_producers: BTreeSet::from(["producer-job".to_owned()]),
            inputs_digest: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_owned(),
        }];
        let ir = owner_test_ir("example/provisioning", vec![unit.clone()]);
        let mut output = String::new();
        ir.render_tool_provisioning(&mut output, RunnerMode::Github, &unit, true);
        assert!(
            output.contains("Restore prepared tool test-runner"),
            "provisioning restores the declared tool"
        );
        assert!(
            output.contains("prepared-tool-v1-test-runner-"),
            "the restore key names the prepared-tool namespace"
        );
        assert!(
            output.contains(Pins::resolved().cache_restore),
            "the restore rides the reviewed pin table"
        );
        // Undeclared units render no prepared-tool steps on either lane.
        for lane in [RunnerMode::Github, RunnerMode::Velnor] {
            let bare = rust_unit("rust", ".");
            let mut output = String::new();
            ir.render_tool_provisioning(&mut output, lane, &bare, true);
            assert!(
                !output.contains("prepared-tool"),
                "undeclared provisioning on {lane:?} mentions no prepared tool"
            );
        }
    }

    fn prepared_need(
        tool: &str,
        digest: &str,
    ) -> crate::primitives::prepared_tools::PreparedToolNeed {
        crate::primitives::prepared_tools::PreparedToolNeed {
            tool_id: tool.to_owned(),
            authorized_producers: BTreeSet::from(["producer-job".to_owned()]),
            inputs_digest: digest.to_owned(),
        }
    }

    #[test]
    fn kind_reusable_unions_prepared_tool_needs_behind_member_gates() {
        const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut alpha = rust_unit("rust-alpha", "crates/alpha");
        alpha.prepared_tools = vec![prepared_need("test-runner", DIGEST_A)];
        let mut beta = rust_unit("rust-beta", "crates/beta");
        beta.prepared_tools = vec![prepared_need("test-runner", DIGEST_B)];
        let ir = owner_test_ir("example/kind-tools", vec![alpha.clone(), beta.clone()]);
        let kind = ir.render_kind_units(UnitKind::Rust, None);
        // The header declares the input, and each distinct need renders one
        // gated block: the caller's records decide which block runs.
        assert!(
            kind.contains("prepared_tools:"),
            "the kind header declares the prepared-tools input"
        );
        assert_eq!(
            kind.matches("Restore prepared tool test-runner").count(),
            4,
            "each distinct need renders one block per lane"
        );
        assert!(
            kind.contains("contains(format(',{0},', inputs.prepared_tools)"),
            "partial member records render behind membership gates"
        );
        // The callers pass their own records through the shared input.
        for (unit, digest) in [(&alpha, DIGEST_A), (&beta, DIGEST_B)] {
            let facts = ir.unit_lane_facts(
                unit,
                &ir.default_unit_contract(unit, true),
                RunnerMode::Github,
            );
            let values = facts.input_values();
            let passed = values
                .iter()
                .find(|(name, _)| *name == lane_input::PREPARED_TOOLS);
            assert_eq!(
                passed.map(|(_, value)| value.as_str()),
                Some(format!("test-runner:{digest}:producer-job").as_str()),
                "the caller passes its own need record"
            );
        }
        // Members that agree share one ungated block.
        let mut gamma = rust_unit("rust-gamma", "crates/gamma");
        gamma.prepared_tools = vec![prepared_need("test-runner", DIGEST_A)];
        let ir = owner_test_ir("example/kind-tools", vec![alpha, gamma]);
        let kind = ir.render_kind_units(UnitKind::Rust, None);
        assert_eq!(
            kind.matches("Restore prepared tool test-runner").count(),
            2,
            "identical needs share one block per lane"
        );
        assert!(
            !kind.contains("inputs.prepared_tools"),
            "a unanimous need needs no gate"
        );
    }

    #[test]
    fn kind_reusable_without_needs_declares_no_prepared_tools_input() {
        let unit = rust_unit("rust", ".");
        let ir = owner_test_ir("example/kind-tools", vec![unit]);
        let kind = ir.render_kind_units(UnitKind::Rust, None);
        assert!(
            !kind.contains("prepared_tool"),
            "an undeclared kind mentions no prepared tools"
        );
    }

    #[test]
    fn collapsed_rust_members_gate_mbx_per_member() {
        let mbx_unit = rust_unit("rust-mbx", "crates/mbx");
        let mut disabled_unit = rust_unit("rust-disabled", "crates/disabled");
        disabled_unit.mbx = Some(false);
        let mut ir = owner_test_ir("example/mbx-gating", vec![mbx_unit, disabled_unit]);
        ir.mr_boxington = true;

        let kind = must_render_kind(&ir);
        assert_eq!(
            kind.matches("Set up Mr. Boxington").count(),
            2,
            "each lane keeps the MBX setup block"
        );
        assert!(
            kind.contains("if: ${{ inputs.mbx_enabled }}"),
            "mixed members gate MBX on the selected member"
        );
        assert!(
            kind.contains("if: ${{ inputs.mbx_enabled == false }}"),
            "mixed members gate the sccache alternative on the selected member"
        );
        assert!(
            kind.contains(
                "- name: Configure sccache environment\n        if: ${{ inputs.mbx_enabled == false }}"
            ),
            "mixed members gate the sccache environment on the selected member"
        );
        assert!(
            kind.contains(
                "          {\n            echo 'CARGO_INCREMENTAL=0'\n            echo 'RUSTC_WRAPPER=sccache'\n            echo 'SCCACHE_GHA_ENABLED=true'\n          } >> \"$GITHUB_ENV\""
            ),
            "sccache exports share one redirect so shellcheck accepts the generated block"
        );
        assert!(
            !kind.contains("echo 'CARGO_INCREMENTAL=0' >> \"$GITHUB_ENV\""),
            "sccache exports do not use individual redirects"
        );
        for assignment in [
            "CARGO_INCREMENTAL=0",
            "RUSTC_WRAPPER=sccache",
            "SCCACHE_GHA_ENABLED=true",
        ] {
            assert!(
                kind.contains(&format!("echo '{assignment}'")),
                "mixed sccache members export {assignment}: {kind}"
            );
        }
        assert!(
            kind.contains("hashFiles(inputs.mbx_dependency_files)"),
            "MBX members keep their per-member snapshot key inputs"
        );

        for (unit, enabled) in [(&ir.units[0], true), (&ir.units[1], false)] {
            for lane in [RunnerMode::Github, RunnerMode::Velnor] {
                let facts = ir.unit_lane_facts(unit, &ir.default_unit_contract(unit, true), lane);
                assert_eq!(facts.mbx_enabled, enabled, "{lane:?} / {}", unit.id);
                let values = facts.input_values();
                assert_eq!(
                    values.iter().any(|(name, value)| {
                        *name == lane_input::MBX_ENABLED && value == "true"
                    }),
                    enabled,
                    "typed MBX input for {lane:?} / {}",
                    unit.id
                );
                assert_eq!(
                    facts.mbx.is_some(),
                    lane == RunnerMode::Github && enabled,
                    "hosted snapshot facts for {lane:?} / {}",
                    unit.id
                );
            }
        }
    }

    #[test]
    fn collapsed_rust_all_mbx_keeps_ungated_mbx_setup() {
        let mut ir = owner_test_ir(
            "example/mbx-all",
            vec![
                rust_unit("rust-a", "crates/a"),
                rust_unit("rust-b", "crates/b"),
            ],
        );
        ir.mr_boxington = true;

        let kind = must_render_kind(&ir);
        assert_eq!(kind.matches("Set up Mr. Boxington").count(), 2);
        assert!(!kind.contains("if: ${{ inputs.mbx_enabled }}"));
        assert!(!kind.contains("if: ${{ inputs.mbx_enabled == false }}"));
        assert!(!kind.contains("Set up sccache"));
    }

    #[test]
    fn collapsed_rust_all_disabled_omits_mbx_and_keeps_sccache() {
        for global_mbx in [true, false] {
            let mut disabled = rust_unit("rust-disabled", "crates/disabled");
            disabled.mbx = Some(false);
            let mut ir = owner_test_ir("example/mbx-none", vec![disabled]);
            ir.mr_boxington = global_mbx;

            let kind = must_render_kind(&ir);
            assert!(!kind.contains("Set up Mr. Boxington"));
            assert_eq!(kind.matches("Set up sccache").count(), 1);
            assert_eq!(
                kind.matches("Configure sccache environment").count(),
                1,
                "the all-disabled job configures sccache once"
            );
            assert!(!kind.contains("if: ${{ inputs.mbx_enabled }}"));
            assert!(!kind.contains("if: ${{ inputs.mbx_enabled == false }}"));
            let facts = ir.unit_lane_facts(
                &ir.units[0],
                &ir.default_unit_contract(&ir.units[0], true),
                RunnerMode::Github,
            );
            assert!(!facts.mbx_enabled);
            assert!(facts.mbx.is_none());
            assert!(
                facts
                    .input_values()
                    .iter()
                    .all(|(name, _)| *name != lane_input::MBX_ENABLED),
                "disabled members pass no MBX input"
            );
        }
    }

    fn candidate_flagged_callers(ir: &WorkflowIr) -> Vec<String> {
        let mut flagged = Vec::new();
        for unit in &ir.units {
            for caller in ir.unit_lane_callers(unit, "ci-unit-rust.yml", None) {
                if caller
                    .inputs
                    .iter()
                    .any(|(name, value)| *name == lane_input::CANDIDATE_PUBLISH && value == "true")
                {
                    flagged.push(caller.job_id.clone());
                }
            }
        }
        flagged
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
    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn must_render_kind(ir: &WorkflowIr) -> String {
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "rust kind reusable renders",
        );
        must_some(rendered, "rust kind has members").1
    }

    #[test]
    fn collapsed_swift_kind_lands_on_macos_while_rust_stays_on_linux() {
        let mut swift = rust_unit("swift-package-native", "native");
        swift.kind = UnitKind::Swift;
        swift.label = "Swift package (native)".to_owned();
        // Apple-bound: Xcode SDK need, so the collapsed job must land on macOS.
        swift.platform = crate::platform::PlatformRequirement::apple_xcode();
        let ir = owner_test_ir(
            "example/fixture",
            vec![swift, rust_unit("rust-widget", "crates/widget")],
        );
        let swift_rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Swift, None),
            "swift kind reusable renders",
        );
        let swift_workflow = must_some(swift_rendered, "swift kind has members").1;
        assert!(
            swift_workflow.contains("runs-on: macos-15"),
            "{swift_workflow}"
        );
        assert!(
            !swift_workflow.contains("runs-on: ubuntu-24.04"),
            "{swift_workflow}"
        );
        let rust_rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "rust kind reusable renders",
        );
        let rust_workflow = must_some(rust_rendered, "rust kind has members").1;
        assert!(
            rust_workflow.contains("runs-on: ubuntu-24.04"),
            "{rust_workflow}"
        );
        assert!(
            !rust_workflow.contains("runs-on: macos-15"),
            "{rust_workflow}"
        );
    }

    #[test]
    fn candidate_publish_flags_only_the_hosted_owner_of_the_generator_crate() {
        let owner = workflow_setup_action_repository().to_owned();
        let mut documentation = rust_unit("docs-generator-root", "crates/velnor-workflow");
        documentation.kind = UnitKind::Docs;
        let units = vec![
            rust_unit("rust-generator-crate", "crates/velnor-workflow"),
            rust_unit("rust-sibling-crate", "crates/sibling"),
            rust_unit("rust-workspace-root", "."),
            documentation,
        ];
        let ir = owner_test_ir(&owner, units);
        for unit in &ir.units {
            for lane in [RunnerMode::Github, RunnerMode::Velnor] {
                let contract = ir.default_unit_contract(unit, false);
                let facts = ir.unit_lane_facts(unit, &contract, lane);
                assert_eq!(
                    facts.candidate_publish,
                    lane == RunnerMode::Github && unit_owns_workflow_crate(unit),
                    "candidate_publish for {} on {lane:?}",
                    unit.id
                );
            }
        }
        assert_eq!(
            ir.units
                .iter()
                .filter(|unit| unit_owns_workflow_crate(unit))
                .count(),
            1,
            "exactly one unit owns the generator crate"
        );

        // A consumer tree that happens to carry the same crate root still
        // publishes nothing: the owner repository check comes first.
        let foreign = owner_test_ir(
            "example/foreign",
            vec![rust_unit("rust-generator-crate", "crates/velnor-workflow")],
        );
        for unit in &foreign.units {
            for lane in [RunnerMode::Github, RunnerMode::Velnor] {
                let contract = foreign.default_unit_contract(unit, false);
                assert!(
                    !foreign
                        .unit_lane_facts(unit, &contract, lane)
                        .candidate_publish,
                    "foreign trees never publish on {lane:?}"
                );
            }
        }
        let anonymous = owner_test_ir(
            "",
            vec![rust_unit("rust-generator-crate", "crates/velnor-workflow")],
        );
        let contract = anonymous.default_unit_contract(&anonymous.units[0], false);
        assert!(
            !anonymous
                .unit_lane_facts(&anonymous.units[0], &contract, RunnerMode::Github)
                .candidate_publish,
            "an empty repository is not the owner"
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one implication pinned at facts, render, and caller level"
    )]
    #[test]
    fn velnor_lane_check_implies_policy_runtime() {
        const CHECK: &str = "cd -- 'crates/velnor-workflow' && mbx run -- --plain --check ../..";
        const OTHER: &str = "cargo test --locked";
        let owner = workflow_setup_action_repository().to_owned();
        // Facts level: the regen-gate command in any one of the six command
        // vectors implies the provision flag on the Velnor lane, never GitHub.
        let mut variants: Vec<(&str, Unit)> = Vec::new();
        let mut pr = rust_unit("rust-check", "crates/check");
        pr.pr_commands = vec![CHECK.to_owned()];
        pr.full_commands = vec![OTHER.to_owned()];
        variants.push(("pr_commands", pr));
        let mut full = rust_unit("rust-check", "crates/check");
        full.pr_commands = vec![OTHER.to_owned()];
        full.full_commands = vec![CHECK.to_owned()];
        variants.push(("full_commands", full));
        let mut github_pr = rust_unit("rust-check", "crates/check");
        github_pr.pr_commands = Vec::new();
        github_pr.full_commands = Vec::new();
        github_pr.github_pr_commands = Some(vec![CHECK.to_owned()]);
        variants.push(("github_pr_commands", github_pr));
        let mut github_full = rust_unit("rust-check", "crates/check");
        github_full.pr_commands = Vec::new();
        github_full.full_commands = Vec::new();
        github_full.github_full_commands = Some(vec![CHECK.to_owned()]);
        variants.push(("github_full_commands", github_full));
        let mut velnor_pr = rust_unit("rust-check", "crates/check");
        velnor_pr.pr_commands = Vec::new();
        velnor_pr.full_commands = Vec::new();
        velnor_pr.velnor_pr_commands = Some(vec![CHECK.to_owned()]);
        variants.push(("velnor_pr_commands", velnor_pr));
        let mut velnor_full = rust_unit("rust-check", "crates/check");
        velnor_full.pr_commands = Vec::new();
        velnor_full.full_commands = Vec::new();
        velnor_full.velnor_full_commands = Some(vec![CHECK.to_owned()]);
        variants.push(("velnor_full_commands", velnor_full));
        for (name, unit) in &variants {
            let ir = owner_test_ir(&owner, vec![unit.clone()]);
            let contract = ir.default_unit_contract(&ir.units[0], false);
            assert!(
                ir.unit_lane_facts(&ir.units[0], &contract, RunnerMode::Velnor)
                    .policy_runtime,
                "{name}: the Velnor lane provisions the pin for the regen gate"
            );
            assert!(
                !ir.unit_lane_facts(&ir.units[0], &contract, RunnerMode::Github)
                    .policy_runtime,
                "{name}: the hosted lane carries the pin in the Planning artifact instead"
            );
        }
        let plain = rust_unit("rust-plain", "crates/plain");
        let bare = owner_test_ir(&owner, vec![plain]);
        let contract = bare.default_unit_contract(&bare.units[0], false);
        for lane in [RunnerMode::Github, RunnerMode::Velnor] {
            assert!(
                !bare
                    .unit_lane_facts(&bare.units[0], &contract, lane)
                    .policy_runtime,
                "a unit without the regen gate provisions nothing on {lane:?}"
            );
        }
        // Render level: the collapsed Velnor lane job provisions the pinned
        // policy binary behind the input gate.
        let mut check = rust_unit("rust-check", "crates/check");
        check.pr_commands = vec![CHECK.to_owned()];
        let ir = owner_test_ir(&owner, vec![check, rust_unit("rust-plain", "crates/plain")]);
        let content = must_render_kind(&ir);
        let (hosted, velnor) = must_some(
            content.split_once("\n  verify-velnor:\n"),
            "both lane jobs render",
        );
        assert!(
            velnor.contains("      - name: Provision pinned Velnor workflow policy runtime\n        if: ${{ inputs.policy_runtime }}\n"),
            "verify-velnor provisions the pinned policy binary behind the input gate: {velnor}"
        );
        assert!(
            !hosted.contains("Provision pinned Velnor workflow policy runtime"),
            "the hosted lane carries the pin in the Planning artifact instead: {hosted}"
        );
        // Caller level: only the Velnor caller of the regen-gate unit passes
        // `policy_runtime: true`.
        let nodes = ir
            .units
            .iter()
            .map(|unit| GraphNode::Unit {
                unit_id: unit.id.clone(),
                job_id: stack_group_job_id(unit.kind),
                name: sidebar_group_name(unit),
                file: nested_unit_workflow_file(unit),
            })
            .collect::<Vec<_>>();
        for kind in [WorkflowKind::PullRequest, WorkflowKind::Main] {
            let rendered = ir.render_nested(kind, &nodes, None);
            assert_eq!(
                rendered.matches("policy_runtime: true").count(),
                1,
                "exactly the Velnor regen-gate caller passes the flag on {kind:?}: {rendered}"
            );
        }
        let pr = ir.render_nested(WorkflowKind::PullRequest, &nodes, None);
        let headers: Vec<String> = ir
            .units
            .iter()
            .flat_map(|unit| ir.unit_lane_callers(unit, "ci-unit-rust.yml", None))
            .map(|caller| format!("  {}:\n", caller.job_id))
            .collect();
        for unit in &ir.units {
            for caller in ir.unit_lane_callers(unit, "ci-unit-rust.yml", None) {
                if caller.lane != RunnerMode::Velnor {
                    continue;
                }
                let header = format!("  {}:\n", caller.job_id);
                let start = must_some(pr.find(&header), "the Velnor caller renders");
                let after = start + 1;
                let end = headers
                    .iter()
                    .filter(|other| *other != &header)
                    .filter_map(|other| {
                        pr[after..]
                            .find(&format!("\n{other}"))
                            .map(|index| after + index)
                    })
                    .min()
                    .unwrap_or(pr.len());
                let block = &pr[start..end];
                if unit.id == "rust-check" {
                    assert!(
                        block.contains("policy_runtime: true"),
                        "the Velnor regen-gate caller passes the flag: {block}"
                    );
                } else {
                    assert!(
                        !block.contains("policy_runtime: true"),
                        "a Velnor caller without the regen gate passes no flag: {block}"
                    );
                }
            }
        }
    }

    #[test]
    fn lane_facts_carry_dependency_closure_and_admission() {
        let mut leaf = rust_unit("rust-leaf", "crates/leaf");
        leaf.depends_on = vec!["rust-root".to_owned()];
        let mut trusted = rust_unit("rust-trusted-leaf", "crates/trusted");
        trusted.requires_trusted = true;
        let ir = owner_test_ir(
            "example/fixture",
            vec![
                rust_unit("rust-root", "crates/root"),
                leaf.clone(),
                trusted.clone(),
            ],
        );
        for (unit, lane, admission) in [
            (&leaf, RunnerMode::Github, LaneAdmission::Github),
            (&leaf, RunnerMode::Velnor, LaneAdmission::Velnor),
            (&trusted, RunnerMode::Velnor, LaneAdmission::VelnorTrusted),
        ] {
            let facts = ir.unit_lane_facts(unit, &ir.default_unit_contract(unit, true), lane);
            assert_eq!(facts.unit_dependencies, unit.depends_on, "{lane:?}");
            assert_eq!(facts.unit_admission, admission, "{lane:?}");
            let values = facts.input_values();
            assert!(
                values
                    .iter()
                    .any(|(name, value)| *name == lane_input::UNIT_ADMISSION
                        && value == admission.info_id()),
                "every caller passes its admission: {values:?}"
            );
        }
        let contract = ir.default_unit_contract(&leaf, true);
        let facts = ir.unit_lane_facts(&leaf, &contract, RunnerMode::Github);
        assert!(
            facts.input_values().iter().any(|(name, value)| {
                *name == lane_input::UNIT_DEPENDENCIES && value == "rust-root"
            }),
            "a unit with dependencies passes them: {:?}",
            facts.input_values()
        );
        let root = &ir.units[0];
        let facts = ir.unit_lane_facts(
            root,
            &ir.default_unit_contract(root, true),
            RunnerMode::Github,
        );
        assert!(
            facts
                .input_values()
                .iter()
                .all(|(name, _)| *name != lane_input::UNIT_DEPENDENCIES),
            "a unit without dependencies passes no closure: {:?}",
            facts.input_values()
        );
    }

    #[test]
    fn collapsed_callee_records_dependencies_from_inputs() {
        let mut leaf = rust_unit("rust-leaf", "crates/leaf");
        leaf.depends_on = vec!["rust-root".to_owned()];
        let ir = owner_test_ir(
            "example/fixture",
            vec![rust_unit("rust-root", "crates/root"), leaf],
        );
        let callee = must_render_kind(&ir);
        for declaration in [
            "      unit_dependencies:\n        required: false\n        type: string\n        default: \"\"\n",
            "      unit_admission:\n        required: false\n        type: string\n        default: \"\"\n",
        ] {
            assert!(callee.contains(declaration), "{callee}");
        }
        assert!(
            callee.contains("- name: Record unit dependencies\n"),
            "{callee}"
        );
        assert!(
            callee.contains("UNIT_DEPENDENCIES: ${{ inputs.unit_dependencies }}"),
            "{callee}"
        );
        assert!(
            callee.contains("UNIT_ADMISSION: ${{ inputs.unit_admission }}"),
            "{callee}"
        );
        assert!(
            !callee.contains("rust-root"),
            "the callee reads the closure through inputs, never as a literal: {callee}"
        );
    }

    #[test]
    fn exactly_one_caller_per_run_passes_candidate_publish() {
        let owner = workflow_setup_action_repository().to_owned();
        let ir = owner_test_ir(
            &owner,
            vec![
                rust_unit("rust-generator-crate", "crates/velnor-workflow"),
                rust_unit("rust-sibling-crate", "crates/sibling"),
            ],
        );
        assert_eq!(candidate_flagged_callers(&ir).len(), 1);

        let nodes = ir
            .units
            .iter()
            .map(|unit| GraphNode::Unit {
                unit_id: unit.id.clone(),
                job_id: stack_group_job_id(unit.kind),
                name: sidebar_group_name(unit),
                file: nested_unit_workflow_file(unit),
            })
            .collect::<Vec<_>>();
        let pr = ir.render_nested(WorkflowKind::PullRequest, &nodes, None);
        assert_eq!(
            pr.matches("candidate_publish: true").count(),
            1,
            "one publishing caller per run: {pr}"
        );
        let flagged = candidate_flagged_callers(&ir);
        let only = must_some(flagged.first(), "one caller passes the flag");
        assert!(
            pr.contains(&format!("  {only}:\n")),
            "the publishing caller renders: {pr}"
        );
    }

    #[test]
    fn no_lane_fetches_pin_history_the_tool_self_fetches() {
        let owner = workflow_setup_action_repository().to_owned();
        let mut checker = rust_unit("rust-generator-crate", "crates/velnor-workflow");
        checker
            .pr_commands
            .push("mbx run --locked -- --plain --check ../..".to_owned());
        let ir = owner_test_ir(
            &owner,
            vec![checker, rust_unit("rust-sibling-crate", "crates/sibling")],
        );
        let content = must_render_kind(&ir);
        let (hosted, velnor) = must_some(
            content.split_once("\n  verify-velnor:\n"),
            "both lane jobs render",
        );
        assert!(
            !hosted.contains("Fetch D19 pin history"),
            "the tool fetches the pin itself; the GitHub lane emits no fetch: {hosted}"
        );
        assert!(
            !velnor.contains("Fetch D19 pin history"),
            "the tool fetches the pin itself; the Velnor lane emits no fetch: {velnor}"
        );
        assert!(
            hosted.contains("- name: Run unit checks"),
            "verification still renders: {hosted}"
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one consumer contract pinned clause by clause"
    )]
    #[test]
    fn candidate_steps_match_the_policy_consumer_contract() {
        let owner = workflow_setup_action_repository().to_owned();
        let ir = owner_test_ir(
            &owner,
            vec![
                rust_unit("rust-generator-crate", "crates/velnor-workflow"),
                rust_unit("rust-sibling-crate", "crates/sibling"),
            ],
        );
        let content = must_render_kind(&ir);
        assert!(
            content.contains(
                "      candidate_publish:\n        required: false\n        type: boolean\n        default: false\n"
            ),
            "the callee declares the flag: {content}"
        );
        let (hosted, velnor) = must_some(
            content.split_once("\n  verify-velnor:\n"),
            "both lane jobs render",
        );
        assert!(
            !velnor.contains("candidate generator product"),
            "the Velnor lane never publishes: {velnor}"
        );
        let checks = must_some(hosted.find("- name: Run unit checks"), "checks render");
        let start = must_some(
            hosted.find("      - name: Prepare candidate generator product"),
            "prepare step renders",
        );
        assert!(
            checks < start,
            "packaging reuses the checks' own build, after it: {hosted}"
        );
        let end = must_some(
            hosted.find("      - name: Report phase timings"),
            "report step renders",
        );
        let candidate = &hosted[start..end];
        assert!(
            hosted.contains(
                "if: ${{ inputs.candidate_publish && (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository) }}"
            ),
            "the prepare step carries the input gate merged with the pull-request same-repo gate: {hosted}"
        );
        assert!(
            hosted.contains(
                "if: ${{ inputs.candidate_publish && (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate.outcome == 'success') }}"
            ),
            "the publish step additionally gates on the prepare step's skip output: {hosted}"
        );
        assert!(
            candidate.contains("CANDIDATE_MERGE_SHA: ${{ inputs.head_sha }}"),
            "the merge SHA names the checked-out tree: {candidate}"
        );
        assert!(
            candidate.contains("CANDIDATE_PR_HEAD_SHA: ${{ github.event.pull_request.head.sha }}"),
            "the PR-head SHA anchors the published identity: {candidate}"
        );
        assert!(
            candidate.contains("CANDIDATE_BASE_SHA: ${{ inputs.base_sha }}"),
            "the base SHA anchors the skip gate: {candidate}"
        );
        assert!(
            !candidate.contains("CANDIDATE_HEAD_SHA"),
            "nothing keys off the merge tree alone anymore: {candidate}"
        );
        for sha in ["$PR_HEAD", "$BASE", "$base_pin"] {
            assert!(
                candidate.contains(&format!(
                    "git fetch --no-tags --depth 1 \"$GITHUB_SERVER_URL/$GITHUB_REPOSITORY\" \"{sha}\""
                )),
                "an unauthenticated shallow fetch pins {sha}: {candidate}"
            );
        }
        for probe in [
            "\"$PR_HEAD^{commit}\"",
            "\"$BASE^{commit}\"",
            "\"$base_pin^{commit}\"",
        ] {
            assert!(
                candidate.contains(&format!("git cat-file -e {probe}")),
                "the fetch is skipped when {probe} is already local: {candidate}"
            );
        }
        assert!(
            !candidate.contains("GH_TOKEN"),
            "fetches are unauthenticated reads of the public repository: {candidate}"
        );
        assert!(
            !candidate.contains("actions/checkout@"),
            "no second checkout: the job extends its own history with fetches: {candidate}"
        );
        assert!(
            candidate.contains("velnor-workflow closure --rev=\"$PR_HEAD\" --candidate"),
            "the head candidate closure names the artifact: {candidate}"
        );
        assert!(
            candidate.contains("velnor-workflow closure --rev=\"$base_pin\" --candidate"),
            "the base candidate closure feeds the skip gate: {candidate}"
        );
        assert!(
            candidate
                .contains("velnor-workflow closure --rev=\"$CANDIDATE_MERGE_SHA\" --candidate"),
            "the merge candidate closure selects the fast path: {candidate}"
        );
        assert!(
            candidate.contains("git show \"$BASE:.github-gen/velnor-workflow.toml\""),
            "the base pin is read from the base tree: {candidate}"
        );
        assert!(
            candidate.contains("git show \"$BASE:.github/workflows/ci-policy.yml\""),
            "the entrypoint literal is the fallback pin source: {candidate}"
        );
        assert!(
            candidate.contains("echo \"skip=true\" >> \"$GITHUB_OUTPUT\""),
            "an unchanged generator skips the publish: {candidate}"
        );
        assert!(
            candidate.contains("test -x target/debug/velnor-workflow"),
            "the fast path reuses the job's own binary: {candidate}"
        );
        assert!(
            candidate.contains("binary=\"target/debug/velnor-workflow\"")
                && candidate.contains("build_rev=\"$CANDIDATE_MERGE_SHA\""),
            "the fast path records the merge tree as the build revision: {candidate}"
        );
        assert!(
            candidate.contains("use_unit_binary=false")
                && candidate.contains(
                    "if [[ \"$(target/debug/velnor-workflow --closure)\" == \"$head_closure\" ]]; then"
                )
                && candidate.contains("if [[ \"$use_unit_binary\" == true ]]; then"),
            "the fast path reuses the checks' binary only when it already reports the head closure (the checks may build wider features): {candidate}"
        );
        assert_eq!(
            candidate.matches("cargo build").count(),
            1,
            "the slow path builds exactly once: {candidate}"
        );
        assert!(
            candidate.contains("cargo build --locked -p velnor-workflow"),
            "the slow build pins the lockfile and the generator package: {candidate}"
        );
        assert!(
            candidate.contains("--manifest-path \"$worktree/crates/velnor-workflow/Cargo.toml\""),
            "the slow build compiles the head worktree: {candidate}"
        );
        assert!(
            !candidate.contains("--no-default-features")
                && !candidate.contains("--all-features")
                && !candidate.contains("cargo install"),
            "the slow build uses default features, the candidate feature set: {candidate}"
        );
        assert!(
            candidate.contains("git worktree add --detach \"$worktree\" \"$PR_HEAD\""),
            "the slow path checks the head out beside the job: {candidate}"
        );
        assert!(
            candidate.contains("trap 'git worktree remove --force \"$worktree\"' EXIT"),
            "the worktree is cleaned up on failure: {candidate}"
        );
        assert!(
            candidate.contains(
                "git worktree remove --force \"$worktree\"\n            trap - EXIT"
            ),
            "the explicit worktree removal disarms the EXIT trap so the step cannot double-remove: {candidate}"
        );
        assert!(
            candidate.contains("binary=\"$worktree/target/debug/velnor-workflow\"")
                && candidate.contains("build_rev=\"$PR_HEAD\""),
            "the slow path records the head as the build revision: {candidate}"
        );
        assert!(
            candidate.contains("\"$stage/velnor-workflow\" --closure")
                && candidate.contains("[[ \"$reported\" == \"$head_closure\" ]]"),
            "the staged binary proves the head closure before upload: {candidate}"
        );
        for argument in [
            "--arg profile debug",
            "--arg platform \"${RUNNER_OS}-${RUNNER_ARCH}\"",
            "--arg repository \"$GITHUB_REPOSITORY\"",
            "--arg run_id \"$GITHUB_RUN_ID\"",
            "--arg revision \"$PR_HEAD\"",
            "--arg closure \"$head_closure\"",
            "--arg build_revision \"$build_rev\"",
            "--arg binary_sha256 \"$digest\"",
        ] {
            assert!(
                candidate.contains(argument),
                "the manifest carries {argument}: {candidate}"
            );
        }
        assert!(
            candidate.contains("build_revision: $build_revision"),
            "the manifest object records the build revision: {candidate}"
        );
        assert!(
            candidate.contains(
                "echo \"name=velnor-workflow-candidate-${head_closure:0:16}-${RUNNER_OS}-${RUNNER_ARCH}\""
            ),
            "the artifact name derives exactly like the policy consumer's: {candidate}"
        );
        assert!(
            !candidate.contains("head_candidate"),
            "the merge-tree closure name is gone: {candidate}"
        );
        assert!(
            candidate.contains("actions/upload-artifact@"),
            "the publish step uses the pinned upload action: {candidate}"
        );
        assert!(
            candidate.contains("name: ${{ steps.candidate.outputs.name }}")
                && candidate.contains("path: ${{ runner.temp }}/velnor-workflow-candidate")
                && candidate.contains("retention-days: 1"),
            "the publish step uploads the staged directory with short retention: {candidate}"
        );
    }

    #[test]
    fn solo_kind_keeps_the_bare_event_gate() {
        // A kind with no sibling keeps the bare event gate: every member
        // publishes, so no input gate is needed.
        let owner = workflow_setup_action_repository().to_owned();
        let solo = owner_test_ir(
            &owner,
            vec![rust_unit("rust-generator-crate", "crates/velnor-workflow")],
        );
        let solo_content = must_render_kind(&solo);
        assert_eq!(
            solo_content
                .matches("if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository\n")
                .count(),
            3,
            "phase start and prepare keep the bare pull-request same-repo gate: {solo_content}"
        );
        assert_eq!(
            solo_content
                .matches("if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate.outcome == 'success'\n")
                .count(),
            2,
            "phase start and publish add the skip gate: {solo_content}"
        );
        assert!(
            !solo_content.contains("inputs.candidate_publish &&"),
            "no presence gate when every member publishes: {solo_content}"
        );
    }

    #[test]
    fn candidate_step_gates_sit_directly_after_their_name_lines() {
        // The presence-gate combiner only merges an `if:` it finds directly
        // after the step's `name:` line.
        let steps = super::candidate_publish_steps("actions/upload-artifact@pinned");
        assert!(
            steps.contains("      - name: Prepare candidate generator product\n        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository\n"),
            "the prepare gate is the pull-request same-repo gate: {steps}"
        );
        assert!(
            steps.contains("      - name: Publish candidate generator product\n        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate.outcome == 'success'\n"),
            "the publish gate adds the skip output: {steps}"
        );
        for marker in [
            "Mark candidate preparation start",
            "Mark candidate preparation end",
            "Mark candidate publication start",
            "Mark candidate publication end",
        ] {
            assert!(
                steps.contains(marker),
                "candidate phase marker missing: {marker}: {steps}"
            );
        }
        assert!(steps.contains("id: candidate_publication"));
        assert!(steps.contains(
            "Mark candidate preparation end\n        if: always() && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outcome == 'success'",
        ));
        assert!(steps.contains(
            "Mark candidate publication end\n        if: always() && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate_publication.outcome == 'success'",
        ));
        assert!(
            !steps.contains("Mark candidate preparation start\n        if: always()")
                && !steps.contains("Mark candidate publication start\n        if: always()"),
            "failed checks cannot start candidate timing markers: {steps}"
        );
    }

    #[test]
    fn foreign_trees_render_no_candidate_surface() {
        let ir = owner_test_ir(
            "example/foreign",
            vec![
                rust_unit("rust-generator-crate", "crates/velnor-workflow"),
                rust_unit("rust-sibling-crate", "crates/sibling"),
            ],
        );
        let content = must_render_kind(&ir);
        assert!(
            !content.contains("candidate generator product")
                && !content.contains("inputs.candidate_publish"),
            "foreign trees render no candidate steps or gates: {content}"
        );
        assert!(
            content.contains(
                "      candidate_publish:\n        required: false\n        type: boolean\n        default: false\n"
            ),
            "the input declaration stays for every kind: {content}"
        );
        assert!(
            candidate_flagged_callers(&ir).is_empty(),
            "no caller passes the flag"
        );
    }
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

/// Lockfile-scoped inputs for the shared Cargo source bundle cache (RC-11).
/// Workspace members share one key; independent manifest trees keep their own
/// lockfile path.
pub(crate) fn cargo_source_lockfile_key_files(unit: &Unit) -> Vec<String> {
    let lockfile = cargo_lockfile_root(unit);
    let lockfile_key = if lockfile == "." {
        "Cargo.lock".to_owned()
    } else {
        format!("{lockfile}/Cargo.lock")
    };
    vec![
        ".cargo/**".to_owned(),
        lockfile_key,
        "rust-toolchain.toml".to_owned(),
        "rust-toolchain".to_owned(),
    ]
}

/// The files a unit's dependency-bundle cache key hashes, in `hashFiles`
/// argument order.
fn cargo_source_cache_key_files(unit: &Unit) -> Vec<String> {
    unit.cache
        .as_ref()
        .filter(|cache| cache.purpose == CachePurpose::CargoSources)
        .map_or_else(
            || unit.cache.as_ref().map(|cache| cache.key_files.clone()),
            |_| Some(cargo_source_lockfile_key_files(unit)),
        )
        .unwrap_or_default()
}

fn cargo_source_cache_hash_expression(unit: &Unit) -> String {
    cargo_source_cache_key_files(unit)
        .iter()
        .map(|path| format!("'{}'", path.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_cargo_bundle_cache_key(id_segment: &str, hash_expression: &str) -> (String, String) {
    let cache_key = format!(
        "ci-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{id_segment}-${{{{ hashFiles({hash_expression}) }}}}"
    );
    let restore_prefix = format!("ci-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{id_segment}-");
    (cache_key, restore_prefix)
}

/// Source-state inputs a unit snapshot hashes into its freshness segment: the
/// compiled sources of the dependency closure plus every watched file a build
/// embeds or copies (a Rust unit's `include_str!` data, a Docker unit's build
/// context). Manifests, lockfiles, toolchain pins, and Cargo configuration are
/// dependency inputs and ride the other segment; hashing them again here would
/// cold-restart the closure on a metadata-only edit without advancing state.
fn snapshot_state_files(members: &[&Unit], unit: &Unit) -> Vec<String> {
    if !cargo_network_is_restricted(unit) {
        let mut files = vec!["Cargo.lock".to_owned(), "deny.toml".to_owned()];
        for watched in &unit.watch {
            if watched.ends_with("deny.toml") || watched.ends_with("audit.toml") {
                files.push(watched.clone());
            }
        }
        files.sort();
        files.dedup();
        return files;
    }
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
    let facts = unit_snapshot_facts(ir, unit);
    let dependency = freshness_expression(&facts.dependency_files);
    let class_prefix = snapshot_class_prefix(namespace, &facts.compatibility);
    let state = freshness_expression(&facts.state_files);
    (
        snapshot_key(&class_prefix, &unit.id, &dependency, &state),
        snapshot_restore_keys(&class_prefix, &unit.id, &dependency),
    )
}

/// The three unit-specific parts of a snapshot key, for callers that pass
/// them through `workflow_call` inputs instead of rendering the literal key.
fn unit_snapshot_facts(ir: &WorkflowIr, unit: &Unit) -> SnapshotFacts {
    let members = closure_members(unit, &ir.units);
    let dependency_files = snapshot_dependency_inputs(&members);
    let compatibility = snapshot_compatibility(ir, unit, &dependency_files).digest();
    SnapshotFacts {
        compatibility,
        dependency_files,
        state_files: snapshot_state_files(&members, unit),
    }
}

/// The snapshot key and restore prefixes a collapsed lane job renders: the
/// same segment grammar as [`unit_snapshot`], with the unit id, compatibility
/// digest, and both `hashFiles` lists read from the caller's inputs.
fn input_snapshot(
    namespace: &str,
    compat_input: &str,
    dependency_input: &str,
    freshness_input: &str,
) -> (String, String) {
    let class_prefix = snapshot_class_prefix(namespace, &lane_input::expression(compat_input));
    let unit = lane_input::expression("unit");
    let dependency = lane_input::hash_files_expression(dependency_input);
    let state = lane_input::hash_files_expression(freshness_input);
    (
        snapshot_key(&class_prefix, &unit, &dependency, &state),
        snapshot_restore_keys(&class_prefix, &unit, &dependency),
    )
}

const VELNOR_CI_TIMING_DIR: &str =
    "$RUNNER_TEMP/velnor-ci-timing-${GITHUB_RUN_ID:-unknown}-${GITHUB_RUN_ATTEMPT:-0}-${GITHUB_JOB:-unknown}";

fn render_epoch_marker_commands(epoch_key: &str, indent: &str) -> String {
    format!(
        "{indent}timing_dir=\"{VELNOR_CI_TIMING_DIR}\"\n\
         {indent}umask 077\n\
         {indent}mkdir -p \"$timing_dir\"\n\
         {indent}marker=\"$timing_dir/{epoch_key}\"\n\
         {indent}if [[ ! -e \"$marker\" ]]; then\n\
         {indent}  if (set -C; printf '%s\\n' \"$(date +%s)\" > \"$marker\") 2>/dev/null; then\n\
         {indent}    chmod 0444 \"$marker\" 2>/dev/null || true\n\
         {indent}  fi\n\
         {indent}fi"
    )
}

/// Append one immutable file marker step for CI phase timing.
fn render_phase_epoch_marker_if(
    output: &mut String,
    epoch_key: &str,
    step_name: &str,
    condition: &str,
) {
    let marker = render_epoch_marker_commands(epoch_key, "          ");
    let _ = writeln!(
        output,
        "      - name: {step_name}\n        if: {condition}\n        run: |\n{marker}"
    );
}

fn render_phase_epoch_marker(output: &mut String, epoch_key: &str, step_name: &str) {
    render_phase_epoch_marker_if(output, epoch_key, step_name, "always()");
}

fn render_phase_epoch_marker_text(epoch_key: &str, step_name: &str, condition: &str) -> String {
    let mut output = String::new();
    render_phase_epoch_marker_if(&mut output, epoch_key, step_name, condition);
    output
}

fn cache_save_completion_gate(save_gate: &str, step_id: &str) -> String {
    format!("always() && ({save_gate}) && steps.{step_id}.outcome == 'success'")
}

fn render_cache_save_start_marker(output: &mut String, kind: &str, save_gate: &str) {
    render_phase_epoch_marker_if(
        output,
        &format!("CACHE_{kind}_SAVE_STARTED"),
        &format!("Mark {kind} cache-save start"),
        save_gate,
    );
}

fn render_cache_save_end_marker(
    output: &mut String,
    kind: &str,
    save_gate: &str,
    save_step_id: &str,
) {
    let completion_gate = cache_save_completion_gate(save_gate, save_step_id);
    render_phase_epoch_marker_if(
        output,
        &format!("CACHE_{kind}_SAVE_ENDED"),
        &format!("Mark {kind} cache-save end"),
        &completion_gate,
    );
}

/// Mark the start of a unit job before checkout.
pub(crate) fn render_ci_job_started_marker(output: &mut String) {
    render_phase_epoch_marker(output, "JOB_STARTED", "Mark CI job start");
}

pub(crate) fn render_ci_runner_setup_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "RUNNER_SETUP_ENDED", "Mark runner setup end");
}

pub(crate) fn render_ci_selection_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "SELECTION_ENDED", "Mark selection transport end");
}

pub(crate) fn render_ci_tool_bootstrap_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "TOOL_BOOTSTRAP_ENDED", "Mark tool bootstrap end");
}

pub(crate) fn render_ci_cache_prep_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "CACHE_PREP_ENDED", "Mark cache prep end");
}

pub(crate) fn render_ci_cargo_fetch_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "CARGO_FETCH_ENDED", "Mark cargo fetch end");
}

/// Emit the post-checks report step shared by both render paths.
///
/// `job_display_name` is the Actions API job name for this job (the workflow
/// `jobs.<id>.name` value), which queue-time lookup matches on exactly.
pub(crate) fn render_phase_report_step(
    output: &mut String,
    report_action_uses: &str,
    job_display_name: &str,
    lane: RunnerMode,
    facts: &CacheReportFacts,
) {
    output.push_str("      - name: Report phase timings and cache outcomes\n");
    output.push_str("        if: always()\n");
    let _ = writeln!(
        output,
        "        uses: {report_action_uses}\n        with:\n          job_label: {job_display_name}"
    );
    render_cache_outcome_report_inputs(output, lane, facts);
}

/// One cache layer whose restore step outputs the post-checks report reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReportedCacheLayer {
    Rustup,
    Mold,
    Mbx,
    CargoBundle,
    DockerSeed,
}

impl ReportedCacheLayer {
    /// The step id whose outputs carry the layer's primary and matched keys.
    fn step_id(self) -> &'static str {
        match self {
            Self::Rustup => "rustup-toolchain",
            Self::Mold => "mold-cache",
            Self::Mbx => "mbx-cache",
            Self::CargoBundle | Self::DockerSeed => "cache",
        }
    }

    /// Snake-case input prefix for the report composite action.
    fn report_input_prefix(self) -> &'static str {
        match self {
            Self::Rustup => "rustup",
            Self::Mold => "mold",
            Self::Mbx => "mbx",
            Self::CargoBundle => "cargo",
            Self::DockerSeed => "docker_seed",
        }
    }
}

/// Which cache layers the post-checks report reads step outputs for, and the
/// host-warm layers a Velnor job declares. Derived from one unit for a literal
/// job, or unioned over a kind's members for a collapsed lane job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheReportFacts {
    pub(crate) layers: BTreeSet<ReportedCacheLayer>,
    /// The rendered `VELNOR_HOST_WARM_LAYERS` value on the Velnor lane.
    pub(crate) host_warm_layers: Option<String>,
}

impl CacheReportFacts {
    pub(crate) fn for_unit(lane: RunnerMode, unit: &Unit, ir: &WorkflowIr) -> Self {
        let tools = WorkflowIr::tools_for_unit(unit, ir.mise_present, ir.mr_boxington);
        let seed = unit
            .cache
            .as_ref()
            .is_some_and(|cache| cache.mutable_mount_seed);
        if lane == RunnerMode::Velnor {
            let layers = velnor_host_warm_layers(unit, ir);
            return Self {
                host_warm_layers: (!layers.is_empty()).then(|| layers.join(",")),
                ..Self::default()
            };
        }
        let mut layers = BTreeSet::new();
        if !velnor_skips_pinned_rust_toolchain(lane) && unit.toolchain.is_some() {
            layers.insert(ReportedCacheLayer::Rustup);
        }
        if tools.contains(&ToolRequirement::Mold) {
            layers.insert(ReportedCacheLayer::Mold);
        }
        if tools.contains(&ToolRequirement::MrBoxington) {
            layers.insert(ReportedCacheLayer::Mbx);
        }
        if unit.cache.as_ref().is_some_and(|cache| {
            !cache.mutable_mount_seed
                && CacheBackend::Detected.lane_enables_actions_cache(lane, ir, unit)
        }) {
            layers.insert(ReportedCacheLayer::CargoBundle);
        }
        if seed && lane == RunnerMode::Github {
            layers.insert(ReportedCacheLayer::DockerSeed);
        }
        Self {
            layers,
            host_warm_layers: None,
        }
    }

    /// The union over a collapsed lane job's members: a layer is reported
    /// when any member restores it (a skipped step reports empty outputs), and
    /// the host-warm list reads from its input when members differ.
    fn union(members: &[Self]) -> Self {
        let host_warm_layers = members
            .iter()
            .filter_map(|facts| facts.host_warm_layers.clone())
            .collect::<BTreeSet<_>>();
        let host_warm_layers = match host_warm_layers.len() {
            0 => None,
            1 if members.iter().all(|facts| facts.host_warm_layers.is_some()) => {
                host_warm_layers.into_iter().next()
            }
            _ => Some(lane_input::expression(lane_input::HOST_WARM_LAYERS)),
        };
        Self {
            layers: members
                .iter()
                .flat_map(|facts| facts.layers.iter().copied())
                .collect(),
            host_warm_layers,
        }
    }
}

/// The host-persistent layers a Velnor job expects warm on its runner.
fn velnor_host_warm_layers(unit: &Unit, ir: &WorkflowIr) -> Vec<&'static str> {
    let tools = WorkflowIr::tools_for_unit(unit, ir.mise_present, ir.mr_boxington);
    let mut host_warm = Vec::<&'static str>::new();
    if velnor_skips_pinned_rust_toolchain(RunnerMode::Velnor) && unit.toolchain.is_some() {
        host_warm.push("rustup");
    }
    if tools.contains(&ToolRequirement::Mold) {
        host_warm.push("mold");
    }
    if tools.contains(&ToolRequirement::MrBoxington) {
        host_warm.push("mbx");
    }
    if unit
        .cache
        .as_ref()
        .is_some_and(cache_is_velnor_host_persistent)
    {
        host_warm.push("cargo");
    }
    if unit
        .cache
        .as_ref()
        .is_some_and(|cache| cache.mutable_mount_seed)
    {
        host_warm.push("docker_seed");
    }
    host_warm
}

fn render_cache_outcome_report_inputs(
    output: &mut String,
    lane: RunnerMode,
    facts: &CacheReportFacts,
) {
    let lane_name = match lane {
        RunnerMode::Github | RunnerMode::Both => "github",
        RunnerMode::Velnor => "velnor",
    };
    let _ = writeln!(output, "          ci_lane: {lane_name}");
    if lane == RunnerMode::Velnor {
        if let Some(layers) = &facts.host_warm_layers {
            let _ = writeln!(output, "          host_warm_layers: {layers}");
        }
        return;
    }
    for layer in &facts.layers {
        let input_prefix = layer.report_input_prefix();
        let step_id = layer.step_id();
        if *layer == ReportedCacheLayer::Mbx {
            let _ = writeln!(
                output,
                "          cache_mbx_hit: {}",
                step_output_expr(step_id, "cache-hit")
            );
            let _ = writeln!(
                output,
                "          cache_mbx_primary: {}",
                step_output_expr(step_id, "cache-primary-key")
            );
            continue;
        }
        let _ = writeln!(
            output,
            "          cache_{input_prefix}_primary: {}",
            step_output_expr(step_id, "cache-primary-key")
        );
        let _ = writeln!(
            output,
            "          cache_{input_prefix}_matched: {}",
            step_output_expr(step_id, "cache-matched-key")
        );
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

fn mise_auto_install_env() -> &'static str {
    "\n          MISE_AUTO_INSTALL: \"false\"\n          MISE_EXEC_AUTO_INSTALL: \"false\"\n          MISE_NOT_FOUND_AUTO_INSTALL: \"false\""
}

/// Env for the steps that run Cargo and repository commands, on both lanes:
/// the `CARGO_NET_OFFLINE` restriction for lockfile-pinned units, and the
/// suppression of every mise auto-install path. Verification must consume only
/// what the provision steps put in place; a task runner that silently installs
/// a missing tool would widen each job's dependency surface and re-introduce
/// the whole-configured-toolset materialisation a minimal provision list
/// exists to prevent.
pub(crate) fn checks_env(unit: &Unit) -> String {
    checks_env_for_members(unit, &[unit])
}

fn checks_env_for_members(unit: &Unit, members: &[&Unit]) -> String {
    let mut env = String::new();
    let restricted = members
        .iter()
        .copied()
        .filter(|member| cargo_network_is_restricted(member))
        .count();
    // Mixed kind groups cannot bake CARGO_NET_OFFLINE: deny/audit members
    // resolve advisory databases on the network. Restricted members export
    // it from the run script instead.
    if restricted == members.len() && cargo_network_is_restricted(unit) {
        env.push_str("\n          CARGO_NET_OFFLINE: \"true\"");
    }
    env.push_str(mise_auto_install_env());
    env
}

/// The checks-step token a GitHub-lane Docker job exports: its hosted image
/// build commands pass `--secret id=github_token,env=GITHUB_TOKEN`, so the
/// step must provide the automatic token. Other lanes and kinds run no such
/// command and get no token.
fn docker_build_token_env_for_members(lane: RunnerMode, members: &[&Unit]) -> &'static str {
    if lane == RunnerMode::Github && members.iter().any(|unit| unit.kind == UnitKind::Docker) {
        "\n          GITHUB_TOKEN: ${{ github.token }}"
    } else {
        ""
    }
}

/// The checks env of a collapsed lane job. `CARGO_NET_OFFLINE` is a literal
/// when every member agrees, and the `cargo_net_offline` input (which
/// defaults to `false`, Cargo's own default) when members differ.
fn collapsed_checks_env(offline: FeatureCoverage) -> String {
    let mut env = String::new();
    if offline.all {
        env.push_str("\n          CARGO_NET_OFFLINE: \"true\"");
    } else if offline.any {
        let _ = write!(
            env,
            "\n          CARGO_NET_OFFLINE: {}",
            lane_input::expression(lane_input::CARGO_NET_OFFLINE)
        );
    }
    env.push_str(mise_auto_install_env());
    env
}

/// Export the sccache defaults after setup for the selected member. Collapsed
/// jobs cannot use [`render_job_env`] because one job serves members with
/// mutually exclusive MBX and sccache transports; `GITHUB_ENV` carries the
/// selected member's values to every later command without assigning empty
/// wrapper variables to MBX members.
fn render_collapsed_sccache_env_step(
    output: &mut String,
    sccache: FeatureCoverage,
    mbx_input: &str,
) {
    if !sccache.any {
        return;
    }
    let block = "      - name: Configure sccache environment\n        run: |\n          {\n            echo 'CARGO_INCREMENTAL=0'\n            echo 'RUSTC_WRAPPER=sccache'\n            echo 'SCCACHE_GHA_ENABLED=true'\n          } >> \"$GITHUB_ENV\"\n";
    output.push_str(&prefix_step_block_with_if(
        block,
        sccache.absent_gate(mbx_input).as_deref(),
    ));
}

fn cargo_offline_run_prelude(members: &[&Unit]) -> String {
    let restricted = members
        .iter()
        .copied()
        .filter(|member| cargo_network_is_restricted(member))
        .collect::<Vec<_>>();
    if restricted.is_empty() || restricted.len() == members.len() {
        return String::new();
    }
    let pattern = restricted
        .iter()
        .map(|member| crate::shell_quote(&member.id))
        .collect::<Vec<_>>()
        .join(" | ");
    format!(
        "          case \"$CI_UNIT_ID\" in\n            {pattern}) export CARGO_NET_OFFLINE=true ;;\n          esac\n"
    )
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

/// Whether the unit runs the generator's own regeneration gate
/// (`--plain --check`), whose D19 guard needs the pinned policy binary while
/// the checks run with the network restricted.
pub(crate) fn unit_runs_workflow_plain_check(unit: &Unit) -> bool {
    unit_commands(unit).any(|command| command.contains("--plain --check"))
}

/// Whether the unit owns the generator crate itself: a Rust unit rooted at
/// the generator crate's manifest directory. The scan mints one unit per
/// manifest root, so at most one unit per repository matches; the owner
/// repository's tree carries exactly that unit, and fixture trees carry
/// none. The match is structural (kind plus root), never the unit id, so a
/// renamed unit cannot silently gain or lose the publish.
fn unit_owns_workflow_crate(unit: &Unit) -> bool {
    unit.kind == UnitKind::Rust && unit.root == "crates/velnor-workflow"
}

/// Stage-1 candidate packaging steps for the collapsed hosted lane job,
/// head-anchored: everything keys off the pull-request head tree, the same
/// identity the owner policy run waits for and verifies.
///
/// The unit job checks out the merge commit, but the policy consumer waits
/// for an artifact named by the audited head's candidate closure and verifies
/// the audited PR-head tree's closure. Naming the artifact from the merge
/// tree flakes whenever main advances in closure paths (rebase/merge-state
/// decides pass/fail), so the prepare step fetches the PR head, the base,
/// and the base pin (shallow, unauthenticated, each skipped when already
/// local) and computes all closures from those.
///
/// The step skips the publish (`skip=true`, exit 0) when the head candidate
/// closure equals the base pin's: no generator change, nothing to publish.
/// Otherwise the fast path reuses the checks' own `target/debug/velnor-workflow`
/// when the merge closure equals the head closure (zero builds), and the slow
/// path builds the generator once from an explicit head worktree in this same
/// step (same job, same toolchain, warm cargo home — a separate job would
/// duplicate checkout, toolchain, and cache for a rare event). Either way the
/// step stages the binary, proves it self-reports the head closure, and writes
/// the manifest the policy consumer verifies (`profile`, `platform`,
/// `repository`, `run_id`, `revision` = PR-head SHA, `closure` = head
/// candidate closure, `build_revision` = tree the binary compiled from,
/// `binary_sha256`). The publish step uploads both files under the name the
/// policy derives the same way (`velnor-workflow-candidate-<closure16>-<os>-<arch>`).
///
/// Both steps carry the pull-request same-repo gate directly after their name
/// line (the presence-gate combiner only merges an `if:` it finds there),
/// and the publish step additionally gates on the prepare step's `skip`
/// output, so the collapsed renderer can add the per-unit input gate without
/// touching them.
fn candidate_phase_marker_text() -> (String, String, String, String) {
    let event_gate =
        "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository";
    (
        render_phase_epoch_marker_text(
            "CANDIDATE_PREPARATION_STARTED",
            "Mark candidate preparation start",
            event_gate,
        ),
        render_phase_epoch_marker_text(
            "CANDIDATE_PREPARATION_ENDED",
            "Mark candidate preparation end",
            "always() && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outcome == 'success'",
        ),
        render_phase_epoch_marker_text(
            "CANDIDATE_PUBLICATION_STARTED",
            "Mark candidate publication start",
            "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate.outcome == 'success'",
        ),
        render_phase_epoch_marker_text(
            "CANDIDATE_PUBLICATION_ENDED",
            "Mark candidate publication end",
            "always() && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate_publication.outcome == 'success'",
        ),
    )
}

fn candidate_publish_steps(upload_artifact_pin: &str) -> String {
    let (preparation_start, preparation_end, publication_start, publication_end) =
        candidate_phase_marker_text();
    format!(
        r#"{preparation_start}      - name: Prepare candidate generator product
        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository
        id: candidate
        env:
          CANDIDATE_MERGE_SHA: ${{{{ inputs.head_sha }}}}
          CANDIDATE_PR_HEAD_SHA: ${{{{ github.event.pull_request.head.sha }}}}
          CANDIDATE_BASE_SHA: ${{{{ inputs.base_sha }}}}
        run: |
          set -euo pipefail
          PR_HEAD="$CANDIDATE_PR_HEAD_SHA"
          BASE="$CANDIDATE_BASE_SHA"
          if ! git cat-file -e "$PR_HEAD^{{commit}}" 2>/dev/null; then
            git fetch --no-tags --depth 1 "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY" "$PR_HEAD"
          fi
          head_closure="$(velnor-workflow closure --rev="$PR_HEAD" --candidate)"
          if ! git cat-file -e "$BASE^{{commit}}" 2>/dev/null; then
            git fetch --no-tags --depth 1 "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY" "$BASE"
          fi
          base_pin="$(git show "$BASE:.github-gen/velnor-workflow.toml" 2>/dev/null | sed -n -E 's/^[[:space:]]*revision[[:space:]]*=[[:space:]]*"([0-9a-f]{{40}})".*/\1/p' | head -n 1)"
          test "$base_pin" != '' || base_pin="$(git show "$BASE:.github/workflows/ci-policy.yml" 2>/dev/null | sed -n -E 's/^.*VELNOR_WORKFLOW_POLICY_REVISION:[[:space:]]*([0-9a-f]{{40}}).*/\1/p' | head -n 1)"
          test "$base_pin" != '' || {{ echo "::error::base $BASE declares no generator pin" >&2; exit 1; }}
          if ! git cat-file -e "$base_pin^{{commit}}" 2>/dev/null; then
            git fetch --no-tags --depth 1 "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY" "$base_pin"
          fi
          base_closure="$(velnor-workflow closure --rev="$base_pin" --candidate)"
          if [[ "$head_closure" == "$base_closure" ]]; then
            echo "head $PR_HEAD shares the base pin's candidate closure; no candidate to publish"
            echo "skip=true" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          merge_closure="$(velnor-workflow closure --rev="$CANDIDATE_MERGE_SHA" --candidate)"
          worktree=""
          use_unit_binary=false
          if [[ "$merge_closure" == "$head_closure" ]]; then
            test -x target/debug/velnor-workflow || {{ echo "::error::candidate packaging needs the unit's own target/debug/velnor-workflow; the checks must build the generator binary" >&2; exit 1; }}
            # The checks may build with different features than the candidate
            # profile stamps, so only reuse their binary when it already
            # reports the head closure; otherwise fall through to a clean
            # default-features build below.
            if [[ "$(target/debug/velnor-workflow --closure)" == "$head_closure" ]]; then
              use_unit_binary=true
            fi
          fi
          if [[ "$use_unit_binary" == true ]]; then
            binary="target/debug/velnor-workflow"
            build_rev="$CANDIDATE_MERGE_SHA"
          else
            worktree="$RUNNER_TEMP/velnor-workflow-head"
            rm -rf "$worktree"
            git worktree add --detach "$worktree" "$PR_HEAD"
            trap 'git worktree remove --force "$worktree"' EXIT
            cargo build --locked -p velnor-workflow --manifest-path "$worktree/crates/velnor-workflow/Cargo.toml"
            binary="$worktree/target/debug/velnor-workflow"
            build_rev="$PR_HEAD"
          fi
          stage="$RUNNER_TEMP/velnor-workflow-candidate"
          rm -rf "$stage"
          mkdir -p "$stage"
          install -m 0755 "$binary" "$stage/velnor-workflow"
          digest="$(sha256sum "$stage/velnor-workflow" | awk '{{print $1}}')"
          reported="$("$stage/velnor-workflow" --closure)"
          [[ "$reported" == "$head_closure" ]] || {{ echo "::error::candidate reports closure $reported, head $PR_HEAD declares $head_closure" >&2; exit 1; }}
          jq -n --arg profile debug --arg platform "${{RUNNER_OS}}-${{RUNNER_ARCH}}" --arg repository "$GITHUB_REPOSITORY" --arg run_id "$GITHUB_RUN_ID" --arg revision "$PR_HEAD" --arg closure "$head_closure" --arg build_revision "$build_rev" --arg binary_sha256 "$digest" '{{profile: $profile, platform: $platform, repository: $repository, run_id: $run_id, revision: $revision, closure: $closure, build_revision: $build_revision, binary_sha256: $binary_sha256}}' > "$stage/candidate-manifest.json"
          if [[ "$worktree" != "" ]]; then
            git worktree remove --force "$worktree"
            trap - EXIT
          fi
          echo "name=velnor-workflow-candidate-${{head_closure:0:16}}-${{RUNNER_OS}}-${{RUNNER_ARCH}}" >> "$GITHUB_OUTPUT"
{preparation_end}{publication_start}      - name: Publish candidate generator product
        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true' && steps.candidate.outcome == 'success'
        id: candidate_publication
        uses: {upload_artifact_pin}
        with:
          name: ${{{{ steps.candidate.outputs.name }}}}
          path: ${{{{ runner.temp }}}}/velnor-workflow-candidate
          if-no-files-found: error
          retention-days: 1
{publication_end}"#,
    )
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
/// run a hosted Cargo command: restore the cached `~/.rustup` keyed by the
/// repository's pin, install exactly that pin, and — when `save_gate` carries
/// the step's `if:` body — save the result for the next run. Image-backed
/// Velnor jobs intentionally bypass this helper because their pinned toolchain
fn step_output_expr(step_id: &str, field: &str) -> String {
    format!("${{{{ steps.{step_id}.outputs.{field} }}}}")
}

/// is part of the runner image.
pub(crate) fn render_pinned_toolchain_steps(
    output: &mut String,
    cache_restore: &str,
    cache_save: &str,
    toolchain: &RustToolchain,
    save_gate: Option<&str>,
) {
    let rustup_id = "rustup-toolchain";
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
        "      - name: Restore Rust toolchain\n        id: {rustup_id}\n        uses: {cache_restore}\n        with:\n          path: |\n{paths}\n          key: {key}"
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

/// Trusted GitHub Actions cache save gate for unit lanes: default-branch push,
/// schedule, and default-branch `workflow_dispatch`. Excludes every
/// `pull_request` variant and `merge_group` (D7, D8).
pub(crate) fn trusted_cache_save_expression(default_branch: &str) -> String {
    format!(
        "(github.event_name == 'push' && github.ref == 'refs/heads/{default_branch}') || \
         github.event_name == 'schedule' || \
         (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}

/// Push-only trusted save gate for release and preview surfaces that never
/// admit schedule or `workflow_dispatch` producers (RC-4 release family).
pub(crate) fn default_branch_push_cache_save_expression(default_branch: &str) -> String {
    format!("github.event_name == 'push' && github.ref == 'refs/heads/{default_branch}'")
}

/// Dependency-bundle save gate: refresh trusted bundles even when checks fail,
/// but never on an exact restore hit (RC-17).
pub(crate) fn dependency_bundle_cache_save_if(default_branch: &str) -> String {
    dependency_bundle_cache_save_if_for_step(default_branch, "cache")
}

pub(crate) fn dependency_bundle_cache_save_if_for_step(
    default_branch: &str,
    cache_step_id: &str,
) -> String {
    format!(
        "always() && ({}) && steps.{cache_step_id}.outputs.cache-hit != 'true'",
        trusted_cache_save_expression(default_branch)
    )
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
        push_mise_tool(&mut tools, nextest_tool_id(lock_keys).to_owned());
    }
    for declared in &unit.mise_tools {
        push_mise_tool(&mut tools, declared.clone());
    }
    tools
}

pub(crate) fn needs_cargo_deny(unit: &Unit) -> bool {
    unit_commands(unit)
        .any(|command| command.contains("cargo deny") || command.contains("mbx deny"))
}

/// Lock spellings for cargo-deny. Repositories and job images may pin either
/// the aqua prebuilt or the cargo backend; install args must match the lock.
pub(crate) const QUALIFIED_CARGO_DENY_TOOL: &str = "aqua:EmbarkStudios/cargo-deny";
pub(crate) const BARE_CARGO_DENY_TOOL: &str = "cargo:cargo-deny";

/// Resolve the cargo-deny tool id against the root lock keys.
pub(crate) fn cargo_deny_tool_id(lock_keys: &BTreeSet<String>) -> Option<String> {
    if lock_keys.contains(QUALIFIED_CARGO_DENY_TOOL) {
        Some(QUALIFIED_CARGO_DENY_TOOL.to_owned())
    } else if lock_keys.contains(BARE_CARGO_DENY_TOOL) {
        Some(BARE_CARGO_DENY_TOOL.to_owned())
    } else if lock_keys.is_empty() {
        Some(QUALIFIED_CARGO_DENY_TOOL.to_owned())
    } else {
        None
    }
}

/// Tool ids the Velnor lane installs explicitly. The job image already pins
/// common CI tools (Bun, `OpenTofu`, mold, Mr. Boxington); this list covers
/// only what a unit's commands or repo declarations pull from the root lock,
/// plus policy tools the hosted lane supplies through dedicated setup actions.
pub(crate) fn velnor_mise_install_tool_ids(
    unit: &Unit,
    lock_keys: &BTreeSet<String>,
) -> Vec<String> {
    let mut tools = mise_tool_ids(unit, lock_keys);
    if needs_cargo_deny(unit)
        && let Some(deny) = cargo_deny_tool_id(lock_keys)
    {
        push_mise_tool(&mut tools, deny);
    }
    tools
}

fn push_mise_tool(tools: &mut Vec<String>, tool: String) {
    if !tools.iter().any(|existing| existing == &tool) {
        tools.push(tool);
    }
}

fn render_velnor_mise_install(output: &mut String, unit: &Unit, lock_keys: &BTreeSet<String>) {
    let tools = velnor_mise_install_tool_ids(unit, lock_keys);
    if tools.is_empty() {
        return;
    }
    let _ = writeln!(
        output,
        "      - name: Install declared Mise tools\n        run: |\n          set -euo pipefail\n          mise --yes install {}",
        tools.join(" ")
    );
}

/// The Velnor-lane mise install for a collapsed lane job: the tool ids arrive
/// space-separated through the `mise_tools` input and are split as shell words.
fn render_velnor_mise_install_from_input(output: &mut String) {
    let _ = writeln!(
        output,
        "      - name: Install declared Mise tools\n        env:\n          MISE_TOOLS: {}\n        run: |\n          set -euo pipefail\n          read -ra tools <<<\"$MISE_TOOLS\"\n          mise --yes install \"${{tools[@]}}\"",
        lane_input::expression(lane_input::MISE_TOOLS)
    );
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

/// Fetch declared Cargo sources after cache restore and before verification.
///
/// When `runtime_unit_id` is false the rendered job's unit id is fixed at
/// generation time, so the step targets that unit alone. When true the id comes
/// from `workflow_call` inputs and the step carries the union case over every
/// restricted member.
pub(crate) fn render_cargo_source_preparation(
    output: &mut String,
    members: &[&Unit],
    unit_id: &str,
    runtime_unit_id: bool,
    skip_on_cache_hit: bool,
    skip_when_offline_ready: bool,
) {
    if !members.iter().any(|unit| cargo_network_is_restricted(unit)) {
        return;
    }
    let cache_hit_gate = if skip_on_cache_hit {
        "        if: ${{ steps.cache.outputs.cache-hit != 'true' }}\n"
    } else {
        ""
    };
    if !runtime_unit_id {
        let Some(active) = members.iter().find(|member| member.id == unit_id) else {
            return;
        };
        if !cargo_network_is_restricted(active) {
            return;
        }
        // A shell word, not a YAML scalar: the value lands inside `run:`.
        let root = crate::shell_quote(&active.root);
        let fetch_body = render_cargo_fetch_body(skip_when_offline_ready);
        let _ = write!(
            output,
            "      - name: Prepare Cargo sources\n{cache_hit_gate}        env:{}\n        run: |\n          set -euo pipefail\n          root={root}\n          if [[ \"$root\" != \".\" ]]; then\n            cd -- \"$root\"\n          fi\n{fetch_body}",
            preparation_env()
        );
        return;
    }
    let mut cases = String::new();
    for member in members {
        if cargo_network_is_restricted(member) {
            let _ = writeln!(
                cases,
                "            {}) root={} ;;",
                crate::shell_quote(&member.id),
                crate::shell_quote(&member.root)
            );
        } else {
            // deny/audit/publish resolve their own inputs; skip fetch.
            let _ = writeln!(
                cases,
                "            {}) exit 0 ;;",
                crate::shell_quote(&member.id)
            );
        }
    }
    let fetch_body = render_cargo_fetch_body(skip_when_offline_ready);
    let _ = write!(
        output,
        "      - name: Prepare Cargo sources\n{cache_hit_gate}        env:\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}{}\n        run: |\n          set -euo pipefail\n          case \"$CI_UNIT_ID\" in\n{cases}            *) echo \"unknown unit for cargo fetch: $CI_UNIT_ID\" >&2; exit 1 ;;\n          esac\n          if [[ \"$root\" != \".\" ]]; then\n            cd -- \"$root\"\n          fi\n{fetch_body}",
        preparation_env()
    );
}

fn render_cargo_fetch_body(skip_when_offline_ready: bool) -> String {
    if skip_when_offline_ready {
        "          if cargo metadata --locked --offline --all-features --format-version 1 >/dev/null 2>&1; then\n            echo \"Cargo sources warm; skipping fetch\"\n          else\n            cargo fetch --locked\n          fi\n".to_owned()
    } else {
        "          cargo fetch --locked\n".to_owned()
    }
}

/// The `cargo fetch --locked` step of a collapsed lane job: the manifest root
/// arrives through the `cargo_root` input, and the warm-store skip is either a
/// literal decision every member shares or a per-unit input the script reads.
fn render_cargo_source_preparation_from_input(
    output: &mut String,
    gate: Option<&str>,
    skip_when_warm: FeatureCoverage,
) {
    let mut env = format!(
        "\n          CARGO_FETCH_ROOT: {}",
        lane_input::expression(lane_input::CARGO_ROOT)
    );
    let fetch_body = if skip_when_warm.all {
        render_cargo_fetch_body(true)
    } else if skip_when_warm.any {
        let _ = write!(
            env,
            "\n          CARGO_FETCH_SKIP_WHEN_WARM: {}",
            lane_input::expression(lane_input::CARGO_FETCH_SKIP_WHEN_WARM)
        );
        "          if [[ \"$CARGO_FETCH_SKIP_WHEN_WARM\" == true ]] && cargo metadata --locked --offline --all-features --format-version 1 >/dev/null 2>&1; then\n            echo \"Cargo sources warm; skipping fetch\"\n          else\n            cargo fetch --locked\n          fi\n".to_owned()
    } else {
        render_cargo_fetch_body(false)
    };
    env.push_str(&preparation_env());
    let gate = gate.map_or_else(String::new, |gate| {
        format!("        if: ${{{{ {gate} }}}}\n")
    });
    let _ = write!(
        output,
        "      - name: Prepare Cargo sources\n{gate}        env:{env}\n        run: |\n          set -euo pipefail\n          root=\"$CARGO_FETCH_ROOT\"\n          if [[ \"$root\" != \".\" ]]; then\n            cd -- \"$root\"\n          fi\n{fetch_body}"
    );
}

/// Unique manifest roots that need `cargo fetch --locked` for `members`.
///
/// Workspace members share one lockfile at the repository root: one fetch at
/// `.` covers them. Independent manifest trees keep an additional root when
/// their watch graph names a lockfile under their own directory.
fn cargo_lockfile_root(member: &Unit) -> String {
    if member.root == "." {
        return ".".to_owned();
    }
    let local_lock = format!("{}/Cargo.lock", member.root);
    if member
        .cache
        .as_ref()
        .is_some_and(|cache| cache.key_files.iter().any(|key| key == &local_lock))
        || member.watch.iter().any(|path| path == &local_lock)
    {
        member.root.clone()
    } else {
        ".".to_owned()
    }
}

fn append_unique_needs(needs: &mut Vec<String>, additional: impl IntoIterator<Item = String>) {
    for need in additional {
        if !needs.contains(&need) {
            needs.push(need);
        }
    }
}

fn prefix_step_block_with_if(block: &str, guard: Option<&str>) -> String {
    let Some(guard) = guard else {
        return block.to_owned();
    };
    let mut out = String::new();
    let mut pending_name: Option<String> = None;
    let mut pending_if: Option<String> = None;

    let flush_step = |out: &mut String, name: &str, if_expr: Option<&str>| {
        out.push_str(name);
        out.push('\n');
        let combined = if let Some(existing) = if_expr {
            format!("        if: ${{{{ {guard} && ({existing}) }}}}\n")
        } else {
            format!("        if: ${{{{ {guard} }}}}\n")
        };
        out.push_str(&combined);
    };

    for line in block.lines() {
        if line.starts_with("      - name:") {
            if let Some(name) = pending_name.take() {
                flush_step(&mut out, &name, pending_if.as_deref());
                pending_if = None;
            }
            pending_name = Some(line.to_owned());
        } else if pending_name.is_some() && line.trim().starts_with("if:") {
            let expr = line.trim().strip_prefix("if:").unwrap_or("").trim();
            pending_if = Some(
                expr.strip_prefix("${{ ")
                    .and_then(|value| value.strip_suffix(" }}"))
                    .unwrap_or(expr)
                    .to_owned(),
            );
        } else {
            if let Some(name) = pending_name.take() {
                flush_step(&mut out, &name, pending_if.as_deref());
                pending_if = None;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(name) = pending_name.take() {
        flush_step(&mut out, &name, pending_if.as_deref());
    }
    out
}

fn cargo_fetch_roots(members: &[&Unit]) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for member in members {
        if cargo_network_is_restricted(member) {
            roots.insert(cargo_lockfile_root(member));
        }
    }
    let mut ordered = Vec::new();
    if roots.remove(".") {
        ordered.push(".".to_owned());
    }
    ordered.extend(roots);
    ordered
}

/// Selection gate for a lane-level cargo prep job: any restricted unit selected.
fn restricted_unit_selection_if(members: &[&Unit]) -> Option<String> {
    let selectors = members
        .iter()
        .filter(|member| cargo_network_is_restricted(member))
        .map(|member| reusable_selected_unit_selector(&member.id))
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        None
    } else {
        Some(selectors.join(" || "))
    }
}

fn cargo_prep_cache_unit<'a>(members: &[&'a Unit]) -> Option<&'a Unit> {
    members
        .iter()
        .copied()
        .find(|member| cargo_network_is_restricted(member) && member.root == ".")
        .or_else(|| {
            members
                .iter()
                .copied()
                .find(|member| cargo_network_is_restricted(member))
        })
}

fn render_cargo_fetch_roots_script(roots: &[String], skip_when_offline_ready: bool) -> String {
    let fetch_body = render_cargo_fetch_body(skip_when_offline_ready);
    let mut script = String::from("          set -euo pipefail\n");
    for root in roots {
        let quoted = crate::shell_quote(root);
        if root == "." {
            script.push_str(&fetch_body);
        } else {
            let _ = writeln!(script, "          cd -- {quoted}");
            script.push_str(&fetch_body);
            let _ = writeln!(script, "          cd -- \"$GITHUB_WORKSPACE\"");
        }
    }
    script
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
    render_seed_restore_steps(output, ir, &paths, &key, &restore_keys, &checks_env(unit));
}

/// The seed restore for a collapsed lane job: paths and the three snapshot
/// segments come from the caller's inputs.
fn render_mutable_mount_seed_restore_from_input(
    output: &mut String,
    ir: &WorkflowIr,
    checks_env: &str,
) {
    let (key, restore_keys) = input_snapshot(
        DOCKER_SEED_SNAPSHOT_NAMESPACE,
        lane_input::SEED_COMPAT,
        lane_input::SEED_DEPENDENCY_FILES,
        lane_input::SEED_FRESHNESS_FILES,
    );
    let paths = format!(
        "            {}",
        lane_input::expression(lane_input::CACHE_PATHS)
    );
    render_seed_restore_steps(output, ir, &paths, &key, &restore_keys, checks_env);
}

fn render_seed_restore_steps(
    output: &mut String,
    ir: &WorkflowIr,
    paths: &str,
    key: &str,
    restore_keys: &str,
    checks_env: &str,
) {
    let _ = writeln!(
        output,
        "      - name: Restore Docker build seed\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}\n          restore-keys: |\n            {restore_keys}",
        ir.pins.cache_restore
    );
    let _ = writeln!(
        output,
        "      - name: Prepare Docker build seed context\n        env:{checks_env}
        run: |
          set -euo pipefail
          mkdir -p {MUTABLE_MOUNT_HOST_DIR}/seed
          if compgen -G \"{MUTABLE_MOUNT_HOST_DIR}/seed/*\" > /dev/null; then
            echo \"Docker build seed restored:\"
            du -sh {MUTABLE_MOUNT_HOST_DIR}/seed/*
          else
            echo \"Docker build seed empty: the build starts from cold cache mounts\"
          fi"
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
    let save = (cache_save && unit.cache.is_some()).then(|| {
        let (paths, _) = unit
            .cache
            .as_ref()
            .map(rendered_cache_values)
            .unwrap_or_default();
        let (key, _) = unit_snapshot(ir, unit, DOCKER_SEED_SNAPSHOT_NAMESPACE);
        (paths, key)
    });
    render_seed_collection_steps(output, ir, &checks_env(unit), save.as_ref());
}

/// The seed collection for a collapsed lane job: the save key reads the same
/// inputs the restore step keyed on.
fn render_mutable_mount_seed_collection_from_input(
    output: &mut String,
    ir: &WorkflowIr,
    checks_env: &str,
    cache_save: bool,
) {
    let save = cache_save.then(|| {
        let (key, _) = input_snapshot(
            DOCKER_SEED_SNAPSHOT_NAMESPACE,
            lane_input::SEED_COMPAT,
            lane_input::SEED_DEPENDENCY_FILES,
            lane_input::SEED_FRESHNESS_FILES,
        );
        let paths = format!(
            "            {}",
            lane_input::expression(lane_input::CACHE_PATHS)
        );
        (paths, key)
    });
    render_seed_collection_steps(output, ir, checks_env, save.as_ref());
}

fn render_seed_collection_steps(
    output: &mut String,
    ir: &WorkflowIr,
    checks_env: &str,
    save: Option<&(String, String)>,
) {
    let trusted_cache = trusted_cache_save_expression(&ir.default_branch);
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
        checks_env,
        MUTABLE_MOUNT_SEED_FILES[MUTABLE_MOUNT_SEED_FILES.len() - 1]
    );
    if let Some((paths, key)) = save {
        let save_gate = dependency_bundle_cache_save_if(&ir.default_branch);
        render_cache_save_start_marker(output, "SEED", &save_gate);
        let _ = writeln!(
            output,
            "      - name: Save Docker build seed\n        id: docker_seed_cache_save\n        if: {save_gate}\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}",
            ir.pins.cache_save
        );
        render_cache_save_end_marker(output, "SEED", &save_gate, "docker_seed_cache_save");
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "these independent switches are the stable generated workflow contract"
)]
pub(crate) struct WorkflowIr {
    pub(crate) default_branch: String,
    pub(crate) github_runner: String,
    pub(crate) macos_runner: String,
    pub(crate) velnor_labels: Vec<String>,
    pub(crate) ci_required: bool,
    pub(crate) velnor_runner_group: Option<String>,
    pub(crate) velnor_trusted_label: Option<String>,
    pub(crate) velnor_trusted_runner_online: bool,
    pub(crate) velnor_trusted_runner_skip_reason: Option<String>,
    pub(crate) pull_request_on_velnor: VelnorPullRequest,
    pub(crate) repository: String,
    /// The D19 generator pin (`ProjectConfig::workflow_revision`).
    pub(crate) workflow_revision: String,
    /// Declared `[policy]` contexts for ruleset API 403 fallback.
    pub(crate) declared_ruleset_contexts: String,
    pub(crate) default_dispatch_runner: String,
    pub(crate) runners: RunnerMode,
    pub(crate) automatic: RunnerMode,
    pub(crate) velnor_rust_needs: VelnorRustNeeds,
    pub(crate) velnor_concurrency_group: Option<String>,
    pub(crate) velnor_serial_stack_groups: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchChoice {
    Github,
    Velnor,
    Both,
    Omitted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomaticEvent {
    PullRequestSameRepository,
    PullRequestFork,
    Push,
    Schedule,
    MergeGroup,
}

fn automatic_event_selects_lane(
    event: AutomaticEvent,
    lane: RunnerMode,
    velnor_pull_request: VelnorPullRequest,
) -> bool {
    match lane {
        RunnerMode::Github => true,
        RunnerMode::Velnor => match event {
            AutomaticEvent::PullRequestSameRepository => {
                velnor_pull_request == VelnorPullRequest::Automatic
            }
            AutomaticEvent::Push | AutomaticEvent::Schedule => true,
            AutomaticEvent::MergeGroup => velnor_pull_request == VelnorPullRequest::Automatic,
            AutomaticEvent::PullRequestFork => false,
        },
        RunnerMode::Both => false,
    }
}

fn dispatch_choice_selects_lane(choice: DispatchChoice, lane: RunnerMode) -> bool {
    match choice {
        DispatchChoice::Github => lane == RunnerMode::Github,
        DispatchChoice::Velnor => lane == RunnerMode::Velnor,
        DispatchChoice::Both | DispatchChoice::Omitted => {
            matches!(lane, RunnerMode::Github | RunnerMode::Velnor)
        }
    }
}

fn dispatch_lane_expression(lane: RunnerMode, include_omitted: bool) -> String {
    let mut choices = vec![
        (DispatchChoice::Github, "github"),
        (DispatchChoice::Velnor, "velnor"),
        (DispatchChoice::Both, "both"),
    ];
    if include_omitted {
        choices.push((DispatchChoice::Omitted, ""));
    }
    let selected = choices
        .into_iter()
        .filter(|(choice, _)| dispatch_choice_selects_lane(*choice, lane))
        .map(|(_, value)| format!("github.event.inputs.runner == '{value}'"))
        .collect::<Vec<_>>()
        .join(" || ");
    format!("github.event_name == 'workflow_dispatch' && ({selected})")
}

/// The admission class of a lane caller: which single predicate decides, for
/// the current event, ref, and dispatch input, whether the lane runs the unit.
///
/// Three surfaces need the same decision — the collapsed reusable job's
/// `if:`, the aggregate caller's `if:`, and the required check's expectation
/// for that caller (`success` when admitted, `skipped` when not) — and each
/// renders it from [`WorkflowIr::lane_admission_expression`] on this key. A
/// fork pull request, a lane-restricted `workflow_dispatch`, and an offline
/// trusted runner are all just values of that predicate, never special cases
/// of the gate. [`crate::validate_lane_admission_single_source`] checks the
/// rendered tree for drift between the three surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LaneAdmission {
    /// The GitHub-hosted lane.
    #[default]
    Github,
    /// The Velnor lane on the base runner labels.
    Velnor,
    /// The Velnor lane on the trusted runner label; fails closed while no
    /// runner claiming that label is online.
    VelnorTrusted,
}

impl LaneAdmission {
    /// The admission class of `unit` on `lane`.
    pub(crate) fn for_unit(lane: RunnerMode, unit: &Unit) -> Self {
        match lane {
            RunnerMode::Velnor if unit.requires_trusted => Self::VelnorTrusted,
            RunnerMode::Velnor => Self::Velnor,
            RunnerMode::Github | RunnerMode::Both => Self::Github,
        }
    }

    /// The lane whose event predicate this class evaluates.
    pub(crate) fn lane(self) -> RunnerMode {
        match self {
            Self::Github => RunnerMode::Github,
            Self::Velnor | Self::VelnorTrusted => RunnerMode::Velnor,
        }
    }

    /// The required check's environment variable carrying the evaluated
    /// predicate (`true` / `false`) for this class.
    pub(crate) fn env_name(self) -> &'static str {
        match self {
            Self::Github => "LANE_ADMITTED_GITHUB",
            Self::Velnor => "LANE_ADMITTED_VELNOR",
            Self::VelnorTrusted => "LANE_ADMITTED_VELNOR_TRUSTED",
        }
    }

    /// The dependency-info input value naming this class: what the nested
    /// job records beside the unit's dependency closure.
    pub(crate) fn info_id(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Velnor => "velnor",
            Self::VelnorTrusted => "velnor-trust-gated",
        }
    }
}

/// One caller the required check validates: the aggregate job id, the unit
/// ids any of which selects it, its admission class, and whether it is a
/// prerequisite of other callers (which only changes the diagnostic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequiredCaller {
    pub(crate) job_id: String,
    pub(crate) selected_by: Vec<String>,
    pub(crate) admission: LaneAdmission,
    pub(crate) prerequisite: bool,
}

/// `contains(...)` over the plan's `units` output for one unit id, as the
/// aggregate callers spell it.
fn aggregate_selected_unit_selector(unit_id: &str) -> String {
    format!("contains(format(',{{0}},', needs.plan.outputs.units), ',{unit_id},')")
}

fn workflow_dispatch_inputs(
    default_scope: &str,
    default_branch: &str,
    extra_inputs: &str,
    runners: RunnerMode,
    automatic: RunnerMode,
    default_dispatch_runner: &str,
) -> String {
    let required = if runners == RunnerMode::Velnor {
        "true"
    } else {
        "false"
    };
    let default_runner = match runners {
        RunnerMode::Github => "github",
        RunnerMode::Velnor => default_dispatch_runner,
        RunnerMode::Both => automatic.as_str(),
    };
    let options = crate::dispatch_runner_options(runners)
        .iter()
        .map(|option| format!("          - {option}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "  workflow_dispatch:\n    inputs:\n      runner:\n        description: Execution backend\n        required: {required}\n        default: {default_runner}\n        type: choice\n        options:\n{options}\n      scope:\n        description: Verification scope\n        required: true\n        default: {default_scope}\n        type: choice\n        options:\n          - affected\n          - full\n      base_sha:\n        description: Git ref or SHA used as the affected-selection base\n        required: false\n        default: refs/heads/{default_branch}\n        type: string\n{extra_inputs}"
    )
}

fn aggregate_concurrency_kind_suffix(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::PullRequest => "pr",
        WorkflowKind::Main => "main",
        WorkflowKind::Nightly => "nightly",
    }
}

fn aggregate_concurrency_group(ir: &WorkflowIr, kind: WorkflowKind) -> String {
    ir.velnor_concurrency_group.as_deref().map_or_else(
        || match kind {
            // Main accepts both pushes and manual dispatches. A ref-based
            // group makes unrelated runs wait behind each other, which is
            // especially harmful because this aggregate intentionally keeps
            // cancellation disabled so valid producers finish.
            WorkflowKind::Main => "ci-${{ github.workflow }}-${{ github.run_id }}".to_owned(),
            WorkflowKind::PullRequest | WorkflowKind::Nightly => {
                "ci-${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}"
                    .to_owned()
            }
        },
        |base| match kind {
            WorkflowKind::Main => format!("{base}-main-${{{{ github.run_id }}}}"),
            WorkflowKind::PullRequest => format!(
                "{base}-{}-${{{{ github.event.pull_request.number || github.ref }}}}",
                aggregate_concurrency_kind_suffix(kind)
            ),
            WorkflowKind::Nightly => {
                format!("{base}-{}", aggregate_concurrency_kind_suffix(kind))
            }
        },
    )
}

fn aggregate_concurrency_block(
    ir: &WorkflowIr,
    kind: WorkflowKind,
    cancel_in_progress: bool,
) -> String {
    let group = aggregate_concurrency_group(ir, kind);
    format!("concurrency:\n  group: {group}\n  cancel-in-progress: {cancel_in_progress}\n\n")
}

/// PR aggregates may be superseded while they are waiting on a dependency.
/// Keep their callers and required mirrors out of the cancelled run while
/// retaining the explicit result/admission predicates below the status guard.
/// Main and nightly aggregates must finish their publishing and alert paths.
fn aggregate_job_guard(cancel_in_progress: bool) -> &'static str {
    if cancel_in_progress {
        "!cancelled()"
    } else {
        "always()"
    }
}

fn aggregate_triggers(
    kind: WorkflowKind,
    default_branch: &str,
    runners: RunnerMode,
    automatic: RunnerMode,
    default_dispatch_runner: &str,
) -> (&'static str, &'static str, String, bool) {
    match kind {
        WorkflowKind::PullRequest => (
            "CI / PR",
            "CI / PR",
            format!(
                "on:\n  pull_request:\n{}",
                workflow_dispatch_inputs(
                    "affected",
                    default_branch,
                    "",
                    runners,
                    automatic,
                    default_dispatch_runner,
                )
            ),
            true,
        ),
        WorkflowKind::Main => (
            "CI / Main",
            "CI / main",
            format!(
                "on:\n  push:\n    branches: [{}]\n{}",
                yaml_scalar(default_branch),
                workflow_dispatch_inputs(
                    "full",
                    default_branch,
                    "",
                    runners,
                    automatic,
                    default_dispatch_runner,
                )
            ),
            false,
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
                    automatic,
                    default_dispatch_runner,
                )
            ),
            false,
        ),
    }
}

/// The plan output that carries one kind's selected-unit matrix.
fn kind_matrix_output(kind: UnitKind) -> String {
    format!("{}_matrix", kind.id_prefix())
}

/// Read a plan matrix output from `NEEDS_JSON`.
///
/// The empty-array default is an `--arg`, not a quoted JSON literal inside the
/// jq program. A `// "[]"` default inside single quotes is one YAML/Rust
/// escape away from `// \"[]\"`, which jq rejects.
#[allow(dead_code)]
pub(crate) fn jq_read_plan_matrix(matrix_key: &str) -> String {
    format!(
        r#"matrix="$(jq -r --arg key '{matrix_key}' --arg empty '[]' '.plan.outputs[$key] // $empty' <<<"$NEEDS_JSON")""#
    )
}

fn kind_from_unit_workflow_file(file: &str) -> Option<UnitKind> {
    let stem = file
        .strip_prefix("ci-unit-")
        .and_then(|value| value.strip_suffix(".yml"))?;
    match stem {
        "rust" => Some(UnitKind::Rust),
        "gradle" => Some(UnitKind::Gradle),
        "node" => Some(UnitKind::Node),
        "bun" => Some(UnitKind::Bun),
        "swift" => Some(UnitKind::Swift),
        "opentofu" => Some(UnitKind::OpenTofu),
        "docker" => Some(UnitKind::Docker),
        "homebrew" => Some(UnitKind::Homebrew),
        "docs" => Some(UnitKind::Docs),
        _ => None,
    }
}

/// One aggregate caller for a (unit, lane) pair (D1, D5).
struct UnitLaneCaller {
    job_id: String,
    unit_id: String,
    name: String,
    lane: RunnerMode,
    file: String,
    /// The per-unit `workflow_call` inputs this caller supplies to the kind
    /// reusable: every unit-specific literal the collapsed lane job reads
    /// through `inputs.*` instead of carrying one step block per unit.
    inputs: Vec<(&'static str, String)>,
}

/// The `workflow_call` inputs a kind reusable reads per-unit facts from.
///
/// GitHub loads a called workflow once per caller into one template-memory
/// budget, so the callee must be O(1) in the number of units: every value that
/// differs between two units of a kind travels through one of these inputs and
/// the callee renders its step block exactly once per lane job. The caller and
/// the callee derive both sides from [`LaneStepFacts`], so they can never name
/// a different set of inputs.
pub(crate) mod lane_input {
    /// Space-separated mise tool ids the lane installs for the unit.
    pub(crate) const MISE_TOOLS: &str = "mise_tools";
    /// `true` when the hosted lane needs the mise task runner without tools.
    pub(crate) const MISE_RUNNER: &str = "mise_runner";
    /// The Mr. Boxington snapshot compatibility digest.
    pub(crate) const MBX_COMPAT: &str = "mbx_compat";
    /// `true` when the selected unit uses Mr. Boxington instead of sccache.
    pub(crate) const MBX_ENABLED: &str = "mbx_enabled";
    /// Newline-separated `hashFiles` patterns of the snapshot dependency segment.
    pub(crate) const MBX_DEPENDENCY_FILES: &str = "mbx_dependency_files";
    /// Newline-separated `hashFiles` patterns of the snapshot freshness segment.
    pub(crate) const MBX_FRESHNESS_FILES: &str = "mbx_freshness_files";
    /// Comma-separated `taiki-e/install-action` tools.
    pub(crate) const CARGO_BIN_TOOLS: &str = "cargo_bin_tools";
    /// The kind toolchain version (`bun-version`).
    pub(crate) const TOOL_VERSION: &str = "tool_version";
    /// The Node package-lock path `setup-node` keys its cache on.
    pub(crate) const NODE_CACHE_DEPENDENCY_PATH: &str = "node_cache_dependency_path";
    /// Newline-separated actions-cache paths (dependency bundle or Docker seed).
    pub(crate) const CACHE_PATHS: &str = "cache_paths";
    /// Newline-separated `hashFiles` patterns of the dependency bundle key.
    pub(crate) const CACHE_KEY_FILES: &str = "cache_key_files";
    /// The Docker build seed snapshot compatibility digest.
    pub(crate) const SEED_COMPAT: &str = "seed_compat";
    /// Newline-separated `hashFiles` patterns of the seed dependency segment.
    pub(crate) const SEED_DEPENDENCY_FILES: &str = "seed_dependency_files";
    /// Newline-separated `hashFiles` patterns of the seed freshness segment.
    pub(crate) const SEED_FRESHNESS_FILES: &str = "seed_freshness_files";
    /// The manifest root `cargo fetch --locked` runs in; empty skips the fetch.
    pub(crate) const CARGO_ROOT: &str = "cargo_root";
    /// `true` when a warm host store lets the fetch step skip itself.
    pub(crate) const CARGO_FETCH_SKIP_WHEN_WARM: &str = "cargo_fetch_skip_when_warm";
    /// `CARGO_NET_OFFLINE` for the checks step; defaults to `false`.
    pub(crate) const CARGO_NET_OFFLINE: &str = "cargo_net_offline";
    /// Comma-separated host-warm layers the Velnor lane reports.
    pub(crate) const HOST_WARM_LAYERS: &str = "host_warm_layers";
    /// `true` when the Velnor lane must provision the pinned policy runtime
    /// for the unit's generator `--check`; hosted lanes carry it in the
    /// Planning runtime artifact unconditionally.
    pub(crate) const POLICY_RUNTIME: &str = "policy_runtime";
    /// `true` when the hosted lane job must publish its own debug product of
    /// the generator crate as the Stage-1 candidate artifact the owner
    /// policy run consumes. Only the generator crate's owning Rust unit sets
    /// it, on the hosted lane, in the owner repository, on pull requests.
    pub(crate) const CANDIDATE_PUBLISH: &str = "candidate_publish";
    /// `true` when the unit needs the Apple executor: a kind whose members
    /// split across the default and Apple executors renders one collapsed
    /// job per executor, and each job admits only its own callers.
    pub(crate) const APPLE_EXECUTOR: &str = "apple_executor";
    /// Comma-separated `depends_on` ids the nested job records; empty when
    /// the unit depends on nothing.
    pub(crate) const UNIT_DEPENDENCIES: &str = "unit_dependencies";
    /// The lane admission class id the nested job records.
    pub(crate) const UNIT_ADMISSION: &str = "unit_admission";
    /// Comma-separated prepared-tool need records (`tool:digest:producers`)
    /// the lane restores for the unit; empty when it needs none.
    pub(crate) const PREPARED_TOOLS: &str = "prepared_tools";

    /// Every per-unit input, in declaration order.
    pub(crate) const ALL: &[&str] = &[
        MISE_TOOLS,
        MISE_RUNNER,
        MBX_ENABLED,
        MBX_COMPAT,
        MBX_DEPENDENCY_FILES,
        MBX_FRESHNESS_FILES,
        CARGO_BIN_TOOLS,
        TOOL_VERSION,
        NODE_CACHE_DEPENDENCY_PATH,
        CACHE_PATHS,
        CACHE_KEY_FILES,
        SEED_COMPAT,
        SEED_DEPENDENCY_FILES,
        SEED_FRESHNESS_FILES,
        CARGO_ROOT,
        CARGO_FETCH_SKIP_WHEN_WARM,
        CARGO_NET_OFFLINE,
        HOST_WARM_LAYERS,
        POLICY_RUNTIME,
        CANDIDATE_PUBLISH,
        APPLE_EXECUTOR,
        UNIT_DEPENDENCIES,
        UNIT_ADMISSION,
        PREPARED_TOOLS,
    ];

    /// The inputs declared as `type: boolean`. Callers pass them unquoted so
    /// the caller-side type check agrees with the declaration, and an omitted
    /// flag reads as `false`. Every other input is a string that reads as
    /// "absent" when empty.
    pub(crate) fn is_flag(name: &str) -> bool {
        matches!(
            name,
            MISE_RUNNER
                | MBX_ENABLED
                | CARGO_FETCH_SKIP_WHEN_WARM
                | CARGO_NET_OFFLINE
                | POLICY_RUNTIME
                | CANDIDATE_PUBLISH
                | APPLE_EXECUTOR
        )
    }

    /// The `workflow_call` declaration lines of one optional input.
    pub(crate) fn declaration(name: &str) -> String {
        if is_flag(name) {
            format!("      {name}:\n        required: false\n        type: boolean\n        default: false")
        } else {
            format!("      {name}:\n        required: false\n        type: string\n        default: \"\"")
        }
    }

    /// `${{ inputs.<name> }}`.
    pub(crate) fn expression(name: &str) -> String {
        format!("${{{{ inputs.{name} }}}}")
    }

    /// `${{ hashFiles(inputs.<name>) }}`: the runner joins every `hashFiles`
    /// argument with a newline before globbing, so one newline-separated input
    /// hashes exactly what the literal `hashFiles('a', 'b')` form hashes.
    pub(crate) fn hash_files_expression(name: &str) -> String {
        format!("${{{{ hashFiles(inputs.{name}) }}}}")
    }

    /// The step gate for a feature only some units of the kind carry.
    pub(crate) fn present_gate(name: &str) -> String {
        if is_flag(name) {
            format!("inputs.{name}")
        } else {
            format!("inputs.{name} != ''")
        }
    }

    /// The step gate for one need record only some units of the kind carry:
    /// the caller's comma-wrapped records contain the comma-wrapped record.
    /// Records never contain commas, so the match cannot be partial.
    pub(crate) fn contains_gate(name: &str, value: &str) -> String {
        format!("contains(format(',{{0}},', inputs.{name}), ',{value},')")
    }
}

/// The snapshot identity of one unit on one namespace, split into the parts
/// a caller passes through inputs: the compatibility digest, the dependency
/// inputs, and the freshness (state) inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnapshotFacts {
    pub(crate) compatibility: String,
    pub(crate) dependency_files: Vec<String>,
    pub(crate) state_files: Vec<String>,
}

/// The per-unit facts one collapsed lane job reads through `workflow_call`
/// inputs. Built once per (unit, lane) by [`WorkflowIr::unit_lane_facts`]; the
/// caller turns it into `with:` values and the callee unions it over the
/// kind's members to decide which steps exist and which need a presence gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent `type: boolean` workflow_call input"
)]
pub(crate) struct LaneStepFacts {
    pub(crate) mise_tools: Vec<String>,
    pub(crate) mise_runner: bool,
    pub(crate) mbx_enabled: bool,
    pub(crate) mbx: Option<SnapshotFacts>,
    pub(crate) cargo_bin_tools: Vec<String>,
    pub(crate) tool_version: Option<String>,
    pub(crate) node_cache_dependency_path: Option<String>,
    /// Dependency bundle: cache paths and key files.
    pub(crate) bundle: Option<(Vec<String>, Vec<String>)>,
    /// Docker mutable mount seed: cache paths and snapshot identity.
    pub(crate) seed: Option<(Vec<String>, SnapshotFacts)>,
    pub(crate) cargo_root: Option<String>,
    pub(crate) cargo_fetch_skip_when_warm: bool,
    pub(crate) cargo_net_offline: bool,
    pub(crate) host_warm_layers: Vec<&'static str>,
    pub(crate) policy_runtime: bool,
    pub(crate) candidate_publish: bool,
    pub(crate) apple_executor: bool,
    pub(crate) unit_dependencies: Vec<String>,
    pub(crate) unit_admission: LaneAdmission,
    /// Prepared-tool need records the lane restores for the unit.
    pub(crate) prepared_tools: Vec<String>,
}

impl LaneStepFacts {
    /// The `with:` values a caller passes: one entry per non-empty fact.
    pub(crate) fn input_values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        if !self.mise_tools.is_empty() {
            values.push((lane_input::MISE_TOOLS, self.mise_tools.join(" ")));
        }
        if self.mise_runner {
            values.push((lane_input::MISE_RUNNER, "true".to_owned()));
        }
        if self.mbx_enabled {
            values.push((lane_input::MBX_ENABLED, "true".to_owned()));
        }
        if let Some(mbx) = &self.mbx {
            values.push((lane_input::MBX_COMPAT, mbx.compatibility.clone()));
            values.push((
                lane_input::MBX_DEPENDENCY_FILES,
                mbx.dependency_files.join("\n"),
            ));
            values.push((lane_input::MBX_FRESHNESS_FILES, mbx.state_files.join("\n")));
        }
        if !self.cargo_bin_tools.is_empty() {
            values.push((lane_input::CARGO_BIN_TOOLS, self.cargo_bin_tools.join(",")));
        }
        if let Some(version) = &self.tool_version {
            values.push((lane_input::TOOL_VERSION, version.clone()));
        }
        if let Some(path) = &self.node_cache_dependency_path {
            values.push((lane_input::NODE_CACHE_DEPENDENCY_PATH, path.clone()));
        }
        if let Some((paths, key_files)) = &self.bundle {
            values.push((lane_input::CACHE_PATHS, paths.join("\n")));
            values.push((lane_input::CACHE_KEY_FILES, key_files.join("\n")));
        }
        if let Some((paths, seed)) = &self.seed {
            values.push((lane_input::CACHE_PATHS, paths.join("\n")));
            values.push((lane_input::SEED_COMPAT, seed.compatibility.clone()));
            values.push((
                lane_input::SEED_DEPENDENCY_FILES,
                seed.dependency_files.join("\n"),
            ));
            values.push((
                lane_input::SEED_FRESHNESS_FILES,
                seed.state_files.join("\n"),
            ));
        }
        if let Some(root) = &self.cargo_root {
            values.push((lane_input::CARGO_ROOT, root.clone()));
        }
        if self.cargo_fetch_skip_when_warm {
            values.push((lane_input::CARGO_FETCH_SKIP_WHEN_WARM, "true".to_owned()));
        }
        if self.cargo_net_offline {
            values.push((lane_input::CARGO_NET_OFFLINE, "true".to_owned()));
        }
        if !self.host_warm_layers.is_empty() {
            values.push((
                lane_input::HOST_WARM_LAYERS,
                self.host_warm_layers.join(","),
            ));
        }
        if self.policy_runtime {
            values.push((lane_input::POLICY_RUNTIME, "true".to_owned()));
        }
        if self.candidate_publish {
            values.push((lane_input::CANDIDATE_PUBLISH, "true".to_owned()));
        }
        if self.apple_executor {
            values.push((lane_input::APPLE_EXECUTOR, "true".to_owned()));
        }
        if !self.unit_dependencies.is_empty() {
            values.push((
                lane_input::UNIT_DEPENDENCIES,
                self.unit_dependencies.join(","),
            ));
        }
        values.push((
            lane_input::UNIT_ADMISSION,
            self.unit_admission.info_id().to_owned(),
        ));
        if !self.prepared_tools.is_empty() {
            values.push((lane_input::PREPARED_TOOLS, self.prepared_tools.join(",")));
        }
        values
    }
}

/// How many members of a collapsed lane job carry one feature: none (the
/// steps are not rendered), all (rendered without a gate), or some (rendered
/// behind an input presence gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FeatureCoverage {
    any: bool,
    all: bool,
}

impl FeatureCoverage {
    fn over(facts: &[LaneStepFacts], predicate: impl Fn(&LaneStepFacts) -> bool) -> Self {
        let count = facts.iter().filter(|facts| predicate(facts)).count();
        Self {
            any: count > 0,
            all: count == facts.len() && !facts.is_empty(),
        }
    }

    /// The `if:` gate the feature's steps need, or `None` when every member
    /// carries it.
    fn gate(self, input: &str) -> Option<String> {
        (self.any && !self.all).then(|| lane_input::present_gate(input))
    }

    /// The gate for a boolean feature's complement when only some members
    /// carry it. This keeps the alternative transport member-scoped too.
    fn absent_gate(self, input: &str) -> Option<String> {
        (self.any && !self.all).then(|| format!("inputs.{input} == false"))
    }
}

/// Render `with:` entries for a reusable-workflow caller: scalars inline,
/// multi-line values as literal block scalars.
fn render_caller_inputs(inputs: &[(&str, String)]) -> String {
    let mut output = String::new();
    for (name, value) in inputs {
        if value.contains('\n') {
            let _ = writeln!(output, "      {name}: |");
            for line in value.lines() {
                let _ = writeln!(output, "        {line}");
            }
        } else if lane_input::is_flag(name) {
            let _ = writeln!(output, "      {name}: {value}");
        } else {
            let _ = writeln!(output, "      {name}: {}", yaml_scalar(value));
        }
    }
    // The caller block ends without a trailing newline, like the `with:` map
    // above it; the surrounding `writeln!` supplies the terminator.
    while output.ends_with('\n') {
        output.pop();
    }
    if output.is_empty() {
        output
    } else {
        format!("\n{output}")
    }
}

/// Membership test a kind-reusable job uses instead of `inputs.unit ==`.
/// Calling the reusable once per kind (not once per matrix unit) keeps GitHub
/// job count linear in the kind's units. A caller-side matrix instantiates
/// every job in the reusable for every selected unit: N×N skipped jobs.
fn reusable_selected_unit_selector(unit_id: &str) -> String {
    format!("contains(format(',{{0}},', inputs.selected_units), ',{unit_id},')")
}

/// The `env:` entries of the required check carrying each admission class
/// the callers use, evaluated by GitHub from the same expression the callee's
/// `if:` and the caller's `if:` render.
fn render_required_admission_env(workflow: &WorkflowIr, callers: &[RequiredCaller]) -> String {
    let mut output = String::new();
    for admission in callers
        .iter()
        .map(|caller| caller.admission)
        .collect::<BTreeSet<_>>()
    {
        let _ = writeln!(
            output,
            "          {}: {}",
            admission.env_name(),
            github_expression(&workflow.lane_admission_expression(admission))
        );
    }
    output
}

/// The per-caller verdict block of the required check.
///
/// A selected caller whose lane the admission predicate admits must be
/// `success`; a selected caller whose lane it does not admit must be
/// `skipped` (GitHub reports a reusable call whose every job skipped as
/// `skipped`); an unselected caller may be either. The predicate is the
/// value GitHub evaluated into the class's environment variable, so a fork
/// pull request, a lane-restricted dispatch, and an offline trusted runner
/// all take the same branch. A shell conditional must never embed a rendered
/// boolean literal (`[[ ... && false ]]` is a non-empty string test), so the
/// expectation is read from the variable, never inlined.
fn render_required_caller_verdicts(output: &mut String, callers: &[RequiredCaller]) {
    for caller in callers {
        let job_id = &caller.job_id;
        let noun = if caller.prerequisite {
            "CI prerequisite"
        } else {
            "CI job"
        };
        let selected = caller
            .selected_by
            .iter()
            .map(|unit_id| format!("\"$selected\" == *\",{unit_id},\"*"))
            .collect::<Vec<_>>()
            .join(" || ");
        let admitted = caller.admission.env_name();
        let _ = writeln!(
            output,
            "          if [[ {selected} ]]; then\n            result=\"$(result_for_job {job_id})\"\n            if [[ \"${admitted}\" == true ]]; then\n              case \"$result\" in\n                success) ;;\n                *) echo \"selected {noun} {job_id} did not pass: $result\" >&2; exit 1 ;;\n              esac\n            else\n              case \"$result\" in\n                skipped) ;;\n                *) echo \"selected {noun} {job_id} ran outside its lane admission ({admitted}=${admitted}): $result\" >&2; exit 1 ;;\n              esac\n            fi\n          else\n            result=\"$(result_for_job {job_id})\"\n            case \"$result\" in\n              success|skipped) ;;\n              *) echo \"unselected {noun} {job_id} failed unexpectedly: $result\" >&2; exit 1 ;;\n            esac\n          fi"
        );
    }
}

#[allow(dead_code)]
impl WorkflowIr {
    pub(crate) fn from_config(config: &ProjectConfig) -> Self {
        let mut tools = BTreeSet::new();
        let mr_boxington = config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Rust && unit.uses_mbx());
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
        // The detection fact only says mise is configured; the renderer needs
        // it wherever a unit runs through mise or declares tools the scan
        // cannot see (docs, homebrew, Swift, and Rust alike).
        let mise_detected = config
            .analysis
            .detected
            .iter()
            .any(|item| item == "mise-present");
        let mise_surface_needed = config.units.iter().any(|unit| {
            unit.kind == UnitKind::Rust || !unit.mise_tools.is_empty() || commands_invoke_mise(unit)
        });
        let mise_present = mise_detected && mise_surface_needed;
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
        let trusted_runner = crate::runners::resolve_trusted_runner_availability(config);
        Self {
            default_branch: config.default_branch.clone(),
            github_runner: config.github_runner.clone(),
            macos_runner: config.macos_runner.clone(),
            velnor_labels: config.velnor_labels.clone(),
            ci_required: config.ci_required,
            velnor_runner_group: velnor_runner_group(config).map(str::to_owned),
            velnor_trusted_label: config.velnor_trusted_label.clone(),
            velnor_trusted_runner_online: trusted_runner.online,
            velnor_trusted_runner_skip_reason: trusted_runner.skip_reason,
            pull_request_on_velnor: if config.pull_request_on_velnor {
                VelnorPullRequest::Automatic
            } else {
                VelnorPullRequest::TrustedOnly
            },
            repository: config.repository.clone(),
            workflow_revision: config.workflow_revision.clone(),
            declared_ruleset_contexts: crate::declared_ruleset_contexts_literal(config),
            default_dispatch_runner: config.default_dispatch_runner.clone(),
            runners: config.runners,
            automatic: config.automatic,
            velnor_rust_needs: config.velnor_rust_needs,
            velnor_concurrency_group: config.velnor_concurrency_group.clone(),
            velnor_serial_stack_groups: config.velnor_serial_stack_groups,
            tools,
            mise_present,
            mr_boxington,
            units: config.units.clone(),
            pins: Pins::resolved(),
            mise_lock_keys: config.mise_lock_keys.clone(),
        }
    }

    fn ci_report_action_uses(&self) -> String {
        crate::ci_report_action_uses(&self.repository, &self.workflow_revision)
    }

    pub(crate) fn render(&self, kind: WorkflowKind) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, cancel_in_progress) = aggregate_triggers(
            kind,
            &self.default_branch,
            self.runners,
            self.automatic,
            &self.default_dispatch_runner,
        );
        let concurrency = aggregate_concurrency_block(self, kind, cancel_in_progress);
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\n{concurrency}permissions:\n  actions: read\n  contents: read\n\n"
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
        // Runner mode is global; every self-hosted job receives the lane
        // admission gate needed for Velnor execution.
        let runners = self.runners;
        self.render_plan(&mut output, runners, runners == RunnerMode::Velnor);
        self.render_velnor_lane_admission(&mut output);
        if kind != WorkflowKind::PullRequest {
            self.render_policy(&mut output, runners, runners == RunnerMode::Velnor);
        }
        self.render_hierarchy_groups(&mut output, kind != WorkflowKind::PullRequest);
        let cache_save = kind != WorkflowKind::PullRequest;
        match runners {
            RunnerMode::Github => self.render_verify_github(
                &mut output,
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
    pub(crate) fn render_nested(
        &self,
        kind: WorkflowKind,
        nodes: &[GraphNode],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, cancel_in_progress) = aggregate_triggers(
            kind,
            &self.default_branch,
            self.runners,
            self.automatic,
            &self.default_dispatch_runner,
        );
        let concurrency = aggregate_concurrency_block(self, kind, cancel_in_progress);
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\n{concurrency}permissions:\n  actions: read\n  contents: read\n\njobs:"
        );
        // Planning follows config.runners. A Velnor-configured repository
        // keeps plan on the image runtime for every aggregate. GitHub-default
        // and both-mode repositories plan on GitHub-hosted runners.
        let mut plan = String::new();
        self.render_plan(&mut plan, self.runners, self.runners == RunnerMode::Velnor);
        output.push_str(&plan);
        self.render_velnor_lane_admission(&mut output);
        if kind != WorkflowKind::PullRequest {
            self.render_policy(
                &mut output,
                self.runners,
                self.runners == RunnerMode::Velnor,
            );
        }
        self.render_node_callers(
            nodes,
            &mut output,
            kind != WorkflowKind::PullRequest,
            cancel_in_progress,
            contracts,
        );
        if kind == WorkflowKind::Nightly {
            self.render_nodes_required(
                nodes,
                contracts,
                &mut output,
                true,
                "nightly-required",
                true,
                cancel_in_progress,
            );
            self.render_nightly_alert(&mut output, "nightly-required", None);
        } else if self.ci_required {
            self.render_nodes_required(
                nodes,
                contracts,
                &mut output,
                kind != WorkflowKind::PullRequest,
                REQUIRED_CHECK,
                false,
                cancel_in_progress,
            );
        }
        while output.ends_with("\n\n") {
            output.pop();
        }
        output
    }

    /// Nightly is a scheduled dispatcher of `ci-main.yml` on the default branch.
    /// `workflow_dispatch` on ci-main is trusted for mbx saves; schedule alone is not.
    pub(crate) fn render_nightly_dispatcher(&self) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let (workflow_name, run_name, triggers, _) = aggregate_triggers(
            WorkflowKind::Nightly,
            &self.default_branch,
            self.runners,
            self.automatic,
            &self.default_dispatch_runner,
        );
        let concurrency = aggregate_concurrency_block(self, WorkflowKind::Nightly, false);
        let default_branch = yaml_scalar(&self.default_branch);
        let default_runner = match self.runners {
            RunnerMode::Github => "github",
            RunnerMode::Velnor => self.default_dispatch_runner.as_str(),
            RunnerMode::Both => self.automatic.as_str(),
        };
        let runner = self.runner_for(self.control_plane_lane());
        let dispatch_if = "github.event_name != 'workflow_dispatch' || !inputs.simulate_failure";
        let simulate_if = "github.event_name == 'workflow_dispatch' && inputs.simulate_failure";
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\n{concurrency}permissions:\n  actions: read\n  contents: read\n\njobs:"
        );
        let _ = writeln!(
            output,
            "  dispatch-ci-main:\n    name: {}\n    if: ${{{{ {dispatch_if} }}}}\n    runs-on: {runner}\n    timeout-minutes: 5\n    permissions:\n      actions: write\n      contents: read\n    steps:\n      - name: Dispatch ci-main on default branch\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          GITHUB_REPOSITORY: ${{{{ github.repository }}}}\n          DEFAULT_BRANCH: {default_branch}\n          DISPATCH_RUNNER: ${{{{ github.event.inputs.runner || '{default_runner}' }}}}\n          DISPATCH_SCOPE: ${{{{ github.event.inputs.scope || 'full' }}}}\n          DISPATCH_BASE_SHA: ${{{{ github.event.inputs.base_sha || format('refs/heads/{{0}}', github.event.repository.default_branch) }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          gh workflow run ci-main.yml \\\n            -R \"$GITHUB_REPOSITORY\" \\\n            --ref \"$DEFAULT_BRANCH\" \\\n            -f runner=\"$DISPATCH_RUNNER\" \\\n            -f scope=\"$DISPATCH_SCOPE\" \\\n            -f base_sha=\"$DISPATCH_BASE_SHA\"",
            crate::control_job_name("Dispatch ci-main"),
        );
        let _ = writeln!(
            output,
            "  nightly-red-to-signal:\n    name: {}\n    if: ${{{{ {simulate_if} }}}}\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Simulate nightly failure\n        shell: bash\n        run: |\n          set -euo pipefail\n          echo \"nightly red-to-signal simulation requested\" >&2\n          exit 1",
            crate::control_job_name("Nightly red-to-signal"),
        );
        self.render_nightly_alert(
            &mut output,
            "nightly-red-to-signal",
            Some("always() && needs.nightly-red-to-signal.result == 'failure'"),
        );
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
                "  {id}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{}\n    with:\n      unit: {}\n      units: ${{{{ needs.plan.outputs.units }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      full_units: {}\n      base_sha: ${{{{ needs.plan.outputs.base_sha }}}}\n      head_sha: ${{{{ needs.plan.outputs.head_sha }}}}",
                yaml_scalar(&name),
                conditions.join(" && "),
                needs.join(", "),
                nested_unit_workflow_file(unit),
                yaml_scalar(&unit.id),
                yaml_scalar(&unit.id),
            );
        }
    }

    /// The (unit, lane) callers of one unit. The `with:` values come from the
    /// same per-unit facts the collapsed callee gates on, derived from the
    /// unit's resolved contract.
    fn unit_lane_callers(
        &self,
        unit: &Unit,
        file: &str,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Vec<UnitLaneCaller> {
        let contract = self.contract_for(unit, contracts);
        contract
            .lanes
            .iter()
            .filter(|job| lane_supports_unit(job.lane, unit))
            .map(|job| UnitLaneCaller {
                job_id: unit_job_id(job.lane, &unit.id),
                unit_id: unit.id.clone(),
                name: unit_job_display_name(unit, job.lane, self.runners),
                lane: job.lane,
                file: file.to_owned(),
                inputs: self
                    .unit_lane_facts(unit, &contract, job.lane)
                    .input_values(),
            })
            .collect()
    }

    fn kind_file_needs_prepare_cargo(&self, file: &str) -> bool {
        self.runners == RunnerMode::Both
            && kind_from_unit_workflow_file(file) == Some(UnitKind::Rust)
            && self.units.iter().any(|unit| {
                unit.kind == UnitKind::Rust
                    && nested_unit_workflow_file(unit) == file
                    && cargo_network_is_restricted(unit)
            })
    }

    /// The unit ids whose selection runs the `prepare-cargo` caller of one
    /// kind file: every network-restricted Rust unit the file holds. The
    /// callee's `velnor-prepare-cargo-sources` job selects on the same set
    /// (`restricted_unit_selection_if`), so the caller, the callee, and the
    /// required check agree on when the prerequisite runs.
    fn prepare_cargo_selected_by(&self, file: &str) -> Vec<String> {
        self.units
            .iter()
            .filter(|unit| {
                unit.kind == UnitKind::Rust
                    && nested_unit_workflow_file(unit) == file
                    && cargo_network_is_restricted(unit)
            })
            .map(|unit| unit.id.clone())
            .collect()
    }

    /// The `prepare-cargo` caller as the required check validates it: a
    /// prerequisite selected by any restricted unit of the file and admitted
    /// with the Velnor lane, whose stores it warms.
    fn prepare_cargo_required_caller(&self, file: &str) -> RequiredCaller {
        RequiredCaller {
            job_id: prepare_cargo_caller_job_id().to_owned(),
            selected_by: self.prepare_cargo_selected_by(file),
            admission: LaneAdmission::Velnor,
            prerequisite: true,
        }
    }

    fn render_prepare_cargo_caller(
        &self,
        output: &mut String,
        file: &str,
        sample_unit: &str,
        include_policy: bool,
        cancel_in_progress: bool,
    ) {
        let caller = self.prepare_cargo_required_caller(file);
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        let mut conditions = vec![
            aggregate_job_guard(cancel_in_progress).to_owned(),
            "needs.plan.result == 'success'".to_owned(),
        ];
        if include_policy {
            conditions.push("needs.policy.result == 'success'".to_owned());
        }
        conditions.push(format!(
            "({})",
            caller
                .selected_by
                .iter()
                .map(|unit_id| aggregate_selected_unit_selector(unit_id))
                .collect::<Vec<_>>()
                .join(" || ")
        ));
        conditions.push(format!(
            "({})",
            self.lane_admission_expression(caller.admission)
        ));
        let _ = writeln!(
            output,
            "  {}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{file}\n    with:\n      unit: {}\n      lane: control\n      selected_units: ${{{{ needs.plan.outputs.units }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      full_units: ${{{{ needs.plan.outputs.full_units }}}}\n      base_sha: ${{{{ needs.plan.outputs.base_sha }}}}\n      head_sha: ${{{{ needs.plan.outputs.head_sha }}}}",
            caller.job_id,
            crate::control_job_name("Prepare Cargo"),
            conditions.join(" && "),
            needs.join(", "),
            yaml_scalar(sample_unit),
        );
    }

    fn render_unit_lane_caller(
        &self,
        output: &mut String,
        unit: &Unit,
        caller: &UnitLaneCaller,
        include_policy: bool,
        extra_needs: &[String],
        cancel_in_progress: bool,
    ) {
        let lane = caller.lane;
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        append_unique_needs(&mut needs, extra_needs.iter().cloned());
        if lane == RunnerMode::Velnor && self.kind_file_needs_prepare_cargo(&caller.file) {
            append_unique_needs(&mut needs, [prepare_cargo_caller_job_id().to_owned()]);
        }
        append_unique_needs(
            &mut needs,
            velnor_rust_dependency_needs(lane, unit, self.velnor_rust_needs, &self.units),
        );
        let mut conditions = vec![
            aggregate_job_guard(cancel_in_progress).to_owned(),
            "needs.plan.result == 'success'".to_owned(),
        ];
        if include_policy {
            conditions.push("needs.policy.result == 'success'".to_owned());
        }
        if lane == RunnerMode::Velnor && self.kind_file_needs_prepare_cargo(&caller.file) {
            conditions.push(
                "(needs.prepare-cargo.result == 'success' || needs.prepare-cargo.result == 'skipped')"
                    .to_owned(),
            );
        }
        for dependency in
            velnor_rust_dependency_needs(lane, unit, self.velnor_rust_needs, &self.units)
        {
            conditions.push(format!(
                "(needs.{dependency}.result == 'success' || needs.{dependency}.result == 'skipped')"
            ));
        }
        conditions.push(aggregate_selected_unit_selector(&caller.unit_id));
        // The caller skips exactly when the callee's lane job would: same
        // predicate, same class, so the aggregate never invokes a reusable
        // whose every job is gated off.
        conditions.push(format!(
            "({})",
            self.lane_admission_expression(LaneAdmission::for_unit(lane, unit))
        ));
        let _ = writeln!(
            output,
            "  {}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{}\n    with:\n      unit: {}\n      lane: {}\n      selected_units: ${{{{ needs.plan.outputs.units }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      full_units: ${{{{ needs.plan.outputs.full_units }}}}\n      base_sha: ${{{{ needs.plan.outputs.base_sha }}}}\n      head_sha: ${{{{ needs.plan.outputs.head_sha }}}}{}",
            caller.job_id,
            yaml_scalar(&caller.name),
            conditions.join(" && "),
            needs.join(", "),
            caller.file,
            yaml_scalar(&caller.unit_id),
            caller.lane.as_str(),
            render_caller_inputs(&caller.inputs),
        );
    }

    /// One reusable-workflow caller per (unit, lane). The kind reusable holds
    /// one job keyed on `inputs.unit` + `inputs.lane` (D5).
    pub(crate) fn render_node_callers(
        &self,
        nodes: &[GraphNode],
        output: &mut String,
        include_policy: bool,
        cancel_in_progress: bool,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) {
        let mut prepare_cargo_files = BTreeSet::new();
        let mut previous_velnor_caller: Option<String> = None;
        for (unit_id, _job_id, _name, file) in nodes.iter().filter_map(GraphNode::as_unit) {
            let Some(unit) = self.units.iter().find(|unit| unit.id == *unit_id) else {
                continue;
            };
            if self.kind_file_needs_prepare_cargo(file)
                && prepare_cargo_files.insert(file.to_owned())
            {
                self.render_prepare_cargo_caller(
                    output,
                    file,
                    unit_id,
                    include_policy,
                    cancel_in_progress,
                );
            }
            for caller in self.unit_lane_callers(unit, file, contracts) {
                let extra_needs = if self.runners == RunnerMode::Velnor
                    && self.velnor_serial_stack_groups
                    && caller.lane == RunnerMode::Velnor
                {
                    previous_velnor_caller.iter().cloned().collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                self.render_unit_lane_caller(
                    output,
                    unit,
                    &caller,
                    include_policy,
                    &extra_needs,
                    cancel_in_progress,
                );
                if caller.lane == RunnerMode::Velnor {
                    previous_velnor_caller = Some(caller.job_id.clone());
                }
            }
        }
    }

    /// The callers the aggregate required check validates, in caller render
    /// order: the `prepare-cargo` prerequisite of each kind file that needs
    /// one, then the `(unit, lane)` callers of every contributed unit node.
    /// The same `contracts` that decide which callers `render_node_callers`
    /// renders decide which ones the check needs, so a unit declared for one
    /// lane never leaves the check depending on a job that does not exist.
    pub(crate) fn required_callers(
        &self,
        nodes: &[GraphNode],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Vec<RequiredCaller> {
        let mut callers = Vec::new();
        let mut prepare_cargo_files = BTreeSet::new();
        let mut seen_units = BTreeSet::<&str>::new();
        for (unit_id, _job_id, _name, file) in nodes.iter().filter_map(GraphNode::as_unit) {
            if !seen_units.insert(unit_id) {
                continue;
            }
            let Some(unit) = self.units.iter().find(|unit| unit.id == *unit_id) else {
                continue;
            };
            if self.kind_file_needs_prepare_cargo(file)
                && prepare_cargo_files.insert(file.to_owned())
            {
                callers.push(self.prepare_cargo_required_caller(file));
            }
            for caller in self.unit_lane_callers(unit, file, contracts) {
                callers.push(RequiredCaller {
                    job_id: caller.job_id,
                    selected_by: vec![caller.unit_id],
                    admission: LaneAdmission::for_unit(caller.lane, unit),
                    prerequisite: false,
                });
            }
        }
        callers
    }

    /// The aggregate required check over every contributed unit node.
    #[expect(
        clippy::too_many_arguments,
        reason = "the required gate has independent plan, policy, simulation, and cancellation inputs"
    )]
    pub(crate) fn render_nodes_required(
        &self,
        nodes: &[GraphNode],
        contracts: Option<&BTreeMap<String, UnitContract>>,
        output: &mut String,
        include_policy: bool,
        check_name: &str,
        simulate_failure: bool,
        cancel_in_progress: bool,
    ) {
        let callers = self.required_callers(nodes, contracts);
        let mut needs = vec!["plan".to_owned()];
        if self.emits_velnor_lane_admission() {
            needs.push("velnor-lane-admission".to_owned());
        }
        if include_policy {
            needs.push("policy".to_owned());
        }
        needs.extend(callers.iter().map(|caller| caller.job_id.clone()));
        let display_name = match check_name {
            REQUIRED_CHECK => yaml_scalar(REQUIRED_CHECK),
            "nightly-required" => crate::control_job_name("Nightly aggregate"),
            other => yaml_scalar(other),
        };
        let if_condition = if self.control_plane_lane() == RunnerMode::Velnor {
            format!(
                "{} && ({})",
                aggregate_job_guard(cancel_in_progress),
                self.velnor_control_plane_expression()
            )
        } else {
            aggregate_job_guard(cancel_in_progress).to_owned()
        };
        let needs_json = github_expression("toJSON(needs)");
        let selected_units = github_expression("needs.plan.outputs.units");
        let _ = write!(
            output,
            "  {check_name}:\n    name: {display_name}\n    if: ${{{{ {if_condition} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated stack results\n        env:\n          NEEDS_JSON: {needs_json}\n          SELECTED_UNITS: {selected_units}\n{}",
            needs.join(", "),
            self.runner_for(self.control_plane_lane()),
            render_required_admission_env(self, &callers),
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
        if needs.iter().any(|job| job == "velnor-lane-admission") {
            output.push_str(
                "          result=\"$(result_for_job velnor-lane-admission)\"\n          case \"$result\" in\n            success|skipped) ;;\n            *) echo \"required CI prerequisite velnor-lane-admission did not pass: $result\" >&2; exit 1 ;;\n          esac\n",
            );
        }
        output.push_str("          selected=\",$SELECTED_UNITS,\"\n");
        render_required_caller_verdicts(output, &callers);
        if check_name == REQUIRED_CHECK {
            let required_gate = if self.control_plane_lane() == RunnerMode::Velnor {
                format!(
                    "{} && ({})",
                    aggregate_job_guard(cancel_in_progress),
                    self.velnor_control_plane_expression()
                )
            } else {
                aggregate_job_guard(cancel_in_progress).to_owned()
            };
            let _ = writeln!(
                output,
                "  required:\n    name: {}\n    if: ${{{{ {required_gate} }}}}\n    needs: [ci-required]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Mirror CI / Required\n        if: ${{{{ needs.ci-required.result != 'success' }}}}\n        run: exit 1",
                crate::control_job_name("Required"),
                self.runner_for(self.control_plane_lane())
            );
        }
    }

    pub(crate) fn render_nightly_alert(
        &self,
        output: &mut String,
        needs_job: &str,
        if_override: Option<&str>,
    ) {
        let if_condition = if_override.map_or_else(
            || {
                if self.control_plane_lane() == RunnerMode::Velnor {
                    format!(
                        "always() && github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
                        self.default_branch
                    )
                } else {
                    "always()".to_owned()
                }
            },
            str::to_owned,
        );
        let _ = writeln!(
            output,
            "  nightly-alert:\n    name: {}\n    if: ${{{{ {if_condition} }}}}\n    needs: [{needs_job}]\n    runs-on: {}\n    permissions:\n      contents: read\n      issues: write\n    steps:\n      - name: Open or update nightly failure signal\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          NIGHTLY_RESULT: ${{{{ needs.{needs_job}.result }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [[ \"$NIGHTLY_RESULT\" == success ]]; then\n            exit 0\n          fi\n          echo \"::error::{needs_job} failed: $NIGHTLY_RESULT\"\n          existing=\"$(gh api \"repos/$GITHUB_REPOSITORY/issues?state=open\" --jq '.[] | select(.title == \"Nightly CI red\") | .number' | sed -n '1p')\"\n          body=\"{needs_job} result: $NIGHTLY_RESULT\nRun: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID\"\n          if [[ -n \"$existing\" ]]; then\n            gh api --method PATCH \"repos/$GITHUB_REPOSITORY/issues/$existing\" -f body=\"$body\" >/dev/null\n          else\n            gh api --method POST \"repos/$GITHUB_REPOSITORY/issues\" -f title='Nightly CI red' -f body=\"$body\" >/dev/null\n          fi",
            crate::control_job_name("Nightly red-to-signal"),
            self.runner_for(self.control_plane_lane())
        );
        let bad_body = format!(
            r#"          body="{needs_job} result: $NIGHTLY_RESULT
Run: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID""#
        );
        let good_body = format!(
            r#"          body="$(printf '%s\n%s\n' \
            "{needs_job} result: $NIGHTLY_RESULT" \
            "Run: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID")""#
        );
        *output = output.replace(&bad_body, &good_body);
    }

    /// The legacy default unit contract: every supported lane, the default
    /// timeout, the detected cache backend, and the unit's own declared cache
    /// contract. A unit that declares a Docker mutable mount seed gets the
    /// seed lifecycle rendered for it exactly as a declared pipeline would —
    /// the default contract defaults the lane surface, never the cache
    /// transport a unit declared.
    pub(crate) fn default_unit_contract(&self, unit: &Unit, cache_save: bool) -> UnitContract {
        UnitContract {
            lanes: Self::default_lane_jobs(self.runners, cache_save),
            timeout_minutes: DEFAULT_UNIT_TIMEOUT_MINUTES,
            cache: CacheBackend::Detected,
            cache_save,
            mutable_mount_seed: unit
                .cache
                .as_ref()
                .is_some_and(|cache| cache.mutable_mount_seed),
        }
    }

    /// The lane jobs a nested unit workflow emits. GitHub is the omitted
    /// default. Velnor-configured repositories emit only the Velnor lane.
    pub(crate) fn default_lane_jobs(runners: RunnerMode, cache_save: bool) -> Vec<LaneJob> {
        match runners {
            RunnerMode::Github => vec![LaneJob {
                lane: RunnerMode::Github,
                cache_save,
                trusted: false,
            }],
            RunnerMode::Velnor => vec![LaneJob {
                lane: RunnerMode::Velnor,
                cache_save: false,
                trusted: true,
            }],
            RunnerMode::Both => vec![
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
            ],
        }
    }

    /// Render one unit's reusable workflow surface.
    ///
    /// The unit contract carries everything a declaring primitive may tune: the
    /// lane jobs to emit, the job timeout, and the cache backend. With the
    /// default contract the output is the surface the generator has always
    /// emitted for a unit.
    pub(crate) fn render_unit_surface(&self, unit: &Unit, contract: &UnitContract) -> String {
        self.render_unit_surface_for_members(unit, contract, &[unit])
    }

    fn render_unit_surface_for_members(
        &self,
        unit: &Unit,
        contract: &UnitContract,
        members: &[&Unit],
    ) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let _ = writeln!(
            output,
            "name: {}\non:\n  workflow_call:\n    inputs:\n      unit:\n        required: true\n        type: string\n      units:\n        required: true\n        type: string\n      scope:\n        required: true\n        type: string\n      full_units:\n        required: true\n        type: string\n      base_sha:\n        required: true\n        type: string\n      head_sha:\n        required: true\n        type: string",
            yaml_scalar(&sidebar_group_name(unit))
        );
        self.render_workflow_env(&mut output, unit);
        output.push_str("\njobs:\n");
        for job in &contract.lanes {
            if lane_supports_unit(job.lane, unit) {
                self.render_lane_job(&mut output, *job, unit, contract, members);
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

    /// The `workflow_call` header of a kind reusable: the graph inputs every
    /// caller passes plus every per-unit input. The per-unit set is declared
    /// in full for every kind, so a caller can pass a unit's facts without
    /// knowing which of them the callee's collapsed block resolved to a
    /// literal: GitHub rejects a `with:` key the callee does not declare.
    /// `env` is the members' agreed job environment, rendered once at the
    /// top level so every collapsed lane job of the kind exports it.
    fn render_kind_units_header(
        kind: UnitKind,
        members: &[&Unit],
        env: &BTreeMap<String, String>,
    ) -> String {
        let mut output = String::from(GENERATED_HEADER);
        let _ = writeln!(
            output,
            "name: {}\non:\n  workflow_call:\n    inputs:\n      unit:\n        required: true\n        type: string\n      selected_units:\n        required: true\n        type: string\n      scope:\n        required: true\n        type: string\n      full_units:\n        required: true\n        type: string\n      base_sha:\n        required: true\n        type: string\n      head_sha:\n        required: true\n        type: string\n      lane:\n        required: true\n        type: string",
            yaml_scalar(unit_group(kind))
        );
        for name in lane_input::ALL {
            // The prepared-tools input is declared only when a member needs
            // it: an unconditional declaration would rewrite every kind
            // header for a feature most repositories never declare.
            if *name == lane_input::PREPARED_TOOLS
                && !members.iter().any(|unit| !unit.prepared_tools.is_empty())
            {
                continue;
            }
            let _ = writeln!(output, "{}", lane_input::declaration(name));
        }
        if !env.is_empty() {
            output.push_str("\nenv:\n");
            for (name, value) in env {
                let _ = writeln!(output, "  {name}: {}", yaml_scalar(value));
            }
        }
        output.push_str("\njobs:\n");
        output
    }

    /// One reusable workflow for every unit of `kind`. Callers pass `unit`,
    /// `lane`, and the unit's per-unit inputs through `workflow_call`; the
    /// reusable holds one collapsed step block per lane job, so its size does
    /// not grow with the number of units of the kind. GitHub loads the callee
    /// once per caller into one template-memory budget, which is why the
    /// callee must stay O(1) in units.
    ///
    /// # Errors
    /// Returns a usage error when members of the kind disagree on a fact the
    /// collapsed job renders literally (the Rust toolchain pin, the kind-level
    /// tools, or the cache-save policy).
    pub(crate) fn render_kind_unit_workflow(
        &self,
        kind: UnitKind,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Result<Option<(String, String)>, GeneratorError> {
        let members = self
            .units
            .iter()
            .filter(|unit| unit.kind == kind)
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Ok(None);
        }
        let env = crate::platform::agreed_env(&members, kind)?;
        let mut output = Self::render_kind_units_header(kind, &members, &env);
        self.append_lane_cargo_prep_jobs(&mut output, &members, contracts);
        self.render_collapsed_kind_verify_job(&mut output, &members, contracts)?;
        Ok(Some((kind_unit_workflow_file(kind), output)))
    }

    fn contract_for(
        &self,
        unit: &Unit,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> UnitContract {
        contracts
            .and_then(|contracts| contracts.get(&unit.id))
            .cloned()
            .unwrap_or_else(|| self.default_unit_contract(unit, true))
    }

    /// The job gate of a collapsed lane job: the lane the caller selected, the
    /// unit's membership in the plan's selection, and the lane's admission
    /// predicate. Membership is a single `contains` over `inputs.unit`, never
    /// an enumeration of the kind's units. A kind split across executors adds
    /// the `apple_executor` clause, so each partition admits only its own
    /// callers; an unsplit kind carries no clause.
    fn collapsed_lane_gate(&self, admission: LaneAdmission, apple: Option<bool>) -> String {
        let mut gate = format!(
            "inputs.lane == '{}' && contains(format(',{{0}},', inputs.selected_units), format(',{{0}},', inputs.unit)) && ({})",
            admission.lane().as_str(),
            self.lane_admission_expression(admission)
        );
        match apple {
            None => {}
            Some(true) => gate.push_str(" && inputs.apple_executor"),
            Some(false) => gate.push_str(" && inputs.apple_executor != true"),
        }
        gate
    }

    fn collapsed_timeout_minutes(
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> u32 {
        members
            .iter()
            .map(|unit| {
                contracts
                    .and_then(|contracts| contracts.get(&unit.id))
                    .map_or(DEFAULT_UNIT_TIMEOUT_MINUTES, |contract| {
                        contract.timeout_minutes
                    })
            })
            .max()
            .unwrap_or(DEFAULT_UNIT_TIMEOUT_MINUTES)
    }

    fn collapsed_lane_members<'a>(
        &self,
        members: &[&'a Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
        lane: RunnerMode,
        trusted_only: Option<bool>,
    ) -> Vec<&'a Unit> {
        members
            .iter()
            .copied()
            .filter(|unit| {
                let contract = self.contract_for(unit, contracts);
                let active = contract
                    .lanes
                    .iter()
                    .any(|job| job.lane == lane && lane_supports_unit(lane, unit));
                active && trusted_only.is_none_or(|trusted| unit.requires_trusted == trusted)
            })
            .collect()
    }

    /// Render the collapsed lane jobs of one kind: one job per (lane, trust)
    /// class that has members.
    fn render_collapsed_kind_verify_job(
        &self,
        output: &mut String,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Result<(), GeneratorError> {
        if members.is_empty() {
            return Ok(());
        }
        let github_members =
            self.collapsed_lane_members(members, contracts, RunnerMode::Github, None);
        // A kind split across executors (portable SwiftPM units beside Xcode
        // scheme units) renders one collapsed job per executor: a single
        // `runs-on` cannot serve both. An unsplit kind keeps the one job it
        // has always rendered. Each side samples its first member instead of
        // the lane default so platform-bound members land on an eligible
        // executor (a lane-blind default silently scheduled Apple work on
        // Linux).
        let (github_apple, github_default): (Vec<_>, Vec<_>) = github_members
            .into_iter()
            .partition(|unit| unit.platform.requires_apple());
        let split = !github_apple.is_empty() && !github_default.is_empty();
        if !github_default.is_empty() {
            let runs_on = self.runner_for_unit(RunnerMode::Github, github_default[0]);
            self.render_collapsed_lane_verify_job(
                output,
                &github_default,
                contracts,
                RunnerMode::Github,
                "verify-github",
                RunnerMode::Github.display_name(),
                &runs_on,
                split.then_some(false),
            )?;
        }
        if !github_apple.is_empty() {
            let runs_on = self.runner_for_unit(RunnerMode::Github, github_apple[0]);
            let (job_id, display_name) = if split {
                (
                    "verify-github-apple",
                    format!("{} · Apple", RunnerMode::Github.display_name()),
                )
            } else {
                (
                    "verify-github",
                    RunnerMode::Github.display_name().to_owned(),
                )
            };
            self.render_collapsed_lane_verify_job(
                output,
                &github_apple,
                contracts,
                RunnerMode::Github,
                job_id,
                &display_name,
                &runs_on,
                split.then_some(true),
            )?;
        }
        let velnor_plain =
            self.collapsed_lane_members(members, contracts, RunnerMode::Velnor, Some(false));
        let velnor_trusted =
            self.collapsed_lane_members(members, contracts, RunnerMode::Velnor, Some(true));
        if !velnor_plain.is_empty() {
            let runs_on = self.runner_for_unit(RunnerMode::Velnor, velnor_plain[0]);
            self.render_collapsed_lane_verify_job(
                output,
                &velnor_plain,
                contracts,
                RunnerMode::Velnor,
                "verify-velnor",
                RunnerMode::Velnor.display_name(),
                &runs_on,
                None,
            )?;
        }
        if !velnor_trusted.is_empty() {
            let sample = velnor_trusted[0];
            let runs_on = self.runner_for_unit(RunnerMode::Velnor, sample);
            self.render_collapsed_lane_verify_job(
                output,
                &velnor_trusted,
                contracts,
                RunnerMode::Velnor,
                "verify-velnor-trusted",
                RunnerMode::Velnor.display_name(),
                &runs_on,
                None,
            )?;
        }
        Ok(())
    }

    /// One collapsed lane job: the job header, the shared runner setup, and
    /// exactly one step block that reads every unit-specific value from the
    /// caller's inputs.
    #[expect(
        clippy::too_many_arguments,
        reason = "the job identity (lane, id, display name, runner, executor partition) is passed explicitly per lane job"
    )]
    fn render_collapsed_lane_verify_job(
        &self,
        output: &mut String,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
        lane: RunnerMode,
        job_id: &str,
        display_name: &str,
        runs_on: &str,
        apple: Option<bool>,
    ) -> Result<(), GeneratorError> {
        // Every member of a collapsed job shares one admission class
        // (`collapsed_lane_members` splits the Velnor lane by trust), so the
        // first member's class is the job's.
        let gate = self.collapsed_lane_gate(LaneAdmission::for_unit(lane, members[0]), apple);
        let mut display_name = display_name.to_owned();
        // P0-6: a trust-gated Velnor lane with no online trusted runner fails
        // closed (the admission predicate carries `&& false`) and says so in
        // its name, instead of queueing forever and holding the aggregate's
        // concurrency slot.
        if let Some(unit) = members
            .iter()
            .find(|unit| self.trust_gated_velnor_job_skipped(lane, unit))
        {
            if let Some(reason) = self.velnor_trusted_runner_skip_reason.as_deref() {
                let _ = writeln!(output, "  # Velnor trusted runner unavailable: {reason}");
            }
            display_name = self.trusted_unit_display_name(lane, unit, display_name);
        }
        let display_name = yaml_scalar(&display_name);
        let _ = writeln!(
            output,
            "  {job_id}:\n    name: {display_name}\n    if: ${{{{ {gate} }}}}\n    runs-on: {runs_on}\n    timeout-minutes: {}",
            Self::collapsed_timeout_minutes(members, contracts),
        );
        output.push_str("    steps:\n");
        render_ci_job_started_marker(output);
        let _ = writeln!(
            output,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}",
            self.pins.checkout
        );
        render_unit_dependency_info_step(output);
        if lane == RunnerMode::Velnor {
            render_velnor_runner_identity_step(output);
        }
        if lane == RunnerMode::Github && self.runners != RunnerMode::Velnor {
            self.render_unit_runtime(output, lane, members[0]);
        }
        render_ci_runner_setup_end_marker(output);
        output.push_str(&workflow_selection_file_materialize(
            &SelectionFieldSources {
                base_sha: "${{ inputs.base_sha }}",
                head_sha: "${{ inputs.head_sha }}",
                scope: "${{ inputs.scope }}",
                units: "${{ inputs.selected_units }}",
                full_units: "${{ inputs.full_units }}",
            },
        ));
        render_ci_selection_end_marker(output);
        self.render_collapsed_lane_steps(output, lane, members, contracts)?;
        output.push('\n');
        Ok(())
    }

    /// The per-unit facts of one (unit, lane) pair. This is the single source
    /// both sides of the `workflow_call` boundary derive from: the caller's
    /// `with:` values and the callee's step gates.
    pub(crate) fn unit_lane_facts(
        &self,
        unit: &Unit,
        contract: &UnitContract,
        lane: RunnerMode,
    ) -> LaneStepFacts {
        let github_lane = lane == RunnerMode::Github;
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let mise_tools = if github_lane {
            if tools.contains(&ToolRequirement::Mise) {
                mise_tool_ids(unit, &self.mise_lock_keys)
            } else {
                Vec::new()
            }
        } else if tools.contains(&ToolRequirement::Mise) {
            velnor_mise_install_tool_ids(unit, &self.mise_lock_keys)
        } else {
            Vec::new()
        };
        let mise_runner = github_lane
            && tools.contains(&ToolRequirement::Mise)
            && mise_tools.is_empty()
            && commands_invoke_mise(unit);
        let mbx_enabled = tools.contains(&ToolRequirement::MrBoxington);
        let mbx = (github_lane && tools.contains(&ToolRequirement::MrBoxington))
            .then(|| unit_snapshot_facts(self, unit));
        let cargo_bin_tools = if github_lane {
            self.cargo_bin_tools(&tools)
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let tool_version = (github_lane && tools.contains(&ToolRequirement::Bun))
            .then(|| unit.tool_version.clone())
            .flatten();
        let node_cache_dependency_path = (github_lane && tools.contains(&ToolRequirement::Node))
            .then(|| {
                unit.cache.as_ref().and_then(|cache| {
                    cache
                        .key_files
                        .iter()
                        .find(|path| path.ends_with("package-lock.json"))
                        .cloned()
                })
            })
            .flatten();
        let seed = (contract.mutable_mount_seed && github_lane)
            .then(|| {
                unit.cache
                    .as_ref()
                    .map(|cache| (cache.paths.clone(), unit_snapshot_facts(self, unit)))
            })
            .flatten();
        let bundle = if seed.is_none()
            && contract.cache.lane_enables_actions_cache(lane, self, unit)
            && let Some(cache) = &unit.cache
        {
            Some((cache.paths.clone(), cargo_source_cache_key_files(unit)))
        } else {
            None
        };
        let skip_cargo_fetch = cargo_network_is_restricted(unit)
            && lane == RunnerMode::Velnor
            && self.runners == RunnerMode::Both;
        let cargo_root =
            (cargo_network_is_restricted(unit) && !skip_cargo_fetch).then(|| unit.root.clone());
        let cargo_fetch_skip_when_warm = cargo_root.is_some()
            && lane == RunnerMode::Velnor
            && unit
                .cache
                .as_ref()
                .is_some_and(cache_is_velnor_host_persistent);
        LaneStepFacts {
            mise_tools,
            mise_runner,
            mbx_enabled,
            mbx,
            cargo_bin_tools,
            tool_version,
            node_cache_dependency_path,
            bundle,
            seed,
            cargo_root,
            cargo_fetch_skip_when_warm,
            cargo_net_offline: cargo_network_is_restricted(unit),
            host_warm_layers: if lane == RunnerMode::Velnor {
                velnor_host_warm_layers(unit, self)
            } else {
                Vec::new()
            },
            policy_runtime: lane == RunnerMode::Velnor && unit_runs_workflow_plain_check(unit),
            candidate_publish: lane == RunnerMode::Github
                && !self.repository.is_empty()
                && self.repository == crate::workflow_setup_action_repository()
                && unit_owns_workflow_crate(unit),
            apple_executor: github_lane && unit.platform.requires_apple(),
            unit_dependencies: unit.depends_on.clone(),
            unit_admission: LaneAdmission::for_unit(lane, unit),
            prepared_tools: unit
                .prepared_tools
                .iter()
                .map(super::prepared_tools::need_record)
                .collect(),
        }
    }

    /// Whether the collapsed lane job renders the cargo-fetch phase at all:
    /// mirrors the literal lane job, which renders the phase (fetch step and
    /// marker) unless both-mode moved the fetch to the lane prep job.
    fn collapsed_renders_cargo_fetch_phase(&self, unit: &Unit, lane: RunnerMode) -> bool {
        !(cargo_network_is_restricted(unit)
            && lane == RunnerMode::Velnor
            && self.runners == RunnerMode::Both)
    }

    /// The union of prepared-tool consumer steps across the collapsed lane's
    /// members: one restore+install block per distinct need record, with the
    /// requested and resolved keys rendered as literals from the union.
    /// Members that agree share one ungated block; a record only some
    /// members carry renders behind a membership gate over the caller's
    /// records, and members with no needs pass an empty input and run none
    /// of the blocks. Blocks scale with distinct needs — one tool bound to
    /// one inputs digest renders once whatever the member count — never
    /// with units.
    fn render_collapsed_prepared_tool_steps(
        &self,
        output: &mut String,
        members: &[&Unit],
        facts: &[LaneStepFacts],
    ) {
        let member_records: Vec<BTreeSet<String>> = facts
            .iter()
            .map(|facts| facts.prepared_tools.iter().cloned().collect())
            .collect();
        if member_records.iter().all(BTreeSet::is_empty) {
            return;
        }
        let mut union: BTreeMap<String, &Unit> = BTreeMap::new();
        for (unit, records) in members.iter().zip(&member_records) {
            for record in records {
                union.entry(record.clone()).or_insert(unit);
            }
        }
        for (record, unit) in &union {
            let Some(need) = unit
                .prepared_tools
                .iter()
                .find(|need| super::prepared_tools::need_record(need) == *record)
            else {
                continue;
            };
            let shared = member_records
                .iter()
                .all(|records| records.contains(record));
            let gate =
                (!shared).then(|| lane_input::contains_gate(lane_input::PREPARED_TOOLS, record));
            let mut block = String::new();
            super::prepared_tools::render_consumer_steps(
                &mut block,
                self.pins.cache_restore,
                std::slice::from_ref(need),
            );
            output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
        }
    }

    /// The single step block of a collapsed lane job. Every unit-specific
    /// value is an `inputs.*` reference; a feature only some members carry
    /// renders behind an input presence gate.
    #[expect(
        clippy::too_many_lines,
        reason = "the collapsed step block keeps the complete lane contract together, in step order"
    )]
    fn render_collapsed_lane_steps(
        &self,
        output: &mut String,
        lane: RunnerMode,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Result<(), GeneratorError> {
        let github_lane = lane == RunnerMode::Github;
        let member_contracts = members
            .iter()
            .map(|unit| self.contract_for(unit, contracts))
            .collect::<Vec<_>>();
        let facts = members
            .iter()
            .zip(&member_contracts)
            .map(|(unit, contract)| self.unit_lane_facts(unit, contract, lane))
            .collect::<Vec<_>>();
        let kind = members[0].kind;
        let disagreement = |what: &str| {
            GeneratorError::usage(format!(
                "collapsed {} lane job for {} units cannot render one step block: members disagree on {what}",
                lane.as_str(),
                kind.label(),
            ))
        };
        let cache_save = {
            let saves = members
                .iter()
                .zip(&member_contracts)
                .map(|(_, contract)| {
                    contract.cache_save
                        && contract
                            .lanes
                            .iter()
                            .any(|job| job.lane == lane && job.cache_save)
                })
                .collect::<BTreeSet<_>>();
            if saves.len() > 1 {
                return Err(disagreement("the cache-save policy"));
            }
            saves.into_iter().next().unwrap_or(false)
        };
        let toolchain = {
            let pins = members
                .iter()
                .map(|unit| unit.toolchain.clone())
                .collect::<Vec<_>>();
            if pins.iter().any(|pin| pin != &pins[0]) {
                return Err(disagreement("the Rust toolchain pin"));
            }
            pins.into_iter().next().flatten()
        };
        let kind_tools = {
            let per_member = members
                .iter()
                .map(|unit| {
                    Self::tools_for_unit(unit, self.mise_present, self.mr_boxington)
                        .into_iter()
                        .filter(|tool| {
                            !matches!(
                                tool,
                                ToolRequirement::Mise
                                    | ToolRequirement::Nextest
                                    | ToolRequirement::CargoDeny
                                    | ToolRequirement::CargoAudit
                                    // MBX and sccache are mutually exclusive
                                    // per-member transports, not kind-level
                                    // tools. Their setup is gated below from
                                    // the selected member's typed input.
                                    | ToolRequirement::MrBoxington
                                    | ToolRequirement::Sccache
                            )
                        })
                        .collect::<BTreeSet<_>>()
                })
                .collect::<Vec<_>>();
            if per_member.iter().any(|tools| tools != &per_member[0]) {
                return Err(disagreement("the kind-level tool set"));
            }
            per_member.into_iter().next().unwrap_or_default()
        };
        let gated = |block: String, coverage: FeatureCoverage, input: &str| -> String {
            prefix_step_block_with_if(&block, coverage.gate(input).as_deref())
        };

        // The pinned policy runtime for units that run the generator's own
        // `--check`: hosted lanes carry it in the Planning runtime artifact,
        // the Velnor lane builds it into the host's persistent store.
        let policy_runtime = FeatureCoverage::over(&facts, |facts| facts.policy_runtime);
        if !github_lane && policy_runtime.any {
            output.push_str(&gated(
                crate::workflow_pinned_policy_runtime_velnor("${{ github.workspace }}"),
                policy_runtime,
                lane_input::POLICY_RUNTIME,
            ));
        }

        // Tool provisioning, in the literal lane job's order.
        let mise_tools = FeatureCoverage::over(&facts, |facts| !facts.mise_tools.is_empty());
        if !github_lane && mise_tools.any {
            let mut block = String::new();
            render_velnor_mise_install_from_input(&mut block);
            output.push_str(&gated(block, mise_tools, lane_input::MISE_TOOLS));
        }
        if !velnor_skips_pinned_rust_toolchain(lane)
            && let Some(toolchain) = &toolchain
        {
            self.render_rust_toolchain_steps(output, toolchain, cache_save);
        }
        if github_lane && mise_tools.any {
            let trusted = trusted_cache_save_expression(&self.default_branch);
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Set up Mise tools\n        uses: {}\n        with:\n          install_args: {}\n          cache: true\n          cache_save: ${{{{ {trusted} }}}}",
                self.pins.mise,
                lane_input::expression(lane_input::MISE_TOOLS)
            );
            output.push_str(&gated(block, mise_tools, lane_input::MISE_TOOLS));
        }
        let mise_runner = FeatureCoverage::over(&facts, |facts| facts.mise_runner);
        if github_lane && mise_runner.any {
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Set up Mise\n        uses: {}\n        with:\n          install: false",
                self.pins.mise
            );
            output.push_str(&gated(block, mise_runner, lane_input::MISE_RUNNER));
        }
        let mbx = FeatureCoverage::over(&facts, |facts| facts.mbx_enabled);
        if mbx.any {
            let mut block = String::new();
            if github_lane {
                let (cache_key, restore_keys) = input_snapshot(
                    UNIT_SNAPSHOT_NAMESPACE,
                    lane_input::MBX_COMPAT,
                    lane_input::MBX_DEPENDENCY_FILES,
                    lane_input::MBX_FRESHNESS_FILES,
                );
                self.render_mbx_github_step(&mut block, &cache_key, &restore_keys);
            } else {
                self.render_mbx_local_step(&mut block);
            }
            output.push_str(&gated(block, mbx, lane_input::MBX_ENABLED));
        }
        let sccache =
            FeatureCoverage::over(&facts, |facts| kind == UnitKind::Rust && !facts.mbx_enabled);
        if github_lane && sccache.any {
            let mut block = String::new();
            let sccache_tools = BTreeSet::from([ToolRequirement::Sccache]);
            self.render_kind_level_tool_steps(&mut block, lane, &sccache_tools, cache_save);
            output.push_str(&prefix_step_block_with_if(
                &block,
                sccache.absent_gate(lane_input::MBX_ENABLED).as_deref(),
            ));
            render_collapsed_sccache_env_step(output, sccache, lane_input::MBX_ENABLED);
        }
        if github_lane && kind_tools.contains(&ToolRequirement::Bun) {
            let versioned = FeatureCoverage::over(&facts, |facts| facts.tool_version.is_some());
            if versioned.any {
                let mut block = String::new();
                let _ = writeln!(
                    block,
                    "      - name: Set up Bun\n        uses: {}\n        with:\n          bun-version: {}",
                    self.pins.bun,
                    lane_input::expression(lane_input::TOOL_VERSION)
                );
                output.push_str(&gated(block, versioned, lane_input::TOOL_VERSION));
            }
            if !versioned.all {
                let gate = versioned
                    .any
                    .then(|| format!("inputs.{} == ''", lane_input::TOOL_VERSION));
                let block = format!(
                    "      - name: Set up Bun\n        uses: {}\n",
                    self.pins.bun
                );
                output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
            }
        }
        if github_lane && kind_tools.contains(&ToolRequirement::Node) {
            let cached =
                FeatureCoverage::over(&facts, |facts| facts.node_cache_dependency_path.is_some());
            if cached.any {
                let mut block = String::new();
                self.render_node_step(
                    &mut block,
                    Some(&lane_input::expression(
                        lane_input::NODE_CACHE_DEPENDENCY_PATH,
                    )),
                );
                output.push_str(&gated(
                    block,
                    cached,
                    lane_input::NODE_CACHE_DEPENDENCY_PATH,
                ));
            }
            if !cached.all {
                let gate = cached
                    .any
                    .then(|| format!("inputs.{} == ''", lane_input::NODE_CACHE_DEPENDENCY_PATH));
                let mut block = String::new();
                self.render_node_step(&mut block, None);
                output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
            }
        }
        let cargo_bin = FeatureCoverage::over(&facts, |facts| !facts.cargo_bin_tools.is_empty());
        if github_lane && cargo_bin.any {
            let mut block = String::new();
            self.render_cargo_bin_tool_steps(
                &mut block,
                &lane_input::expression(lane_input::CARGO_BIN_TOOLS),
                cache_save,
            );
            output.push_str(&gated(block, cargo_bin, lane_input::CARGO_BIN_TOOLS));
        }
        self.render_collapsed_prepared_tool_steps(output, members, &facts);
        self.render_kind_level_tool_steps(output, lane, &kind_tools, cache_save);
        render_ci_tool_bootstrap_end_marker(output);

        // Cache preparation.
        let checks_offline = FeatureCoverage::over(&facts, |facts| facts.cargo_net_offline);
        let checks_env = collapsed_checks_env(checks_offline);
        let seed = FeatureCoverage::over(&facts, |facts| facts.seed.is_some());
        let bundle = FeatureCoverage::over(&facts, |facts| facts.bundle.is_some());
        if seed.any {
            let mut block = String::new();
            render_mutable_mount_seed_restore_from_input(&mut block, self, &checks_env);
            output.push_str(&gated(block, seed, lane_input::SEED_COMPAT));
        }
        if bundle.any {
            for unit in members {
                if let Some(cache) = &unit.cache {
                    render_retained_output_cache_note(output, self, unit, cache);
                }
            }
            let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(
                kind.id_prefix(),
                &format!("inputs.{}", lane_input::CACHE_KEY_FILES),
            );
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Restore unit cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n            {}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                self.pins.cache_restore,
                lane_input::expression(lane_input::CACHE_PATHS),
            );
            output.push_str(&gated(block, bundle, lane_input::CACHE_KEY_FILES));
        }
        render_ci_cache_prep_end_marker(output);

        // Cargo source preparation.
        let fetch_phase = members
            .iter()
            .any(|unit| self.collapsed_renders_cargo_fetch_phase(unit, lane));
        if fetch_phase {
            let fetch = FeatureCoverage::over(&facts, |facts| facts.cargo_root.is_some());
            if fetch.any {
                let mut conditions = Vec::new();
                if let Some(gate) = fetch.gate(lane_input::CARGO_ROOT) {
                    conditions.push(gate);
                }
                if bundle.any {
                    // A restored dependency bundle skips the fetch; a member
                    // without a bundle has no `cache` step and fetches always.
                    let hit = "steps.cache.outputs.cache-hit != 'true'";
                    if bundle.all || !seed.any {
                        conditions.push(hit.to_owned());
                    } else {
                        conditions.push(format!(
                            "(inputs.{} == '' || {hit})",
                            lane_input::CACHE_KEY_FILES
                        ));
                    }
                }
                let skip_when_warm =
                    FeatureCoverage::over(&facts, |facts| facts.cargo_fetch_skip_when_warm);
                let gate = (!conditions.is_empty()).then(|| conditions.join(" && "));
                render_cargo_source_preparation_from_input(output, gate.as_deref(), skip_when_warm);
            }
            render_ci_cargo_fetch_end_marker(output);
        }

        // Verification. No pin-fetch step: the tool fetches the declared pin
        // itself before closure verification, so no lane needs its own.
        let checks_started_marker = render_epoch_marker_commands("CHECKS_STARTED", "          ");
        let checks_ended_marker = render_epoch_marker_commands("CHECKS_ENDED", "          ");
        let token_env = docker_build_token_env_for_members(lane, members);
        let _ = writeln!(
            output,
            "      - name: Run unit checks\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ inputs.base_sha }}}}\n          HEAD_SHA: ${{{{ inputs.head_sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{checks_env}{token_env}\n        run: |\n          set -o pipefail\n{checks_started_marker}\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n{checks_ended_marker}\n          exit $rc",
        );

        // Stage-1 candidate packaging, after the checks that build the
        // binary it reuses: only the hosted job of the generator crate's
        // owning unit publishes, only on pull requests (the steps carry
        // that event gate themselves), and only on success (no `always()`).
        let candidate = FeatureCoverage::over(&facts, |facts| facts.candidate_publish);
        if github_lane && candidate.any {
            let mut block = String::new();
            let candidate_event_gate =
                "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository";
            let candidate_event_complete_gate =
                "always() && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate_publication.outcome == 'success'";
            render_phase_epoch_marker_if(
                &mut block,
                "CANDIDATE_STARTED",
                "Mark candidate phase start",
                candidate_event_gate,
            );
            block.push_str(&candidate_publish_steps(self.pins.upload_artifact));
            render_phase_epoch_marker_if(
                &mut block,
                "CANDIDATE_ENDED",
                "Mark candidate phase end",
                candidate_event_complete_gate,
            );
            output.push_str(&gated(block, candidate, lane_input::CANDIDATE_PUBLISH));
        }

        // Cache collection.
        let cache_save_bundle = cache_save && github_lane && bundle.any;
        if seed.any {
            let mut block = String::new();
            render_mutable_mount_seed_collection_from_input(
                &mut block,
                self,
                &checks_env,
                cache_save,
            );
            output.push_str(&gated(block, seed, lane_input::SEED_COMPAT));
        }
        if cache_save_bundle {
            let (cache_key, _) = format_cargo_bundle_cache_key(
                kind.id_prefix(),
                &format!("inputs.{}", lane_input::CACHE_KEY_FILES),
            );
            let mut block = String::new();
            let save_gate = dependency_bundle_cache_save_if(&self.default_branch);
            render_cache_save_start_marker(&mut block, "BUNDLE", &save_gate);
            let _ = writeln!(
                block,
                "      - name: Save unit cache\n        id: unit_cache_save\n        if: {save_gate}\n        uses: {}\n        with:\n          path: |\n            {}\n          key: {cache_key}",
                self.pins.cache_save,
                lane_input::expression(lane_input::CACHE_PATHS),
            );
            render_cache_save_end_marker(&mut block, "BUNDLE", &save_gate, "unit_cache_save");
            output.push_str(&gated(block, bundle, lane_input::CACHE_KEY_FILES));
        }
        let report = members
            .iter()
            .map(|unit| CacheReportFacts::for_unit(lane, unit, self))
            .collect::<Vec<_>>();
        render_phase_report_step(
            output,
            &self.ci_report_action_uses(),
            &lane_input::expression("unit"),
            lane,
            &CacheReportFacts::union(&report),
        );
        Ok(())
    }

    fn append_lane_cargo_prep_jobs(
        &self,
        output: &mut String,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) {
        let roots = cargo_fetch_roots(members);
        if roots.is_empty() {
            return;
        }
        let Some(if_gate) = restricted_unit_selection_if(members) else {
            return;
        };
        let Some(cache_unit) = cargo_prep_cache_unit(members) else {
            return;
        };
        let skip_when_offline_ready = cache_unit
            .cache
            .as_ref()
            .is_some_and(cache_is_velnor_host_persistent);
        let fetch_script = render_cargo_fetch_roots_script(&roots, skip_when_offline_ready);
        // Only Velnor runners share persistent Cargo stores between jobs.
        // GitHub-hosted jobs must fetch into their own ephemeral workspace.
        for lane in [RunnerMode::Velnor] {
            if !members.iter().any(|unit| {
                let contract = contracts
                    .and_then(|contracts| contracts.get(&unit.id))
                    .cloned()
                    .unwrap_or_else(|| self.default_unit_contract(unit, true));
                contract
                    .lanes
                    .iter()
                    .any(|job| job.lane == lane && lane_supports_unit(lane, unit))
            }) {
                continue;
            }
            let prep_id = format!("{}-prepare-cargo-sources", lane.as_str());
            let intended_lane = if self.runners == RunnerMode::Both {
                "control"
            } else {
                lane.as_str()
            };
            // The prep job warms the lane's stores, so it is admitted exactly
            // when the lane's base (non-trust-gated) jobs are.
            let prep_gate = format!(
                "inputs.lane == '{intended_lane}' && ({if_gate}) && ({})",
                self.lane_admission_expression(LaneAdmission::Velnor)
            );
            let _ = writeln!(
                output,
                "  {prep_id}:\n    name: prepare-cargo\n    if: ${{{{ {prep_gate} }}}}\n    runs-on: {}\n    timeout-minutes: 20\n    steps:",
                self.runner_for(lane),
            );
            let _ = writeln!(
                output,
                "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}",
                self.pins.checkout
            );
            output.push_str(&workflow_selection_file_materialize(
                &SelectionFieldSources {
                    base_sha: "${{ inputs.base_sha }}",
                    head_sha: "${{ inputs.head_sha }}",
                    scope: "${{ inputs.scope }}",
                    units: "${{ inputs.selected_units }}",
                    full_units: "${{ inputs.full_units }}",
                },
            ));
            if CacheBackend::Detected.lane_enables_actions_cache(lane, self, cache_unit)
                && let Some(cache) = &cache_unit.cache
            {
                let (paths, _) = rendered_cache_values(cache);
                let id_segment = cache_unit.kind.id_prefix();
                let hash = cargo_source_cache_hash_expression(cache_unit);
                let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(id_segment, &hash);
                let _ = writeln!(
                    output,
                    "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                    yaml_scalar(&cache_unit.label),
                    self.pins.cache_restore,
                );
            }
            let cache_hit_gate =
                if CacheBackend::Detected.lane_enables_actions_cache(lane, self, cache_unit) {
                    "        if: ${{ steps.cache.outputs.cache-hit != 'true' }}\n"
                } else {
                    ""
                };
            let _ = write!(
                output,
                "      - name: Prepare Cargo sources\n{cache_hit_gate}        env:{}\n        run: |\n{fetch_script}",
                preparation_env()
            );
        }
    }

    /// The kind reusable's content, for tests that assert on its body.
    #[cfg(test)]
    #[expect(
        clippy::panic,
        reason = "test helper: a render error is a test failure"
    )]
    pub(crate) fn render_kind_units(
        &self,
        kind: UnitKind,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> String {
        self.render_kind_unit_workflow(kind, contracts)
            .unwrap_or_else(|error| panic!("render {} kind reusable: {error}", kind.label()))
            .map(|(_, content)| content)
            .unwrap_or_default()
    }

    pub(crate) fn render_workflow_env(&self, output: &mut String, unit: &Unit) {
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let mold = self.mise_present && unit.kind != UnitKind::Swift;
        let mut entries: Vec<String> = Vec::new();
        // Declared env shadows the generator defaults key by key, so a
        // repository-owned build flag always wins and no key renders twice.
        let shadowed = |key: &str| unit.env.contains_key(key);
        if tools.contains(&ToolRequirement::Sccache) {
            if !shadowed("CARGO_INCREMENTAL") {
                entries.push("  CARGO_INCREMENTAL: \"0\"".to_owned());
            }
            if !shadowed("RUSTC_WRAPPER") {
                entries.push("  RUSTC_WRAPPER: sccache".to_owned());
            }
            if !shadowed("SCCACHE_GHA_ENABLED") {
                entries.push("  SCCACHE_GHA_ENABLED: \"true\"".to_owned());
            }
        }
        if mold && !shadowed("RUSTFLAGS") {
            entries.push("  RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"".to_owned());
        }
        if tools.contains(&ToolRequirement::OpenTofu) && !shadowed("TF_PLUGIN_CACHE_DIR") {
            entries.push("  TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache".to_owned());
        }
        let mut declared = unit.env.iter().collect::<Vec<_>>();
        declared.sort();
        for (name, value) in declared {
            entries.push(format!("  {name}: {}", yaml_scalar(value)));
        }
        if entries.is_empty() {
            return;
        }
        output.push_str("\nenv:\n");
        for entry in entries {
            output.push_str(&entry);
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
        members: &[&Unit],
    ) {
        self.render_lane_job_for_input(output, job, unit, contract, None, members);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each generated lane job keeps its complete setup and execution contract together"
    )]
    fn render_lane_job_for_input(
        &self,
        output: &mut String,
        job: LaneJob,
        unit: &Unit,
        contract: &UnitContract,
        input_unit: Option<&str>,
        members: &[&Unit],
    ) {
        let lane = job.lane;
        let id = input_unit.map_or_else(
            || lane.as_str().to_owned(),
            |unit_id| unit_job_id(lane, unit_id),
        );
        let cache_save = job.cache_save && contract.cache_save;
        let report_label = if input_unit.is_some() {
            unit.id.clone()
        } else {
            lane.display_name().to_owned()
        };
        let name = self.trusted_unit_display_name(lane, unit, report_label.clone());
        // Both-mode prep and dependency closure live on aggregate callers (D6).
        let uses_lane_cargo_prep = input_unit.is_some()
            && lane == RunnerMode::Velnor
            && cargo_network_is_restricted(unit)
            && !members.is_empty()
            && self.runners != RunnerMode::Both;
        if self.trust_gated_velnor_job_skipped(lane, unit)
            && let Some(reason) = self.velnor_trusted_runner_skip_reason.as_deref()
        {
            let _ = writeln!(output, "  # Velnor trusted runner unavailable: {reason}");
        }
        let _ = writeln!(output, "  {id}:\n    name: {}", yaml_scalar(&name));
        let lane_gate = self.lane_admission_expression(LaneAdmission::for_unit(lane, unit));
        let gate = input_unit.map_or_else(
            || format!("({lane_gate})"),
            |unit_id| {
                format!(
                    "inputs.lane == '{}' && {} && ({lane_gate})",
                    lane.as_str(),
                    reusable_selected_unit_selector(unit_id)
                )
            },
        );
        let _ = writeln!(output, "    if: ${{{{ {gate} }}}}");
        let mut needs = Vec::new();
        if uses_lane_cargo_prep {
            needs.push(format!("{}-prepare-cargo-sources", lane.as_str()));
        }
        if self.runners != RunnerMode::Both {
            append_unique_needs(
                &mut needs,
                velnor_rust_dependency_needs(lane, unit, self.velnor_rust_needs, &self.units),
            );
        }
        if !needs.is_empty() {
            let _ = writeln!(output, "    needs: [{}]", needs.join(", "));
        }
        let _ = writeln!(output, "    runs-on: {}", self.runner_for_unit(lane, unit));
        if input_unit.is_some() {
            self.render_job_env(output, lane, unit);
        }
        let _ = writeln!(output, "    timeout-minutes: {}", contract.timeout_minutes);
        Self::render_job_services(output, unit);
        output.push_str("    steps:\n");
        render_ci_job_started_marker(output);
        let _ = write!(
            output,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}",
            self.pins.checkout
        );
        if lane == RunnerMode::Velnor {
            output.push('\n');
            output.push_str("      - name: Velnor runner identity\n");
            render_velnor_runner_identity_step(output);
        }
        self.render_unit_runtime(output, lane, unit);
        render_ci_runner_setup_end_marker(output);
        let units_source =
            input_unit.map_or("${{ inputs.units }}", |_| "${{ inputs.selected_units }}");
        output.push_str(&workflow_selection_file_materialize(
            &SelectionFieldSources {
                base_sha: "${{ inputs.base_sha }}",
                head_sha: "${{ inputs.head_sha }}",
                scope: "${{ inputs.scope }}",
                units: units_source,
                full_units: "${{ inputs.full_units }}",
            },
        ));
        render_ci_selection_end_marker(output);
        self.render_tool_provisioning(output, lane, unit, cache_save);
        render_ci_tool_bootstrap_end_marker(output);
        let seed = contract.mutable_mount_seed;
        let skip_fetch_on_cache_hit = if seed && lane == RunnerMode::Github {
            render_mutable_mount_seed_restore(output, self, unit);
            false
        } else if contract.cache.lane_enables_actions_cache(lane, self, unit)
            && let Some(cache) = &unit.cache
        {
            render_retained_output_cache_note(output, self, unit, cache);
            let (paths, _) = rendered_cache_values(cache);
            let id_segment = unit.kind.id_prefix();
            let hash = cargo_source_cache_hash_expression(unit);
            let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(id_segment, &hash);
            let _ = writeln!(
                output,
                "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                yaml_scalar(&unit.label),
                self.pins.cache_restore,
            );
            true
        } else {
            false
        };
        render_ci_cache_prep_end_marker(output);
        let runtime_unit_id = input_unit.is_none();
        if !uses_lane_cargo_prep {
            let skip_when_offline_ready = lane == RunnerMode::Velnor
                && unit
                    .cache
                    .as_ref()
                    .is_some_and(cache_is_velnor_host_persistent);
            render_cargo_source_preparation(
                output,
                members,
                &unit.id,
                runtime_unit_id,
                skip_fetch_on_cache_hit,
                skip_when_offline_ready,
            );
            render_ci_cargo_fetch_end_marker(output);
        }
        let unit_id_value =
            input_unit.map_or_else(|| "${{ inputs.unit }}".to_owned(), ToOwned::to_owned);
        let checks_env = if runtime_unit_id {
            checks_env_for_members(unit, members)
        } else {
            checks_env(unit)
        };
        let token_env = if runtime_unit_id {
            docker_build_token_env_for_members(lane, members)
        } else {
            docker_build_token_env_for_members(lane, &[unit])
        };
        let offline_prelude = if runtime_unit_id {
            cargo_offline_run_prelude(members)
        } else {
            String::new()
        };
        let checks_started_marker = render_epoch_marker_commands("CHECKS_STARTED", "          ");
        let checks_ended_marker = render_epoch_marker_commands("CHECKS_ENDED", "          ");
        let _ = writeln!(
            output,
            "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: {unit_id_value}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ inputs.base_sha }}}}\n          HEAD_SHA: ${{{{ inputs.head_sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}{token_env}\n        run: |\n          set -o pipefail\n{offline_prelude}{checks_started_marker}\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n{checks_ended_marker}\n          exit $rc",
            yaml_scalar(&unit.label),
            checks_env,
        );
        if seed && lane == RunnerMode::Github {
            render_mutable_mount_seed_collection(output, self, unit, cache_save);
        } else if cache_save
            && lane == RunnerMode::Github
            && contract.cache.lane_enables_actions_cache(lane, self, unit)
            && let Some(cache) = unit.cache.as_ref()
        {
            let (paths, _) = rendered_cache_values(cache);
            let id_segment = unit.kind.id_prefix();
            let hash = cargo_source_cache_hash_expression(unit);
            let (cache_key, _) = format_cargo_bundle_cache_key(id_segment, &hash);
            let save_gate = dependency_bundle_cache_save_if(&self.default_branch);
            render_cache_save_start_marker(output, "BUNDLE", &save_gate);
            let _ = writeln!(
                output,
                "      - name: Save {} cache\n        id: unit_cache_save\n        if: {save_gate}\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}",
                yaml_scalar(&unit.label),
                self.pins.cache_save
            );
            render_cache_save_end_marker(output, "BUNDLE", &save_gate, "unit_cache_save");
        }
        render_phase_report_step(
            output,
            &self.ci_report_action_uses(),
            &yaml_scalar(&report_label),
            lane,
            &CacheReportFacts::for_unit(lane, unit, self),
        );
        output.push('\n');
    }

    fn render_job_env(&self, output: &mut String, lane: RunnerMode, unit: &Unit) {
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let mold = self.mise_present && unit.kind != UnitKind::Swift;
        let postgres = (unit.kind == UnitKind::Gradle)
            .then(|| {
                unit.services
                    .iter()
                    .find(|service| service.name == "postgres")
            })
            .flatten();
        let mut entries = Vec::<String>::new();
        let shadowed = |key: &str| unit.env.contains_key(key);
        if tools.contains(&ToolRequirement::Sccache) {
            if !shadowed("CARGO_INCREMENTAL") {
                entries.push("      CARGO_INCREMENTAL: \"0\"".to_owned());
            }
            if !shadowed("RUSTC_WRAPPER") {
                entries.push("      RUSTC_WRAPPER: sccache".to_owned());
            }
            if !shadowed("SCCACHE_GHA_ENABLED") {
                entries.push("      SCCACHE_GHA_ENABLED: \"true\"".to_owned());
            }
        }
        if mold && !shadowed("RUSTFLAGS") {
            entries.push("      RUSTFLAGS: \"-C link-arg=-fuse-ld=mold\"".to_owned());
        }
        if tools.contains(&ToolRequirement::OpenTofu) && !shadowed("TF_PLUGIN_CACHE_DIR") {
            entries.push("      TF_PLUGIN_CACHE_DIR: ~/.terraform.d/plugin-cache".to_owned());
        }
        let mut declared = unit.env.iter().collect::<Vec<_>>();
        declared.sort();
        for (name, value) in declared {
            entries.push(format!("      {name}: {}", yaml_scalar(value)));
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

    pub(crate) fn render_workflow_runtime_setup(&self, output: &mut String, lane: RunnerMode) {
        output.push_str(&workflow_runtime_setup(
            lane,
            &self.repository,
            &self.workflow_revision,
        ));
    }

    pub(crate) fn render_workflow_runtime_download(&self, output: &mut String, lane: RunnerMode) {
        output.push_str(&workflow_runtime_download(lane, &self.workflow_revision));
    }

    fn render_unit_runtime(&self, output: &mut String, lane: RunnerMode, unit: &Unit) {
        if lane != RunnerMode::Github {
            return;
        }
        // Velnor Planning does not publish a SOURCE_REV product. Manual GitHub
        // dispatch jobs bootstrap the pinned runtime themselves. Apple jobs
        // cannot consume a Linux-built plan artifact even when Planning is hosted.
        // Portable SwiftPM jobs can: they run on the default executor.
        if self.control_plane_lane() != RunnerMode::Github
            || self.runners == RunnerMode::Velnor
            || unit.platform.requires_apple()
        {
            self.render_workflow_runtime_setup(output, lane);
        } else {
            self.render_workflow_runtime_download(output, lane);
        }
    }

    fn control_plane_lane(&self) -> RunnerMode {
        match self.automatic {
            RunnerMode::Velnor => RunnerMode::Velnor,
            RunnerMode::Github | RunnerMode::Both => RunnerMode::Github,
        }
    }

    pub(crate) fn render_plan(&self, output: &mut String, _runners: RunnerMode, _trusted: bool) {
        // Planning follows `[workflow] automatic`. `automatic = velnor` keeps
        // the control plane off GitHub-hosted runners. `automatic = both`
        // plans on GitHub so this repository can compare lanes. GitHub
        // planning pins `uses:` to SOURCE_REV. `rev:` uses a context-gated
        // `${{ github.sha }}` with a static fallback when this repository owns
        // the setup action.
        let runners = self.control_plane_lane();
        let gate = if runners == RunnerMode::Velnor {
            format!(
                "    if: ${{{{ {} }}}}\n",
                self.velnor_control_plane_expression()
            )
        } else {
            String::new()
        };
        let runtime_setup = if runners == RunnerMode::Velnor {
            String::new()
        } else {
            crate::workflow_runtime_setup_with_install_rev(
                RunnerMode::Github,
                &self.repository,
                &self.workflow_revision,
                &crate::workflow_setup_install_rev(&self.repository, &self.workflow_revision),
            )
        };
        let base_sha = self.base_sha_expression();
        // Both-mode planning consumes the admitted lanes: a velnor-only
        // dispatch plans a velnor-only selection, so units the Velnor lane
        // cannot run stay unselected (and green under the required gate)
        // instead of failing a selection they can never satisfy.
        // Single-lane modes plan unfiltered, as before. The dispatch
        // `runner` input carries the manual selection; automatic events
        // fall back to the configured automatic lanes.
        let lanes_env = if self.runners == RunnerMode::Both {
            format!(
                "          VELNOR_LANES: ${{{{ github.event.inputs.runner || '{}' }}}}\n",
                self.automatic.as_str()
            )
        } else {
            String::new()
        };
        let mut outputs = vec![
            "      scope: ${{ steps.plan.outputs.scope }}".to_owned(),
            "      base_sha: ${{ steps.plan.outputs.base_sha }}".to_owned(),
            "      head_sha: ${{ steps.plan.outputs.head_sha }}".to_owned(),
            "      units: ${{ steps.plan.outputs.units }}".to_owned(),
            "      full_units: ${{ steps.plan.outputs.full_units }}".to_owned(),
        ];
        let mut matrices = BTreeSet::new();
        for unit in &self.units {
            matrices.insert(kind_matrix_output(unit.kind));
        }
        for name in matrices {
            outputs.push(format!(
                "      {name}: ${{{{ steps.plan.outputs.{name} }}}}"
            ));
        }
        let _ = writeln!(
            output,
            "  plan:\n    name: {}\n{gate}    runs-on: {}\n    outputs:\n{}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Select affected units\n        id: plan\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          CI_SCOPE_OVERRIDE: ${{{{ github.event.inputs.scope || '' }}}}\n          BASE_SHA: ${{{{ {base_sha} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n{lanes_env}        run: |\n          set -euo pipefail\n          if [[ -z \"${{CI_SCOPE_OVERRIDE:-}}\" ]]; then unset CI_SCOPE_OVERRIDE; fi\n          velnor-workflow plan --config .github/ci/project.toml\n",
            crate::control_job_name("Planning"),
            self.runner_for(runners),
            outputs.join("\n"),
            self.pins.checkout,
            base_sha = base_sha,
        );
        if runners != RunnerMode::Velnor {
            output.push_str(&workflow_runtime_artifact_upload(&self.workflow_revision));
        }
    }

    pub(crate) fn render_policy(&self, output: &mut String, runners: RunnerMode, trusted: bool) {
        let velnor = runners == RunnerMode::Velnor;
        let gate = velnor.then(|| self.trusted_runner_gate(runners, trusted));
        output.push_str(&crate::policy_job(&crate::PolicyJobSpec {
            name: "Policy",
            revision: &self.workflow_revision,
            runner: &self.runner_for(runners),
            repository: &self.repository,
            cache_backend: if velnor { "local" } else { "github" },
            trusted_gate: gate.as_deref(),
            default_branch: &self.default_branch,
            declared_ruleset_contexts: &self.declared_ruleset_contexts,
        }));
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
            "(github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule')) || ({})",
            self.default_branch,
            Self::velnor_dispatch_selection_expression()
        )
    }

    /// A `workflow_dispatch` that selects the Velnor lane, on any ref.
    ///
    /// Dispatch is not ref-gated: GitHub only accepts a dispatch from an
    /// actor with write access and only onto a ref of this repository, which
    /// is the same authorship a same-repository pull-request head carries,
    /// and the runner classifies both as `TrustClass::Trusted` without
    /// consulting `github.ref` (`velnor-runner` `trust_class.rs`,
    /// `TrustClass::derive`: every non-`pull_request*`, non-`workflow_run`
    /// event executes base-repository code; conformance test
    /// `trust_class_conformance_non_pr_events_are_trusted` lists
    /// `workflow_dispatch`). A default-branch gate here would only withhold
    /// the Velnor lane from a maintainer proving a branch on it.
    fn velnor_dispatch_selection_expression() -> &'static str {
        "github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')"
    }

    fn velnor_automatic_event_expression(&self) -> String {
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::Push,
            RunnerMode::Velnor,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::Schedule,
            RunnerMode::Velnor,
            self.pull_request_on_velnor,
        ));
        debug_assert!(!automatic_event_selects_lane(
            AutomaticEvent::PullRequestFork,
            RunnerMode::Velnor,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::MergeGroup,
            RunnerMode::Velnor,
            self.pull_request_on_velnor,
        ));
        format!(
            "{} || (github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule'))",
            "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository",
            self.default_branch
        )
    }

    fn automatic_event_expression(&self) -> String {
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::PullRequestSameRepository,
            RunnerMode::Github,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::PullRequestFork,
            RunnerMode::Github,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::Push,
            RunnerMode::Github,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::Schedule,
            RunnerMode::Github,
            self.pull_request_on_velnor,
        ));
        debug_assert!(automatic_event_selects_lane(
            AutomaticEvent::MergeGroup,
            RunnerMode::Github,
            self.pull_request_on_velnor,
        ));
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

    /// The one admission predicate of a lane class: the GitHub expression
    /// that is true exactly when the class's jobs run for the current event,
    /// ref, and dispatch input. Rendered verbatim into the callee job's
    /// `if:`, the aggregate caller's `if:`, and the required check's
    /// `LANE_ADMITTED_*` environment, so the three cannot disagree.
    ///
    /// A trust-gated class with no online trusted runner is `&& false`: the
    /// predicate itself says the lane is not admitted, and the required check
    /// therefore expects `skipped` for it like any other unadmitted lane.
    pub(crate) fn lane_admission_expression(&self, admission: LaneAdmission) -> String {
        let event = self.lane_event_expression(admission.lane());
        match admission {
            LaneAdmission::Github | LaneAdmission::Velnor => event,
            LaneAdmission::VelnorTrusted if self.velnor_trusted_runner_online => event,
            LaneAdmission::VelnorTrusted => format!("({event}) && false"),
        }
    }

    fn lane_event_expression(&self, lane: RunnerMode) -> String {
        let omitted_github = matches!(self.automatic, RunnerMode::Github | RunnerMode::Both);
        let omitted_velnor = matches!(self.automatic, RunnerMode::Velnor | RunnerMode::Both);
        let dispatch_github = dispatch_lane_expression(RunnerMode::Github, omitted_github);
        let dispatch_velnor = dispatch_lane_expression(RunnerMode::Velnor, omitted_velnor);
        match lane {
            RunnerMode::Github => {
                if matches!(self.automatic, RunnerMode::Github | RunnerMode::Both) {
                    format!(
                        "{} || ({dispatch_github})",
                        self.automatic_event_expression()
                    )
                } else {
                    dispatch_github
                }
            }
            // Dispatch admits the Velnor lane on any ref; see
            // `velnor_dispatch_selection_expression` for the trust argument.
            RunnerMode::Velnor => {
                if matches!(self.automatic, RunnerMode::Velnor | RunnerMode::Both) {
                    self.velnor_lane_event_expression(&dispatch_velnor)
                } else {
                    dispatch_velnor
                }
            }
            RunnerMode::Both => "github.event_name == 'workflow_dispatch'".to_owned(),
        }
    }

    fn velnor_control_plane_expression(&self) -> String {
        if self.pull_request_on_velnor == VelnorPullRequest::Automatic {
            format!(
                "{} || ({})",
                self.velnor_automatic_event_expression(),
                Self::velnor_dispatch_selection_expression()
            )
        } else {
            self.trusted_event_expression()
        }
    }

    fn velnor_lane_event_expression(&self, dispatch: &str) -> String {
        if self.pull_request_on_velnor == VelnorPullRequest::Automatic {
            format!(
                "{} || ({dispatch})",
                self.velnor_automatic_event_expression()
            )
        } else {
            format!(
                "(github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule')) || ({dispatch})",
                self.default_branch
            )
        }
    }

    pub(crate) fn render_verify_github(
        &self,
        output: &mut String,
        cache_save: bool,
        include_policy: bool,
    ) {
        self.render_verify_lane(output, RunnerMode::Github, cache_save, include_policy);
    }

    pub(crate) fn render_verify_velnor(
        &self,
        output: &mut String,
        cache_save: bool,
        include_policy: bool,
    ) {
        self.render_verify_lane(output, RunnerMode::Velnor, cache_save, include_policy);
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
            let group_name = crate::control_job_name(crate::lane_kind_label(kind));
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

    #[expect(
        clippy::too_many_lines,
        reason = "the lane renderer emits one complete, inspectable verification job"
    )]
    pub(crate) fn render_verify_lane(
        &self,
        output: &mut String,
        lane: RunnerMode,
        cache_save: bool,
        include_policy: bool,
    ) {
        for unit in self
            .units
            .iter()
            .filter(|unit| lane_supports_unit(lane, unit))
        {
            let id = unit_job_id(lane, &unit.id);
            let needs = unit_needs(
                lane,
                unit,
                include_policy,
                self.velnor_rust_needs,
                &self.units,
            );
            let runner = self.runner_for_unit(lane, unit);
            let job_name = yaml_scalar(&crate::comparison_job_name(lane, unit));
            let verify_name = yaml_scalar(&unit.label);
            let _ = writeln!(output, "  {id}:\n    name: {job_name}");
            // The in-workflow job is gated on the same admission the
            // required check expects it to satisfy.
            let _ = writeln!(
                output,
                "    if: ${{{{ ({}) }}}}",
                self.lane_admission_expression(LaneAdmission::for_unit(lane, unit))
            );
            let _ = writeln!(output, "    needs: [{}]", needs.join(", "));
            let _ = writeln!(
                output,
                "    runs-on: {runner}\n    timeout-minutes: 45\n    steps:",
            );
            render_ci_job_started_marker(output);
            let _ = writeln!(
                output,
                "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false",
                self.pins.checkout,
            );
            if lane == RunnerMode::Velnor {
                render_velnor_runner_identity_step(output);
            }
            self.render_unit_runtime(output, lane, unit);
            render_ci_runner_setup_end_marker(output);
            output.push_str(&workflow_selection_file_materialize(
                &SelectionFieldSources {
                    base_sha: "${{ needs.plan.outputs.base_sha }}",
                    head_sha: "${{ needs.plan.outputs.head_sha }}",
                    scope: "${{ needs.plan.outputs.scope }}",
                    units: "${{ needs.plan.outputs.units }}",
                    full_units: "${{ needs.plan.outputs.full_units }}",
                },
            ));
            render_ci_selection_end_marker(output);
            self.render_tool_provisioning(output, lane, unit, cache_save);
            render_ci_tool_bootstrap_end_marker(output);
            let cargo_cache_restored = if CacheBackend::Detected
                .lane_enables_actions_cache(lane, self, unit)
                && let Some(cache) = &unit.cache
            {
                render_retained_output_cache_note(output, self, unit, cache);
                let (paths, _) = rendered_cache_values(cache);
                let id_segment = if cache.purpose == CachePurpose::CargoSources {
                    unit.kind.id_prefix()
                } else {
                    unit.id.as_str()
                };
                let hash = cargo_source_cache_hash_expression(unit);
                let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(id_segment, &hash);
                let _ = writeln!(
                        output,
                        "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                        verify_name,
                        self.pins.cache_restore,
                    );
                true
            } else {
                false
            };
            render_ci_cache_prep_end_marker(output);
            let skip_when_offline_ready = lane == RunnerMode::Velnor
                && unit
                    .cache
                    .as_ref()
                    .is_some_and(cache_is_velnor_host_persistent);
            render_cargo_source_preparation(
                output,
                &[unit],
                &unit.id,
                false,
                cargo_cache_restored,
                skip_when_offline_ready,
            );
            render_ci_cargo_fetch_end_marker(output);
            let base_sha = self.base_sha_expression();
            let checks_started_marker =
                render_epoch_marker_commands("CHECKS_STARTED", "          ");
            let checks_ended_marker = render_epoch_marker_commands("CHECKS_ENDED", "          ");
            let token_env = docker_build_token_env_for_members(lane, &[unit]);
            let _ = writeln!(
                output,
                "      - name: Run {} checks\n        env:\n          CI_SCOPE: ${{{{ needs.plan.outputs.scope }}}}\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ {} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{}{token_env}\n        run: |\n          set -o pipefail\n{checks_started_marker}\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {} 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n{checks_ended_marker}\n          exit $rc",
                verify_name,
                yaml_scalar(&unit.id),
                base_sha,
                checks_env(unit),
                yaml_scalar(&unit.id),
            );
            if cache_save
                && lane == RunnerMode::Github
                && CacheBackend::Detected.lane_enables_actions_cache(lane, self, unit)
                && let Some(cache) = &unit.cache
            {
                let (paths, _) = rendered_cache_values(cache);
                let id_segment = if cache.purpose == CachePurpose::CargoSources {
                    unit.kind.id_prefix()
                } else {
                    unit.id.as_str()
                };
                let hash = cargo_source_cache_hash_expression(unit);
                let (cache_key, _) = format_cargo_bundle_cache_key(id_segment, &hash);
                let save_gate = dependency_bundle_cache_save_if(&self.default_branch);
                render_cache_save_start_marker(output, "BUNDLE", &save_gate);
                let _ = writeln!(
                        output,
                        "      - name: Save {} cache\n        id: unit_cache_save\n        if: {save_gate}\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}",
                        verify_name,
                        self.pins.cache_save,
                    );
                render_cache_save_end_marker(output, "BUNDLE", &save_gate, "unit_cache_save");
            }
            render_phase_report_step(
                output,
                &self.ci_report_action_uses(),
                &job_name,
                lane,
                &CacheReportFacts::for_unit(lane, unit, self),
            );
            output.push('\n');
        }
    }

    pub(crate) fn trust_gated_velnor_job_skipped(&self, lane: RunnerMode, unit: &Unit) -> bool {
        lane == RunnerMode::Velnor && unit.requires_trusted && !self.velnor_trusted_runner_online
    }

    /// Fail a release dispatch gate closed while the trusted runner is
    /// offline. CI lane jobs carry the same `&& false` inside
    /// [`Self::lane_admission_expression`] for the trusted class.
    pub(crate) fn append_trusted_runner_availability_gate(
        &self,
        lane: RunnerMode,
        unit: &Unit,
        gate: String,
    ) -> String {
        if self.trust_gated_velnor_job_skipped(lane, unit) {
            format!("({gate}) && false")
        } else {
            gate
        }
    }

    pub(crate) fn trusted_unit_display_name(
        &self,
        lane: RunnerMode,
        unit: &Unit,
        name: String,
    ) -> String {
        if !self.trust_gated_velnor_job_skipped(lane, unit) {
            return name;
        }
        let label = self
            .velnor_trusted_label
            .as_deref()
            .unwrap_or("trusted runner");
        format!("{name} · skipped (no online {label} runner)")
    }

    pub(crate) fn runner_for_unit(&self, lane: RunnerMode, unit: &Unit) -> String {
        if lane == RunnerMode::Github {
            return yaml_scalar(crate::platform::github_runner_for_unit(
                &self.github_runner,
                &self.macos_runner,
                unit,
            ));
        }
        if lane == RunnerMode::Velnor && unit.requires_trusted {
            // Generation validates the label is declared before rendering;
            // a hand-built IR without one falls back to the base labels.
            if let Some(label) = self.velnor_trusted_label.as_deref() {
                let mut labels = self.velnor_labels.clone();
                labels.push(label.to_owned());
                return velnor_runner(&labels, self.velnor_runner_group.as_deref());
            }
        }
        self.runner_for(lane)
    }

    pub(crate) fn uses_mr_boxington(&self, unit: &Unit) -> bool {
        self.mr_boxington && unit.kind == UnitKind::Rust && unit.uses_mbx()
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
                if mr_boxington && unit.uses_mbx() {
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
        // Declared tools and mise-run commands provision through mise whatever
        // the kind: the scan cannot see tools a test invokes at runtime, so
        // the repository declares them and the job installs them. This path
        // must not depend on the Rust-scoped `mise_present` flag — docs,
        // Homebrew, and other non-Rust units carry their own `mise_tools`.
        if !unit.mise_tools.is_empty() || commands_invoke_mise(unit) {
            tools.insert(ToolRequirement::Mise);
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
        let trusted_cache = trusted_cache_save_expression(&self.default_branch);
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

    /// The cargo-bin tools a hosted lane installs through the pinned
    /// install-action, in step order.
    fn cargo_bin_tools(&self, tools: &BTreeSet<ToolRequirement>) -> Vec<&'static str> {
        [
            (!self.mise_present && tools.contains(&ToolRequirement::Nextest))
                .then_some("cargo-nextest"),
            tools
                .contains(&ToolRequirement::CargoDeny)
                .then_some("cargo-deny"),
            tools
                .contains(&ToolRequirement::CargoAudit)
                .then_some("cargo-audit"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The single cargo-bin install step: restore the shared `~/.cargo/bin`
    /// toolchain cache, then install every listed tool through one pinned
    /// install-action invocation (the action accepts a comma-separated list).
    fn render_cargo_bin_tool_steps(&self, output: &mut String, tool_list: &str, cache_save: bool) {
        output.push_str(&hosted_cargo_bin_toolchain_restore());
        output.push_str(&hosted_cargo_bin_toolchain_verify(tool_list));
        let _ = writeln!(
            output,
            "      - name: Set up cargo bin tools\n        if: ${{{{ steps.cargo-bin-toolchain.outputs.cache-hit != 'true' || steps.cargo-bin-verify.outputs.missing == 'true' }}}}\n        uses: {}\n        with:\n          tool: {tool_list}\n          fallback: none",
            self.pins.rust_tool
        );
        if cache_save {
            output.push_str(&hosted_cargo_bin_toolchain_save(&self.default_branch));
        }
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
        if !github_lane && tools.contains(&ToolRequirement::Mise) {
            // Hosted mise-action is not admitted on Velnor. Auto-install is
            // off on the checks step, so declared lockfile tools must be
            // installed explicitly or shims fail closed. Install only what
            // this unit's commands need — never the whole root manifest.
            render_velnor_mise_install(output, unit, &self.mise_lock_keys);
        }
        if !velnor_skips_pinned_rust_toolchain(lane)
            && let Some(toolchain) = &unit.toolchain
        {
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
                let trusted = trusted_cache_save_expression(&self.default_branch);
                let _ = writeln!(
                    output,
                    "      - name: Set up Mise tools\n        uses: {}\n        with:\n          install_args: {}\n          cache: true\n          cache_save: ${{{{ {trusted} }}}}",
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
                self.render_mbx_github_step(output, &cache_key, &restore_keys);
            } else {
                self.render_mbx_local_step(output);
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
            let cache_dependency_path = unit.cache.as_ref().and_then(|cache| {
                cache
                    .key_files
                    .iter()
                    .find(|path| path.ends_with("package-lock.json"))
            });
            match cache_dependency_path {
                Some(path) => self.render_node_step(output, Some(&yaml_scalar(path))),
                None => self.render_node_step(output, None),
            }
        }
        let install_action_tools = self.cargo_bin_tools(&tools);
        if github_lane && !install_action_tools.is_empty() {
            self.render_cargo_bin_tool_steps(output, &install_action_tools.join(","), cache_save);
        }
        if !unit.prepared_tools.is_empty() {
            // Declared prepared tools restore after every lane-local
            // provisioning step: the consumer needs the runtime on `PATH`
            // (rendered before provisioning) and must not disturb the
            // toolchain state the steps above established.
            super::prepared_tools::render_consumer_steps(
                output,
                self.pins.cache_restore,
                &unit.prepared_tools,
            );
        }
        self.render_kind_level_tool_steps(output, lane, &tools, cache_save);
    }

    /// GitHub lane: the store budget export precedes the action so the
    /// hosted store is bounded for the import, every Cargo command, and the
    /// post-step export alike (see `MR_BOXINGTON_HOSTED_STORE_BUDGET`).
    fn render_mbx_github_step(&self, output: &mut String, cache_key: &str, restore_keys: &str) {
        render_mr_boxington_store_budget_step(output);
        let _ = writeln!(
            output,
            "      - name: Set up Mr. Boxington\n        id: mbx-cache\n        uses: {}\n        with:\n          backend: github\n          github-cache-mode: objects\n          version: {MR_BOXINGTON_VERSION}\n          cache-key: {cache_key}\n          restore-keys: |\n            {restore_keys}\n          save-on-workflow-dispatch: true",
            self.pins.mr_boxington
        );
    }

    /// Velnor lane: the job image pins Mr. Boxington and the runner mounts
    /// its host-persistent local store, so the local backend reuses it with
    /// no download and no cache transport. The version is deliberately
    /// omitted: pinning one would force a release download on every job
    /// instead of reusing PATH mbx.
    fn render_mbx_local_step(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "      - name: Set up Mr. Boxington\n        id: mbx-cache\n        uses: {}\n        with:\n          backend: local",
            self.pins.mr_boxington
        );
    }

    /// `cache_dependency_path` is the rendered YAML scalar (a literal or an
    /// `inputs.*` expression) of the lockfile the npm cache keys on.
    fn render_node_step(&self, output: &mut String, cache_dependency_path: Option<&str>) {
        let cache_options = cache_dependency_path.map_or_else(
            || "\n          package-manager-cache: false".to_owned(),
            |path| format!("\n          cache: npm\n          cache-dependency-path: {path}"),
        );
        let _ = writeln!(
            output,
            "      - name: Set up Node.js\n        uses: {}\n        with:\n          node-version: lts/*{cache_options}",
            self.pins.node
        );
    }

    /// The tool steps that depend only on the unit kind and repository
    /// facts, never on one unit's values: Gradle, sccache, mold, Docker
    /// Buildx, `OpenTofu`, and Homebrew.
    fn render_kind_level_tool_steps(
        &self,
        output: &mut String,
        lane: RunnerMode,
        tools: &BTreeSet<ToolRequirement>,
        cache_save: bool,
    ) {
        let github_lane = lane == RunnerMode::Github;
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
        cancel_in_progress: bool,
        include_policy: bool,
    ) {
        let display_name = yaml_scalar(REQUIRED_CHECK);
        let lanes = match runners {
            RunnerMode::Github => vec![RunnerMode::Github],
            RunnerMode::Velnor => vec![RunnerMode::Velnor],
            RunnerMode::Both => vec![RunnerMode::Github, RunnerMode::Velnor],
        };
        let mut needs = vec!["plan".to_owned()];
        if self.emits_velnor_lane_admission() {
            needs.push("velnor-lane-admission".to_owned());
        }
        if include_policy {
            needs.push("policy".to_owned());
        }
        let mut callers = Vec::new();
        for lane in lanes {
            for unit in self
                .units
                .iter()
                .filter(|unit| lane_supports_unit(lane, unit))
            {
                let id = unit_job_id(lane, &unit.id);
                needs.push(id.clone());
                callers.push(RequiredCaller {
                    job_id: id,
                    selected_by: vec![unit.id.clone()],
                    admission: LaneAdmission::for_unit(lane, unit),
                    prerequisite: false,
                });
            }
        }
        let gate = if self.control_plane_lane() == RunnerMode::Velnor {
            format!(
                "{} && ({})",
                aggregate_job_guard(cancel_in_progress),
                self.velnor_control_plane_expression()
            )
        } else {
            aggregate_job_guard(cancel_in_progress).to_owned()
        };
        let needs_json = github_expression("toJSON(needs)");
        let selected_units = github_expression("needs.plan.outputs.units");
        let _ = writeln!(
            output,
            "  ci-required:\n    name: {display_name}\n    if: ${{{{ {gate} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Validate generated unit results\n        env:\n          NEEDS_JSON: {needs_json}\n          SELECTED_UNITS: {selected_units}\n{}        shell: bash\n        run: |\n          set -euo pipefail\n          result_for_job() {{\n            jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"\n          }}",
            needs.join(", "),
            self.runner_for(self.control_plane_lane()),
            render_required_admission_env(self, &callers),
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
        if needs.iter().any(|job| job == "velnor-lane-admission") {
            output.push_str(
                "          result=\"$(result_for_job velnor-lane-admission)\"\n          case \"$result\" in\n            success|skipped) ;;\n            *) echo \"required CI prerequisite velnor-lane-admission did not pass: $result\" >&2; exit 1 ;;\n          esac\n",
            );
        }
        output.push_str("          selected=\",$SELECTED_UNITS,\"\n");
        render_required_caller_verdicts(output, &callers);
        let required_gate = if self.control_plane_lane() == RunnerMode::Velnor {
            format!(
                "{} && ({})",
                aggregate_job_guard(cancel_in_progress),
                self.velnor_control_plane_expression()
            )
        } else {
            aggregate_job_guard(cancel_in_progress).to_owned()
        };
        let _ = writeln!(
            output,
            "  required:\n    name: {}\n    if: ${{{{ {required_gate} }}}}\n    needs: [ci-required]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Mirror CI / Required\n        if: ${{{{ needs.ci-required.result != 'success' }}}}\n        run: exit 1",
            crate::control_job_name("Required"),
            self.runner_for(self.control_plane_lane())
        );
    }

    fn emits_velnor_lane_admission(&self) -> bool {
        self.runners == RunnerMode::Both
    }

    fn render_velnor_lane_admission(&self, output: &mut String) {
        if !self.emits_velnor_lane_admission() {
            return;
        }
        let if_expr = "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository";
        let _ = writeln!(
            output,
            "  velnor-lane-admission:\n    name: {}\n    if: ${{{{ {if_expr} }}}}\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Velnor lane omitted for fork\n        run: |\n          echo '::notice::Velnor lane omitted for fork pull request; validating GitHub lane only.'",
            crate::control_job_name("Velnor admission"),
            self.runner_for(self.control_plane_lane()),
        );
    }
}

/// The nested job's dependency record: the caller's unit, its dependency
/// closure, and its lane admission class, as the caller passed them through
/// `workflow_call` inputs. The step reads inputs only, so the callee stays
/// O(1) in the kind's units, and it carries no gate: the record is emitted
/// on every run of the job.
pub(crate) fn render_unit_dependency_info_step(output: &mut String) {
    output.push_str(
        "      - name: Record unit dependencies\n        env:\n          UNIT_ID: ${{ inputs.unit }}\n          UNIT_DEPENDENCIES: ${{ inputs.unit_dependencies }}\n          UNIT_ADMISSION: ${{ inputs.unit_admission }}\n          UNIT_LANE: ${{ inputs.lane }}\n        run: |\n          {\n            echo '## Unit dependencies'\n            echo\n            echo \"- Unit: $UNIT_ID\"\n            echo \"- Lane: $UNIT_LANE\"\n            echo \"- Admission: $UNIT_ADMISSION\"\n            if [[ -z \"$UNIT_DEPENDENCIES\" ]]; then\n              echo '- Dependencies: none'\n            else\n              echo \"- Dependencies: $UNIT_DEPENDENCIES\"\n            fi\n          } >> \"$GITHUB_STEP_SUMMARY\"\n",
    );
}

pub(crate) fn render_velnor_runner_identity_step(output: &mut String) {
    output.push_str(
        "      - name: Velnor runner identity\n        shell: bash\n        run: |\n          set -euo pipefail\n          {\n            echo '## Velnor runner identity'\n            echo\n            echo \"- Host: ${VELNOR_HOST:-unset}\"\n            echo \"- Instance: ${VELNOR_INSTANCE:-unset}\"\n            echo \"- Slot: ${VELNOR_SLOT:-unset}\"\n            echo \"- GitHub runner: ${RUNNER_NAME:-unset}\"\n            echo \"- OS/arch: ${RUNNER_OS:-unset}/${RUNNER_ARCH:-unset}\"\n            echo \"- Execution backend: ${VELNOR_EXECUTION_BACKEND:-unset}\"\n            echo \"- Velnor version: ${VELNOR_MANIFEST_VERSION:-${VELNOR_SOURCE_SHA:-unset}}\"\n          } | tee -a \"${GITHUB_STEP_SUMMARY:-/dev/null}\"\n",
    );
}

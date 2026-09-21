//! The resolved render environment for the declared CI surface.
//!
//! `WorkflowIr` is the provider and toolchain context every primitive renders
//! against: which providers exist, which selector each provider routes to, and
//! which tools a unit needs provisioned. It carries no repository knowledge of
//! its own; every value comes from the scanned shape and the repo-owned
//! generation config.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::snapshot::{
    freshness_expression, snapshot_class_prefix, snapshot_key, snapshot_restore_keys,
    CompatibilityFacts, KeySegments, SNAPSHOT_SCHEMA,
};
use super::{
    cache::{cache_is_local_host_persistent, local_skips_pinned_rust_toolchain},
    CacheBackend, GraphNode, Pins, ProviderJob, UnitContract, DEFAULT_UNIT_TIMEOUT_MINUTES,
    MUTABLE_MOUNT_HOST_DIR,
};
use crate::s2::provider::{ProviderId, ProviderSet, SelectorMap};
use crate::s2::reuse::REQUIRED_CHECK;
use crate::s2::{
    config_rust_toolchain, github_expression, hosted_cargo_bin_toolchain_restore,
    hosted_cargo_bin_toolchain_save, hosted_cargo_bin_toolchain_verify, hosted_mold_setup,
    kind_unit_workflow_file, nested_unit_workflow_file, prepare_cargo_caller_job_id,
    product_dependency_needs, provider_supports_unit, render_mr_boxington_store_budget_step,
    rendered_cache_values, rust_dependency_needs, stack_group_job_id, unit_group,
    unit_group_job_id, unit_job_display_name, unit_job_id, workflow_runtime_artifact_upload,
    workflow_runtime_download, workflow_runtime_setup, workflow_selection_file_materialize,
    yaml_scalar, CachePurpose, CacheSpec, GeneratorError, ProjectConfig, RustNeeds, RustToolchain,
    SelectionFieldSources, Unit, UnitKind, ValidationPhase, XcodeToolchain, GENERATED_HEADER,
    MR_BOXINGTON_VERSION, OPEN_TOFU_VERSION,
};

/// GitHub rejects reusable workflow files above this size.
pub(crate) const GITHUB_WORKFLOW_BYTE_LIMIT: usize = 500_000;

/// The plan job's expected-work artifact, by exact name. Same-run threading:
/// the plan job uploads it, `ci-required` downloads it by this exact name
/// from the same run, and the aggregate's SHA binding proves it is this
/// plan's file before scoring anything.
pub(crate) const EXPECTED_WORK_ARTIFACT: &str = "velnor-expected-work";

/// The workspace-relative expected-work directory: the plan step creates it
/// before invoking `plan`, so the runtime's write never needs a
/// pre-existing dir — including under the pinned older product.
pub(crate) const EXPECTED_WORK_DIR: &str = ".velnor-ci-expected-work";

/// The workspace-relative expected-work file: the plan step writes it here
/// (via [`EXPECTED_WORK_FILE_ENV`]) and the upload step publishes this path.
/// It always lives directly inside [`EXPECTED_WORK_DIR`].
pub(crate) const EXPECTED_WORK_FILE: &str = ".velnor-ci-expected-work/expected-work.json";

/// The env var binding `plan` to its expected-work file. Spelled identically
/// to the runtime's read; an older runtime ignores it and the upload step
/// then fails the plan job loudly via `if-no-files-found`.
pub(crate) const EXPECTED_WORK_FILE_ENV: &str = "VELNOR_EXPECTED_WORK_FILE";

/// The per-record result artifact prefix. One artifact per concluded
/// `(unit, provider)`: `{RESULT_ARTIFACT_PREFIX}{unit}-{provider}`.
pub(crate) const RESULT_ARTIFACT_PREFIX: &str = "velnor-result-";

/// The workspace-relative directory holding one record file per concluded
/// `(unit, provider)` — in unit jobs the single file this job wrote, in
/// `ci-required` the merged download of every record.
pub(crate) const RESULT_DIR: &str = ".velnor-ci-results";

/// The workspace-relative collected results file the aggregate scores: the
/// merge of every downloaded record, or the empty set when no unit ran.
pub(crate) const COLLECTED_RESULTS_FILE: &str = ".velnor-ci-results.json";

/// The plan job's expected-work upload: the aggregate's half of the
/// plan/aggregate cut. `if-no-files-found: error` keeps a plan that wrote no
/// file (an older runtime, a truncated step) red instead of silently
/// unbound.
fn render_expected_work_upload_step(upload_artifact_pin: &str) -> String {
    format!(
        "      - name: Publish expected work\n        uses: {upload_artifact_pin}\n        with:\n          name: {EXPECTED_WORK_ARTIFACT}\n          path: {EXPECTED_WORK_FILE}\n          if-no-files-found: error\n          retention-days: 7\n"
    )
}

/// The `ci-required` aggregate steps, rendered before the shell verdict step.
///
/// Conjunction, never replacement: the aggregate verdict AND the shell
/// verdict must both pass. The shell reasons over `needs.*.result` plus the
/// admission predicates and guards the controls the aggregate cannot see
/// (plan/policy/admission prerequisites, malformed plan outputs, the
/// expected-callers contract); the aggregate reasons over SHA-bound expected
/// work plus per-record evidence the shell cannot see (duplicates, matrix
/// completeness, prerequisite gating). Either side failing fails the check,
/// so no previously-failing case newly passes.
///
/// `runtime_steps` provisions the `velnor-workflow` binary via the verified
/// plan-artifact download. The results download tolerates zero artifacts — a
/// no-work plan runs no unit jobs — because the aggregate fails a real-work
/// plan with zero records anyway; every other step fails the check.
fn render_aggregate_score_steps(runtime_steps: &str, download_artifact_pin: &str) -> String {
    format!(
        "{runtime_steps}      - name: Download expected work\n        uses: {download_artifact_pin}\n        with:\n          name: {EXPECTED_WORK_ARTIFACT}\n          path: {EXPECTED_WORK_DIR}\n      - name: Download reported unit results\n        # A no-work plan runs no unit jobs, so zero result artifacts is the\n        # expected case there — and the aggregate fails a real-work plan with\n        # zero records anyway. Tolerate the empty download; never the verdict.\n        continue-on-error: true\n        uses: {download_artifact_pin}\n        with:\n          pattern: {RESULT_ARTIFACT_PREFIX}*\n          merge-multiple: true\n          path: {RESULT_DIR}\n      - name: Collect reported unit results\n        shell: bash\n        run: |\n          set -euo pipefail\n          shopt -s nullglob\n          mkdir -p {RESULT_DIR}\n          files=({RESULT_DIR}/result-*.json)\n          for file in \"${{files[@]}}\"; do\n            if jq -e 'any(.results[]?; has(\"reused_from\"))' \"$file\" >/dev/null; then\n              echo \"::error::$file carries reused_from without a validate_reuse decision; render emits no reused results\" >&2\n              exit 1\n            fi\n          done\n          if (( ${{#files[@]}} == 0 )); then\n            printf '{{\"results\":[]}}\\n' > {COLLECTED_RESULTS_FILE}\n          else\n            jq -s '{{results: ([.[].results // empty] | add // [])}}' \"${{files[@]}}\" > {COLLECTED_RESULTS_FILE}\n          fi\n          echo \"collected $(jq '.results | length' {COLLECTED_RESULTS_FILE}) reported result(s) from ${{#files[@]}} record file(s)\"\n      - name: Score expected work against reported results\n        env:\n          BASE_SHA: ${{{{ needs.plan.outputs.base_sha }}}}\n          HEAD_SHA: ${{{{ needs.plan.outputs.head_sha }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          velnor-workflow aggregate --expected {EXPECTED_WORK_FILE} --results {COLLECTED_RESULTS_FILE}\n"
    )
}

/// The record tail of one collapsed provider verify job: exactly one result
/// record per concluded `(unit, provider)`.
///
/// Collection discipline (exactly-once across retries, providers, and
/// splits): the record step runs `always()` on the job's own class gate only
/// (`record_gate`: the dispatched unit's trust partition when local
/// providers split one), so exactly one provider job of the kind records
/// each dispatch; the upload overwrites its exact-name artifact, so a
/// retried job replaces its one record instead of doubling it; and the
/// aggregate still rejects duplicate keys, so a second producer of the same
/// record fails closed instead of merging silently.
///
/// The outcome derives from `job.status`, which unit code cannot fake: the
/// checks steps `exit` nonzero on failure, every live checks path runs at
/// least one checks step for the dispatched unit, and anything but an
/// all-green job records `failure` (`cancelled` stays `cancelled`). A
/// skipped job records nothing, and the aggregate fails the missing record
/// unless the plan expected no work. Records carry no `matrix` (the planner
/// writes empty matrices: one unmatrixed verdict per provider) and no
/// `reused_from` (no live reuse path; collection rejects any).
fn render_unit_result_steps(
    upload_artifact_pin: &str,
    provider: &str,
    record_gate: &str,
) -> String {
    format!(
        "      - name: Record unit result\n        if: ${{{{ {record_gate} }}}}\n        env:\n          VELNOR_RESULT_UNIT: ${{{{ inputs.unit }}}}\n          VELNOR_RESULT_LANE: {provider}\n          VELNOR_RESULT_OUTCOME: ${{{{ job.status }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          case \"$VELNOR_RESULT_OUTCOME\" in\n            success) outcome=success ;;\n            cancelled) outcome=cancelled ;;\n            *) outcome=failure ;;\n          esac\n          mkdir -p {RESULT_DIR}\n          jq -n --arg unit \"$VELNOR_RESULT_UNIT\" --arg lane \"$VELNOR_RESULT_LANE\" --arg outcome \"$outcome\" '{{results: [{{unit: $unit, lane: $lane, outcome: $outcome}}]}}' > \"{RESULT_DIR}/result-$VELNOR_RESULT_UNIT-$VELNOR_RESULT_LANE.json\"\n      - name: Upload unit result\n        if: ${{{{ {record_gate} }}}}\n        uses: {upload_artifact_pin}\n        with:\n          name: {RESULT_ARTIFACT_PREFIX}${{{{ inputs.unit }}}}-{provider}\n          path: {RESULT_DIR}/result-${{{{ inputs.unit }}}}-{provider}.json\n          if-no-files-found: error\n          overwrite: true\n          retention-days: 7\n"
    )
}

/// The snapshot namespace the unit-provider compiler snapshots live in.
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
        explicit_toolchain_step_id, provider_input, unit_owns_workflow_crate, GraphNode, Pins,
        ProviderAdmission, ProviderId, ProviderSet, RequiredCaller, RustNeeds, RustToolchain, Unit,
        UnitKind, WorkflowIr, WorkflowKind, XcodeToolchain, REQUIRED_CHECK,
    };
    use crate::s2::platform::{NamedProduct, Prerequisite};
    use crate::s2::{
        nested_unit_workflow_file, sidebar_group_name, stack_group_job_id,
        workflow_setup_action_repository,
    };

    #[test]
    fn dispatch_admission_selects_the_static_universe_without_an_input() {
        let mut ir = owner_test_ir("example/owner", Vec::new());
        ir.automatic_providers = ProviderSet::from([ProviderId::GithubHosted]);
        // A manual provider admits dispatches only, with no provider input
        // to match: the comma boundary the old matcher needed is gone with
        // the input.
        for provider in [ProviderId::GithubSelfHosted, ProviderId::Velnor] {
            let expression =
                ir.provider_admission_expression(ProviderAdmission::Provider(provider));
            assert_eq!(
                expression, "github.event_name == 'workflow_dispatch'",
                "{provider:?} admits dispatches only: {expression}"
            );
            assert!(
                !expression.contains("inputs.providers"),
                "{provider:?} matches no input: {expression}"
            );
        }
        let hosted =
            ir.provider_admission_expression(ProviderAdmission::Provider(ProviderId::GithubHosted));
        assert_eq!(
            hosted, "true",
            "an automatic provider admits every event: {hosted}"
        );
    }

    #[test]
    fn caller_and_callee_selectors_share_one_plan_json_needle() {
        // The aggregate callers read the plan's `units` JSON through
        // `needs.plan.outputs.units`; the kind reusables read the same JSON
        // through `inputs.selected_units`. GitHub `contains()` is a literal
        // substring test, so both needles must appear verbatim in the plan
        // output: bare quotes, no backslashes. The caller once over-escaped
        // its needle (`'\"unit_id\":\"…\"'`), which never matches plan JSON
        // and skipped every unit job.
        let aggregate = super::aggregate_selected_unit_selector("docker");
        let reusable = super::reusable_selected_unit_selector("docker");
        assert_eq!(
            aggregate.replace("needs.plan.outputs.units", "UNITS"),
            reusable.replace("inputs.selected_units", "UNITS"),
            "caller and callee share one needle spelling: {aggregate} vs {reusable}"
        );
        assert!(
            !aggregate.contains('\\'),
            "the caller needle carries no escape characters: {aggregate}"
        );
        let needle = "\"unit_id\":\"docker\"";
        assert!(
            aggregate.contains(&format!("'{needle}'")),
            "the caller needle quotes the member exactly as plan JSON spells it: {aggregate}"
        );
        let plan_units = r#"[{"unit_id":"docker","providers":["github-hosted","velnor"]}]"#;
        assert!(
            plan_units.contains(needle),
            "the needle matches a plan `units` record under contains() semantics"
        );
    }

    #[test]
    fn automatic_admission_follows_the_automatic_set_and_trust_adds_the_event_gate() {
        let owner = "example/owner";
        let mut ir = owner_test_ir(owner, Vec::new());
        ir.automatic_providers = ProviderSet::from([ProviderId::GithubHosted]);
        let hosted =
            ir.provider_admission_expression(ProviderAdmission::Provider(ProviderId::GithubHosted));
        assert_eq!(
            hosted, "true",
            "an automatic provider runs every event: {hosted}"
        );
        let velnor =
            ir.provider_admission_expression(ProviderAdmission::Provider(ProviderId::Velnor));
        assert_eq!(
            velnor, "github.event_name == 'workflow_dispatch'",
            "a manual provider skips automatic events: {velnor}"
        );
        let trusted = ir
            .provider_admission_expression(ProviderAdmission::ProviderTrusted(ProviderId::Velnor));
        assert!(
            trusted.contains("github.event.pull_request.head.repo.fork"),
            "a trusted-only class gates on the event: {trusted}"
        );
        assert!(
            trusted.contains("github.event_name == 'workflow_dispatch'"),
            "a manual trusted-only class still admits dispatches only: {trusted}"
        );
    }

    /// One hand-built Rust unit: the id is fixture-local, the root decides
    /// ownership of the generator crate.
    fn rust_unit(id: &str, root: &str) -> Unit {
        Unit {
            xcode: None,
            id: id.to_owned(),
            label: format!("Rust crate ({id})"),
            kind: UnitKind::Rust,
            root: root.to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: vec!["cargo test --locked".to_owned()],
            full_commands: vec!["cargo test --locked".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            trust: crate::s2::provider::TrustReq::UntrustedOk,
            platform: crate::s2::provider::Platform::LinuxX64,
            capabilities: crate::s2::provider::Capabilities::default(),
            workspace_check: false,
            full_history: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
    }

    /// One hand-built Docker unit: its image build commands pass the
    /// `github_token` build secret, so its checks steps must export it.
    fn docker_unit(id: &str) -> Unit {
        let mut unit = rust_unit(id, ".");
        unit.label = format!("Docker ({id})");
        unit.kind = UnitKind::Docker;
        unit.pr_commands = vec![
            "docker buildx build --load --target ci --file 'Dockerfile' --tag local-ci:dockerfile '.' --secret id=github_token,env=GITHUB_TOKEN"
                .to_owned(),
        ];
        unit.full_commands = vec![
            "docker buildx build --load --file 'Dockerfile' --tag local-ci:dockerfile '.' --secret id=github_token,env=GITHUB_TOKEN"
                .to_owned(),
        ];
        unit
    }

    /// One hand-built Rust unit carrying the joined native pack, mirroring
    /// what the scan appends to a `BoltFFI` producer.
    fn boltffi_unit(id: &str) -> Unit {
        let mut unit = rust_unit(id, "libs/bridge-ffi");
        unit.pr_commands
            .push("cd -- 'libs/bridge-ffi' && boltffi -v pack apple".to_owned());
        unit.full_commands
            .push("cd -- 'libs/bridge-ffi' && boltffi -v pack apple".to_owned());
        unit
    }

    #[test]
    fn boltffi_need_installs_the_locked_tool_id() {
        let unit = boltffi_unit("rust-producer");
        assert!(super::needs_boltffi(&unit));
        assert!(!super::needs_boltffi(&rust_unit("rust-plain", ".")));
        let mut across_separator = rust_unit("rust-echo", ".");
        across_separator.pr_commands = vec!["echo boltffi && make pack".to_owned()];
        assert!(!super::needs_boltffi(&across_separator));
        let lock = BTreeSet::from([super::BOLTFFI_TOOL.to_owned()]);
        assert_eq!(
            super::mise_tool_ids(&unit, &lock),
            vec![super::BOLTFFI_TOOL.to_owned()]
        );
        assert!(super::mise_tool_ids(&rust_unit("rust-plain", "."), &lock).is_empty());
    }

    #[test]
    fn boltffi_validation_refuses_an_unpinned_pack() {
        let unit = boltffi_unit("rust-producer");
        let pinned = BTreeSet::from([super::BOLTFFI_TOOL.to_owned()]);
        assert!(
            super::validate_boltffi_tools_are_locked(std::slice::from_ref(&unit), &pinned).is_ok()
        );
        let plain = rust_unit("rust-plain", ".");
        assert!(super::validate_boltffi_tools_are_locked(
            std::slice::from_ref(&plain),
            &BTreeSet::new()
        )
        .is_ok());
        let missing = BTreeSet::from(["rust".to_owned()]);
        let error = must_err(
            super::validate_boltffi_tools_are_locked(std::slice::from_ref(&unit), &missing),
            "unpinned pack must fail generation",
        );
        let message = error.to_string();
        assert!(message.contains("rust-producer"), "{message}");
        assert!(message.contains(super::BOLTFFI_TOOL), "{message}");
        assert!(message.contains("rust"), "{message}");
    }

    fn owner_test_ir(repository: &str, units: Vec<Unit>) -> WorkflowIr {
        WorkflowIr {
            default_branch: "main".to_owned(),
            providers: ProviderId::ALL.into_iter().collect(),
            automatic_providers: ProviderId::ALL.into_iter().collect(),
            selectors: crate::s2::scan::default_selectors(),
            ci_required: true,
            repository: repository.to_owned(),
            workflow_revision: "0".repeat(40),
            rust_needs: RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            tools: BTreeSet::new(),
            mise_present: false,
            mr_boxington: false,
            units,
            pins: Pins::resolved(),
            mise_lock_keys: BTreeSet::new(),
            declared_ruleset_contexts: String::new(),
            rust_pin: None,
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
        ir.rust_needs = RustNeeds::DependencyClosure;
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
            pr.contains("expected CI job") && pr.contains("cancelled)") && pr.contains("skipped)"),
            "the required gate retains explicit failed, cancelled, and skipped verdicts"
        );
        assert!(
            pr.contains("cancel-in-progress: true")
                && pr.contains("ci-required:\n    name:")
                && pr.contains("if: ${{ !cancelled() }}"),
            "PR concurrency and required checks use the cancellation-aware aggregate guard"
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
    fn tool_provisioning_renders_declared_prepared_tools() {
        let mut unit = rust_unit("rust", ".");
        unit.prepared_tools = vec![crate::s2::primitives::prepared_tools::PreparedToolNeed {
            tool_id: "test-runner".to_owned(),
            authorized_producers: BTreeSet::from(["producer-job".to_owned()]),
            inputs_digest: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_owned(),
        }];
        let ir = owner_test_ir("example/provisioning", vec![unit.clone()]);
        let mut output = String::new();
        ir.render_tool_provisioning(&mut output, ProviderId::GithubHosted, &unit, true);
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
        // Undeclared units render no prepared-tool steps on any provider.
        for provider in [ProviderId::GithubHosted, ProviderId::Velnor] {
            let bare = rust_unit("rust", ".");
            let mut output = String::new();
            ir.render_tool_provisioning(&mut output, provider, &bare, true);
            assert!(
                !output.contains("prepared-tool"),
                "undeclared provisioning on {provider:?} mentions no prepared tool"
            );
        }
    }

    #[test]
    fn full_history_facts_travel_per_invocation_and_render_unquoted() {
        let mut deep = rust_unit("rust-deep", "crates/deep");
        deep.full_history = true;
        let shallow = rust_unit("rust-shallow", "crates/shallow");
        let ir = owner_test_ir("example/full-history", vec![deep.clone(), shallow.clone()]);
        for provider in [ProviderId::GithubHosted, ProviderId::Velnor] {
            let deep_facts =
                ir.unit_provider_facts(&deep, &ir.default_unit_contract(&deep, true), provider);
            let shallow_facts = ir.unit_provider_facts(
                &shallow,
                &ir.default_unit_contract(&shallow, true),
                provider,
            );
            assert!(
                deep_facts.full_history,
                "{provider:?} carries the deep unit's flag"
            );
            assert!(
                !shallow_facts.full_history,
                "{provider:?} leaves the shallow unit shallow"
            );
            let deep_values = deep_facts.input_values();
            assert!(
                deep_values.contains(&(provider_input::FULL_HISTORY, "true".to_owned())),
                "{provider:?} passes the flag for the deep unit: {deep_values:?}"
            );
            assert!(
                !shallow_facts
                    .input_values()
                    .iter()
                    .any(|(name, _)| *name == provider_input::FULL_HISTORY),
                "{provider:?} omits the flag for the shallow unit"
            );
            let rendered = super::render_caller_inputs(&deep_values);
            assert!(
                rendered
                    .lines()
                    .any(|line| line == "      full_history: true"),
                "{provider:?} renders the boolean unquoted: {rendered}"
            );
        }
        let bare = super::ProviderStepFacts::default();
        assert!(
            !bare
                .input_values()
                .iter()
                .any(|(name, _)| *name == provider_input::FULL_HISTORY),
            "default facts omit the flag"
        );
    }

    #[test]
    fn kind_header_declares_full_history_only_when_a_member_needs_it() {
        let shallow_ir = owner_test_ir(
            "example/full-history",
            vec![rust_unit("rust-a", "crates/a")],
        );
        let shallow = must_some(
            must_ok(
                shallow_ir.render_kind_unit_workflow(UnitKind::Rust, None),
                "shallow kind renders",
            ),
            "shallow kind has members",
        )
        .1;
        assert!(
            !shallow.contains("full_history"),
            "a kind with no deep member declares nothing: {shallow}"
        );
        let mut deep = rust_unit("rust-deep", "crates/deep");
        deep.full_history = true;
        let mixed_ir = owner_test_ir(
            "example/full-history",
            vec![deep, rust_unit("rust-shallow", "crates/shallow")],
        );
        let mixed = must_some(
            must_ok(
                mixed_ir.render_kind_unit_workflow(UnitKind::Rust, None),
                "mixed kind renders",
            ),
            "mixed kind has members",
        )
        .1;
        let declaration =
            "      full_history:\n        required: false\n        type: boolean\n        default: false";
        assert!(
            mixed.contains(declaration),
            "a kind with a deep member declares the boolean flag: {mixed}"
        );
        assert_eq!(
            mixed.matches("full_history:").count(),
            1,
            "the flag is declared exactly once: {mixed}"
        );
        assert_eq!(
            provider_input::declaration(provider_input::FULL_HISTORY),
            declaration,
            "the declaration helper pins the exact bytes"
        );
    }

    #[test]
    fn collapsed_checkout_stays_byte_identical_without_deep_members() {
        let unit = rust_unit("rust-a", "crates/a");
        let ir = owner_test_ir("example/full-history", vec![unit.clone()]);
        let mut output = String::new();
        must_ok(
            ir.render_collapsed_provider_verify_job(
                &mut output,
                &[&unit],
                None,
                ProviderId::GithubHosted,
                "verify",
                "Verify",
                "ubuntu-24.04",
                None,
            ),
            "shallow job renders",
        );
        assert!(
            !output.contains("fetch-depth"),
            "a shallow job carries no fetch-depth key: {output}"
        );
        let checkout = format!(
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}",
            ir.pins.checkout
        );
        assert!(
            output.contains(&checkout),
            "the shallow checkout keeps today's exact bytes: {output}"
        );
    }

    #[test]
    fn mixed_kind_renders_one_flipped_fetch_depth_expression_per_checkout() {
        let mut deep = rust_unit("rust-deep", "crates/deep");
        deep.full_history = true;
        let shallow = rust_unit("rust-shallow", "crates/shallow");
        let ir = owner_test_ir("example/full-history", vec![deep.clone(), shallow]);
        let mut output = String::new();
        must_ok(
            ir.render_collapsed_provider_verify_job(
                &mut output,
                &[&deep],
                None,
                ProviderId::GithubHosted,
                "verify",
                "Verify",
                "ubuntu-24.04",
                None,
            ),
            "deep job renders",
        );
        // The caller invokes once per (unit, provider), so one expression
        // serves mixed kinds; the operand order is load-bearing because 0 is
        // falsy in GitHub Actions expressions.
        let checkout = format!(
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}\n          fetch-depth: ${{{{ inputs.full_history == false && 1 || 0 }}}}",
            ir.pins.checkout
        );
        assert!(
            output.contains(&checkout),
            "the deep checkout pins the exact flipped expression: {output}"
        );
        assert!(
            !output.contains("inputs.full_history && 0"),
            "never the falsy-0 form, which yields 1 in both cases: {output}"
        );
        let rendered = must_some(
            must_ok(
                ir.render_kind_unit_workflow(UnitKind::Rust, None),
                "mixed kind renders",
            ),
            "mixed kind has members",
        )
        .1;
        assert!(
            rendered.contains("fetch-depth: ${{ inputs.full_history == false && 1 || 0 }}"),
            "the kind reusable carries the flipped expression: {rendered}"
        );
        assert!(
            !rendered.contains("inputs.full_history && 0"),
            "the kind reusable never carries the falsy-0 form: {rendered}"
        );
    }

    #[test]
    fn callers_pass_full_history_only_on_deep_unit_call_jobs() {
        let mut deep = rust_unit("rust-deep", "crates/deep");
        deep.full_history = true;
        let shallow = rust_unit("rust-shallow", "crates/shallow");
        let ir = owner_test_ir("example/full-history", vec![deep.clone(), shallow.clone()]);
        let file = nested_unit_workflow_file(&deep);
        for (unit, expect) in [(&deep, true), (&shallow, false)] {
            let callers = ir.unit_provider_callers(unit, &file, None);
            assert!(!callers.is_empty(), "unit {} renders call jobs", unit.id);
            for caller in &callers {
                let mut output = String::new();
                ir.render_unit_provider_caller(&mut output, unit, caller, false, &[], false);
                if expect {
                    assert!(
                        output
                            .lines()
                            .any(|line| line == "      full_history: true"),
                        "the deep unit's {} call job passes unquoted true: {output}",
                        caller.job_id
                    );
                } else {
                    assert!(
                        !output.contains("full_history"),
                        "the shallow unit's {} call job passes nothing: {output}",
                        caller.job_id
                    );
                }
            }
        }
    }

    fn prepared_need(
        tool: &str,
        digest: &str,
    ) -> crate::s2::primitives::prepared_tools::PreparedToolNeed {
        crate::s2::primitives::prepared_tools::PreparedToolNeed {
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
            6,
            "each distinct need renders one block per provider"
        );
        assert!(
            kind.contains("contains(format(',{0},', inputs.prepared_tools)"),
            "partial member records render behind membership gates"
        );
        // The callers pass their own records through the shared input.
        for (unit, digest) in [(&alpha, DIGEST_A), (&beta, DIGEST_B)] {
            let facts = ir.unit_provider_facts(
                unit,
                &ir.default_unit_contract(unit, true),
                ProviderId::GithubHosted,
            );
            let values = facts.input_values();
            let passed = values
                .iter()
                .find(|(name, _)| *name == provider_input::PREPARED_TOOLS);
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
            3,
            "identical needs share one block per provider"
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

    fn transport_product(name: &str, outputs: &[&str]) -> NamedProduct {
        NamedProduct {
            name: name.to_owned(),
            outputs: outputs.iter().map(ToString::to_string).collect(),
            ..Default::default()
        }
    }

    fn transport_edge(producer: &str, product: &str) -> Prerequisite {
        Prerequisite {
            producer: producer.to_owned(),
            product: product.to_owned(),
            ..Default::default()
        }
    }

    /// Producer with one transportable product and one output-less product,
    /// plus a consumer that needs both. The output-less edge always rebuilds
    /// locally; only the declared-outputs edge rides the artifact transport.
    fn transport_fixture() -> (Unit, Unit) {
        let mut producer = rust_unit("rust-ffi", "crates/ffi");
        producer.products = vec![
            transport_product("xcframework", &["native/out/lib.xcframework"]),
            transport_product("sourceless", &[]),
        ];
        let mut consumer = rust_unit("rust-app", "crates/app");
        consumer.prerequisites = vec![
            transport_edge("rust-ffi", "xcframework"),
            transport_edge("rust-ffi", "sourceless"),
        ];
        (producer, consumer)
    }

    #[test]
    fn kind_reusable_transports_eligible_products_between_members() {
        let (producer, consumer) = transport_fixture();
        let ir = owner_test_ir("example/transport", vec![producer, consumer]);
        let kind = ir.render_kind_units(UnitKind::Rust, None);
        assert!(
            kind.contains("product_provides:"),
            "the kind header declares the provides input"
        );
        assert!(
            kind.contains("product_transport_ready:"),
            "the kind header declares the readiness input"
        );
        assert!(
            kind.contains("Download product velnor-product-rust-ffi--xcframework"),
            "the consumer downloads the eligible product"
        );
        assert!(
            kind.contains("Verify product velnor-product-rust-ffi--xcframework"),
            "the consumer verifies the downloaded product"
        );
        assert!(
            kind.contains("Stage product velnor-product-rust-ffi--xcframework"),
            "the producer stages the eligible product"
        );
        assert!(
            kind.contains("Upload product velnor-product-rust-ffi--xcframework"),
            "the producer uploads the staged product"
        );
        assert!(
            kind.contains("rust-ffi/xcframework:true"),
            "the consumer block gates on the caller-evaluated verdict"
        );
        assert!(
            !kind.contains("velnor-product-rust-ffi--sourceless"),
            "an output-less product rides no artifact"
        );
    }

    #[test]
    fn transport_facts_pass_records_only_on_hosted() {
        let (producer, consumer) = transport_fixture();
        let ir = owner_test_ir("example/transport", vec![producer, consumer.clone()]);
        let hosted = ir.unit_provider_facts(
            &consumer,
            &ir.default_unit_contract(&consumer, true),
            ProviderId::GithubHosted,
        );
        let values = hosted.input_values();
        let (_, verdicts) = must_some(
            values
                .iter()
                .find(|(name, _)| *name == provider_input::PRODUCT_TRANSPORT_READY),
            &format!("the hosted caller passes readiness verdicts: {values:?}"),
        );
        assert!(
            verdicts.contains("rust-ffi/xcframework:${{ needs.")
                && verdicts.contains(".result == 'success' }}"),
            "the verdict names the edge and the producer job result: {verdicts}"
        );
        assert!(
            !verdicts.contains("sourceless"),
            "an output-less edge carries no verdict: {verdicts}"
        );
        let consumer_provides = values
            .iter()
            .find(|(name, _)| *name == provider_input::PRODUCT_PROVIDES);
        assert!(
            consumer_provides.is_none(),
            "a pure consumer provides no products: {values:?}"
        );
        let maker_facts = ir.unit_provider_facts(
            &ir.units[0],
            &ir.default_unit_contract(&ir.units[0], true),
            ProviderId::GithubHosted,
        );
        let maker_values = maker_facts.input_values();
        let record = maker_values
            .iter()
            .find(|(name, _)| *name == provider_input::PRODUCT_PROVIDES);
        assert_eq!(
            record.map(|(_, value)| value.as_str()),
            Some("rust-ffi/xcframework"),
            "the hosted caller passes the producer record: {maker_values:?}"
        );
        // Local providers share the workspace: no records, no readiness.
        for provider in [ProviderId::Velnor, ProviderId::GithubSelfHosted] {
            let local = ir.unit_provider_facts(
                &consumer,
                &ir.default_unit_contract(&consumer, true),
                provider,
            );
            let local_values = local.input_values();
            assert!(
                !local_values
                    .iter()
                    .any(|(name, _)| *name == provider_input::PRODUCT_PROVIDES
                        || *name == provider_input::PRODUCT_TRANSPORT_READY),
                "{provider:?} passes no transport records: {local_values:?}"
            );
        }
    }

    #[test]
    fn kind_reusable_without_products_declares_no_transport_inputs() {
        let ir = owner_test_ir("example/no-transport", vec![rust_unit("rust", ".")]);
        let kind = ir.render_kind_units(UnitKind::Rust, None);
        assert!(
            !kind.contains("product_provides") && !kind.contains("product_transport_ready"),
            "a product-less kind declares no transport inputs"
        );
        assert!(
            !kind.contains("velnor-product-"),
            "a product-less kind renders no transport steps"
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
            ProviderId::ALL.len(),
            "each provider keeps the MBX setup block"
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
            for provider in ProviderId::ALL {
                let facts =
                    ir.unit_provider_facts(unit, &ir.default_unit_contract(unit, true), provider);
                assert_eq!(facts.mbx_enabled, enabled, "{provider:?} / {}", unit.id);
                let values = facts.input_values();
                assert_eq!(
                    values.iter().any(|(name, value)| {
                        *name == provider_input::MBX_ENABLED && value == "true"
                    }),
                    enabled,
                    "typed MBX input for {provider:?} / {}",
                    unit.id
                );
                assert_eq!(
                    facts.mbx.is_some(),
                    provider == ProviderId::GithubHosted && enabled,
                    "hosted snapshot facts for {provider:?} / {}",
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
        assert_eq!(
            kind.matches("Set up Mr. Boxington").count(),
            ProviderId::ALL.len()
        );
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
            let facts = ir.unit_provider_facts(
                &ir.units[0],
                &ir.default_unit_contract(&ir.units[0], true),
                ProviderId::GithubHosted,
            );
            assert!(!facts.mbx_enabled);
            assert!(facts.mbx.is_none());
            assert!(
                facts
                    .input_values()
                    .iter()
                    .all(|(name, _)| *name != provider_input::MBX_ENABLED),
                "disabled members pass no MBX input"
            );
        }
    }

    fn candidate_flagged_callers(ir: &WorkflowIr) -> Vec<String> {
        let mut flagged = Vec::new();
        for unit in &ir.units {
            for caller in ir.unit_provider_callers(unit, "ci-unit-rust.yml", None) {
                if caller.inputs.iter().any(|(name, value)| {
                    *name == provider_input::CANDIDATE_PUBLISH && value == "true"
                }) {
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

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_err<T: std::fmt::Debug, E>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(value) => panic!("{context}: unexpectedly succeeded with {value:?}"),
            Err(error) => error,
        }
    }

    fn must_render_kind(ir: &WorkflowIr) -> String {
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "rust kind reusable renders",
        );
        must_some(rendered, "rust kind has members").1
    }

    fn required_gate_script(ir: &WorkflowIr, nodes: &[GraphNode], include_policy: bool) -> String {
        let mut rendered = String::new();
        ir.render_nodes_required(
            nodes,
            None,
            &mut rendered,
            include_policy,
            REQUIRED_CHECK,
            false,
            false,
        );
        // The aggregate steps precede the verdict step; anchor on the
        // verdict step's name so the fixture executes the shell verdict, not
        // the collection script.
        let script = must_some(
            rendered
                .split_once("- name: Validate generated stack results")
                .and_then(|(_, step)| step.split_once("        run: |\n"))
                .map(|(_, body)| body.split("\n  required:\n").next().unwrap_or(body)),
            "required gate script",
        );
        script
            .lines()
            .map(|line| line.strip_prefix("          ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run_required_gate(
        script: &str,
        policy_result: Option<&str>,
        selected_units: &str,
        callers: &[RequiredCaller],
        nonempty_selection: bool,
    ) -> bool {
        let mut needs = serde_json::Map::new();
        needs.insert("plan".to_owned(), serde_json::json!({"result": "success"}));
        if let Some(result) = policy_result {
            needs.insert("policy".to_owned(), serde_json::json!({"result": result}));
        }
        for caller in callers {
            let result = if nonempty_selection
                && !caller.prerequisite
                && caller.provider == ProviderId::GithubHosted
            {
                "success"
            } else {
                "skipped"
            };
            needs.insert(caller.job_id.clone(), serde_json::json!({"result": result}));
        }
        let output = must_ok(
            Command::new("bash")
                .args(["-euo", "pipefail", "-c", script])
                .env("NEEDS_JSON", serde_json::Value::Object(needs).to_string())
                .env("SELECTED_UNITS", selected_units)
                .env("PLAN_DIGEST", "synthetic-plan-digest")
                .env("EXCLUDED", "[]")
                .env(
                    "EXPECTED_CALLERS",
                    super::required_execution_contract(callers),
                )
                .env("PROVIDER_ADMITTED_GITHUB_HOSTED", "true")
                .env("PROVIDER_ADMITTED_GITHUB_SELF_HOSTED", "true")
                .env("PROVIDER_ADMITTED_VELNOR", "true")
                .env("PROVIDER_ADMITTED_GITHUB_HOSTED_TRUSTED", "true")
                .env("PROVIDER_ADMITTED_GITHUB_SELF_HOSTED_TRUSTED", "true")
                .env("PROVIDER_ADMITTED_VELNOR_TRUSTED", "true")
                .env("PROVIDER_ADMITTED_ANY_LOCAL_TRUSTED", "true")
                .output(),
            "bash and jq execute the rendered gate",
        );
        output.status.success()
    }

    #[test]
    fn required_controls_drive_needs_and_strict_policy_verdicts() {
        let unit = rust_unit("rust", "crates/rust");
        let ir = owner_test_ir("example/required-controls", vec![unit.clone()]);
        let nodes = vec![GraphNode::Unit {
            unit_id: unit.id.clone(),
            job_id: stack_group_job_id(unit.kind),
            name: sidebar_group_name(&unit),
            file: nested_unit_workflow_file(&unit),
        }];
        let callers = ir.required_callers(&nodes, None);
        let script = required_gate_script(&ir, &nodes, true);
        assert!(script.contains("result=\"$(result_for_job plan)\""));
        assert!(script.contains("result=\"$(result_for_job policy)\""));

        for (nonempty_selection, selected_units) in [
            (false, "[]"),
            (
                true,
                r#"[{"unit_id":"rust","providers":["github-hosted"]}]"#,
            ),
        ] {
            assert!(
                run_required_gate(
                    &script,
                    Some("success"),
                    selected_units,
                    &callers,
                    nonempty_selection,
                ),
                "successful policy must pass for {selected_units}"
            );
            for result in ["failure", "cancelled", "skipped"] {
                assert!(
                    !run_required_gate(
                        &script,
                        Some(result),
                        selected_units,
                        &callers,
                        nonempty_selection,
                    ),
                    "policy {result} must fail for {selected_units}"
                );
            }
            assert!(
                !run_required_gate(&script, None, selected_units, &callers, nonempty_selection,),
                "missing policy must fail for {selected_units}"
            );
        }
        assert!(
            !run_required_gate(&script, Some("success"), "{malformed-plan", &callers, false,),
            "malformed selected-unit plan must fail closed"
        );

        for selected_units in [
            r#"[{"unit_id":"unknown","providers":["github-hosted"]}]"#,
            r#"[{"unit_id":"rust","providers":["unknown"]}]"#,
            r#"[{"unit_id":"rust","providers":[]}]"#,
            r#"[{"unit_id":"rust","providers":["github-hosted","github-hosted"]}]"#,
            r#"[{"unit_id":"rust","providers":["github-hosted"]},{"unit_id":"rust","providers":["github-hosted"]}]"#,
            r#"[{"unit_id":"rust","providers":[1]}]"#,
        ] {
            assert!(
                !run_required_gate(&script, Some("success"), selected_units, &callers, true,),
                "unmapped or duplicate selected obligation must fail closed: {selected_units}"
            );
        }

        let pull_request_script = required_gate_script(&ir, &nodes, false);
        assert!(!pull_request_script.contains("result_for_job policy"));
    }

    /// One job's body from a rendered workflow. Job bodies indent past two
    /// spaces, so the next `\n  ` past a job header is the next job whatever
    /// the render order.
    fn job_block<'a>(content: &'a str, job: &str) -> &'a str {
        let header = format!("\n  {job}:\n");
        let start = must_some(content.find(header.as_str()), job) + header.len();
        let tail = &content[start..];
        let mut end = tail.len();
        let mut search = 0;
        while let Some(found) = tail[search..].find("\n  ") {
            let candidate = search + found + 3;
            if tail[candidate..]
                .chars()
                .next()
                .is_some_and(|next| next != ' ')
            {
                end = search + found;
                break;
            }
            search = candidate;
        }
        &tail[..end]
    }

    #[test]
    fn collapsed_swift_kind_lands_on_macos_while_rust_stays_on_linux() {
        let mut swift = rust_unit("swift-package-native", "native");
        swift.kind = UnitKind::Swift;
        swift.label = "Swift package (native)".to_owned();
        // Apple-bound: Xcode SDK need, so the collapsed job must land on macOS.
        swift.platform = crate::s2::provider::Platform::MacosArm64;
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
            swift_workflow.contains("runs-on: macos-26"),
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
            !rust_workflow.contains("runs-on: macos-15")
                && !rust_workflow.contains("runs-on: macos-26"),
            "{rust_workflow}"
        );
    }

    /// One macOS Rust member (a `BoltFFI` producer) beside Linux Rust
    /// members: the collapsed hosted jobs split by executor, so the Linux
    /// units stay on the hosted selector instead of following the one
    /// macOS member onto the Apple image.
    fn mixed_platform_rust_units() -> Vec<Unit> {
        let mut producer = boltffi_unit("rust-bridge-ffi");
        producer.platform = crate::s2::provider::Platform::MacosArm64;
        vec![
            rust_unit("rust-widget", "crates/widget"),
            producer,
            rust_unit("rust-gadget", "crates/gadget"),
        ]
    }

    #[test]
    fn mixed_platform_rust_kind_splits_hosted_jobs_by_executor() {
        let ir = owner_test_ir("example/fixture", mixed_platform_rust_units());
        // Facts level: only the (hosted, macOS) pair sets the flag.
        for unit in &ir.units {
            let contract = ir.contract_for(unit, None);
            let hosted = ir.unit_provider_facts(unit, &contract, ProviderId::GithubHosted);
            assert_eq!(
                hosted.apple_executor,
                unit.id == "rust-bridge-ffi",
                "{} sets the executor flag only on macOS",
                unit.id
            );
            for provider in [ProviderId::GithubSelfHosted, ProviderId::Velnor] {
                assert!(
                    !ir.unit_provider_facts(unit, &contract, provider)
                        .apple_executor,
                    "local providers never set the executor flag: {} on {provider:?}",
                    unit.id
                );
            }
        }
        // Render level: one job per executor, each admitting only its own
        // callers at the job gate and the result record alike.
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "rust kind reusable renders",
        );
        let workflow = must_some(rendered, "rust kind has members").1;
        assert!(
            workflow.contains(
                "      apple_executor:\n        required: false\n        type: boolean\n        default: false"
            ),
            "the header declares the executor flag: {workflow}"
        );
        let default = job_block(&workflow, "verify-github-hosted");
        assert!(
            default.contains("runs-on: ubuntu-24.04"),
            "Linux units stay on the hosted selector: {default}"
        );
        assert!(
            default.contains("inputs.apple_executor != true"),
            "the default job admits only default callers: {default}"
        );
        assert!(
            default.contains("if: ${{ always() && inputs.apple_executor != true }}"),
            "the default job records only default dispatches: {default}"
        );
        let apple = job_block(&workflow, "verify-github-hosted-apple");
        assert!(
            apple.contains("runs-on: macos-26"),
            "the macOS unit takes the Apple image: {apple}"
        );
        assert!(
            apple.contains("inputs.apple_executor }}"),
            "the Apple job admits only Apple callers: {apple}"
        );
        assert!(
            apple.contains("if: ${{ always() && inputs.apple_executor }}"),
            "the Apple job records only Apple dispatches: {apple}"
        );
        // Caller level: exactly the macOS hosted caller passes the flag.
        let nodes = aggregate_fixture_nodes(&ir);
        for kind in [WorkflowKind::PullRequest, WorkflowKind::Main] {
            let rendered = ir.render_nested(kind, &nodes, None);
            assert_eq!(
                rendered.matches("apple_executor: true").count(),
                1,
                "exactly the macOS hosted caller passes the flag on {kind:?}: {rendered}"
            );
        }
    }

    #[test]
    fn executor_split_keeps_unsplit_kinds_on_one_job() {
        // All-Linux: the one hosted job on the selector, no executor clause.
        let linux = owner_test_ir(
            "example/fixture",
            vec![
                rust_unit("rust-a", "crates/a"),
                rust_unit("rust-b", "crates/b"),
            ],
        );
        let rendered = must_some(
            must_ok(
                linux.render_kind_unit_workflow(UnitKind::Rust, None),
                "rust kind reusable renders",
            ),
            "rust kind has members",
        )
        .1;
        assert!(rendered.contains("  verify-github-hosted:\n"), "{rendered}");
        assert!(
            !rendered.contains("verify-github-hosted-apple"),
            "an unsplit kind renders no Apple job: {rendered}"
        );
        let hosted = job_block(&rendered, "verify-github-hosted");
        assert!(hosted.contains("runs-on: ubuntu-24.04"), "{hosted}");
        assert!(
            !hosted.contains("apple_executor"),
            "an unsplit job carries no executor clause: {hosted}"
        );
        // All-macOS: the same one job, on the Apple image, no clause.
        let mut apple_a = rust_unit("swift-a", "a");
        apple_a.kind = UnitKind::Swift;
        apple_a.platform = crate::s2::provider::Platform::MacosArm64;
        let mut apple_b = rust_unit("swift-b", "b");
        apple_b.kind = UnitKind::Swift;
        apple_b.platform = crate::s2::provider::Platform::MacosArm64;
        let macos = owner_test_ir("example/fixture", vec![apple_a, apple_b]);
        let rendered = must_some(
            must_ok(
                macos.render_kind_unit_workflow(UnitKind::Swift, None),
                "swift kind reusable renders",
            ),
            "swift kind has members",
        )
        .1;
        assert!(
            !rendered.contains("verify-github-hosted-apple"),
            "a uniform Apple kind renders no second job: {rendered}"
        );
        let hosted = job_block(&rendered, "verify-github-hosted");
        assert!(hosted.contains("runs-on: macos-26"), "{hosted}");
        assert!(
            !hosted.contains("apple_executor"),
            "a uniform Apple job carries no executor clause: {hosted}"
        );
    }

    #[test]
    fn executor_split_is_deterministic() {
        let render = |units| {
            let ir = owner_test_ir("example/fixture", units);
            must_some(
                must_ok(
                    ir.render_kind_unit_workflow(UnitKind::Rust, None),
                    "rust kind reusable renders",
                ),
                "rust kind has members",
            )
            .1
        };
        let first = render(mixed_platform_rust_units());
        let second = render(mixed_platform_rust_units());
        assert_eq!(first, second, "identical members render identical bytes");
        let mut reordered = mixed_platform_rust_units();
        reordered.rotate_right(1);
        assert_eq!(
            first,
            render(reordered),
            "member order never leaks into the split render"
        );
    }

    fn xcode_swift_unit(id: &str, root: &str, version: &str) -> Unit {
        let mut unit = rust_unit(id, root);
        unit.kind = UnitKind::Swift;
        unit.label = format!("Swift package ({root})");
        unit.platform = crate::s2::provider::Platform::MacosArm64;
        unit.xcode = Some(XcodeToolchain {
            version: version.to_owned(),
        });
        unit
    }

    #[test]
    fn xcode_pin_renders_probe_step_on_hosted_swift() {
        let ir = owner_test_ir(
            "example/fixture",
            vec![xcode_swift_unit("swift-package-native", "native", "26.6")],
        );
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Swift, None),
            "swift kind reusable renders",
        );
        let workflow = must_some(rendered, "swift kind has members").1;
        assert!(workflow.contains("- name: Select Xcode 26.6"), "{workflow}");
        assert!(workflow.contains("DEVELOPER_DIR="), "{workflow}");
        assert!(workflow.contains("xcodebuild -version"), "{workflow}");
    }

    /// A fake `/Applications` tree plus `xcodebuild`/`swift` shims that
    /// executes the exact rendered probe script.
    struct ProbeStage {
        root: std::path::PathBuf,
    }

    impl ProbeStage {
        fn create(name: &str) -> Self {
            static NEXT_STAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "velnor-xcode-probe-{name}-{}-{}",
                std::process::id(),
                NEXT_STAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            must_ok(
                std::fs::create_dir_all(root.join("Applications")),
                "stage Applications",
            );
            must_ok(std::fs::create_dir_all(root.join("bin")), "stage bin");
            for tool in ["xcodebuild", "swift"] {
                let shim = root.join("bin").join(tool);
                must_ok(
                    std::fs::write(&shim, format!("#!/bin/sh\necho fake-{tool}\n")),
                    "stage tool shim",
                );
                Self::make_executable(&shim);
            }
            Self { root }
        }

        fn make_executable(path: &std::path::Path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                must_ok(
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)),
                    "stage shim is executable",
                );
            }
            #[cfg(not(unix))]
            {
                let _ = path;
            }
        }

        fn install_versioned(&self, name: &str) {
            must_ok(
                std::fs::create_dir_all(
                    self.root
                        .join("Applications")
                        .join(format!("{name}.app/Contents/Developer")),
                ),
                "stage versioned Xcode",
            );
        }

        fn install_unversioned(&self, version: &str) {
            let dir = self
                .root
                .join("Applications")
                .join("Xcode.app/Contents/Developer/usr/bin");
            must_ok(std::fs::create_dir_all(&dir), "stage Xcode.app");
            let stub = dir.join("xcodebuild");
            must_ok(
                std::fs::write(
                    &stub,
                    format!("#!/bin/sh\necho 'Xcode {version}'\necho 'Build version TEST'\n"),
                ),
                "stage Xcode.app xcodebuild",
            );
            Self::make_executable(&stub);
        }

        fn run(&self, pin: &str) -> (bool, String, String, String) {
            let script = WorkflowIr::xcode_probe_script(pin).replace(
                "/Applications",
                &self.root.join("Applications").to_string_lossy(),
            );
            let path = format!(
                "{}:{}",
                self.root.join("bin").to_string_lossy(),
                std::env::var("PATH").unwrap_or_default()
            );
            let env_file = self.root.join("github-env.txt");
            let output = must_ok(
                Command::new("bash")
                    .args(["-c", &script])
                    .env("PATH", path)
                    .env("GITHUB_ENV", &env_file)
                    .output(),
                "bash executes the probe",
            );
            let env = std::fs::read_to_string(&env_file).unwrap_or_default();
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
                env,
            )
        }
    }

    impl Drop for ProbeStage {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn xcode_probe_selects_exact_prefix_and_unversioned() {
        let stage = ProbeStage::create("exact");
        stage.install_versioned("Xcode_26.6");
        stage.install_versioned("Xcode_26.6.1");
        stage.install_unversioned("27.0");
        let (success, _, stderr, env) = stage.run("26.6");
        assert!(success, "exact match wins: {stderr}");
        assert!(
            env.contains("DEVELOPER_DIR=") && env.contains("Xcode_26.6.app/Contents/Developer"),
            "exact Xcode_26.6.app is exported, not the newer prefix or Xcode.app: {env}"
        );

        let stage = ProbeStage::create("prefix");
        stage.install_versioned("Xcode_26.6.1");
        stage.install_versioned("Xcode_26.6.2");
        stage.install_unversioned("27.0");
        let (success, _, stderr, env) = stage.run("26.6");
        assert!(success, "newest prefix match wins: {stderr}");
        assert!(
            env.contains("Xcode_26.6.2.app/Contents/Developer"),
            "newest prefix Xcode_26.6.2.app is exported: {env}"
        );

        let stage = ProbeStage::create("unversioned");
        stage.install_unversioned("26.6.1");
        let (success, _, stderr, env) = stage.run("26.6");
        assert!(success, "version-matching Xcode.app is accepted: {stderr}");
        assert!(
            env.contains("Xcode.app/Contents/Developer"),
            "Xcode.app is exported: {env}"
        );
    }

    #[test]
    fn xcode_probe_fails_closed_with_diagnostic() {
        let stage = ProbeStage::create("mismatch");
        stage.install_versioned("Xcode_27.0");
        stage.install_unversioned("27.0");
        let (success, _, stderr, _) = stage.run("26.6");
        assert!(!success, "a wrong-version Xcode.app is refused");
        assert!(
            stderr.contains("::error::no installed Xcode matches pin 26.6"),
            "refusal names the pin: {stderr}"
        );

        let stage = ProbeStage::create("empty");
        let (success, _, stderr, _) = stage.run("26.6");
        assert!(!success, "no installed Xcode fails");
        assert!(
            stderr.contains("::error::no installed Xcode matches pin 26.6"),
            "an empty tree reports the diagnostic instead of dying silently: {stderr}"
        );
    }

    #[test]
    fn swift_without_xcode_pin_renders_no_probe_step() {
        let mut swift = rust_unit("swift-package-native", "native");
        swift.kind = UnitKind::Swift;
        swift.platform = crate::s2::provider::Platform::MacosArm64;
        let ir = owner_test_ir("example/fixture", vec![swift]);
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Swift, None),
            "swift kind reusable renders",
        );
        let workflow = must_some(rendered, "swift kind has members").1;
        assert!(!workflow.contains("Select Xcode"), "{workflow}");
    }

    #[test]
    fn disagreeing_xcode_pins_fail_swift_render() {
        let ir = owner_test_ir(
            "example/fixture",
            vec![
                xcode_swift_unit("swift-package-a", "a", "26.6"),
                xcode_swift_unit("swift-package-b", "b", "26.7"),
            ],
        );
        let error = must_err(
            ir.render_kind_unit_workflow(UnitKind::Swift, None),
            "members disagree on the Xcode pin",
        );
        assert!(error.to_string().contains("Xcode toolchain pin"), "{error}");
    }

    #[test]
    fn apple_cache_purposes_enable_actions_cache_and_render() {
        let cache = |purpose| {
            Some(crate::s2::CacheSpec {
                key_files: vec!["Package.swift".to_owned(), "mise.lock".to_owned()],
                paths: vec!["~/.swiftpm".to_owned()],
                purpose,
                mbx_output_cache_justification: None,
                mutable_mount_seed: false,
            })
        };
        let mut package = rust_unit("swift-package-native", "native");
        package.kind = UnitKind::Swift;
        package.platform = crate::s2::provider::Platform::MacosArm64;
        package.cache = cache(crate::s2::CachePurpose::SwiftPmSources);
        let mut scheme = rust_unit("swift-xcodeproj-app", "native");
        scheme.kind = UnitKind::Swift;
        scheme.platform = crate::s2::provider::Platform::MacosArm64;
        scheme.cache = cache(crate::s2::CachePurpose::XcodeIntermediates);
        let ir = owner_test_ir("example/fixture", vec![package, scheme]);
        for unit in &ir.units {
            assert!(
                crate::s2::primitives::CacheBackend::Detected.provider_enables_actions_cache(
                    crate::s2::provider::ProviderId::GithubHosted,
                    &ir,
                    unit,
                ),
                "apple cache enables restore/save steps: {}",
                unit.id
            );
        }
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Swift, None),
            "swift kind reusable renders",
        );
        let workflow = must_some(rendered, "swift kind has members").1;
        assert!(
            workflow.contains("Restore unit cache"),
            "swift cache renders a restore step: {workflow}"
        );
        assert!(
            workflow.contains("-swift-${{ hashFiles(inputs.cache_key_files) }}"),
            "swift cache keeps the kind-level key segment: {workflow}"
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
            for provider in [ProviderId::GithubHosted, ProviderId::Velnor] {
                let contract = ir.default_unit_contract(unit, false);
                let facts = ir.unit_provider_facts(unit, &contract, provider);
                assert_eq!(
                    facts.candidate_publish,
                    provider == ProviderId::GithubHosted && unit_owns_workflow_crate(unit),
                    "candidate_publish for {} on {provider:?}",
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
            for provider in [ProviderId::GithubHosted, ProviderId::Velnor] {
                let contract = foreign.default_unit_contract(unit, false);
                assert!(
                    !foreign
                        .unit_provider_facts(unit, &contract, provider)
                        .candidate_publish,
                    "foreign trees never publish on {provider:?}"
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
                .unit_provider_facts(&anonymous.units[0], &contract, ProviderId::GithubHosted)
                .candidate_publish,
            "an empty repository is not the owner"
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one implication pinned at facts, render, and caller level"
    )]
    #[test]
    fn local_provider_check_implies_policy_runtime() {
        const CHECK: &str = "cd -- 'crates/velnor-workflow' && mbx run -- --plain --check ../..";
        const OTHER: &str = "cargo test --locked";
        const PROVISION: &str = "      - name: Provision pinned Velnor workflow policy runtime\n        if: ${{ inputs.policy_runtime }}\n";
        let owner = workflow_setup_action_repository().to_owned();
        // Facts level: the regen-gate command in either command vector
        // implies the provision flag on every local provider, never GitHub.
        let mut variants: Vec<(&str, Unit)> = Vec::new();
        let mut pr = rust_unit("rust-check", "crates/check");
        pr.pr_commands = vec![CHECK.to_owned()];
        pr.full_commands = vec![OTHER.to_owned()];
        variants.push(("pr_commands", pr));
        let mut full = rust_unit("rust-check", "crates/check");
        full.pr_commands = vec![OTHER.to_owned()];
        full.full_commands = vec![CHECK.to_owned()];
        variants.push(("full_commands", full));
        for (name, unit) in &variants {
            let ir = owner_test_ir(&owner, vec![unit.clone()]);
            let contract = ir.default_unit_contract(&ir.units[0], false);
            for provider in [ProviderId::GithubSelfHosted, ProviderId::Velnor] {
                assert!(
                    ir.unit_provider_facts(&ir.units[0], &contract, provider)
                        .policy_runtime,
                    "{name}: {provider:?} provisions the pin for the regen gate"
                );
            }
            assert!(
                !ir.unit_provider_facts(&ir.units[0], &contract, ProviderId::GithubHosted)
                    .policy_runtime,
                "{name}: the hosted provider carries the pin in the Planning artifact instead"
            );
        }
        let plain = rust_unit("rust-plain", "crates/plain");
        let bare = owner_test_ir(&owner, vec![plain]);
        let contract = bare.default_unit_contract(&bare.units[0], false);
        for provider in ProviderId::ALL {
            assert!(
                !bare
                    .unit_provider_facts(&bare.units[0], &contract, provider)
                    .policy_runtime,
                "a unit without the regen gate provisions nothing on {provider:?}"
            );
        }
        // Render level: each collapsed local provider job provisions the
        // pinned policy binary behind the input gate.
        let mut check = rust_unit("rust-check", "crates/check");
        check.pr_commands = vec![CHECK.to_owned()];
        let ir = owner_test_ir(&owner, vec![check, rust_unit("rust-plain", "crates/plain")]);
        let content = must_render_kind(&ir);
        for job in ["verify-github-self-hosted", "verify-velnor"] {
            assert!(
                job_block(&content, job).contains(PROVISION),
                "{job} provisions the pinned policy binary behind the input gate: {}",
                job_block(&content, job)
            );
        }
        assert!(
            !job_block(&content, "verify-github-hosted")
                .contains("Provision pinned Velnor workflow policy runtime"),
            "the hosted provider carries the pin in the Planning artifact instead: {}",
            job_block(&content, "verify-github-hosted")
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
                2,
                "exactly the local regen-gate callers pass the flag on {kind:?}: {rendered}"
            );
        }
        let pr = ir.render_nested(WorkflowKind::PullRequest, &nodes, None);
        let headers: Vec<String> = ir
            .units
            .iter()
            .flat_map(|unit| ir.unit_provider_callers(unit, "ci-unit-rust.yml", None))
            .map(|caller| format!("  {}:\n", caller.job_id))
            .collect();
        for unit in &ir.units {
            for caller in ir.unit_provider_callers(unit, "ci-unit-rust.yml", None) {
                if !caller.provider.is_local() {
                    continue;
                }
                let header = format!("  {}:\n", caller.job_id);
                let start = must_some(pr.find(&header), "the local caller renders");
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
                        "the local regen-gate caller passes the flag: {block}"
                    );
                } else {
                    assert!(
                        !block.contains("policy_runtime: true"),
                        "a local caller without the regen gate passes no flag: {block}"
                    );
                }
            }
        }
    }

    #[test]
    fn provider_facts_carry_dependency_closure_and_admission() {
        let mut leaf = rust_unit("rust-leaf", "crates/leaf");
        leaf.depends_on = vec!["rust-root".to_owned()];
        let mut trusted = rust_unit("rust-trusted-leaf", "crates/trusted");
        trusted.trust = crate::s2::provider::TrustReq::TrustedOnly;
        let ir = owner_test_ir(
            "example/fixture",
            vec![
                rust_unit("rust-root", "crates/root"),
                leaf.clone(),
                trusted.clone(),
            ],
        );
        for (unit, provider, admission) in [
            (
                &leaf,
                ProviderId::GithubHosted,
                ProviderAdmission::Provider(ProviderId::GithubHosted),
            ),
            (
                &leaf,
                ProviderId::Velnor,
                ProviderAdmission::ProviderTrusted(ProviderId::Velnor),
            ),
            (
                &trusted,
                ProviderId::GithubHosted,
                ProviderAdmission::ProviderTrusted(ProviderId::GithubHosted),
            ),
        ] {
            let facts =
                ir.unit_provider_facts(unit, &ir.default_unit_contract(unit, true), provider);
            assert_eq!(facts.unit_dependencies, unit.depends_on, "{provider:?}");
            assert_eq!(facts.unit_admission, admission, "{provider:?}");
            let values = facts.input_values();
            assert!(
                values
                    .iter()
                    .any(|(name, value)| *name == provider_input::UNIT_ADMISSION
                        && value == admission.info_id()),
                "every caller passes its admission: {values:?}"
            );
        }
        let contract = ir.default_unit_contract(&leaf, true);
        let facts = ir.unit_provider_facts(&leaf, &contract, ProviderId::GithubHosted);
        assert!(
            facts.input_values().iter().any(|(name, value)| {
                *name == provider_input::UNIT_DEPENDENCIES && value == "rust-root"
            }),
            "a unit with dependencies passes them: {:?}",
            facts.input_values()
        );
        let root = &ir.units[0];
        let facts = ir.unit_provider_facts(
            root,
            &ir.default_unit_contract(root, true),
            ProviderId::GithubHosted,
        );
        assert!(
            facts
                .input_values()
                .iter()
                .all(|(name, _)| *name != provider_input::UNIT_DEPENDENCIES),
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
    fn github_lane_fetches_pin_history_for_check_running_units() {
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
            "both provider jobs render",
        );
        assert!(
            !velnor.contains("Fetch D19 pin history"),
            "the Velnor provider provisions the pin through its pinned-renderer step: {velnor}"
        );
        let fetch = must_some(
            hosted.find("      - name: Fetch D19 pin history"),
            "fetch step renders on the GitHub provider: {hosted}",
        );
        let checks = must_some(hosted.find("- name: Run unit checks"), "checks render");
        assert!(
            fetch < checks,
            "the pin is present before verification runs: {hosted}"
        );
        let step = &hosted[fetch..checks];
        assert!(
            step.contains("'rust-generator-crate') : ;;"),
            "the check-running member proceeds to the fetch: {step}"
        );
        assert!(
            step.contains("'rust-sibling-crate') exit 0 ;;"),
            "other members skip the fetch: {step}"
        );
        assert!(
            step.contains("git fetch --no-tags --depth 1"),
            "a shallow checkout gains the pin commit: {step}"
        );
    }

    #[test]
    fn docker_checks_steps_export_the_build_token_on_every_provider() {
        let owner = "example/owner";
        let ir = owner_test_ir(owner, vec![docker_unit("docker-example")]);
        let rendered = must_ok(
            ir.render_kind_unit_workflow(UnitKind::Docker, None),
            "docker kind reusable renders",
        );
        let content = must_some(rendered, "docker kind has members").1;
        let checks = content
            .split("- name: Run unit checks")
            .skip(1)
            .collect::<Vec<_>>();
        assert!(
            !checks.is_empty(),
            "docker kind renders checks steps: {content}"
        );
        for step in &checks {
            let env = must_some(step.split_once("run: |"), "checks step runs commands").0;
            assert!(
                env.contains("GITHUB_TOKEN: ${{ github.token }}"),
                "every docker checks step exports the token: {env}"
            );
        }
        let rust = owner_test_ir(owner, vec![rust_unit("rust-example", ".")]);
        let rust_content = must_some(
            must_ok(
                rust.render_kind_unit_workflow(UnitKind::Rust, None),
                "rust kind reusable renders",
            ),
            "rust kind has members",
        )
        .1;
        assert!(
            !rust_content.contains("GITHUB_TOKEN: ${{ github.token }}"),
            "other kinds run no secret-passing command and get no token: {rust_content}"
        );
    }

    #[test]
    fn report_inputs_carry_the_telemetry_lane_not_the_provider_id() {
        // The report action speaks telemetry schema v1 (`lane` is
        // `github|velnor`), so every provider maps to its lane label at
        // the call site; the provider id would neither match the action's
        // `ci_lane` input nor the schema enum.
        for (provider, lane) in [
            (ProviderId::GithubHosted, "github"),
            (ProviderId::GithubSelfHosted, "github"),
            (ProviderId::Velnor, "velnor"),
        ] {
            let mut output = String::new();
            super::render_phase_report_step(
                &mut output,
                "./.github/actions/report-velnor-ci-outcomes",
                "example",
                provider,
                &super::CacheReportFacts {
                    layers: BTreeSet::new(),
                    host_warm_layers: None,
                },
            );
            assert!(
                output.contains(&format!("ci_lane: {lane}\n")),
                "{provider:?} reports the {lane} lane: {output}"
            );
            assert!(
                !output.contains("ci_provider"),
                "{provider:?} must not rename the report action's input: {output}"
            );
        }
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
            "both provider jobs render",
        );
        assert!(
            !velnor.contains("candidate generator product"),
            "the Velnor provider never publishes: {velnor}"
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
        assert_eq!(
            hosted
                .matches(
                    "if: ${{ inputs.candidate_publish && (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository) }}"
                )
                .count(),
            1,
            "the prepare step carries the input gate merged with the pull-request same-repo gate: {hosted}"
        );
        assert!(
            hosted.contains(
                "if: ${{ inputs.candidate_publish && (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true') }}"
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
            1,
            "the prepare step keeps the bare pull-request same-repo gate: {solo_content}"
        );
        assert_eq!(
            solo_content
                .matches("if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true'\n")
                .count(),
            1,
            "the publish step adds the skip gate: {solo_content}"
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
            steps.contains("      - name: Publish candidate generator product\n        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true'\n"),
            "the publish gate adds the skip output: {steps}"
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

    #[test]
    fn plan_job_threads_expected_work_and_maps_no_work_outputs() {
        let ir = owner_test_ir("example/s4-plan", vec![rust_unit("rust", "crates/rust")]);
        let mut plan = String::new();
        ir.render_plan(&mut plan);
        assert!(
            plan.contains("VELNOR_EXPECTED_WORK_FILE: .velnor-ci-expected-work/expected-work.json"),
            "the plan step binds the expected-work file path: {plan}"
        );
        assert!(
            plan.contains("name: velnor-expected-work\n"),
            "the plan job publishes the expected-work artifact under its exact name: {plan}"
        );
        assert!(
            plan.contains(
                "path: .velnor-ci-expected-work/expected-work.json\n          if-no-files-found: error"
            ),
            "a missing expected-work file fails the plan job loudly, never silently: {plan}"
        );
        assert!(
            plan.contains("planned_no_work: ${{ steps.plan.outputs.planned_no_work }}"),
            "the plan job maps the no-work marker output: {plan}"
        );
        assert!(
            plan.contains("no_work_reason: ${{ steps.plan.outputs.no_work_reason }}"),
            "the plan job maps the no-work reason output: {plan}"
        );
    }

    #[test]
    fn plan_job_scopes_expected_work_providers_to_scheduled_universe() {
        // The plan scope must equal the scheduled provider scope: a narrowed
        // universe that plans unfiltered writes phantom entries for
        // unscheduled providers, and the aggregate fails closed on records
        // no job can report. There is no `providers` dispatch input —
        // manual dispatches select the static universe — so the plan
        // carries the static automatic set, which under the singleton
        // visibility policy is exactly the scheduled universe.
        let mut ir = owner_test_ir(
            "example/s4-plan-providers",
            vec![rust_unit("rust", "crates/rust")],
        );
        ir.providers = ProviderSet::from([ProviderId::GithubHosted]);
        ir.automatic_providers = ProviderSet::from([ProviderId::GithubHosted]);
        let mut plan = String::new();
        ir.render_plan(&mut plan);
        assert!(
            plan.contains("VELNOR_PROVIDERS: github-hosted\n"),
            "a single-provider universe plans only its provider: {plan}"
        );
        assert!(
            !plan.contains("inputs.providers"),
            "no dispatch provider input exists to narrow by: {plan}"
        );
    }

    #[test]
    fn plan_step_creates_expected_work_dir_before_invoking_plan() {
        // The pinned product predates the runtime's own parent creation, so
        // the branch-controlled render prepares the dir for the old binary.
        let ir = owner_test_ir(
            "example/s4-plan-mkdir",
            vec![rust_unit("rust", "crates/rust")],
        );
        let mut plan = String::new();
        ir.render_plan(&mut plan);
        let mkdir = must_some(
            plan.find(&format!("mkdir -p {}\n", super::EXPECTED_WORK_DIR)),
            "expected-work dir creation",
        );
        let invoke = must_some(
            plan.find("velnor-workflow plan --config"),
            "plan invocation",
        );
        assert!(
            mkdir < invoke,
            "the plan step creates the expected-work dir before invoking plan: {plan}"
        );
        assert_eq!(
            super::EXPECTED_WORK_FILE,
            format!("{}/expected-work.json", super::EXPECTED_WORK_DIR),
            "the env-bound file stays inside the created dir",
        );
    }

    #[test]
    fn no_work_branch_contract_is_presence_only() {
        let ir = owner_test_ir("example/s4-branch", vec![rust_unit("rust", "crates/rust")]);
        let nodes = aggregate_fixture_nodes(&ir);
        for kind in [WorkflowKind::PullRequest, WorkflowKind::Main] {
            let surface = ir.render_nested(kind, &nodes, None);
            assert!(
                surface.contains("never `false`"),
                "{kind:?} documents the presence-only marker contract: {surface}"
            );
            // The contract comment itself names the legal comparison; only
            // code lines count as branches.
            let code = surface
                .lines()
                .filter(|line| !line.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !code.contains("planned_no_work == 'false'"),
                "{kind:?} never branches on the impossible `false` value: {surface}"
            );
            assert!(
                !code.contains("planned_no_work ==") && !code.contains("planned_no_work !="),
                "{kind:?} branches nowhere on the marker: the aggregate tolerates no-work itself: {surface}"
            );
        }
    }

    #[test]
    fn required_check_scores_aggregate_before_the_shell_verdict() {
        let ir = owner_test_ir(
            "example/s4-required",
            vec![rust_unit("rust", "crates/rust")],
        );
        let nodes = aggregate_fixture_nodes(&ir);
        let mut rendered = String::new();
        ir.render_nodes_required(
            &nodes,
            None,
            &mut rendered,
            false,
            REQUIRED_CHECK,
            false,
            false,
        );
        assert!(
            rendered.contains("name: velnor-expected-work\n"),
            "ci-required downloads the expected-work artifact by exact name: {rendered}"
        );
        assert!(
            rendered.contains("pattern: velnor-result-*"),
            "ci-required collects every per-unit result artifact: {rendered}"
        );
        assert!(
            rendered.contains(
                "velnor-workflow aggregate --expected .velnor-ci-expected-work/expected-work.json --results .velnor-ci-results.json"
            ),
            "ci-required scores collected results against expected work: {rendered}"
        );
        assert!(
            rendered.contains("BASE_SHA: ${{ needs.plan.outputs.base_sha }}")
                && rendered.contains("HEAD_SHA: ${{ needs.plan.outputs.head_sha }}"),
            "the aggregate binds the plan identity from this run's plan outputs: {rendered}"
        );
        let aggregate = must_some(
            rendered.find("velnor-workflow aggregate"),
            "aggregate invocation",
        );
        let verdict = must_some(
            rendered.find("Validate generated stack results"),
            "shell verdict step",
        );
        assert!(
            aggregate < verdict,
            "the aggregate scores before the shell verdict re-confirms: {rendered}"
        );
        assert!(
            rendered.contains("plan_expects"),
            "the shell verdict still evaluates every caller against the plan: {rendered}"
        );
    }

    #[test]
    fn record_step_creates_result_dir_before_first_write() {
        let steps = super::render_unit_result_steps(
            "actions/upload-artifact@pinned",
            "github-hosted",
            "always()",
        );
        let mkdir = must_some(
            steps.find("mkdir -p .velnor-ci-results"),
            "result dir creation",
        );
        let write = must_some(
            steps.find("> \".velnor-ci-results/result-"),
            "first result write",
        );
        assert!(
            mkdir < write,
            "the record step creates the result dir before writing: {steps}"
        );
        assert!(
            steps.contains("path: .velnor-ci-results/result-${{ inputs.unit }}-github-hosted.json"),
            "the upload publishes exactly what the record step wrote: {steps}"
        );
    }

    #[test]
    fn collect_step_creates_result_dir_before_first_read() {
        let steps = super::render_aggregate_score_steps("", "actions/download-artifact@pinned");
        let mkdir = must_some(
            steps.find("mkdir -p .velnor-ci-results"),
            "result dir creation",
        );
        let glob = must_some(
            steps.find("files=(.velnor-ci-results/result-*.json)"),
            "result glob",
        );
        assert!(
            mkdir < glob,
            "the collect step creates the result dir before globbing: {steps}"
        );
    }

    #[test]
    fn aggregate_wiring_leaves_the_shell_verdict_verbatim() {
        let ir = owner_test_ir(
            "example/s4-verbatim",
            vec![rust_unit("rust", "crates/rust")],
        );
        let nodes = aggregate_fixture_nodes(&ir);
        let callers = ir.required_callers(&nodes, None);
        let mut verdicts = String::new();
        super::render_required_caller_verdicts(&mut verdicts, &callers);
        let mut rendered = String::new();
        ir.render_nodes_required(
            &nodes,
            None,
            &mut rendered,
            false,
            REQUIRED_CHECK,
            false,
            false,
        );
        assert!(
            rendered.contains(&verdicts),
            "the per-caller verdict block renders byte-for-byte beside the aggregate: {rendered}"
        );
    }

    #[test]
    fn aggregate_failure_can_fail_the_required_check() {
        let ir = owner_test_ir("example/s4-fail", vec![rust_unit("rust", "crates/rust")]);
        let nodes = aggregate_fixture_nodes(&ir);
        let rendered = ir.render_nested(WorkflowKind::PullRequest, &nodes, None);
        let check = job_block(&rendered, REQUIRED_CHECK);
        for step in [
            "Download expected work",
            "Collect reported unit results",
            "Score expected work against reported results",
        ] {
            let start = must_some(check.find(step), step);
            let block = &check[start..];
            let end = block.find("\n      - name: ").unwrap_or(block.len());
            let block = &block[..end];
            assert!(
                !block.contains("continue-on-error"),
                "{step} must fail the check, never tolerate: {block}"
            );
        }
        let aggregate = must_some(check.find("velnor-workflow aggregate"), "aggregate script");
        assert!(
            check[..aggregate].contains("set -euo pipefail"),
            "the aggregate step runs under fail-fast shell options: {check}"
        );
        assert!(
            !check.contains("aggregate --expected") || !check.contains("|| true"),
            "nothing swallows the aggregate exit status: {check}"
        );
    }

    #[test]
    fn result_collection_rejects_reused_evidence_without_a_decision() {
        let ir = owner_test_ir("example/s4-reuse", vec![rust_unit("rust", "crates/rust")]);
        let nodes = aggregate_fixture_nodes(&ir);
        let mut rendered = String::new();
        ir.render_nodes_required(
            &nodes,
            None,
            &mut rendered,
            false,
            REQUIRED_CHECK,
            false,
            false,
        );
        assert!(
            rendered.contains("reused_from"),
            "collection names the refused reused-evidence field: {rendered}"
        );
        let kind = must_render_kind(&ir);
        assert!(
            !kind.contains("reused_from"),
            "no rendered producer emits reused results: {kind}"
        );
    }

    #[test]
    fn unit_jobs_emit_exactly_one_result_record_per_provider() {
        let ir = owner_test_ir("example/s4-record", vec![rust_unit("rust", "crates/rust")]);
        let kind = must_render_kind(&ir);
        for (job, provider) in [
            ("verify-github-hosted", "github-hosted"),
            ("verify-github-self-hosted", "github-self-hosted"),
            ("verify-velnor", "velnor"),
        ] {
            let block = job_block(&kind, job);
            assert_eq!(
                block.matches("- name: Record unit result").count(),
                1,
                "{job} records exactly one result: {block}"
            );
            assert_eq!(
                block.matches("- name: Upload unit result").count(),
                1,
                "{job} uploads exactly one result: {block}"
            );
            assert!(
                block.contains("always()"),
                "{job} records even when the checks fail: {block}"
            );
            assert!(
                block.contains(&format!("VELNOR_RESULT_LANE: {provider}")),
                "{job} pins the schema-3 provider vocabulary: {block}"
            );
            assert!(
                block.contains("overwrite: true"),
                "{job} retries overwrite the one record: {block}"
            );
            assert!(
                block.contains("job.status"),
                "{job} derives the outcome from the job status PR code cannot fake: {block}"
            );
        }
    }

    #[test]
    fn trust_split_providers_record_exactly_once() {
        let mut trusted = rust_unit("rust-trusted", "crates/trusted");
        trusted.trust = crate::s2::provider::TrustReq::TrustedOnly;
        let ir = owner_test_ir(
            "example/s4-split",
            vec![rust_unit("rust-plain", "crates/plain"), trusted],
        );
        let kind = must_render_kind(&ir);
        let plain = job_block(&kind, "verify-velnor");
        let gated = job_block(&kind, "verify-velnor-trusted");
        assert!(
            plain.contains("inputs.unit_trust == 'untrusted-ok'"),
            "the plain job records only plain dispatches: {plain}"
        );
        assert!(
            gated.contains("inputs.unit_trust == 'trusted-only'"),
            "the trusted job records only trusted dispatches: {gated}"
        );
        assert_eq!(
            kind.matches("- name: Record unit result").count(),
            5,
            "one record step per provider job across the split: {kind}"
        );
    }

    /// The repository pin, as the scan parses it: channel plus the file's
    /// own components, targets, and profile.
    fn pin_toolchain() -> RustToolchain {
        RustToolchain {
            channel: "1.91.1".to_owned(),
            components: vec!["clippy".to_owned(), "rustfmt".to_owned()],
            targets: vec!["wasm32-unknown-unknown".to_owned()],
            profile: Some("minimal".to_owned()),
        }
    }

    /// A declared per-unit channel: a bare channel string with no pin facts.
    fn declared_toolchain(channel: &str) -> RustToolchain {
        RustToolchain {
            channel: channel.to_owned(),
            components: Vec::new(),
            targets: Vec::new(),
            profile: None,
        }
    }

    /// A hosted-only IR over `units` with the recorded pin: multi-channel
    /// coverage renders without the local providers the matrix refuses.
    fn matrix_test_ir(units: Vec<Unit>, rust_pin: Option<RustToolchain>) -> WorkflowIr {
        let mut ir = owner_test_ir("example/toolchain-matrix", units);
        ir.providers = BTreeSet::from([ProviderId::GithubHosted]);
        ir.automatic_providers = BTreeSet::from([ProviderId::GithubHosted]);
        ir.rust_pin = rust_pin;
        ir
    }

    fn caller_toolchain(ir: &WorkflowIr, unit: &Unit) -> Option<String> {
        ir.unit_provider_callers(unit, "ci-unit-rust.yml", None)
            .iter()
            .find_map(|caller| {
                caller
                    .inputs
                    .iter()
                    .find(|(name, _)| *name == provider_input::TOOLCHAIN)
                    .map(|(_, value)| value.clone())
            })
    }

    #[test]
    fn single_channel_kind_carries_no_toolchain_input() {
        let pin = pin_toolchain();
        let mut primary = rust_unit("rust", "crates/primary");
        primary.toolchain = Some(pin.clone());
        let mut second = rust_unit("rust-second", "crates/second");
        second.toolchain = Some(pin.clone());
        let ir = matrix_test_ir(vec![primary.clone(), second], Some(pin));
        let kind = must_render_kind(&ir);
        assert!(
            !kind.contains("inputs.toolchain"),
            "a single-channel kind provisions without any input: {kind}"
        );
        assert!(
            !kind.contains("toolchain:\n        required: false"),
            "a single-channel kind declares no toolchain input: {kind}"
        );
        assert!(
            kind.contains("rustup toolchain install --profile minimal"),
            "the pin leg keeps the file-driven install: {kind}"
        );
        assert!(
            caller_toolchain(&ir, &primary).is_none(),
            "single-channel callers pass no toolchain input"
        );
    }

    #[test]
    fn two_channel_kind_renders_one_gated_leg_per_channel() {
        let pin = pin_toolchain();
        let mut primary = rust_unit("rust", "crates/primary");
        primary.toolchain = Some(pin.clone());
        let mut msrv = rust_unit("rust-msrv", "crates/primary");
        msrv.toolchain = Some(declared_toolchain("1.88.0"));
        let ir = matrix_test_ir(vec![primary.clone(), msrv.clone()], Some(pin));
        let kind = must_render_kind(&ir);
        // The callee declares the leg input; each caller passes its unit's
        // own channel. No `strategy.matrix`: callers already fan out one job
        // per unit, so a matrix would run every unit on every channel.
        assert!(
            kind.contains("      toolchain:\n        required: false\n        type: string"),
            "the kind reusable declares the toolchain input: {kind}"
        );
        assert!(
            !kind.contains("strategy:") && !kind.contains("matrix."),
            "legs ride per-unit inputs, never a multiplied matrix: {kind}"
        );
        assert_eq!(
            caller_toolchain(&ir, &primary).as_deref(),
            Some("1.91.1"),
            "the primary caller passes the pin channel"
        );
        assert_eq!(
            caller_toolchain(&ir, &msrv).as_deref(),
            Some("1.88.0"),
            "the MSRV caller passes its declared channel"
        );
        // The pin leg: file-driven, gated, today's cache key and step id.
        assert!(
            kind.contains("if: ${{ inputs.toolchain == '1.91.1' }}"),
            "the pin leg gates on its channel: {kind}"
        );
        assert!(
            kind.contains("rustup toolchain install --profile minimal\n"),
            "the pin leg keeps the file-driven install: {kind}"
        );
        // The MSRV leg: explicit install, leg-suffixed key and step id, and
        // the export that retargets every later plain `cargo` invocation.
        assert!(
            kind.contains("if: ${{ inputs.toolchain == '1.88.0' }}"),
            "the MSRV leg gates on its channel: {kind}"
        );
        assert!(
            kind.contains("rustup toolchain install '1.88.0' --profile 'minimal'\n"),
            "the MSRV leg installs its channel explicitly: {kind}"
        );
        assert!(
            kind.contains("key: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-1.88.0"),
            "the MSRV leg keys its cache on the channel: {kind}"
        );
        assert!(
            kind.contains("id: rustup-toolchain-1-88-0"),
            "the MSRV leg restores under its own step id: {kind}"
        );
        assert!(
            kind.contains("steps.rustup-toolchain-1-88-0.outputs.cache-hit != 'true'"),
            "the MSRV save gate reads its own leg's cache-hit: {kind}"
        );
        assert!(
            kind.contains("echo \"RUSTUP_TOOLCHAIN=1.88.0\" >> \"$GITHUB_ENV\""),
            "the MSRV leg retargets later cargo invocations: {kind}"
        );
        assert!(
            !kind.contains("+1.88.0"),
            "both legs run plain commands, never `cargo +toolchain`: {kind}"
        );
    }

    #[test]
    fn declared_pin_channel_joins_the_pin_leg() {
        let pin = pin_toolchain();
        let mut primary = rust_unit("rust", "crates/primary");
        primary.toolchain = Some(pin.clone());
        // A bare declaration equal to the pin channel is still the pin leg:
        // the file's own facts provision it, not the bare declaration.
        let mut alias = rust_unit("rust-alias", "crates/primary");
        alias.toolchain = Some(declared_toolchain("1.91.1"));
        let mut msrv = rust_unit("rust-msrv", "crates/primary");
        msrv.toolchain = Some(declared_toolchain("1.88.0"));
        let ir = matrix_test_ir(vec![primary, alias, msrv], Some(pin));
        let kind = must_render_kind(&ir);
        // Two legs, each gating its restore/provision/save steps on its own
        // channel: the alias rides the pin leg instead of a third.
        let pin_gates = kind.matches("inputs.toolchain == '1.91.1'").count();
        let msrv_gates = kind.matches("inputs.toolchain == '1.88.0'").count();
        assert!(
            pin_gates > 0 && pin_gates == msrv_gates,
            "two symmetric legs, no third: {kind}"
        );
        assert!(
            kind.contains("rustup target add 'wasm32-unknown-unknown'"),
            "the pin leg provisions the file's targets: {kind}"
        );
    }

    #[test]
    fn channel_collision_refuses() {
        let pin = pin_toolchain();
        let mut first = rust_unit("rust-first", "crates/first");
        first.toolchain = Some(RustToolchain {
            targets: vec!["wasm32-unknown-unknown".to_owned()],
            ..declared_toolchain("1.88.0")
        });
        let mut second = rust_unit("rust-second", "crates/second");
        second.toolchain = Some(declared_toolchain("1.88.0"));
        let ir = matrix_test_ir(vec![first, second], Some(pin));
        let error = must_err(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "one channel with two toolchains must fail",
        );
        assert!(
            error.to_string().contains("1.88.0"),
            "the refusal names the collided channel: {error}"
        );
    }

    #[test]
    fn explicit_step_ids_sanitize_and_dedupe() {
        let mut taken = BTreeSet::from(["rustup-toolchain".to_owned()]);
        assert_eq!(
            explicit_toolchain_step_id("1.88.0", &mut taken),
            "rustup-toolchain-1-88-0"
        );
        assert_eq!(
            explicit_toolchain_step_id("1-88-0", &mut taken),
            "rustup-toolchain-1-88-0-2",
            "channels that sanitize alike still restore under disjoint ids"
        );
        assert_eq!(
            explicit_toolchain_step_id("nightly", &mut taken),
            "rustup-toolchain-nightly"
        );
    }

    #[test]
    fn matrix_on_a_local_provider_refuses_at_render() {
        let pin = pin_toolchain();
        let mut primary = rust_unit("rust", "crates/primary");
        primary.toolchain = Some(pin.clone());
        let mut msrv = rust_unit("rust-msrv", "crates/primary");
        msrv.toolchain = Some(declared_toolchain("1.88.0"));
        // The full provider universe: the local jobs cannot provision legs.
        let mut ir = owner_test_ir("example/toolchain-local", vec![primary, msrv]);
        ir.rust_pin = Some(pin);
        let error = must_err(
            ir.render_kind_unit_workflow(UnitKind::Rust, None),
            "a matrix on a local provider must fail",
        );
        assert!(
            error
                .to_string()
                .contains("cannot provision per-leg toolchains"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_pin_renders_every_leg_explicitly() {
        let mut primary = rust_unit("rust", "crates/primary");
        primary.toolchain = Some(declared_toolchain("stable"));
        let mut msrv = rust_unit("rust-msrv", "crates/primary");
        msrv.toolchain = Some(declared_toolchain("1.88.0"));
        let ir = matrix_test_ir(vec![primary, msrv], None);
        let kind = must_render_kind(&ir);
        assert!(
            kind.contains("rustup toolchain install 'stable' --profile 'minimal'"),
            "without a recorded pin no leg may assume the file: {kind}"
        );
        assert!(
            kind.contains("rustup toolchain install '1.88.0' --profile 'minimal'"),
            "without a recorded pin no leg may assume the file: {kind}"
        );
        assert!(
            !kind.contains("Restore Rust toolchain\n"),
            "without a recorded pin no leg renders the file-driven block: {kind}"
        );
    }

    #[test]
    fn literal_provision_follows_the_unit_leg() {
        let pin = pin_toolchain();
        let mut pinned = rust_unit("rust", "crates/primary");
        pinned.toolchain = Some(pin.clone());
        let mut msrv = rust_unit("rust-msrv", "crates/primary");
        msrv.toolchain = Some(declared_toolchain("1.88.0"));
        let ir = matrix_test_ir(vec![pinned.clone(), msrv.clone()], Some(pin));
        let mut file_driven = String::new();
        ir.render_tool_provisioning(&mut file_driven, ProviderId::GithubHosted, &pinned, true);
        assert!(
            file_driven.contains("rustup toolchain install --profile minimal\n"),
            "the pin leg provisions file-driven: {file_driven}"
        );
        let mut explicit = String::new();
        ir.render_tool_provisioning(&mut explicit, ProviderId::GithubHosted, &msrv, true);
        assert!(
            explicit.contains("rustup toolchain install '1.88.0' --profile 'minimal'"),
            "a non-pin leg provisions explicitly: {explicit}"
        );
        assert!(
            explicit.contains("echo \"RUSTUP_TOOLCHAIN=1.88.0\" >> \"$GITHUB_ENV\""),
            "a non-pin leg retargets later cargo invocations: {explicit}"
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

/// A dependency-bundle cache key: OS/arch plus the provider/platform/trust
/// segments that make cross-tier and cross-provider hits impossible. The
/// segments are literals where the generator knows them, `${{ inputs.* }}`
/// expressions in collapsed jobs.
fn format_cargo_bundle_cache_key(
    id_segment: &str,
    hash_expression: &str,
    provider: &str,
    platform: &str,
    trust: &str,
) -> (String, String) {
    let cache_key = format!(
        "ci-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{provider}-{platform}-{trust}-{id_segment}-${{{{ hashFiles({hash_expression}) }}}}"
    );
    let restore_prefix = format!(
        "ci-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{provider}-{platform}-{trust}-{id_segment}-"
    );
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
/// names — those are provider-specific facts a repo contract states explicitly —
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
        xcode: None,
        host_image: config
            .selectors
            .get(&ProviderId::GithubHosted)
            .and_then(|selector| selector.runs_on.first().cloned())
            .unwrap_or_default(),
        provider: ProviderId::GithubHosted.as_str().to_owned(),
        platform: crate::s2::provider::Platform::LinuxX64.as_str().to_owned(),
        trust: crate::s2::provider::TrustReq::UntrustedOk
            .as_str()
            .to_owned(),
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
    provider: ProviderId,
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
        xcode: unit.xcode.clone(),
        host_image: ir
            .selectors
            .get(&provider)
            .and_then(|selector| selector.runs_on.first().cloned())
            .unwrap_or_default(),
        provider: provider.as_str().to_owned(),
        platform: unit.platform.as_str().to_owned(),
        trust: unit.trust.as_str().to_owned(),
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
fn unit_snapshot(
    ir: &WorkflowIr,
    unit: &Unit,
    provider: ProviderId,
    namespace: &str,
) -> (String, String) {
    let facts = unit_snapshot_facts(ir, unit, provider);
    let dependency = freshness_expression(&facts.dependency_files);
    let class_prefix = snapshot_class_prefix(namespace, &facts.compatibility);
    let state = freshness_expression(&facts.state_files);
    let segments = KeySegments {
        provider: provider.as_str().to_owned(),
        platform: unit.platform.as_str().to_owned(),
        trust: unit.trust.as_str().to_owned(),
    };
    (
        snapshot_key(&class_prefix, &unit.id, &dependency, &state, &segments),
        snapshot_restore_keys(&class_prefix, &unit.id, &dependency, &segments),
    )
}

/// The three unit-specific parts of a snapshot key, for callers that pass
/// them through `workflow_call` inputs instead of rendering the literal key.
fn unit_snapshot_facts(ir: &WorkflowIr, unit: &Unit, provider: ProviderId) -> SnapshotFacts {
    let members = closure_members(unit, &ir.units);
    let dependency_files = snapshot_dependency_inputs(&members);
    let compatibility = snapshot_compatibility(ir, unit, provider, &dependency_files).digest();
    SnapshotFacts {
        compatibility,
        dependency_files,
        state_files: snapshot_state_files(&members, unit),
    }
}

/// The snapshot key and restore prefixes a collapsed provider job renders: the
/// same segment grammar as [`unit_snapshot`], with the unit id, provider,
/// platform, trust, compatibility digest, and both `hashFiles` lists read
/// from the caller's inputs.
fn input_snapshot(
    namespace: &str,
    compat_input: &str,
    dependency_input: &str,
    freshness_input: &str,
) -> (String, String) {
    let class_prefix = snapshot_class_prefix(namespace, &provider_input::expression(compat_input));
    let segments = KeySegments {
        provider: provider_input::expression("provider"),
        platform: provider_input::expression(provider_input::UNIT_PLATFORM),
        trust: provider_input::expression(provider_input::UNIT_TRUST),
    };
    let unit = provider_input::expression("unit");
    let dependency = provider_input::hash_files_expression(dependency_input);
    let state = provider_input::hash_files_expression(freshness_input);
    (
        snapshot_key(&class_prefix, &unit, &dependency, &state, &segments),
        snapshot_restore_keys(&class_prefix, &unit, &dependency, &segments),
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
fn render_phase_epoch_marker(output: &mut String, epoch_key: &str, step_name: &str) {
    let marker = render_epoch_marker_commands(epoch_key, "          ");
    let _ = writeln!(
        output,
        "      - name: {step_name}\n        if: always()\n        run: |\n{marker}"
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

pub(crate) fn render_ci_cleanup_end_marker(output: &mut String) {
    render_phase_epoch_marker(output, "CLEANUP_ENDED", "Mark cleanup end");
}

/// Emit the post-checks report step shared by both render paths.
///
/// `job_display_name` is the Actions API job name for this job (the workflow
/// `jobs.<id>.name` value), which queue-time lookup matches on exactly.
pub(crate) fn render_phase_report_step(
    output: &mut String,
    report_action_uses: &str,
    job_display_name: &str,
    provider: ProviderId,
    facts: &CacheReportFacts,
) {
    output.push_str("      - name: Report phase timings and cache outcomes\n");
    output.push_str("        if: always()\n");
    let _ = writeln!(
        output,
        "        uses: {report_action_uses}\n        with:\n          job_label: {job_display_name}"
    );
    render_cache_outcome_report_inputs(output, provider, facts);
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
/// job, or unioned over a kind's members for a collapsed provider job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheReportFacts {
    pub(crate) layers: BTreeSet<ReportedCacheLayer>,
    /// The rendered `VELNOR_HOST_WARM_LAYERS` value on the Velnor provider.
    pub(crate) host_warm_layers: Option<String>,
}

impl CacheReportFacts {
    pub(crate) fn for_unit(provider: ProviderId, unit: &Unit, ir: &WorkflowIr) -> Self {
        let tools = WorkflowIr::tools_for_unit(unit, ir.mise_present, ir.mr_boxington);
        let seed = unit
            .cache
            .as_ref()
            .is_some_and(|cache| cache.mutable_mount_seed);
        if provider == ProviderId::Velnor {
            let layers = local_host_warm_layers(unit, ir);
            return Self {
                host_warm_layers: (!layers.is_empty()).then(|| layers.join(",")),
                ..Self::default()
            };
        }
        let mut layers = BTreeSet::new();
        if !local_skips_pinned_rust_toolchain(provider) && unit.toolchain.is_some() {
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
                && CacheBackend::Detected.provider_enables_actions_cache(provider, ir, unit)
        }) {
            layers.insert(ReportedCacheLayer::CargoBundle);
        }
        if seed && provider == ProviderId::GithubHosted {
            layers.insert(ReportedCacheLayer::DockerSeed);
        }
        Self {
            layers,
            host_warm_layers: None,
        }
    }

    /// The union over a collapsed provider job's members: a layer is reported
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
            _ => Some(provider_input::expression(provider_input::HOST_WARM_LAYERS)),
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
fn local_host_warm_layers(unit: &Unit, ir: &WorkflowIr) -> Vec<&'static str> {
    let tools = WorkflowIr::tools_for_unit(unit, ir.mise_present, ir.mr_boxington);
    let mut host_warm = Vec::<&'static str>::new();
    if local_skips_pinned_rust_toolchain(ProviderId::Velnor) && unit.toolchain.is_some() {
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
        .is_some_and(cache_is_local_host_persistent)
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
    provider: ProviderId,
    facts: &CacheReportFacts,
) {
    // The report action speaks telemetry schema v1, whose `lane` enum is
    // `github|velnor`: the provider maps to the lane label at this
    // boundary instead of renaming the action's input. Both GitHub
    // fleets are the github lane; only the Velnor fleet is velnor.
    let lane_name = match provider {
        ProviderId::GithubHosted | ProviderId::GithubSelfHosted => "github",
        ProviderId::Velnor => "velnor",
    };
    let _ = writeln!(output, "          ci_lane: {lane_name}");
    if provider.is_local() {
        if let Some(layers) = &facts.host_warm_layers {
            let _ = writeln!(output, "          host_warm_layers: {layers}");
            // The Velnor lane has no restore steps to observe; the host-warm
            // list is the declaration, so the classifier reports listed
            // layers as declared-but-unobserved instead of disabled.
            let _ = writeln!(output, "          cache_declared_layers: {layers}");
        }
        return;
    }
    let declared = facts
        .layers
        .iter()
        .map(|layer| layer.report_input_prefix())
        .collect::<Vec<_>>()
        .join(",");
    let _ = writeln!(output, "          cache_declared_layers: {declared}");
    for layer in &facts.layers {
        let input_prefix = layer.report_input_prefix();
        let step_id = layer.step_id();
        let _ = writeln!(
            output,
            "          cache_{input_prefix}_outcome: ${{{{ steps.{step_id}.outcome }}}}"
        );
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
    let mut commands = unit.pr_commands.iter().chain(&unit.full_commands);
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

/// The checks-step token a Docker job exports: its image build commands
/// pass `--secret id=github_token,env=GITHUB_TOKEN`, so the step must
/// provide the automatic token. Every provider runs the same commands, so
/// every provider's Docker checks step exports it. Other kinds run no such
/// command and get no token.
pub(crate) fn docker_build_token_env_for_members(members: &[&Unit]) -> &'static str {
    if members.iter().any(|unit| unit.kind == UnitKind::Docker) {
        "\n          GITHUB_TOKEN: ${{ github.token }}"
    } else {
        ""
    }
}

/// The checks env of a collapsed provider job. `CARGO_NET_OFFLINE` is a literal
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
            provider_input::expression(provider_input::CARGO_NET_OFFLINE)
        );
    }
    env.push_str(mise_auto_install_env());
    env
}

/// Export the sccache defaults after setup for the selected member. Collapsed
/// jobs cannot use a per-unit job env because one job serves members with
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

/// Every command the unit runs, on either provider: the scan-derived base
/// commands plus every provider-specific override a repo-owned config declared.
/// A unit may pin commands per provider only — its base vectors then stay empty —
/// so a predicate that skips the overrides would conclude the unit runs
/// nothing at all.
fn unit_commands(unit: &Unit) -> impl Iterator<Item = &String> {
    unit.pr_commands.iter().chain(&unit.full_commands)
}

/// Whether the unit runs the generator's own regeneration gate
/// (`--plain --check`), whose D19 guard needs the pinned policy binary while
/// the checks run with the network restricted.
pub(crate) fn unit_runs_workflow_plain_check(unit: &Unit) -> bool {
    unit_commands(unit).any(|command| command.contains("--plain --check"))
}

/// The D19 pin-fetch commands every hosted job that runs the generator's
/// `--plain --check` renders before its checks: resolve the declared pin
/// from the generation config and fetch the commit when the shallow
/// checkout lacks it, so the guard's `closure_of_tree(pin)` reads from
/// local history. The collapsed provider job and the release legs share
/// these lines so the two paths cannot drift.
pub(crate) const D19_PIN_FETCH_COMMANDS: &str = "          pin=\"$(sed -n -E 's/^[[:space:]]*revision[[:space:]]*=[[:space:]]*\"([0-9a-f]{40})\".*/\\1/p' .github-gen/velnor-workflow.toml | head -n 1)\"\n          test \"$pin\" != '' || { echo \"::error::D19 pin missing from .github-gen/velnor-workflow.toml\" >&2; exit 1; }\n          if ! git cat-file -e \"$pin^{commit}\" 2>/dev/null; then\n            git fetch --no-tags --depth 1 \"$GITHUB_SERVER_URL/$GITHUB_REPOSITORY\" \"$pin\"\n          fi";

/// Whether the unit owns the generator crate itself: a Rust unit rooted at
/// the generator crate's manifest directory. The scan mints one unit per
/// manifest root, so at most one unit per repository matches; the owner
/// repository's tree carries exactly that unit, and fixture trees carry
/// none. The match is structural (kind plus root), never the unit id, so a
/// renamed unit cannot silently gain or lose the publish.
fn unit_owns_workflow_crate(unit: &Unit) -> bool {
    unit.kind == UnitKind::Rust && unit.root == "crates/velnor-workflow"
}

/// Stage-1 candidate packaging steps for the collapsed hosted provider job,
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
fn candidate_publish_steps(upload_artifact_pin: &str) -> String {
    format!(
        r#"      - name: Prepare candidate generator product
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
      - name: Publish candidate generator product
        if: github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && steps.candidate.outputs.skip != 'true'
        uses: {upload_artifact_pin}
        with:
          name: ${{{{ steps.candidate.outputs.name }}}}
          path: ${{{{ runner.temp }}}}/velnor-workflow-candidate
          if-no-files-found: error
          retention-days: 1
"#,
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
            .map(|target| crate::s2::shell_quote(target))
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

/// Render one explicit-channel provision leg of a multi-channel Rust kind:
/// restore the leg-suffixed `~/.rustup` cache, install the leg's channel by
/// name, and export `RUSTUP_TOOLCHAIN` so every later cargo invocation —
/// fetch, checks, tool installs — resolves to the leg instead of the pin
/// file the checkout carries. The pin leg keeps the file-driven install
/// above; only legs whose channel differs from the pin render here.
///
/// The cache key names the channel instead of hashing the pin files: every
/// leg checks out the same pin file, so a file hash would collide across
/// legs and restore another channel's toolchain state.
pub(crate) fn render_explicit_toolchain_steps(
    output: &mut String,
    cache_restore: &str,
    cache_save: &str,
    toolchain: &RustToolchain,
    step_id: &str,
    save_gate: Option<&str>,
) {
    let (paths, _) = rendered_cache_values(&CacheSpec {
        key_files: Vec::new(),
        paths: vec!["~/.rustup".to_owned()],
        purpose: CachePurpose::Toolchains,
        mbx_output_cache_justification: None,
        mutable_mount_seed: false,
    });
    let channel = crate::s2::shell_quote(&toolchain.channel);
    let key = format!(
        "velnor-rustup-${{{{ runner.os }}}}-${{{{ runner.arch }}}}-{}",
        toolchain.channel
    );
    let _ = writeln!(
        output,
        "      - name: Restore Rust toolchain ({})\n        id: {step_id}\n        uses: {cache_restore}\n        with:\n          path: |\n{paths}\n          key: {key}",
        toolchain.channel
    );
    // A declared channel carries no components, targets, or profile of its
    // own: the leg provisions it with a minimal profile, so pin-only
    // components can never leak onto a channel that lacks them. Targets are
    // qualified with `--toolchain`: the checkout's pin file still resolves
    // every unqualified rustup invocation to the pin channel.
    let profile = toolchain.profile.as_deref().unwrap_or("minimal");
    let mut install = format!(
        "rustup toolchain install {channel} --profile {}",
        crate::s2::shell_quote(profile)
    );
    for component in &toolchain.components {
        let _ = write!(install, " -c {}", crate::s2::shell_quote(component));
    }
    let _ = writeln!(
        output,
        "      - name: Provision Rust toolchain ({})\n        shell: bash\n        run: |\n          set -euo pipefail\n          {install}",
        toolchain.channel
    );
    if !toolchain.targets.is_empty() {
        let targets = toolchain
            .targets
            .iter()
            .map(|target| crate::s2::shell_quote(target))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            output,
            "          rustup target add --toolchain {channel} {targets}"
        );
    }
    let _ = writeln!(
        output,
        "          echo \"RUSTUP_TOOLCHAIN={}\" >> \"$GITHUB_ENV\"",
        toolchain.channel
    );
    if let Some(save_gate) = save_gate {
        let _ = writeln!(
            output,
            "      - name: Save Rust toolchain ({})\n        if: {save_gate}\n        uses: {cache_save}\n        with:\n          path: |\n{paths}\n          key: {key}",
            toolchain.channel
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
    if needs_boltffi(unit) {
        push_mise_tool(&mut tools, BOLTFFI_TOOL.to_owned());
    }
    for declared in &unit.mise_tools {
        push_mise_tool(&mut tools, declared.clone());
    }
    tools
}

/// Whether any of the unit's commands drive the `BoltFFI` pack the native join
/// appends to a producing Rust unit. One predicate feeds both the mise
/// install list and the lock validation, so the two can never disagree
/// about what a unit needs.
pub(crate) fn needs_boltffi(unit: &Unit) -> bool {
    unit_commands(unit).any(|command| {
        let mut seen_boltffi = false;
        command.split_whitespace().any(|token| {
            // Shell separators start a new simple command: a `pack` past
            // one belongs to whatever follows, not to the earlier `boltffi`.
            if matches!(token, "&&" | "||" | ";" | "|") {
                seen_boltffi = false;
                return false;
            }
            if seen_boltffi {
                token == "pack"
            } else {
                seen_boltffi = token == "boltffi" || token.ends_with("/boltffi");
                false
            }
        })
    })
}

/// The single mise spelling the `BoltFFI` CLI pins under: the cargo backend
/// serving the `boltffi_cli` crate, as evidenced by the `mise.lock` key.
/// `mise --locked` requires install args to equal the lock keys byte for
/// byte, so validation refuses any other state before rendering.
pub(crate) const BOLTFFI_TOOL: &str = "cargo:boltffi_cli";

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

/// Tool ids the Velnor provider installs explicitly. The job image already pins
/// common CI tools (Bun, `OpenTofu`, mold, Mr. Boxington); this list covers
/// only what a unit's commands or repo declarations pull from the root lock,
/// plus policy tools the hosted provider supplies through dedicated setup actions.
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

/// The Velnor-provider mise install for a collapsed provider job: the tool ids arrive
/// space-separated through the `mise_tools` input and are split as shell words.
fn render_velnor_mise_install_from_input(output: &mut String) {
    let _ = writeln!(
        output,
        "      - name: Install declared Mise tools\n        env:\n          MISE_TOOLS: {}\n        run: |\n          set -euo pipefail\n          read -ra tools <<<\"$MISE_TOOLS\"\n          mise --yes install \"${{tools[@]}}\"",
        provider_input::expression(provider_input::MISE_TOOLS)
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

/// Refuse a unit that needs `BoltFFI` while the root lock does not pin its
/// tool id: rendering anything else would emit `install_args` the runner's
/// own lock check rejects, and unlike nextest there is no cargo-bin
/// fallback, so an absent lock fails here rather than at pack time.
///
/// # Errors
/// Returns a usage error naming the first unit whose `BoltFFI` need the lock
/// does not pin, with every key the lock does pin.
pub(crate) fn validate_boltffi_tools_are_locked(
    units: &[Unit],
    lock_keys: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if lock_keys.contains(BOLTFFI_TOOL) {
        return Ok(());
    }
    for unit in units {
        if needs_boltffi(unit) {
            let known = lock_keys.iter().cloned().collect::<Vec<_>>().join(", ");
            return Err(GeneratorError::usage(format!(
                "unit {} runs boltffi pack but mise.lock does not pin {BOLTFFI_TOOL}; pin it and re-lock so install_args match the lock, known keys: {known}",
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
        let root = crate::s2::shell_quote(&active.root);
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
                crate::s2::shell_quote(&member.id),
                crate::s2::shell_quote(&member.root)
            );
        } else {
            // deny/audit/publish resolve their own inputs; skip fetch.
            let _ = writeln!(
                cases,
                "            {}) exit 0 ;;",
                crate::s2::shell_quote(&member.id)
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

/// The `cargo fetch --locked` step of a collapsed provider job: the manifest root
/// arrives through the `cargo_root` input, and the warm-store skip is either a
/// literal decision every member shares or a per-unit input the script reads.
fn render_cargo_source_preparation_from_input(
    output: &mut String,
    gate: Option<&str>,
    skip_when_warm: FeatureCoverage,
) {
    let mut env = format!(
        "\n          CARGO_FETCH_ROOT: {}",
        provider_input::expression(provider_input::CARGO_ROOT)
    );
    let fetch_body = if skip_when_warm.all {
        render_cargo_fetch_body(true)
    } else if skip_when_warm.any {
        let _ = write!(
            env,
            "\n          CARGO_FETCH_SKIP_WHEN_WARM: {}",
            provider_input::expression(provider_input::CARGO_FETCH_SKIP_WHEN_WARM)
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

/// Selection gate for a provider-level cargo prep job: any restricted unit selected.
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
        let quoted = crate::s2::shell_quote(root);
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

/// The seed restore for a collapsed provider job: paths and the three snapshot
/// segments come from the caller's inputs.
fn render_mutable_mount_seed_restore_from_input(
    output: &mut String,
    ir: &WorkflowIr,
    checks_env: &str,
) {
    let (key, restore_keys) = input_snapshot(
        DOCKER_SEED_SNAPSHOT_NAMESPACE,
        provider_input::SEED_COMPAT,
        provider_input::SEED_DEPENDENCY_FILES,
        provider_input::SEED_FRESHNESS_FILES,
    );
    let paths = format!(
        "            {}",
        provider_input::expression(provider_input::CACHE_PATHS)
    );
    render_seed_restore_steps(output, ir, &paths, &key, &restore_keys, checks_env);
}

/// The seed restore for a concrete `(provider, unit)` job such as a release
/// leg: the same steps and the same snapshot grammar as the collapsed
/// provider job's input-driven restore, with the paths and all three
/// segments rendered literally from the unit. The key matches the unit
/// provider job's key for the same pair, so release legs share its seed.
pub(crate) fn render_mutable_mount_seed_restore_for_unit(
    output: &mut String,
    ir: &WorkflowIr,
    provider: ProviderId,
    unit: &Unit,
    checks_env: &str,
) {
    let Some(cache) = unit.cache.as_ref() else {
        return;
    };
    let (key, restore_keys) = unit_snapshot(ir, unit, provider, DOCKER_SEED_SNAPSHOT_NAMESPACE);
    let (paths, _) = rendered_cache_values(cache);
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

/// The seed collection for a collapsed provider job: the save key reads the same
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
            provider_input::SEED_COMPAT,
            provider_input::SEED_DEPENDENCY_FILES,
            provider_input::SEED_FRESHNESS_FILES,
        );
        let paths = format!(
            "            {}",
            provider_input::expression(provider_input::CACHE_PATHS)
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
        let _ = writeln!(
            output,
            "      - name: Save Docker build seed\n        if: {}\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {key}",
            dependency_bundle_cache_save_if(&ir.default_branch),
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
    cache: &crate::s2::CacheSpec,
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
    pub(crate) providers: ProviderSet,
    pub(crate) automatic_providers: ProviderSet,
    // NOTE: no `default_dispatch_providers`. Manual dispatches select the
    // static universe; there is no dispatch-side provider input to default.
    pub(crate) selectors: SelectorMap,
    pub(crate) ci_required: bool,
    pub(crate) repository: String,
    /// The D19 generator pin (`ProjectConfig::workflow_revision`).
    pub(crate) workflow_revision: String,
    /// Declared `[policy]` contexts for ruleset API 403 fallback.
    pub(crate) declared_ruleset_contexts: String,
    pub(crate) rust_needs: RustNeeds,
    pub(crate) concurrency_group: Option<String>,
    pub(crate) serial_stack_groups: bool,
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
    /// The repository's own parsed Rust pin: the file-driven provision leg.
    /// A unit leg whose channel differs provisions explicitly instead.
    pub(crate) rust_pin: Option<RustToolchain>,
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

/// The admission class of a provider caller: which single predicate decides,
/// for the current event, ref, and dispatch input, whether the provider runs
/// the unit.
///
/// Three surfaces need the same decision — the collapsed reusable job's
/// `if:`, the aggregate caller's `if:`, and the required check's expectation
/// for that caller — and each renders it from
/// [`WorkflowIr::provider_admission_expression`] on this key. A fork pull
/// request, a provider-restricted `workflow_dispatch`, and an untrusted event
/// are all just values of that predicate, never special cases of the gate.
/// [`crate::s2::validate_provider_admission_single_source`] checks the rendered
/// tree for drift between the three surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProviderAdmission {
    /// The provider on any event it is statically eligible for.
    Provider(ProviderId),
    /// The provider gated to trusted events (local providers, trusted-only
    /// units).
    ProviderTrusted(ProviderId),
    /// Any local provider in the universe, gated to trusted events. The
    /// `control` prerequisite caller warms every local provider's stores, so
    /// it is admitted when any of them is — keying it to one local provider
    /// would skip warming on dispatches that select only the other.
    AnyLocalTrusted,
}

impl Default for ProviderAdmission {
    /// The hosted provider on any event: the empty facts baseline, mirroring
    /// the schema-1 lane default.
    fn default() -> Self {
        Self::Provider(ProviderId::GithubHosted)
    }
}

impl ProviderAdmission {
    /// The admission class of `unit` on `provider`.
    pub(crate) fn for_unit(provider: ProviderId, unit: &Unit) -> Self {
        if provider.is_local() || unit.trust == crate::s2::provider::TrustReq::TrustedOnly {
            Self::ProviderTrusted(provider)
        } else {
            Self::Provider(provider)
        }
    }

    /// The single provider whose event predicate this class evaluates, if it
    /// has one. The union class matches no single provider.
    pub(crate) fn provider(self) -> Option<ProviderId> {
        match self {
            Self::Provider(provider) | Self::ProviderTrusted(provider) => Some(provider),
            Self::AnyLocalTrusted => None,
        }
    }

    /// Whether this class requires a trusted event.
    pub(crate) fn trusted_only(self) -> bool {
        matches!(self, Self::ProviderTrusted(_) | Self::AnyLocalTrusted)
    }

    /// The required check's environment variable carrying the evaluated
    /// predicate (`true` / `false`) for this class.
    pub(crate) fn env_name(self) -> &'static str {
        match self {
            Self::Provider(provider) => match provider {
                ProviderId::GithubHosted => "PROVIDER_ADMITTED_GITHUB_HOSTED",
                ProviderId::GithubSelfHosted => "PROVIDER_ADMITTED_GITHUB_SELF_HOSTED",
                ProviderId::Velnor => "PROVIDER_ADMITTED_VELNOR",
            },
            Self::ProviderTrusted(provider) => match provider {
                ProviderId::GithubHosted => "PROVIDER_ADMITTED_GITHUB_HOSTED_TRUSTED",
                ProviderId::GithubSelfHosted => "PROVIDER_ADMITTED_GITHUB_SELF_HOSTED_TRUSTED",
                ProviderId::Velnor => "PROVIDER_ADMITTED_VELNOR_TRUSTED",
            },
            Self::AnyLocalTrusted => "PROVIDER_ADMITTED_ANY_LOCAL_TRUSTED",
        }
    }

    /// The dependency-info input value naming this class: what the caller
    /// records beside the unit's dependency closure.
    pub(crate) fn info_id(self) -> &'static str {
        match self {
            Self::AnyLocalTrusted => "any-local-trusted",
            Self::Provider(ProviderId::GithubHosted) => "github-hosted",
            Self::Provider(ProviderId::GithubSelfHosted) => "github-self-hosted",
            Self::Provider(ProviderId::Velnor) => "velnor",
            Self::ProviderTrusted(ProviderId::GithubHosted) => "github-hosted-trust-gated",
            Self::ProviderTrusted(ProviderId::GithubSelfHosted) => "github-self-hosted-trust-gated",
            Self::ProviderTrusted(ProviderId::Velnor) => "velnor-trust-gated",
        }
    }
}

/// One caller the required check validates: the aggregate job id, the
/// (unit, provider) pair it runs, its admission class, and whether it is a
/// prerequisite of other callers (which only changes the diagnostic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequiredCaller {
    pub(crate) job_id: String,
    pub(crate) unit_id: String,
    pub(crate) provider: ProviderId,
    pub(crate) selected_by: Vec<String>,
    pub(crate) admission: ProviderAdmission,
    pub(crate) prerequisite: bool,
}

/// A control-plane job the aggregate required check must both depend on and
/// verify. Keep this list separate from unit callers: a control obligation is
/// selected by workflow construction (for example, `policy` exists on main
/// and nightly but not pull requests), while a unit obligation is selected by
/// the affected plan and provider admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequiredControl {
    job_id: &'static str,
}

fn required_controls(include_policy: bool) -> Vec<RequiredControl> {
    let mut controls = vec![RequiredControl { job_id: "plan" }];
    if include_policy {
        controls.push(RequiredControl { job_id: "policy" });
    }
    controls
}

/// The plan-JSON needle both the aggregate callers and the kind reusables
/// match with `contains(...)`: bare quotes, exactly as the plan output spells
/// the `unit_id` member. One spelling shared by both emitters so they cannot
/// diverge again — GitHub `contains()` is a literal substring test, so an
/// over-escaped needle never matches and every unit job skips.
fn selected_unit_needle(unit_id: &str) -> String {
    format!("'\"unit_id\":\"{unit_id}\"'")
}

/// `contains(...)` over the plan's `units` JSON output for one unit id, as the
/// aggregate callers spell it.
fn aggregate_selected_unit_selector(unit_id: &str) -> String {
    format!(
        "contains(needs.plan.outputs.units, {})",
        selected_unit_needle(unit_id)
    )
}

/// The `workflow_dispatch` inputs: scope and base ref only. There is
/// deliberately no provider input — manual dispatches select the static
/// universe, so no alternate-provider dispatch input can exist.
fn workflow_dispatch_inputs(
    default_scope: &str,
    default_branch: &str,
    extra_inputs: &str,
) -> String {
    format!(
        "  workflow_dispatch:\n    inputs:\n      scope:\n        description: Verification scope\n        required: true\n        default: {default_scope}\n        type: choice\n        options:\n          - affected\n          - full\n      base_sha:\n        description: Git ref or SHA used as the affected-selection base\n        required: false\n        default: refs/heads/{default_branch}\n        type: string\n{extra_inputs}"
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
    ir.concurrency_group.as_deref().map_or_else(
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
) -> (&'static str, &'static str, String, bool) {
    match kind {
        WorkflowKind::PullRequest => (
            "CI / PR",
            "CI / PR",
            format!(
                "on:\n  pull_request:\n{}",
                workflow_dispatch_inputs("affected", default_branch, "",)
            ),
            true,
        ),
        WorkflowKind::Main => (
            "CI / Main",
            "CI / main",
            format!(
                "on:\n  push:\n    branches: [{}]\n{}",
                yaml_scalar(default_branch),
                workflow_dispatch_inputs("full", default_branch, "",)
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

/// One aggregate caller for a (unit, provider) pair (D1, D5).
struct UnitProviderCaller {
    job_id: String,
    unit_id: String,
    name: String,
    provider: ProviderId,
    file: String,
    /// The per-unit `workflow_call` inputs this caller supplies to the kind
    /// reusable: every unit-specific literal the collapsed provider job reads
    /// through `inputs.*` instead of carrying one step block per unit.
    inputs: Vec<(&'static str, String)>,
}

/// The `workflow_call` inputs a kind reusable reads per-unit facts from.
///
/// GitHub loads a called workflow once per caller into one template-memory
/// budget, so the callee must be O(1) in the number of units: every value that
/// differs between two units of a kind travels through one of these inputs and
/// the callee renders its step block exactly once per provider job. The caller and
/// the callee derive both sides from [`ProviderStepFacts`], so they can never name
/// a different set of inputs.
pub(crate) mod provider_input {
    /// Space-separated mise tool ids the provider installs for the unit.
    pub(crate) const MISE_TOOLS: &str = "mise_tools";
    /// `true` when the hosted provider needs the mise task runner without tools.
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
    /// Comma-separated host-warm layers the Velnor provider reports.
    pub(crate) const HOST_WARM_LAYERS: &str = "host_warm_layers";
    /// `true` when the Velnor provider must provision the pinned policy runtime
    /// for the unit's generator `--check`; hosted lanes carry it in the
    /// Planning runtime artifact unconditionally.
    pub(crate) const POLICY_RUNTIME: &str = "policy_runtime";
    /// The unit's typed platform (`linux-x64`, `linux-arm64`, `macos-arm64`).
    pub(crate) const UNIT_PLATFORM: &str = "unit_platform";
    /// The unit's typed trust requirement (`untrusted-ok`, `trusted-only`).
    pub(crate) const UNIT_TRUST: &str = "unit_trust";
    /// `true` when the hosted provider job must publish its own debug product of
    /// the generator crate as the Stage-1 candidate artifact the owner
    /// policy run consumes. Only the generator crate's owning Rust unit sets
    /// it, on the hosted provider, in the owner repository, on pull requests.
    pub(crate) const CANDIDATE_PUBLISH: &str = "candidate_publish";
    /// `true` when the unit needs the Apple executor: a kind whose members
    /// split across the default and Apple executors renders one collapsed
    /// job per executor, and each job admits only its own callers.
    pub(crate) const APPLE_EXECUTOR: &str = "apple_executor";
    /// Comma-separated `depends_on` ids the nested job records; empty when
    /// the unit depends on nothing.
    pub(crate) const UNIT_DEPENDENCIES: &str = "unit_dependencies";
    /// The provider admission class id the nested job records.
    pub(crate) const UNIT_ADMISSION: &str = "unit_admission";
    /// Comma-separated prepared-tool need records (`tool:digest:producers`)
    /// the provider restores for the unit; empty when it needs none.
    pub(crate) const PREPARED_TOOLS: &str = "prepared_tools";
    /// Comma-separated transport records (`producer/product`) the unit
    /// publishes as artifacts; empty when it produces nothing.
    pub(crate) const PRODUCT_PROVIDES: &str = "product_provides";
    /// Comma-separated `{record}:{verdict}` pairs, one per transportable
    /// prerequisite edge; the caller evaluates each producer job's result
    /// inline because the callee cannot read the caller's `needs`.
    pub(crate) const PRODUCT_TRANSPORT_READY: &str = "product_transport_ready";
    /// Comma-separated validation phases the unit verifies through
    /// (`fmt,clippy,test,doctest`); empty when the unit keeps the single
    /// legacy checks step.
    pub(crate) const VALIDATION_PHASES: &str = "validation_phases";
    /// `true` when the unit's checkout must clone full history instead of
    /// the default depth-1 shallow clone. Diff-aware gates (merge-base
    /// against the base SHA) need ancestry the shallow checkout lacks.
    pub(crate) const FULL_HISTORY: &str = "full_history";
    /// The Rust channel the selected unit verifies under. Declared only when
    /// the kind spans more than one channel; every caller of such a kind
    /// passes its unit's channel so the provision legs gate on it.
    pub(crate) const TOOLCHAIN: &str = "toolchain";

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
        UNIT_PLATFORM,
        UNIT_TRUST,
        UNIT_DEPENDENCIES,
        UNIT_ADMISSION,
        PREPARED_TOOLS,
        PRODUCT_PROVIDES,
        PRODUCT_TRANSPORT_READY,
        VALIDATION_PHASES,
        FULL_HISTORY,
        TOOLCHAIN,
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
                | FULL_HISTORY
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

/// The distinct Rust channels a unit set verifies under. More than one means
/// the kind file carries the `toolchain` input and provisions one leg per
/// channel; one or zero keeps today's single file-driven provision.
fn rust_toolchain_channels<'a>(units: impl Iterator<Item = &'a Unit>) -> BTreeSet<&'a str> {
    units
        .filter(|unit| unit.kind == UnitKind::Rust)
        .filter_map(|unit| unit.toolchain.as_ref().map(|pin| pin.channel.as_str()))
        .collect()
}

/// Group a collapsed provider job's members into provision legs, in channel
/// order: one leg per distinct channel, each leg carrying the toolchain its
/// provision block installs. A channel that matches the repository pin
/// renders the pin's own file facts — a declared bare channel equal to the
/// pin simply joins the pin leg — while any other channel requires every
/// member on it to agree exactly, or generation refuses the contradiction.
///
/// The error is the disagreement fragment the caller wraps: members that mix
/// pinned and unpinned toolchains, or that disagree within one channel.
fn toolchain_leg_groups(
    members: &[&Unit],
    pin: Option<&RustToolchain>,
) -> Result<Vec<RustToolchain>, String> {
    let mut by_channel: BTreeMap<&str, Vec<&RustToolchain>> = BTreeMap::new();
    let mut saw_unpinned = false;
    for unit in members {
        match unit.toolchain.as_ref() {
            None => saw_unpinned = true,
            Some(toolchain) => by_channel
                .entry(toolchain.channel.as_str())
                .or_default()
                .push(toolchain),
        }
    }
    if saw_unpinned && !by_channel.is_empty() {
        return Err("the Rust toolchain pin".to_owned());
    }
    let mut legs = Vec::new();
    for (channel, toolchains) in &by_channel {
        if let Some(pin) = pin
            && pin.channel == *channel
        {
            legs.push(pin.clone());
        } else if toolchains
            .iter()
            .all(|candidate| *candidate == toolchains[0])
        {
            legs.push(toolchains[0].clone());
        } else {
            return Err(format!("the Rust toolchain pin for channel `{channel}`"));
        }
    }
    Ok(legs)
}

/// The restore-step id of an explicit provision leg: the pinned leg keeps
/// `rustup-toolchain`, and every other leg suffixes it with its sanitized
/// channel so the save gate reads its own step's `cache-hit`. Step ids admit
/// only letters, digits, `_`, and `-`; anything else folds to `-`, with a
/// numeric suffix when two channels sanitize alike.
fn explicit_toolchain_step_id(channel: &str, taken: &mut BTreeSet<String>) -> String {
    let sanitized: String = channel
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let mut candidate = format!("rustup-toolchain-{sanitized}");
    let mut suffix = 2;
    while taken.contains(&candidate) {
        candidate = format!("rustup-toolchain-{sanitized}-{suffix}");
        suffix += 1;
    }
    taken.insert(candidate.clone());
    candidate
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

/// The per-unit facts one collapsed provider job reads through `workflow_call`
/// inputs. Built once per (unit, provider) by [`WorkflowIr::unit_provider_facts`]; the
/// caller turns it into `with:` values and the callee unions it over the
/// kind's members to decide which steps exist and which need a presence gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent `type: boolean` workflow_call input"
)]
pub(crate) struct ProviderStepFacts {
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
    pub(crate) platform: String,
    pub(crate) trust: String,
    pub(crate) unit_dependencies: Vec<String>,
    pub(crate) unit_admission: ProviderAdmission,
    /// Prepared-tool need records the provider restores for the unit.
    pub(crate) prepared_tools: Vec<String>,
    /// Transport records (`producer/product`) the unit publishes; hosted only.
    pub(crate) product_provides: Vec<String>,
    /// Caller-evaluated readiness verdicts, one per transportable edge.
    pub(crate) product_transport_ready: Option<String>,
    /// The validation phases the unit verifies through, in step order;
    /// empty when the unit keeps the single legacy checks step.
    pub(crate) validation_phases: Vec<ValidationPhase>,
    /// Whether the unit's checkout clones full history. Provider-independent:
    /// the same value travels per (unit, provider) invocation.
    pub(crate) full_history: bool,
    /// The Rust channel the unit verifies under. Set only when the unit's
    /// kind spans more than one channel; single-channel kinds provision the
    /// pin without any input.
    pub(crate) toolchain: Option<String>,
}

impl ProviderStepFacts {
    /// The `with:` values a caller passes: one entry per non-empty fact.
    /// Tailed groups (transport, verification selectors) live in their own
    /// push helpers so this list stays under the line budget.
    pub(crate) fn input_values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        if !self.mise_tools.is_empty() {
            values.push((provider_input::MISE_TOOLS, self.mise_tools.join(" ")));
        }
        if self.mise_runner {
            values.push((provider_input::MISE_RUNNER, "true".to_owned()));
        }
        if self.mbx_enabled {
            values.push((provider_input::MBX_ENABLED, "true".to_owned()));
        }
        if let Some(mbx) = &self.mbx {
            values.push((provider_input::MBX_COMPAT, mbx.compatibility.clone()));
            values.push((
                provider_input::MBX_DEPENDENCY_FILES,
                mbx.dependency_files.join("\n"),
            ));
            values.push((
                provider_input::MBX_FRESHNESS_FILES,
                mbx.state_files.join("\n"),
            ));
        }
        if !self.cargo_bin_tools.is_empty() {
            values.push((
                provider_input::CARGO_BIN_TOOLS,
                self.cargo_bin_tools.join(","),
            ));
        }
        if let Some(version) = &self.tool_version {
            values.push((provider_input::TOOL_VERSION, version.clone()));
        }
        if let Some(path) = &self.node_cache_dependency_path {
            values.push((provider_input::NODE_CACHE_DEPENDENCY_PATH, path.clone()));
        }
        if let Some((paths, key_files)) = &self.bundle {
            values.push((provider_input::CACHE_PATHS, paths.join("\n")));
            values.push((provider_input::CACHE_KEY_FILES, key_files.join("\n")));
        }
        if let Some((paths, seed)) = &self.seed {
            values.push((provider_input::CACHE_PATHS, paths.join("\n")));
            values.push((provider_input::SEED_COMPAT, seed.compatibility.clone()));
            values.push((
                provider_input::SEED_DEPENDENCY_FILES,
                seed.dependency_files.join("\n"),
            ));
            values.push((
                provider_input::SEED_FRESHNESS_FILES,
                seed.state_files.join("\n"),
            ));
        }
        if let Some(root) = &self.cargo_root {
            values.push((provider_input::CARGO_ROOT, root.clone()));
        }
        if self.cargo_fetch_skip_when_warm {
            values.push((
                provider_input::CARGO_FETCH_SKIP_WHEN_WARM,
                "true".to_owned(),
            ));
        }
        if self.cargo_net_offline {
            values.push((provider_input::CARGO_NET_OFFLINE, "true".to_owned()));
        }
        if !self.host_warm_layers.is_empty() {
            values.push((
                provider_input::HOST_WARM_LAYERS,
                self.host_warm_layers.join(","),
            ));
        }
        if self.policy_runtime {
            values.push((provider_input::POLICY_RUNTIME, "true".to_owned()));
        }
        if self.candidate_publish {
            values.push((provider_input::CANDIDATE_PUBLISH, "true".to_owned()));
        }
        if self.apple_executor {
            values.push((provider_input::APPLE_EXECUTOR, "true".to_owned()));
        }
        // Cache keys always carry the platform/trust segments, so every
        // caller passes them unconditionally.
        values.push((provider_input::UNIT_PLATFORM, self.platform.clone()));
        values.push((provider_input::UNIT_TRUST, self.trust.clone()));
        if !self.unit_dependencies.is_empty() {
            values.push((
                provider_input::UNIT_DEPENDENCIES,
                self.unit_dependencies.join(","),
            ));
        }
        values.push((
            provider_input::UNIT_ADMISSION,
            self.unit_admission.info_id().to_owned(),
        ));
        if !self.prepared_tools.is_empty() {
            values.push((
                provider_input::PREPARED_TOOLS,
                self.prepared_tools.join(","),
            ));
        }
        self.push_transport_values(&mut values);
        self.push_verification_values(&mut values);
        values
    }

    /// The tail `with:` values, in declaration order: the unit's validation
    /// phases, its full-history checkout flag, plus its toolchain channel
    /// when the kind spans more than one.
    fn push_verification_values(&self, values: &mut Vec<(&'static str, String)>) {
        if !self.validation_phases.is_empty() {
            values.push((
                provider_input::VALIDATION_PHASES,
                ValidationPhase::id_list(&self.validation_phases).join(","),
            ));
        }
        if self.full_history {
            values.push((provider_input::FULL_HISTORY, "true".to_owned()));
        }
        if let Some(channel) = &self.toolchain {
            values.push((provider_input::TOOLCHAIN, channel.clone()));
        }
    }

    /// The transport `with:` values: the records this unit publishes, plus
    /// the caller-evaluated readiness verdicts when it consumes any.
    fn push_transport_values(&self, values: &mut Vec<(&'static str, String)>) {
        if !self.product_provides.is_empty() {
            values.push((
                provider_input::PRODUCT_PROVIDES,
                self.product_provides.join(","),
            ));
        }
        if let Some(ready) = &self.product_transport_ready {
            values.push((provider_input::PRODUCT_TRANSPORT_READY, ready.clone()));
        }
    }
}

/// How many members of a collapsed provider job carry one feature: none (the
/// steps are not rendered), all (rendered without a gate), or some (rendered
/// behind an input presence gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FeatureCoverage {
    any: bool,
    all: bool,
}

impl FeatureCoverage {
    fn over(facts: &[ProviderStepFacts], predicate: impl Fn(&ProviderStepFacts) -> bool) -> Self {
        let count = facts.iter().filter(|facts| predicate(facts)).count();
        Self {
            any: count > 0,
            all: count == facts.len() && !facts.is_empty(),
        }
    }

    /// The `if:` gate the feature's steps need, or `None` when every member
    /// carries it.
    fn gate(self, input: &str) -> Option<String> {
        (self.any && !self.all).then(|| provider_input::present_gate(input))
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
        } else if provider_input::is_flag(name) {
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
    format!(
        "contains(inputs.selected_units, {})",
        selected_unit_needle(unit_id)
    )
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
            github_expression(&workflow.provider_admission_expression(admission))
        );
    }
    output
}

/// The canonical execution obligations the aggregate check may validate. The
/// plan is untrusted workflow output, so the gate must reject a unit/provider
/// pair that the renderer did not emit a caller for instead of silently
/// ignoring it. Keep this contract derived from the same callers used for the
/// needs list and the per-caller verdicts.
fn required_execution_contract(callers: &[RequiredCaller]) -> String {
    let obligations = callers
        .iter()
        .filter(|caller| !caller.prerequisite)
        .map(|caller| {
            serde_json::json!({
                "unit_id": caller.unit_id,
                "provider": caller.provider.as_str(),
                "job_id": caller.job_id,
            })
        })
        .collect::<Vec<_>>();
    match serde_json::to_string(&obligations) {
        Ok(contract) => contract,
        Err(_) => "null".to_owned(),
    }
}

/// The per-caller verdict block of the required check: strict expected-set
/// comparison against the frozen plan.
///
/// Each (unit, provider) pair the plan declared expected must be exactly
/// `success`: missing, skipped, cancelled, timed-out, or failed results fail
/// the check, and so does a result that ran outside the expected set. The
/// expectation is read from the plan's `units` JSON output, fixed before
/// execution — never re-derived. A shell conditional must never embed a
/// rendered boolean literal (`[[ ... && false ]]` is a non-empty string
/// test), so the admission expectation is read from the class variable, never
/// inlined.
fn render_required_caller_verdicts(output: &mut String, callers: &[RequiredCaller]) {
    for caller in callers {
        let job_id = &caller.job_id;
        let noun = if caller.prerequisite {
            "CI prerequisite"
        } else {
            "CI job"
        };
        // A prerequisite is expected when any unit that selects it is
        // expected on any local provider, whose stores it warms.
        let expected_condition = if caller.prerequisite {
            caller
                .selected_by
                .iter()
                .map(|unit_id| format!("plan_expects_local \"{unit_id}\""))
                .collect::<Vec<_>>()
                .join(" || ")
        } else {
            format!(
                "plan_expects \"{}\" \"{}\"",
                caller.unit_id,
                caller.provider.as_str()
            )
        };
        let admitted = caller.admission.env_name();
        let _ = writeln!(
            output,
            "          if {expected_condition}; then\n            result=\"$(result_for_job {job_id})\"\n            if [[ \"${admitted}\" == true ]]; then\n              case \"$result\" in\n                success) ;;\n                skipped) echo \"expected {noun} {job_id} was skipped: a skipped expected result cannot pass\" >&2; exit 1 ;;\n                cancelled) echo \"expected {noun} {job_id} was cancelled: a cancelled expected result cannot pass\" >&2; exit 1 ;;\n                *) echo \"expected {noun} {job_id} did not pass: $result\" >&2; exit 1 ;;\n              esac\n            else\n              case \"$result\" in\n                skipped) ;;\n                *) echo \"selected {noun} {job_id} ran outside its provider admission ({admitted}=${admitted}): $result\" >&2; exit 1 ;;\n              esac\n            fi\n          else\n            result=\"$(result_for_job {job_id})\"\n            case \"$result\" in\n              skipped) ;;\n              success) echo \"unexpected {noun} {job_id} succeeded outside the expected set: the plan did not declare it\" >&2; exit 1 ;;\n              *) echo \"unexpected {noun} {job_id} ran outside the expected set: $result\" >&2; exit 1 ;;\n            esac\n          fi"
        );
    }
}

/// One collapsed provider job awaiting render: the provider, its
/// trust/executor partition members, the owned identity strings, and the
/// executor partition (`None` when the provider does not split).
struct CollapsedProviderJob<'a> {
    provider: ProviderId,
    members: Vec<&'a Unit>,
    job_id: String,
    display: String,
    apple: Option<bool>,
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
        Self {
            default_branch: config.default_branch.clone(),
            providers: config.providers.clone(),
            automatic_providers: config.automatic_providers.clone(),
            selectors: config.selectors.clone(),
            ci_required: config.ci_required,
            repository: config.repository.clone(),
            workflow_revision: config.workflow_revision.clone(),
            declared_ruleset_contexts: crate::s2::declared_ruleset_contexts_literal(config),
            rust_needs: config.rust_needs,
            concurrency_group: config.concurrency_group.clone(),
            serial_stack_groups: config.serial_stack_groups,
            tools,
            mise_present,
            mr_boxington,
            units: config.units.clone(),
            pins: Pins::resolved(),
            mise_lock_keys: config.mise_lock_keys.clone(),
            rust_pin: config.rust_pin.clone(),
        }
    }

    fn ci_report_action_uses(&self) -> String {
        crate::s2::ci_report_action_uses(&self.repository, &self.workflow_revision)
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
        let (workflow_name, run_name, triggers, cancel_in_progress) =
            aggregate_triggers(kind, &self.default_branch);
        let concurrency = aggregate_concurrency_block(self, kind, cancel_in_progress);
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\n{concurrency}permissions:\n  actions: read\n  contents: read\n\njobs:"
        );
        // Planning and policy are control plane.
        let mut plan = String::new();
        self.render_plan(&mut plan);
        output.push_str(&plan);
        if kind != WorkflowKind::PullRequest {
            self.render_policy(&mut output);
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
        let (workflow_name, run_name, triggers, _) =
            aggregate_triggers(WorkflowKind::Nightly, &self.default_branch);
        let concurrency = aggregate_concurrency_block(self, WorkflowKind::Nightly, false);
        let default_branch = yaml_scalar(&self.default_branch);
        let runner = self.runs_on_yaml(self.control_plane_provider());
        let dispatch_if = self.control_plane_gated_condition(
            "github.event_name != 'workflow_dispatch' || !inputs.simulate_failure",
        );
        let simulate_if = self.control_plane_gated_condition(
            "github.event_name == 'workflow_dispatch' && inputs.simulate_failure",
        );
        let _ = writeln!(
            output,
            "name: {workflow_name}\nrun-name: {run_name} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\n{triggers}\n\n{concurrency}permissions:\n  actions: read\n  contents: read\n\njobs:"
        );
        let _ = writeln!(
            output,
            "  dispatch-ci-main:\n    name: {}\n    if: ${{{{ {dispatch_if} }}}}\n    runs-on: {runner}\n    timeout-minutes: 5\n    permissions:\n      actions: write\n      contents: read\n    steps:\n      - name: Dispatch ci-main on default branch\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          GITHUB_REPOSITORY: ${{{{ github.repository }}}}\n          DEFAULT_BRANCH: {default_branch}\n          DISPATCH_SCOPE: ${{{{ github.event.inputs.scope || 'full' }}}}\n          DISPATCH_BASE_SHA: ${{{{ github.event.inputs.base_sha || format('refs/heads/{{0}}', github.event.repository.default_branch) }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          gh workflow run ci-main.yml \\\n            -R \"$GITHUB_REPOSITORY\" \\\n            --ref \"$DEFAULT_BRANCH\" \\\n            -f scope=\"$DISPATCH_SCOPE\" \\\n            -f base_sha=\"$DISPATCH_BASE_SHA\"",
            crate::s2::control_job_name("Dispatch ci-main"),
        );
        let _ = writeln!(
            output,
            "  nightly-red-to-signal:\n    name: {}\n    if: ${{{{ {simulate_if} }}}}\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Simulate nightly failure\n        shell: bash\n        run: |\n          set -euo pipefail\n          echo \"nightly red-to-signal simulation requested\" >&2\n          exit 1",
            crate::s2::control_job_name("Nightly red-to-signal"),
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

    /// The (unit, provider) callers of one unit. The `with:` values come from the
    /// same per-unit facts the collapsed callee gates on, derived from the
    /// unit's resolved contract.
    fn unit_provider_callers(
        &self,
        unit: &Unit,
        file: &str,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Vec<UnitProviderCaller> {
        let contract = self.contract_for(unit, contracts);
        contract
            .providers
            .iter()
            .filter(|job| provider_supports_unit(job.provider, unit))
            .map(|job| UnitProviderCaller {
                job_id: unit_job_id(job.provider, &unit.id),
                unit_id: unit.id.clone(),
                name: unit_job_display_name(unit, job.provider),
                provider: job.provider,
                file: file.to_owned(),
                inputs: self
                    .unit_provider_facts(unit, &contract, job.provider)
                    .input_values(),
            })
            .collect()
    }

    fn kind_file_needs_prepare_cargo(&self, file: &str) -> bool {
        self.providers.iter().any(|provider| provider.is_local())
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
    /// with any local provider, whose stores it warms — every local
    /// provider's, through its own callee job.
    fn prepare_cargo_required_caller(&self, file: &str) -> RequiredCaller {
        let provider = self
            .providers
            .iter()
            .copied()
            .find(|provider| provider.is_local())
            .unwrap_or_else(|| self.control_plane_provider());
        let selected_by = self.prepare_cargo_selected_by(file);
        RequiredCaller {
            job_id: prepare_cargo_caller_job_id().to_owned(),
            unit_id: selected_by.first().cloned().unwrap_or_default(),
            provider,
            selected_by,
            admission: ProviderAdmission::AnyLocalTrusted,
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
            self.provider_admission_expression(caller.admission)
        ));
        let _ = writeln!(
            output,
            "  {}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{file}\n    with:\n      unit: {}\n      provider: control\n      selected_units: ${{{{ needs.plan.outputs.units }}}}\n      selected_unit_ids: ${{{{ needs.plan.outputs.unit_ids }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      full_units: ${{{{ needs.plan.outputs.full_units }}}}\n      plan_digest: ${{{{ needs.plan.outputs.plan_digest }}}}\n      base_sha: ${{{{ needs.plan.outputs.base_sha }}}}\n      head_sha: ${{{{ needs.plan.outputs.head_sha }}}}",
            caller.job_id,
            crate::s2::control_job_name("Prepare Cargo"),
            conditions.join(" && "),
            needs.join(", "),
            yaml_scalar(sample_unit),
        );
    }

    fn render_unit_provider_caller(
        &self,
        output: &mut String,
        unit: &Unit,
        caller: &UnitProviderCaller,
        include_policy: bool,
        extra_needs: &[String],
        cancel_in_progress: bool,
    ) {
        let provider = caller.provider;
        let mut needs = vec!["plan".to_owned()];
        if include_policy {
            needs.push("policy".to_owned());
        }
        append_unique_needs(&mut needs, extra_needs.iter().cloned());
        if provider.is_local() && self.kind_file_needs_prepare_cargo(&caller.file) {
            append_unique_needs(&mut needs, [prepare_cargo_caller_job_id().to_owned()]);
        }
        append_unique_needs(
            &mut needs,
            rust_dependency_needs(provider, unit, self.rust_needs, &self.units),
        );
        append_unique_needs(
            &mut needs,
            product_dependency_needs(provider, unit, &self.units),
        );
        let mut conditions = vec![
            aggregate_job_guard(cancel_in_progress).to_owned(),
            "needs.plan.result == 'success'".to_owned(),
        ];
        if include_policy {
            conditions.push("needs.policy.result == 'success'".to_owned());
        }
        if provider.is_local() && self.kind_file_needs_prepare_cargo(&caller.file) {
            conditions.push(
                "(needs.prepare-cargo.result == 'success' || needs.prepare-cargo.result == 'skipped')"
                    .to_owned(),
            );
        }
        for dependency in rust_dependency_needs(provider, unit, self.rust_needs, &self.units) {
            conditions.push(format!(
                "(needs.{dependency}.result == 'success' || needs.{dependency}.result == 'skipped')"
            ));
        }
        // A skipped or failed producer means no artifact: the consumer's
        // guarded rebuild covers it, so the caller still runs.
        for dependency in product_dependency_needs(provider, unit, &self.units) {
            conditions.push(format!(
                "(needs.{dependency}.result == 'success' || needs.{dependency}.result == 'skipped')"
            ));
        }
        conditions.push(aggregate_selected_unit_selector(&caller.unit_id));
        // The caller skips exactly when the callee's provider job would: same
        // predicate, same class, so the aggregate never invokes a reusable
        // whose every job is gated off.
        conditions.push(format!(
            "({})",
            self.provider_admission_expression(ProviderAdmission::for_unit(provider, unit))
        ));
        let _ = writeln!(
            output,
            "  {}:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    uses: ./.github/workflows/{}\n    with:\n      unit: {}\n      provider: {}\n      selected_units: ${{{{ needs.plan.outputs.units }}}}\n      selected_unit_ids: ${{{{ needs.plan.outputs.unit_ids }}}}\n      scope: ${{{{ needs.plan.outputs.scope }}}}\n      full_units: ${{{{ needs.plan.outputs.full_units }}}}\n      plan_digest: ${{{{ needs.plan.outputs.plan_digest }}}}\n      base_sha: ${{{{ needs.plan.outputs.base_sha }}}}\n      head_sha: ${{{{ needs.plan.outputs.head_sha }}}}{}",
            caller.job_id,
            yaml_scalar(&caller.name),
            conditions.join(" && "),
            needs.join(", "),
            caller.file,
            yaml_scalar(&caller.unit_id),
            caller.provider.as_str(),
            render_caller_inputs(&caller.inputs),
        );
    }

    /// One reusable-workflow caller per (unit, provider). The kind reusable holds
    /// one job keyed on `inputs.unit` + `inputs.provider` (D5).
    pub(crate) fn render_node_callers(
        &self,
        nodes: &[GraphNode],
        output: &mut String,
        include_policy: bool,
        cancel_in_progress: bool,
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) {
        let mut prepare_cargo_files = BTreeSet::new();
        let mut previous_local_caller: Option<String> = None;
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
            for caller in self.unit_provider_callers(unit, file, contracts) {
                let extra_needs = if self.serial_stack_groups && caller.provider.is_local() {
                    previous_local_caller.iter().cloned().collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                self.render_unit_provider_caller(
                    output,
                    unit,
                    &caller,
                    include_policy,
                    &extra_needs,
                    cancel_in_progress,
                );
                if caller.provider.is_local() {
                    previous_local_caller = Some(caller.job_id.clone());
                }
            }
        }
    }

    /// The callers the aggregate required check validates, in caller render
    /// order: the `prepare-cargo` prerequisite of each kind file that needs
    /// one, then the `(unit, provider)` callers of every contributed unit node.
    /// The same `contracts` that decide which callers `render_node_callers`
    /// renders decide which ones the check needs, so a unit declared for one
    /// provider never leaves the check depending on a job that does not exist.
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
            for caller in self.unit_provider_callers(unit, file, contracts) {
                callers.push(RequiredCaller {
                    job_id: caller.job_id,
                    unit_id: caller.unit_id.clone(),
                    provider: caller.provider,
                    selected_by: vec![caller.unit_id],
                    admission: ProviderAdmission::for_unit(caller.provider, unit),
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
        let controls = required_controls(include_policy);
        let callers = self.required_callers(nodes, contracts);
        let mut needs = controls
            .iter()
            .map(|control| control.job_id.to_owned())
            .collect::<Vec<_>>();
        needs.extend(callers.iter().map(|caller| caller.job_id.clone()));
        let display_name = match check_name {
            REQUIRED_CHECK => yaml_scalar(REQUIRED_CHECK),
            "nightly-required" => crate::s2::control_job_name("Nightly aggregate"),
            other => yaml_scalar(other),
        };
        let needs_json = github_expression("toJSON(needs)");
        let selected_units = github_expression("needs.plan.outputs.units");
        let plan_digest = github_expression("needs.plan.outputs.plan_digest");
        let excluded = github_expression("needs.plan.outputs.excluded");
        let expected_callers = required_execution_contract(&callers);
        let _ = write!(
            output,
            "  {check_name}:\n    name: {display_name}\n    if: ${{{{ {} }}}}\n    needs: [{}]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n",
            self.control_plane_gated_condition(aggregate_job_guard(cancel_in_progress)),
            needs.join(", "),
            self.runs_on_yaml(self.control_plane_provider()),
        );
        // The aggregate scores first, the shell verdict re-confirms after:
        // conjunction, so either side failing fails the check. A hosted
        // control plane downloads the verified plan-artifact runtime the
        // plan publishes for this download; a local control plane scores
        // with the ambient fleet runtime, like every other local job.
        let runtime_steps =
            workflow_runtime_download(self.control_plane_provider(), &self.workflow_revision);
        output.push_str(&render_aggregate_score_steps(
            &runtime_steps,
            self.pins.download_artifact,
        ));
        let _ = write!(
            output,
            "      - name: Validate generated stack results\n        env:\n          NEEDS_JSON: {needs_json}\n          SELECTED_UNITS: {selected_units}\n          PLAN_DIGEST: {plan_digest}\n          EXCLUDED: {excluded}\n          EXPECTED_CALLERS: {}\n{}",
            yaml_scalar(&expected_callers),
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
            "          if [[ -z \"$PLAN_DIGEST\" ]]; then\n            echo \"plan did not freeze a plan digest: the expected set has no identity\" >&2\n            exit 1\n          fi\n          echo \"verdict binds plan digest $PLAN_DIGEST\"\n          result_for_job() {\n            jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"\n          }\n          plan_expects() {\n            [[ \"$(jq -r --arg unit \"$1\" --arg provider \"$2\" '[.[] | select(.unit_id == $unit) | .providers[] | select(. == $provider)] | length' <<<\"$SELECTED_UNITS\")\" -gt 0 ]]\n          }\n",
        );
        output.push_str(
            "          if ! jq -e --argjson expected \"$EXPECTED_CALLERS\" '($expected | type == \"array\") and all($expected[]; type == \"object\" and (.unit_id | type) == \"string\" and (.provider | type) == \"string\" and (.job_id | type) == \"string\")' -n; then\n            echo \"generated required-caller contract is malformed\" >&2\n            exit 1\n          fi\n          if ! jq -e --argjson expected \"$EXPECTED_CALLERS\" 'type == \"array\" and (map(.unit_id) | unique | length) == length and all(.[]; . as $entry | ($entry | type) == \"object\" and ($entry.unit_id | type) == \"string\" and ($entry.unit_id | length) > 0 and ($entry.providers | type) == \"array\" and ($entry.providers | length) > 0 and (($entry.providers | map(type == \"string\" and length > 0) | all)) and (($entry.providers | unique | length) == ($entry.providers | length)) and all($entry.providers[]; . as $provider | any($expected[]; .unit_id == $entry.unit_id and .provider == $provider)))' <<<\"$SELECTED_UNITS\" >/dev/null; then\n            echo \"plan selected-unit output contains an unknown, duplicate, empty, or unmapped obligation\" >&2\n            exit 1\n          fi\n          if ! jq -e 'type == \"object\"' <<<\"$NEEDS_JSON\" >/dev/null; then\n            echo \"workflow needs output is malformed\" >&2\n            exit 1\n          fi\n",
        );
        // The prerequisite trigger: the unit is expected on any local
        // provider, whose stores the prerequisite warms. The local set is
        // generated from the deployed providers (canonical order) so the
        // verdict names exactly the expected set and never leaks a foreign
        // provider name onto a surface that did not deploy it. Rendered
        // only when a prerequisite verdict uses it.
        if callers.iter().any(|caller| caller.prerequisite) {
            let local_alternation = ProviderId::ALL
                .into_iter()
                .filter(|provider| provider.is_local() && self.providers.contains(provider))
                .map(|provider| format!(". == \"{}\"", provider.as_str()))
                .collect::<Vec<_>>()
                .join(" or ");
            let _ = writeln!(
                output,
                "          plan_expects_local() {{\n            [[ \"$(jq -r --arg unit \"$1\" '[.[] | select(.unit_id == $unit) | .providers[] | select({local_alternation})] | length' <<<\"$SELECTED_UNITS\")\" -gt 0 ]]\n          }}",
            );
        }
        for control in &controls {
            let job = control.job_id;
            let _ = writeln!(
                output,
                "          result=\"$(result_for_job {job})\"\n          if [[ \"$result\" != success ]]; then\n            echo \"required CI prerequisite {job} did not pass: $result\" >&2\n            exit 1\n          fi"
            );
        }
        render_required_caller_verdicts(output, &callers);
        if check_name == REQUIRED_CHECK {
            let _ = writeln!(
                output,
                "  required:\n    name: {}\n    if: ${{{{ {} }}}}\n    needs: [ci-required]\n    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Mirror CI / Required\n        if: ${{{{ needs.ci-required.result != 'success' }}}}\n        run: exit 1",
                crate::s2::control_job_name("Required"),
                self.control_plane_gated_condition(aggregate_job_guard(cancel_in_progress)),
                self.runs_on_yaml(self.control_plane_provider())
            );
        }
    }

    pub(crate) fn render_nightly_alert(
        &self,
        output: &mut String,
        needs_job: &str,
        if_override: Option<&str>,
    ) {
        let if_condition = if_override.map_or_else(|| "always()".to_owned(), str::to_owned);
        let if_condition = self.control_plane_gated_condition(&if_condition);
        let _ = writeln!(
            output,
            "  nightly-alert:\n    name: {}\n    if: ${{{{ {if_condition} }}}}\n    needs: [{needs_job}]\n    runs-on: {}\n    permissions:\n      contents: read\n      issues: write\n    steps:\n      - name: Open or update nightly failure signal\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          NIGHTLY_RESULT: ${{{{ needs.{needs_job}.result }}}}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [[ \"$NIGHTLY_RESULT\" == success ]]; then\n            exit 0\n          fi\n          echo \"::error::{needs_job} failed: $NIGHTLY_RESULT\"\n          existing=\"$(gh api \"repos/$GITHUB_REPOSITORY/issues?state=open\" --jq '.[] | select(.title == \"Nightly CI red\") | .number' | sed -n '1p')\"\n          body=\"{needs_job} result: $NIGHTLY_RESULT\nRun: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID\"\n          if [[ -n \"$existing\" ]]; then\n            gh api --method PATCH \"repos/$GITHUB_REPOSITORY/issues/$existing\" -f body=\"$body\" >/dev/null\n          else\n            gh api --method POST \"repos/$GITHUB_REPOSITORY/issues\" -f title='Nightly CI red' -f body=\"$body\" >/dev/null\n          fi",
            crate::s2::control_job_name("Nightly red-to-signal"),
            self.runs_on_yaml(self.control_plane_provider())
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

    /// The default unit contract: every provider of the universe, the default
    /// timeout, the detected cache backend, and the unit's own declared cache
    /// contract. A unit that declares a Docker mutable mount seed gets the
    /// seed lifecycle rendered for it exactly as a declared pipeline would —
    /// the default contract defaults the provider surface, never the cache
    /// transport a unit declared.
    pub(crate) fn default_unit_contract(&self, unit: &Unit, cache_save: bool) -> UnitContract {
        UnitContract {
            providers: Self::default_provider_jobs(&self.providers, cache_save),
            timeout_minutes: DEFAULT_UNIT_TIMEOUT_MINUTES,
            cache: CacheBackend::Detected,
            cache_save,
            mutable_mount_seed: unit
                .cache
                .as_ref()
                .is_some_and(|cache| cache.mutable_mount_seed),
        }
    }

    /// The provider jobs a nested unit workflow emits over the universe: every
    /// provider in canonical order. Only hosted saves caches, and only local
    /// providers carry the trusted-event gate.
    pub(crate) fn default_provider_jobs(
        providers: &ProviderSet,
        cache_save: bool,
    ) -> Vec<ProviderJob> {
        providers
            .iter()
            .map(|provider| ProviderJob {
                provider: *provider,
                cache_save: cache_save && *provider == ProviderId::GithubHosted,
                trusted: provider.is_local(),
            })
            .collect()
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
            "name: {}\non:\n  workflow_call:\n    inputs:\n      unit:\n        required: true\n        type: string\n      selected_units:\n        required: true\n        type: string\n      selected_unit_ids:\n        required: true\n        type: string\n      scope:\n        required: true\n        type: string\n      full_units:\n        required: true\n        type: string\n      base_sha:\n        required: true\n        type: string\n      head_sha:\n        required: true\n        type: string\n      plan_digest:\n        required: true\n        type: string\n      provider:\n        required: true\n        type: string",
            yaml_scalar(unit_group(kind))
        );
        for name in provider_input::ALL {
            // The prepared-tools input is declared only when a member needs
            // it: an unconditional declaration would rewrite every kind
            // header for a feature most repositories never declare.
            if *name == provider_input::PREPARED_TOOLS
                && !members.iter().any(|unit| !unit.prepared_tools.is_empty())
            {
                continue;
            }
            // The transport inputs likewise stay out of headers whose
            // members neither produce nor consume build products.
            if *name == provider_input::PRODUCT_PROVIDES
                && !members.iter().any(|unit| {
                    unit.products
                        .iter()
                        .any(super::product_transport::transport_eligible)
                })
            {
                continue;
            }
            if *name == provider_input::PRODUCT_TRANSPORT_READY
                && !members.iter().any(|unit| !unit.prerequisites.is_empty())
            {
                continue;
            }
            // The validation-phases input is declared only when a member is
            // phased: an unconditional declaration would rewrite every kind
            // header for a feature only phased rust units use.
            if *name == provider_input::VALIDATION_PHASES
                && !members.iter().any(|unit| unit.has_phases())
            {
                continue;
            }
            // The full-history input is declared only when a member needs
            // it: an unconditional declaration would rewrite every kind
            // header for a feature only diff-aware gates use.
            if *name == provider_input::FULL_HISTORY
                && !members.iter().any(|unit| unit.full_history)
            {
                continue;
            }
            // The toolchain input is declared only when the kind's members
            // span more than one channel: a single-channel kind provisions
            // the pin file-driven, exactly as before, byte for byte.
            if *name == provider_input::TOOLCHAIN
                && rust_toolchain_channels(members.iter().copied()).len() < 2
            {
                continue;
            }
            let _ = writeln!(output, "{}", provider_input::declaration(name));
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
    /// `provider`, and the unit's per-unit inputs through `workflow_call`; the
    /// reusable holds one collapsed step block per provider job, so its size does
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
        let env = crate::s2::platform::agreed_env(&members, kind)?;
        let mut output = Self::render_kind_units_header(kind, &members, &env);
        self.append_provider_cargo_prep_jobs(&mut output, &members, contracts);
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

    /// The job gate of a collapsed provider job: the provider the caller selected, the
    /// unit's membership in the plan's selection, and the provider's admission
    /// predicate. Membership is a single `contains` over `inputs.unit`, never
    /// an enumeration of the kind's units. A kind split across executors adds
    /// the `apple_executor` clause, so each partition admits only its own
    /// callers; an unsplit kind carries no clause.
    fn collapsed_provider_gate(
        &self,
        provider: ProviderId,
        admission: ProviderAdmission,
        apple: Option<bool>,
    ) -> String {
        let mut gate = format!(
            "inputs.provider == '{}' && contains(inputs.selected_units, format('\"unit_id\":\"{{0}}\"', inputs.unit)) && ({})",
            provider.as_str(),
            self.provider_admission_expression(admission)
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

    fn collapsed_provider_members<'a>(
        &self,
        members: &[&'a Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
        provider: ProviderId,
        trusted_only: Option<bool>,
    ) -> Vec<&'a Unit> {
        members
            .iter()
            .copied()
            .filter(|unit| {
                let contract = self.contract_for(unit, contracts);
                let active = contract
                    .providers
                    .iter()
                    .any(|job| job.provider == provider && provider_supports_unit(provider, unit));
                active
                    && trusted_only.is_none_or(|trusted| {
                        (unit.trust == crate::s2::provider::TrustReq::TrustedOnly) == trusted
                    })
            })
            .collect()
    }

    /// Render the collapsed provider jobs of one kind: one job per (provider,
    /// trust) class that has members. Hosted members share one job unless the
    /// kind splits across executors; local providers split trusted-only
    /// members into their own job so the trusted-event gate stays per-job.
    fn render_collapsed_kind_verify_job(
        &self,
        output: &mut String,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Result<(), GeneratorError> {
        if members.is_empty() {
            return Ok(());
        }
        // Collect the jobs first so the owned strings outlive each render
        // call.
        let mut jobs: Vec<CollapsedProviderJob<'_>> = Vec::new();
        for provider in &self.providers {
            let provider = *provider;
            if provider == ProviderId::GithubHosted {
                let hosted = self.collapsed_provider_members(members, contracts, provider, None);
                // A kind split across executors (portable units beside macOS
                // units) renders one collapsed job per executor: a single
                // `runs-on` cannot serve both. An unsplit kind keeps the one
                // job it has always rendered. Each partition samples its own
                // members for the runner and admits only its own callers, so
                // one macOS member can no longer route the kind's Linux units
                // onto the macOS image.
                let (apple, default): (Vec<_>, Vec<_>) = hosted
                    .into_iter()
                    .partition(|unit| unit.platform == crate::s2::provider::Platform::MacosArm64);
                let split = !apple.is_empty() && !default.is_empty();
                if !default.is_empty() {
                    jobs.push(CollapsedProviderJob {
                        provider,
                        members: default,
                        job_id: "verify-github-hosted".to_owned(),
                        display: "GitHub · hosted".to_owned(),
                        apple: split.then_some(false),
                    });
                }
                if !apple.is_empty() {
                    let (job_id, display) = if split {
                        (
                            "verify-github-hosted-apple".to_owned(),
                            "GitHub · hosted · Apple".to_owned(),
                        )
                    } else {
                        (
                            "verify-github-hosted".to_owned(),
                            "GitHub · hosted".to_owned(),
                        )
                    };
                    jobs.push(CollapsedProviderJob {
                        provider,
                        members: apple,
                        job_id,
                        display,
                        apple: split.then_some(true),
                    });
                }
                continue;
            }
            for trusted in [false, true] {
                let split =
                    self.collapsed_provider_members(members, contracts, provider, Some(trusted));
                if split.is_empty() {
                    continue;
                }
                let job_id = if trusted {
                    format!("verify-{}-trusted", provider.as_str())
                } else {
                    format!("verify-{}", provider.as_str())
                };
                let display = if trusted {
                    format!("{} · trusted", provider.as_str())
                } else {
                    provider.as_str().to_owned()
                };
                jobs.push(CollapsedProviderJob {
                    provider,
                    members: split,
                    job_id,
                    display,
                    apple: None,
                });
            }
        }
        for job in &jobs {
            // macOS-platform members only exist on hosted partitions, and
            // need the GitHub-owned macOS image instead of the Linux
            // selector. Each partition is executor-homogeneous, so sampling
            // any member selects the partition's runner.
            let macos = job.provider == ProviderId::GithubHosted
                && job
                    .members
                    .iter()
                    .any(|unit| unit.platform == crate::s2::provider::Platform::MacosArm64);
            let runs_on = if macos {
                yaml_scalar(crate::s2::MACOS_HOSTED_RUNS_ON)
            } else {
                self.runs_on_yaml(job.provider)
            };
            self.render_collapsed_provider_verify_job(
                output,
                &job.members,
                contracts,
                job.provider,
                &job.job_id,
                &job.display,
                &runs_on,
                job.apple,
            )?;
        }
        Ok(())
    }

    /// The step-summary record of one unit's dependency closure: the unit,
    /// the provider it runs on, its admission class, and the dependency
    /// list, all read from the caller's inputs.
    fn render_unit_dependency_info_step(output: &mut String) {
        output.push_str(
            "      - name: Record unit dependencies\n        env:\n          UNIT_ID: ${{ inputs.unit }}\n          UNIT_DEPENDENCIES: ${{ inputs.unit_dependencies }}\n          UNIT_ADMISSION: ${{ inputs.unit_admission }}\n          UNIT_PROVIDER: ${{ inputs.provider }}\n        run: |\n          {\n            echo '## Unit dependencies'\n            echo\n            echo \"- Unit: $UNIT_ID\"\n            echo \"- Provider: $UNIT_PROVIDER\"\n            echo \"- Admission: $UNIT_ADMISSION\"\n            if [[ -z \"$UNIT_DEPENDENCIES\" ]]; then\n              echo '- Dependencies: none'\n            else\n              echo \"- Dependencies: $UNIT_DEPENDENCIES\"\n            fi\n          } >> \"$GITHUB_STEP_SUMMARY\"\n",
        );
    }

    /// One collapsed provider job: the job header, the shared runner setup, and
    /// exactly one step block that reads every unit-specific value from the
    /// caller's inputs.
    #[expect(
        clippy::too_many_arguments,
        reason = "the job identity (provider, id, display name, runner, executor partition) is passed explicitly per provider job"
    )]
    fn render_collapsed_provider_verify_job(
        &self,
        output: &mut String,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
        provider: ProviderId,
        job_id: &str,
        display_name: &str,
        runs_on: &str,
        apple: Option<bool>,
    ) -> Result<(), GeneratorError> {
        // Every member of a collapsed job shares one admission class
        // (`collapsed_provider_members` splits local providers by trust), so the
        // first member's class is the job's.
        let gate = self.collapsed_provider_gate(
            provider,
            ProviderAdmission::for_unit(provider, members[0]),
            apple,
        );
        let display_name = yaml_scalar(display_name);
        let _ = writeln!(
            output,
            "  {job_id}:\n    name: {display_name}\n    if: ${{{{ {gate} }}}}\n    runs-on: {runs_on}\n    timeout-minutes: {}",
            Self::collapsed_timeout_minutes(members, contracts),
        );
        output.push_str("    steps:\n");
        render_ci_job_started_marker(output);
        // The caller invokes once per (unit, provider) and passes
        // `full_history` per invocation, so one expression serves mixed
        // kinds: deep legs clone full history, shallow legs stay depth-1.
        // The operand order is load-bearing: number 0 is FALSY in GitHub
        // Actions expressions, so `inputs.full_history && 0 || 1` yields 1
        // in both cases and would silently keep deep legs shallow. The
        // flipped form below yields 0 for deep legs and 1 otherwise.
        let fetch_depth = if members.iter().any(|unit| unit.full_history) {
            "\n          fetch-depth: ${{ inputs.full_history == false && 1 || 0 }}"
        } else {
            ""
        };
        let _ = writeln!(
            output,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n          ref: ${{{{ inputs.head_sha }}}}{fetch_depth}",
            self.pins.checkout
        );
        Self::render_unit_dependency_info_step(output);
        if provider == ProviderId::GithubHosted {
            self.render_unit_runtime(output, provider, members[0]);
        }
        render_ci_runner_setup_end_marker(output);
        output.push_str(&workflow_selection_file_materialize(
            &SelectionFieldSources {
                base_sha: "${{ inputs.base_sha }}",
                head_sha: "${{ inputs.head_sha }}",
                scope: "${{ inputs.scope }}",
                units: "${{ inputs.selected_unit_ids }}",
                full_units: "${{ inputs.full_units }}",
                plan_digest: "${{ inputs.plan_digest }}",
            },
        ));
        render_ci_selection_end_marker(output);
        self.render_collapsed_provider_steps(output, provider, members, contracts)?;
        // Exactly one record per dispatch: an unsplit hosted provider job
        // records every dispatch; local providers split by trust, so each
        // job records only dispatches of its own partition, plus its
        // executor partition when the kind splits one. Sibling jobs run the
        // same collapsed steps for the dispatch but stay silent here, so the
        // aggregate never sees a duplicate key. The `==` form fails closed
        // on a missing input (no record, and the aggregate fails the missing
        // record) instead of recording for an unknown dispatch.
        let mut record_gate = if provider.is_local() {
            format!(
                "always() && inputs.unit_trust == '{}'",
                members[0].trust.as_str()
            )
        } else {
            "always()".to_owned()
        };
        match apple {
            None => {}
            Some(true) => record_gate.push_str(" && inputs.apple_executor"),
            Some(false) => record_gate.push_str(" && inputs.apple_executor != true"),
        }
        output.push_str(&render_unit_result_steps(
            self.pins.upload_artifact,
            provider.as_str(),
            &record_gate,
        ));
        output.push('\n');
        Ok(())
    }

    /// The per-unit facts of one (unit, provider) pair. This is the single source
    /// both sides of the `workflow_call` boundary derive from: the caller's
    /// `with:` values and the callee's step gates.
    pub(crate) fn unit_provider_facts(
        &self,
        unit: &Unit,
        contract: &UnitContract,
        provider: ProviderId,
    ) -> ProviderStepFacts {
        let hosted = provider == ProviderId::GithubHosted;
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        let mise_tools =
            Self::mise_tool_ids_for_provider(hosted, &tools, unit, &self.mise_lock_keys);
        let mise_runner = hosted
            && tools.contains(&ToolRequirement::Mise)
            && mise_tools.is_empty()
            && commands_invoke_mise(unit);
        let mbx = (hosted && tools.contains(&ToolRequirement::MrBoxington))
            .then(|| unit_snapshot_facts(self, unit, provider));
        let cargo_bin_tools = if hosted {
            self.cargo_bin_tools(&tools)
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let tool_version = (hosted && tools.contains(&ToolRequirement::Bun))
            .then(|| unit.tool_version.clone())
            .flatten();
        let node_cache_dependency_path = (hosted && tools.contains(&ToolRequirement::Node))
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
        let seed = (contract.mutable_mount_seed && hosted)
            .then(|| {
                unit.cache.as_ref().map(|cache| {
                    (
                        cache.paths.clone(),
                        unit_snapshot_facts(self, unit, provider),
                    )
                })
            })
            .flatten();
        let bundle = if seed.is_none()
            && contract
                .cache
                .provider_enables_actions_cache(provider, self, unit)
            && let Some(cache) = &unit.cache
        {
            Some((cache.paths.clone(), cargo_source_cache_key_files(unit)))
        } else {
            None
        };
        let skip_cargo_fetch =
            cargo_network_is_restricted(unit) && provider.is_local() && self.providers.len() > 1;
        let cargo_root =
            (cargo_network_is_restricted(unit) && !skip_cargo_fetch).then(|| unit.root.clone());
        let cargo_fetch_skip_when_warm = cargo_root.is_some()
            && provider.is_local()
            && unit
                .cache
                .as_ref()
                .is_some_and(cache_is_local_host_persistent);
        ProviderStepFacts {
            mise_tools,
            mise_runner,
            mbx_enabled: tools.contains(&ToolRequirement::MrBoxington),
            mbx,
            cargo_bin_tools,
            tool_version,
            node_cache_dependency_path,
            bundle,
            seed,
            cargo_root,
            cargo_fetch_skip_when_warm,
            cargo_net_offline: cargo_network_is_restricted(unit),
            host_warm_layers: if provider.is_local() {
                local_host_warm_layers(unit, self)
            } else {
                Vec::new()
            },
            policy_runtime: provider.is_local() && unit_runs_workflow_plain_check(unit),
            candidate_publish: provider == ProviderId::GithubHosted
                && !self.repository.is_empty()
                && self.repository == crate::s2::workflow_setup_action_repository()
                && unit_owns_workflow_crate(unit),
            apple_executor: hosted && unit.platform == crate::s2::provider::Platform::MacosArm64,
            platform: unit.platform.as_str().to_owned(),
            trust: unit.trust.as_str().to_owned(),
            unit_dependencies: unit.depends_on.clone(),
            unit_admission: ProviderAdmission::for_unit(provider, unit),
            prepared_tools: super::prepared_tools::need_records(&unit.prepared_tools),
            product_provides: Self::transport_provides(unit, provider),
            product_transport_ready: Self::transport_ready(&self.units, unit, provider),
            validation_phases: unit.runnable_phases(),
            full_history: unit.full_history,
            toolchain: Self::toolchain_fact_for_kind(&self.units, unit),
        }
    }

    /// The channel fact a caller passes for `unit`: the unit's own channel,
    /// but only when its kind spans more than one — the header declares the
    /// input under the same predicate, so the two sides always agree.
    fn toolchain_fact_for_kind(units: &[Unit], unit: &Unit) -> Option<String> {
        if unit.kind != UnitKind::Rust {
            return None;
        }
        if rust_toolchain_channels(units.iter()).len() < 2 {
            return None;
        }
        unit.toolchain.as_ref().map(|pin| pin.channel.clone())
    }

    /// The mise tool ids one unit installs on `hosted`: the hosted spell
    /// for GitHub runners, the Velnor install spell for local lanes.
    fn mise_tool_ids_for_provider(
        hosted: bool,
        tools: &BTreeSet<ToolRequirement>,
        unit: &Unit,
        lock_keys: &BTreeSet<String>,
    ) -> Vec<String> {
        if hosted {
            if tools.contains(&ToolRequirement::Mise) {
                mise_tool_ids(unit, lock_keys)
            } else {
                Vec::new()
            }
        } else if tools.contains(&ToolRequirement::Mise) {
            velnor_mise_install_tool_ids(unit, lock_keys)
        } else {
            Vec::new()
        }
    }

    /// The transport records one unit publishes on `provider`: its eligible
    /// products. Hosted only; local providers share the workspace.
    fn transport_provides(unit: &Unit, provider: ProviderId) -> Vec<String> {
        if provider != ProviderId::GithubHosted {
            return Vec::new();
        }
        unit.products
            .iter()
            .filter(|product| super::product_transport::transport_eligible(product))
            .map(|product| super::product_transport::transport_record(&unit.id, &product.name))
            .collect()
    }

    /// The caller-evaluated readiness verdicts for one unit's transportable
    /// prerequisite edges on `provider`, or `None` when no edge can ride
    /// the transport and the consumer always rebuilds.
    fn transport_ready(units: &[Unit], unit: &Unit, provider: ProviderId) -> Option<String> {
        if provider != ProviderId::GithubHosted {
            return None;
        }
        let mut edges = Vec::new();
        for prerequisite in &unit.prerequisites {
            let Some(producer) = units
                .iter()
                .find(|candidate| candidate.id == prerequisite.producer)
            else {
                continue;
            };
            let eligible = producer.products.iter().any(|product| {
                product.name == prerequisite.product
                    && super::product_transport::transport_eligible(product)
            });
            if eligible && provider_supports_unit(provider, producer) {
                edges.push((
                    super::product_transport::transport_record(
                        &prerequisite.producer,
                        &prerequisite.product,
                    ),
                    unit_job_id(provider, &prerequisite.producer),
                ));
            }
        }
        super::product_transport::ready_records(&edges)
    }

    /// Whether the collapsed provider job renders the cargo-fetch phase at all:
    /// mirrors the literal provider job, which renders the phase (fetch step and
    /// marker) unless both-mode moved the fetch to the provider prep job.
    fn collapsed_renders_cargo_fetch_phase(&self, unit: &Unit, provider: ProviderId) -> bool {
        !(cargo_network_is_restricted(unit) && provider.is_local() && self.providers.len() > 1)
    }

    /// The union of prepared-tool consumer steps across the collapsed provider's
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
        facts: &[ProviderStepFacts],
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
            let gate = (!shared)
                .then(|| provider_input::contains_gate(provider_input::PREPARED_TOOLS, record));
            let mut block = String::new();
            super::prepared_tools::render_consumer_steps(
                &mut block,
                self.pins.cache_restore,
                std::slice::from_ref(need),
            );
            output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
        }
    }

    /// The union of product-consumer steps across the collapsed provider's
    /// members: one download+verify block per distinct transportable
    /// prerequisite edge, gated on the caller-evaluated `{record}:true`
    /// verdict. The verdict varies per run, so every block carries its
    /// gate; members whose producer did not run skip the block and their
    /// guarded rebuild covers the product. Hosted only.
    fn render_collapsed_product_consumer_steps(&self, output: &mut String, members: &[&Unit]) {
        let mut union: BTreeMap<String, (String, &crate::s2::platform::NamedProduct)> =
            BTreeMap::new();
        for member in members {
            for prerequisite in &member.prerequisites {
                let Some(product) = self.units.iter().find_map(|unit| {
                    if unit.id != prerequisite.producer {
                        return None;
                    }
                    unit.products
                        .iter()
                        .find(|product| product.name == prerequisite.product)
                }) else {
                    continue;
                };
                if !super::product_transport::transport_eligible(product) {
                    continue;
                }
                let record = super::product_transport::transport_record(
                    &prerequisite.producer,
                    &prerequisite.product,
                );
                union
                    .entry(record)
                    .or_insert_with(|| (prerequisite.producer.clone(), product));
            }
        }
        for (record, (producer, product)) in &union {
            let gate = provider_input::contains_gate(
                provider_input::PRODUCT_TRANSPORT_READY,
                &format!("{record}:true"),
            );
            let marker = crate::s2::platform::transport_marker(producer, &product.name);
            let block = super::product_transport::render_consumer_block(
                self.pins.download_artifact,
                producer,
                product,
                &marker,
            );
            output.push_str(&prefix_step_block_with_if(&block, Some(&gate)));
        }
    }

    /// The union of product-producer steps across the collapsed provider's
    /// members: one stage+upload block per distinct eligible product, after
    /// the checks that build it. A failed check skips the upload, so no
    /// artifact ever certifies a red producer. Hosted only.
    fn render_collapsed_product_producer_steps(&self, output: &mut String, members: &[&Unit]) {
        let mut union: BTreeMap<String, (String, &crate::s2::platform::NamedProduct)> =
            BTreeMap::new();
        for member in members {
            for product in &member.products {
                if !super::product_transport::transport_eligible(product) {
                    continue;
                }
                let record = super::product_transport::transport_record(&member.id, &product.name);
                union
                    .entry(record)
                    .or_insert_with(|| (member.id.clone(), product));
            }
        }
        if union.is_empty() {
            return;
        }
        let member_records: Vec<BTreeSet<String>> = members
            .iter()
            .map(|unit| {
                unit.products
                    .iter()
                    .filter(|product| super::product_transport::transport_eligible(product))
                    .map(|product| {
                        super::product_transport::transport_record(&unit.id, &product.name)
                    })
                    .collect()
            })
            .collect();
        for (record, (producer, product)) in &union {
            let shared = member_records
                .iter()
                .all(|records| records.contains(record));
            let gate = (!shared)
                .then(|| provider_input::contains_gate(provider_input::PRODUCT_PROVIDES, record));
            let block = super::product_transport::render_producer_block(
                self.pins.upload_artifact,
                producer,
                product,
            );
            output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
        }
    }

    /// The single step block of a collapsed provider job. Every unit-specific
    /// value is an `inputs.*` reference; a feature only some members carry
    /// renders behind an input presence gate.
    #[expect(
        clippy::too_many_lines,
        reason = "the collapsed step block keeps the complete provider contract together, in step order"
    )]
    fn render_collapsed_provider_steps(
        &self,
        output: &mut String,
        provider: ProviderId,
        members: &[&Unit],
        contracts: Option<&BTreeMap<String, UnitContract>>,
    ) -> Result<(), GeneratorError> {
        let hosted = provider == ProviderId::GithubHosted;
        let member_contracts = members
            .iter()
            .map(|unit| self.contract_for(unit, contracts))
            .collect::<Vec<_>>();
        let facts = members
            .iter()
            .zip(&member_contracts)
            .map(|(unit, contract)| self.unit_provider_facts(unit, contract, provider))
            .collect::<Vec<_>>();
        let kind = members[0].kind;
        let disagreement = |what: &str| {
            GeneratorError::usage(format!(
                "collapsed {} provider job for {} units cannot render one step block: members disagree on {what}",
                provider.as_str(),
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
                            .providers
                            .iter()
                            .any(|job| job.provider == provider && job.cache_save)
                })
                .collect::<BTreeSet<_>>();
            if saves.len() > 1 {
                return Err(disagreement("the cache-save policy"));
            }
            saves.into_iter().next().unwrap_or(false)
        };
        // One provision leg per channel: a single leg renders today's
        // file-driven provision untouched, while several legs render one
        // gated block each. Local providers provision no toolchain at all,
        // so a matrix there would verify every leg under the image's
        // toolchain instead — refused here as well as at load validation.
        let toolchain_legs = toolchain_leg_groups(members, self.rust_pin.as_ref())
            .map_err(|what| disagreement(&what))?;
        if provider.is_local() && toolchain_legs.len() > 1 {
            let channels = toolchain_legs
                .iter()
                .map(|leg| leg.channel.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GeneratorError::usage(format!(
                "collapsed {} provider job for {} units spans more than one toolchain channel ({channels}), but a local provider cannot provision per-leg toolchains",
                provider.as_str(),
                kind.label(),
            )));
        }
        let xcode = {
            let pins = members
                .iter()
                .map(|unit| unit.xcode.clone())
                .collect::<Vec<_>>();
            if pins.iter().any(|pin| pin != &pins[0]) {
                return Err(disagreement("the Xcode toolchain pin"));
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
        // the Velnor provider builds it into the host's persistent store.
        let policy_runtime = FeatureCoverage::over(&facts, |facts| facts.policy_runtime);
        if !hosted && policy_runtime.any {
            output.push_str(&gated(
                crate::s2::workflow_pinned_policy_runtime_local(
                    &self.workflow_revision,
                    "${{ github.workspace }}",
                ),
                policy_runtime,
                provider_input::POLICY_RUNTIME,
            ));
        }

        // Tool provisioning, in the literal provider job's order.
        let mise_tools = FeatureCoverage::over(&facts, |facts| !facts.mise_tools.is_empty());
        if !hosted && mise_tools.any {
            let mut block = String::new();
            render_velnor_mise_install_from_input(&mut block);
            output.push_str(&gated(block, mise_tools, provider_input::MISE_TOOLS));
        }
        if !local_skips_pinned_rust_toolchain(provider) && !toolchain_legs.is_empty() {
            if toolchain_legs.len() == 1 {
                self.render_rust_toolchain_steps(output, &toolchain_legs[0], cache_save);
            } else {
                self.render_toolchain_matrix_legs(output, &toolchain_legs, cache_save);
            }
        }
        if hosted
            && kind == UnitKind::Swift
            && let Some(xcode) = &xcode
        {
            Self::render_xcode_toolchain_step(output, xcode);
        }
        if hosted && mise_tools.any {
            let trusted = trusted_cache_save_expression(&self.default_branch);
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Set up Mise tools\n        uses: {}\n        with:\n          install_args: {}\n          cache: true\n          cache_save: ${{{{ {trusted} }}}}",
                self.pins.mise,
                provider_input::expression(provider_input::MISE_TOOLS)
            );
            output.push_str(&gated(block, mise_tools, provider_input::MISE_TOOLS));
        }
        let mise_runner = FeatureCoverage::over(&facts, |facts| facts.mise_runner);
        if hosted && mise_runner.any {
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Set up Mise\n        uses: {}\n        with:\n          install: false",
                self.pins.mise
            );
            output.push_str(&gated(block, mise_runner, provider_input::MISE_RUNNER));
        }
        let mbx = FeatureCoverage::over(&facts, |facts| facts.mbx_enabled);
        if mbx.any {
            let mut block = String::new();
            if hosted {
                let (cache_key, restore_keys) = input_snapshot(
                    UNIT_SNAPSHOT_NAMESPACE,
                    provider_input::MBX_COMPAT,
                    provider_input::MBX_DEPENDENCY_FILES,
                    provider_input::MBX_FRESHNESS_FILES,
                );
                self.render_mbx_github_step(&mut block, &cache_key, &restore_keys);
            } else {
                self.render_mbx_local_step(&mut block);
            }
            output.push_str(&gated(block, mbx, provider_input::MBX_ENABLED));
        }
        let sccache =
            FeatureCoverage::over(&facts, |facts| kind == UnitKind::Rust && !facts.mbx_enabled);
        if hosted && sccache.any {
            let mut block = String::new();
            let sccache_tools = BTreeSet::from([ToolRequirement::Sccache]);
            self.render_kind_level_tool_steps(&mut block, provider, &sccache_tools, cache_save);
            output.push_str(&prefix_step_block_with_if(
                &block,
                sccache.absent_gate(provider_input::MBX_ENABLED).as_deref(),
            ));
            render_collapsed_sccache_env_step(output, sccache, provider_input::MBX_ENABLED);
        }
        if hosted && kind_tools.contains(&ToolRequirement::Bun) {
            let versioned = FeatureCoverage::over(&facts, |facts| facts.tool_version.is_some());
            if versioned.any {
                let mut block = String::new();
                let _ = writeln!(
                    block,
                    "      - name: Set up Bun\n        uses: {}\n        with:\n          bun-version: {}",
                    self.pins.bun,
                    provider_input::expression(provider_input::TOOL_VERSION)
                );
                output.push_str(&gated(block, versioned, provider_input::TOOL_VERSION));
            }
            if !versioned.all {
                let gate = versioned
                    .any
                    .then(|| format!("inputs.{} == ''", provider_input::TOOL_VERSION));
                let block = format!(
                    "      - name: Set up Bun\n        uses: {}\n",
                    self.pins.bun
                );
                output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
            }
        }
        if hosted && kind_tools.contains(&ToolRequirement::Node) {
            let cached =
                FeatureCoverage::over(&facts, |facts| facts.node_cache_dependency_path.is_some());
            if cached.any {
                let mut block = String::new();
                self.render_node_step(
                    &mut block,
                    Some(&provider_input::expression(
                        provider_input::NODE_CACHE_DEPENDENCY_PATH,
                    )),
                );
                output.push_str(&gated(
                    block,
                    cached,
                    provider_input::NODE_CACHE_DEPENDENCY_PATH,
                ));
            }
            if !cached.all {
                let gate = cached.any.then(|| {
                    format!(
                        "inputs.{} == ''",
                        provider_input::NODE_CACHE_DEPENDENCY_PATH
                    )
                });
                let mut block = String::new();
                self.render_node_step(&mut block, None);
                output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
            }
        }
        let cargo_bin = FeatureCoverage::over(&facts, |facts| !facts.cargo_bin_tools.is_empty());
        if hosted && cargo_bin.any {
            let mut block = String::new();
            self.render_cargo_bin_tool_steps(
                &mut block,
                &provider_input::expression(provider_input::CARGO_BIN_TOOLS),
                cache_save,
            );
            output.push_str(&gated(block, cargo_bin, provider_input::CARGO_BIN_TOOLS));
        }
        self.render_collapsed_prepared_tool_steps(output, members, &facts);
        self.render_kind_level_tool_steps(output, provider, &kind_tools, cache_save);
        render_ci_tool_bootstrap_end_marker(output);

        // Cache preparation.
        let checks_offline = FeatureCoverage::over(&facts, |facts| facts.cargo_net_offline);
        let checks_env = collapsed_checks_env(checks_offline);
        let seed = FeatureCoverage::over(&facts, |facts| facts.seed.is_some());
        let bundle = FeatureCoverage::over(&facts, |facts| facts.bundle.is_some());
        if seed.any {
            let mut block = String::new();
            render_mutable_mount_seed_restore_from_input(&mut block, self, &checks_env);
            output.push_str(&gated(block, seed, provider_input::SEED_COMPAT));
        }
        if bundle.any {
            for unit in members {
                if let Some(cache) = &unit.cache {
                    render_retained_output_cache_note(output, self, unit, cache);
                }
            }
            let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(
                kind.id_prefix(),
                &format!("inputs.{}", provider_input::CACHE_KEY_FILES),
                &provider_input::expression("provider"),
                &provider_input::expression(provider_input::UNIT_PLATFORM),
                &provider_input::expression(provider_input::UNIT_TRUST),
            );
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Restore unit cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n            {}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                self.pins.cache_restore,
                provider_input::expression(provider_input::CACHE_PATHS),
            );
            output.push_str(&gated(block, bundle, provider_input::CACHE_KEY_FILES));
        }
        render_ci_cache_prep_end_marker(output);

        // Cargo source preparation.
        let fetch_phase = members
            .iter()
            .any(|unit| self.collapsed_renders_cargo_fetch_phase(unit, provider));
        if fetch_phase {
            let fetch = FeatureCoverage::over(&facts, |facts| facts.cargo_root.is_some());
            if fetch.any {
                let mut conditions = Vec::new();
                if let Some(gate) = fetch.gate(provider_input::CARGO_ROOT) {
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
                            provider_input::CACHE_KEY_FILES
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

        // The generator's self-check (`--plain --check`) resolves the D19 pin's
        // closures from local history, but unit checkouts are shallow. Fetch
        // the pin commit for check-running members before verification.
        // Local providers provision the pin through the pinned-renderer step,
        // so only the hosted provider needs this fetch.
        let check_members: Vec<&&Unit> = members
            .iter()
            .filter(|unit| unit_runs_workflow_plain_check(unit))
            .collect();
        if hosted && !check_members.is_empty() {
            let mut cases = String::new();
            for member in members {
                if check_members.iter().any(|check| check.id == member.id) {
                    let _ = writeln!(
                        cases,
                        "            {}) : ;;",
                        crate::s2::shell_quote(&member.id)
                    );
                } else {
                    let _ = writeln!(
                        cases,
                        "            {}) exit 0 ;;",
                        crate::s2::shell_quote(&member.id)
                    );
                }
            }
            let _ = writeln!(
                output,
                "      - name: Fetch D19 pin history\n        env:\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}\n        run: |\n          set -euo pipefail\n          case \"$CI_UNIT_ID\" in\n{cases}            *) echo \"unknown unit for pin fetch: $CI_UNIT_ID\" >&2; exit 1 ;;\n          esac\n{D19_PIN_FETCH_COMMANDS}",
            );
        }

        // Verified product transport: download and install the producer's
        // artifact before the checks consume it. The verify step exports
        // the ready marker the guarded rebuild reads.
        if hosted {
            self.render_collapsed_product_consumer_steps(output, members);
        }

        // Verification. Units with validation phases verify through one
        // step per runnable phase behind `run --phase`; units without keep
        // the single legacy step. A kind mixing both gates each side on the
        // dispatched unit's `validation_phases` input. Every step shares the
        // job's checkout, caches, and unit log (phase steps append; the
        // legacy step owns the log when it is the only checks step).
        let checks_started_marker = render_epoch_marker_commands("CHECKS_STARTED", "          ");
        let checks_ended_marker = render_epoch_marker_commands("CHECKS_ENDED", "          ");
        let token_env = docker_build_token_env_for_members(members);
        let phased = FeatureCoverage::over(&facts, |facts| !facts.validation_phases.is_empty());
        let rendered_phases = ValidationPhase::RUNNABLE
            .iter()
            .filter(|phase| {
                facts
                    .iter()
                    .any(|facts| facts.validation_phases.contains(phase))
            })
            .copied()
            .collect::<Vec<_>>();
        for (index, phase) in rendered_phases.iter().enumerate() {
            let coverage =
                FeatureCoverage::over(&facts, |facts| facts.validation_phases.contains(phase));
            let gate = (coverage.any && !coverage.all).then(|| {
                provider_input::contains_gate(provider_input::VALIDATION_PHASES, phase.as_str())
            });
            let started = if index == 0 {
                format!("{checks_started_marker}\n")
            } else {
                String::new()
            };
            let ended = if index + 1 == rendered_phases.len() {
                format!("{checks_ended_marker}\n")
            } else {
                String::new()
            };
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: {}\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ inputs.base_sha }}}}\n          HEAD_SHA: ${{{{ inputs.head_sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{checks_env}{token_env}\n        run: |\n          set -o pipefail\n{started}          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" --phase {} 2>&1 | tee -a \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n{ended}          exit $rc",
                phase.step_name(),
                phase.as_str(),
            );
            output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
        }
        if !phased.all {
            let gate = phased
                .any
                .then(|| format!("inputs.{} == ''", provider_input::VALIDATION_PHASES));
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Run unit checks\n        env:\n          CI_SCOPE: ${{{{ inputs.scope }}}}\n          CI_UNIT_ID: ${{{{ inputs.unit }}}}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          BASE_SHA: ${{{{ inputs.base_sha }}}}\n          HEAD_SHA: ${{{{ inputs.head_sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection{checks_env}{token_env}\n        run: |\n          set -o pipefail\n{checks_started_marker}\n          rc=0\n          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" 2>&1 | tee \"$RUNNER_TEMP/velnor-unit-log.txt\" || rc=$?\n{checks_ended_marker}\n          exit $rc",
            );
            output.push_str(&prefix_step_block_with_if(&block, gate.as_deref()));
        }

        // Stage-1 candidate packaging, after the checks that build the
        // binary it reuses: only the hosted job of the generator crate's
        // owning unit publishes, only on pull requests (the steps carry
        // that event gate themselves), and only on success (no `always()`).
        let candidate = FeatureCoverage::over(&facts, |facts| facts.candidate_publish);
        if hosted && candidate.any {
            output.push_str(&gated(
                candidate_publish_steps(self.pins.upload_artifact),
                candidate,
                provider_input::CANDIDATE_PUBLISH,
            ));
        }

        // Product publication: stage and upload the built products after
        // the checks that produced them.
        if hosted {
            self.render_collapsed_product_producer_steps(output, members);
        }

        // Cache collection.
        if seed.any {
            let mut block = String::new();
            render_mutable_mount_seed_collection_from_input(
                &mut block,
                self,
                &checks_env,
                cache_save,
            );
            output.push_str(&gated(block, seed, provider_input::SEED_COMPAT));
        }
        if cache_save && hosted && bundle.any {
            let (cache_key, _) = format_cargo_bundle_cache_key(
                kind.id_prefix(),
                &format!("inputs.{}", provider_input::CACHE_KEY_FILES),
                &provider_input::expression("provider"),
                &provider_input::expression(provider_input::UNIT_PLATFORM),
                &provider_input::expression(provider_input::UNIT_TRUST),
            );
            let mut block = String::new();
            let _ = writeln!(
                block,
                "      - name: Save unit cache\n        if: {}\n        uses: {}\n        with:\n          path: |\n            {}\n          key: {cache_key}",
                dependency_bundle_cache_save_if(&self.default_branch),
                self.pins.cache_save,
                provider_input::expression(provider_input::CACHE_PATHS),
            );
            output.push_str(&gated(block, bundle, provider_input::CACHE_KEY_FILES));
        }
        render_ci_cleanup_end_marker(output);
        let report = members
            .iter()
            .map(|unit| CacheReportFacts::for_unit(provider, unit, self))
            .collect::<Vec<_>>();
        render_phase_report_step(
            output,
            &self.ci_report_action_uses(),
            &provider_input::expression("unit"),
            provider,
            &CacheReportFacts::union(&report),
        );
        Ok(())
    }

    fn append_provider_cargo_prep_jobs(
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
            .is_some_and(cache_is_local_host_persistent);
        let fetch_script = render_cargo_fetch_roots_script(&roots, skip_when_offline_ready);
        // Only local providers share persistent Cargo stores between jobs.
        // GitHub-hosted jobs fetch into their own ephemeral workspace.
        for provider in self
            .providers
            .iter()
            .copied()
            .filter(|provider| provider.is_local())
        {
            if !members.iter().any(|unit| {
                let contract = contracts
                    .and_then(|contracts| contracts.get(&unit.id))
                    .cloned()
                    .unwrap_or_else(|| self.default_unit_contract(unit, true));
                contract
                    .providers
                    .iter()
                    .any(|job| job.provider == provider && provider_supports_unit(provider, unit))
            }) {
                continue;
            }
            let prep_id = format!("{}-prepare-cargo-sources", provider.as_str());
            // The control caller invokes the reusable once per file; the prep
            // job's unit-membership gate selects the restricted units.
            let intended_provider = "control";
            // The prep job warms the provider's stores, so it is admitted
            // exactly when the provider's jobs are. Local providers gate all
            // jobs on trusted events, so the prep gate carries the trusted
            // class — the base class is never evaluated for a local
            // provider, and a gate the check never evaluates cannot be
            // single-sourced.
            let prep_gate = format!(
                "inputs.provider == '{intended_provider}' && ({if_gate}) && ({})",
                self.provider_admission_expression(ProviderAdmission::ProviderTrusted(provider))
            );
            let _ = writeln!(
                output,
                "  {prep_id}:\n    name: prepare-cargo\n    if: ${{{{ {prep_gate} }}}}\n    runs-on: {}\n    timeout-minutes: 20\n    steps:",
                self.runs_on_yaml(provider),
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
                    units: "${{ inputs.selected_unit_ids }}",
                    full_units: "${{ inputs.full_units }}",
                    plan_digest: "${{ inputs.plan_digest }}",
                },
            ));
            if CacheBackend::Detected.provider_enables_actions_cache(provider, self, cache_unit)
                && let Some(cache) = &cache_unit.cache
            {
                let (paths, _) = rendered_cache_values(cache);
                let id_segment = cache_unit.kind.id_prefix();
                let hash = cargo_source_cache_hash_expression(cache_unit);
                let (cache_key, restore_prefix) = format_cargo_bundle_cache_key(
                    id_segment,
                    &hash,
                    provider.as_str(),
                    cache_unit.platform.as_str(),
                    cache_unit.trust.as_str(),
                );
                let _ = writeln!(
                    output,
                    "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: {cache_key}\n          restore-keys: |\n            {restore_prefix}",
                    yaml_scalar(&cache_unit.label),
                    self.pins.cache_restore,
                );
            }
            let cache_hit_gate = if CacheBackend::Detected
                .provider_enables_actions_cache(provider, self, cache_unit)
            {
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

    pub(crate) fn render_workflow_runtime_setup(&self, output: &mut String, provider: ProviderId) {
        output.push_str(&workflow_runtime_setup(
            provider,
            &self.repository,
            &self.workflow_revision,
        ));
    }

    pub(crate) fn render_workflow_runtime_download(
        &self,
        output: &mut String,
        provider: ProviderId,
    ) {
        output.push_str(&workflow_runtime_download(
            provider,
            &self.workflow_revision,
        ));
    }

    fn render_unit_runtime(&self, output: &mut String, provider: ProviderId, unit: &Unit) {
        if provider != ProviderId::GithubHosted {
            return;
        }
        // Manual dispatch jobs bootstrap the pinned runtime themselves. Apple
        // jobs cannot consume a Linux-built plan artifact even when Planning
        // is hosted.
        if unit.kind == UnitKind::Swift {
            self.render_workflow_runtime_setup(output, provider);
        } else {
            self.render_workflow_runtime_download(output, provider);
        }
    }

    /// The control-plane provider for this surface: hosted when the universe
    /// contains it, otherwise the canonical-first local provider. Under the
    /// visibility policy the universe is a singleton, so this is exactly the
    /// visibility provider.
    pub(crate) fn control_plane_provider(&self) -> ProviderId {
        crate::s2::provider::control_plane_provider(&self.providers)
    }

    /// The `runs-on:` YAML value routing one provider to its selector. Pure
    /// selector lookup: routing only, never authorization, never a fanout
    /// instruction.
    pub(crate) fn runs_on_yaml(&self, provider: ProviderId) -> String {
        self.selectors
            .get(&provider)
            .map(crate::s2::selector_runs_on_yaml)
            .unwrap_or_default()
    }

    pub(crate) fn render_plan(&self, output: &mut String) {
        // Planning is control plane. Hosted planning pins `uses:` to
        // SOURCE_REV. `rev:` uses a context-gated `${{ github.sha }}`
        // with a static fallback when this repository owns the setup action.
        let control_plane = self.control_plane_provider();
        let runtime_setup = crate::s2::workflow_runtime_setup_with_install_rev(
            control_plane,
            &self.repository,
            &self.workflow_revision,
            &crate::s2::workflow_setup_install_rev(&self.repository, &self.workflow_revision),
        );
        let base_sha = self.base_sha_expression();
        // Every event selects the static automatic set: there is no provider
        // input to read.
        let automatic = self
            .automatic_providers
            .iter()
            .map(ProviderId::as_str)
            .collect::<Vec<_>>()
            .join(",");
        // A local control plane never plans untrusted events: fork and bot
        // pull requests skip planning (and therefore the whole aggregate),
        // exactly like any other local job without a trusted event.
        let gate = self.control_plane_event_gate();
        let mut outputs = vec![
            "      scope: ${{ steps.plan.outputs.scope }}".to_owned(),
            "      base_sha: ${{ steps.plan.outputs.base_sha }}".to_owned(),
            "      head_sha: ${{ steps.plan.outputs.head_sha }}".to_owned(),
            "      units: ${{ steps.plan.outputs.units }}".to_owned(),
            "      unit_ids: ${{ steps.plan.outputs.unit_ids }}".to_owned(),
            "      full_units: ${{ steps.plan.outputs.full_units }}".to_owned(),
            "      plan_digest: ${{ steps.plan.outputs.plan_digest }}".to_owned(),
            "      excluded: ${{ steps.plan.outputs.excluded }}".to_owned(),
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
        // Presence-only no-work marker: `planned_no_work` is `true` or
        // absent, never `false`. Consumers MUST branch
        // `needs.plan.outputs.planned_no_work == 'true'` for the no-work
        // path; any other value (including absent) means the plan selected
        // work. Nothing branches yet: the aggregate tolerates explicit
        // no-work itself, so the marker is proof, not control flow.
        outputs.push(
            "      # Presence-only no-work marker: `planned_no_work` is `true` or absent,\n      # never `false`. Consumers MUST branch\n      # `needs.plan.outputs.planned_no_work == 'true'` for the no-work path;\n      # any other value (including absent) means the plan selected work."
                .to_owned(),
        );
        outputs.push("      planned_no_work: ${{ steps.plan.outputs.planned_no_work }}".to_owned());
        outputs.push("      no_work_reason: ${{ steps.plan.outputs.no_work_reason }}".to_owned());
        let _ = writeln!(
            output,
            "  plan:\n    name: {}\n{gate}    runs-on: {}\n    outputs:\n{}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Select affected units\n        id: plan\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          CI_SCOPE_OVERRIDE: ${{{{ github.event.inputs.scope || '' }}}}\n          BASE_SHA: ${{{{ {base_sha} }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_PROVIDERS: {automatic}\n          VELNOR_EVENT_TRUSTED: ${{{{ ({trusted}) && 'true' || 'false' }}}}\n          {expected_work_env}: {expected_work_file}\n        run: |\n          set -euo pipefail\n          if [[ -z \"${{CI_SCOPE_OVERRIDE:-}}\" ]]; then unset CI_SCOPE_OVERRIDE; fi\n          mkdir -p {expected_work_dir}\n          velnor-workflow plan --config .github/ci/project.toml\n",
            crate::s2::control_job_name("Planning"),
            self.runs_on_yaml(self.control_plane_provider()),
            outputs.join("\n"),
            self.pins.checkout,
            base_sha = base_sha,
            trusted = Self::trusted_event_expression(),
            expected_work_env = EXPECTED_WORK_FILE_ENV,
            expected_work_file = EXPECTED_WORK_FILE,
            expected_work_dir = EXPECTED_WORK_DIR,
        );
        output.push_str(&render_expected_work_upload_step(self.pins.upload_artifact));
        // The runtime artifact feeds hosted consumers only: hosted unit
        // jobs and the hosted aggregate download it for their verified
        // runtime. A local control plane plans ambient — no setup step, so
        // no `steps.runtime` closure for Prepare to check — and every
        // local job runs the fleet binary, so the plan publishes only for
        // a hosted control plane and never orphans an artifact.
        if control_plane == ProviderId::GithubHosted {
            output.push_str(&workflow_runtime_artifact_upload(&self.workflow_revision));
        }
    }

    pub(crate) fn render_policy(&self, output: &mut String) {
        let local = self.control_plane_provider().is_local();
        let gate = local.then(|| crate::s2::control_plane_trusted_gate(&self.default_branch));
        output.push_str(&crate::s2::policy_job(&crate::s2::PolicyJobSpec {
            name: "Policy",
            revision: &self.workflow_revision,
            runner: &self.runs_on_yaml(self.control_plane_provider()),
            repository: &self.repository,
            cache_backend: if local { "local" } else { "github" },
            trusted_gate: gate.as_deref(),
            default_branch: &self.default_branch,
            declared_ruleset_contexts: &self.declared_ruleset_contexts,
        }));
    }

    /// The trusted-event predicate every admission class shares: fork and bot
    /// pull requests are untrusted; every other event (same-repo PRs, push,
    /// schedule, `merge_group`, dispatch — dispatch requires write access) is
    /// trusted. The plan step renders the same predicate into
    /// `VELNOR_EVENT_TRUSTED`, so the planner and the gates agree by
    /// construction.
    pub(crate) fn trusted_event_expression() -> String {
        crate::s2::TRUSTED_EVENT_EXPRESSION.to_owned()
    }

    /// The `if:` line a local control-plane job carries: the trusted-event
    /// predicate, parenthesized exactly as the policy gate matcher expects.
    /// Hosted control planes need no gate and render nothing.
    fn control_plane_event_gate(&self) -> String {
        if self.control_plane_provider().is_local() {
            format!(
                "    if: ${{{{ ({}) }}}}\n",
                Self::trusted_event_expression()
            )
        } else {
            String::new()
        }
    }

    /// Conjoin a job's functional `if:` condition with the trusted-event
    /// predicate on a local control plane. The functional side stays inside
    /// its own parentheses so the gate matcher verifies the shape without
    /// parsing expressions. Hosted control planes keep the condition as is.
    fn control_plane_gated_condition(&self, condition: &str) -> String {
        if self.control_plane_provider().is_local() {
            format!("({condition}) && ({})", Self::trusted_event_expression())
        } else {
            condition.to_owned()
        }
    }

    /// A `workflow_dispatch` selecting `provider`, on any ref.
    ///
    /// Dispatch is not ref-gated: GitHub only accepts a dispatch from an
    /// actor with write access and only onto a ref of this repository, which
    /// is the same authorship a same-repository pull-request head carries.
    /// There is no provider input to match: dispatches select the static
    /// universe.
    fn dispatch_provider_expression() -> &'static str {
        "github.event_name == 'workflow_dispatch'"
    }

    fn base_sha_expression(&self) -> String {
        format!(
            "github.event.pull_request.base.sha || github.event.inputs.base_sha || github.event.before || 'refs/heads/{}'",
            self.default_branch
        )
    }

    /// The one admission predicate of a provider class: the GitHub expression
    /// that is true exactly when the class's jobs run for the current event,
    /// ref, and dispatch input. Rendered verbatim into the callee job's
    /// `if:`, the aggregate caller's `if:`, and the required check's
    /// `PROVIDER_ADMITTED_*` environment, so the three cannot disagree.
    ///
    /// A trust-gated class with no online trusted runner is `&& false`: the
    /// predicate itself says the provider is not admitted, and the required check
    /// therefore expects `skipped` for it like any other unadmitted provider.
    pub(crate) fn provider_admission_expression(&self, admission: ProviderAdmission) -> String {
        match admission {
            ProviderAdmission::AnyLocalTrusted => {
                let mut locals = self
                    .providers
                    .iter()
                    .copied()
                    .filter(|provider| provider.is_local())
                    .map(|provider| {
                        self.provider_admission_expression(ProviderAdmission::ProviderTrusted(
                            provider,
                        ))
                    })
                    .collect::<Vec<_>>();
                if locals.is_empty() {
                    return "false".to_owned();
                }
                locals.sort();
                locals.dedup();
                if locals.len() == 1 {
                    return locals.swap_remove(0);
                }
                format!("({})", locals.join(") || ("))
            }
            ProviderAdmission::Provider(provider)
            | ProviderAdmission::ProviderTrusted(provider) => {
                // Dispatches select the static universe, so the event side is
                // a tautology for automatic providers and dispatch-only
                // otherwise. Render the collapsed form, never `(A) || (!A)`.
                let event = if self.automatic_providers.contains(&provider) {
                    "true".to_owned()
                } else {
                    Self::dispatch_provider_expression().to_owned()
                };
                if admission.trusted_only() {
                    if event == "true" {
                        format!("({})", Self::trusted_event_expression())
                    } else {
                        format!("({event}) && ({})", Self::trusted_event_expression())
                    }
                } else {
                    event
                }
            }
        }
    }

    pub(crate) fn render_hierarchy_groups(&self, output: &mut String, include_policy: bool) {
        let kinds = self
            .units
            .iter()
            .map(|unit| unit.kind)
            .collect::<BTreeSet<_>>();
        let gate = self.control_plane_event_gate();
        for kind in kinds {
            let group_id = stack_group_job_id(kind);
            let group_name = crate::s2::control_job_name(crate::s2::provider_kind_label(kind));
            let _ = writeln!(output, "  {group_id}:\n    name: {group_name}");
            let needs = if include_policy {
                "[plan, policy]"
            } else {
                "[plan]"
            };
            let _ = writeln!(
                output,
                "    needs: {needs}\n{gate}    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit {} jobs\n        run: echo 'group ready'\n",
                self.runs_on_yaml(self.control_plane_provider()),
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
                    "  {child_id}:\n    name: {child_name}\n    needs: [{group_id}]\n{gate}    runs-on: {}\n    timeout-minutes: 5\n    steps:\n      - name: Admit runner jobs\n        run: echo 'crate group ready'\n",
                    self.runs_on_yaml(self.control_plane_provider()),
                );
            }
        }
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

    /// Render the hosted-provider Rust toolchain contract: restore the cached
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
        // The pin leg provisions file-driven, exactly as before; a leg whose
        // channel differs from the recorded pin provisions explicitly,
        // because the checkout's pin file would resolve a file-driven
        // install to the wrong channel. An unknown pin keeps the file-driven
        // shape: hand-built configs record none, and a declared channel
        // without a pin file has no Rust sources to verify (the scan
        // refuses pin-less Rust repositories), so its checks fail at
        // runtime regardless of which toolchain provisions.
        if self
            .rust_pin
            .as_ref()
            .is_none_or(|pin| pin.channel == toolchain.channel)
        {
            render_pinned_toolchain_steps(
                output,
                self.pins.cache_restore,
                self.pins.cache_save,
                toolchain,
                save_gate.as_deref(),
            );
        } else {
            render_explicit_toolchain_steps(
                output,
                self.pins.cache_restore,
                self.pins.cache_save,
                toolchain,
                "rustup-toolchain",
                save_gate.as_deref(),
            );
        }
    }

    /// The provision legs of a multi-channel collapsed provider job: the pin
    /// leg keeps the file-driven block, every other channel renders an
    /// explicit block, and each block gates on the caller's `toolchain`
    /// input. Legs render in channel order with disjoint restore-step ids,
    /// so each save gate reads its own leg's `cache-hit`.
    fn render_toolchain_matrix_legs(
        &self,
        output: &mut String,
        legs: &[RustToolchain],
        cache_save: bool,
    ) {
        let trusted_cache = trusted_cache_save_expression(&self.default_branch);
        let mut taken = BTreeSet::from(["rustup-toolchain".to_owned()]);
        for leg in legs {
            let pin_leg = self
                .rust_pin
                .as_ref()
                .is_some_and(|pin| pin.channel == leg.channel);
            let step_id = if pin_leg {
                "rustup-toolchain".to_owned()
            } else {
                explicit_toolchain_step_id(&leg.channel, &mut taken)
            };
            let save_gate = cache_save.then(|| {
                format!("({trusted_cache}) && steps.{step_id}.outputs.cache-hit != 'true'")
            });
            let mut block = String::new();
            if pin_leg {
                render_pinned_toolchain_steps(
                    &mut block,
                    self.pins.cache_restore,
                    self.pins.cache_save,
                    leg,
                    save_gate.as_deref(),
                );
            } else {
                render_explicit_toolchain_steps(
                    &mut block,
                    self.pins.cache_restore,
                    self.pins.cache_save,
                    leg,
                    &step_id,
                    save_gate.as_deref(),
                );
            }
            let gate = format!("inputs.{} == '{}'", provider_input::TOOLCHAIN, leg.channel);
            output.push_str(&prefix_step_block_with_if(&block, Some(&gate)));
        }
    }

    /// The hosted-provider Xcode contract: select the installed Xcode
    /// matching the repository's `.xcode-version` pin, fail closed when no
    /// installed Xcode matches, and export `DEVELOPER_DIR` so every later
    /// step uses the same toolchain instead of the image default. An exact
    /// `Xcode_<pin>.app` match wins; otherwise the newest installed
    /// `Xcode_<pin>*.app` is selected; otherwise an unversioned
    /// `Xcode.app` is accepted only when its reported version matches the
    /// pin. Every fallible probe pipeline ends in `|| true`: under
    /// `set -euo pipefail` a bare failing substitution would kill the step
    /// before the diagnostic runs. Local providers own their Xcode
    /// installation and skip this step.
    fn render_xcode_toolchain_step(output: &mut String, xcode: &XcodeToolchain) {
        let _ = writeln!(
            output,
            "      - name: Select Xcode {}\n        run: |{}",
            xcode.version(),
            Self::xcode_probe_script(xcode.version()).lines().fold(
                String::new(),
                |mut indented, line| {
                    indented.push_str("\n          ");
                    indented.push_str(line);
                    indented
                }
            ),
        );
    }

    /// The `Select Xcode` probe body, factored out so behavioral tests
    /// execute the exact script the workflow renders.
    fn xcode_probe_script(pin: &str) -> String {
        format!(
            "set -euo pipefail\nwant=\"{pin}\"\ndir=\"\"\nif [ -d \"/Applications/Xcode_${{want}}.app\" ]; then\n  dir=\"/Applications/Xcode_${{want}}.app\"\nelse\n  dir=\"$(ls -d /Applications/Xcode_${{want}}*.app 2>/dev/null | sort | tail -n 1 || true)\"\nfi\nif [ -z \"$dir\" ] && [ -d /Applications/Xcode.app ]; then\n  found=\"$(/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -version 2>/dev/null | head -n 1 | awk '{{print $2}}' || true)\"\n  case \"$found\" in\n    \"$want\"|\"$want\".*) dir=/Applications/Xcode.app ;;\n  esac\nfi\nif [ -z \"$dir\" ]; then\n  echo \"::error::no installed Xcode matches pin $want\" >&2\n  ls /Applications | grep -i xcode || true\n  exit 1\nfi\necho \"DEVELOPER_DIR=$dir/Contents/Developer\" >> \"$GITHUB_ENV\"\nexport DEVELOPER_DIR=\"$dir/Contents/Developer\"\nxcodebuild -version\nswift --version"
        )
    }

    /// The cargo-bin tools a hosted provider installs through the pinned
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
        provider: ProviderId,
        unit: &Unit,
        cache_save: bool,
    ) {
        // The Velnor job image is the toolchain boundary for self-hosted jobs.
        // Hosted setup actions either are not admitted by Velnor or would
        // redundantly download tools already pinned in that image. Keep
        // transport/setup actions provider-specific instead of rendering one
        // action surface and hoping the runner can ignore the other provider.
        let hosted = provider == ProviderId::GithubHosted;
        let tools = Self::tools_for_unit(unit, self.mise_present, self.mr_boxington);
        if !hosted && tools.contains(&ToolRequirement::Mise) {
            // Hosted mise-action is not admitted on Velnor. Auto-install is
            // off on the checks step, so declared lockfile tools must be
            // installed explicitly or shims fail closed. Install only what
            // this unit's commands need — never the whole root manifest.
            render_velnor_mise_install(output, unit, &self.mise_lock_keys);
        }
        if !local_skips_pinned_rust_toolchain(provider)
            && let Some(toolchain) = &unit.toolchain
        {
            self.render_rust_toolchain_steps(output, toolchain, cache_save);
        }
        if hosted && tools.contains(&ToolRequirement::Mise) {
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
            if provider == ProviderId::GithubHosted {
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
                let (cache_key, restore_keys) =
                    unit_snapshot(self, unit, provider, UNIT_SNAPSHOT_NAMESPACE);
                self.render_mbx_github_step(output, &cache_key, &restore_keys);
            } else {
                self.render_mbx_local_step(output);
            }
        }
        if hosted && tools.contains(&ToolRequirement::Bun) {
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
        if hosted && tools.contains(&ToolRequirement::Node) {
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
        if hosted && !install_action_tools.is_empty() {
            self.render_cargo_bin_tool_steps(output, &install_action_tools.join(","), cache_save);
        }
        if !unit.prepared_tools.is_empty() {
            // Declared prepared tools restore after every provider-local
            // provisioning step: the consumer needs the runtime on `PATH`
            // (rendered before provisioning) and must not disturb the
            // toolchain state the steps above established.
            super::prepared_tools::render_consumer_steps(
                output,
                self.pins.cache_restore,
                &unit.prepared_tools,
            );
        }
        self.render_kind_level_tool_steps(output, provider, &tools, cache_save);
    }

    /// GitHub provider: the store budget export precedes the action so the
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

    /// Velnor provider: the job image pins Mr. Boxington and the runner mounts
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
        provider: ProviderId,
        tools: &BTreeSet<ToolRequirement>,
        cache_save: bool,
    ) {
        let hosted = provider == ProviderId::GithubHosted;
        if hosted && tools.contains(&ToolRequirement::Gradle) {
            let _ = writeln!(
                output,
                "      - name: Set up Gradle\n        uses: {}",
                self.pins.gradle
            );
        }
        if hosted && tools.contains(&ToolRequirement::Sccache) {
            let _ = writeln!(
                output,
                "      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0",
                self.pins.sccache
            );
        }
        if hosted && tools.contains(&ToolRequirement::Mold) {
            output.push_str(&hosted_mold_setup(&self.default_branch, cache_save));
        }
        if hosted && tools.contains(&ToolRequirement::DockerBuildx) {
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
        if hosted && tools.contains(&ToolRequirement::OpenTofu) {
            let _ = writeln!(
                output,
                "      - name: Set up OpenTofu\n        uses: {}\n        with:\n          tofu_version: {}\n          tofu_wrapper: false",
                self.pins.opentofu_setup,
                OPEN_TOFU_VERSION,
            );
        }
        if hosted && tools.contains(&ToolRequirement::Homebrew) {
            output.push_str(
                "      - name: Prepare Linuxbrew path\n        shell: bash\n        run: |\n          set -euo pipefail\n          if command -v brew >/dev/null 2>&1; then\n            exit 0\n          fi\n          linuxbrew_bin=/home/linuxbrew/.linuxbrew/bin\n          linuxbrew_sbin=/home/linuxbrew/.linuxbrew/sbin\n          if [[ ! -x \"$linuxbrew_bin/brew\" ]]; then\n            printf '%s\\n' 'Homebrew unavailable: install brew or expose it on PATH' >&2\n            exit 1\n          fi\n          [[ -n \"${GITHUB_PATH:-}\" ]] || { printf '%s\\n' 'GITHUB_PATH is unavailable' >&2; exit 1; }\n          printf '%s\\n%s\\n' \"$linuxbrew_bin\" \"$linuxbrew_sbin\" >> \"$GITHUB_PATH\"\n",
            );
        }
    }
}

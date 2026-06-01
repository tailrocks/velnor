#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
JACKIN_ROOT="${VELNOR_JACKIN_ROOT:-/tmp/velnor-jackin}"
CHAINARGOS_ROOT="${VELNOR_CHAINARGOS_ROOT:-/tmp/velnor-chainargos}"

if [[ ! -d "$CHAINARGOS_ROOT/.github" && -d /tmp/velnor-java-monorepo/.github ]]; then
  CHAINARGOS_ROOT="/tmp/velnor-java-monorepo"
fi

if [[ ! -d "$JACKIN_ROOT/.github" ]]; then
  echo "missing jackin target checkout: $JACKIN_ROOT" >&2
  echo "set VELNOR_JACKIN_ROOT to a jackin checkout" >&2
  exit 2
fi

if [[ ! -d "$CHAINARGOS_ROOT/.github" ]]; then
  echo "missing ChainArgos target checkout: $CHAINARGOS_ROOT" >&2
  echo "set VELNOR_CHAINARGOS_ROOT to the ChainArgos monorepo checkout" >&2
  exit 2
fi

cd "$ROOT"

bash -n \
  scripts/chainargos_rust_target_sequence.sh \
  scripts/chainargos_target_smoke.sh \
  scripts/fixture_smoke.sh \
  scripts/fixture_status.sh \
  scripts/jackin_rust_linux_sequence.sh \
  scripts/jackin_target_smoke.sh \
  scripts/live_evidence_common.sh \
  scripts/live_host_doctor.sh \
  scripts/live_evidence_common_test.sh \
  scripts/target_smoke_common.sh \
  scripts/target_verify.sh \
  scripts/workflow_dispatch_common.sh \
  scripts/workflow_dispatch_common_test.sh

python3 scripts/target_audit.py --check-target-mvp "$JACKIN_ROOT" "$CHAINARGOS_ROOT" >/tmp/velnor-target-audit.txt
python3 scripts/target_audit.py --self-test "$JACKIN_ROOT" "$CHAINARGOS_ROOT" >/tmp/velnor-target-audit-self-test.txt
python3 scripts/check_runner_reference.py
scripts/live_evidence_common_test.sh
scripts/workflow_dispatch_common_test.sh

tests=(
  cached_target_action_metadata_expressions_use_supported_subset
  fetched_target_composite_actions_expand_to_supported_invocations
  fetched_target_composite_actions_have_repository_action_closure
  fetched_target_workflow_actions_have_metadata
  target_workflow_expressions_use_supported_subset
  resolves_job_context_data_expressions_and_conditions
  target_marketplace_actions_map_to_native_adapters
  target_workflow_repository_actions_plan_from_cached_metadata
  native_repository_action_plan_does_not_require_ref
  native_repository_actions_ignore_pinned_ref_metadata
  native_repository_actions_do_not_require_ref_metadata
  plans_external_checkout_with_path_ref_token_and_full_fetch
  plans_checkout_from_run_service_typed_inputs
  checkout_ref_from_previous_step_requires_runtime_context
  writes_safe_directory_for_workspace_checkout
  target_workflow_run_preview_gate_matches_jackin_shape
  recognizes_matching_job_cancellation_message
  parses_broker_migration_message_url
  target_check_image_output_gates_java_monorepo_build_steps
  applies_job_run_defaults_to_script_steps
  applies_run_service_typed_job_run_defaults
  target_jackin_release_job_env_resolves_needs_version
  target_required_job_cancelled_need_condition_fails_and_skips_ok_step
  builds_target_cache_action_plan_from_multiline_inputs
  target_cache_and_artifact_actions_receive_runtime_env
  builds_github_runtime_env_from_job_message
  reads_runtime_endpoint_values_case_insensitively
  native_artifacts_are_shared_across_jobs_in_same_run_workdir
  native_download_artifact_all_mode_uses_named_directories
  native_download_artifact_reports_container_download_path
  native_download_artifact_normalizes_permissions
  native_upload_artifact_expands_target_release_globs
  native_upload_artifact_excludes_hidden_files_by_default
  native_upload_artifact_requires_overwrite_for_duplicate_name
  native_upload_artifact_maps_container_tmp_to_host_temp
  native_cache_reports_miss_without_node_sidecar
  native_cache_fail_on_miss_is_quiet_on_hit
  native_cache_trims_folded_yaml_primary_key
  native_cache_lookup_only_does_not_restore_paths
  native_cache_restore_key_uses_newest_prefix_match
  native_cache_saves_and_restores_from_shared_workdir
  builds_target_upload_artifact_invocation_inputs
  builds_target_download_artifact_invocation_inputs
  target_aggregate_needs_expands_exact_failure_gate
  target_docker_action_inputs_match_current_workflows
  mounts_host_docker_cli_when_socket_is_mounted
  target_docker_javascript_actions_receive_socket_and_cli_mounts
  native_docker_adapters_invoke_docker_cli_without_node_sidecars
  native_docker_metadata_matches_target_pr_and_publish_tags
  native_docker_build_push_honors_load_input_separately
  target_renovate_action_receives_docker_cli_socket_and_env
  target_sccache_action_soft_fails_and_gates_wrapper_step
  target_pages_actions_receive_runtime_env_and_outputs
  target_docs_environment_url_uses_deployment_step_output
  target_docs_sitemap_step_receives_deployment_page_url
  target_check_deployed_docs_keeps_sitemap_step_output_input
  target_check_deployed_docs_runtime_inputs_resolve_after_sitemap_step
  target_rust_docker_job_outputs_resolve_after_filter_and_targets_steps
  target_jackin_dispatch_or_filter_job_output_uses_runtime_fallback
  target_jackin_release_job_outputs_collect_platform_shas
  target_paths_filter_receives_event_context_and_outputs_gate_steps
  target_setup_actions_share_home_toolcache_and_path
  native_setup_tool_adapters_use_job_container_without_node_sidecars
  target_rust_tool_installers_share_cargo_home_path_and_cache_env
  target_rust_cache_receives_runtime_env_and_posts_on_failure
  default_labels_keep_velnor_only
  task_agent_payload_keeps_runner_labels
  task_agent_accepts_lowercase_label_types_from_github
  target_mvp_labels_cover_current_x64_linux_target_jobs
  target_mvp_arm_label_is_explicit
  arm_label_requires_arm_host
  linux_host_is_required
  run_preflight_args_preserve_target_docker_requirements
  run_preflight_args_default_work_dir_under_config
)

for test_name in "${tests[@]}"; do
  output="$(cargo test -q -p velnor-runner "$test_name" 2>&1)"
  printf '%s\n' "$output"
  if grep -q "running 0 tests" <<<"$output"; then
    echo "test filter matched zero tests: $test_name" >&2
    exit 1
  fi
done

echo "target audit written to /tmp/velnor-target-audit.txt"
echo "target verifier passed shell syntax check, ${#tests[@]} focused checks, live evidence helper self-test, and workflow dispatch helper self-test"

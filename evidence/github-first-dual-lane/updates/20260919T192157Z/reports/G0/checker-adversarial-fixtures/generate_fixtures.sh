#!/usr/bin/env bash
set -euo pipefail

out_dir=${1:?output directory}
mkdir -p "$out_dir"

source_revision=abe9ad82a2d4d01b706bbc6122ab6ccb150faad9
source_digest=sha256:b39b3bcb5d149db66a03483bd7a19482af2657df6f962872030d820a39f13514
row_digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
observed=2026-09-20T00:00:00Z

read -r -d '' repo_text <<'EOF' || true
tailrocks/velnor
tailrocks/velnor-apt
tailrocks/parallax
tailrocks/tracing-request-level
tailrocks/termrock
tailrocks/termpane
tailrocks/tablerock
tailrocks/schemalane
tailrocks/ruxel
tailrocks/pg-bigdecimal
tailrocks/parallax-telemetry-playground
tailrocks/homebrew-tablerock
tailrocks/homebrew-ruxel
tailrocks/homebrew-parallax
tailrocks/homebrew-holla
tailrocks/holla-apt
tailrocks/holla
tailrocks/homebrew-velnor
tailrocks/tailrocks-typescript-skills
tailrocks/tailrocks-skill-authoring-skills
tailrocks/tailrocks-rust-skills
tailrocks/tailrocks-roadmap-skills
tailrocks/tailrocks-pull-request-skills
tailrocks/tailrocks-open-source-skills
tailrocks/tailrocks-macos-skills
tailrocks/tailrocks-code-quality-skills
jackin-project/jackin
jackin-project/jackin-agent-smith
jackin-project/homebrew-tap
jackin-project/jackin-the-architect
jackin-project/jackin-sentinel
jackin-project/jackin-role-action
EOF
repo_json=$(printf '%s\n' "$repo_text" | jq -R . | jq -s .)

# G0: inventory-only, with every required source/snapshot/record field filled.
jq -n \
  --argjson repos "$repo_json" \
  --arg source_revision "$source_revision" \
  --arg source_digest "$source_digest" \
  --arg row_digest "$row_digest" \
  --arg observed "$observed" \
  '
  def context: [{context:"CI", app_id:"1"}];
  def workloads: [{workload_id:"unit", platform:"linux", architecture:"x64"}];
  def expected_jobs: [{job_id:"unit-github", workload_id:"unit", provider:"github", platform:"linux", architecture:"x64", required:true, child_workflow:null}];
  def eligibility: {github:"eligible", velnor:"not-applicable"};
  def hosts: {github:{runner_kind:"github-hosted", required_labels:[], forbidden_labels:["self-hosted"]}};
  def manifest_repositories:
    $repos | map({repository:., repository_role:"library", default_branch:"main",
      expected_workload_ids:["unit"], required_check_contexts_and_apps:context,
      workload_platform_architecture:workloads, expected_jobs:expected_jobs,
      generated_plan_digest:$row_digest, workflow_path:".github/workflows/ci.yml",
      workflow_revision:$source_revision, provider_eligibility:eligibility,
      host_contracts:hosts, release_applicability:"not-applicable",
      generator_revision:$source_revision, runtime_product_id:"velnor",
      generator_artifact_digest:$row_digest, configuration_digest:$row_digest,
      generated_tree_digest:$row_digest, scan_state_digest:$row_digest,
      runtime_release_version:"0.1.0", runtime_source_sha:$source_revision,
      job_image_digest:$row_digest});
  def snapshot_repositories:
    $repos | to_entries | map(.value as $repo | {
      repository:$repo, repository_id:(1000 + .key), default_branch:"main",
      default_branch_sha:$source_revision,
      ruleset:{required_checks:context, source_url:("https://github.com/" + $repo + "/rulesets/1"), pages_complete:true},
      workflows:[{path:".github/workflows/ci.yml", revision:$source_revision, source_sha:$source_revision, event:"push", source_url:("https://github.com/" + $repo + "/blob/" + $source_revision + "/.github/workflows/ci.yml")}],
      main_executions:[], open_prs:[]});
  def inventory_records:
    $repos | map({repository:., repository_role:"library", default_branch:"main",
      default_branch_sha:$source_revision, observed_at_utc:$observed,
      generator_revision:$source_revision, runtime_product_id:"velnor",
      generator_artifact_digest:$row_digest, configuration_digest:$row_digest,
      generated_tree_digest:$row_digest, scan_state_digest:$row_digest,
      runtime_release_version:"0.1.0", runtime_source_sha:$source_revision,
      job_image_digest:$row_digest, expected_workload_ids:["unit"],
      required_check_contexts_and_apps:context, workload_platform_architecture:workloads,
      provider_eligibility:{}, justified_exclusions:[], pr_number:null,
      pr_head_sha:null, pr_base_sha:null, tested_merge_sha:null, merge_group_sha:null,
      workflow_path:".github/workflows/ci.yml", workflow_revision:$source_revision,
      event:"inventory", run_id:0, run_attempt:0, run_url:"",
      trigger_source_sha:$source_revision, actual_checkout_sha:$source_revision,
      provider:"inventory", runner_name:"", host_id:"", runner_kind:"", runner_labels:[],
      run_status:"", run_conclusion:"", expected_jobs:[], actual_job_ids:[],
      actual_job_conclusions:{}, logs:[], child_run_links:[], required_checks:[],
      release:null, install:null, owner:"inventory", reviewer:"inventory",
      gate_status:"inventory", blocker:null, next_action:null});
  {
    schema_version:2, manifest_id:"github-first-dual-lane-2026-09-19",
    source:{repository:"tailrocks/velnor", revision:$source_revision, digest:$source_digest, reviewed_by:"independent-review"},
    repositories:manifest_repositories
  }' > "$out_dir/g0-manifest.json"

jq -n \
  --argjson repos "$repo_json" \
  --arg source_revision "$source_revision" \
  --arg observed "$observed" \
  '
  def context: [{context:"CI", app_id:"1"}];
  def workloads: [{workload_id:"unit", platform:"linux", architecture:"x64"}];
  def expected_jobs: [{job_id:"unit-github", workload_id:"unit", provider:"github", platform:"linux", architecture:"x64", required:true, child_workflow:null}];
  def snapshot_repositories:
    $repos | to_entries | map(.value as $repo | {
      repository:$repo, repository_id:(1000 + .key), default_branch:"main",
      default_branch_sha:$source_revision,
      ruleset:{required_checks:context, source_url:("https://github.com/" + $repo + "/rulesets/1"), pages_complete:true},
      workflows:[{path:".github/workflows/ci.yml", revision:$source_revision, source_sha:$source_revision, event:"push", source_url:("https://github.com/" + $repo + "/blob/" + $source_revision + "/.github/workflows/ci.yml")}],
      main_executions:[], open_prs:[]});
  {schema_version:2, snapshot_id:"snapshot-adversarial-g0", manifest_id:"github-first-dual-lane-2026-09-19",
   observed_at_utc:$observed,
   source:{collector:"fixture-collector", collector_revision:$source_revision, api_base:"https://api.github.com", captured_at_utc:$observed, read_only:true, page_count:1, permission_scopes:["metadata:read"]},
   repositories:snapshot_repositories}' > "$out_dir/g0-snapshot.json"

jq -n \
  --argjson repos "$repo_json" \
  --arg source_revision "$source_revision" \
  --arg row_digest "$row_digest" \
  --arg observed "$observed" \
  '
  def context: [{context:"CI", app_id:"1"}];
  def workloads: [{workload_id:"unit", platform:"linux", architecture:"x64"}];
  def records:
    $repos | map({repository:., repository_role:"library", default_branch:"main",
      default_branch_sha:$source_revision, observed_at_utc:$observed,
      generator_revision:$source_revision, runtime_product_id:"velnor",
      generator_artifact_digest:$row_digest, configuration_digest:$row_digest,
      generated_tree_digest:$row_digest, scan_state_digest:$row_digest,
      runtime_release_version:"0.1.0", runtime_source_sha:$source_revision,
      job_image_digest:$row_digest, expected_workload_ids:["unit"],
      required_check_contexts_and_apps:context, workload_platform_architecture:workloads,
      provider_eligibility:{}, justified_exclusions:[], pr_number:null,
      pr_head_sha:null, pr_base_sha:null, tested_merge_sha:null, merge_group_sha:null,
      workflow_path:".github/workflows/ci.yml", workflow_revision:$source_revision,
      event:"inventory", run_id:0, run_attempt:0, run_url:"",
      trigger_source_sha:$source_revision, actual_checkout_sha:$source_revision,
      provider:"inventory", runner_name:"", host_id:"", runner_kind:"", runner_labels:[],
      run_status:"", run_conclusion:"", expected_jobs:[], actual_job_ids:[],
      actual_job_conclusions:{}, logs:[], child_run_links:[], required_checks:[],
      release:null, install:null, owner:"inventory", reviewer:"inventory",
      gate_status:"inventory", blocker:null, next_action:null});
  {schema_version:2, manifest_id:"github-first-dual-lane-2026-09-19", snapshot_id:"snapshot-adversarial-g0", stage:"G0",
   records:records, reviewer_attestation:null,
   g0_inventory:{source_url:("https://github.com/tailrocks/velnor/tree/" + $source_revision),
      repository_count:32, open_pr_count:0, workflow_repository_count:32, ruleset_repository_count:32,
      workload_matrix_digest:$row_digest, dependency_graph_digest:$row_digest,
      access_scopes:["metadata:read"], access_gaps:[], orchestrator_model:"gpt-6-astra",
      orchestrator_effort:"low", agent_model:"gpt-5.6-luna", agent_effort:"max"}}' > "$out_dir/g0-evidence.json"

# G1: one real hosted execution per repository. This is a complete positive
# control for execution/expected-job/log/check binding, while remaining
# release-free at G1.
jq --arg observed "$observed" --arg source_revision "$source_revision" \
  'def context: [{context:"CI", app_id:"1"}];
   def expected_jobs: [{job_id:"unit-github", workload_id:"unit", provider:"github", platform:"linux", architecture:"x64", required:true, child_workflow:null}];
   .repositories |= map(.provider_eligibility={github:"eligible",velnor:"not-applicable"}
      | .host_contracts={github:{runner_kind:"github-hosted",required_labels:[],forbidden_labels:["self-hosted"]}})' \
  "$out_dir/g0-manifest.json" > "$out_dir/g1-manifest.json"

jq --arg source_revision "$source_revision" --arg observed "$observed" \
  'def context: [{context:"CI", app_id:"1"}];
   def make_execution($repo;$n):
     (1000 + $n) as $run | ("job-" + ($n|tostring)) as $job |
     {run_id:$run,run_attempt:1,run_url:("https://github.com/"+$repo+"/actions/runs/"+($run|tostring)),workflow_path:".github/workflows/ci.yml",workflow_revision:$source_revision,event:"push",trigger_source_sha:$source_revision,actual_checkout_sha:$source_revision,status:"completed",conclusion:"success",provider:"github",runner_name:"GitHub Actions",host_id:("host-"+($n|tostring)),runner_kind:"github-hosted",runner_labels:["ubuntu-latest"],jobs:[{job_id:$job,job_name:"unit-github",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",status:"completed",conclusion:"success",event:"push",runner_name:"GitHub Actions",host_id:("host-"+($n|tostring)),runner_kind:"github-hosted",runner_labels:["ubuntu-latest"],source_url:("https://github.com/"+$repo+"/actions/jobs/"+$job)}],required_checks:[{context:"CI",app_id:"1",status:"completed",conclusion:"success",run_id:$run,job_id:$job,source_url:("https://github.com/"+$repo+"/checks/"+($run|tostring)),event:"push"}],child_runs:[]};
   .repositories |= map(. as $repo | .main_executions=[make_execution(.repository;(.repository_id-1000+1))])' \
  "$out_dir/g0-snapshot.json" > "$out_dir/g1-snapshot.json"

# Replace the inventory record array with deterministic execution records. The
# repository order is the canonical order, so the numeric suffix is stable.
jq --arg source_revision "$source_revision" --arg row_digest "$row_digest" --arg observed "$observed" \
  'def context: [{context:"CI", app_id:"1"}];
   def expected_jobs: [{job_id:"unit-github", workload_id:"unit", provider:"github", platform:"linux", architecture:"x64", required:true, child_workflow:null}];
   .records = (.records | to_entries | map(.value as $record | (.key + 1) as $n | (1000 + $n) as $run | ("job-"+($n|tostring)) as $job |
     ($record | .provider_eligibility={github:"eligible",velnor:"not-applicable"}
      | .event="push" | .run_id=$run | .run_attempt=1
      | .run_url=("https://github.com/"+.repository+"/actions/runs/"+($run|tostring))
      | .trigger_source_sha=$source_revision | .actual_checkout_sha=$source_revision
      | .provider="github" | .runner_name="GitHub Actions" | .host_id=("host-"+($n|tostring))
      | .runner_kind="github-hosted" | .runner_labels=["ubuntu-latest"]
      | .run_status="completed" | .run_conclusion="success" | .expected_jobs=expected_jobs
      | .actual_job_ids=[$job] | .actual_job_conclusions={($job):"success"}
      | .logs=[("https://github.com/"+.repository+"/actions/jobs/"+$job+"/logs")]
      | .required_checks=[{context:"CI",app_id:"1",job_id:$job,status:"completed",conclusion:"success",run_id:$run,source_url:("https://github.com/"+.repository+"/checks/"+($run|tostring)),event:"push"}]
      | .owner="owner" | .reviewer="reviewer" | .gate_status="pass")))
   | .stage="G1" | .g0_inventory=null' \
  "$out_dir/g0-evidence.json" > "$out_dir/g1-evidence.json"

# Adversarial mutations. Every output is made from one positive control.
jq '.records[0].blocker="blocked" | .records[0].next_action="repair"' \
  "$out_dir/g0-evidence.json" > "$out_dir/g0-blocker-next-action.json"
jq '.repositories[0].provider_eligibility.third_party="eligible" | .repositories[0].expected_jobs[0].provider="third_party"' \
  "$out_dir/g0-manifest.json" > "$out_dir/g0-third-provider-manifest.json"
jq '.records[0].provider="third_party"' \
  "$out_dir/g0-evidence.json" > "$out_dir/g0-third-provider-record.json"
jq '.repositories[0].repository="tailrocks/velnor-substituted"' \
  "$out_dir/g0-manifest.json" > "$out_dir/g0-substituted-name.json"
jq '.records[0].logs=[]' "$out_dir/g1-evidence.json" > "$out_dir/g1-empty-logs.json"
jq '.records[0].logs=["not-a-url"]' "$out_dir/g1-evidence.json" > "$out_dir/g1-malformed-logs.json"
jq '.repositories[0].expected_jobs[0].child_workflow={repository:"tailrocks/velnor",workflow_path:".github/workflows/child.yml",event:"workflow_run"}' \
  "$out_dir/g1-manifest.json" > "$out_dir/g1-required-child-omitted.json.manifest"
jq '.records[0].actual_job_ids += ["job-extra"]' "$out_dir/g1-evidence.json" > "$out_dir/g1-extra-job-id.json"
jq '.records[0].actual_job_ids += ["job-1"]' "$out_dir/g1-evidence.json" > "$out_dir/g1-duplicate-job-id.json"
jq '.repositories[0].repository="tailrocks/velnor-substituted"' "$out_dir/g1-manifest.json" > "$out_dir/g1-substituted-name.json"
jq '.records[0].release={applicability:"not-applicable",justification:"fixture",execution:null,channel:"stable"}' \
  "$out_dir/g1-evidence.json" > "$out_dir/g1-flat-release-alias.json"
jq '.records[0].install={applicability:"not-applicable",justification:"fixture",environment:null,operations:null,installed:null,service:null,functional_result:null,version:"0.1.0"}' \
  "$out_dir/g1-evidence.json" > "$out_dir/g1-flat-install-alias.json"

# Canonical-command alias probes use the genuinely passing G0 control.
cp "$out_dir/g0-manifest.json" "$out_dir/g0-positive-manifest.json"
cp "$out_dir/g0-snapshot.json" "$out_dir/g0-positive-snapshot.json"
cp "$out_dir/g0-evidence.json" "$out_dir/g0-positive-evidence.json"

#!/usr/bin/env bash
set -euo pipefail

# Distinct second-wave fixtures. This script consumes the already-generated
# positive controls and never regenerates or mutates the historical matrix.
out_dir=${1:?fixture directory}
fx="$out_dir"

source_revision=abe9ad82a2d4d01b706bbc6122ab6ccb150faad9
stale_revision=1111111111111111111111111111111111111111
unknown_revision=2222222222222222222222222222222222222222
pr_head=3333333333333333333333333333333333333333
pr_base=4444444444444444444444444444444444444444
pr_merge=5555555555555555555555555555555555555555
pr_wrong=6666666666666666666666666666666666666666
row_digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

g1m="$fx/g1-manifest.json"
g1s="$fx/g1-snapshot.json"
g1e="$fx/g1-evidence.json"
placeholder="$fx/canonical-release-placeholder.json"

# Source-plan omission: add a second required job/check to the manifest and to
# the positive snapshot/record. The negative case removes only B from the
# EvidenceRecord, leaving the authoritative source plan and live job intact.
jq '
  .repositories |= map(
    .required_check_contexts_and_apps += [{context:"CI-B",app_id:"2"}]
    | .expected_jobs += [{job_id:"unit-github-b",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",required:true,child_workflow:null}]
  )
' "$g1m" > "$fx/g1-two-job-manifest.json"

jq '
  .repositories |= map(
    .ruleset.required_checks += [{context:"CI-B",app_id:"2"}]
    | .main_executions |= map(
        . as $run
        | (($run.jobs[0].job_id | sub("job-";"job-b-"))) as $job
        | .jobs += [($run.jobs[0]
            | .job_id=$job
            | .job_name="unit-github-b"
            | .source_url=(.source_url | sub("/actions/jobs/[^/]+$"; "/actions/jobs/" + $job)))]
        | (.run_id | tostring) as $run_id
        | .required_checks += [{context:"CI-B",app_id:"2",status:"completed",conclusion:"success",run_id:($run_id|tonumber),job_id:$job,source_url:(.run_url | sub("/actions/runs/[^/]+$"; "/checks/" + $run_id)),event:"push"}]
      )
  )
' "$g1s" > "$fx/g1-two-job-snapshot.json"

jq '
  .records |= map(
    . as $record
    | (($record.actual_job_ids[0] | sub("job-";"job-b-"))) as $job
    | .required_check_contexts_and_apps += [{context:"CI-B",app_id:"2"}]
    | .expected_jobs += [{job_id:"unit-github-b",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",required:true,child_workflow:null}]
    | .actual_job_ids += [$job]
    | .actual_job_conclusions[$job]="success"
    | (.run_id | tostring) as $run_id
    | .required_checks += [{context:"CI-B",app_id:"2",job_id:$job,status:"completed",conclusion:"success",run_id:($run_id|tonumber),source_url:(.run_url | sub("/actions/runs/[^/]+$"; "/checks/" + $run_id)),event:"push"}]
  )
' "$g1e" > "$fx/g1-two-job-evidence.json"

jq '
  .records |= map(
    . as $record
    | (($record.actual_job_ids | map(select(startswith("job-b-"))) | .[0])) as $job
    | .expected_jobs |= map(select(.job_id != "unit-github-b"))
    | .actual_job_ids |= map(select(. != $job))
    | .actual_job_conclusions |= with_entries(select(.key != $job))
    | .required_check_contexts_and_apps |= map(select(.context != "CI-B"))
    | .required_checks |= map(select(.context != "CI-B"))
  )
' "$fx/g1-two-job-evidence.json" > "$fx/g1-two-job-omitted-record.json"

# Child obligation erase: the authoritative manifest and captured child graph
# remain complete; only the evidence record's expected-job child declaration is
# erased. This differs from the historical "child run omitted" fixture.
jq '
  .repositories |= (to_entries | map(
    if .key == 0 then
      .value | .main_executions |= map(
        .child_runs=[{parent_run_id:.run_id,run_id:3001,run_attempt:1,repository:"tailrocks/velnor",workflow_path:".github/workflows/child.yml",event:"workflow_run",source_sha:"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9",provider:"github",status:"completed",conclusion:"success",source_url:"https://github.com/tailrocks/velnor/actions/runs/3001"}]
      )
    else .value
    end
  ))
' "$g1s" > "$fx/g1-child-complete-snapshot.json"

jq '
  .records |= (to_entries | map(
    if .key == 0 then
      .value
      | .child_run_links=[{parent_run_id:.run_id,run_id:3001,run_attempt:1,repository:"tailrocks/velnor",workflow_path:".github/workflows/child.yml",event:"workflow_run",source_sha:"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9",provider:"github",status:"completed",conclusion:"success",run_url:"https://github.com/tailrocks/velnor/actions/runs/3001"}]
      | .expected_jobs[0].child_workflow={repository:"tailrocks/velnor",workflow_path:".github/workflows/child.yml",event:"workflow_run"}
    else .value
    end
  ))
  | .stage="G1"
' "$g1e" > "$fx/g1-child-complete-evidence.json"

jq '
  .records[0].expected_jobs[0].child_workflow=null
' "$fx/g1-child-complete-evidence.json" > "$fx/g1-child-obligation-erased-evidence.json"

# Snapshot/record self-consistency is not live freshness. Both sides are
# rewritten to a stale but syntactically valid branch tip, preserving all
# internal links so the offline false-green is isolated.
jq --arg stale "$stale_revision" '
  .repositories |= map(
    .default_branch_sha=$stale
    | .main_executions |= map(.trigger_source_sha=$stale | .actual_checkout_sha=$stale)
  )
' "$g1s" > "$fx/g1-stale-self-matching-snapshot.json"
jq --arg stale "$stale_revision" '
  .records |= map(.default_branch_sha=$stale | .trigger_source_sha=$stale | .actual_checkout_sha=$stale)
' "$g1e" > "$fx/g1-stale-self-matching-evidence.json"

# Terminal run state must be authoritative independently of successful child
# jobs. Keep job/check claims successful while changing the run state.
jq '.repositories |= map(.main_executions |= map(.status="queued"))' "$g1s" > "$fx/g1-queued-run-snapshot.json"
jq '.records |= map(.run_status="queued")' "$g1e" > "$fx/g1-queued-run-evidence.json"
jq '.repositories |= map(.main_executions |= map(.status="completed" | .conclusion="cancelled"))' "$g1s" > "$fx/g1-cancelled-run-snapshot.json"
jq '.records |= map(.run_status="completed" | .run_conclusion="cancelled")' "$g1e" > "$fx/g1-cancelled-run-evidence.json"

# Unknown status/trust values are not successful substitutes.
jq '.repositories |= map(.main_executions |= map(.status="mystery"))' "$g1s" > "$fx/g1-unknown-status-snapshot.json"
jq '.records |= map(.run_status="mystery")' "$g1e" > "$fx/g1-unknown-status-evidence.json"
jq '.repositories |= map(.main_executions |= map(.runner_kind="mystery"))' "$g1s" > "$fx/g1-unknown-trust-snapshot.json"
jq '.records |= map(.runner_kind="mystery")' "$g1e" > "$fx/g1-unknown-trust-evidence.json"

# workflow_run is a child producer event, not a qualifying default-branch
# execution. Keep the entire supplied pair coherent to test source semantics.
jq '
  .repositories |= map(.main_executions |= map(
    .event="workflow_run"
    | .jobs |= map(.event="workflow_run")
    | .required_checks |= map(.event="workflow_run")
  ))
' "$g1s" > "$fx/g1-workflow-run-producer-snapshot.json"
jq '.records |= map(.event="workflow_run" | .required_checks |= map(.event="workflow_run"))' "$g1e" > "$fx/g1-workflow-run-producer-evidence.json"

# Hosted counterpart absence: start from the known Velnor-only host fixture,
# make GitHub independently eligible and add a GitHub source-plan job/host
# contract, but leave the captured run/record Velnor-only. G4/G5 must demand the
# missing hosted lane instead of accepting a Velnor-only result.
jq '
  .repositories |= map(
    .provider_eligibility.github="eligible"
    | .host_contracts.github={runner_kind:"github-hosted",required_labels:[],forbidden_labels:["self-hosted"]}
    | .expected_jobs += [{job_id:"unit-github",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",required:true,child_workflow:null}]
  )
' "$fx/g4-fakehost-manifest.json" > "$fx/g4-no-hosted-counterpart-manifest.json"
cp "$fx/g4-fakehost-snapshot.json" "$fx/g4-no-hosted-counterpart-snapshot.json"
jq '.records |= map(.provider_eligibility.github="eligible")' "$fx/g4-fakehost-evidence-release-shape.json" > "$fx/g4-no-hosted-counterpart-evidence.json"
cp "$fx/g4-no-hosted-counterpart-evidence.json" "$fx/g5-no-hosted-counterpart-evidence.json"

# G2 release/install waiver: mark the source plan applicable, then downgrade
# both typed evidence objects to not-applicable with a justification. Current
# 2ba guards only Applicability::Required, so this exposes the missing
# applicable=>required invariant.
jq '.repositories |= map(.release_applicability="applicable")' "$g1m" > "$fx/g2-waived-applicable-manifest.json"
jq '
  .stage="G2"
  | .records |= map(
      .release={applicability:"not-applicable",justification:"waived by fixture",execution:null}
      | .install={applicability:"not-applicable",justification:"waived by fixture",environment:null,operations:null,installed:null,service:null,functional_result:null}
    )
' "$g1e" > "$fx/g2-waived-applicable-evidence.json"

# EvidenceRecord pin checks: one mismatch and one omission set are kept as
# separate cases so the report can distinguish wrong values from absent pins.
jq '
  .records[0]
  |= (.generator_revision="2222222222222222222222222222222222222222"
      | .runtime_product_id="other-runtime"
      | .generator_artifact_digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      | .configuration_digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      | .generated_tree_digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      | .scan_state_digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      | .runtime_release_version="9.9.9"
      | .runtime_source_sha="2222222222222222222222222222222222222222"
      | .job_image_digest="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
' "$g1e" > "$fx/g1-pin-mismatch-evidence.json"
jq '
  .records[0]
  |= (.generator_revision=""
      | .generator_artifact_digest=""
      | .configuration_digest=""
      | .generated_tree_digest=""
      | .scan_state_digest=""
      | .runtime_source_sha=""
      | .job_image_digest="")
' "$g1e" > "$fx/g1-pin-omission-evidence.json"

# b11 first-slice role-coverage negatives. A one-provider push is not a PR
# candidate and cannot satisfy current-PR coverage. These are used with the
# b11 checker only; 2ba does not yet enforce the same typed PR inventory.
jq '
  .repositories |= map(.open_prs=[{number:953,state:"open",draft:false,author:"contributor",author_association:"CONTRIBUTOR",head_repository:"contributor/velnor",head_sha:"3333333333333333333333333333333333333333",base_sha:"4444444444444444444444444444444444444444",merge_sha:null,merge_group_sha:null,source_url:"https://github.com/tailrocks/velnor/pull/953",executions:[]}])
' "$g1s" > "$fx/g1-role-coverage-missing-pr-snapshot.json"
jq '
  .records |= map(.pr_number=null | .pr_head_sha=null | .pr_base_sha=null | .tested_merge_sha=null | .event="push")
' "$g1e" > "$fx/g1-role-coverage-missing-pr-evidence.json"
jq '.stage="G3" | .records |= map(.release={applicability:"not-applicable",justification:"coverage fixture",execution:null} | .install={applicability:"not-applicable",justification:"coverage fixture",environment:null,operations:null,installed:null,service:null,functional_result:null})' "$fx/g1-role-coverage-missing-pr-evidence.json" > "$fx/g3-role-coverage-missing-pr-evidence.json"
cp "$fx/g1-role-coverage-missing-pr-snapshot.json" "$fx/g3-role-coverage-missing-pr-snapshot.json"

# Manual dispatch with self-successful jobs and no PR association is a
# diagnostic run, not current-main evidence. The source is intentionally
# immutable and not dispatched here; this is an offline regression fixture.
jq '
  .repositories |= map(.main_executions |= map(.event="workflow_dispatch" | .status="completed" | .conclusion="success"))
' "$g1s" > "$fx/g1-unassociated-manual-success-snapshot.json"
jq '
  .records |= map(.event="workflow_dispatch" | .run_status="completed" | .run_conclusion="success" | .pr_number=null | .pr_head_sha=null | .pr_base_sha=null | .tested_merge_sha=null)
' "$g1e" > "$fx/g1-unassociated-manual-success-evidence.json"

# One current open PR with a completed post-merge candidate. Main records stay
# present for every repository; only the first repository gains this PR subject
# and its matching execution record. The three wrong-identity cases mutate the
# EvidenceRecord only; missing-postmerge mutates the authoritative PR snapshot.
jq --arg head "$pr_head" --arg base "$pr_base" --arg merge "$pr_merge" --arg source "$source_revision" '
  .repositories |= (to_entries | map(
    if .key == 0 then
      .value
      | .open_prs=[{
          number:953,state:"open",draft:false,author:"contributor",author_association:"CONTRIBUTOR",
          head_repository:"contributor/velnor",head_sha:$head,base_sha:$base,merge_sha:$merge,merge_group_sha:null,
          source_url:"https://github.com/tailrocks/velnor/pull/953",
          executions:[
            (.main_executions[0]
             | .run_id=1953
             | .run_attempt=1
             | .run_url="https://github.com/tailrocks/velnor/actions/runs/1953"
             | .event="pull_request"
             | .trigger_source_sha=$head
             | .actual_checkout_sha=$merge
             | .jobs |= map(.job_id="job-pr-1" | .event="pull_request" | .source_url="https://github.com/tailrocks/velnor/actions/jobs/job-pr-1")
             | .required_checks |= map(.run_id=1953 | .event="pull_request" | .job_id="job-pr-1" | .source_url="https://github.com/tailrocks/velnor/checks/1953"))
          ]
        }]
    else .value
    end
  ))
' "$g1s" > "$fx/g1-pr-positive-snapshot.json"

jq --arg head "$pr_head" --arg base "$pr_base" --arg merge "$pr_merge" '
  .records |= (.[0:1] + [(
    .[0]
    | .pr_number=953
    | .pr_head_sha=$head
    | .pr_base_sha=$base
    | .tested_merge_sha=$merge
    | .merge_group_sha=null
    | .event="pull_request"
    | .run_id=1953
    | .run_url="https://github.com/tailrocks/velnor/actions/runs/1953"
    | .trigger_source_sha=$head
    | .actual_checkout_sha=$merge
    | .run_status="completed"
    | .run_conclusion="success"
    | .actual_job_ids=["job-pr-1"]
    | .actual_job_conclusions={"job-pr-1":"success"}
    | .required_checks |= map(.run_id=1953 | .event="pull_request" | .job_id="job-pr-1" | .source_url="https://github.com/tailrocks/velnor/checks/1953")
    | .logs=["https://github.com/tailrocks/velnor/actions/jobs/job-pr-1/logs"]
  )] + .[1:])
' "$g1e" > "$fx/g1-pr-positive-evidence.json"

jq --arg wrong "$pr_wrong" '.repositories[0].open_prs[0].merge_sha=null' "$fx/g1-pr-positive-snapshot.json" > "$fx/g1-pr-missing-postmerge-snapshot.json"
cp "$fx/g1-pr-positive-evidence.json" "$fx/g1-pr-missing-postmerge-evidence.json"
jq --arg wrong "$pr_wrong" '.records |= map(if .pr_number == 953 then .pr_head_sha=$wrong else . end)' "$fx/g1-pr-positive-evidence.json" > "$fx/g1-pr-wrong-head-evidence.json"
jq --arg wrong "$pr_wrong" '.records |= map(if .pr_number == 953 then .pr_base_sha=$wrong else . end)' "$fx/g1-pr-positive-evidence.json" > "$fx/g1-pr-wrong-base-evidence.json"
jq --arg wrong "$pr_wrong" '.records |= map(if .pr_number == 953 then .tested_merge_sha=$wrong else . end)' "$fx/g1-pr-positive-evidence.json" > "$fx/g1-pr-wrong-merge-evidence.json"

# Keep jq's output strict and make the generated fixture inventory auditable.
for file in "$fx"/g1-two-job-manifest.json "$fx"/g1-two-job-snapshot.json "$fx"/g1-two-job-evidence.json "$fx"/g1-two-job-omitted-record.json \
  "$fx"/g1-child-complete-snapshot.json "$fx"/g1-child-complete-evidence.json "$fx"/g1-child-obligation-erased-evidence.json \
  "$fx"/g1-stale-self-matching-snapshot.json "$fx"/g1-stale-self-matching-evidence.json \
  "$fx"/g1-queued-run-snapshot.json "$fx"/g1-queued-run-evidence.json "$fx"/g1-cancelled-run-snapshot.json "$fx"/g1-cancelled-run-evidence.json \
  "$fx"/g1-unknown-status-snapshot.json "$fx"/g1-unknown-status-evidence.json "$fx"/g1-unknown-trust-snapshot.json "$fx"/g1-unknown-trust-evidence.json \
  "$fx"/g1-workflow-run-producer-snapshot.json "$fx"/g1-workflow-run-producer-evidence.json \
  "$fx"/g4-no-hosted-counterpart-manifest.json "$fx"/g4-no-hosted-counterpart-snapshot.json "$fx"/g4-no-hosted-counterpart-evidence.json "$fx"/g5-no-hosted-counterpart-evidence.json \
  "$fx"/g2-waived-applicable-manifest.json "$fx"/g2-waived-applicable-evidence.json \
  "$fx"/g1-pin-mismatch-evidence.json "$fx"/g1-pin-omission-evidence.json \
  "$fx"/g1-role-coverage-missing-pr-snapshot.json "$fx"/g1-role-coverage-missing-pr-evidence.json \
  "$fx"/g1-unassociated-manual-success-snapshot.json "$fx"/g1-unassociated-manual-success-evidence.json \
  "$fx"/g1-pr-positive-snapshot.json "$fx"/g1-pr-positive-evidence.json "$fx"/g1-pr-missing-postmerge-snapshot.json "$fx"/g1-pr-missing-postmerge-evidence.json \
  "$fx"/g1-pr-wrong-head-evidence.json "$fx"/g1-pr-wrong-base-evidence.json "$fx"/g1-pr-wrong-merge-evidence.json; do
  jq -e . "$file" >/dev/null
done

printf '%s\n' "generated extension fixtures in $fx"

#!/usr/bin/env bash
set -euo pipefail

fx=${1:?fixture directory}
source_revision=abe9ad82a2d4d01b706bbc6122ab6ccb150faad9
wrong_revision=2222222222222222222222222222222222222222
manifest_digest_b=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

# Build a coherent two-lane G6 source plan from the G1 controls. Each lane has
# one provider-specific job; each record carries the full source-plan job list
# because lane parity compares the declarative plan, while job reconciliation
# filters it by the record provider.
jq '
  .repositories |= map(
    .provider_eligibility={github:"eligible",velnor:"eligible"}
    | .host_contracts.velnor={runner_kind:"velnor-managed",required_labels:["velnor-host"],forbidden_labels:["github-hosted"]}
    | .expected_jobs += [{job_id:"unit-velnor",workload_id:"unit",provider:"velnor",platform:"linux",architecture:"x64",required:true,child_workflow:null}]
  )
' "$fx/g1-manifest.json" > "$fx/g6-dual-manifest.json"

jq '
  .repositories |= map(
    . as $repo
    | (.main_executions[0]) as $base
    | (($base.run_id + 10000)) as $run
    | (($base.jobs[0].job_id | sub("job-";"job-v-"))) as $job
    | ($base
       | .run_id=$run
       | .run_url=(.run_url | sub("/actions/runs/[^/]+$"; "/actions/runs/" + ($run|tostring)))
       | .provider="velnor"
       | .runner_name="Velnor Runner"
       | .host_id=("velnor-host-" + ($run|tostring))
       | .runner_kind="velnor-managed"
       | .runner_labels=["velnor-host"]
       | .jobs |= map(.job_id=$job | .job_name="unit-velnor" | .provider="velnor" | .runner_name="Velnor Runner" | .host_id=("velnor-host-" + ($run|tostring)) | .runner_kind="velnor-managed" | .runner_labels=["velnor-host"] | .source_url=("https://github.com/" + $repo.repository + "/actions/jobs/" + $job))
       | .required_checks |= map(.run_id=$run | .job_id=$job | .source_url=("https://github.com/" + $repo.repository + "/checks/" + ($run|tostring)))
     ) as $velnor
    | .main_executions += [$velnor]
  )
' "$fx/g1-snapshot.json" > "$fx/g6-dual-snapshot.json"

jq '
  .stage="G6"
  | .records |= (to_entries | map(
      .value as $github
      | (($github.run_id + 10000)) as $run
      | (($github.actual_job_ids[0] | sub("job-";"job-v-"))) as $job
      | ($github
         | .provider_eligibility={github:"eligible",velnor:"eligible"}
         | .provider="velnor"
         | .run_id=$run
         | .run_url=(.run_url | sub("/actions/runs/[^/]+$"; "/actions/runs/" + ($run|tostring)))
         | .runner_name="Velnor Runner"
         | .host_id=("velnor-host-" + ($run|tostring))
         | .runner_kind="velnor-managed"
         | .runner_labels=["velnor-host"]
         | .expected_jobs=[{job_id:"unit-github",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",required:true,child_workflow:null},{job_id:"unit-velnor",workload_id:"unit",provider:"velnor",platform:"linux",architecture:"x64",required:true,child_workflow:null}]
         | .actual_job_ids=[$job]
         | .actual_job_conclusions={($job):"success"}
         | .logs=[("https://github.com/" + .repository + "/actions/jobs/" + $job + "/logs")]
         | .required_checks=[{context:"CI",app_id:"1",job_id:$job,status:"completed",conclusion:"success",run_id:$run,source_url:("https://github.com/" + .repository + "/checks/" + ($run|tostring)),event:"push"}]
         | .release={applicability:"not-applicable",justification:"fixture does not exercise release scope",execution:null}
         | .install={applicability:"not-applicable",justification:"fixture does not exercise install scope",environment:null,operations:null,installed:null,service:null,functional_result:null}
       ) as $velnor
      | {key:.key, value:$github}
      , {key:(.key + 32), value:$velnor}
    ) | sort_by(.key) | map(.value))
  | .records |= map(.provider_eligibility={github:"eligible",velnor:"eligible"} | .expected_jobs=[{job_id:"unit-github",workload_id:"unit",provider:"github",platform:"linux",architecture:"x64",required:true,child_workflow:null},{job_id:"unit-velnor",workload_id:"unit",provider:"velnor",platform:"linux",architecture:"x64",required:true,child_workflow:null}] | .release={applicability:"not-applicable",justification:"fixture does not exercise release scope",execution:null} | .install={applicability:"not-applicable",justification:"fixture does not exercise install scope",environment:null,operations:null,installed:null,service:null,functional_result:null})
' "$fx/g1-evidence.json" > "$fx/g6-dual-evidence.json"

# Assert one GitHub + one Velnor row for every repository before mutating.
jq -e '(.records|length)==64 and ([.records[].provider]|group_by(.)|map(length)|sort)==[32,32]' "$fx/g6-dual-evidence.json" >/dev/null

# Source mismatch: move only the Velnor authoritative run and its record to a
# different valid SHA. Branch freshness and lane parity must reject it.
jq --arg wrong "$wrong_revision" '
  .repositories |= map(.main_executions |= map(if .provider=="velnor" then .trigger_source_sha=$wrong | .actual_checkout_sha=$wrong else . end))
' "$fx/g6-dual-snapshot.json" > "$fx/g6-lane-source-mismatch-snapshot.json"
jq --arg wrong "$wrong_revision" '
  .records |= map(if .provider=="velnor" then .trigger_source_sha=$wrong | .actual_checkout_sha=$wrong else . end)
' "$fx/g6-dual-evidence.json" > "$fx/g6-lane-source-mismatch-evidence.json"

# Workload mismatch: Velnor's captured job/record agree with each other but
# no longer match the reviewed source workload matrix or GitHub lane.
jq '
  .repositories |= map(.main_executions |= map(if .provider=="velnor" then .jobs |= map(.workload_id="other") else . end))
' "$fx/g6-dual-snapshot.json" > "$fx/g6-lane-workload-mismatch-snapshot.json"
jq '
  .records |= map(if .provider=="velnor" then .expected_workload_ids=["other"] | .workload_platform_architecture=[{workload_id:"other",platform:"linux",architecture:"x64"}] else . end)
' "$fx/g6-dual-evidence.json" > "$fx/g6-lane-workload-mismatch-evidence.json"

# Publisher divergence: add typed release executions with different producer
# manifest digests to the two otherwise-associated lanes. Install remains
# explicitly justified so single-publisher is visible in the result.
release='{"applicability":"required","justification":null,"execution":{"channel":"stable","version":"0.1.0","tag_target_sha":"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9","release_id":"fixture-release","asset_digests":{},"producer":{"repository":"tailrocks/velnor","workflow_path":".github/workflows/release.yml","run_id":7001,"run_url":"https://github.com/tailrocks/velnor/actions/runs/7001","source_commit":"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9","manifest_sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"apt":{"repository":"tailrocks/homebrew-velnor","revision":"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9","suite":"stable","candidate":"0.1.0","manifest_sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"homebrew":{"tap":"tailrocks/homebrew-velnor","revision":"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9","formula":"velnor","version":"0.1.0","manifest_sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}'
jq --argjson release "$release" '.repositories |= map(.release_applicability="applicable")' "$fx/g6-dual-manifest.json" > "$fx/g6-duplicate-publisher-manifest.json"
jq --argjson release "$release" --arg digest "$manifest_digest_b" '
  .records |= map(
    .release=$release
    | .install={applicability:"not-applicable",justification:"fixture install scope",environment:null,operations:null,installed:null,service:null,functional_result:null}
    | if .provider=="velnor" then .release.execution.producer.manifest_sha256=$digest | .release.execution.apt.manifest_sha256=$digest | .release.execution.homebrew.manifest_sha256=$digest else . end
  )
' "$fx/g6-dual-evidence.json" > "$fx/g6-duplicate-publisher-evidence.json"

# Both lanes still need a PR-subject record. A push for each lane is not a
# substitute for the current PR candidate at G6.
jq '.repositories[0].open_prs=[{number:953,state:"open",draft:false,author:"contributor",author_association:"CONTRIBUTOR",head_repository:"contributor/velnor",head_sha:"3333333333333333333333333333333333333333",base_sha:"4444444444444444444444444444444444444444",merge_sha:"5555555555555555555555555555555555555555",merge_group_sha:null,source_url:"https://github.com/tailrocks/velnor/pull/953",executions:[]}]' "$fx/g6-dual-snapshot.json" > "$fx/g6-role-coverage-missing-pr-snapshot.json"
cp "$fx/g6-dual-evidence.json" "$fx/g6-role-coverage-missing-pr-evidence.json"

# G7 self-review strings: bind a structurally valid external attestation, then
# make every owner/reviewer pair identical. Offline mode must still be rejected
# and the self-review must be independently reported.
jq '
  .stage="G7"
  | .records |= map(.owner="same-reviewer" | .reviewer="same-reviewer")
  | .reviewer_attestation={reviewer:"external-reviewer",report_digest:"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",manifest_id:"github-first-dual-lane-2026-09-19",snapshot_id:.snapshot_id,attested_at_utc:"2026-09-20T00:00:00Z"}
' "$fx/g6-dual-evidence.json" > "$fx/g7-self-review-evidence.json"

printf '%s\n' "generated G6 fixtures in $fx"

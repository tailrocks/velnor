#!/usr/bin/env bash
set -euo pipefail

checker=${1:?path to detached velnor-tools binary}
fixture_dir=${2:?fixture directory}
out_dir=${3:?result directory}
mkdir -p "$out_dir"
: > "$out_dir/results.ndjson"

run_case() {
  local name=$1 command_name=$2 stage=$3 manifest=$4 snapshot=$5 evidence=$6
  local release=${7:-}
  local stdout_file="$out_dir/$name.stdout"
  local stderr_file="$out_dir/$name.stderr"
  local exit_code
  local -a args=("$command_name" --stage "$stage" --manifest "$manifest" --snapshot "$snapshot" --evidence "$evidence" --json)
  if [[ -n "$release" ]]; then
    args+=(--release-manifest "$release")
  fi
  set +e
  "$checker" "${args[@]}" >"$stdout_file" 2>"$stderr_file"
  exit_code=$?
  set -e
  if jq -e . "$stdout_file" >/dev/null 2>&1; then
    jq -c --arg case "$name" --argjson exit_code "$exit_code" \
      '{case:$case,exit_code:$exit_code,status,findings:(.findings|length),codes:(.findings|map(.code)|unique)}' \
      "$stdout_file" >> "$out_dir/results.ndjson"
  else
    jq -cn --arg case "$name" --argjson exit_code "$exit_code" \
      --rawfile error "$stderr_file" \
      '{case:$case,exit_code:$exit_code,status:"parse-error",error:($error|split("\n")[0])}' \
      >> "$out_dir/results.ndjson"
  fi
}

g1m="$fixture_dir/g1-manifest.json"
g1s="$fixture_dir/g1-snapshot.json"
g1e="$fixture_dir/g1-evidence.json"
placeholder="$fixture_dir/canonical-release-placeholder.json"

# Positive controls and distinct G1/G2/G4/G5 cases. Historical 34 cases are
# deliberately not called here.
run_case extension-positive-g1 evidence-check G1 "$g1m" "$g1s" "$g1e"
run_case g1-two-job-positive evidence-check G1 "$fixture_dir/g1-two-job-manifest.json" "$fixture_dir/g1-two-job-snapshot.json" "$fixture_dir/g1-two-job-evidence.json"
run_case g1-two-job-record-omits-b evidence-check G1 "$fixture_dir/g1-two-job-manifest.json" "$fixture_dir/g1-two-job-snapshot.json" "$fixture_dir/g1-two-job-omitted-record.json"
run_case g1-child-complete-positive evidence-check G1 "$fixture_dir/g1-child-manifest.json" "$fixture_dir/g1-child-complete-snapshot.json" "$fixture_dir/g1-child-complete-evidence.json"
run_case g1-child-obligation-erased evidence-check G1 "$fixture_dir/g1-child-manifest.json" "$fixture_dir/g1-child-complete-snapshot.json" "$fixture_dir/g1-child-obligation-erased-evidence.json"
run_case g1-stale-self-matching evidence-check G1 "$g1m" "$fixture_dir/g1-stale-self-matching-snapshot.json" "$fixture_dir/g1-stale-self-matching-evidence.json"
run_case g1-queued-run evidence-check G1 "$g1m" "$fixture_dir/g1-queued-run-snapshot.json" "$fixture_dir/g1-queued-run-evidence.json"
run_case g1-cancelled-run evidence-check G1 "$g1m" "$fixture_dir/g1-cancelled-run-snapshot.json" "$fixture_dir/g1-cancelled-run-evidence.json"
run_case g1-unknown-status evidence-check G1 "$g1m" "$fixture_dir/g1-unknown-status-snapshot.json" "$fixture_dir/g1-unknown-status-evidence.json"
run_case g1-unknown-trust evidence-check G1 "$g1m" "$fixture_dir/g1-unknown-trust-snapshot.json" "$fixture_dir/g1-unknown-trust-evidence.json"
run_case g1-workflow-run-producer evidence-check G1 "$g1m" "$fixture_dir/g1-workflow-run-producer-snapshot.json" "$fixture_dir/g1-workflow-run-producer-evidence.json"
run_case g4-no-hosted-counterpart evidence-check G4 "$fixture_dir/g4-no-hosted-counterpart-manifest.json" "$fixture_dir/g4-no-hosted-counterpart-snapshot.json" "$fixture_dir/g4-no-hosted-counterpart-evidence.json" "$placeholder"
run_case g5-no-hosted-counterpart evidence-check G5 "$fixture_dir/g4-no-hosted-counterpart-manifest.json" "$fixture_dir/g4-no-hosted-counterpart-snapshot.json" "$fixture_dir/g5-no-hosted-counterpart-evidence.json" "$placeholder"
run_case g2-applicable-release-install-waiver evidence-check G2 "$fixture_dir/g2-waived-applicable-manifest.json" "$g1s" "$fixture_dir/g2-waived-applicable-evidence.json" "$placeholder"
run_case g1-pin-mismatch evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-pin-mismatch-evidence.json"
run_case g1-pin-omission evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-pin-omission-evidence.json"
run_case g1-role-coverage-missing-pr evidence-check G1 "$g1m" "$fixture_dir/g1-role-coverage-missing-pr-snapshot.json" "$fixture_dir/g1-role-coverage-missing-pr-evidence.json"
run_case g3-role-coverage-missing-pr evidence-check G3 "$g1m" "$fixture_dir/g3-role-coverage-missing-pr-snapshot.json" "$fixture_dir/g3-role-coverage-missing-pr-evidence.json" "$placeholder"
run_case g1-pr-positive evidence-check G1 "$g1m" "$fixture_dir/g1-pr-positive-snapshot.json" "$fixture_dir/g1-pr-positive-evidence.json"
run_case g1-pr-missing-postmerge evidence-check G1 "$g1m" "$fixture_dir/g1-pr-missing-postmerge-snapshot.json" "$fixture_dir/g1-pr-missing-postmerge-evidence.json"
run_case g1-pr-wrong-head evidence-check G1 "$g1m" "$fixture_dir/g1-pr-positive-snapshot.json" "$fixture_dir/g1-pr-wrong-head-evidence.json"
run_case g1-pr-wrong-base evidence-check G1 "$g1m" "$fixture_dir/g1-pr-positive-snapshot.json" "$fixture_dir/g1-pr-wrong-base-evidence.json"
run_case g1-pr-wrong-merge evidence-check G1 "$g1m" "$fixture_dir/g1-pr-positive-snapshot.json" "$fixture_dir/g1-pr-wrong-merge-evidence.json"
run_case g1-unassociated-manual-success evidence-check G1 "$g1m" "$fixture_dir/g1-unassociated-manual-success-snapshot.json" "$fixture_dir/g1-unassociated-manual-success-evidence.json"
run_case g6-dual-positive evidence-check G6 "$fixture_dir/g6-dual-manifest.json" "$fixture_dir/g6-dual-snapshot.json" "$fixture_dir/g6-dual-evidence.json" "$placeholder"
run_case g6-lane-source-mismatch evidence-check G6 "$fixture_dir/g6-dual-manifest.json" "$fixture_dir/g6-lane-source-mismatch-snapshot.json" "$fixture_dir/g6-lane-source-mismatch-evidence.json" "$placeholder"
run_case g6-lane-workload-mismatch evidence-check G6 "$fixture_dir/g6-dual-manifest.json" "$fixture_dir/g6-lane-workload-mismatch-snapshot.json" "$fixture_dir/g6-lane-workload-mismatch-evidence.json" "$placeholder"
run_case g6-duplicate-publisher evidence-check G6 "$fixture_dir/g6-duplicate-publisher-manifest.json" "$fixture_dir/g6-dual-snapshot.json" "$fixture_dir/g6-duplicate-publisher-evidence.json" "$placeholder"
run_case g6-role-coverage-missing-pr evidence-check G6 "$fixture_dir/g6-dual-manifest.json" "$fixture_dir/g6-role-coverage-missing-pr-snapshot.json" "$fixture_dir/g6-role-coverage-missing-pr-evidence.json" "$placeholder"
run_case g7-self-review evidence-check G7 "$fixture_dir/g6-dual-manifest.json" "$fixture_dir/g6-dual-snapshot.json" "$fixture_dir/g7-self-review-evidence.json" "$placeholder"

cat "$out_dir/results.ndjson"

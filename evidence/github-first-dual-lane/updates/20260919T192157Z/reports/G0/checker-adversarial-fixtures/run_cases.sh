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

g0m="$fixture_dir/g0-manifest.json"
g0s="$fixture_dir/g0-snapshot.json"
g0e="$fixture_dir/g0-evidence.json"
g1m="$fixture_dir/g1-manifest.json"
g1s="$fixture_dir/g1-snapshot.json"
g1e="$fixture_dir/g1-evidence.json"
placeholder="$fixture_dir/canonical-release-placeholder.json"

run_case positive-g0 evidence-check G0 "$g0m" "$g0s" "$g0e"
run_case g0-blocker-next-action evidence-check G0 "$g0m" "$g0s" "$fixture_dir/g0-blocker-next-action.json"
run_case g0-third-provider-manifest evidence-check G0 "$fixture_dir/g0-third-provider-manifest.json" "$g0s" "$g0e"
run_case g0-third-provider-record evidence-check G0 "$g0m" "$g0s" "$fixture_dir/g0-third-provider-record.json"
run_case g0-substituted-name evidence-check G0 "$fixture_dir/g0-substituted-name.json" "$g0s" "$g0e"
run_case positive-g1 evidence-check G1 "$g1m" "$g1s" "$g1e"
run_case g1-empty-logs evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-empty-logs.json"
run_case g1-malformed-logs evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-malformed-logs.json"
run_case g1-expected-job-binding evidence-check G1 "$g1m" "$fixture_dir/g1-renamed-job-snapshot.json" "$g1e"
run_case g1-extra-job-id evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-extra-job-id.json"
run_case g1-duplicate-job-id evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-duplicate-job-id.json"
run_case g1-required-child-omitted evidence-check G1 "$fixture_dir/g1-required-child-omitted.json.manifest" "$g1s" "$g1e"
run_case g1-manual-dispatch evidence-check G1 "$g1m" "$fixture_dir/g1-manual-dispatch-snapshot.json" "$fixture_dir/g1-manual-dispatch-evidence.json"
run_case g1-github-fake-velnor-host evidence-check G1 "$g1m" "$fixture_dir/g1-github-fake-velnor-host-snapshot.json" "$fixture_dir/g1-github-fake-velnor-host-evidence.json"
run_case g1-evil-runurl evidence-check G1 "$g1m" "$fixture_dir/g1-evil-runurl-snapshot.json" "$fixture_dir/g1-evil-runurl-evidence.json"
run_case g1-evil-log evidence-check G1 "$g1m" "$g1s" "$fixture_dir/g1-evil-log-evidence.json"
run_case g1-evil-check evidence-check G1 "$g1m" "$fixture_dir/g1-evil-check-snapshot.json" "$fixture_dir/g1-evil-check-evidence.json"
run_case g1-third-provider-map evidence-check G1 "$fixture_dir/g1-third-provider-manifest.json" "$g1s" "$fixture_dir/g1-third-provider-evidence.json"
run_case g1-child-evil-provider evidence-check G1 "$fixture_dir/g1-child-manifest.json" "$fixture_dir/g1-child-evilprovider-snapshot.json" "$fixture_dir/g1-child-evilprovider-evidence.json"
run_case g4-fake-host evidence-check G4 "$fixture_dir/g4-fakehost-manifest.json" "$fixture_dir/g4-fakehost-snapshot.json" "$fixture_dir/g4-fakehost-evidence-release-shape.json" "$placeholder"
run_case g5-fake-host evidence-check G5 "$fixture_dir/g4-fakehost-manifest.json" "$fixture_dir/g4-fakehost-snapshot.json" "$fixture_dir/g5-fakehost-evidence.json" "$placeholder"

for command_name in check-evidence evidence-verify verify-evidence; do
  run_case "alias-$command_name" "$command_name" G0 "$g0m" "$g0s" "$g0e"
done

for input_name in g0-unknown-manifest-field g0-unknown-snapshot-field g0-unknown-evidence-field g0-old-flat-fleet g0-uppercase-applicability g0-uppercase-eligibility g1-flat-release-alias g1-flat-release-alias-different g1-flat-install-alias g1-flat-install-alias-different; do
  case "$input_name" in
    g0-unknown-manifest-field|g0-old-flat-fleet|g0-uppercase-applicability|g0-uppercase-eligibility)
      run_case "$input_name" evidence-check G0 "$fixture_dir/$input_name.json" "$g0s" "$g0e" ;;
    g0-unknown-snapshot-field)
      run_case "$input_name" evidence-check G0 "$g0m" "$fixture_dir/$input_name.json" "$g0e" ;;
    g0-unknown-evidence-field)
      run_case "$input_name" evidence-check G0 "$g0m" "$g0s" "$fixture_dir/$input_name.json" ;;
    *)
      run_case "$input_name" evidence-check G1 "$g1m" "$g1s" "$fixture_dir/$input_name.json" ;;
  esac
done

cat "$out_dir/results.ndjson"

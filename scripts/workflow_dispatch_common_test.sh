#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/workflow_dispatch_common.sh"

state_file="$(mktemp)"
args_file="$(mktemp)"
trap 'rm -f "$state_file" "$args_file"' EXIT
printf '0' >"$state_file"

gh() {
  if [[ "$1 $2" == "run list" ]]; then
    local calls
    calls="$(cat "$state_file")"
    calls=$((calls + 1))
    printf '%s' "$calls" >"$state_file"
    if [[ "$calls" -eq 1 ]]; then
      printf '%s\n' 100
    else
      printf '%s\n%s\n' 101 100
    fi
    return 0
  fi

  if [[ "$1 $2" == "workflow run" ]]; then
    printf '%s\n' "$*" >"$args_file"
    echo "created workflow_dispatch event"
    return 0
  fi

  echo "unexpected gh call: $*" >&2
  return 2
}

sleep() {
  :
}

run_id="$(dispatch_workflow_and_wait_run_id owner/repo ci.yml main 'package=a,push=false')"
run_args="$(cat "$args_file")"

if [[ "$run_id" != "101" ]]; then
  echo "expected new run id 101, got '$run_id'" >&2
  exit 1
fi

if [[ "$run_args" != *"workflow run ci.yml --repo owner/repo --ref main -f package=a -f push=false"* ]]; then
  echo "workflow dispatch args did not preserve ref and inputs: $run_args" >&2
  exit 1
fi

echo "workflow dispatch helper self-test passed"

#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/workflow_dispatch_common.sh"

state_file="$(mktemp)"
calls_file="$(mktemp)"
trap 'rm -f "$state_file" "$calls_file"' EXIT
printf '0' >"$state_file"

gh() {
  printf '%s\n' "$*" >>"$calls_file"

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
    echo "created workflow_dispatch event"
    return 0
  fi

  echo "unexpected gh call: $*" >&2
  return 2
}

sleep() {
  :
}

reset_mock() {
  printf '0' >"$state_file"
  : >"$calls_file"
}

assert_branch_ref_dispatch() {
  local run_id calls
  reset_mock

  run_id="$(dispatch_workflow_and_wait_run_id owner/repo ci.yml main 'package=a,push=false')"
  calls="$(cat "$calls_file")"

  if [[ "$run_id" != "101" ]]; then
    echo "expected new run id 101, got '$run_id'" >&2
    exit 1
  fi

  if [[ "$calls" != *"workflow run ci.yml --repo owner/repo --ref main -f package=a -f push=false"* ]]; then
    echo "workflow dispatch args did not preserve ref and inputs: $calls" >&2
    exit 1
  fi

  if [[ "$calls" != *"run list --repo owner/repo --workflow ci.yml --event workflow_dispatch --limit 20 --json databaseId --branch main"* ]]; then
    echo "branch ref did not filter run lookup by branch: $calls" >&2
    exit 1
  fi
}

assert_sha_ref_dispatch() {
  local sha run_id calls
  sha="0123456789abcdef0123456789abcdef01234567"
  reset_mock

  run_id="$(dispatch_workflow_and_wait_run_id owner/repo ci.yml "$sha" 'package=a')"
  calls="$(cat "$calls_file")"

  if [[ "$run_id" != "101" ]]; then
    echo "expected new run id 101 for SHA ref, got '$run_id'" >&2
    exit 1
  fi

  if [[ "$calls" != *"workflow run ci.yml --repo owner/repo --ref $sha -f package=a"* ]]; then
    echo "workflow dispatch args did not preserve SHA ref: $calls" >&2
    exit 1
  fi

  if [[ "$calls" == *"--branch $sha"* ]]; then
    echo "SHA ref should not be passed to gh run list --branch: $calls" >&2
    exit 1
  fi
}

assert_branch_ref_dispatch
assert_sha_ref_dispatch

echo "workflow dispatch helper self-test passed"

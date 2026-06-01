#!/usr/bin/env bash
set -euo pipefail

fixture_smoke_dispatch_default() {
  local dispatch="${1:-}"
  local run_id="${2:-}"

  if [[ -z "$dispatch" ]]; then
    if [[ -n "$run_id" ]]; then
      dispatch=false
    else
      dispatch=true
    fi
  fi

  if [[ "$dispatch" != "true" && "$dispatch" != "false" ]]; then
    return 2
  fi

  if [[ "$dispatch" == "false" && -z "$run_id" ]]; then
    return 3
  fi

  printf '%s\n' "$dispatch"
}

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"

  if [[ "$actual" != "$expected" ]]; then
    echo "$label: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

assert_eq "$(fixture_smoke_dispatch_default "" "")" "true" "fresh run default"
assert_eq "$(fixture_smoke_dispatch_default "" "123")" "false" "existing run default"
assert_eq "$(fixture_smoke_dispatch_default "true" "123")" "true" "explicit dispatch wins"

if fixture_smoke_dispatch_default "false" "" >/dev/null; then
  echo "expected explicit no-dispatch without run id to fail" >&2
  exit 1
fi

if fixture_smoke_dispatch_default "maybe" "123" >/dev/null; then
  echo "expected invalid dispatch value to fail" >&2
  exit 1
fi

echo "fixture smoke defaults self-test passed"

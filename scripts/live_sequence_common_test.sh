#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/live_sequence_common.sh"

assert_passes() {
  local label="$1"
  shift

  if ! "$@"; then
    echo "$label: expected success" >&2
    exit 1
  fi
}

assert_fails() {
  local label="$1"
  shift

  if "$@" >/dev/null 2>&1; then
    echo "$label: expected failure" >&2
    exit 1
  fi
}

assert_passes "true bool" velnor_require_bool TEST_BOOL true
assert_passes "false bool" velnor_require_bool TEST_BOOL false
assert_fails "mixed-case bool" velnor_require_bool TEST_BOOL True
assert_fails "empty bool" velnor_require_bool TEST_BOOL ""
assert_fails "word bool" velnor_require_bool TEST_BOOL yes

assert_passes "one job" velnor_require_positive_int TEST_COUNT 1
assert_passes "many jobs" velnor_require_positive_int TEST_COUNT 42
assert_fails "zero jobs" velnor_require_positive_int TEST_COUNT 0
assert_fails "negative jobs" velnor_require_positive_int TEST_COUNT -1
assert_fails "non-number jobs" velnor_require_positive_int TEST_COUNT two

echo "live sequence helper self-test passed"

#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

assert_failure_evidence_trap() {
  local script="$1"
  local label="$2"

  if ! grep -q 'record_failure_evidence()' "$script"; then
    echo "$label smoke script is missing record_failure_evidence" >&2
    exit 1
  fi

  if ! grep -q 'write_live_evidence "failed-before-completion"' "$script"; then
    echo "$label smoke script does not write failed-before-completion evidence" >&2
    exit 1
  fi

  if ! grep -q 'trap record_failure_evidence ERR' "$script"; then
    echo "$label smoke script does not trap ERR for failure evidence" >&2
    exit 1
  fi

  if ! grep -q 'trap cleanup_runner EXIT' "$script"; then
    echo "$label smoke script does not preserve cleanup_runner EXIT trap" >&2
    exit 1
  fi
}

assert_failure_evidence_trap "$ROOT/scripts/fixture_smoke.sh" "fixture"
assert_failure_evidence_trap "$ROOT/scripts/target_smoke_common.sh" "target"

echo "smoke failure evidence self-test passed"

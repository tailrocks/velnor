#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK_STATUS="${VELNOR_FIXTURE_READINESS_CHECK_STATUS:-true}"
CHECK_AUDIT="${VELNOR_FIXTURE_READINESS_CHECK_AUDIT:-true}"
RUN_LOCAL_TESTS="${VELNOR_FIXTURE_READINESS_RUN_LOCAL_TESTS:-false}"
FIXTURE_STATUS_SCRIPT="${VELNOR_FIXTURE_STATUS_SCRIPT:-scripts/fixture_status.sh}"
FIXTURE_AUDIT_SCRIPT="${VELNOR_FIXTURE_AUDIT_SCRIPT:-scripts/fixture_audit.py}"
LIVE_HOST_DOCTOR_SCRIPT="${VELNOR_LIVE_HOST_DOCTOR_SCRIPT:-scripts/live_host_doctor.sh}"

source "$ROOT/scripts/live_sequence_common.sh"

velnor_require_bool VELNOR_FIXTURE_READINESS_CHECK_STATUS "$CHECK_STATUS"
velnor_require_bool VELNOR_FIXTURE_READINESS_CHECK_AUDIT "$CHECK_AUDIT"
velnor_require_bool VELNOR_FIXTURE_READINESS_RUN_LOCAL_TESTS "$RUN_LOCAL_TESTS"

cd "$ROOT"

on_readiness_error() {
  local status=$?
  echo "fixture readiness failed; run scripts/fixture_report.sh for a non-mutating markdown handoff report" >&2
  exit "$status"
}
trap on_readiness_error ERR

echo "==> Checking fixture proof readiness"
echo "This script does not register runners or dispatch workflows."

if [[ "$CHECK_STATUS" == "true" ]]; then
  echo "==> Inspecting fixture workflow status"
  "$FIXTURE_STATUS_SCRIPT"
fi

if [[ "$CHECK_AUDIT" == "true" ]]; then
  echo "==> Auditing fixture feature surface"
  "$FIXTURE_AUDIT_SCRIPT"
fi

if [[ "$RUN_LOCAL_TESTS" == "true" ]]; then
  echo "==> Running local target verifier"
  scripts/target_verify.sh

  echo "==> Running Rust test suite"
  cargo test -q
fi

echo "==> Checking live host readiness"
"$LIVE_HOST_DOCTOR_SCRIPT"

echo "Fixture readiness passed. It is safe to attempt scripts/fixture_smoke.sh on this host."

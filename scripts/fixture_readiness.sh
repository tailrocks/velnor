#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

args=()
if [[ -n "${VELNOR_FIXTURE_READINESS_CHECK_STATUS:-}" ]]; then
  args+=(--check-status "$VELNOR_FIXTURE_READINESS_CHECK_STATUS")
fi
if [[ -n "${VELNOR_FIXTURE_READINESS_CHECK_AUDIT:-}" ]]; then
  args+=(--check-audit "$VELNOR_FIXTURE_READINESS_CHECK_AUDIT")
fi
if [[ -n "${VELNOR_FIXTURE_READINESS_RUN_LOCAL_TESTS:-}" ]]; then
  args+=(--run-local-tests "$VELNOR_FIXTURE_READINESS_RUN_LOCAL_TESTS")
fi
if [[ -n "${VELNOR_FIXTURE_STATUS_SCRIPT:-}" ]]; then
  args+=(--fixture-status-script "$VELNOR_FIXTURE_STATUS_SCRIPT")
fi
if [[ -n "${VELNOR_FIXTURE_AUDIT_SCRIPT:-}" ]]; then
  args+=(--fixture-audit-script "$VELNOR_FIXTURE_AUDIT_SCRIPT")
fi
if [[ -n "${VELNOR_LIVE_HOST_DOCTOR_SCRIPT:-}" ]]; then
  args+=(--live-host-doctor-script "$VELNOR_LIVE_HOST_DOCTOR_SCRIPT")
fi

if [[ "${#args[@]}" -eq 0 ]]; then
  cargo run -q -p velnor-tools -- fixture-readiness
else
  cargo run -q -p velnor-tools -- fixture-readiness "${args[@]}"
fi

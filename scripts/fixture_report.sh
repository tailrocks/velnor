#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

args=()
if [[ -n "${VELNOR_FIXTURE_REPORT_DIR:-}" ]]; then
  args+=(--report-dir "$VELNOR_FIXTURE_REPORT_DIR")
fi
if [[ -n "${VELNOR_FIXTURE_REPORT_PATH:-}" ]]; then
  args+=(--report-path "$VELNOR_FIXTURE_REPORT_PATH")
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
  cargo run -q -p velnor-tools -- fixture-report
else
  cargo run -q -p velnor-tools -- fixture-report "${args[@]}"
fi

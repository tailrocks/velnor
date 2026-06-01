#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${VELNOR_FIXTURE_REPORT_DIR:-$ROOT/.velnor-live-evidence}"
REPORT_PATH="${VELNOR_FIXTURE_REPORT_PATH:-}"
FIXTURE_STATUS_SCRIPT="${VELNOR_FIXTURE_STATUS_SCRIPT:-scripts/fixture_status.sh}"
FIXTURE_AUDIT_SCRIPT="${VELNOR_FIXTURE_AUDIT_SCRIPT:-scripts/fixture_audit.py}"
LIVE_HOST_DOCTOR_SCRIPT="${VELNOR_LIVE_HOST_DOCTOR_SCRIPT:-scripts/live_host_doctor.sh}"

cd "$ROOT"

if [[ -z "$REPORT_PATH" ]]; then
  mkdir -p "$REPORT_DIR"
  REPORT_PATH="$REPORT_DIR/fixture-readiness-report.md"
else
  mkdir -p "$(dirname "$REPORT_PATH")"
fi

run_report_section() {
  local title="$1"
  shift
  local output status

  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e

  {
    echo "## $title"
    echo
    echo "- status: $status"
    echo
    echo '```text'
    printf '%s\n' "$output"
    echo '```'
    echo
  } >>"$REPORT_PATH"

  return "$status"
}

{
  echo "# Velnor Fixture Readiness Report"
  echo
  echo "This report does not register runners or dispatch workflows."
  echo
  echo "- generated_at_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- velnor_commit: $(git rev-parse HEAD 2>/dev/null || printf '<unavailable>')"
  echo "- velnor_branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null || printf '<unavailable>')"
  echo "- velnor_dirty_files: $(git status --short 2>/dev/null | wc -l | tr -d ' ' || printf 'unknown')"
  echo "- fixture_repo: ${VELNOR_FIXTURE_REPO:-donbeave/velnor-actions-fixture}"
  echo "- fixture_workflow: ${VELNOR_FIXTURE_WORKFLOW:-compat.yml}"
  echo "- docker_host: ${DOCKER_HOST:-<local>}"
  echo
} >"$REPORT_PATH"

overall=0
run_report_section "Fixture Workflow Status" "$FIXTURE_STATUS_SCRIPT" || overall=1
run_report_section "Fixture Feature Audit" "$FIXTURE_AUDIT_SCRIPT" || overall=1
run_report_section "Live Host Readiness" "$LIVE_HOST_DOCTOR_SCRIPT" || overall=1

{
  echo "## Next Action"
  echo
  if [[ "$overall" -eq 0 ]]; then
    echo "Fixture readiness passed. Run \`scripts/fixture_smoke.sh\` on this host."
  else
    echo "Fixture readiness has blockers. Fix the failing section above before running \`scripts/fixture_smoke.sh\`."
    echo
    echo "Do not register Velnor or dispatch real target repository workflows from this report alone."
  fi
  echo
} >>"$REPORT_PATH"

if [[ "$overall" -eq 0 ]]; then
  echo "Fixture report passed: $REPORT_PATH"
else
  echo "Fixture report found blockers: $REPORT_PATH" >&2
fi

exit "$overall"

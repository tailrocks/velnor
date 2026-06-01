#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

WATCH_RUN="${VELNOR_JACKIN_SEQUENCE_WATCH_RUN:-true}"
INCLUDE_CONSTRUCT="${VELNOR_JACKIN_SEQUENCE_INCLUDE_CONSTRUCT:-true}"
INCLUDE_DOCS="${VELNOR_JACKIN_SEQUENCE_INCLUDE_DOCS:-true}"

run_jackin_smoke() {
  local workflow="$1"
  local job_count="$2"

  echo "==> Jackin Rust/Linux target sequence: $workflow ($job_count job(s))"
  env \
    VELNOR_TARGET_WORKFLOW="$workflow" \
    VELNOR_TARGET_JOB_COUNT="$job_count" \
    VELNOR_TARGET_WATCH_RUN="$WATCH_RUN" \
    "$ROOT/scripts/jackin_target_smoke.sh"
}

run_jackin_smoke ci.yml "${VELNOR_JACKIN_CI_JOB_COUNT:-5}"

if [[ "$INCLUDE_CONSTRUCT" == "true" ]]; then
  run_jackin_smoke construct.yml "${VELNOR_JACKIN_CONSTRUCT_JOB_COUNT:-5}"
fi

if [[ "$INCLUDE_DOCS" == "true" ]]; then
  run_jackin_smoke docs.yml "${VELNOR_JACKIN_DOCS_JOB_COUNT:-5}"
fi

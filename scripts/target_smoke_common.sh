#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="${VELNOR_TARGET_REPO:?VELNOR_TARGET_REPO is required}"
TARGET_URL="${VELNOR_TARGET_URL:-https://github.com/$TARGET_REPO}"
RUNNER_NAME="${VELNOR_RUNNER_NAME:-velnor-target-mvp}"
WORK_DIR="${VELNOR_WORK_DIR:-$ROOT/.velnor-work}"
DOCKER_HOST_WORK_DIR="${VELNOR_DOCKER_HOST_WORK_DIR:-}"
REQUIRE_DOCKER_SOCKET="${VELNOR_REQUIRE_DOCKER_SOCKET:-true}"
IDLE_TIMEOUT_SECONDS="${VELNOR_IDLE_TIMEOUT_SECONDS:-900}"
CLEANUP_RUNNER="${VELNOR_TARGET_CLEANUP_RUNNER:-false}"
DUMP_JOB_MESSAGES="${VELNOR_DUMP_JOB_MESSAGES:-$ROOT/.velnor-job-dumps/target}"
JOB_COUNT="${VELNOR_TARGET_JOB_COUNT:-1}"
WORKFLOW="${VELNOR_TARGET_WORKFLOW:-}"
TARGET_REF="${VELNOR_TARGET_REF:-}"
TARGET_INPUTS="${VELNOR_TARGET_INPUTS:-}"
RUN_ID="${VELNOR_TARGET_RUN_ID:-}"
WATCH_RUN="${VELNOR_TARGET_WATCH_RUN:-false}"
TARGET_LABEL="${VELNOR_TARGET_LABEL:-target}"
TARGET_MVP_ARM_LABEL="${VELNOR_TARGET_MVP_ARM_LABEL:-false}"
REGISTERED_RUNNER=false
LIVE_EVIDENCE_TITLE="Target"
LIVE_EVIDENCE_REPO="$TARGET_REPO"
LIVE_EVIDENCE_WORKFLOW="${WORKFLOW:-existing-run}"
LIVE_EVIDENCE_REF="${TARGET_REF:-<default>}"
LIVE_EVIDENCE_INPUTS="${TARGET_INPUTS:-<none>}"

live_evidence_extra_metadata() {
  echo "- target label: $TARGET_LABEL"
  echo "- target MVP ARM label: $TARGET_MVP_ARM_LABEL"
}

source "$ROOT/scripts/live_evidence_common.sh"
source "$ROOT/scripts/live_sequence_common.sh"
source "$ROOT/scripts/workflow_dispatch_common.sh"

cleanup_runner() {
  if [[ "$REGISTERED_RUNNER" == "true" && "$CLEANUP_RUNNER" == "true" ]]; then
    echo "==> Removing target runner"
    cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" || true
  fi
}

trap cleanup_runner EXIT

velnor_require_positive_int VELNOR_TARGET_JOB_COUNT "$JOB_COUNT"
velnor_require_positive_int VELNOR_IDLE_TIMEOUT_SECONDS "$IDLE_TIMEOUT_SECONDS"
velnor_require_optional_positive_int VELNOR_TARGET_RUN_ID "$RUN_ID"
velnor_require_bool VELNOR_REQUIRE_DOCKER_SOCKET "$REQUIRE_DOCKER_SOCKET"
velnor_require_bool VELNOR_TARGET_CLEANUP_RUNNER "$CLEANUP_RUNNER"
velnor_require_bool VELNOR_TARGET_WATCH_RUN "$WATCH_RUN"
velnor_require_bool VELNOR_TARGET_MVP_ARM_LABEL "$TARGET_MVP_ARM_LABEL"
velnor_require_live_evidence_controls
validate_workflow_dispatch_inputs "$TARGET_INPUTS"

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  echo "GITHUB_TOKEN is required to register the target self-hosted runner." >&2
  exit 2
fi

if [[ -n "$WORKFLOW" || -n "$RUN_ID" ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "GitHub CLI 'gh' is required when VELNOR_TARGET_WORKFLOW or VELNOR_TARGET_RUN_ID is set." >&2
    exit 2
  fi
fi

cd "$ROOT"

run_args=(--work-dir "$WORK_DIR")
if [[ -n "$DUMP_JOB_MESSAGES" ]]; then
  run_args+=(--dump-job-message "$DUMP_JOB_MESSAGES")
fi
if [[ -n "$DOCKER_HOST_WORK_DIR" ]]; then
  run_args+=(--docker-host-work-dir "$DOCKER_HOST_WORK_DIR")
fi
if [[ "$REQUIRE_DOCKER_SOCKET" == "true" ]]; then
  run_args+=(--require-docker-socket)
fi

echo "==> Checking live host readiness"
VELNOR_CHECK_TARGET_MVP_CONFIG=false scripts/live_host_doctor.sh

echo "==> Registering $TARGET_LABEL target runner"
configure_args=(
  --url "$TARGET_URL"
  --pat "$GITHUB_TOKEN"
  --name "$RUNNER_NAME"
  --target-mvp-labels
  --replace
)
if [[ "$TARGET_MVP_ARM_LABEL" == "true" ]]; then
  configure_args+=(--target-mvp-arm-label)
fi

cargo run --bin velnor-runner -- configure \
  "${configure_args[@]}"
REGISTERED_RUNNER=true

echo "==> Checking target MVP runner config"
cargo run --bin velnor-runner -- status --check-target-mvp

if [[ -n "$WORKFLOW" ]]; then
  echo "==> Dispatching target workflow $WORKFLOW"
  echo "==> Waiting for dispatched run to appear"
  if ! RUN_ID="$(dispatch_workflow_and_wait_run_id "$TARGET_REPO" "$WORKFLOW" "$TARGET_REF" "$TARGET_INPUTS")"; then
    echo "Timed out waiting for dispatched target workflow run." >&2
    exit 1
  fi
fi

if [[ -n "$RUN_ID" ]]; then
  echo "==> Target run before Velnor"
  gh run view "$RUN_ID" --repo "$TARGET_REPO" \
    --json status,conclusion,jobs,url \
    --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'
fi

echo "==> Running $JOB_COUNT $TARGET_LABEL target job(s)"
for job_index in $(seq 1 "$JOB_COUNT"); do
  echo "==> Velnor $TARGET_LABEL target job $job_index/$JOB_COUNT"
  cargo run --bin velnor-runner -- run \
    "${run_args[@]}" \
    --once \
    --idle-timeout-seconds "$IDLE_TIMEOUT_SECONDS"
done

if [[ -n "$RUN_ID" ]]; then
  echo "==> Target run after Velnor"
  show_github_run_status
  write_live_evidence "after-velnor"
  if [[ "$WATCH_RUN" == "true" ]]; then
    echo "==> Waiting for target run completion"
    if gh run watch "$RUN_ID" --repo "$TARGET_REPO" --exit-status; then
      write_live_evidence "completed"
    else
      watch_status=$?
      write_live_evidence "completed-with-failure"
      exit "$watch_status"
    fi
  fi
fi

echo "$TARGET_LABEL target smoke job completed."

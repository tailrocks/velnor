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
TARGET_LABEL="${VELNOR_TARGET_LABEL:-target}"
TARGET_MVP_ARM_LABEL="${VELNOR_TARGET_MVP_ARM_LABEL:-false}"
REGISTERED_RUNNER=false

cleanup_runner() {
  if [[ "$REGISTERED_RUNNER" == "true" && "$CLEANUP_RUNNER" == "true" ]]; then
    echo "==> Removing target runner"
    cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" || true
  fi
}

trap cleanup_runner EXIT

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
  workflow_run_args=("$WORKFLOW" --repo "$TARGET_REPO")
  if [[ -n "$TARGET_REF" ]]; then
    workflow_run_args+=(--ref "$TARGET_REF")
  fi
  if [[ -n "$TARGET_INPUTS" ]]; then
    IFS=',' read -r -a target_inputs <<<"$TARGET_INPUTS"
    for input in "${target_inputs[@]}"; do
      workflow_run_args+=(-f "$input")
    done
  fi
  gh workflow run "${workflow_run_args[@]}"
  echo "==> Waiting for dispatched run to appear"
  for _ in $(seq 1 30); do
    run_list_args=(--repo "$TARGET_REPO" --workflow "$WORKFLOW" --event workflow_dispatch --limit 1 --json databaseId)
    if [[ -n "$TARGET_REF" ]]; then
      run_list_args+=(--branch "$TARGET_REF")
    fi
    RUN_ID="$(gh run list "${run_list_args[@]}" --jq '.[0].databaseId // ""')"
    if [[ -n "$RUN_ID" ]]; then
      break
    fi
    sleep 2
  done
  if [[ -z "$RUN_ID" ]]; then
    echo "Timed out waiting for dispatched target workflow run." >&2
    exit 1
  fi
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
  gh run view "$RUN_ID" --repo "$TARGET_REPO" \
    --json status,conclusion,jobs,url \
    --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'
fi

echo "$TARGET_LABEL target smoke job completed."

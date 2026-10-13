#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="${VELNOR_TARGET_REPO:-ChainArgos/java-monorepo}"
TARGET_URL="${VELNOR_TARGET_URL:-https://github.com/$TARGET_REPO}"
RUNNER_NAME="${VELNOR_RUNNER_NAME:-velnor-target-mvp}"
WORK_DIR="${VELNOR_WORK_DIR:-$ROOT/.velnor-work}"
DOCKER_HOST_WORK_DIR="${VELNOR_DOCKER_HOST_WORK_DIR:-}"
REQUIRE_DOCKER_SOCKET="${VELNOR_REQUIRE_DOCKER_SOCKET:-true}"
IDLE_TIMEOUT_SECONDS="${VELNOR_IDLE_TIMEOUT_SECONDS:-900}"
CLEANUP_RUNNER="${VELNOR_TARGET_CLEANUP_RUNNER:-false}"
DUMP_JOB_MESSAGES="${VELNOR_DUMP_JOB_MESSAGES:-$ROOT/.velnor-job-dumps/java-target}"
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

echo "==> Registering target runner"
cargo run --bin velnor-runner -- configure \
  --url "$TARGET_URL" \
  --pat "$GITHUB_TOKEN" \
  --name "$RUNNER_NAME" \
  --target-mvp-labels \
  --replace
REGISTERED_RUNNER=true

echo "==> Checking target MVP runner config"
cargo run --bin velnor-runner -- status --check-target-mvp

echo "==> Running one target job"
cargo run --bin velnor-runner -- run \
  "${run_args[@]}" \
  --once \
  --idle-timeout-seconds "$IDLE_TIMEOUT_SECONDS"

echo "Target smoke job completed."

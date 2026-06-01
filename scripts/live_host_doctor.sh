#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="${VELNOR_WORK_DIR:-$ROOT/.velnor-work}"
DOCKER_HOST_WORK_DIR="${VELNOR_DOCKER_HOST_WORK_DIR:-}"
REQUIRE_DOCKER_SOCKET="${VELNOR_REQUIRE_DOCKER_SOCKET:-true}"
CHECK_TARGET_MVP_CONFIG="${VELNOR_CHECK_TARGET_MVP_CONFIG:-false}"
RUN_TARGET_VERIFY="${VELNOR_RUN_TARGET_VERIFY:-false}"
TARGET_MVP_ARM_LABEL="${VELNOR_TARGET_MVP_ARM_LABEL:-false}"

cd "$ROOT"

host_os="$(uname -s)"
if [[ "$host_os" != "Linux" ]]; then
  echo "unsupported host OS '$host_os'; Velnor live proof scripts are Linux-only" >&2
  exit 2
fi

if [[ "$TARGET_MVP_ARM_LABEL" == "true" ]]; then
  host_arch="$(uname -m)"
  case "$host_arch" in
    aarch64|arm64) ;;
    *)
      echo "unsupported ARM runner label on host architecture '$host_arch'; only set VELNOR_TARGET_MVP_ARM_LABEL=true on an ARM Linux host" >&2
      exit 2
      ;;
  esac
fi

echo "==> Checking required host tools"
for tool in git docker cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 2
  fi
done

if [[ -n "${DOCKER_HOST:-}" ]]; then
  echo "DOCKER_HOST=$DOCKER_HOST"
  if [[ "$DOCKER_HOST" == tcp://* || "$DOCKER_HOST" == ssh://* ]]; then
    echo "remote Docker daemon detected; VELNOR_DOCKER_HOST_WORK_DIR must point at a daemon-visible mount path when local and daemon paths differ"
  fi
fi

if [[ -n "$DOCKER_HOST_WORK_DIR" ]]; then
  echo "VELNOR_DOCKER_HOST_WORK_DIR=$DOCKER_HOST_WORK_DIR"
fi

if [[ "$RUN_TARGET_VERIFY" == "true" ]]; then
  echo "==> Running target verifier"
  scripts/target_verify.sh
fi

echo "==> Checking actions/runner reference"
python3 scripts/check_runner_reference.py

echo "==> Running Docker preflight"
preflight_args=(--work-dir "$WORK_DIR")
if [[ -n "$DOCKER_HOST_WORK_DIR" ]]; then
  preflight_args+=(--docker-host-work-dir "$DOCKER_HOST_WORK_DIR")
fi
if [[ "$REQUIRE_DOCKER_SOCKET" == "true" ]]; then
  preflight_args+=(--require-docker-socket)
fi
cargo run --bin velnor-runner -- preflight "${preflight_args[@]}"

if [[ "$CHECK_TARGET_MVP_CONFIG" == "true" ]]; then
  echo "==> Checking target MVP runner config"
  cargo run --bin velnor-runner -- status --check-target-mvp
fi

echo "Live host doctor passed."

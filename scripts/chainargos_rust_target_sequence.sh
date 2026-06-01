#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/live_sequence_common.sh"

WATCH_RUN="${VELNOR_CHAINARGOS_SEQUENCE_WATCH_RUN:-true}"
RUST_PACKAGES="${VELNOR_CHAINARGOS_RUST_PACKAGES:-bitcoin-processor-app}"
DOCKER_TARGETS="${VELNOR_CHAINARGOS_DOCKER_TARGETS:-bitcoin-processor-app}"
DOCKER_PUSH="${VELNOR_CHAINARGOS_DOCKER_PUSH:-false}"
INCLUDE_DOCKER="${VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_DOCKER:-true}"
INCLUDE_KESTRA="${VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_KESTRA:-true}"

velnor_require_bool VELNOR_CHAINARGOS_SEQUENCE_WATCH_RUN "$WATCH_RUN"
velnor_require_bool VELNOR_CHAINARGOS_DOCKER_PUSH "$DOCKER_PUSH"
velnor_require_bool VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_DOCKER "$INCLUDE_DOCKER"
velnor_require_bool VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_KESTRA "$INCLUDE_KESTRA"
velnor_require_positive_int VELNOR_CHAINARGOS_ANSIBLE_JOB_COUNT "${VELNOR_CHAINARGOS_ANSIBLE_JOB_COUNT:-1}"
velnor_require_positive_int VELNOR_CHAINARGOS_RUST_JOB_COUNT "${VELNOR_CHAINARGOS_RUST_JOB_COUNT:-4}"
velnor_require_positive_int VELNOR_CHAINARGOS_RUST_DOCKER_JOB_COUNT "${VELNOR_CHAINARGOS_RUST_DOCKER_JOB_COUNT:-3}"
velnor_require_positive_int VELNOR_CHAINARGOS_KESTRA_JOB_COUNT "${VELNOR_CHAINARGOS_KESTRA_JOB_COUNT:-3}"

run_chainargos_smoke() {
  local workflow="$1"
  local job_count="$2"
  local inputs="${3:-}"

  echo "==> ChainArgos Rust target sequence: $workflow ($job_count job(s))"
  env \
    VELNOR_TARGET_WORKFLOW="$workflow" \
    VELNOR_TARGET_JOB_COUNT="$job_count" \
    VELNOR_TARGET_INPUTS="$inputs" \
    VELNOR_TARGET_WATCH_RUN="$WATCH_RUN" \
    "$ROOT/scripts/chainargos_target_smoke.sh"
}

run_chainargos_smoke ansible.yml "${VELNOR_CHAINARGOS_ANSIBLE_JOB_COUNT:-1}"
run_chainargos_smoke rust.yml "${VELNOR_CHAINARGOS_RUST_JOB_COUNT:-4}" "packages=$RUST_PACKAGES"

if [[ "$INCLUDE_DOCKER" == "true" ]]; then
  run_chainargos_smoke rust-docker.yml \
    "${VELNOR_CHAINARGOS_RUST_DOCKER_JOB_COUNT:-3}" \
    "targets=$DOCKER_TARGETS,push=$DOCKER_PUSH"
fi

if [[ "$INCLUDE_KESTRA" == "true" ]]; then
  run_chainargos_smoke kestra-build-publish.yml "${VELNOR_CHAINARGOS_KESTRA_JOB_COUNT:-3}"
fi

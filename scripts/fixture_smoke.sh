#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_REPO="${VELNOR_FIXTURE_REPO:-donbeave/velnor-actions-fixture}"
FIXTURE_URL="${VELNOR_FIXTURE_URL:-https://github.com/$FIXTURE_REPO}"
RUNNER_NAME="${VELNOR_RUNNER_NAME:-velnor-target-mvp}"
RUNNER_LABEL="${VELNOR_RUNNER_LABEL:-velnor-target-mvp}"
WORK_DIR="${VELNOR_WORK_DIR:-$ROOT/.velnor-work}"
DOCKER_HOST_WORK_DIR="${VELNOR_DOCKER_HOST_WORK_DIR:-}"
REQUIRE_DOCKER_SOCKET="${VELNOR_REQUIRE_DOCKER_SOCKET:-true}"
IDLE_TIMEOUT_SECONDS="${VELNOR_IDLE_TIMEOUT_SECONDS:-900}"
WORKFLOW="${VELNOR_FIXTURE_WORKFLOW:-compat.yml}"
DISPATCH="${VELNOR_FIXTURE_DISPATCH:-false}"
FIXTURE_REF="${VELNOR_FIXTURE_REF:-}"
FIXTURE_INPUTS="${VELNOR_FIXTURE_INPUTS:-}"
RUN_ID="${VELNOR_FIXTURE_RUN_ID:-26762850861}"
JOB_COUNT="${VELNOR_FIXTURE_JOB_COUNT:-2}"
CLEANUP_RUNNER="${VELNOR_FIXTURE_CLEANUP_RUNNER:-true}"
DUMP_JOB_MESSAGES="${VELNOR_DUMP_JOB_MESSAGES:-$ROOT/.velnor-job-dumps/fixture}"
REGISTERED_RUNNER=false
LIVE_EVIDENCE_TITLE="Fixture"
LIVE_EVIDENCE_REPO="$FIXTURE_REPO"
LIVE_EVIDENCE_WORKFLOW="$WORKFLOW"
LIVE_EVIDENCE_REF="${FIXTURE_REF:-<default>}"
LIVE_EVIDENCE_INPUTS="${FIXTURE_INPUTS:-<none>}"

live_evidence_extra_metadata() {
  echo "- runner label: $RUNNER_LABEL"
}

source "$ROOT/scripts/live_evidence_common.sh"
source "$ROOT/scripts/workflow_dispatch_common.sh"

cleanup_runner() {
  if [[ "$REGISTERED_RUNNER" == "true" && "$CLEANUP_RUNNER" == "true" ]]; then
    echo "==> Removing fixture runner"
    cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" || true
  fi
}

trap cleanup_runner EXIT

if ! [[ "$JOB_COUNT" =~ ^[1-9][0-9]*$ ]]; then
  echo "VELNOR_FIXTURE_JOB_COUNT must be a positive integer." >&2
  exit 2
fi

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  echo "GITHUB_TOKEN is required to register the fixture self-hosted runner." >&2
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "GitHub CLI 'gh' is required to inspect fixture workflow status." >&2
  exit 2
fi

cd "$ROOT"

echo "==> Checking runner source and local tests"
cargo test -q

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
scripts/live_host_doctor.sh

echo "==> Registering fixture runner"
cargo run --bin velnor-runner -- configure \
  --url "$FIXTURE_URL" \
  --pat "$GITHUB_TOKEN" \
  --name "$RUNNER_NAME" \
  --labels "$RUNNER_LABEL" \
  --replace
REGISTERED_RUNNER=true

if [[ "$DISPATCH" == "true" ]]; then
  echo "==> Dispatching fresh fixture workflow $WORKFLOW"
  echo "==> Waiting for dispatched run to appear"
  if ! RUN_ID="$(dispatch_workflow_and_wait_run_id "$FIXTURE_REPO" "$WORKFLOW" "$FIXTURE_REF" "$FIXTURE_INPUTS")"; then
    echo "Timed out waiting for dispatched fixture workflow run." >&2
    exit 1
  fi
fi

echo "==> Fixture run before Velnor"
gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'

echo "==> Running $JOB_COUNT Velnor fixture job(s)"
for job_index in $(seq 1 "$JOB_COUNT"); do
  echo "==> Velnor fixture job $job_index/$JOB_COUNT"
  cargo run --bin velnor-runner -- run \
    "${run_args[@]}" \
    --once \
    --idle-timeout-seconds "$IDLE_TIMEOUT_SECONDS"
done

echo "==> Fixture run after Velnor"
gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'
write_live_evidence "after-velnor"

echo "==> Waiting briefly for compare-results"
if gh run watch "$RUN_ID" --repo "$FIXTURE_REPO" --exit-status; then
  write_live_evidence "completed"
else
  watch_status=$?
  write_live_evidence "completed-with-failure"
  exit "$watch_status"
fi

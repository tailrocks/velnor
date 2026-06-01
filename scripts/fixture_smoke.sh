#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_REPO="${VELNOR_FIXTURE_REPO:-donbeave/velnor-actions-fixture}"
FIXTURE_URL="${VELNOR_FIXTURE_URL:-https://github.com/$FIXTURE_REPO}"
RUNNER_NAME="${VELNOR_RUNNER_NAME:-velnor-target-mvp}"
RUNNER_LABEL="${VELNOR_RUNNER_LABEL:-velnor-target-mvp}"
WORK_DIR="${VELNOR_WORK_DIR:-$ROOT/.velnor-work}"
IDLE_TIMEOUT_SECONDS="${VELNOR_IDLE_TIMEOUT_SECONDS:-900}"
RUN_ID="${VELNOR_FIXTURE_RUN_ID:-26762850861}"
JOB_COUNT="${VELNOR_FIXTURE_JOB_COUNT:-2}"

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
python3 scripts/check_runner_reference.py
cargo test -q

echo "==> Running Docker preflight"
cargo run --bin velnor-runner -- preflight \
  --work-dir "$WORK_DIR" \
  --require-docker-socket

echo "==> Registering fixture runner"
cargo run --bin velnor-runner -- configure \
  --url "$FIXTURE_URL" \
  --pat "$GITHUB_TOKEN" \
  --name "$RUNNER_NAME" \
  --labels "$RUNNER_LABEL" \
  --replace

echo "==> Fixture run before Velnor"
gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'

echo "==> Running $JOB_COUNT Velnor fixture job(s)"
for job_index in $(seq 1 "$JOB_COUNT"); do
  echo "==> Velnor fixture job $job_index/$JOB_COUNT"
  cargo run --bin velnor-runner -- run \
    --work-dir "$WORK_DIR" \
    --once \
    --idle-timeout-seconds "$IDLE_TIMEOUT_SECONDS"
done

echo "==> Fixture run after Velnor"
gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'

echo "==> Waiting briefly for compare-results"
gh run watch "$RUN_ID" --repo "$FIXTURE_REPO" --exit-status

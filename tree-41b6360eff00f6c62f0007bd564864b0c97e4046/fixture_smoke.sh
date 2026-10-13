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
LIVE_EVIDENCE_DIR="${VELNOR_LIVE_EVIDENCE_DIR:-$ROOT/.velnor-live-evidence}"
LIVE_EVIDENCE_LOG_LINES="${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}"
REGISTERED_RUNNER=false

sanitize_filename() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_'
}

write_runner_snapshot() {
  local runner_snapshot

  echo
  echo "## Registered Runner Snapshot"
  echo
  echo "| name | os | status | busy | labels |"
  echo "| --- | --- | --- | --- | --- |"

  if runner_snapshot="$(gh api "repos/$FIXTURE_REPO/actions/runners" --paginate \
    --jq ".runners[] | select(.name == \"$RUNNER_NAME\") | \"| \" + .name + \" | \" + .os + \" | \" + .status + \" | \" + (.busy | tostring) + \" | \" + ([.labels[].name] | join(\", \")) + \" |\"" 2>&1)"; then
    if [[ -n "$runner_snapshot" ]]; then
      printf '%s\n' "$runner_snapshot"
    else
      echo "| $RUNNER_NAME | <not found> | <not found> | <not found> | <not found> |"
    fi
  else
    echo "| $RUNNER_NAME | <unavailable> | <unavailable> | <unavailable> | $(printf '%s' "$runner_snapshot" | tr '\n' ' ') |"
  fi
}

write_artifact_snapshot() {
  local artifact_snapshot

  echo
  echo "## Run Artifacts"
  echo
  echo "| name | size bytes | expired | download URL |"
  echo "| --- | ---: | --- | --- |"

  if artifact_snapshot="$(gh api "repos/$FIXTURE_REPO/actions/runs/$RUN_ID/artifacts" --paginate \
    --jq '.artifacts[] | "| " + .name + " | " + (.size_in_bytes | tostring) + " | " + (.expired | tostring) + " | " + .archive_download_url + " |"' 2>&1)"; then
    if [[ -n "$artifact_snapshot" ]]; then
      printf '%s\n' "$artifact_snapshot"
    else
      echo "| <none> | 0 | false | <none> |"
    fi
  else
    echo "| <unavailable> | 0 | false | $(printf '%s' "$artifact_snapshot" | tr '\n' ' ') |"
  fi
}

write_log_snapshot() {
  local log_file error_file
  log_file="$(mktemp)"
  error_file="$(mktemp)"

  echo
  echo "## GitHub Log Excerpt"
  echo
  echo "- excerpt lines: $LIVE_EVIDENCE_LOG_LINES"
  echo

  if gh run view "$RUN_ID" --repo "$FIXTURE_REPO" --log >"$log_file" 2>"$error_file"; then
    echo "### First Lines"
    echo
    echo '```text'
    head -n "$LIVE_EVIDENCE_LOG_LINES" "$log_file"
    echo '```'
    echo
    echo "### Last Lines"
    echo
    echo '```text'
    tail -n "$LIVE_EVIDENCE_LOG_LINES" "$log_file"
    echo '```'
  else
    echo '```text'
    tr '\n' ' ' <"$error_file"
    echo
    echo '```'
  fi

  rm -f "$log_file" "$error_file"
}

write_live_evidence() {
  local phase="$1"

  if [[ -z "$RUN_ID" ]]; then
    return
  fi

  mkdir -p "$LIVE_EVIDENCE_DIR"

  local safe_repo safe_workflow evidence_file
  safe_repo="$(sanitize_filename "$FIXTURE_REPO")"
  safe_workflow="$(sanitize_filename "$WORKFLOW")"
  evidence_file="$LIVE_EVIDENCE_DIR/${safe_repo}-${safe_workflow}-${RUN_ID}.md"

  {
    echo "# Velnor Fixture Live Evidence"
    echo
    echo "- phase: $phase"
    echo "- repository: $FIXTURE_REPO"
    echo "- run id: $RUN_ID"
    echo "- workflow: $WORKFLOW"
    echo "- ref: ${FIXTURE_REF:-<default>}"
    echo "- inputs: ${FIXTURE_INPUTS:-<none>}"
    echo "- runner name: $RUNNER_NAME"
    echo "- runner label: $RUNNER_LABEL"
    echo "- job count requested: $JOB_COUNT"
    echo "- Velnor commit: $(git rev-parse HEAD)"
    echo "- host: $(uname -s)/$(uname -m)"
    echo "- work dir: $WORK_DIR"
    echo "- Docker host work dir: ${DOCKER_HOST_WORK_DIR:-<same as work dir>}"
    echo "- Docker host: ${DOCKER_HOST:-<local default>}"
    echo "- require Docker socket: $REQUIRE_DOCKER_SOCKET"
    echo "- job message dumps: ${DUMP_JOB_MESSAGES:-<disabled>}"
    echo "- captured at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo
    echo "## GitHub Run"
    echo
    gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
      --json status,conclusion,jobs,url \
      --jq '
        "- url: " + .url,
        "- status: " + .status,
        "- conclusion: " + (.conclusion // ""),
        "",
        "| job | status | conclusion |",
        "| --- | --- | --- |",
        (.jobs[] | "| " + .name + " | " + .status + " | " + (.conclusion // "") + " |")
      '
    write_runner_snapshot
    write_artifact_snapshot
    write_log_snapshot
  } >"$evidence_file"

  echo "==> Wrote live evidence $evidence_file"
}

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
  workflow_run_args=("$WORKFLOW" --repo "$FIXTURE_REPO")
  if [[ -n "$FIXTURE_REF" ]]; then
    workflow_run_args+=(--ref "$FIXTURE_REF")
  fi
  if [[ -n "$FIXTURE_INPUTS" ]]; then
    IFS=',' read -r -a fixture_inputs <<<"$FIXTURE_INPUTS"
    for input in "${fixture_inputs[@]}"; do
      workflow_run_args+=(-f "$input")
    done
  fi
  gh workflow run "${workflow_run_args[@]}"
  echo "==> Waiting for dispatched run to appear"
  for _ in $(seq 1 30); do
    run_list_args=(--repo "$FIXTURE_REPO" --workflow "$WORKFLOW" --event workflow_dispatch --limit 1 --json databaseId)
    if [[ -n "$FIXTURE_REF" ]]; then
      run_list_args+=(--branch "$FIXTURE_REF")
    fi
    RUN_ID="$(gh run list "${run_list_args[@]}" --jq '.[0].databaseId // ""')"
    if [[ -n "$RUN_ID" ]]; then
      break
    fi
    sleep 2
  done
  if [[ -z "$RUN_ID" ]]; then
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

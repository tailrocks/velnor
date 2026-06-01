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
LIVE_EVIDENCE_DIR="${VELNOR_LIVE_EVIDENCE_DIR:-$ROOT/.velnor-live-evidence}"
LIVE_EVIDENCE_LOG_LINES="${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}"
LIVE_EVIDENCE_LOCAL_ENTRIES="${VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES:-80}"
JOB_COUNT="${VELNOR_TARGET_JOB_COUNT:-1}"
WORKFLOW="${VELNOR_TARGET_WORKFLOW:-}"
TARGET_REF="${VELNOR_TARGET_REF:-}"
TARGET_INPUTS="${VELNOR_TARGET_INPUTS:-}"
RUN_ID="${VELNOR_TARGET_RUN_ID:-}"
WATCH_RUN="${VELNOR_TARGET_WATCH_RUN:-false}"
TARGET_LABEL="${VELNOR_TARGET_LABEL:-target}"
TARGET_MVP_ARM_LABEL="${VELNOR_TARGET_MVP_ARM_LABEL:-false}"
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

  if runner_snapshot="$(gh api "repos/$TARGET_REPO/actions/runners" --paginate \
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

  if artifact_snapshot="$(gh api "repos/$TARGET_REPO/actions/runs/$RUN_ID/artifacts" --paginate \
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

write_step_snapshot() {
  local step_snapshot

  echo
  echo "## GitHub Job Step Snapshot"
  echo
  echo "| job | step | number | status | conclusion | started | completed |"
  echo "| --- | --- | ---: | --- | --- | --- | --- |"

  if step_snapshot="$(gh run view "$RUN_ID" --repo "$TARGET_REPO" --json jobs \
    --jq '.jobs[] as $job | ($job.steps // [])[] | "| " + $job.name + " | " + .name + " | " + (.number | tostring) + " | " + .status + " | " + (.conclusion // "") + " | " + (.startedAt // "") + " | " + (.completedAt // "") + " |"' 2>&1)"; then
    if [[ -n "$step_snapshot" ]]; then
      printf '%s\n' "$step_snapshot"
    else
      echo "| <none> | <none> | 0 | <none> | <none> | <none> | <none> |"
    fi
  else
    echo "| <unavailable> | $(printf '%s' "$step_snapshot" | tr '\n' ' ') | 0 | <unavailable> | <unavailable> | <unavailable> | <unavailable> |"
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

  if gh run view "$RUN_ID" --repo "$TARGET_REPO" --log >"$log_file" 2>"$error_file"; then
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

write_local_storage_snapshot() {
  local stores=()
  local store_name store size

  echo
  echo "## Velnor Local Storage Snapshot"
  echo
  echo "- max entries per store: $LIVE_EVIDENCE_LOCAL_ENTRIES"
  echo

  for store_name in _velnor_caches _velnor_artifacts _velnor_sccache; do
    while IFS= read -r store; do
      stores+=("$store")
    done < <(find "$WORK_DIR" -type d -name "$store_name" 2>/dev/null | sort || true)
  done

  if [[ "${#stores[@]}" -eq 0 ]]; then
    echo "No Velnor local cache, artifact, or sccache stores found under $WORK_DIR."
    return
  fi

  for store in "${stores[@]}"; do
    size="$(du -sh "$store" 2>/dev/null | awk '{print $1}')"
    echo "### $store"
    echo
    echo "- size: ${size:-unknown}"
    echo
    echo '```text'
    find "$store" -mindepth 1 -maxdepth 3 2>/dev/null | sort | head -n "$LIVE_EVIDENCE_LOCAL_ENTRIES"
    echo '```'
    echo
  done
}

write_live_evidence() {
  local phase="$1"

  if [[ -z "$RUN_ID" ]]; then
    return
  fi

  mkdir -p "$LIVE_EVIDENCE_DIR"

  local workflow_name="${WORKFLOW:-existing-run}"
  local safe_repo safe_workflow evidence_file
  safe_repo="$(sanitize_filename "$TARGET_REPO")"
  safe_workflow="$(sanitize_filename "$workflow_name")"
  evidence_file="$LIVE_EVIDENCE_DIR/${safe_repo}-${safe_workflow}-${RUN_ID}.md"

  {
    echo "# Velnor Target Live Evidence"
    echo
    echo "- phase: $phase"
    echo "- target label: $TARGET_LABEL"
    echo "- repository: $TARGET_REPO"
    echo "- run id: $RUN_ID"
    echo "- workflow: ${WORKFLOW:-<existing run>}"
    echo "- ref: ${TARGET_REF:-<default>}"
    echo "- inputs: ${TARGET_INPUTS:-<none>}"
    echo "- runner name: $RUNNER_NAME"
    echo "- target MVP ARM label: $TARGET_MVP_ARM_LABEL"
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
    gh run view "$RUN_ID" --repo "$TARGET_REPO" \
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
    write_step_snapshot
    write_log_snapshot
    write_local_storage_snapshot
  } >"$evidence_file"

  echo "==> Wrote live evidence $evidence_file"
}

cleanup_runner() {
  if [[ "$REGISTERED_RUNNER" == "true" && "$CLEANUP_RUNNER" == "true" ]]; then
    echo "==> Removing target runner"
    cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" || true
  fi
}

trap cleanup_runner EXIT

if ! [[ "$JOB_COUNT" =~ ^[1-9][0-9]*$ ]]; then
  echo "VELNOR_TARGET_JOB_COUNT must be a positive integer." >&2
  exit 2
fi

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
  gh run view "$RUN_ID" --repo "$TARGET_REPO" \
    --json status,conclusion,jobs,url \
    --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)'
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

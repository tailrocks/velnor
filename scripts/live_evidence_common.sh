#!/usr/bin/env bash

LIVE_EVIDENCE_DIR="${LIVE_EVIDENCE_DIR:-${VELNOR_LIVE_EVIDENCE_DIR:-$ROOT/.velnor-live-evidence}}"
LIVE_EVIDENCE_LOG_LINES="${LIVE_EVIDENCE_LOG_LINES:-${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}}"
LIVE_EVIDENCE_LOCAL_ENTRIES="${LIVE_EVIDENCE_LOCAL_ENTRIES:-${VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES:-80}}"

sanitize_filename() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_'
}

if ! declare -F live_evidence_extra_metadata >/dev/null; then
  live_evidence_extra_metadata() {
    true
  }
fi

write_runner_snapshot() {
  local runner_snapshot

  echo
  echo "## Registered Runner Snapshot"
  echo
  echo "| name | os | status | busy | labels |"
  echo "| --- | --- | --- | --- | --- |"

  if runner_snapshot="$(gh api "repos/$LIVE_EVIDENCE_REPO/actions/runners" --paginate \
    --jq ".runners[] | select(.name == \"$RUNNER_NAME\" or (.name | startswith(\"$RUNNER_NAME-slot-\"))) | \"| \" + .name + \" | \" + .os + \" | \" + .status + \" | \" + (.busy | tostring) + \" | \" + ([.labels[].name] | join(\", \")) + \" |\"" 2>&1)"; then
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

  if artifact_snapshot="$(gh api "repos/$LIVE_EVIDENCE_REPO/actions/runs/$RUN_ID/artifacts" --paginate \
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

  if step_snapshot="$(gh run view "$RUN_ID" --repo "$LIVE_EVIDENCE_REPO" --json jobs \
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

  if gh run view "$RUN_ID" --repo "$LIVE_EVIDENCE_REPO" --log >"$log_file" 2>"$error_file"; then
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

write_job_dump_snapshot() {
  local dump_count

  echo
  echo "## Sanitized Job Message Dumps"
  echo

  if [[ -z "${DUMP_JOB_MESSAGES:-}" ]]; then
    echo "Job message dumps disabled."
    return
  fi

  echo "- directory: $DUMP_JOB_MESSAGES"

  if [[ ! -d "$DUMP_JOB_MESSAGES" ]]; then
    echo "- files: 0"
    return
  fi

  dump_count="$(find "$DUMP_JOB_MESSAGES" -type f 2>/dev/null | wc -l | tr -d ' ')"
  echo "- files: ${dump_count:-0}"
  echo
  echo '```text'
  find "$DUMP_JOB_MESSAGES" -type f 2>/dev/null | sort | head -n "$LIVE_EVIDENCE_LOCAL_ENTRIES"
  echo '```'
}

write_source_snapshot() {
  local commit branch dirty_count

  commit="$(git rev-parse HEAD 2>/dev/null || printf '<unavailable>')"
  branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || printf '<unavailable>')"
  dirty_count="$(git status --short 2>/dev/null | wc -l | tr -d ' ' || printf 'unknown')"

  echo
  echo "## Velnor Source Snapshot"
  echo
  echo "- commit: $commit"
  echo "- branch: $branch"
  echo "- dirty files: ${dirty_count:-0}"
  echo
  if [[ "${dirty_count:-0}" != "0" && "${dirty_count:-0}" != "unknown" ]]; then
    echo '```text'
    git status --short 2>/dev/null | head -n "$LIVE_EVIDENCE_LOCAL_ENTRIES"
    echo '```'
  fi
}

write_github_run_snapshot() {
  local run_snapshot

  echo
  echo "## GitHub Run"
  echo

  if run_snapshot="$(gh run view "$RUN_ID" --repo "$LIVE_EVIDENCE_REPO" \
    --json status,conclusion,jobs,url \
    --jq '
      "- url: " + .url,
      "- status: " + .status,
      "- conclusion: " + (.conclusion // ""),
      "",
      "| job | database id | status | conclusion | URL |",
      "| --- | ---: | --- | --- | --- |",
      (.jobs[] | "| " + .name + " | " + ((.databaseId // "") | tostring) + " | " + (.status // "") + " | " + (.conclusion // "") + " | " + (.url // "") + " |")
    ' 2>&1)"; then
    printf '%s\n' "$run_snapshot"
  else
    echo "GitHub run snapshot unavailable:"
    echo
    echo '```text'
    printf '%s\n' "$run_snapshot"
    echo '```'
  fi
}

show_github_run_status() {
  local run_status

  if run_status="$(gh run view "$RUN_ID" --repo "$LIVE_EVIDENCE_REPO" \
    --json status,conclusion,jobs,url \
    --jq '.url, (.jobs[] | [.name,.status,(.conclusion // "")] | @tsv)' 2>&1)"; then
    printf '%s\n' "$run_status"
  else
    echo "GitHub run status unavailable:"
    printf '%s\n' "$run_status"
  fi
}

write_live_evidence() {
  local phase="$1"

  if [[ -z "$RUN_ID" ]]; then
    return
  fi

  mkdir -p "$LIVE_EVIDENCE_DIR"

  local workflow_name="${LIVE_EVIDENCE_WORKFLOW:-existing-run}"
  local safe_repo safe_workflow evidence_file
  safe_repo="$(sanitize_filename "$LIVE_EVIDENCE_REPO")"
  safe_workflow="$(sanitize_filename "$workflow_name")"
  evidence_file="$LIVE_EVIDENCE_DIR/${safe_repo}-${safe_workflow}-${RUN_ID}.md"

  {
    echo "# Velnor ${LIVE_EVIDENCE_TITLE:-Target} Live Evidence"
    echo
    echo "- phase: $phase"
    echo "- repository: $LIVE_EVIDENCE_REPO"
    echo "- run id: $RUN_ID"
    echo "- workflow: ${LIVE_EVIDENCE_WORKFLOW:-<existing run>}"
    echo "- ref: ${LIVE_EVIDENCE_REF:-<default>}"
    echo "- inputs: ${LIVE_EVIDENCE_INPUTS:-<none>}"
    echo "- runner name: $RUNNER_NAME"
    live_evidence_extra_metadata
    echo "- job count requested: $JOB_COUNT"
    echo "- host: $(uname -s)/$(uname -m)"
    echo "- work dir: $WORK_DIR"
    echo "- Docker host work dir: ${DOCKER_HOST_WORK_DIR:-<same as work dir>}"
    echo "- Docker host: ${DOCKER_HOST:-<local default>}"
    echo "- require Docker socket: $REQUIRE_DOCKER_SOCKET"
    echo "- job message dumps: ${DUMP_JOB_MESSAGES:-<disabled>}"
    echo "- captured at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    write_source_snapshot
    write_github_run_snapshot
    write_runner_snapshot
    write_artifact_snapshot
    write_step_snapshot
    write_log_snapshot
    write_local_storage_snapshot
    write_job_dump_snapshot
  } >"$evidence_file"

  echo "==> Wrote live evidence $evidence_file"
}

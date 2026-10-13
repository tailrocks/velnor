#!/usr/bin/env bash

velnor_require_bool() {
  local name="$1"
  local value="$2"

  if [[ "$value" != "true" && "$value" != "false" ]]; then
    echo "$name must be 'true' or 'false'." >&2
    return 2
  fi
}

velnor_require_positive_int() {
  local name="$1"
  local value="$2"

  if ! [[ "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "$name must be a positive integer." >&2
    return 2
  fi
}

velnor_require_optional_positive_int() {
  local name="$1"
  local value="$2"

  if [[ -n "$value" ]]; then
    velnor_require_positive_int "$name" "$value"
  fi
}

velnor_require_nonempty() {
  local name="$1"
  local value="$2"

  if [[ -z "$value" ]]; then
    echo "$name must not be empty." >&2
    return 2
  fi
}

velnor_require_repo_slug() {
  local name="$1"
  local value="$2"

  if ! [[ "$value" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
    echo "$name must be a GitHub repository slug in owner/name form." >&2
    return 2
  fi
}

velnor_require_optional_workflow_file() {
  local name="$1"
  local value="$2"

  if [[ -n "$value" && ! "$value" =~ ^[A-Za-z0-9_.-]+\.ya?ml$ ]]; then
    echo "$name must be a workflow file name ending in .yml or .yaml." >&2
    return 2
  fi
}

velnor_require_live_evidence_controls() {
  local log_lines="${LIVE_EVIDENCE_LOG_LINES:-${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}}"
  local local_entries="${LIVE_EVIDENCE_LOCAL_ENTRIES:-${VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES:-80}}"

  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOG_LINES "$log_lines" || return $?
  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES "$local_entries" || return $?
}

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

velnor_require_live_evidence_controls() {
  local log_lines="${LIVE_EVIDENCE_LOG_LINES:-${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}}"
  local local_entries="${LIVE_EVIDENCE_LOCAL_ENTRIES:-${VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES:-80}}"

  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOG_LINES "$log_lines" || return $?
  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES "$local_entries" || return $?
}

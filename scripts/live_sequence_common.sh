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

velnor_require_workflow_file() {
  local name="$1"
  local value="$2"

  velnor_require_nonempty "$name" "$value" || return $?
  velnor_require_optional_workflow_file "$name" "$value" || return $?
}

velnor_require_live_evidence_controls() {
  local log_lines="${LIVE_EVIDENCE_LOG_LINES:-${VELNOR_LIVE_EVIDENCE_LOG_LINES:-80}}"
  local local_entries="${LIVE_EVIDENCE_LOCAL_ENTRIES:-${VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES:-80}}"

  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOG_LINES "$log_lines" || return $?
  velnor_require_positive_int VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES "$local_entries" || return $?
}

velnor_fail_if_other_online_runners_match_labels() {
  local repo="$1"
  local expected_name="$2"
  shift 2
  local labels=("$@")
  local runner_rows name status label_csv label other_matches=()

  if [[ "${VELNOR_ALLOW_OTHER_MATCHING_RUNNERS:-false}" == "true" ]]; then
    echo "Skipping matching runner exclusivity check because VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true."
    return 0
  fi

  if [[ "${#labels[@]}" -eq 0 ]]; then
    return 0
  fi

  if ! runner_rows="$(gh api "repos/$repo/actions/runners" --paginate \
    --jq '.runners[] | [.name, .status, ([.labels[].name] | join(","))] | @tsv')"; then
    echo "failed to list self-hosted runners for $repo; cannot verify exclusive live proof labels" >&2
    return 2
  fi

  while IFS=$'\t' read -r name status label_csv; do
    [[ -n "$name" ]] || continue
    [[ "$status" == "online" ]] || continue
    [[ "$name" != "$expected_name" ]] || continue

    for label in "${labels[@]}"; do
      if [[ ",$label_csv," == *",$label,"* ]]; then
        other_matches+=("$name ($label)")
      fi
    done
  done <<<"$runner_rows"

  if [[ "${#other_matches[@]}" -gt 0 ]]; then
    echo "other online self-hosted runners can match this live proof:" >&2
    printf '  %s\n' "${other_matches[@]}" >&2
    echo "stop/remove them or set VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true for a deliberate non-exclusive run" >&2
    return 2
  fi
}

velnor_print_job_execution_model() {
  local job_count="$1"
  local label="${2:-Velnor}"

  echo "==> $label job execution model"
  echo "One Velnor runner process handles one active GitHub job at a time."
  echo "This smoke script will consume $job_count job(s) sequentially with repeated --once runs."
  echo "For parallel GitHub jobs, run multiple Velnor processes with distinct runner names and work directories."
}

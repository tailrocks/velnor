#!/usr/bin/env bash

workflow_dispatch_run_ids() {
  local repo="$1"
  local workflow="$2"
  local ref="${3:-}"
  local -a args=(--repo "$repo" --workflow "$workflow" --event workflow_dispatch --limit 20 --json databaseId)

  if workflow_dispatch_ref_can_filter_branch "$ref"; then
    args+=(--branch "$ref")
  fi

  gh run list "${args[@]}" --jq '.[].databaseId'
}

workflow_dispatch_ref_can_filter_branch() {
  local ref="${1:-}"
  [[ -n "$ref" && ! "$ref" =~ ^[0-9a-fA-F]{40}$ ]]
}

validate_workflow_dispatch_inputs() {
  local inputs="${1:-}"
  local input key

  if [[ -z "$inputs" ]]; then
    return 0
  fi

  IFS=',' read -r -a workflow_inputs <<<"$inputs"
  for input in "${workflow_inputs[@]}"; do
    if [[ -z "$input" || "$input" != *=* ]]; then
      echo "workflow dispatch inputs must be comma-separated key=value pairs: '$inputs'" >&2
      return 2
    fi

    key="${input%%=*}"
    if [[ -z "$key" || ! "$key" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]]; then
      echo "workflow dispatch input key must match [A-Za-z_][A-Za-z0-9_-]*: '$input'" >&2
      return 2
    fi
  done
}

dispatch_workflow_and_wait_run_id() {
  local repo="$1"
  local workflow="$2"
  local ref="${3:-}"
  local inputs="${4:-}"
  local before_ids run_id
  local -a workflow_run_args=("$workflow" --repo "$repo")

  validate_workflow_dispatch_inputs "$inputs"

  before_ids="$(workflow_dispatch_run_ids "$repo" "$workflow" "$ref" || true)"

  if [[ -n "$ref" ]]; then
    workflow_run_args+=(--ref "$ref")
  fi
  if [[ -n "$inputs" ]]; then
    local -a workflow_inputs
    IFS=',' read -r -a workflow_inputs <<<"$inputs"
    for input in "${workflow_inputs[@]}"; do
      workflow_run_args+=(-f "$input")
    done
  fi

  gh workflow run "${workflow_run_args[@]}" >&2

  for _ in $(seq 1 30); do
    while IFS= read -r run_id; do
      if [[ -n "$run_id" ]] && ! grep -Fxq "$run_id" <<<"$before_ids"; then
        printf '%s\n' "$run_id"
        return 0
      fi
    done < <(workflow_dispatch_run_ids "$repo" "$workflow" "$ref")
    sleep 2
  done

  return 1
}

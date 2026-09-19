#!/usr/bin/env bash
set -euo pipefail

source_dir=${1:?source fixture directory}
target_dir=${2:?b11 fixture directory}
mkdir -p "$target_dir"

# b11 makes evidence_role an explicit required field. Preserve every source
# JSON byte-for-byte semantically, adding only the role required by the new
# schema. G0 inventory rows are inventory; execution-stage rows are default
# branch unless a later PR-specific fixture rewrites its role.
for source in "$source_dir"/*.json; do
  name=${source##*/}
  if [[ "$name" == g0-* ]]; then
    role=inventory
  else
    role=default_branch
  fi
  if jq -e 'has("records")' "$source" >/dev/null 2>&1; then
    if [[ "$name" == g1-pr-* ]]; then
      jq --arg role "$role" '.records |= map(if .pr_number != null then .evidence_role="pull_request" else .evidence_role=$role end)' "$source" > "$target_dir/$name"
    elif [[ "$name" == g7-self-review-evidence.json ]]; then
      jq '.reviewer_attestation.artifact={source_repository:"tailrocks/velnor",source_revision:"abe9ad82a2d4d01b706bbc6122ab6ccb150faad9",source_tree_digest:"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",source_diff_digest:"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",run_manifest_digest:"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",source_url:"https://github.com/tailrocks/velnor-review"} | .records |= map(.evidence_role=$role)' "$source" > "$target_dir/$name"
    else
      jq --arg role "$role" '.records |= map(.evidence_role=$role)' "$source" > "$target_dir/$name"
    fi
  else
    cp "$source" "$target_dir/$name"
  fi
done

printf '%s\n' "prepared b11 fixtures in $target_dir"

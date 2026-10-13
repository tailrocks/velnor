#!/usr/bin/env bash
# runner_group_doctor.sh — fail when the trusted runner-group allowlist drifts
# from the set of org repositories whose canonical workflows use the Velnor lane.
#
# Outage class this guards (2026-08-24): the `velnor-trusted` org runner group
# (visibility=selected) silently lost all but 3 of its allowlisted repositories.
# Runners stayed online+idle while velnor-lane jobs in unlisted repos queued
# forever. Detection was manual; this script makes it a one-command check.
#
# Usage:
#   scripts/runner_group_doctor.sh [--org tailrocks] [--group velnor-trusted]
#
# Requires: gh authenticated with admin:org (read of runner groups) + repo.
set -euo pipefail

ORG="tailrocks"
GROUP="velnor-trusted"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --org) ORG="$2"; shift 2 ;;
    --group) GROUP="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

echo "==> Resolving runner group '$GROUP' in org '$ORG'"
GROUP_ID="$(gh api -H 'X-GitHub-Api-Version: 2026-03-10' \
  "orgs/${ORG}/actions/runner-groups" \
  --jq ".runner_groups[] | select(.name == \"${GROUP}\") | .id")"
if [[ -z "$GROUP_ID" ]]; then
  echo "runner group not found: ${ORG}/${GROUP}" >&2
  exit 2
fi
echo "group id: ${GROUP_ID}"

echo "==> Reading allowlisted repositories"
ALLOWED="$(gh api --paginate -H 'X-GitHub-Api-Version: 2026-03-10' \
  "orgs/${ORG}/actions/runner-groups/${GROUP_ID}/repositories" \
  --jq '.repositories[].full_name' | sort)"

echo "==> Scanning org repositories for velnor-target-mvp workflows"
VELNOR_LANE_REPOS=()
while IFS= read -r repo; do
  workflow_paths="$(gh api "repos/${repo}/git/trees/HEAD?recursive=1" \
    --jq '.tree[] | select(.type == "blob" and (.path | startswith(".github/workflows/"))) | .path' \
    2>/dev/null || true)"
  [[ -z "$workflow_paths" ]] && continue
  while IFS= read -r path; do
    if gh api "repos/${repo}/contents/${path}" --jq .content 2>/dev/null \
      | base64 -d 2>/dev/null | grep -q 'velnor-target-mvp'; then
      VELNOR_LANE_REPOS+=("$repo")
      break
    fi
  done <<< "$workflow_paths"
done < <(gh repo list "$ORG" --limit 200 --json nameWithOwner,isArchived \
  --jq '.[] | select(.isArchived | not) | .nameWithOwner')

MISSING=()
for repo in "${VELNOR_LANE_REPOS[@]}"; do
  if ! grep -qx "$repo" <<< "$ALLOWED"; then
    MISSING+=("$repo")
  fi
done

if [[ ${#MISSING[@]} -gt 0 ]]; then
  echo "FAIL: velnor-lane repositories missing from group '${GROUP}' allowlist:" >&2
  for repo in "${MISSING[@]}"; do
    echo "  $repo" >&2
    repo_id="$(gh api "repos/${repo}" --jq .id)"
    echo "    fix: gh api --method PUT -H 'X-GitHub-Api-Version: 2026-03-10' \\" >&2
    echo "      \"orgs/${ORG}/actions/runner-groups/${GROUP_ID}/repositories/${repo_id}\" --silent" >&2
  done
  echo "Docs: docs/org-fleet-migration.md (Tailrocks access repair checklist)" >&2
  exit 1
fi

echo "OK: all ${#VELNOR_LANE_REPOS[@]} velnor-lane repositories are allowlisted in '${GROUP}'."

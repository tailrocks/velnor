#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/live_sequence_common.sh"

FIXTURE_REPO="${VELNOR_FIXTURE_REPO:-donbeave/velnor-actions-fixture}"
WORKFLOW="${VELNOR_FIXTURE_WORKFLOW:-compat.yml}"
RUN_ID="${VELNOR_FIXTURE_RUN_ID:-}"

velnor_require_optional_positive_int VELNOR_FIXTURE_RUN_ID "$RUN_ID"

if ! command -v gh >/dev/null 2>&1; then
  echo "GitHub CLI 'gh' is required to inspect fixture workflow status." >&2
  exit 2
fi

if [[ -z "$RUN_ID" ]]; then
  RUN_ID="$(gh run list --repo "$FIXTURE_REPO" --workflow "$WORKFLOW" --limit 1 --json databaseId --jq '.[0].databaseId // ""')"
  if [[ -z "$RUN_ID" ]]; then
    echo "No fixture workflow runs found for $WORKFLOW in $FIXTURE_REPO." >&2
    exit 1
  fi
fi

gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '
    "url\t" + .url,
    "run\t" + .status + "\t" + (.conclusion // ""),
    (.jobs[] | "job\t" + .name + "\t" + .status + "\t" + (.conclusion // ""))
  '

#!/usr/bin/env bash
set -euo pipefail

FIXTURE_REPO="${VELNOR_FIXTURE_REPO:-donbeave/velnor-actions-fixture}"
RUN_ID="${VELNOR_FIXTURE_RUN_ID:-26762850861}"

if ! command -v gh >/dev/null 2>&1; then
  echo "GitHub CLI 'gh' is required to inspect fixture workflow status." >&2
  exit 2
fi

gh run view "$RUN_ID" --repo "$FIXTURE_REPO" \
  --json status,conclusion,jobs,url \
  --jq '
    "url\t" + .url,
    "run\t" + .status + "\t" + (.conclusion // ""),
    (.jobs[] | "job\t" + .name + "\t" + .status + "\t" + (.conclusion // ""))
  '

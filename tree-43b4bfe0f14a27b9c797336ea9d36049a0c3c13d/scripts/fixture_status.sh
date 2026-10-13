#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

args=()
if [[ -n "${VELNOR_FIXTURE_REPO:-}" ]]; then
  args+=(--repo "$VELNOR_FIXTURE_REPO")
fi
if [[ -n "${VELNOR_FIXTURE_WORKFLOW:-}" ]]; then
  args+=(--workflow "$VELNOR_FIXTURE_WORKFLOW")
fi
if [[ -n "${VELNOR_FIXTURE_RUN_ID:-}" ]]; then
  args+=(--run-id "$VELNOR_FIXTURE_RUN_ID")
fi

if [[ "${#args[@]}" -eq 0 ]]; then
  cargo run -q -p velnor-tools -- fixture-status
else
  cargo run -q -p velnor-tools -- fixture-status "${args[@]}"
fi

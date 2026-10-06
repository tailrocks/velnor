#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

args=()
if [[ -n "${VELNOR_JACKIN_ROOT:-}" ]]; then
  args+=(--jackin-root "$VELNOR_JACKIN_ROOT")
fi
if [[ -n "${VELNOR_CHAINARGOS_ROOT:-}" ]]; then
  args+=(--chainargos-root "$VELNOR_CHAINARGOS_ROOT")
fi
if [[ -n "${VELNOR_SKIP_TARGET_FRESHNESS_CHECK:-}" ]]; then
  args+=(--skip-target-freshness-check "$VELNOR_SKIP_TARGET_FRESHNESS_CHECK")
fi

if [[ "${#args[@]}" -eq 0 ]]; then
  cargo run -q -p velnor-tools -- target-verify
else
  cargo run -q -p velnor-tools -- target-verify "${args[@]}"
fi

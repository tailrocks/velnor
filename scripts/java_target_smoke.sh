#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export VELNOR_TARGET_REPO="${VELNOR_TARGET_REPO:-ChainArgos/java-monorepo}"
export VELNOR_DUMP_JOB_MESSAGES="${VELNOR_DUMP_JOB_MESSAGES:-$ROOT/.velnor-job-dumps/java-target}"
export VELNOR_TARGET_LABEL="${VELNOR_TARGET_LABEL:-Java}"

exec "$ROOT/scripts/target_smoke_common.sh"

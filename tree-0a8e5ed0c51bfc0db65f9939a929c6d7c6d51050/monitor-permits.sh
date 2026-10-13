#!/usr/bin/env bash
# Host Capacity & Permit Arbitration Quick Monitor
# All modes use the same read-only, validated ledger snapshot.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LEDGER_DB="${VELNOR_PERMIT_LEDGER:-${HOME}/.velnor-store/permit-ledger.db}"

usage() {
  cat <<EOF
Usage: $0 [command] [options]

Commands:
  check        Run assertions once (0 = proven PASS, 1 = FAIL or UNVERIFIABLE)
  watch        Run real-time terminal dashboard (default)
  json         Output JSON state/assertions; same exit status as check
  sql          Print ledger tables (read-only; no gate/FIFO certification)
  help         Show this message

Options:
  --gate G     Gate boundary to test (G3, G4, G5) [default: G3]
  --interval N Watch refresh interval in seconds [default: 1.0]
  --db PATH    Override ledger path [default: $LEDGER_DB]
  --expected-max-jobs N  Assert ledger capacity equals N (otherwise derive from DB)
  --host-config PATH    Explicit host/ledger/scope/gate JSON; required for gates

Current schema has no admission/eligibility history. FIFO is UNVERIFIABLE;
check/json fail closed. Ordering conflicts are diagnostic evidence, not proof.
See monitor_permits.py --help for the host policy format.
EOF
}

CMD="${1:-watch}"
shift || true

case "$CMD" in
  check)
    python3 "$SCRIPT_DIR/monitor_permits.py" --check "$@"
    ;;
  watch)
    python3 "$SCRIPT_DIR/monitor_permits.py" --watch "$@"
    ;;
  json)
    python3 "$SCRIPT_DIR/monitor_permits.py" --json "$@"
    ;;
  sql)
    python3 "$SCRIPT_DIR/monitor_permits.py" --sql "$@"
    ;;
  help|--help|-h)
    usage
    ;;
  *)
    python3 "$SCRIPT_DIR/monitor_permits.py" "$CMD" "$@"
    ;;
esac

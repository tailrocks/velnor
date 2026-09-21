#!/usr/bin/env bash
# Host Capacity & Permit Arbitration Quick Monitor
# Wrapper for monitor_permits.py or direct sqlite3 queries

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LEDGER_DB="${VELNOR_PERMIT_LEDGER:-/Users/donbeave/.velnor-store/permit-ledger.db}"

usage() {
  cat <<EOF
Usage: $0 [command] [options]

Commands:
  check        Run all invariant assertions once and exit (0 = PASS, 1 = FAIL)
  watch        Run real-time terminal dashboard (default)
  json         Output full JSON state and invariant evaluation
  sql          Execute raw SQL queries directly against the ledger db
  help         Show this message

Options:
  --gate G     Gate boundary to test (G3, G4, G5) [default: G3]
  --interval N Watch refresh interval in seconds [default: 1.0]
  --db PATH    Override ledger path [default: $LEDGER_DB]
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
    sqlite3 "$LEDGER_DB" <<'EOF'
.header on
.mode column
SELECT '--- CAPACITY META ---' AS section;
SELECT max_jobs, generation, reconciled_generation FROM permit_meta WHERE id = 1;
SELECT '--- ACTIVE PERMITS (' || count(*) || ' / 4) ---' AS section FROM permits;
SELECT holder, lane, state, datetime(acquired_unix, 'unixepoch', 'localtime') AS acquired_at, (unixepoch() - acquired_unix) AS age_s, pid FROM permits;
SELECT '--- DEMANDS BY STATE ---' AS section;
SELECT state, count(*) as count FROM permit_demands GROUP BY state;
SELECT '--- WAITING DEMANDS (FIFO) ---' AS section;
SELECT sequence, holder, lane, datetime(first_seen_unix, 'unixepoch', 'localtime') AS queued_at, (unixepoch() - first_seen_unix) AS wait_s FROM permit_demands WHERE state = 'eligible' ORDER BY first_seen_unix ASC, sequence ASC;
EOF
    ;;
  help|--help|-h)
    usage
    ;;
  *)
    python3 "$SCRIPT_DIR/monitor_permits.py" "$CMD" "$@"
    ;;
esac

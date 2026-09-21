#!/usr/bin/env python3
"""
Host Capacity & Permit Arbitration Monitor
Author: Alexey Zhokhov <alexey@zhokhov.com>

Monitors host-wide capacity allocation, invariant compliance, and FIFO ordering
in the Velnor SQLite Permit Ledger (/Users/donbeave/.velnor-store/permit-ledger.db).

Supported modes:
  --check              Evaluate all invariants once and exit (0 = PASS, 1 = FAIL)
  --watch              Interactive live terminal dashboard
  --json               Dump ledger state and invariant report as JSON
  --gate {G3,G4,G5}    Enforce gate-specific permit tenancy boundaries
"""

import argparse
import json
import os
import sqlite3
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

DEFAULT_LEDGER_PATH = os.environ.get(
    "VELNOR_PERMIT_LEDGER",
    "/Users/donbeave/.velnor-store/permit-ledger.db",
)

STORE_ROOT = Path("/Users/donbeave/.velnor-store")

# Gate tenancy expectations
GATE_ALLOWED_SCOPES = {
    "G3": ["essential-mac", "donbeave/essential-mac"],
    "G4": [
        "essential-mac",
        "donbeave/essential-mac",
        "jackin-agent-brown",
        "ChainArgos/jackin-agent-brown",
        "cloudflare-tofu",
        "ChainArgos/cloudflare-tofu",
        "github-terraform",
        "ChainArgos/github-terraform",
    ],
    "G5": [
        "essential-mac",
        "donbeave/essential-mac",
        "jackin-agent-brown",
        "ChainArgos/jackin-agent-brown",
        "cloudflare-tofu",
        "ChainArgos/cloudflare-tofu",
        "github-terraform",
        "ChainArgos/github-terraform",
        "java-monorepo",
        "ChainArgos/java-monorepo",
    ],
}

KNOWN_HEARTBEAT_SLOTS = [
    ("essential-mac", STORE_ROOT / "scaleset-essential-mac" / ".slot-1.heartbeat"),
    ("chainargos", STORE_ROOT / "scaleset-chainargos" / ".slot-1.heartbeat"),
    ("java-monorepo-1", STORE_ROOT / "scaleset-java-monorepo" / ".slot-1.heartbeat"),
    ("java-monorepo-2", STORE_ROOT / "scaleset-java-monorepo" / ".slot-2.heartbeat"),
    ("java-monorepo-3", STORE_ROOT / "scaleset-java-monorepo" / ".slot-3.heartbeat"),
    ("java-monorepo-4", STORE_ROOT / "scaleset-java-monorepo" / ".slot-4.heartbeat"),
]


def check_heartbeat(path: Path) -> Dict[str, Any]:
    if not path.exists():
        return {"exists": False, "alive": False, "detail": "not found"}
    try:
        mtime = path.stat().st_mtime
        age = time.time() - mtime
        raw = path.read_text().strip()
        data = json.loads(raw) if raw else {}
        pid = data.get("pid")
        pid_alive = False
        if pid:
            try:
                os.kill(pid, 0)
                pid_alive = True
            except OSError:
                pid_alive = False
        return {
            "exists": True,
            "path": str(path),
            "age_seconds": round(age, 1),
            "alive": pid_alive and age < 60,
            "pid": pid,
            "pid_alive": pid_alive,
            "sequence": data.get("sequence"),
            "generation": data.get("generation"),
        }
    except Exception as e:
        return {"exists": True, "alive": False, "detail": str(e)}


def query_ledger(db_path: str) -> Dict[str, Any]:
    if not os.path.exists(db_path):
        return {"error": f"Database file not found: {db_path}"}

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=5.0)
    conn.row_factory = sqlite3.Row
    cur = conn.cursor()

    # Meta
    cur.execute(
        "SELECT max_jobs, generation, reconciled_generation FROM permit_meta WHERE id = 1"
    )
    meta_row = cur.fetchone()
    if not meta_row:
        return {"error": "permit_meta table empty"}

    max_jobs = meta_row["max_jobs"]
    generation = meta_row["generation"]
    reconciled_generation = meta_row["reconciled_generation"]

    # Active permits joined with demand info
    cur.execute(
        """
        SELECT 
            p.holder,
            p.lane,
            p.state,
            p.acquired_unix,
            p.updated_unix,
            p.generation,
            p.pid,
            d.scope,
            d.sequence,
            d.first_seen_unix,
            d.state as demand_state
        FROM permits p
        LEFT JOIN permit_demands d ON p.holder = d.holder
        ORDER BY p.acquired_unix ASC
        """
    )
    permits = [dict(r) for r in cur.fetchall()]

    # Demands count by state
    cur.execute("SELECT state, COUNT(*) as cnt FROM permit_demands GROUP BY state")
    demand_counts = {r["state"]: r["cnt"] for r in cur.fetchall()}

    # Waiting eligible demands (FIFO order)
    cur.execute(
        """
        SELECT holder, lane, scope, first_seen_unix, sequence, updated_unix
        FROM permit_demands
        WHERE state = 'eligible'
        ORDER BY first_seen_unix ASC, sequence ASC
        """
    )
    eligible_demands = [dict(r) for r in cur.fetchall()]

    # Last 5 completed/terminal jobs
    cur.execute(
        """
        SELECT holder, lane, scope, first_seen_unix, sequence, updated_unix
        FROM permit_demands
        WHERE state = 'terminal'
        ORDER BY updated_unix DESC
        LIMIT 5
        """
    )
    recent_terminal = [dict(r) for r in cur.fetchall()]

    conn.close()

    now = int(time.time())

    for p in permits:
        p["acquired_duration_secs"] = now - p["acquired_unix"]
        p["acquired_iso"] = datetime.fromtimestamp(p["acquired_unix"]).strftime("%H:%M:%S")

    for d in eligible_demands:
        d["wait_duration_secs"] = now - d["first_seen_unix"]

    return {
        "max_jobs": max_jobs,
        "generation": generation,
        "reconciled_generation": reconciled_generation,
        "is_reconciled": generation == reconciled_generation,
        "occupied_permits": len(permits),
        "permits": permits,
        "demand_counts": demand_counts,
        "eligible_demands": eligible_demands,
        "recent_terminal": recent_terminal,
        "query_time": now,
        "query_time_iso": datetime.fromtimestamp(now).strftime("%Y-%m-%d %H:%M:%S"),
    }


def evaluate_invariants(
    data: Dict[str, Any], gate: Optional[str] = None
) -> Tuple[bool, List[Dict[str, Any]]]:
    invariants: List[Dict[str, Any]] = []
    overall_pass = True

    if "error" in data:
        invariants.append({
            "name": "DATABASE_ACCESSIBLE",
            "passed": False,
            "message": data["error"],
        })
        return False, invariants

    max_jobs = data.get("max_jobs")
    occupied = data.get("occupied_permits", 0)

    # 1. Authority Limit: max_jobs == 4
    inv1_pass = (max_jobs == 4)
    invariants.append({
        "name": "HOST_CAPACITY_AUTHORITY",
        "passed": inv1_pass,
        "message": f"max_jobs is {max_jobs} (expected 4)",
    })
    if not inv1_pass:
        overall_pass = False

    # 2. Zero Overcommit: occupied <= max_jobs
    inv2_pass = (occupied <= (max_jobs or 4))
    invariants.append({
        "name": "ZERO_OVERCOMMIT",
        "passed": inv2_pass,
        "message": f"Active permits {occupied} <= limit {max_jobs}",
    })
    if not inv2_pass:
        overall_pass = False

    # 3. Epoch Reconciled
    inv3_pass = data.get("is_reconciled", False)
    invariants.append({
        "name": "EPOCH_RECONCILED",
        "passed": inv3_pass,
        "message": (
            f"Generation {data.get('generation')} matches reconciled "
            f"{data.get('reconciled_generation')}"
        ),
    })
    if not inv3_pass:
        overall_pass = False

    # 4. Invariant: Only active jobs hold permits
    # Row presence is occupancy. Uncertain states flag a potential leak.
    uncertain_permits = [p for p in data.get("permits", []) if p["state"] == "uncertain"]
    inv4_pass = len(uncertain_permits) == 0
    invariants.append({
        "name": "NO_UNCERTAIN_LEAKS",
        "passed": inv4_pass,
        "message": f"{len(uncertain_permits)} uncertain permits held",
    })
    if not inv4_pass:
        overall_pass = False

    # 5. FIFO Integrity: No older eligible demand deferred if permits are free
    eligible = data.get("eligible_demands", [])
    if eligible and occupied < (max_jobs or 4):
        inv5_pass = False
        invariants.append({
            "name": "FIFO_CAPACITY_DRAIN",
            "passed": False,
            "message": (
                f"{len(eligible)} eligible demands queued but {max_jobs - occupied} "
                "permits remain free"
            ),
        })
        overall_pass = False
    else:
        invariants.append({
            "name": "FIFO_CAPACITY_DRAIN",
            "passed": True,
            "message": f"Queue consistent (occupied: {occupied}/{max_jobs}, waiting: {len(eligible)})",
        })

    # 6. Gate Tenancy Isolation
    if gate:
        allowed = GATE_ALLOWED_SCOPES.get(gate.upper())
        if allowed is not None:
            disallowed_holders = []
            for p in data.get("permits", []):
                h = p["holder"]
                s = p.get("scope") or ""
                # For scale set, holder format is scaleset/<pool_id>/<req_id>
                # Pool 1 is essential-mac in current config
                matched = False
                for a in allowed:
                    if a in s or a in h:
                        matched = True
                        break
                # Special check for pool 1 when essential-mac is allowed
                if "essential-mac" in allowed and h.startswith("scaleset/1/"):
                    matched = True
                if not matched:
                    disallowed_holders.append(f"{h} (scope: '{s}')")

            inv6_pass = len(disallowed_holders) == 0
            invariants.append({
                "name": f"GATE_{gate.upper()}_TENANCY_ISOLATION",
                "passed": inv6_pass,
                "message": (
                    f"All permits conform to Gate {gate.upper()} allowed scopes"
                    if inv6_pass
                    else f"Disallowed holders in Gate {gate.upper()}: {disallowed_holders}"
                ),
            })
            if not inv6_pass:
                overall_pass = False

    return overall_pass, invariants


def format_table(data: Dict[str, Any], invariants: List[Dict[str, Any]], gate: Optional[str]) -> str:
    lines = []
    lines.append("\033[1;36m=======================================================================\033[0m")
    lines.append(
        f"\033[1;37m VELNOR PERMIT LEDGER MONITOR\033[0m | Time: {data.get('query_time_iso')}"
    )
    lines.append("\033[1;36m=======================================================================\033[0m")

    max_j = data.get("max_jobs", "?")
    occ = data.get("occupied_permits", 0)
    gen = data.get("generation", "?")
    r_gen = data.get("reconciled_generation", "?")
    pct = (occ / max_j * 100) if isinstance(max_j, int) and max_j > 0 else 0

    bar_len = 20
    filled = int(bar_len * (occ / (max_j if isinstance(max_j, int) else 4)))
    bar = "█" * filled + "░" * (bar_len - filled)
    bar_color = "\033[1;32m" if occ < (max_j or 4) else "\033[1;33m"

    lines.append(
        f" Host Capacity: [{bar_color}{bar}\033[0m] "
        f"\033[1;37m{occ} / {max_j}\033[0m slots occupied ({pct:.0f}%)"
    )
    lines.append(
        f" Epoch Generation: {gen} (reconciled: {r_gen}) | "
        f"Demands: {data.get('demand_counts', {})}"
    )
    lines.append("-----------------------------------------------------------------------")

    # Invariant checks
    lines.append("\033[1;33mInvariant Assertions:\033[0m")
    for inv in invariants:
        mark = "\033[1;32m[PASS]\033[0m" if inv["passed"] else "\033[1;31m[FAIL]\033[0m"
        lines.append(f"  {mark} {inv['name']:<28} {inv['message']}")
    lines.append("-----------------------------------------------------------------------")

    # Active Permits
    permits = data.get("permits", [])
    lines.append(f"\033[1;34mActive Permits ({len(permits)} held):\033[0m")
    if not permits:
        lines.append("  (no active permits held - capacity fully available)")
    else:
        lines.append(
            f"  {'HOLDER':<36} {'LANE':<10} {'STATE':<14} {'ACQUIRED':<10} {'AGE':<7} {'PID'}"
        )
        for p in permits:
            h = p["holder"]
            if len(h) > 35:
                h = h[:16] + "..." + h[-16:]
            st = p["state"]
            st_color = "\033[1;32m" if st == "running" else "\033[1;33m"
            age_s = f"{p['acquired_duration_secs']}s"
            pid_s = str(p["pid"]) if p.get("pid") else "-"
            lines.append(
                f"  {h:<36} {p['lane']:<10} {st_color}{st:<14}\033[0m "
                f"{p['acquired_iso']:<10} {age_s:<7} {pid_s}"
            )
    lines.append("-----------------------------------------------------------------------")

    # Demands in queue
    eligible = data.get("eligible_demands", [])
    lines.append(f"\033[1;34mEligible Demand Queue ({len(eligible)} waiting FIFO):\033[0m")
    if not eligible:
        lines.append("  (queue empty - 0 waiting demands)")
    else:
        for i, d in enumerate(eligible, 1):
            h = d["holder"]
            if len(h) > 35:
                h = h[:16] + "..." + h[-16:]
            lines.append(
                f"  #{i:<2} Seq {d['sequence']:<5} {d['lane']:<10} {h:<36} "
                f"waiting {d['wait_duration_secs']}s"
            )
    lines.append("-----------------------------------------------------------------------")

    # Slot Heartbeats
    lines.append("\033[1;34mSlot Daemon Heartbeats:\033[0m")
    for name, hb_path in KNOWN_HEARTBEAT_SLOTS:
        hb = check_heartbeat(hb_path)
        if hb.get("alive"):
            st = f"\033[1;32mALIVE\033[0m (PID {hb['pid']}, seq {hb['sequence']}, age {hb['age_seconds']}s)"
        elif hb.get("exists"):
            st = f"\033[1;30mDEAD/STALE\033[0m (PID {hb.get('pid')}, age {hb.get('age_seconds')}s)"
        else:
            st = "\033[1;30mNOT_CONFIGURED\033[0m"
        lines.append(f"  {name:<18} : {st}")

    lines.append("\033[1;36m=======================================================================\033[0m")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Velnor Host Permit Ledger Monitor")
    parser.add_argument("--db", default=DEFAULT_LEDGER_PATH, help="Path to permit-ledger.db")
    parser.add_argument("--check", action="store_true", help="Run assertions once and exit (0=PASS, 1=FAIL)")
    parser.add_argument("--watch", action="store_true", help="Watch mode in real time")
    parser.add_argument("--interval", type=float, default=1.0, help="Watch refresh interval in seconds")
    parser.add_argument("--gate", choices=["G3", "G4", "G5"], default="G3", help="Gate boundary to verify")
    parser.add_argument("--json", action="store_true", help="Output JSON format")

    args = parser.parse_args()

    if args.watch:
        try:
            while True:
                data = query_ledger(args.db)
                passed, invs = evaluate_invariants(data, gate=args.gate)
                output = format_table(data, invs, gate=args.gate)
                os.system("clear")
                print(output)
                time.sleep(args.interval)
        except KeyboardInterrupt:
            print("\nExiting monitor.")
            sys.exit(0)
    else:
        data = query_ledger(args.db)
        passed, invs = evaluate_invariants(data, gate=args.gate)

        if args.json:
            result = {
                "invariants_passed": passed,
                "invariants": invs,
                "ledger": data,
            }
            print(json.dumps(result, indent=2))
        else:
            print(format_table(data, invs, gate=args.gate))

        if args.check:
            sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()

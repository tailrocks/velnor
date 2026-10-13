#!/usr/bin/env python3
"""Read-only host capacity and permit evidence monitor.

Author: Alexey Zhokhov <alexey@zhokhov.com>

--check and --json exit 0 only when every assertion is proven. UNVERIFIABLE
assertions fail closed. This ledger retains demand order, but not admission or
eligibility history: it cannot certify historical FIFO.
--sql prints validated snapshot columns without certifying gates or FIFO.

Gate checks require --host-config with explicit, operator-supplied bindings:
  {"host": "workstation", "ledger": "/absolute/path/permit-ledger.db",
   "scopes": {"essential-mac": "donbeave/essential-mac"},
   "gates": {"G3": ["donbeave/essential-mac"]}}
Repository-shaped scopes match exactly; aliases require the scopes mapping.
The path binds this policy to the inspected file, not to verified hardware.
The database contains no physical host identity or CPU/memory quota evidence.
"""

import argparse
from contextlib import closing
from datetime import datetime
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import sys
import time
from typing import Any, Dict, List, Optional, Tuple


DEFAULT_LEDGER_PATH = os.environ.get(
    "VELNOR_PERMIT_LEDGER", str(Path.home() / ".velnor-store/permit-ledger.db")
)
LANES = {"native", "scale-set"}
PERMIT_STATES = {
    "reserved", "acquiring", "provisioning", "assignable", "running",
    "cleaning", "uncertain",
}
DEMAND_STATES = {"eligible", "granted", "terminal", "cancelled"}
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")


class EvidenceError(ValueError):
    """A static, secret-free error suitable for monitor output."""


def is_integer(value: Any, minimum: int = 0, maximum: int = 2**63 - 1) -> bool:
    return type(value) is int and minimum <= value <= maximum


def is_identity(value: Any) -> bool:
    return (
        isinstance(value, str) and bool(value) and value == value.strip()
        and value.isprintable()
    )


def validate_rows(meta: Dict[str, Any], permits: List[Dict[str, Any]],
                  demands: List[Dict[str, Any]]) -> None:
    if not is_integer(meta["max_jobs"], maximum=2**32 - 1):
        raise EvidenceError("Ledger capacity must be a configured nonnegative integer")
    if not is_integer(meta["generation"]) or not is_integer(meta["reconciled_generation"], -1):
        raise EvidenceError("Invalid ledger generation evidence")
    for rows, kind in ((permits, "permit"), (demands, "demand")):
        holders = set()
        sequences = set()
        for row in rows:
            if not is_identity(row["holder"]) or row["holder"] in holders:
                raise EvidenceError("Missing or duplicate holder identity")
            holders.add(row["holder"])
            states = PERMIT_STATES if kind == "permit" else DEMAND_STATES
            if row["lane"] not in LANES or row["state"] not in states:
                raise EvidenceError("Unsupported ledger lane or state")
            start = "acquired_unix" if kind == "permit" else "first_seen_unix"
            if (not is_integer(row[start]) or not is_integer(row["updated_unix"])
                    or row["updated_unix"] < row[start]):
                raise EvidenceError("Invalid ledger timestamp evidence")
            if kind == "permit":
                if not is_integer(row["generation"], maximum=meta["generation"]):
                    raise EvidenceError("Invalid permit generation evidence")
                if row["pid"] is not None and not is_integer(row["pid"], 1, 2**32 - 1):
                    raise EvidenceError("Invalid permit process evidence")
            else:
                if not isinstance(row["scope"], str) or (row["scope"] and not is_identity(row["scope"])):
                    raise EvidenceError("Invalid demand scope evidence")
                if not is_integer(row["sequence"], 1) or row["sequence"] in sequences:
                    raise EvidenceError("Missing or duplicate durable demand order")
                sequences.add(row["sequence"])


def query_ledger(db_path: str) -> Dict[str, Any]:
    """Read one consistent snapshot; never create or modify the ledger."""
    try:
        path = Path(db_path).resolve()
        if not path.is_file():
            raise EvidenceError("Ledger database is missing or is not a regular file")
        # Quote '?' and '#' in filenames so callers cannot inject URI modes.
        with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5.0)) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA query_only = ON")
            conn.execute("BEGIN")
            tables = {row[0] for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )}
            if not {"permit_meta", "permits", "permit_demands"} <= tables:
                raise EvidenceError("Required ledger tables are missing")
            meta_rows = [dict(row) for row in conn.execute(
                "SELECT id, max_jobs, generation, reconciled_generation FROM permit_meta"
            )]
            if len(meta_rows) != 1 or type(meta_rows[0]["id"]) is not int or meta_rows[0]["id"] != 1:
                raise EvidenceError("Expected exactly one ledger authority row with id 1")
            meta = meta_rows[0]
            # Explicit columns: never dump arbitrary table or credential fields.
            permits = [dict(row) for row in conn.execute(
                "SELECT holder, lane, state, acquired_unix, updated_unix, generation, pid "
                "FROM permits ORDER BY acquired_unix, holder"
            )]
            demands = [dict(row) for row in conn.execute(
                "SELECT holder, lane, scope, first_seen_unix, sequence, state, updated_unix "
                "FROM permit_demands ORDER BY first_seen_unix, sequence"
            )]
            validate_rows(meta, permits, demands)

        now = int(time.time())
        by_holder = {d["holder"]: d for d in demands}
        demand_counts: Dict[str, int] = {}
        permit_counts: Dict[str, int] = {}
        for demand in demands:
            demand_counts[demand["state"]] = demand_counts.get(demand["state"], 0) + 1
        for permit in permits:
            permit_counts[permit["state"]] = permit_counts.get(permit["state"], 0) + 1
            demand = by_holder.get(permit["holder"], {})
            for key in ("scope", "sequence", "first_seen_unix"):
                permit[key] = demand.get(key)
            permit["demand_state"] = demand.get("state")
            permit["acquired_duration_secs"] = now - permit["acquired_unix"]
            permit["acquired_iso"] = datetime.fromtimestamp(permit["acquired_unix"]).strftime("%H:%M:%S")
        eligible = [dict(d, wait_duration_secs=now - d["first_seen_unix"])
                    for d in demands if d["state"] == "eligible"]
        terminal = sorted((d for d in demands if d["state"] == "terminal"),
                          key=lambda d: d["updated_unix"], reverse=True)[:5]
        return {
            "max_jobs": meta["max_jobs"], "generation": meta["generation"],
            "reconciled_generation": meta["reconciled_generation"],
            "is_reconciled": meta["generation"] == meta["reconciled_generation"],
            "occupied_permits": len(permits), "permits": permits,
            "permit_state_counts": permit_counts,
            "demands": demands, "demand_counts": demand_counts,
            "eligible_demands": eligible, "recent_terminal": terminal,
            "query_time": now,
            "query_time_iso": datetime.fromtimestamp(now).strftime("%Y-%m-%d %H:%M:%S"),
        }
    except EvidenceError as error:
        return {"error": str(error)}
    except (sqlite3.Error, OSError, ValueError, OverflowError):
        # SQLite errors and paths can contain private input; do not echo them.
        return {"error": "Ledger is unreadable, corrupt, or has unsupported schema/data"}


def unique_object(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
    result = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError("Duplicate host configuration key")
        result[key] = value
    return result


def load_host_config(config_path: Optional[str], db_path: str) -> Dict[str, Any]:
    if not config_path:
        return {"error": "Gate evidence requires --host-config with host, ledger, scopes, and gates"}
    try:
        with open(config_path, encoding="utf-8") as source:
            config = json.load(source, object_pairs_hook=unique_object)
        if not isinstance(config, dict) or set(config) != {"host", "ledger", "scopes", "gates"}:
            raise EvidenceError("Host configuration must contain host, ledger, scopes, and gates only")
        if not is_identity(config["host"]) or not is_identity(config["ledger"]):
            raise EvidenceError("Host identity and ledger path must be explicit")
        ledger = Path(config["ledger"])
        if not ledger.is_absolute() or ledger.resolve() != Path(db_path).resolve():
            raise EvidenceError("Host configuration does not bind the inspected ledger path")
        scopes, gates = config["scopes"], config["gates"]
        if not isinstance(scopes, dict) or not isinstance(gates, dict) or not gates:
            raise EvidenceError("Invalid host scope or gate configuration")
        for scope, repository in scopes.items():
            if (not is_identity(scope) or not isinstance(repository, str)
                    or not REPOSITORY.fullmatch(repository)
                    or (REPOSITORY.fullmatch(scope) and scope != repository)):
                raise EvidenceError("Scopes require exact, unambiguous repository identities")
        for gate, repositories in gates.items():
            if (gate not in {"G3", "G4", "G5"} or not isinstance(repositories, list)
                    or not repositories
                    or any(not isinstance(repo, str) or not REPOSITORY.fullmatch(repo)
                           for repo in repositories)
                    or len(set(repositories)) != len(repositories)):
                raise EvidenceError("Gates require explicit lists of exact repository identities")
        return config
    except EvidenceError as error:
        return {"error": str(error)}
    except (OSError, ValueError, TypeError):
        return {"error": "Host configuration is unreadable or invalid JSON"}


def fifo_evidence(permits: List[Dict[str, Any]], demands: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Describe observable inversions, without inventing admission history.

    first_seen_unix/sequence retain age across retries. updated_unix is mutable;
    acquired_unix can record reconciliation adoption. Neither establishes past
    eligibility, staleness, or actual admission order. Even a clean snapshot
    cannot prove that released permits respected FIFO.
    """
    def order(demand: Dict[str, Any]) -> Tuple[int, int]:
        return demand["first_seen_unix"], demand["sequence"]

    by_holder = {d["holder"]: d for d in demands}
    held = [(p, by_holder[p["holder"]]) for p in permits if p["holder"] in by_holder]
    waiting_pairs = []
    held_inversions = []
    for permit, demand in held:
        for older in demands:
            if (older["state"] == "eligible" and order(older) < order(demand)
                    and older["first_seen_unix"] <= permit["acquired_unix"]):
                waiting_pairs.append({"older_sequence": older["sequence"],
                                      "held_sequence": demand["sequence"]})
        for other_permit, older in held:
            if (order(older) < order(demand)
                    and older["first_seen_unix"] <= permit["acquired_unix"]
                    and other_permit["acquired_unix"] > permit["acquired_unix"]):
                held_inversions.append({"older_sequence": older["sequence"],
                                        "younger_sequence": demand["sequence"]})
    return {"older_waiting_pairs": waiting_pairs, "held_order_inversions": held_inversions}


def evaluate_invariants(
    data: Dict[str, Any], gate: Optional[str] = None,
    expected_max_jobs: Optional[int] = None,
    host_config: Optional[Dict[str, Any]] = None,
) -> Tuple[bool, List[Dict[str, Any]]]:
    invariants: List[Dict[str, Any]] = []

    def record(name: str, passed: bool, message: str, status: Optional[str] = None,
               **evidence: Any) -> None:
        invariants.append({"name": name, "passed": passed,
                           "status": status or ("PASS" if passed else "FAIL"),
                           "message": message, **evidence})

    if "error" in data:
        record("DATABASE_ACCESSIBLE", False, data["error"])
        return False, invariants
    try:
        validate_rows(data, data["permits"], data["demands"])
    except (EvidenceError, KeyError, TypeError):
        record("LEDGER_EVIDENCE", False, "Required ledger evidence is missing or invalid")
        return False, invariants

    limit, permits, demands = data["max_jobs"], data["permits"], data["demands"]
    occupied = len(permits)
    capacity_matches = expected_max_jobs is None or (
        is_integer(expected_max_jobs, maximum=2**32 - 1) and limit == expected_max_jobs
    )
    record("HOST_CAPACITY_AUTHORITY", capacity_matches,
           f"Ledger max_jobs={limit}" + ("; no external capacity assertion supplied"
           if expected_max_jobs is None else f"; expected {expected_max_jobs}"))
    record("ZERO_OVERCOMMIT", occupied <= limit, f"{occupied} occupied permits; ledger limit {limit}")
    record("EPOCH_RECONCILED", data["generation"] == data["reconciled_generation"],
           f"Generation {data['generation']}; reconciled {data['reconciled_generation']}")
    uncertain = sum(p["state"] == "uncertain" for p in permits)
    record("NO_UNCERTAIN_LEAKS", uncertain == 0,
           f"{uncertain} uncertain permits retained; absence does not prove worker liveness")

    by_holder = {d["holder"]: d for d in demands}
    held_holders = {p["holder"] for p in permits}
    inconsistent = 0
    for permit in permits:
        demand = by_holder.get(permit["holder"])
        if (not demand or demand["lane"] != permit["lane"]
                or (demand["state"] != "granted"
                    and not (permit["state"] == "uncertain" and demand["state"] == "terminal"))):
            inconsistent += 1
    inconsistent += sum(d["state"] == "granted" and d["holder"] not in held_holders for d in demands)
    record("PERMIT_DEMAND_CONSISTENCY", inconsistent == 0,
           f"{inconsistent} missing or inconsistent permit/demand associations")

    evidence = fifo_evidence(permits, demands)
    conflicts = len(evidence["older_waiting_pairs"]) + len(evidence["held_order_inversions"])
    record("FIFO_ADMISSION_ORDER", False,
           f"{conflicts} potential ordering conflicts in current rows; no durable admission/"
           "eligibility history or adoption marker. FIFO cannot be certified from this schema.",
           status="UNVERIFIABLE", evidence=evidence)

    if gate is not None:
        config = host_config or {"error": "Explicit host/gate evidence is missing; supply --host-config"}
        if "error" in config or gate not in config.get("gates", {}):
            record(f"GATE_{gate}_TENANCY_ISOLATION", False,
                   config.get("error", "Requested gate has no explicit host policy"), "UNVERIFIABLE")
        else:
            allowed = set(config["gates"][gate])
            disallowed = 0
            for permit in permits:
                scope = by_holder.get(permit["holder"], {}).get("scope", "")
                repository = config["scopes"].get(scope, scope)
                if not scope or repository not in allowed:
                    disallowed += 1
            record(f"GATE_{gate}_TENANCY_ISOLATION", disallowed == 0,
                   f"{disallowed} permits lack an allowed exact scope/repository binding; "
                   "policy is operator-supplied, physical host identity is not in the ledger")
    return all(inv["passed"] for inv in invariants), invariants


def contextual_evidence(data: Dict[str, Any], config: Dict[str, Any]) -> Dict[str, Any]:
    bound = "error" not in config
    return {
        "host": {
            "identity": config["host"] if bound else None,
            "source": "operator-supplied host configuration" if bound else "unavailable",
            "ledger_path_bound": bound,
            "physical_identity_verified": False,
        },
        "quota": {
            "slot_limit": data.get("max_jobs"),
            "slot_limit_source": "permit_meta.max_jobs" if "error" not in data else "unavailable",
            "cpu_memory": {"status": "UNVERIFIABLE",
                           "message": "CPU/memory quota evidence is not recorded in the permit ledger"},
        },
        "admission": {
            "permit_state_counts": data.get("permit_state_counts"),
            "demand_state_counts": data.get("demand_counts"),
            "history": "unavailable; current states are not an admission log",
        },
        "ledger_generation": data.get("generation"),
        "reconciled_generation": data.get("reconciled_generation"),
    }


def format_table(data: Dict[str, Any], invariants: List[Dict[str, Any]], gate: Optional[str]) -> str:
    lines = ["VELNOR PERMIT LEDGER MONITOR", f"Snapshot: {data.get('query_time_iso', 'unavailable')}"]
    if "error" not in data:
        lines.extend([
            f"Host capacity: {data['occupied_permits']} / {data['max_jobs']} occupied",
            f"Epoch: {data['generation']} (reconciled: {data['reconciled_generation']})",
            f"Permit states: {data['permit_state_counts']}; demand states: {data['demand_counts']}",
        ])
    lines.extend(f"[{inv['status']}] {inv['name']}: {inv['message']}" for inv in invariants)
    if "error" in data:
        return "\n".join(lines)
    lines.append("Active permits (all states count toward capacity):")
    for permit in data["permits"]:
        lines.append(f"  {permit['holder']} {permit['lane']} {permit['state']} "
                     f"age={permit['acquired_duration_secs']}s pid={permit['pid']}")
    lines.append("Eligible demands (durable observation order, not proven admission order):")
    for demand in data["eligible_demands"]:
        lines.append(f"  seq={demand['sequence']} {demand['holder']} {demand['lane']} "
                     f"waiting={demand['wait_duration_secs']}s")
    return "\n".join(lines)


def format_sql(data: Dict[str, Any]) -> str:
    """Keep SQL diagnostic mode on the same safe snapshot reader."""
    if "error" in data:
        return "[FAIL] DATABASE_ACCESSIBLE: " + data["error"]
    lines = ["--- CAPACITY META ---", "max_jobs\tgeneration\treconciled_generation",
             f"{data['max_jobs']}\t{data['generation']}\t{data['reconciled_generation']}",
             f"--- ACTIVE PERMITS ({data['occupied_permits']} / {data['max_jobs']}) ---",
             "holder\tlane\tstate\tacquired_unix\tpid"]
    for permit in data["permits"]:
        lines.append("\t".join(str(permit[key]) for key in
                               ("holder", "lane", "state", "acquired_unix", "pid")))
    lines.extend(["--- DEMANDS BY STATE ---", "state\tcount"])
    lines.extend(f"{state}\t{count}" for state, count in data["demand_counts"].items())
    lines.extend(["--- WAITING DEMANDS (OBSERVATION ORDER ONLY) ---",
                  "sequence\tholder\tlane\tfirst_seen_unix"])
    for demand in data["eligible_demands"]:
        lines.append("\t".join(str(demand[key]) for key in
                               ("sequence", "holder", "lane", "first_seen_unix")))
    return "\n".join(lines)


def capacity_argument(value: str) -> int:
    try:
        parsed = int(value)
        if is_integer(parsed, maximum=2**32 - 1):
            return parsed
    except ValueError:
        pass
    raise argparse.ArgumentTypeError("expected a nonnegative 32-bit integer")


def interval_argument(value: str) -> float:
    try:
        parsed = float(value)
        if math.isfinite(parsed) and parsed > 0:
            return parsed
    except ValueError:
        pass
    raise argparse.ArgumentTypeError("expected a finite, positive interval")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", default=DEFAULT_LEDGER_PATH, help="Path to permit-ledger.db")
    parser.add_argument("--check", action="store_true", help="Check once; FAIL/UNVERIFIABLE exit 1")
    parser.add_argument("--watch", action="store_true", help="Watch mode in real time")
    parser.add_argument("--interval", type=interval_argument, default=1.0, help="Watch interval in seconds")
    parser.add_argument("--gate", choices=["G3", "G4", "G5"], default="G3", help="Gate boundary to verify")
    parser.add_argument("--host-config", help="Explicit host, ledger, scope and gate policy JSON")
    parser.add_argument("--expected-max-jobs", type=capacity_argument, help="Assert ledger capacity equals this N")
    parser.add_argument("--json", action="store_true", help="JSON report; FAIL/UNVERIFIABLE exit 1")
    parser.add_argument("--sql", action="store_true", help="Print ledger tables, without gate/FIFO certification")
    args = parser.parse_args()
    if sum((args.watch, args.json, args.sql)) > 1 or (args.sql and args.check):
        parser.error("choose one output mode; --sql cannot certify --check assertions")
    if args.sql and (args.expected_max_jobs is not None or args.host_config is not None):
        parser.error("--sql is diagnostic; use check/json for capacity or host policy assertions")
    try:
        while True:
            data = query_ledger(args.db)
            if args.sql:
                print(format_sql(data))
                return 1 if "error" in data else 0
            config = load_host_config(args.host_config, args.db)
            passed, invariants = evaluate_invariants(data, args.gate, args.expected_max_jobs, config)
            context = contextual_evidence(data, config)
            if args.json:
                print(json.dumps({"invariants_passed": passed, "invariants": invariants,
                                  "ledger": data, "evidence": context}, indent=2))
            else:
                if args.watch and sys.stdout.isatty():
                    print("\033[2J\033[H", end="")
                print(f"Host: {context['host']['identity'] or 'UNVERIFIABLE'} "
                      "(operator-supplied; physical identity unverified)")
                print("CPU/memory quotas: UNVERIFIABLE (not recorded in ledger)")
                print(format_table(data, invariants, args.gate), flush=True)
            if not args.watch:
                return 0 if passed else 1
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())

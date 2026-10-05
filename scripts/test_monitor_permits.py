"""Regression tests using the inspected permit ledger schema; no live writes."""

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import monitor_permits as monitor


SCRIPTS = Path(__file__).resolve().parent
REPO = "donbeave/essential-mac"
SCHEMA = """
CREATE TABLE permit_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1), max_jobs INTEGER,
    generation INTEGER NOT NULL DEFAULT 0,
    reconciled_generation INTEGER NOT NULL DEFAULT -1
);
CREATE TABLE permits (
    holder TEXT PRIMARY KEY, lane TEXT NOT NULL, state TEXT NOT NULL,
    acquired_unix INTEGER NOT NULL, updated_unix INTEGER NOT NULL,
    generation INTEGER NOT NULL, pid INTEGER
);
CREATE TABLE permit_demands (
    holder TEXT PRIMARY KEY, lane TEXT NOT NULL, scope TEXT NOT NULL,
    first_seen_unix INTEGER NOT NULL, sequence INTEGER NOT NULL UNIQUE,
    state TEXT NOT NULL, updated_unix INTEGER NOT NULL
);
CREATE INDEX idx_permit_demands_oldest
    ON permit_demands (state, first_seen_unix, sequence);
INSERT INTO permit_meta VALUES (1, 16, 7, 7);
"""


class MonitorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.db = self.root / "permit-ledger.db"
        self.conn = sqlite3.connect(self.db)
        self.conn.executescript(SCHEMA)
        self.policy_path = self.root / "host.json"
        self.policy = {
            "host": "test-host", "ledger": str(self.db),
            "scopes": {"essential-mac": REPO},
            "gates": {"G3": [REPO], "G4": [REPO, "ChainArgos/cloudflare-tofu"]},
        }
        self.write_policy()

    def tearDown(self):
        self.conn.close()
        self.temp.cleanup()

    def write_policy(self):
        self.policy_path.write_text(json.dumps(self.policy), encoding="utf-8")

    def execute(self, sql, parameters=()):
        self.conn.execute(sql, parameters)
        self.conn.commit()

    def demand(self, holder="native/one", sequence=1, first_seen=100,
               state="eligible", scope=REPO, lane="native", updated=110):
        self.execute("INSERT INTO permit_demands VALUES (?, ?, ?, ?, ?, ?, ?)",
                     (holder, lane, scope, first_seen, sequence, state, updated))

    def permit(self, holder="native/one", acquired=110, state="running", lane="native"):
        self.execute("INSERT INTO permits VALUES (?, ?, ?, ?, ?, 7, NULL)",
                     (holder, lane, state, acquired, acquired))

    def held(self, holder="native/one", sequence=1, first_seen=100, acquired=110,
             state="running", scope=REPO, lane="native"):
        self.demand(holder, sequence, first_seen, "granted", scope, lane, acquired)
        self.permit(holder, acquired, state, lane)

    def snapshot(self):
        data = monitor.query_ledger(str(self.db))
        self.assertNotIn("error", data)
        return data

    def invariants(self, **options):
        passed, invariants = monitor.evaluate_invariants(
            self.snapshot(), gate="G3", host_config=self.policy, **options
        )
        self.assertFalse(passed, "Current schema cannot certify FIFO history")
        return {row["name"]: row for row in invariants}

    def cli(self, *options, wrapper=None, env=None):
        command = (["bash", str(SCRIPTS / "monitor-permits.sh"), wrapper] if wrapper
                   else [sys.executable, "-B", str(SCRIPTS / "monitor_permits.py")])
        return subprocess.run(command + ["--db", str(self.db), *options],
                              text=True, capture_output=True, timeout=10, env=env)

    def report(self, *options, wrapper=None):
        result = self.cli(*(("--json",) if not wrapper else ()), *options, wrapper=wrapper)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stderr, "")
        report = json.loads(result.stdout)
        self.assertEqual(set(report), {"invariants_passed", "invariants", "ledger", "evidence"})
        self.assertIs(report["invariants_passed"], False)
        self.assertIsInstance(report["invariants"], list)
        self.assertIsInstance(report["ledger"], dict)
        for row in report["invariants"]:
            self.assertIsInstance(row["passed"], bool)
            self.assertIn(row["status"], {"PASS", "FAIL", "UNVERIFIABLE"})
            self.assertEqual(row["passed"], row["status"] == "PASS")
        return report

    def test_capacity_comes_from_ledger_including_zero(self):
        for capacity in (0, 1, 4, 16, 32):
            with self.subTest(capacity=capacity):
                self.execute("UPDATE permit_meta SET max_jobs = ?", (capacity,))
                invariants = self.invariants()
                self.assertTrue(invariants["HOST_CAPACITY_AUTHORITY"]["passed"])
                self.assertTrue(invariants["ZERO_OVERCOMMIT"]["passed"])
                self.assertIn(f"0 / {capacity}", monitor.format_table(self.snapshot(), [], "G3"))

    def test_expected_capacity_is_an_assertion_not_an_override(self):
        self.assertTrue(self.invariants(expected_max_jobs=16)["HOST_CAPACITY_AUTHORITY"]["passed"])
        self.assertFalse(self.invariants(expected_max_jobs=4)["HOST_CAPACITY_AUTHORITY"]["passed"])
        report = self.report("--expected-max-jobs", "4")
        self.assertEqual(report["ledger"]["max_jobs"], 16)

    def test_invalid_capacity_cannot_be_hidden_by_expected_value(self):
        for capacity in (None, -1, 2.5, "SECRET_BAD_CAPACITY", 2**32):
            with self.subTest(capacity=capacity):
                self.execute("UPDATE permit_meta SET max_jobs = ?", (capacity,))
                report = self.report("--expected-max-jobs", "16")
                self.assertIn("error", report["ledger"])
                self.assertNotIn("SECRET_BAD_CAPACITY", json.dumps(report))

    def test_every_admission_state_occupies_capacity(self):
        self.execute("UPDATE permit_meta SET max_jobs = 6")
        for sequence, state in enumerate(sorted(monitor.PERMIT_STATES), 1):
            self.held(holder=f"native/{sequence}", sequence=sequence, state=state)
        data = self.snapshot()
        self.assertEqual(data["occupied_permits"], 7)
        self.assertEqual(data["permit_state_counts"], {state: 1 for state in monitor.PERMIT_STATES})
        invariants = self.invariants()
        self.assertFalse(invariants["ZERO_OVERCOMMIT"]["passed"])
        self.assertFalse(invariants["NO_UNCERTAIN_LEAKS"]["passed"])

    def test_queued_demands_and_message_counts_do_not_spend_permits(self):
        self.demand()
        self.execute("CREATE TABLE jobs_messages (available INTEGER)")
        self.execute("INSERT INTO jobs_messages VALUES (9999)")
        self.assertEqual(self.snapshot()["occupied_permits"], 0)
        self.assertEqual(self.snapshot()["max_jobs"], 16)
        self.assertTrue(self.invariants()["ZERO_OVERCOMMIT"]["passed"])

    def test_epoch_must_be_reconciled(self):
        self.execute("UPDATE permit_meta SET reconciled_generation = -1")
        self.assertFalse(self.invariants()["EPOCH_RECONCILED"]["passed"])

    def test_exact_repository_and_explicit_alias_are_allowed(self):
        for sequence, scope in enumerate((REPO, "essential-mac"), 1):
            self.held(holder=f"native/{sequence}", sequence=sequence, scope=scope)
        self.assertTrue(self.invariants()["GATE_G3_TENANCY_ISOLATION"]["passed"])

    def test_substrings_holder_names_and_pool_ids_never_authorize(self):
        for holder, scope in (
            ("native/one", "evil-essential-mac-copy"),
            ("native/one", "foreign/essential-mac"),
            ("native/one", REPO + "-copy"),
            ("native/one", "Donbeave/essential-mac"),
            ("native/essential-mac/one", "foreign/repo"),
            ("scaleset/1/one", "foreign/repo"),
            ("scaleset/1/one", ""),
        ):
            with self.subTest(holder=holder, scope=scope):
                self.execute("DELETE FROM permits")
                self.execute("DELETE FROM permit_demands")
                self.held(holder=holder, scope=scope)
                self.assertFalse(self.invariants()["GATE_G3_TENANCY_ISOLATION"]["passed"])

    def test_alias_requires_explicit_scope_mapping(self):
        self.held(scope="essential-mac")
        self.policy["scopes"] = {}
        self.assertFalse(self.invariants()["GATE_G3_TENANCY_ISOLATION"]["passed"])

    def test_requested_gate_uses_its_explicit_repositories(self):
        self.held(scope="ChainArgos/cloudflare-tofu")
        self.assertFalse(self.invariants()["GATE_G3_TENANCY_ISOLATION"]["passed"])
        _, rows = monitor.evaluate_invariants(self.snapshot(), "G4", host_config=self.policy)
        self.assertTrue(next(row for row in rows if row["name"] == "GATE_G4_TENANCY_ISOLATION")["passed"])

    def test_missing_gate_evidence_is_unverifiable_even_for_empty_ledger(self):
        report = self.report()
        gate = next(row for row in report["invariants"] if row["name"].startswith("GATE_"))
        self.assertEqual(gate["status"], "UNVERIFIABLE")
        self.assertIsNone(report["evidence"]["host"]["identity"])

    def test_host_policy_must_bind_the_selected_ledger(self):
        self.policy["ledger"] = str(self.root / "different.db")
        self.write_policy()
        report = self.report("--host-config", str(self.policy_path))
        self.assertFalse(report["evidence"]["host"]["ledger_path_bound"])
        self.assertIn("does not bind", json.dumps(report))

    def test_host_and_quota_evidence_have_explicit_provenance(self):
        self.held(state="reserved")
        report = self.report("--host-config", str(self.policy_path))
        evidence = report["evidence"]
        self.assertEqual(evidence["host"]["identity"], "test-host")
        self.assertFalse(evidence["host"]["physical_identity_verified"])
        self.assertTrue(evidence["host"]["ledger_path_bound"])
        self.assertEqual(evidence["ledger_generation"], 7)
        self.assertEqual(evidence["reconciled_generation"], 7)
        self.assertEqual(evidence["admission"]["permit_state_counts"], {"reserved": 1})
        self.assertEqual(evidence["admission"]["demand_state_counts"], {"granted": 1})
        self.assertEqual(evidence["quota"]["slot_limit"], 16)
        self.assertEqual(evidence["quota"]["cpu_memory"]["status"], "UNVERIFIABLE")

    def test_malformed_duplicate_or_ambiguous_policy_fails_without_echoing_input(self):
        for raw in (
            "SECRET_MALFORMED_POLICY", '[]',
            '{"host":"SECRET_FIRST","host":"SECRET_SECOND"}',
            json.dumps(dict(self.policy, scopes={REPO: "foreign/repo"})),
            json.dumps(dict(self.policy, gates={"G3": ["essential-mac"]})),
        ):
            with self.subTest(raw=raw):
                self.policy_path.write_text(raw, encoding="utf-8")
                report = self.report("--host-config", str(self.policy_path))
                self.assertFalse(report["evidence"]["host"]["ledger_path_bound"])
                self.assertNotIn("SECRET_", json.dumps(report))

    def test_orphan_permit_and_orphan_grant_fail_consistency(self):
        self.permit()
        self.assertFalse(self.invariants()["PERMIT_DEMAND_CONSISTENCY"]["passed"])
        self.assertFalse(self.invariants()["GATE_G3_TENANCY_ISOLATION"]["passed"])
        self.execute("DELETE FROM permits")
        self.demand(state="granted")
        self.assertFalse(self.invariants()["PERMIT_DEMAND_CONSISTENCY"]["passed"])

    def test_lane_and_admission_state_mismatches_fail(self):
        self.held()
        for sql in ("UPDATE permit_demands SET lane = 'scale-set'",
                    "UPDATE permit_demands SET lane = 'native', state = 'eligible'"):
            self.execute(sql)
            self.assertFalse(self.invariants()["PERMIT_DEMAND_CONSISTENCY"]["passed"])

    def test_uncertain_terminal_hold_is_retained_occupancy(self):
        self.held(state="uncertain")
        self.execute("UPDATE permit_demands SET state = 'terminal'")
        self.assertEqual(self.snapshot()["occupied_permits"], 1)
        self.assertTrue(self.invariants()["PERMIT_DEMAND_CONSISTENCY"]["passed"])

    def test_full_capacity_bypass_is_exposed_without_claiming_historical_proof(self):
        self.execute("UPDATE permit_meta SET max_jobs = 1")
        self.demand("native/older", sequence=1, first_seen=90, updated=100)
        self.held("scaleset/7/younger", sequence=2, lane="scale-set")
        fifo = self.invariants()["FIFO_ADMISSION_ORDER"]
        self.assertEqual(fifo["status"], "UNVERIFIABLE")
        self.assertEqual(fifo["evidence"]["older_waiting_pairs"],
                         [{"older_sequence": 1, "held_sequence": 2}])

    def test_fifo_uses_first_seen_then_sequence_as_durable_order(self):
        for older_time, older_sequence, younger_time, younger_sequence in (
            (100, 1, 100, 2), (90, 2, 100, 1),
        ):
            with self.subTest(older_time=older_time):
                self.execute("DELETE FROM permits")
                self.execute("DELETE FROM permit_demands")
                self.demand("native/older", older_sequence, older_time)
                self.held("native/younger", younger_sequence, younger_time)
                pairs = self.invariants()["FIFO_ADMISSION_ORDER"]["evidence"]["older_waiting_pairs"]
                self.assertEqual(pairs, [{"older_sequence": older_sequence,
                                          "held_sequence": younger_sequence}])

    def test_held_order_inversion_is_only_diagnostic(self):
        self.held("native/older", 1, 90, 130)
        self.held("native/younger", 2, 100, 120)
        fifo = self.invariants()["FIFO_ADMISSION_ORDER"]
        self.assertEqual(fifo["status"], "UNVERIFIABLE")
        self.assertEqual(fifo["evidence"]["held_order_inversions"],
                         [{"older_sequence": 1, "younger_sequence": 2}])

    def test_retry_refresh_staleness_and_same_second_do_not_prove_bypass(self):
        self.demand("native/older", 1, 10, updated=20)
        self.held("native/younger", 2, 100, 500)
        for updated in (20, 500, 600):
            with self.subTest(updated=updated):
                self.execute("UPDATE permit_demands SET updated_unix = ? WHERE sequence = 1", (updated,))
                self.assertEqual(self.invariants()["FIFO_ADMISSION_ORDER"]["status"], "UNVERIFIABLE")

    def test_same_second_acquisitions_are_not_ordered_by_holder_or_sequence(self):
        self.held("native/z-older", 1, 90, 120)
        self.held("native/a-younger", 2, 100, 120)
        fifo = self.invariants()["FIFO_ADMISSION_ORDER"]
        self.assertEqual(fifo["evidence"]["held_order_inversions"], [])
        self.assertFalse(fifo["passed"])

    def test_empty_or_completed_ledger_cannot_certify_fifo_history(self):
        self.assertEqual(self.invariants()["FIFO_ADMISSION_ORDER"]["status"], "UNVERIFIABLE")
        self.demand(state="terminal")
        self.assertEqual(self.invariants()["FIFO_ADMISSION_ORDER"]["status"], "UNVERIFIABLE")

    def test_free_capacity_with_queue_is_not_reported_as_proven_fifo_violation(self):
        self.demand()
        invariants = self.invariants()
        self.assertNotIn("FIFO_CAPACITY_DRAIN", invariants)
        self.assertEqual(invariants["FIFO_ADMISSION_ORDER"]["status"], "UNVERIFIABLE")

    def test_missing_database_returns_json_failure_without_creating_file(self):
        missing = self.root / "SECRET_PATH.db"
        report = self.report("--db", str(missing))
        self.assertIn("error", report["ledger"])
        self.assertNotIn("SECRET_PATH", json.dumps(report))
        self.assertFalse(missing.exists())

    def test_corrupt_missing_schema_and_empty_meta_are_json_failures(self):
        corrupt = self.root / "corrupt.db"
        corrupt.write_bytes(b"SECRET_CORRUPT_DATABASE")
        empty = self.root / "empty.db"
        empty.touch()
        for path in (corrupt, empty, self.root):
            with self.subTest(path=path):
                report = self.report("--db", str(path))
                self.assertIn("error", report["ledger"])
                self.assertNotIn("SECRET_", json.dumps(report))
        self.execute("DELETE FROM permit_meta")
        self.assertIn("error", self.report()["ledger"])

    def test_invalid_states_and_timestamps_return_sanitized_json_failure(self):
        self.held()
        for sql in (
            "UPDATE permits SET state = 'SECRET_INVALID_STATE'",
            "UPDATE permits SET state = 'running', acquired_unix = -1",
            "UPDATE permits SET acquired_unix = 100, lane = 'SECRET_INVALID_LANE'",
            "UPDATE permits SET lane = 'native', generation = 8",
            "UPDATE permits SET generation = 7, pid = -1",
            "UPDATE permits SET pid = NULL, acquired_unix = 9223372036854775807, updated_unix = 9223372036854775807",
        ):
            self.execute(sql)
            report = self.report()
            self.assertIn("error", report["ledger"])
            self.assertNotIn("SECRET_", json.dumps(report))

    def test_invalid_demand_state_and_missing_order_column_fail(self):
        self.demand(state="SECRET_INVALID_STATE")
        self.assertIn("error", self.report()["ledger"])
        self.execute("DROP TABLE permit_demands")
        self.execute("CREATE TABLE permit_demands (holder TEXT)")
        self.assertIn("error", self.report()["ledger"])

    def test_secret_columns_are_never_read_or_printed(self):
        self.held()
        self.execute("ALTER TABLE permits ADD COLUMN jit_config TEXT")
        self.execute("UPDATE permits SET jit_config = 'SECRET_JIT_CONFIG'")
        self.execute("CREATE TABLE tokens (token TEXT)")
        self.execute("INSERT INTO tokens VALUES ('SECRET_TOKEN')")
        for options, wrapper in ((("--json",), None), (("--check",), None), ((), "sql")):
            result = self.cli(*options, wrapper=wrapper)
            self.assertNotIn("SECRET_", result.stdout + result.stderr)
            self.assertNotIn("jit_config", result.stdout)

    def test_database_filename_cannot_inject_uri_options(self):
        self.conn.close()
        new_path = self.root / "ledger?#mode=rw.db"
        self.db.rename(new_path)
        self.db = new_path
        self.conn = sqlite3.connect(self.db)
        self.assertEqual(self.snapshot()["max_jobs"], 16)

    def test_connection_closes_on_success_and_invalid_evidence(self):
        real_connect = sqlite3.connect
        closed = []

        class TrackedConnection(sqlite3.Connection):
            def close(self):
                closed.append(True)
                super().close()

        def connect(*args, **kwargs):
            return real_connect(*args, factory=TrackedConnection, **kwargs)

        with patch.object(monitor.sqlite3, "connect", connect):
            self.snapshot()
            self.execute("DELETE FROM permit_meta")
            self.assertIn("error", monitor.query_ledger(str(self.db)))
        self.assertEqual(closed, [True, True])

    def test_all_queries_share_one_snapshot_under_concurrent_writes(self):
        self.conn.execute("PRAGMA journal_mode = WAL")
        self.held()
        real_connect = sqlite3.connect
        writes = []

        def on_query(sql):
            if sql.startswith("SELECT holder, lane, state"):
                self.execute("DELETE FROM permits")
                self.execute("UPDATE permit_meta SET max_jobs = 0")
                writes.append(True)

        def connect(*args, **kwargs):
            connection = real_connect(*args, **kwargs)
            connection.set_trace_callback(on_query)
            return connection

        with patch.object(monitor.sqlite3, "connect", connect):
            data = self.snapshot()
        self.assertEqual(writes, [True])
        self.assertEqual((data["max_jobs"], data["occupied_permits"]), (16, 1))
        self.assertEqual(self.conn.execute("SELECT max_jobs FROM permit_meta").fetchone()[0], 0)

    def test_monitor_never_changes_ledger_bytes(self):
        self.held()
        before = self.db.read_bytes()
        self.report("--host-config", str(self.policy_path))
        self.cli(wrapper="sql")
        self.assertEqual(self.db.read_bytes(), before)

    def test_wrapper_json_and_check_preserve_failure_status(self):
        self.report(wrapper="json")
        result = self.cli(wrapper="check")
        self.assertEqual(result.returncode, 1)
        self.assertIn("UNVERIFIABLE", result.stdout)
        self.assertEqual(result.stderr, "")

    def test_wrapper_sql_honors_db_and_displays_actual_capacity(self):
        env = dict(os.environ, VELNOR_PERMIT_LEDGER=str(self.root / "wrong.db"))
        result = self.cli(wrapper="sql", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("(0 / 16)", result.stdout)
        self.assertNotIn("(0 / 4)", result.stdout)
        self.assertFalse((self.root / "wrong.db").exists())

    def test_wrapper_sql_missing_db_fails_without_creating_it(self):
        missing = self.root / "missing.db"
        result = self.cli("--db", str(missing), wrapper="sql")
        self.assertEqual(result.returncode, 1)
        self.assertIn("[FAIL]", result.stdout)
        self.assertFalse(missing.exists())

    def test_sql_does_not_silently_ignore_assertion_options(self):
        for option, value in (("--expected-max-jobs", "4"), ("--host-config", str(self.policy_path))):
            with self.subTest(option=option):
                result = self.cli(option, value, wrapper="sql")
                self.assertEqual(result.returncode, 2)
                self.assertIn("use check/json", result.stderr)

    def test_watch_displays_missing_evidence_without_crashing(self):
        output = io.StringIO()
        with patch.object(sys, "argv", ["monitor", "--watch", "--db", str(self.root / "missing.db")]), \
                patch.object(monitor.time, "sleep", side_effect=KeyboardInterrupt), redirect_stdout(output):
            self.assertEqual(monitor.main(), 130)
        self.assertIn("[FAIL] DATABASE_ACCESSIBLE", output.getvalue())
        self.assertNotIn("0 /", output.getvalue())

    def test_invalid_watch_interval_is_rejected(self):
        for interval in ("0", "-1", "nan", "inf"):
            with self.subTest(interval=interval):
                result = self.cli("--watch", "--interval", interval)
                self.assertEqual(result.returncode, 2)
                self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()

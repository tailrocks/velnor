"""Unit fixtures for the C1 permit-source roster helper."""

from __future__ import annotations

import importlib.util
import fcntl
import io
import os
import pathlib
import stat
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout


HERE = pathlib.Path(__file__).resolve().parent
HELPER = HERE.parent / "provision-permit-ledger-roster.py"
FIXTURE = HERE / "fixtures" / "permit-ledger.sources"
SPEC = importlib.util.spec_from_file_location("permit_ledger_roster", HELPER)
assert SPEC is not None and SPEC.loader is not None
roster_helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(roster_helper)


class PermitLedgerRosterTests(unittest.TestCase):
    def setUp(self) -> None:
        temp_root = pathlib.Path(tempfile.gettempdir()).resolve()
        self.temporary = tempfile.TemporaryDirectory(
            prefix="velnor-roster-test-", dir=temp_root
        )
        self.root = pathlib.Path(self.temporary.name)
        self.owner_uid = os.getuid()
        self.owner_gid = os.getgid()
        self.etc = self.root / "etc" / "velnor"
        self.data = self.root / "var" / "lib" / "velnor"
        self.roster = self.etc / "permit-ledger.sources"
        self.ledger = self.data / "permit-ledger.db"
        self.state_db = self.data / "state.db"
        self.demand_db = self.data / "scaleset.db"
        self.slot_one = self.root / "runner" / "slots" / "slot-1"
        self.slot_two = self.root / "runner" / "slots" / "slot-2"
        self.etc.mkdir(parents=True)
        self.roster.write_text(FIXTURE.read_text(encoding="utf-8"), encoding="utf-8")
        os.chmod(self.etc, 0o750)
        os.chmod(self.roster, 0o640)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def provision(self, *, check: bool = False, slots: list[pathlib.Path] | None = None) -> list[str]:
        return roster_helper.provision_roster(
            roster_path=self.roster,
            ledger_path=self.ledger,
            state_db_paths=[self.state_db],
            demand_db_paths=[self.demand_db],
            native_slot_paths=slots or [self.slot_one, self.slot_two],
            check_only=check,
            owner_uid=self.owner_uid,
            owner_gid=self.owner_gid,
        )

    def test_first_provision_creates_every_kind_and_preserves_fixture_comments(self) -> None:
        added = self.provision()

        contents = self.roster.read_text(encoding="utf-8")
        self.assertIn("# Velnor C1 permit-source roster fixture.", contents)
        self.assertIn(f"permit-ledger {self.ledger}", contents)
        self.assertIn(f"state-db {self.state_db}", contents)
        self.assertIn(f"demand-db {self.demand_db}", contents)
        self.assertIn(f"native-slot {self.slot_one}", contents)
        self.assertIn(f"native-slot {self.slot_two}", contents)
        self.assertEqual(len(added), 5)
        for path in (self.ledger, self.state_db, self.demand_db):
            self.assertTrue(path.is_file())
            self.assertEqual(path.stat().st_size, 0)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        for path in (self.slot_one, self.slot_two):
            self.assertTrue(path.is_dir())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o750)
        lock = pathlib.Path(f"{self.roster}.lock")
        self.assertTrue(lock.is_file())
        self.assertEqual(lock.stat().st_uid, self.owner_uid)
        self.assertEqual(lock.stat().st_gid, self.owner_gid)
        self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o600)

    def test_repeat_provision_is_idempotent(self) -> None:
        first_added = self.provision()
        first_contents = self.roster.read_bytes()
        first_roster_stat = self.roster.stat()

        second_added = self.provision()

        self.assertEqual(len(first_added), 5)
        self.assertEqual(second_added, [])
        self.assertEqual(self.roster.read_bytes(), first_contents)
        self.assertEqual(self.roster.stat().st_ino, first_roster_stat.st_ino)

    def test_check_does_not_create_missing_paths_or_lock(self) -> None:
        with self.assertRaises(roster_helper.RosterError):
            self.provision(check=True)

        self.assertFalse(self.ledger.exists())
        self.assertFalse(self.roster.with_name(self.roster.name + ".lock").exists())

    def test_cli_apply_rejects_missing_locked_provisioner_proof(self) -> None:
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            status = roster_helper.main(["--stock-daemon", "--root", str(self.root)])

        self.assertEqual(status, 2)
        self.assertIn("exclusive package lock", output.getvalue())
        self.assertFalse(self.ledger.exists())
        self.assertFalse(self.roster.with_name(self.roster.name + ".lock").exists())

    def test_cli_apply_rejects_an_unrelated_open_lock_fd(self) -> None:
        environment = self.etc / "velnor.env"
        environment.write_text("VELNOR_NAME=velnor\n", encoding="utf-8")
        os.chmod(environment, 0o640)
        lock_directory = self.root / "run" / "velnor"
        lock_directory.mkdir(parents=True)
        expected_lock = lock_directory / "package-transaction.lock"
        expected_lock.touch(mode=0o600)
        wrong_lock = self.root / "wrong.lock"
        wrong_lock.touch(mode=0o600)
        descriptor = os.open(wrong_lock, os.O_RDWR)
        output = io.StringIO()
        try:
            with redirect_stdout(output), redirect_stderr(output):
                status = roster_helper.main(
                    [
                        "--stock-daemon",
                        "--root",
                        str(self.root),
                        "--package-lock-fd",
                        str(descriptor),
                    ]
                )
        finally:
            os.close(descriptor)

        self.assertEqual(status, 2)
        self.assertIn("package transaction lock metadata", output.getvalue())
        self.assertFalse(self.ledger.exists())
        self.assertFalse(self.roster.with_name(self.roster.name + ".lock").exists())

    def test_cli_apply_accepts_the_locked_and_drained_provisioner_barrier(self) -> None:
        environment = self.etc / "velnor.env"
        environment.write_text("VELNOR_NAME=velnor\nVELNOR_SLOTS=4\n", encoding="utf-8")
        os.chmod(environment, 0o640)
        (self.root / "run/systemd/system").mkdir(parents=True)
        lock_directory = self.root / "run/velnor"
        lock_directory.mkdir(parents=True)
        os.chmod(lock_directory, 0o750)
        package_lock = lock_directory / "package-transaction.lock"
        package_lock.touch(mode=0o600)
        os.chmod(package_lock, 0o600)
        systemctl = self.root / "usr/bin/systemctl"
        systemctl.parent.mkdir(parents=True)
        systemctl.write_text(
            "#!/bin/sh\nprintf 'velnor-daemon.service loaded inactive dead\\n'\n",
            encoding="utf-8",
        )
        os.chmod(systemctl, 0o755)

        descriptor = os.open(package_lock, os.O_RDWR)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            output = io.StringIO()
            with redirect_stdout(output), redirect_stderr(output):
                status = roster_helper.main(
                    [
                        "--stock-daemon",
                        "--root",
                        str(self.root),
                        "--package-lock-fd",
                        str(descriptor),
                    ]
                )
        finally:
            os.close(descriptor)

        self.assertEqual(status, 0, output.getvalue())
        self.assertIn("permit-source roster updated", output.getvalue())
        self.assertEqual(stat.S_IMODE(package_lock.stat().st_mode), 0o600)
        self.assertTrue(self.roster.is_file())
        self.assertTrue((self.data / "state.db").is_file())
        self.assertEqual(
            len(list((self.root / "var/lib/velnor/runner/daemons/velnor/slots").glob("slot-*"))),
            4,
        )

    def test_check_accepts_complete_roster_without_rewriting(self) -> None:
        self.provision()
        before = self.roster.stat()
        contents = self.roster.read_bytes()

        self.assertEqual(self.provision(check=True), [])

        after = self.roster.stat()
        self.assertEqual(self.roster.read_bytes(), contents)
        self.assertEqual(after.st_ino, before.st_ino)

    def test_existing_database_contents_are_preserved(self) -> None:
        self.data.mkdir(parents=True)
        original = b"SQLite format 3\x00existing-state"
        self.state_db.write_bytes(original)
        os.chmod(self.state_db, 0o600)

        self.provision()

        self.assertEqual(self.state_db.read_bytes(), original)

    def test_existing_database_must_be_root_private_mode_0600(self) -> None:
        self.data.mkdir(parents=True)
        self.state_db.touch(mode=0o644)
        os.chmod(self.state_db, 0o644)

        with self.assertRaisesRegex(roster_helper.RosterError, "mode 0600"):
            self.provision()

        self.assertEqual(stat.S_IMODE(self.state_db.stat().st_mode), 0o644)
        self.assertFalse(self.ledger.exists())

    def test_source_paths_cannot_alias_fixed_roster_or_lock(self) -> None:
        for path in (self.roster, pathlib.Path(f"{self.roster}.lock")):
            with self.subTest(path=path):
                with self.assertRaisesRegex(roster_helper.RosterError, "alias"):
                    roster_helper.provision_roster(
                        roster_path=self.roster,
                        ledger_path=self.ledger,
                        state_db_paths=[path],
                        demand_db_paths=[],
                        native_slot_paths=[self.slot_one],
                        owner_uid=self.owner_uid,
                        owner_gid=self.owner_gid,
                    )
                self.assertFalse(pathlib.Path(f"{self.roster}.lock").exists())
                self.assertFalse(self.ledger.exists())

    def test_missing_path_component_before_dotdot_is_rejected(self) -> None:
        unsafe = self.data / "missing" / ".." / "state.db"
        with self.assertRaisesRegex(roster_helper.RosterError, "missing component"):
            roster_helper.provision_roster(
                roster_path=self.roster,
                ledger_path=self.ledger,
                state_db_paths=[unsafe],
                demand_db_paths=[],
                native_slot_paths=[self.slot_one],
                owner_uid=self.owner_uid,
                owner_gid=self.owner_gid,
            )
        self.assertFalse(self.data.exists())
        self.assertFalse(pathlib.Path(f"{self.roster}.lock").exists())

    def test_dotdot_after_regular_file_is_rejected(self) -> None:
        directory = self.data / "regular-parent"
        directory.mkdir(parents=True)
        regular = directory / "file"
        regular.write_text("not a directory", encoding="utf-8")
        unsafe = regular / ".." / "state.db"

        with self.assertRaisesRegex(roster_helper.RosterError, "non-directory component"):
            roster_helper._normalize_absolute_path(unsafe, "configured")

        guest_regular = self.root / "var/lib/velnor/file"
        guest_regular.parent.mkdir(parents=True, exist_ok=True)
        guest_regular.write_text("not a directory", encoding="utf-8")
        with self.assertRaisesRegex(roster_helper.RosterError, "non-directory component"):
            roster_helper._rooted_path(
                self.root,
                "/var/lib/velnor/file/../state.db",
                pathlib.Path("/"),
            )

    def test_roster_parser_does_not_split_non_rust_line_separators(self) -> None:
        unusual_path = self.data / "ledger.db\u0085state-db.db"
        unusual = f"permit-ledger {unusual_path}\n"
        entries = roster_helper._parse_roster(unusual, self.etc)
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0][0], "permit-ledger")
        self.assertEqual(entries[0][1], unusual_path)

    def test_environment_file_parser_matches_runner_systemd_syntax(self) -> None:
        env_file = self.etc / "velnor.env"
        env_file.write_text(
            "# comment\n"
            "; semicolon comment\n"
            "PLAIN=value with spaces  \n"
            'DOUBLE="quoted \\"inner\\" value"\n'
            "SINGLE='single quoted'\n"
            "CONTINUED=first " + "\\" + "\n"
            "second\n"
            "EMPTY=\n",
            encoding="utf-8",
        )
        os.chmod(env_file, 0o640)

        values = roster_helper._read_environment_file(
            env_file,
            owner_uid=self.owner_uid,
        )

        self.assertEqual(values["PLAIN"], "value with spaces")
        self.assertEqual(values["DOUBLE"], 'quoted "inner" value')
        self.assertEqual(values["SINGLE"], "single quoted")
        self.assertEqual(values["CONTINUED"], "first  second")
        self.assertEqual(values["EMPTY"], "")

    def test_stock_daemon_first_boot_creates_roster_lock_state_and_four_slots(self) -> None:
        env_file = self.etc / "velnor.env"
        env_file.write_text(
            "; package-managed comment\n"
            "VELNOR_NAME=velnor\n"
            "VELNOR_OPERATOR_LABEL=Build Agent\n"
            "VELNOR_UNRELATED_CONTINUATION=first " + "\\" + "\n"
            "second\n"
            "VELNOR_SLOTS=4\n"
            "VELNOR_SCALE_SET_CONFIG=/etc/velnor/scaleset.toml\n",
            encoding="utf-8",
        )
        os.chmod(env_file, 0o640)
        scale_config = self.etc / "scaleset.toml"
        scale_config.write_text('state_db = "/var/lib/velnor/scaleset.db"\n', encoding="utf-8")
        os.chmod(scale_config, 0o640)

        (self.root / "run/systemd/system").mkdir(parents=True)
        lock_directory = self.root / "run/velnor"
        lock_directory.mkdir(parents=True)
        os.chmod(lock_directory, 0o750)
        package_lock = lock_directory / "package-transaction.lock"
        package_lock.touch(mode=0o600)
        os.chmod(package_lock, 0o600)
        systemctl = self.root / "usr/bin/systemctl"
        systemctl.parent.mkdir(parents=True)
        systemctl.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        os.chmod(systemctl, 0o755)
        descriptor = os.open(package_lock, os.O_RDWR)
        output = io.StringIO()
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with redirect_stdout(output), redirect_stderr(output):
                status = roster_helper.main(
                    [
                        "--stock-daemon",
                        "--root",
                        str(self.root),
                        "--package-lock-fd",
                        str(descriptor),
                    ]
                )
        finally:
            os.close(descriptor)
        self.assertEqual(status, 0, output.getvalue())

        expected_slots = [
            self.root
            / "var/lib/velnor/runner/daemons/velnor/slots"
            / f"slot-{slot_index}"
            for slot_index in range(1, 5)
        ]
        ledger = self.data / "permit-ledger.db"
        scaleset_db = self.data / "scaleset.db"
        contents = self.roster.read_text(encoding="utf-8")
        for path in (ledger, self.state_db, scaleset_db, *expected_slots):
            self.assertIn(str(path), contents)
            self.assertTrue(path.exists())
        for path in (ledger, self.state_db, scaleset_db):
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        for path in expected_slots:
            self.assertTrue(path.is_dir())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o750)
        lock = pathlib.Path(f"{self.roster}.lock")
        self.assertTrue(lock.is_file())
        self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o600)

        with redirect_stdout(output), redirect_stderr(output):
            self.assertEqual(
                roster_helper.main(["--stock-daemon", "--root", str(self.root), "--check"]),
                0,
            )

    def test_stock_daemon_check_fails_before_creating_first_boot_state(self) -> None:
        env_file = self.etc / "velnor.env"
        env_file.write_text("VELNOR_NAME=velnor\nVELNOR_SLOTS=4\n", encoding="utf-8")
        os.chmod(env_file, 0o640)
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            self.assertEqual(
                roster_helper.main(["--stock-daemon", "--root", str(self.root), "--check"]),
                2,
            )
        self.assertFalse(self.ledger.exists())
        self.assertFalse(self.state_db.exists())
        self.assertFalse(pathlib.Path(f"{self.roster}.lock").exists())

    def test_symlinked_lock_is_refused_without_touching_target(self) -> None:
        lock = self.roster.with_name(self.roster.name + ".lock")
        target = self.root / "lock-target"
        target.write_text("keep", encoding="utf-8")
        lock.symlink_to(target)

        with self.assertRaises(roster_helper.RosterError):
            self.provision()

        self.assertEqual(target.read_text(encoding="utf-8"), "keep")
        self.assertFalse(self.ledger.exists())

    def test_unsafe_existing_lock_is_not_repaired(self) -> None:
        lock = self.roster.with_name(self.roster.name + ".lock")
        lock.touch(mode=0o600)
        os.chmod(lock, 0o660)

        with self.assertRaisesRegex(roster_helper.RosterError, "group/world writable"):
            self.provision()

        self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o660)
        self.assertFalse(self.ledger.exists())

    def test_wrong_existing_ledger_refuses_without_replacing_roster(self) -> None:
        self.ledger.parent.mkdir(parents=True)
        other_ledger = self.data / "other-ledger.db"
        other_ledger.touch(mode=0o600)
        original = f"permit-ledger {other_ledger}\n"
        self.roster.write_text(original, encoding="utf-8")
        os.chmod(self.roster, 0o640)

        with self.assertRaisesRegex(roster_helper.RosterError, "different ledger"):
            self.provision()

        self.assertEqual(self.roster.read_text(encoding="utf-8"), original)
        self.assertFalse(self.state_db.exists())

    def test_conflicting_requested_paths_fail_before_creating_any_source(self) -> None:
        original = self.roster.read_text(encoding="utf-8")

        with self.assertRaisesRegex(roster_helper.RosterError, "also be a state-db"):
            roster_helper.provision_roster(
                roster_path=self.roster,
                ledger_path=self.ledger,
                state_db_paths=[self.ledger],
                demand_db_paths=[],
                native_slot_paths=[self.slot_one],
                owner_uid=self.owner_uid,
                owner_gid=self.owner_gid,
            )

        self.assertFalse(self.data.exists())
        self.assertFalse(self.slot_one.exists())
        self.assertEqual(self.roster.read_text(encoding="utf-8"), original)

    def test_existing_demand_source_cannot_be_the_requested_ledger(self) -> None:
        self.data.mkdir(parents=True)
        self.ledger.touch(mode=0o600)
        original = f"demand-db {self.ledger}\n"
        self.roster.write_text(original, encoding="utf-8")
        os.chmod(self.roster, 0o640)

        with self.assertRaisesRegex(roster_helper.RosterError, "cannot also be a demand-db"):
            self.provision()

        self.assertEqual(self.roster.read_text(encoding="utf-8"), original)
        self.assertFalse(self.state_db.exists())
        self.assertFalse(self.slot_one.exists())

    def test_unknown_and_duplicate_roster_entries_fail_closed(self) -> None:
        for contents, message in (
            ("mystery /var/lib/velnor/db\n", "unknown roster entry"),
            (
                f"state-db {self.state_db}\nstate-db {self.state_db}\n",
                "duplicate state-db",
            ),
        ):
            with self.subTest(message=message):
                self.roster.write_text(contents, encoding="utf-8")
                os.chmod(self.roster, 0o640)
                with self.assertRaisesRegex(roster_helper.RosterError, message):
                    self.provision()
                self.assertEqual(self.roster.read_text(encoding="utf-8"), contents)

    def test_stale_unrequested_source_is_not_silently_recreated(self) -> None:
        self.data.mkdir(parents=True)
        stale = self.data / "retired-state.db"
        self.roster.write_text(
            f"permit-ledger {self.ledger}\nstate-db {stale}\n",
            encoding="utf-8",
        )
        os.chmod(self.roster, 0o640)

        with self.assertRaisesRegex(roster_helper.RosterError, "path is missing"):
            self.provision()

        self.assertFalse(stale.exists())
        self.assertFalse(self.state_db.exists())

    def test_symlinked_database_and_world_writable_roster_are_rejected(self) -> None:
        self.data.mkdir(parents=True)
        target = self.data / "target.db"
        target.touch(mode=0o600)
        self.ledger.symlink_to(target)
        with self.assertRaisesRegex(roster_helper.RosterError, "symlink"):
            self.provision()

        self.ledger.unlink()
        os.chmod(self.roster, 0o666)
        with self.assertRaisesRegex(roster_helper.RosterError, "group/world writable"):
            self.provision()

    def test_relative_existing_roster_paths_resolve_from_roster_directory(self) -> None:
        self.data.mkdir(parents=True)
        self.ledger.touch(mode=0o600)
        self.state_db.touch(mode=0o600)
        relative_ledger = os.path.relpath(self.ledger, self.etc)
        relative_state = os.path.relpath(self.state_db, self.etc)
        self.roster.write_text(
            f"permit-ledger {relative_ledger}\nstate-db={relative_state}\n",
            encoding="utf-8",
        )
        os.chmod(self.roster, 0o640)

        added = self.provision()

        self.assertEqual(len(added), 3)
        contents = self.roster.read_text(encoding="utf-8")
        self.assertIn(f"permit-ledger {relative_ledger}", contents)
        self.assertIn(f"state-db={relative_state}", contents)
        self.assertIn(f"demand-db {self.demand_db}", contents)


if __name__ == "__main__":
    unittest.main()

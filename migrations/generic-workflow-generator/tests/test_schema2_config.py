#!/usr/bin/env python3
"""Deterministic fixtures for the one-time generation-config migration."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


MIGRATION_DIR = Path(__file__).resolve().parents[1]
HELPER = MIGRATION_DIR / "schema2-config.py"
SHELL_CONFIG = MIGRATION_DIR / "schema2-config.sh"
FIXTURE = Path(__file__).parent / "fixtures" / "schema1-legacy.toml"
ROUNDTRIP_FIXTURE = Path(__file__).parent / "fixtures" / "schema2-roundtrip.toml"
REVISION = "a" * 40


def migrate(path: Path, repository: str = "example/legacy") -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(HELPER),
            str(path),
            "--repository",
            repository,
            "--revision",
            REVISION,
            "--default-branch",
            "main",
        ],
        check=False,
        capture_output=True,
        text=True,
    )


class Schema2ConfigMigrationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="velnor-schema2-config-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.config = self.root / ".github-gen" / "velnor-workflow.toml"

    def test_multiline_arrays_comments_and_old_runner_aliases_round_trip(self) -> None:
        self.config.parent.mkdir(parents=True)
        shutil.copyfile(FIXTURE, self.config)

        result = migrate(self.config)
        self.assertEqual(result.returncode, 0, result.stderr)
        migrated_text = self.config.read_text(encoding="utf-8")
        migrated = tomllib.loads(migrated_text)

        self.assertEqual(migrated["schema"], 2)
        self.assertEqual(migrated["generator"]["revision"], REVISION)
        self.assertEqual(migrated["workflow"]["providers"], ["github-hosted", "velnor"])
        self.assertEqual(migrated["workflow"]["automatic_providers"], ["github-hosted"])
        self.assertEqual(migrated["workflow"]["default_dispatch_providers"], ["github-hosted"])
        self.assertEqual(migrated["workflow"]["default_branch"], "release/candidate")
        self.assertEqual(
            migrated["workflow"]["selectors"]["github-hosted"]["runs_on"],
            ["ubuntu-26.04"],
        )
        self.assertEqual(
            migrated["workflow"]["selectors"]["velnor"]["runs_on"],
            ["self-hosted", "velnor-migration-fixture"],
        )
        self.assertEqual(
            migrated["release"]["reason"],
            "First line.\nSecond line # this hash is content, not a TOML comment.\n",
        )
        self.assertEqual(migrated["release"]["job"][0]["provider"], "github-hosted")
        self.assertEqual(migrated["release"]["job"][0]["platform"], "linux-x64")
        self.assertEqual(migrated["release"]["job"][0]["tasks"], ["build-release"])
        self.assertEqual(
            [(row["provider"], row["platform"]) for row in migrated["check_profile"]],
            [
                ("github-hosted", "linux-x64"),
                ("github-hosted", "macos-arm64"),
                ("velnor", "linux-x64"),
            ],
        )
        self.assertNotIn("runner", migrated["release"]["job"][0])
        self.assertNotIn("runner", migrated["check_profile"][0])
        self.assertNotIn("automatic_lanes", migrated["workflow"])
        self.assertNotIn("# both old execution lanes", migrated_text)
        self.assertNotIn("# keep array comments out of the value", migrated_text)
        self.assertIn("# this hash is content", migrated_text)

        # A second run is byte-idempotent and leaves the comments-free S2 form
        # unchanged rather than repeatedly rewriting the repository config.
        before = self.config.read_bytes()
        second = migrate(self.config)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(self.config.read_bytes(), before)

    def test_checked_in_schema2_fixture_is_exact_converter_output(self) -> None:
        self.config.parent.mkdir(parents=True)
        shutil.copyfile(FIXTURE, self.config)

        result = migrate(self.config)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.config.read_bytes(), ROUNDTRIP_FIXTURE.read_bytes())

    def test_missing_file_gets_complete_default_schema2_config(self) -> None:
        result = migrate(self.config, repository="owner/repo")
        self.assertEqual(result.returncode, 0, result.stderr)
        document = tomllib.loads(self.config.read_text(encoding="utf-8"))
        self.assertEqual(document["schema"], 2)
        self.assertEqual(document["generator"], {"repository": "owner/repo", "revision": REVISION})
        self.assertEqual(document["workflow"]["providers"], ["github-hosted", "velnor"])
        self.assertEqual(document["workflow"]["default_branch"], "main")

    def test_malformed_toml_is_reported_without_touching_original(self) -> None:
        self.config.parent.mkdir(parents=True)
        original = b'schema = 1\n\n[workflow]\nproviders = ["github-hosted",\n'
        self.config.write_bytes(original)

        result = migrate(self.config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not valid TOML", result.stderr)
        self.assertEqual(self.config.read_bytes(), original)

    def test_unknown_s1_nested_field_fails_before_write(self) -> None:
        self.config.parent.mkdir(parents=True)
        original = (
            b'schema = 1\n\n[generator]\nrepository = "example/legacy"\n'
            b'\n[workflow]\nrunners = "both"\nnew_unmapped_field = "keep me"\n'
        )
        self.config.write_bytes(original)

        result = migrate(self.config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("new_unmapped_field is not an S2 field", result.stderr)
        self.assertIn("add an explicit S2 field mapping", result.stderr)
        self.assertEqual(self.config.read_bytes(), original)

    def test_schema2_legacy_runner_field_is_rejected_without_write(self) -> None:
        self.config.parent.mkdir(parents=True)
        original = (
            b'schema = 2\n\n[generator]\nrepository = "example/legacy"\n'
            b'\n[workflow]\nrunners = "both"\n'
        )
        self.config.write_bytes(original)

        result = migrate(self.config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("workflow.runners is not an S2 field", result.stderr)
        self.assertEqual(self.config.read_bytes(), original)

    def test_check_profile_without_convertible_placement_fails_before_write(self) -> None:
        self.config.parent.mkdir(parents=True)
        original = (
            b'schema = 1\n\n[generator]\nrepository = "example/legacy"\n'
            b'\n[[check_profile]]\nid = "smoke"\ntasks = ["check-smoke"]\n'
        )
        self.config.write_bytes(original)

        result = migrate(self.config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("needs provider and platform", result.stderr)
        self.assertEqual(self.config.read_bytes(), original)

    def test_symlinked_config_directory_is_not_followed(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        protected = outside / "velnor-workflow.toml"
        original = b"schema = 1\n"
        protected.write_bytes(original)
        self.config.parent.symlink_to(outside, target_is_directory=True)

        result = migrate(self.config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to write through symlinked config directory", result.stderr)
        self.assertEqual(protected.read_bytes(), original)

    def test_shell_entrypoints_parse(self) -> None:
        for script in (SHELL_CONFIG, MIGRATION_DIR / "migrate-repo.sh"):
            with self.subTest(script=script.name):
                result = subprocess.run(
                    ["bash", "-n", str(script)], check=False, capture_output=True, text=True
                )
                self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()

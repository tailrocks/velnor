"""Fake filesystem tests for the C1 APT evidence fingerprint."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest


HERE = pathlib.Path(__file__).resolve().parent
HELPER = HERE.parent / "apt-input-fingerprint.py"
SPEC = importlib.util.spec_from_file_location("apt_input_fingerprint", HELPER)
assert SPEC is not None and SPEC.loader is not None
fingerprint_helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fingerprint_helper)


class AptInputFingerprintTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="velnor-apt-fingerprint-")
        self.root = pathlib.Path(self.temporary.name)
        self.config = self.root / "apt.conf"
        self.source_list = self.root / "sources.list"
        self.source_parts = self.root / "sources.list.d"
        self.keyring_dir = self.root / "usr" / "share" / "keyrings"
        self.config.write_text('APT::Get::AllowUnauthenticated "false";\n', encoding="utf-8")
        self.source_list.write_text("", encoding="utf-8")
        self.source_parts.mkdir()
        self.keyring_dir.mkdir(parents=True)
        self.archive_gpg = self.keyring_dir / "debian-archive-keyring.gpg"
        self.archive_pgp = self.keyring_dir / "debian-archive-keyring.pgp"
        self.archive_pgp.write_bytes(b"initial Debian archive keyring")
        self.archive_gpg.symlink_to(self.archive_pgp.name)
        fingerprint_helper.DEBIAN_ARCHIVE_KEYRING_GPG = self.archive_gpg
        fingerprint_helper.DEBIAN_ARCHIVE_KEYRING_PGP = self.archive_pgp

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def fingerprint(self) -> str:
        return fingerprint_helper.fingerprint(
            self.config,
            self.source_list,
            self.source_parts,
            [self.archive_gpg, self.archive_pgp],
        )

    def test_trixie_archive_keyring_alias_tracks_canonical_target(self) -> None:
        original = self.fingerprint()
        self.archive_pgp.write_bytes(b"rotated Debian archive keyring")

        self.assertNotEqual(original, self.fingerprint())

    def test_known_archive_alias_rejects_a_different_target(self) -> None:
        rogue_keyring = self.keyring_dir / "other-keyring.pgp"
        rogue_keyring.write_bytes(b"unapproved keyring")
        self.archive_gpg.unlink()
        self.archive_gpg.symlink_to(rogue_keyring.name)

        with self.assertRaisesRegex(ValueError, "unapproved keyring alias"):
            self.fingerprint()

    def test_arbitrary_apt_input_symlink_remains_rejected(self) -> None:
        target = self.root / "outside.list"
        target.write_text("deb https://example.invalid/debian trixie main\n", encoding="utf-8")
        symlink = self.source_parts / "external.sources"
        symlink.symlink_to(target)

        with self.assertRaisesRegex(ValueError, "APT input is a symlink"):
            self.fingerprint()


if __name__ == "__main__":
    unittest.main()

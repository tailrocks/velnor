"""Executable proof for the G1 bootstrap prefetch contract."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tools.bootstrap_prefetch import (
    DEFAULT_GIT_REV,
    DEFAULT_GIT_URL,
    PrefetchError,
    build_bundle,
    copy_cache_bounded,
    fetch_environment,
    git_census,
    measure_closure,
    network_policy,
    validate_bundle,
)


ROOT = Path(__file__).resolve().parents[1]
SOURCE_HEAD = "3ed0023b038335d7b22dfa2758457e3808f777ee"
SOURCE_TREE = "45be601efb57e8d9da424a07e9115beee93a1564"
GIT_SOURCE = ((DEFAULT_GIT_URL, DEFAULT_GIT_REV),)


def _cargo_tool(name: str) -> str:
    return subprocess.check_output(["rustup", "which", name], text=True).strip()


class BootstrapPrefetchTests(unittest.TestCase):
    def _bundle(self, temporary: Path) -> Path:
        bundle = temporary / "bundle"
        build_bundle(
            ROOT,
            bundle,
            source_head_sha=SOURCE_HEAD,
            source_tree_sha=SOURCE_TREE,
            target_package="velnor-workflow",
            producer_closure_nodes=115,
            expected_git=GIT_SOURCE,
        )
        return bundle

    def test_actual_current_closure_is_115_and_termrock_is_exact(self) -> None:
        count, git_lines = measure_closure(
            ROOT,
            cargo_home=Path.home() / ".cargo",
            cargo_bin=_cargo_tool("cargo"),
            rustc_bin=_cargo_tool("rustc"),
            path=os.environ["PATH"],
        )
        self.assertEqual(count, 115)
        self.assertEqual(len(git_lines), 1)
        self.assertIn(f"#{DEFAULT_GIT_REV[:8]}", git_lines[0])

    def test_current_workspace_builds_source_free_bundle_and_metadata_parses(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-prefetch-test-") as name:
            temporary = Path(name)
            bundle = self._bundle(temporary)
            manifest = validate_bundle(bundle, expected_git=GIT_SOURCE)
            self.assertEqual(manifest["workspace_manifest_count"], 10)
            self.assertEqual(manifest["lock_package_count"], 438)
            rust_files = sorted(bundle.rglob("*.rs"))
            self.assertTrue(rust_files)
            self.assertTrue(all(path.stat().st_size == 0 for path in rust_files))
            self.assertTrue(
                all("/.prefetch-targets/" in f"/{path.relative_to(bundle).as_posix()}" for path in rust_files)
            )
            self.assertFalse(any(path.name == "build.rs" for path in bundle.rglob("*")))
            self.assertFalse((bundle / ".cargo").exists())

            home = temporary / "home"
            cargo_home = temporary / "cargo-home"
            home.mkdir()
            cargo_home.mkdir()
            environment = fetch_environment(
                home,
                cargo_home,
                path=f"{Path(_cargo_tool('cargo')).parent}:{Path(_cargo_tool('rustc')).parent}:/usr/bin:/bin",
            )
            environment["CARGO_NET_OFFLINE"] = "true"
            result = subprocess.run(
                [
                    _cargo_tool("cargo"),
                    "metadata",
                    "--locked",
                    "--offline",
                    "--no-deps",
                    "--format-version",
                    "1",
                    "--manifest-path",
                    str(bundle / "Cargo.toml"),
                ],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_hostile_target_path_and_git_source_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-hostile-") as name:
            root = Path(name)
            member = root / "member"
            member.mkdir()
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["member"]\nresolver = "3"\n',
                encoding="utf-8",
            )
            (root / "Cargo.lock").write_text(
                'version = 4\n\n[[package]]\nname = "member"\nversion = "0.1.0"\n',
                encoding="utf-8",
            )
            (root / "rust-toolchain.toml").write_text(
                '[toolchain]\nchannel = "1.98.1"\n',
                encoding="utf-8",
            )
            (member / "Cargo.toml").write_text(
                '[package]\nname = "member"\nversion = "0.1.0"\nedition = "2024"\n'
                '[lib]\npath = "../../escape.rs"\n',
                encoding="utf-8",
            )
            with self.assertRaises(PrefetchError):
                build_bundle(
                    root,
                    root / "bundle",
                    source_head_sha=SOURCE_HEAD,
                    source_tree_sha=SOURCE_TREE,
                    target_package="member",
                    producer_closure_nodes=0,
                    expected_git=GIT_SOURCE,
                )

            (member / "Cargo.toml").write_text(
                '[package]\nname = "member"\nversion = "0.1.0"\nedition = "2024"\n'
                '[dependencies]\nother = { git = "https://evil.invalid/other.git", rev = "'
                + DEFAULT_GIT_REV
                + '" }\n',
                encoding="utf-8",
            )
            with self.assertRaises(PrefetchError):
                build_bundle(
                    root,
                    root / "bundle-git",
                    source_head_sha=SOURCE_HEAD,
                    source_tree_sha=SOURCE_TREE,
                    target_package="member",
                    producer_closure_nodes=0,
                    expected_git=GIT_SOURCE,
                )

            (member / "Cargo.toml").write_text(
                '[package]\nname = "member"\nversion = "0.1.0"\nedition = "2024"\n'
                '[dependencies]\nother = { version = "1", registry = "evil" }\n',
                encoding="utf-8",
            )
            with self.assertRaises(PrefetchError):
                build_bundle(
                    root,
                    root / "bundle-registry",
                    source_head_sha=SOURCE_HEAD,
                    source_tree_sha=SOURCE_TREE,
                    target_package="member",
                    producer_closure_nodes=0,
                    expected_git=GIT_SOURCE,
                )

    def test_self_excluding_digest_and_numeric_schema(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-schema-") as name:
            bundle = self._bundle(Path(name))
            manifest_path = bundle / "manifest.json"
            value = json.loads(manifest_path.read_text(encoding="utf-8"))
            value["workspace_manifest_count"] = "10"
            manifest_path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(PrefetchError):
                validate_bundle(bundle, expected_git=GIT_SOURCE)

            value["workspace_manifest_count"] = 10
            value["bundle_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(PrefetchError):
                validate_bundle(bundle, expected_git=GIT_SOURCE)

    def test_fetch_environment_ignores_ambient_credentials(self) -> None:
        old = os.environ.get("GH_TOKEN")
        os.environ["GH_TOKEN"] = "must-not-cross-boundary"
        try:
            environment = fetch_environment(Path("/tmp/home"), Path("/tmp/cargo"))
        finally:
            if old is None:
                os.environ.pop("GH_TOKEN", None)
            else:
                os.environ["GH_TOKEN"] = old
        self.assertNotIn("GH_TOKEN", environment)
        self.assertNotIn("GITHUB_TOKEN", environment)
        self.assertNotIn("HTTP_PROXY", environment)
        self.assertEqual(environment["RUSTC"], "/bin/false")
        self.assertEqual(environment["GIT_CONFIG_GLOBAL"], "/dev/null")
        self.assertFalse(network_policy()["enforced"])

    def test_git_census_requires_exact_single_source(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-git-census-") as name:
            root = Path(name)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "Cargo.toml").write_text("[package]\nname='stub'\nversion='0.1.0'\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(source), "add", "Cargo.toml"], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(source),
                    "-c",
                    "user.name=bootstrap-test",
                    "-c",
                    "user.email=bootstrap-test@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "fixture",
                ],
                check=True,
            )
            revision = subprocess.check_output(
                ["git", "-C", str(source), "rev-parse", "HEAD"],
                text=True,
            ).strip()
            cargo_home = root / "cargo-home"
            db = cargo_home / "git" / "db" / "termrock-fixture"
            checkout = cargo_home / "git" / "checkouts" / "termrock-fixture" / revision
            db.parent.mkdir(parents=True)
            checkout.parent.mkdir(parents=True)
            subprocess.run(["git", "clone", "-q", "--bare", str(source), str(db)], check=True)
            subprocess.run(["git", "clone", "-q", str(db), str(checkout)], check=True)
            records = git_census(cargo_home, ((DEFAULT_GIT_URL, revision),))
            self.assertEqual(records[0]["rev"], revision)

            extra = cargo_home / "git" / "db" / "unreviewed"
            subprocess.run(["git", "clone", "-q", "--bare", str(source), str(extra)], check=True)
            with self.assertRaises(PrefetchError):
                git_census(cargo_home, ((DEFAULT_GIT_URL, revision),))

    def test_bounded_cache_copy_rejects_links_and_quota(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-copy-") as name:
            root = Path(name)
            source = root / "source"
            source.mkdir()
            (source / "nested").mkdir()
            (source / "nested" / "ok").write_bytes(b"ok")
            copied = root / "copied"
            self.assertEqual(copy_cache_bounded(source, copied), {"files": 1, "bytes": 2})

            linked = root / "linked"
            linked.mkdir()
            os.symlink(source / "nested" / "ok", linked / "link")
            with self.assertRaises(PrefetchError):
                copy_cache_bounded(linked, root / "linked-copy")

            oversized = root / "oversized"
            oversized.mkdir()
            (oversized / "large").write_bytes(b"123")
            with self.assertRaises(PrefetchError):
                copy_cache_bounded(oversized, root / "oversized-copy", max_bytes=2)


if __name__ == "__main__":
    unittest.main()

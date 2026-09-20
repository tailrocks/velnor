"""Executable proof for the G1 bootstrap prefetch contract."""

from __future__ import annotations

import json
import hashlib
import os
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

from tools.bootstrap_prefetch import (
    BaseOwnedContract,
    BundleContract,
    PrefetchError,
    BASE_CONTRACT_PRODUCER,
    BASE_CONTRACT_SCHEMA,
    builder_recipe_contract,
    bundle_contract_payload,
    build_bundle,
    copy_cache_bounded,
    fetch_environment,
    git_census,
    measure_closure,
    measure_closure_contract,
    network_policy,
    load_base_owned_contract,
    validate_bundle,
    _validate_git_url,
)


ROOT = Path(__file__).resolve().parents[1]
TARGET_MANIFEST = next(
    candidate
    for candidate in sorted((ROOT / "crates").glob("*/Cargo.toml"))
    if tomllib.loads(candidate.read_text(encoding="utf-8"))
    .get("package", {})
    .get("name", "")
    .endswith("-workflow")
)
TARGET_PACKAGE = tomllib.loads(TARGET_MANIFEST.read_text(encoding="utf-8"))["package"]["name"]
TEST_BASE_IMAGE_DIGEST = hashlib.sha256(b"owned unit-test builder identity").hexdigest()


def _cargo_tool(name: str) -> str:
    return subprocess.check_output(["rustup", "which", name], text=True).strip()


def _git(directory: Path, *args: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", "-C", str(directory), *args],
        check=check,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def _source_revision(directory: Path = ROOT) -> str:
    return _git(directory, "rev-parse", "HEAD")


def _source_tree(directory: Path = ROOT) -> str:
    return _git(directory, "rev-parse", "HEAD^{tree}")


def _cache_git_tree(url: str, revision: str) -> str:
    db_root = Path.home() / ".cargo" / "git" / "db"
    for database in sorted(db_root.iterdir()):
        if not database.is_dir():
            continue
        probe = subprocess.run(
            ["git", "-C", str(database), "cat-file", "-e", f"{revision}^{{commit}}"],
            check=False,
            capture_output=True,
            text=True,
        )
        if probe.returncode == 0:
            return _git(database, "rev-parse", f"{revision}^{{tree}}")
    raise AssertionError(f"missing cached Git revision {url}@{revision}")


def _reviewed_git_sources() -> tuple[tuple[str, str], ...]:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    result: set[tuple[str, str]] = set()
    for package in lock["package"]:
        source = package.get("source")
        if not isinstance(source, str) or not source.startswith("git+"):
            continue
        before, revision = source[4:].rsplit("#", 1)
        url, lock_revision = before.split("?rev=", 1)
        if lock_revision != revision:
            raise AssertionError("lock Git revision fragment differs")
        result.add((url, revision))
    return tuple(sorted(result))


GIT_SOURCE = _reviewed_git_sources()


def _commit(directory: Path, message: str) -> str:
    _git(directory, "add", "-A")
    _git(
        directory,
        "-c",
        "user.name=bootstrap-test",
        "-c",
        "user.email=bootstrap-test@example.invalid",
        "commit",
        "-qm",
        message,
    )
    return _source_revision(directory)


def _fixture(directory: Path, *, workspace_git_dependency: bool = False) -> str:
    member = directory / "member"
    member.mkdir(parents=True)
    (member / "src").mkdir()
    (member / "src" / "lib.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
    workspace_dependency = ""
    member_dependency = ""
    if workspace_git_dependency:
        workspace_dependency = (
            "\n[workspace.dependencies]\n"
            'hidden = { git = "https://evil.invalid/hidden.git", '
            'rev = "0000000000000000000000000000000000000000" }\n'
        )
        member_dependency = "\n[dependencies]\nhidden.workspace = true\n"
    (directory / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["member"]\nresolver = "3"\n'
        + workspace_dependency,
        encoding="utf-8",
    )
    (directory / "Cargo.lock").write_text(
        'version = 4\n\n[[package]]\nname = "member"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    (directory / "rust-toolchain.toml").write_text(
        '[toolchain]\nchannel = "stable"\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "member"\nversion = "0.1.0"\nedition = "2024"\n'
        '[lib]\npath = "src/lib.rs"\n'
        + member_dependency,
        encoding="utf-8",
    )
    subprocess.run(["git", "init", "-q", str(directory)], check=True)
    return _commit(directory, "fixture")


def _fixture_build(directory: Path, output: Path, revision: str) -> dict[str, object]:
    return build_bundle(
        directory,
        output,
        source_revision=revision,
        target_package="member",
        target_triple="x86_64-unknown-linux-gnu",
        cargo_home=Path.home() / ".cargo",
        builder_base_digest=TEST_BASE_IMAGE_DIGEST,
        cargo_bin=_cargo_tool("cargo"),
        rustc_bin=_cargo_tool("rustc"),
        path=os.environ["PATH"],
        reviewed_git=(),
    )


def _trusted_contract(
    source_root: Path,
    manifest: dict[str, object],
    *,
    target_manifest: str,
    target_package: str,
    path: str,
    candidate_root: Path,
    base_digest: str = TEST_BASE_IMAGE_DIGEST,
) -> BaseOwnedContract:
    cargo = _cargo_tool("cargo")
    rustc = _cargo_tool("rustc")
    head = _source_revision(source_root)
    tree = _source_tree(source_root)
    closure = measure_closure_contract(
        source_root,
        manifest=target_manifest,
        package_name=target_package,
        source_head_sha=head,
        source_tree_sha=tree,
        target="x86_64-unknown-linux-gnu",
        cargo_home=Path.home() / ".cargo",
        cargo_bin=cargo,
        rustc_bin=rustc,
        path=path,
    )
    recipe = builder_recipe_contract(
        cargo_bin=cargo,
        rustc_bin=rustc,
        path=path,
        builder_base_digest=base_digest,
        target_package=target_package,
        target_triple="x86_64-unknown-linux-gnu",
        manifest=target_manifest,
    )
    git_sources = GIT_SOURCE if source_root == ROOT else ()
    git_provenance = tuple(
        (url, revision, _cache_git_tree(url, revision))
        for url, revision in git_sources
    )
    data = BundleContract(
        head,
        tree,
        str(manifest["dependency_contract_digest"]),
        str(manifest["toolchain_input_digest"]),
        closure,
        recipe,
        tuple(git_sources),
        git_provenance,
    )
    trusted_root = Path(tempfile.mkdtemp(prefix="bootstrap-base-contract-"))
    envelope = {
        "schema": BASE_CONTRACT_SCHEMA,
        "producer": BASE_CONTRACT_PRODUCER,
        "contract": bundle_contract_payload(data),
    }
    raw = json.dumps(
        envelope,
        ensure_ascii=True,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    contract_path = trusted_root / "contract.json"
    contract_path.write_bytes(raw)
    contract_path.chmod(0o644)
    return load_base_owned_contract(
        contract_path,
        trusted_root=trusted_root,
        candidate_root=candidate_root,
        expected_sha256=hashlib.sha256(raw).hexdigest(),
    )


def _rewrite_manifest(path: Path, value: dict[str, object]) -> None:
    base = dict(value)
    base.pop("bundle_sha256", None)
    value["bundle_sha256"] = hashlib.sha256(
        json.dumps(
            base,
            ensure_ascii=True,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()
    path.write_text(json.dumps(value), encoding="utf-8")


class BootstrapPrefetchTests(unittest.TestCase):
    def _bundle(self, temporary: Path) -> Path:
        bundle = temporary / "bundle"
        build_bundle(
            ROOT,
            bundle,
            source_revision=_source_revision(),
            target_package=TARGET_PACKAGE,
            cargo_home=Path.home() / ".cargo",
            builder_base_digest=TEST_BASE_IMAGE_DIGEST,
            cargo_bin=_cargo_tool("cargo"),
            rustc_bin=_cargo_tool("rustc"),
            path=os.environ["PATH"],
            reviewed_git=GIT_SOURCE,
        )
        return bundle

    def test_actual_current_closure_is_measured_and_git_revision_is_full(self) -> None:
        count = measure_closure(
            ROOT,
            manifest=TARGET_MANIFEST.relative_to(ROOT).as_posix(),
            package_name=TARGET_PACKAGE,
            cargo_home=Path.home() / ".cargo",
            cargo_bin=_cargo_tool("cargo"),
            rustc_bin=_cargo_tool("rustc"),
            path=os.environ["PATH"],
        )
        self.assertEqual(count, 115)
        self.assertTrue(GIT_SOURCE)
        self.assertTrue(all(len(revision) == 40 for _, revision in GIT_SOURCE))

    def test_current_workspace_builds_source_free_bundle_and_metadata_parses(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-prefetch-test-") as name:
            temporary = Path(name)
            bundle = self._bundle(temporary)
            manifest = json.loads((bundle / "manifest.json").read_text(encoding="utf-8"))
            manifest = validate_bundle(
                bundle,
                contract=_trusted_contract(
                    ROOT,
                    manifest,
                    target_manifest=TARGET_MANIFEST.relative_to(ROOT).as_posix(),
                    target_package=TARGET_PACKAGE,
                    path=os.environ["PATH"],
                    candidate_root=temporary,
                ),
                reviewed_git=GIT_SOURCE,
                cargo_home=Path.home() / ".cargo",
                verify_git_cache=False,
            )
            self.assertEqual(manifest["workspace_manifest_count"], 10)
            self.assertEqual(manifest["lock_package_count"], 438)
            self.assertEqual(manifest["closure_package_count"], 115)
            self.assertEqual(
                (bundle / "Cargo.lock").read_bytes(),
                subprocess.check_output(
                    ["git", "-C", str(ROOT), "show", f"{_source_revision()}:Cargo.lock"]
                ),
            )
            rust_files = sorted(bundle.rglob("*.rs"))
            self.assertTrue(rust_files)
            self.assertTrue(all(path.stat().st_size == 0 for path in rust_files))
            self.assertTrue(
                all(
                    "/.prefetch-targets/"
                    in f"/{path.relative_to(bundle).as_posix()}"
                    for path in rust_files
                )
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

    def test_source_inputs_reject_wrong_head_no_repo_unsafe_mode_hardlink_and_fifo(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-source-hostile-") as name:
            temporary = Path(name)
            wrong_head = _git(ROOT, "rev-parse", "HEAD^")
            with self.assertRaises(PrefetchError):
                build_bundle(
                    ROOT,
                    temporary / "wrong-head",
                    source_revision=wrong_head,
                    target_package=TARGET_PACKAGE,
                    cargo_home=Path.home() / ".cargo",
                    builder_base_digest=TEST_BASE_IMAGE_DIGEST,
                    reviewed_git=GIT_SOURCE,
                )

            no_repo = temporary / "no-repo"
            _fixture(no_repo)
            shutil.rmtree(no_repo / ".git")
            with self.assertRaises(PrefetchError):
                build_bundle(
                    no_repo,
                    temporary / "no-repo-bundle",
                    source_revision=None,
                    target_package="member",
                    cargo_home=Path.home() / ".cargo",
                    builder_base_digest=TEST_BASE_IMAGE_DIGEST,
                    reviewed_git=(),
                )

            for kind in ("mode", "hardlink", "fifo"):
                clone = temporary / kind
                subprocess.run(["git", "clone", "-q", "--local", str(ROOT), str(clone)], check=True)
                if kind == "mode":
                    (clone / "Cargo.toml").chmod(0o755)
                elif kind == "hardlink":
                    os.link(clone / "Cargo.toml", clone / "Cargo.toml.link")
                    _commit(clone, "hardlink")
                else:
                    if not hasattr(os, "mkfifo"):
                        self.skipTest("FIFO is unavailable")
                    (clone / "Cargo.lock").unlink()
                    os.mkfifo(clone / "Cargo.lock")
                with self.assertRaises(PrefetchError):
                    build_bundle(
                        clone,
                        temporary / f"{kind}-bundle",
                        source_revision=_source_revision(clone),
                        target_package=TARGET_PACKAGE,
                        cargo_home=Path.home() / ".cargo",
                        builder_base_digest=TEST_BASE_IMAGE_DIGEST,
                        reviewed_git=GIT_SOURCE,
                    )

    def test_workspace_dependencies_are_in_the_git_source_census(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-workspace-source-") as name:
            root = Path(name)
            revision = _fixture(root, workspace_git_dependency=True)
            with self.assertRaises(PrefetchError):
                build_bundle(
                    root,
                    root.parent / "workspace-source-bundle",
                    source_revision=revision,
                    target_package="member",
                    cargo_home=Path.home() / ".cargo",
                    builder_base_digest=TEST_BASE_IMAGE_DIGEST,
                    reviewed_git=(),
                )

    def test_git_url_requires_canonical_owner_repository(self) -> None:
        self.assertEqual(
            _validate_git_url(
                "git+https://github.com/example/repo.git",
                "fixture URL",
            ),
            "https://github.com/example/repo.git",
        )
        for value in (
            "https://github.com/",
            "https://github.com////",
            "https://github.com/owner",
            "https://github.com/owner/",
            "https://github.com/.git",
            "https://github.com/owner/.git",
            "https://github.com/example/repo?",
            "https://github.com/example/repo#",
            "https://github.com/example/repo%2F.git",
            "https://github.com/example/repo%5C.git",
            "https://github.com/example/repo%3Fq.git",
            "https://github.com/example/repo/../other.git",
            "https://github.com/example//repo.git",
            "https://github.com/example/repo;param.git",
            "https://github.com/example/repo%00.git",
            "https://github.com/example/repo%2e%2e/other.git",
            "https://github.com/example/repo\\other.git",
            "https://github.com/example/repo.git@other",
            "https://github.com/Example/repo.git",
            "https://github.com/example/repo.git.git",
        ):
            with self.subTest(value=value), self.assertRaises(PrefetchError):
                _validate_git_url(value, "fixture URL")

    def test_dependency_digest_excludes_source_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-source-identity-") as name:
            root = Path(name)
            first = _fixture(root)
            with tempfile.TemporaryDirectory(prefix="bootstrap-bundles-") as bundles:
                first_manifest = _fixture_build(root, Path(bundles) / "first", first)
                (root / "member" / "src" / "lib.rs").write_text(
                    "pub fn value() -> u8 { 2 }\n",
                    encoding="utf-8",
                )
                second = _commit(root, "code-only")
                second_manifest = _fixture_build(root, Path(bundles) / "second", second)
            self.assertNotEqual(first_manifest["source_head_sha"], second_manifest["source_head_sha"])
            self.assertNotEqual(first_manifest["source_tree_sha"], second_manifest["source_tree_sha"])
            self.assertEqual(
                first_manifest["dependency_contract_digest"],
                second_manifest["dependency_contract_digest"],
            )

    def test_trusted_contract_rejects_forged_count_source_recipe_and_directories(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-contract-" ) as name:
            temporary = Path(name)
            source = temporary / "source"
            source.mkdir()
            revision = _fixture(source)
            bundle = temporary / "bundle"
            manifest = _fixture_build(source, bundle, revision)
            contract = _trusted_contract(
                source,
                manifest,
                target_manifest="member/Cargo.toml",
                target_package="member",
                path=os.environ["PATH"],
                candidate_root=temporary,
            )

            forged_count = temporary / "forged-count"
            shutil.copytree(bundle, forged_count)
            forged_manifest = json.loads(
                (forged_count / "manifest.json").read_text(encoding="utf-8")
            )
            forged_manifest["closure_package_count"] += 1
            _rewrite_manifest(forged_count / "manifest.json", forged_manifest)
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    forged_count,
                    contract=contract,
                    reviewed_git=(),
                    cargo_home=Path.home() / ".cargo",
                )

            forged_source = temporary / "forged-source"
            shutil.copytree(bundle, forged_source)
            forged_manifest = json.loads(
                (forged_source / "manifest.json").read_text(encoding="utf-8")
            )
            forged_manifest["source_head_sha"] = "0" * 40
            _rewrite_manifest(forged_source / "manifest.json", forged_manifest)
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    forged_source,
                    contract=contract,
                    reviewed_git=(),
                    cargo_home=Path.home() / ".cargo",
                )

            extra_directory = temporary / "extra-directory"
            shutil.copytree(bundle, extra_directory)
            (extra_directory / "unexpected-member").mkdir()
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    extra_directory,
                    contract=contract,
                    reviewed_git=(),
                    cargo_home=Path.home() / ".cargo",
                )

            recipe_mismatch = _trusted_contract(
                source,
                manifest,
                target_manifest="member/Cargo.toml",
                target_package="member",
                path="/recipe/different",
                candidate_root=temporary,
            )
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    bundle,
                    contract=recipe_mismatch,
                    reviewed_git=(),
                    cargo_home=Path.home() / ".cargo",
                )

    def test_base_contract_transport_rejects_plain_objects_and_candidate_paths(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-contract-transport-") as name:
            temporary = Path(name)
            source = temporary / "source"
            source.mkdir()
            revision = _fixture(source)
            bundle = temporary / "bundle"
            manifest = _fixture_build(source, bundle, revision)
            trusted = _trusted_contract(
                source,
                manifest,
                target_manifest="member/Cargo.toml",
                target_package="member",
                path=os.environ["PATH"],
                candidate_root=temporary,
            )
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    bundle,
                    contract=trusted.contract,
                    reviewed_git=(),
                    cargo_home=Path.home() / ".cargo",
                )

            envelope = {
                "schema": BASE_CONTRACT_SCHEMA,
                "producer": BASE_CONTRACT_PRODUCER,
                "contract": bundle_contract_payload(trusted.contract),
            }
            raw = json.dumps(
                envelope,
                ensure_ascii=True,
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            candidate_contract_path = bundle / "contract.json"
            candidate_contract_path.write_bytes(raw)
            candidate_contract_path.chmod(0o644)
            with self.assertRaises(PrefetchError):
                load_base_owned_contract(
                    candidate_contract_path,
                    trusted_root=bundle,
                    candidate_root=temporary,
                    expected_sha256=hashlib.sha256(raw).hexdigest(),
                )

    def test_self_excluding_digest_and_numeric_schema(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-schema-") as name:
            bundle = self._bundle(Path(name))
            manifest_path = bundle / "manifest.json"
            value = json.loads(manifest_path.read_text(encoding="utf-8"))
            contract = _trusted_contract(
                ROOT,
                value,
                target_manifest=TARGET_MANIFEST.relative_to(ROOT).as_posix(),
                target_package=TARGET_PACKAGE,
                path=os.environ["PATH"],
                candidate_root=Path(name),
            )
            value["workspace_manifest_count"] = "10"
            manifest_path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    bundle,
                    contract=contract,
                    reviewed_git=GIT_SOURCE,
                    cargo_home=Path.home() / ".cargo",
                )

            value["workspace_manifest_count"] = 10
            value["bundle_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(PrefetchError):
                validate_bundle(
                    bundle,
                    contract=contract,
                    reviewed_git=GIT_SOURCE,
                    cargo_home=Path.home() / ".cargo",
                )

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

    def test_git_census_requires_exact_single_clean_source(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bootstrap-git-census-") as name:
            root = Path(name)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "Cargo.toml").write_text(
                "[package]\nname='stub'\nversion='0.1.0'\n",
                encoding="utf-8",
            )
            revision = _commit(source, "fixture")
            cargo_home = root / "cargo-home"
            db = cargo_home / "git" / "db" / "fixture"
            checkout = cargo_home / "git" / "checkouts" / "fixture" / revision
            db.parent.mkdir(parents=True)
            checkout.parent.mkdir(parents=True)
            subprocess.run(["git", "clone", "-q", "--bare", str(source), str(db)], check=True)
            subprocess.run(["git", "clone", "-q", str(db), str(checkout)], check=True)
            reviewed = (("https://github.com/example/fixture.git", revision),)
            subprocess.run(
                ["git", "-C", str(db), "remote", "set-url", "origin", reviewed[0][0]],
                check=True,
            )
            tree = _git(db, "rev-parse", f"{revision}^{{tree}}")
            provenance = ((reviewed[0][0], revision, tree),)
            records = git_census(cargo_home, reviewed, provenance=provenance)
            self.assertEqual(records[0]["rev"], revision)

            submodule_source = root / "submodule-source"
            submodule_source.mkdir()
            nested = root / "nested-submodule"
            nested.mkdir()
            subprocess.run(["git", "init", "-q", str(nested)], check=True)
            (nested / "nested.txt").write_text("nested\n", encoding="utf-8")
            nested_revision = _commit(nested, "nested")
            subprocess.run(["git", "init", "-q", str(submodule_source)], check=True)
            (submodule_source / "root.txt").write_text("root\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(submodule_source), "add", "root.txt"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(submodule_source),
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    f"160000,{nested_revision},submodule",
                ],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(submodule_source),
                    "-c",
                    "user.name=bootstrap-test",
                    "-c",
                    "user.email=bootstrap-test@example.invalid",
                    "commit",
                    "-qm",
                    "submodule",
                ],
                check=True,
            )
            submodule_revision = _source_revision(submodule_source)
            submodule_home = root / "submodule-cargo-home"
            submodule_db = submodule_home / "git" / "db" / "fixture"
            submodule_checkout = (
                submodule_home / "git" / "checkouts" / "fixture" / submodule_revision
            )
            submodule_db.parent.mkdir(parents=True)
            submodule_checkout.parent.mkdir(parents=True)
            subprocess.run(
                [
                    "git",
                    "clone",
                    "-q",
                    "--bare",
                    str(submodule_source),
                    str(submodule_db),
                ],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(submodule_db),
                    "remote",
                    "set-url",
                    "origin",
                    reviewed[0][0],
                ],
                check=True,
            )
            subprocess.run(
                ["git", "clone", "-q", str(submodule_db), str(submodule_checkout)],
                check=True,
            )
            submodule_tree = _git(
                submodule_db,
                "rev-parse",
                f"{submodule_revision}^{{tree}}",
            )
            with self.assertRaises(PrefetchError):
                git_census(
                    submodule_home,
                    ((reviewed[0][0], submodule_revision),),
                    provenance=((reviewed[0][0], submodule_revision, submodule_tree),),
                )

            wrong_url = ("https://github.com/other/fixture.git", revision)
            with self.assertRaises(PrefetchError):
                git_census(
                    cargo_home,
                    (wrong_url,),
                    provenance=((wrong_url[0], revision, tree),),
                )

            (checkout / "untracked").write_text("unsafe\n", encoding="utf-8")
            with self.assertRaises(PrefetchError):
                git_census(cargo_home, reviewed, provenance=provenance)

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

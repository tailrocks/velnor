//! Native integration coverage for a source-bound Homebrew package producer.

#![expect(
    clippy::expect_used,
    reason = "fixture failures should name the failed operation"
)]
#![expect(
    clippy::panic,
    reason = "fixture failures should include the artifact or command"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tar::Archive;
use toml::Value as TomlValue;
use velnor_tools::homebrew_preview::{
    produce, verify, ProducerConfig, MANIFEST_SCHEMA, MARKER_NAME, PACKAGE_RELATIVE, PAYLOAD_NAME,
    SOURCE_REF, SOURCE_REPOSITORY,
};

const TASK_BASE: &str = "69ee4fad2cb76443bb9fb82ac9a384a1ae2de85b";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    source_commit: String,
    package_dir: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "homebrew-source-package-009-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        let root = root.canonicalize().expect("canonicalize fixture root");
        let workspace = root.join("source");

        let repository = repository_root();
        let base_commit = git_text(&repository, &["rev-parse", "HEAD"]);
        let cloned = Command::new("git")
            .args(["clone", "--quiet", "--shared", "--no-checkout"])
            .arg(&repository)
            .arg(&workspace)
            .output()
            .expect("clone source checkout fixture");
        assert_success(&cloned, "clone source checkout");
        git(
            &workspace,
            &["checkout", "--quiet", "-B", "main", &base_commit],
        );
        let source_origin = format!("https://github.com/{SOURCE_REPOSITORY}.git");
        git(&workspace, &["remote", "set-url", "origin", &source_origin]);
        overlay_candidate_source(&repository, &workspace);
        let source_commit = git_text(&workspace, &["rev-parse", "HEAD"]);

        let package_dir = workspace.join(PACKAGE_RELATIVE);
        fs::create_dir_all(&package_dir).expect("pre-create empty handoff directory");
        let fixture = Self {
            root,
            workspace,
            source_commit,
            package_dir,
        };
        produce(&fixture.config("produce-scratch"))
            .expect("run real Homebrew source package producer");
        fixture
    }

    fn config(&self, scratch_label: &str) -> ProducerConfig {
        let scratch_dir = self.root.join(scratch_label);
        fs::create_dir_all(&scratch_dir).expect("pre-create package scratch directory");
        ProducerConfig {
            workspace: self.workspace.clone(),
            source_checkout: self.workspace.clone(),
            package_relative: PathBuf::from(PACKAGE_RELATIVE),
            verified_package_dir: self.package_dir.clone(),
            scratch_dir,
            source_repository: SOURCE_REPOSITORY.to_owned(),
            source_ref: SOURCE_REF.to_owned(),
            expected_source_ref: SOURCE_REF.to_owned(),
            source_commit: self.source_commit.clone(),
            channel: "preview".to_owned(),
            manifest_schema: MANIFEST_SCHEMA.to_owned(),
        }
    }

    fn archive_path(&self) -> PathBuf {
        self.package_dir.join(PAYLOAD_NAME)
    }

    fn manifest(&self) -> JsonValue {
        read_json(&find_manifest_path(&self.package_dir))
    }

    fn identity(&self) -> JsonValue {
        read_json(&find_identity_path(&self.package_dir))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn overlay_candidate_source(repository: &Path, source_checkout: &Path) {
    let tracked_diff = git_bytes(repository, &["diff", "--binary", "HEAD"]);
    if !tracked_diff.is_empty() {
        let patch_path = source_checkout
            .parent()
            .expect("fixture source parent")
            .join("candidate-overlay.patch");
        fs::write(&patch_path, tracked_diff).expect("write candidate overlay patch");
        let patch_path = patch_path.to_str().expect("overlay patch path is UTF-8");
        git(source_checkout, &["apply", "--binary", patch_path]);
    }

    for relative in git_bytes(
        repository,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .split(|byte| *byte == 0)
    .filter(|path| !path.is_empty())
    .map(|path| String::from_utf8_lossy(path).into_owned())
    {
        copy_path(
            &repository.join(&relative),
            &source_checkout.join(&relative),
        );
    }
    git(source_checkout, &["add", "-A"]);

    let staged = Command::new("git")
        .current_dir(source_checkout)
        .args(["diff", "--cached", "--quiet"])
        .output()
        .expect("check candidate fixture index");
    assert!(
        matches!(staged.status.code(), Some(0 | 1)),
        "check candidate fixture index failed: {}",
        String::from_utf8_lossy(&staged.stderr)
    );
    if staged.status.code() == Some(1) {
        let committed = Command::new("git")
            .current_dir(source_checkout)
            .args([
                "-c",
                "user.name=Homebrew Package Fixture",
                "-c",
                "user.email=task009-fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "deterministic candidate source fixture",
            ])
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00+00:00")
            .output()
            .expect("commit isolated candidate source fixture");
        assert_success(&committed, "commit isolated candidate source fixture");
    }
}

fn copy_path(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).expect("inspect untracked candidate path");
    fs::create_dir_all(destination.parent().expect("untracked candidate parent"))
        .expect("create untracked candidate parent");
    if metadata.file_type().is_file() {
        fs::copy(source, destination).expect("copy untracked candidate file");
    } else if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).expect("read untracked symlink target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination).expect("copy untracked symlink");
        #[cfg(not(unix))]
        panic!("untracked symlinks require a Unix fixture host");
    } else {
        panic!("untracked candidate path is not a file or symlink");
    }
}

struct MemberHeader {
    mode: u32,
    uid: u64,
    gid: u64,
    mtime: u64,
    owner: String,
    group: String,
}

struct ArchiveSnapshot {
    names: Vec<String>,
    headers: BTreeMap<String, MemberHeader>,
    marker: Vec<u8>,
}

#[test]
fn both_executables_are_packaged() {
    let fixture = Fixture::new("both-executables");
    assert_package_inventory(&fixture);

    let manifest = fixture.manifest();
    let assets = manifest["assets"].as_array().expect("payload assets array");
    assert_eq!(assets.len(), 1, "source package has one payload");
    assert_eq!(assets[0]["name"], PAYLOAD_NAME);
    assert_digest_matches(
        &fixture.package_dir.join(PAYLOAD_NAME),
        &assets[0]["sha256"],
    );

    let unpacked = fixture.root.join("both-executables-source");
    fs::create_dir_all(&unpacked).expect("create source extraction directory");
    unpack_archive(&fixture, &unpacked);
    let source_root = unpacked.join(archive_root(&fixture).trim_end_matches('/'));
    let metadata = cargo_metadata(&source_root, &fixture.root.join("both-metadata-target"));
    let build_contract = source_build_contract(&metadata);
    assert_eq!(
        build_contract.binary_names.len(),
        2,
        "producer source package must expose both release-build binaries"
    );
}

#[test]
fn source_archive_is_complete_and_bound() {
    let first = Fixture::new("archive-first");
    let second = Fixture::new("archive-second");
    assert_eq!(first.source_commit, second.source_commit);
    assert_eq!(
        fs::read(first.archive_path()).expect("read first source archive"),
        fs::read(second.archive_path()).expect("read retry source archive"),
        "same source SHA must yield byte-identical gzip tarballs"
    );

    let snapshot = inspect_archive(&first);
    let root = archive_root(&first);
    assert_eq!(
        snapshot.names.iter().filter(|name| *name == &root).count(),
        1,
        "archive must have exactly one source root directory"
    );
    assert!(snapshot.names.iter().all(|name| name.starts_with(&root)));
    assert_eq!(
        snapshot.marker,
        format!("{}\n", first.source_commit).into_bytes(),
        "source marker must equal the full commit SHA and one LF"
    );

    let commit_epoch = git_text(
        &first.workspace,
        &["show", "-s", "--format=%ct", &first.source_commit],
    )
    .parse::<u64>()
    .expect("parse source commit timestamp");
    for (name, header) in &snapshot.headers {
        assert_eq!(header.uid, 0, "uid for {name}");
        assert_eq!(header.gid, 0, "gid for {name}");
        assert_eq!(header.mtime, commit_epoch, "mtime for {name}");
        assert!(header.owner.is_empty(), "owner name for {name}");
        assert!(header.group.is_empty(), "group name for {name}");
    }
    assert_git_tree_is_present(&first, &snapshot, &root);
    assert_archive_modes_match_git(&first, &snapshot, &root);
    assert!(
        snapshot.marker.len() == 41
            && snapshot.marker[..40]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "source marker is not a lowercase 40-hex SHA with LF"
    );
}

#[test]
fn preview_version_is_truthful() {
    let fixture = Fixture::new("version");
    let manifest = fixture.manifest();
    let metadata = cargo_metadata(
        &fixture.workspace,
        &fixture.root.join("version-metadata-target"),
    );
    let build_contract = source_build_contract(&metadata);
    let revision_count = git_text(
        &fixture.workspace,
        &["rev-list", "--count", &fixture.source_commit],
    );
    let expected = format!(
        "{}-preview.{revision_count}+{}",
        build_contract.runtime_version,
        &fixture.source_commit[..7]
    );

    assert_eq!(manifest["version"], expected);
    assert_eq!(manifest["source_commit"], fixture.source_commit);
    assert_eq!(manifest["source_ref"], SOURCE_REF);
    assert_eq!(manifest["source_repository"], SOURCE_REPOSITORY);

    let identity = fixture.identity();
    assert_eq!(identity["source_digest"], fixture.source_commit);
    assert_eq!(identity["manifest"], manifest);
    assert_eq!(identity["source_ref"], SOURCE_REF);
    assert_eq!(identity["source_repository"], SOURCE_REPOSITORY);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the contract test keeps archive and manifest proof adjacent"
)]
fn macos_build_contract_is_valid() {
    let fixture = Fixture::new("macos-contract");
    let snapshot = inspect_archive(&fixture);
    let root = archive_root(&fixture);
    let members = snapshot
        .names
        .iter()
        .filter_map(|name| name.strip_prefix(&root))
        .collect::<Vec<_>>();

    assert!(members.iter().all(|path| {
        !path
            .split('/')
            .any(|component| component == ".git" || component == "target")
            && !path.contains("package-release-scratch")
    }));

    let unpacked = fixture.root.join("unpacked");
    fs::create_dir_all(&unpacked).expect("create archive extraction directory");
    unpack_archive(&fixture, &unpacked);
    let source_root = unpacked.join(root.trim_end_matches('/'));
    let metadata = cargo_metadata(&source_root, &fixture.root.join("metadata-target"));
    let build_contract = source_build_contract(&metadata);
    assert_eq!(
        build_contract.binary_names.len(),
        2,
        "source archive must contain both release-build binary targets"
    );

    let manifest_version = fixture.manifest()["version"]
        .as_str()
        .expect("source-derived preview version")
        .to_owned();
    let marker_path = source_root.join(MARKER_NAME);
    assert_eq!(
        fs::read(&marker_path).expect("read archived source identity marker"),
        format!("{}\n", fixture.source_commit).as_bytes(),
        "source build identity must come from the archive marker"
    );

    let build_target = fixture.root.join("homebrew-build-target");
    let archive_build = cargo_build_preview(
        &source_root,
        &build_target,
        &fixture.source_commit,
        Some(&manifest_version),
        &build_contract,
    );
    assert_cargo_success(
        &archive_build,
        "build both preview binaries from source archive",
    );
    let cli_binary = build_target.join(format!("release/{}", build_contract.cli_binary));
    let runner_binary = build_target.join(format!("release/{}", build_contract.runtime_binary));
    assert!(
        cli_binary.is_file(),
        "cargo did not emit the discovered CLI binary"
    );
    assert!(
        runner_binary.is_file(),
        "cargo did not emit the discovered runtime binary"
    );
    assert_native_host_executable(&cli_binary, "CLI");
    assert_native_host_executable(&runner_binary, "runtime");
    assert_binary_version(
        &cli_binary,
        &format!(
            "{manifest_version} preview preview {}",
            fixture.source_commit
        ),
        "archive marker build",
    );

    let negative_target = fixture.root.join("preview-negative-target");
    let invalid_versions = [
        (
            "absent",
            None,
            "VELNOR_PREVIEW_BUILD_VERSION is required for source archives",
        ),
        (
            "malformed",
            Some("invalid-preview-version".to_owned()),
            "has the wrong crate-version prefix",
        ),
        (
            "mismatched",
            Some(format!(
                "{}-preview.1+{}",
                build_contract.runtime_version,
                different_short_sha(&fixture.source_commit)
            )),
            "does not bind the source SHA",
        ),
    ];
    for (label, version, expected_error) in invalid_versions {
        let output = cargo_check_preview(
            &source_root,
            &negative_target,
            &fixture.source_commit,
            version.as_deref(),
            &build_contract,
        );
        assert_cargo_rejected(&output, label, expected_error);
    }

    // The existing Git-backed source-checkout path still supports the optional
    // version override: with Git identity available, an unset override keeps
    // the runner crate version while retaining the preview kind and SHA.
    fs::remove_file(&marker_path).expect("remove archive-only marker for Git control");
    copy_directory_tree(&fixture.workspace.join(".git"), &source_root.join(".git"));
    assert_eq!(
        git_text(&source_root, &["rev-parse", "HEAD"]),
        fixture.source_commit,
        "Git-backed preview control must use the same source SHA"
    );
    assert!(
        git_text(&source_root, &["status", "--porcelain"]).is_empty(),
        "Git-backed preview control must be clean"
    );
    let git_preview = cargo_build_preview(
        &source_root,
        &build_target,
        &fixture.source_commit,
        None,
        &build_contract,
    );
    assert_cargo_success(
        &git_preview,
        "build Git-backed preview without version override",
    );
    assert_binary_version(
        &cli_binary,
        &format!(
            "{} preview preview {}",
            build_contract.runtime_version, fixture.source_commit
        ),
        "Git-backed preview fallback",
    );
}

#[test]
fn apt_native_channel_is_unmodified() {
    let repository = repository_root();
    assert_automation_layout_unchanged(
        &repository,
        &[".github/workflows", ".github/ci", ".github-gen"],
    );
    assert_toml_subtree_unchanged(&repository, "mise.toml", &["tasks", "release:check"]);

    let metadata = cargo_metadata(
        &repository,
        &std::env::temp_dir().join(format!("homebrew-apt-metadata-{}", std::process::id())),
    );
    let packages = metadata["packages"].as_array().expect("Cargo package list");
    let debian_manifests = packages
        .iter()
        .filter(|package| package["metadata"]["deb"].is_object())
        .filter_map(|package| package["manifest_path"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        debian_manifests.len(),
        1,
        "one package manifest must declare native package metadata"
    );
    let relative_manifest = Path::new(debian_manifests[0])
        .strip_prefix(&repository)
        .expect("package manifest belongs to repository")
        .to_str()
        .expect("package manifest path is UTF-8");
    assert_toml_subtree_unchanged(
        &repository,
        relative_manifest,
        &["package", "metadata", "deb"],
    );
}

/// The drift gates pin a historical base commit that shallow CI checkouts
/// do not contain. Fetch just that commit when it is missing instead of
/// failing with "fatal: not a tree object".
fn ensure_task_base(repository: &Path) {
    let probe = Command::new("git")
        .current_dir(repository)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{TASK_BASE}^{{commit}}"),
        ])
        .output()
        .expect("probe baseline commit");
    if !probe.status.success() {
        git(
            repository,
            &["fetch", "--no-tags", "--depth", "1", "origin", TASK_BASE],
        );
    }
}

fn baseline_paths(repository: &Path, prefixes: &[&str]) -> Vec<String> {
    ensure_task_base(repository);
    let mut args = vec!["ls-tree", "-r", "--name-only", "-z", TASK_BASE, "--"];
    args.extend(prefixes.iter().copied());
    git_bytes(repository, &args)
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn assert_automation_layout_unchanged(repository: &Path, prefixes: &[&str]) {
    for prefix in prefixes {
        let mut expected = baseline_paths(repository, &[prefix]);
        expected.sort();
        let mut actual = worktree_paths(repository, prefix);
        actual.sort();
        assert_eq!(
            actual, expected,
            "automation file set changed under {prefix}"
        );
    }
}

/// Relative file paths under `prefix` in the working tree, mirroring
/// [`baseline_paths`] name semantics so an added file fails as loudly as a
/// removed one. A missing directory reads as empty, matching `ls-tree`.
fn worktree_paths(repository: &Path, prefix: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = vec![repository.join(prefix)];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let entry = entry.expect("read automation directory entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                paths.push(
                    path.strip_prefix(repository)
                        .expect("automation path belongs to repository")
                        .to_str()
                        .expect("automation path is UTF-8")
                        .to_owned(),
                );
            }
        }
    }
    paths
}

#[test]
fn invalid_or_partial_package_is_rejected() {
    if std::env::var_os("TASK009_FROM_ENV_CONFLICT_CHILD").is_some() {
        let error = ProducerConfig::from_env()
            .expect_err("CLI configuration accepted conflicting source SHA values");
        assert_eq!(
            error.to_string(),
            "source commit differs from expected source commit"
        );
        return;
    }

    let fixture = Fixture::new("negative-controls");

    let mut wrong_sha = fixture.config("verify-wrong-sha");
    wrong_sha.source_commit = "1111111111111111111111111111111111111111".to_owned();
    assert!(
        verify(&wrong_sha).is_err(),
        "verifier accepted package against a different expected source SHA"
    );

    let payload_path = fixture.package_dir.join(PAYLOAD_NAME);
    let original_payload =
        fs::read(&payload_path).expect("read valid payload for corruption control");
    let mut corrupted_payload = original_payload.clone();
    corrupted_payload[0] ^= 1;
    fs::write(&payload_path, corrupted_payload).expect("corrupt payload digest control");
    assert!(
        verify(&fixture.config("verify-wrong-digest")).is_err(),
        "verifier accepted a payload whose digest differs from the manifest"
    );
    fs::write(&payload_path, original_payload)
        .expect("restore valid payload before partial control");

    fs::remove_file(find_identity_path(&fixture.package_dir))
        .expect("remove identity for partial-package control");
    assert!(
        verify(&fixture.config("verify-partial-package")).is_err(),
        "verifier accepted a partial package without its identity document"
    );

    assert_from_env_rejects_conflicting_source_sha(&fixture);
}

fn assert_package_inventory(fixture: &Fixture) {
    let manifest_path = find_manifest_path(&fixture.package_dir);
    let identity_path = find_identity_path(&fixture.package_dir);
    let manifest = read_json(&manifest_path);
    let supporting_assets = manifest["supporting_assets"]
        .as_array()
        .expect("supporting assets array");
    assert_eq!(
        supporting_assets.len(),
        1,
        "one checksum support file expected"
    );
    let support_name = supporting_assets[0]["name"]
        .as_str()
        .expect("supporting asset name")
        .to_owned();
    let expected = BTreeSet::from([
        file_name(&manifest_path),
        file_name(&identity_path),
        support_name.clone(),
        PAYLOAD_NAME.to_owned(),
    ]);
    let actual = fs::read_dir(&fixture.package_dir)
        .expect("read produced package directory")
        .map(|entry| {
            let entry = entry.expect("read package directory entry");
            assert!(entry
                .file_type()
                .expect("read package entry type")
                .is_file());
            entry.file_name().to_string_lossy().into_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "package must contain its exact regular files"
    );

    assert_eq!(manifest["schema"], MANIFEST_SCHEMA);
    assert_eq!(manifest["source_repository"], SOURCE_REPOSITORY);
    assert_eq!(manifest["source_ref"], SOURCE_REF);
    assert_eq!(manifest["source_commit"], fixture.source_commit);
    assert_digest_matches(
        &fixture.package_dir.join(&support_name),
        &supporting_assets[0]["sha256"],
    );

    let sums = fs::read_to_string(fixture.package_dir.join(&support_name))
        .expect("read checksum support file");
    let payload_digest = sha256(&fs::read(fixture.archive_path()).expect("read source archive"));
    assert!(sums
        .lines()
        .any(|line| line == format!("{payload_digest}  {PAYLOAD_NAME}")));

    let identity = read_json(&identity_path);
    let identity_keys = identity
        .as_object()
        .expect("identity object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        identity_keys,
        BTreeSet::from([
            "manifest",
            "source_digest",
            "source_ref",
            "source_repository"
        ])
    );
    assert_eq!(identity["source_digest"], fixture.source_commit);
    assert_eq!(identity["manifest"], manifest);
    assert_eq!(identity["source_repository"], SOURCE_REPOSITORY);
    assert_eq!(identity["source_ref"], SOURCE_REF);
}

fn assert_git_tree_is_present(fixture: &Fixture, snapshot: &ArchiveSnapshot, root: &str) {
    let paths = git_bytes(
        &fixture.workspace,
        &["ls-tree", "-r", "--name-only", "-z", &fixture.source_commit],
    );
    let mut expected = BTreeSet::from([root.to_owned()]);
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
    {
        expected.insert(format!("{root}{path}"));
        let components = path.split('/').collect::<Vec<_>>();
        let mut directory = String::new();
        for component in &components[..components.len().saturating_sub(1)] {
            if !directory.is_empty() {
                directory.push('/');
            }
            directory.push_str(component);
            expected.insert(format!("{root}{directory}/"));
        }
    }
    expected.insert(format!("{root}{MARKER_NAME}"));

    let archived = snapshot.names.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        snapshot.names.len(),
        archived.len(),
        "archive contains duplicate paths"
    );
    assert_eq!(
        archived, expected,
        "archive inventory must equal the tracked Git tree plus marker"
    );
}

fn assert_archive_modes_match_git(fixture: &Fixture, snapshot: &ArchiveSnapshot, root: &str) {
    let rows = git_bytes(
        &fixture.workspace,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--format=%(objectmode)%x09%(path)",
            &fixture.source_commit,
        ],
    );
    for row in rows.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
        let row = String::from_utf8_lossy(row);
        let (git_mode, path) = row.split_once('\t').expect("Git mode/path row");
        let expected_mode = match git_mode {
            "100755" => 0o755,
            "120000" => 0o777,
            _ => 0o644,
        };
        let archive_name = format!("{root}{path}");
        let actual_mode = snapshot
            .headers
            .get(&archive_name)
            .unwrap_or_else(|| panic!("archive lacks Git file {path}"))
            .mode;
        assert_eq!(actual_mode, expected_mode, "Git mode for {path}");
    }
}

#[derive(Clone, Debug)]
struct BuildPackage {
    name: String,
    version: String,
    binaries: Vec<String>,
    release_features: Vec<String>,
}

struct SourceBuildContract {
    packages: Vec<String>,
    binary_names: Vec<String>,
    _cli_package: String,
    runtime_package: String,
    cli_binary: String,
    runtime_binary: String,
    runtime_version: String,
}

fn cargo_metadata(source_root: &Path, target_dir: &Path) -> JsonValue {
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(source_root)
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .expect("run Cargo metadata against source tree");
    assert_success(&output, "cargo metadata on source tree");
    serde_json::from_slice(&output.stdout).expect("parse Cargo metadata")
}

fn source_build_contract(metadata: &JsonValue) -> SourceBuildContract {
    let packages = metadata["packages"].as_array().expect("Cargo package list");
    let candidates = packages
        .iter()
        .filter_map(|package| {
            let release_features = package["features"]["release-build"].as_array()?;
            let binaries = package["targets"]
                .as_array()?
                .iter()
                .filter(|target| {
                    target["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
                })
                .filter_map(|target| target["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            if binaries.is_empty() {
                return None;
            }
            Some(BuildPackage {
                name: package["name"].as_str()?.to_owned(),
                version: package["version"].as_str()?.to_owned(),
                binaries,
                release_features: release_features
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        candidates.len(),
        2,
        "two binary packages must expose release-build"
    );

    let forwarders = candidates
        .iter()
        .filter_map(|package| {
            package
                .release_features
                .iter()
                .find_map(|feature| feature.strip_suffix("/release-build"))
                .map(|dependency| (package, dependency))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        forwarders.len(),
        1,
        "one binary package must forward release-build to the runtime package"
    );
    let (cli, runtime_name) = forwarders[0];
    let runtime = candidates
        .iter()
        .find(|package| package.name == runtime_name)
        .expect("release-build forwarding dependency package");
    assert_ne!(cli.name, runtime.name);
    assert_eq!(cli.binaries.len(), 1, "forwarding package bin target count");
    assert!(
        runtime.binaries.contains(&runtime.name),
        "runtime package must expose its namesake binary; got {:?}",
        runtime.binaries
    );

    let mut package_names = vec![cli.name.clone(), runtime.name.clone()];
    package_names.sort();
    let mut binary_names = vec![cli.binaries[0].clone(), runtime.name.clone()];
    binary_names.sort();
    SourceBuildContract {
        packages: package_names,
        binary_names,
        _cli_package: cli.name.clone(),
        runtime_package: runtime.name.clone(),
        cli_binary: cli.binaries[0].clone(),
        runtime_binary: runtime.name.clone(),
        runtime_version: runtime.version.clone(),
    }
}

fn inspect_archive(fixture: &Fixture) -> ArchiveSnapshot {
    let decompressed = Command::new("gzip")
        .args(["-dc"])
        .arg(fixture.archive_path())
        .output()
        .expect("decompress source archive for inspection");
    assert_success(&decompressed, "gzip -dc source archive");

    let mut archive = Archive::new(Cursor::new(decompressed.stdout));
    let mut names = Vec::new();
    let mut headers = BTreeMap::new();
    let mut marker = Vec::new();
    for entry in archive.entries().expect("read tar entries") {
        let mut entry = entry.expect("read tar entry");
        let name = entry
            .path()
            .expect("read tar path")
            .to_string_lossy()
            .into_owned();
        let header = entry.header();
        let summary = MemberHeader {
            mode: header.mode().expect("read tar mode"),
            uid: header.uid().expect("read tar uid"),
            gid: header.gid().expect("read tar gid"),
            mtime: header.mtime().expect("read tar timestamp"),
            owner: header
                .username()
                .expect("decode tar user name")
                .unwrap_or_default()
                .to_owned(),
            group: header
                .groupname()
                .expect("decode tar group name")
                .unwrap_or_default()
                .to_owned(),
        };
        let marker_parent = name.strip_suffix(MARKER_NAME).unwrap_or_default();
        if marker_parent.ends_with('/') && !marker_parent[..marker_parent.len() - 1].contains('/') {
            entry
                .read_to_end(&mut marker)
                .expect("read archive source marker");
        }
        names.push(name.clone());
        headers.insert(name, summary);
    }
    ArchiveSnapshot {
        names,
        headers,
        marker,
    }
}

fn unpack_archive(fixture: &Fixture, destination: &Path) {
    let decompressed = Command::new("gzip")
        .args(["-dc"])
        .arg(fixture.archive_path())
        .output()
        .expect("decompress archive for extraction");
    assert_success(&decompressed, "gzip -dc for extraction");
    let mut archive = Archive::new(Cursor::new(decompressed.stdout));
    archive
        .unpack(destination)
        .expect("extract verified archive");
}

fn assert_toml_subtree_unchanged(repository: &Path, path: &str, keys: &[&str]) {
    ensure_task_base(repository);
    let current_bytes = fs::read(repository.join(path)).expect("read current TOML config");
    let base_bytes = git_bytes(repository, &["show", &format!("{TASK_BASE}:{path}")]);
    let current = toml::from_str::<TomlValue>(&String::from_utf8_lossy(&current_bytes))
        .expect("parse current TOML config");
    let base = toml::from_str::<TomlValue>(&String::from_utf8_lossy(&base_bytes))
        .expect("parse baseline TOML config");
    assert_eq!(
        toml_at(&current, keys),
        toml_at(&base, keys),
        "APT TOML config: {path}"
    );
}

fn toml_at<'a>(value: &'a TomlValue, keys: &[&str]) -> &'a TomlValue {
    keys.iter().fold(value, |node, key| {
        node.get(*key)
            .unwrap_or_else(|| panic!("missing TOML key {key}"))
    })
}

fn assert_digest_matches(path: &Path, expected: &JsonValue) {
    let expected = expected.as_str().expect("manifest SHA-256 string");
    let actual = sha256(&fs::read(path).expect("read manifest asset"));
    assert_eq!(actual, expected, "asset digest in manifest");
}

fn cargo_build_preview(
    source_root: &Path,
    target_dir: &Path,
    source_sha: &str,
    preview_version: Option<&str>,
    contract: &SourceBuildContract,
) -> Output {
    let features = contract
        .packages
        .iter()
        .map(|package| format!("{package}/release-build"))
        .collect::<Vec<_>>()
        .join(",");
    let mut command = Command::new("cargo");
    command
        .args(["build", "--locked", "--release", "--bins"])
        .current_dir(source_root)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("VELNOR_RELEASE_BUILD", "1")
        .env("VELNOR_PREVIEW_SOURCE_SHA", source_sha);
    for package in &contract.packages {
        command.arg("-p").arg(package);
    }
    command.arg("--features").arg(features);
    set_optional_env(
        &mut command,
        "VELNOR_PREVIEW_BUILD_VERSION",
        preview_version,
    );
    run_logged(&mut command, "archive preview binary build")
}

fn cargo_check_preview(
    source_root: &Path,
    target_dir: &Path,
    source_sha: &str,
    preview_version: Option<&str>,
    contract: &SourceBuildContract,
) -> Output {
    let mut command = Command::new("cargo");
    command
        .args(["check", "--locked", "-p", &contract.runtime_package])
        .args(["--features", "release-build"])
        .current_dir(source_root)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("VELNOR_RELEASE_BUILD", "1")
        .env("VELNOR_PREVIEW_SOURCE_SHA", source_sha);
    set_optional_env(
        &mut command,
        "VELNOR_PREVIEW_BUILD_VERSION",
        preview_version,
    );
    run_logged(&mut command, "archive preview validation build")
}

fn set_optional_env(command: &mut Command, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        command.env(name, value);
    } else {
        command.env_remove(name);
    }
}

fn run_logged(command: &mut Command, label: &str) -> Output {
    let rendered = format!("{command:?}");
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run nested command {label}: {error}"));
    println!(
        "nested command: {label}\n{rendered}\nexit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn assert_cargo_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_cargo_rejected(output: &Output, label: &str, expected_error: &str) {
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "preview build accepted {label} version input"
    );
    assert!(
        logs.contains(expected_error),
        "{label} build failed for the wrong reason; expected {expected_error:?}, got:\n{logs}"
    );
}

fn different_short_sha(source_sha: &str) -> String {
    let mut short = source_sha[..7].to_owned();
    let replacement = if short.starts_with('0') { '1' } else { '0' };
    short.replace_range(..1, &replacement.to_string());
    short
}

fn assert_native_host_executable(binary: &Path, label: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(binary)
            .unwrap_or_else(|error| panic!("read {label} binary metadata: {error}"))
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "{label} binary is not executable");
    }
    #[cfg(not(unix))]
    {
        let _ = (binary, label);
    }
}

fn assert_binary_version(binary: &Path, expected: &str, label: &str) {
    let output = Command::new(binary)
        .arg("version")
        .output()
        .unwrap_or_else(|error| panic!("run {label} version command: {error}"));
    assert_success(&output, &format!("run {label} version command"));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        expected,
        "{label} binary identity"
    );
}

fn copy_directory_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied Git metadata directory");
    for entry in fs::read_dir(source).expect("read source Git metadata directory") {
        let entry = entry.expect("read source Git metadata entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("read Git metadata entry type");
        if file_type.is_dir() {
            copy_directory_tree(&source_path, &destination_path);
        } else {
            assert!(file_type.is_file(), "unsupported Git metadata entry type");
            fs::copy(&source_path, &destination_path).expect("copy Git metadata file");
        }
    }
}

fn assert_from_env_rejects_conflicting_source_sha(fixture: &Fixture) {
    let scratch = fixture.root.join("from-env-scratch");
    fs::create_dir_all(&scratch).expect("create CLI config scratch directory");
    let conflicting_sha = if fixture.source_commit.starts_with('1') {
        "2222222222222222222222222222222222222222"
    } else {
        "1111111111111111111111111111111111111111"
    };
    let mut command =
        Command::new(std::env::current_exe().expect("current integration test binary"));
    command
        .args([
            "--exact",
            "invalid_or_partial_package_is_rejected",
            "--nocapture",
        ])
        .env("TASK009_FROM_ENV_CONFLICT_CHILD", "1")
        .env("GITHUB_WORKSPACE", &fixture.workspace)
        .env("VELNOR_SOURCE_CHECKOUT_DIR", &fixture.workspace)
        .env("PACKAGE_DIR", PACKAGE_RELATIVE)
        .env("VELNOR_VERIFIED_PACKAGE_DIR", &fixture.package_dir)
        .env("PACKAGE_RELEASE_SCRATCH_DIR", scratch)
        .env("VELNOR_SOURCE_COMMIT", &fixture.source_commit)
        .env("EXPECTED_SOURCE_COMMIT", conflicting_sha)
        .env("VELNOR_SOURCE_REF", SOURCE_REF)
        .env("EXPECTED_SOURCE_REF", SOURCE_REF)
        .env("VELNOR_PACKAGE_CHANNEL", "preview")
        .env("EXPECTED_SOURCE_REPOSITORY", SOURCE_REPOSITORY)
        .env("EXPECTED_MANIFEST_SCHEMA", MANIFEST_SCHEMA);
    let output = run_logged(
        &mut command,
        "conflicting source identity environment control",
    );
    assert_success(&output, "reject conflicting source SHA environment values");
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .concat()
}

fn read_json(path: &Path) -> JsonValue {
    serde_json::from_slice(&fs::read(path).expect("read JSON package file"))
        .expect("parse package JSON")
}

fn find_manifest_path(directory: &Path) -> PathBuf {
    find_json_asset(directory, "manifest", |value| {
        value["schema"] == MANIFEST_SCHEMA && value["assets"].is_array()
    })
}

fn find_identity_path(directory: &Path) -> PathBuf {
    find_json_asset(directory, "identity", |value| {
        value["source_digest"].is_string() && value["manifest"].is_object()
    })
}

fn find_json_asset(directory: &Path, label: &str, matches: impl Fn(&JsonValue) -> bool) -> PathBuf {
    let matches = fs::read_dir(directory)
        .expect("read package directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter(|path| matches(&read_json(path)))
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one package {label} document");
    matches
        .into_iter()
        .next()
        .expect("single package JSON asset")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .expect("package asset file name")
        .to_string_lossy()
        .into_owned()
}

fn archive_root(fixture: &Fixture) -> String {
    let snapshot = inspect_archive(fixture);
    let roots = snapshot
        .names
        .iter()
        .filter_map(|name| {
            let (component, remainder) = name.split_once('/')?;
            remainder.is_empty().then(|| format!("{component}/"))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(roots.len(), 1, "archive must have one top-level directory");
    let root = roots.into_iter().next().expect("single archive root");
    let root_component = root.strip_suffix('/').expect("root directory slash");
    assert!(
        root_component.ends_with(&format!("-{}", fixture.source_commit)),
        "archive root must bind the full source SHA"
    );
    root
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root exists")
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert_success(&output, "Git fixture command");
}

fn git_text(root: &Path, args: &[&str]) -> String {
    String::from_utf8(git_bytes(root, args))
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn git_bytes(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run Git command");
    assert_success(&output, "Git command");
    output.stdout
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

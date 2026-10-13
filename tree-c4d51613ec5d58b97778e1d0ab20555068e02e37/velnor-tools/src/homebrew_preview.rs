//! Deterministic, source-bound source archive for the Velnor Homebrew preview.
//!
//! This is the repository-owned producer used by the typed `package-release`
//! primitive. It writes only to the already-created, ignored verified handoff;
//! Cargo build output belongs to the primitive's disposable runner-temp scratch
//! directory and survives until the package verification step completes.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context, Result};
use flate2::{write::GzEncoder, Compression, GzBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::{Builder as TarBuilder, EntryType, Header, HeaderMode};

pub const PAYLOAD_NAME: &str = "velnor-homebrew-source.tar.gz";
pub const MARKER_NAME: &str = "homebrew-preview-source.sha";
pub const MANIFEST_SCHEMA: &str = "velnor.package-release.v1";
pub const SOURCE_REPOSITORY: &str = "tailrocks/velnor";
pub const SOURCE_REF: &str = "refs/heads/main";
pub const PACKAGE_RELATIVE: &str = "target/homebrew-preview-package";
pub const ENV_GITHUB_WORKSPACE: &str = "GITHUB_WORKSPACE";
pub const ENV_SOURCE_CHECKOUT_DIR: &str = "VELNOR_SOURCE_CHECKOUT_DIR";
pub const ENV_PACKAGE_DIR: &str = "PACKAGE_DIR";
pub const ENV_VERIFIED_PACKAGE_DIR: &str = "VELNOR_VERIFIED_PACKAGE_DIR";
pub const ENV_PACKAGE_RELEASE_SCRATCH_DIR: &str = "PACKAGE_RELEASE_SCRATCH_DIR";
pub const ENV_EXPECTED_SOURCE_REPOSITORY: &str = "EXPECTED_SOURCE_REPOSITORY";
pub const ENV_SOURCE_REF: &str = "VELNOR_SOURCE_REF";
pub const ENV_EXPECTED_SOURCE_REF: &str = "EXPECTED_SOURCE_REF";
pub const ENV_SOURCE_COMMIT: &str = "VELNOR_SOURCE_COMMIT";
pub const ENV_EXPECTED_SOURCE_COMMIT: &str = "EXPECTED_SOURCE_COMMIT";
pub const ENV_PACKAGE_CHANNEL: &str = "VELNOR_PACKAGE_CHANNEL";
pub const ENV_EXPECTED_MANIFEST_SCHEMA: &str = "EXPECTED_MANIFEST_SCHEMA";
pub const ENV_EXPECTED_SOURCE_TREE: &str = "EXPECTED_SOURCE_TREE";
pub const BUILD_ENV_RELEASE: &str = "VELNOR_RELEASE_BUILD";
pub const BUILD_ENV_PREVIEW_SOURCE_SHA: &str = "VELNOR_PREVIEW_SOURCE_SHA";
pub const BUILD_ENV_PREVIEW_VERSION: &str = "VELNOR_PREVIEW_BUILD_VERSION";

const CHECKSUMS_NAME: &str = "SHA256SUMS";
const MANIFEST_NAME: &str = "release-manifest.json";
const IDENTITY_NAME: &str = "identity.json";
const ARCHIVE_ROOT_PREFIX: &str = "velnor-";

#[derive(Clone, Debug)]
pub struct ProducerConfig {
    pub workspace: PathBuf,
    pub source_checkout: PathBuf,
    pub package_relative: PathBuf,
    pub verified_package_dir: PathBuf,
    pub scratch_dir: PathBuf,
    pub source_repository: String,
    pub source_ref: String,
    pub expected_source_ref: String,
    pub source_commit: String,
    pub channel: String,
    pub manifest_schema: String,
}

impl ProducerConfig {
    /// Read the exact environment exported by Velnor's typed package-release
    /// workflow. The aliases support its build and verification steps without
    /// allowing either step to silently select a different source identity.
    pub fn from_env() -> Result<Self> {
        let workspace = path_env(ENV_GITHUB_WORKSPACE)?.unwrap_or(env::current_dir()?);
        let source_checkout = path_env(ENV_SOURCE_CHECKOUT_DIR)?.unwrap_or_else(|| workspace.clone());
        let package_relative = PathBuf::from(required_env(ENV_PACKAGE_DIR)?);
        let verified_package_dir = path_env(ENV_VERIFIED_PACKAGE_DIR)?
            .unwrap_or_else(|| workspace.join(&package_relative));
        let scratch_dir = path_env(ENV_PACKAGE_RELEASE_SCRATCH_DIR)?.unwrap_or_default();
        let source_repository = required_env(ENV_EXPECTED_SOURCE_REPOSITORY)?;
        let source_ref = optional_env(ENV_SOURCE_REF)?
            .or(optional_env(ENV_EXPECTED_SOURCE_REF)?)
            .context("source ref or expected source ref is required")?;
        let expected_source_ref = required_env(ENV_EXPECTED_SOURCE_REF)?;
        let expected_source_commit = required_env(ENV_EXPECTED_SOURCE_COMMIT)?;
        let source_commit = optional_env(ENV_SOURCE_COMMIT)?
            .unwrap_or_else(|| expected_source_commit.clone());
        ensure!(
            source_commit == expected_source_commit,
            "source commit differs from expected source commit"
        );
        let channel = required_env(ENV_PACKAGE_CHANNEL)?;
        let manifest_schema = required_env(ENV_EXPECTED_MANIFEST_SCHEMA)?;

        Ok(Self {
            workspace,
            source_checkout,
            package_relative,
            verified_package_dir,
            scratch_dir,
            source_repository,
            source_ref,
            expected_source_ref,
            source_commit,
            channel,
            manifest_schema,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    name: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    assets: Vec<Asset>,
    schema: String,
    source_commit: String,
    source_ref: String,
    source_repository: String,
    supporting_assets: Vec<Asset>,
    version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    manifest: Manifest,
    source_digest: String,
    source_ref: String,
    source_repository: String,
}

#[derive(Clone, Debug)]
struct SourceEntry {
    mode: u32,
    kind: String,
    oid: String,
    path: String,
}

#[derive(Clone, Debug)]
enum MemberKind {
    Directory,
    File(Vec<u8>),
    Symlink(String),
}

#[derive(Clone, Debug)]
struct ArchiveMember {
    path: String,
    kind: MemberKind,
    mode: u32,
}

/// Produce exactly the declared four-file package in the pre-created handoff.
/// Any pre-existing file, including output left by a failed prior invocation,
/// is rejected instead of reused.
pub fn produce(config: &ProducerConfig) -> Result<()> {
    let source = validate_config(config, true)?;
    validate_handoff(config, &source, true)?;
    let files = expected_package(config, &source)?;
    for (name, bytes) in &files {
        write_new_file(&config.verified_package_dir.join(name), bytes)
            .with_context(|| format!("write package asset {name}"))?;
    }
    verify_expected_files(&config.verified_package_dir, &files)
}

/// Verify the complete handoff by regenerating its deterministic bytes from
/// the admitted Git object and comparing every declared file byte-for-byte.
pub fn verify(config: &ProducerConfig) -> Result<()> {
    let source = validate_config(config, false)?;
    validate_handoff(config, &source, false)?;
    let expected = expected_package(config, &source)?;
    verify_expected_files(&config.verified_package_dir, &expected)
}

fn verify_expected_files(directory: &Path, expected: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let actual_names = top_level_file_names(directory)?;
    let expected_names = expected.keys().cloned().collect::<BTreeSet<_>>();
    ensure!(
        actual_names == expected_names,
        "package handoff has missing, extra, or non-regular entries: expected {expected_names:?}, got {actual_names:?}"
    );
    for (name, expected_bytes) in expected {
        let path = directory.join(&name);
        let actual_bytes = fs::read(&path).with_context(|| format!("read package asset {name}"))?;
        ensure!(
            actual_bytes.as_slice() == expected_bytes.as_slice(),
            "package asset {name} differs from the admitted source identity or deterministic output"
        );
    }
    Ok(())
}

struct ValidatedSource {
    root: PathBuf,
    commit: String,
    commit_epoch: u64,
    base_version: String,
}

fn validate_config(config: &ProducerConfig, require_scratch: bool) -> Result<ValidatedSource> {
    ensure!(
        config.source_repository == SOURCE_REPOSITORY,
        "source repository must be {SOURCE_REPOSITORY}"
    );
    ensure!(
        config.source_ref == SOURCE_REF,
        "source ref must be {SOURCE_REF}"
    );
    ensure!(
        config.expected_source_ref == SOURCE_REF,
        "expected source ref must be {SOURCE_REF}"
    );
    ensure!(
        config.source_ref == config.expected_source_ref,
        "source ref differs from the configured ref"
    );
    ensure!(
        config.channel == "preview",
        "Homebrew preview channel must be preview"
    );
    ensure!(
        config.manifest_schema == MANIFEST_SCHEMA,
        "manifest schema must be {MANIFEST_SCHEMA}"
    );
    ensure!(
        is_lower_sha(&config.source_commit),
        "source commit must be 40 lowercase hexadecimal characters"
    );
    ensure!(
        config.package_relative == Path::new(PACKAGE_RELATIVE),
        "package directory must be {PACKAGE_RELATIVE}"
    );
    validate_relative_path(&config.package_relative)?;

    let workspace = canonical_dir(&config.workspace, "workspace")?;
    let source_root = canonical_dir(&config.source_checkout, "source checkout")?;
    let handoff = config
        .verified_package_dir
        .canonicalize()
        .context("canonicalize verified package directory")?;
    ensure!(
        handoff == workspace.join(&config.package_relative),
        "verified package directory differs from workspace/PACKAGE_DIR"
    );
    if require_scratch || !config.scratch_dir.as_os_str().is_empty() {
        let scratch = canonical_dir(&config.scratch_dir, "package scratch")?;
        ensure!(
            !scratch.starts_with(&workspace),
            "package scratch must be outside the source checkout workspace"
        );
    }

    let top = git_output(&source_root, &["rev-parse", "--show-toplevel"])?;
    let git_root = canonical_dir(Path::new(top.trim()), "Git top-level")?;
    ensure!(
        git_root == source_root,
        "source checkout path is not the Git top-level"
    );
    let commit = git_output(&source_root, &["rev-parse", "HEAD^{commit}"])?;
    ensure!(
        commit == config.source_commit,
        "source checkout HEAD differs from the admitted source commit"
    );
    let tree = git_output(&source_root, &["rev-parse", "HEAD^{tree}"])?;
    ensure!(is_lower_sha(&tree), "source checkout tree ID is invalid");
    if let Ok(expected_tree) = env::var(ENV_EXPECTED_SOURCE_TREE) {
        ensure!(
            tree == expected_tree,
            "source checkout tree differs from the admitted source tree"
        );
    }
    let origin = git_output(&source_root, &["remote", "get-url", "origin"])?;
    ensure!(
        normalize_github_repository(origin.trim()).as_deref()
            == Some(config.source_repository.as_str()),
        "source checkout origin does not match the configured repository"
    );
    let status = git_output(
        &source_root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude)target/homebrew-preview-package",
        ],
    )?;
    ensure!(
        status.is_empty(),
        "source checkout is dirty outside the package handoff"
    );

    let commit_epoch = git_output(&source_root, &["show", "-s", "--format=%ct", &commit])?
        .trim()
        .parse::<u64>()
        .context("source commit timestamp is invalid")?;
    let base_version = runner_version(&source_root)?;

    Ok(ValidatedSource {
        root: source_root,
        commit,
        commit_epoch,
        base_version,
    })
}

fn validate_handoff(
    config: &ProducerConfig,
    source: &ValidatedSource,
    must_be_empty: bool,
) -> Result<()> {
    let parent = config
        .verified_package_dir
        .parent()
        .context("package handoff has no parent")?;
    ensure!(parent.is_dir(), "package handoff parent does not exist");
    let mut current = config
        .workspace
        .canonicalize()
        .context("canonicalize workspace")?;
    for component in config.package_relative.components() {
        let Component::Normal(name) = component else {
            bail!("package handoff path has an unsafe component");
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("inspect handoff component {}", current.display()))?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "package handoff contains a symlink component"
        );
        ensure!(
            metadata.is_dir(),
            "package handoff component is not a directory"
        );
    }
    let actual_handoff = config
        .verified_package_dir
        .canonicalize()
        .context("canonicalize package handoff")?;
    ensure!(
        current == actual_handoff,
        "package handoff path changed during validation"
    );
    if must_be_empty {
        ensure!(
            fs::read_dir(&config.verified_package_dir)?.next().is_none(),
            "package handoff is not empty; refusing stale or partial output"
        );
    }
    ensure!(
        source.root == config.source_checkout.canonicalize()?,
        "source checkout changed during package production"
    );
    Ok(())
}

fn expected_package(
    config: &ProducerConfig,
    source: &ValidatedSource,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let archive = source_archive(source)?;
    let archive_digest = sha256_hex(&archive);
    let version = format!(
        "{}-preview.{}+{}",
        source.base_version,
        git_output(&source.root, &["rev-list", "--count", &source.commit])?.trim(),
        &source.commit[..7]
    );
    ensure!(
        valid_preview_version(&version, &source.base_version, &source.commit),
        "derived preview version is invalid"
    );
    let manifest = Manifest {
        assets: vec![Asset {
            name: PAYLOAD_NAME.to_owned(),
            sha256: archive_digest,
        }],
        schema: config.manifest_schema.clone(),
        source_commit: source.commit.clone(),
        source_ref: config.source_ref.clone(),
        source_repository: config.source_repository.clone(),
        supporting_assets: Vec::new(),
        version,
    };
    // package-release requires at least one supporting asset. SHA256SUMS is
    // derived from the immutable payload and covers exactly that payload.
    let checksum = format!("{}  {PAYLOAD_NAME}\n", manifest.assets[0].sha256);
    let manifest = Manifest {
        supporting_assets: vec![Asset {
            name: CHECKSUMS_NAME.to_owned(),
            sha256: sha256_hex(checksum.as_bytes()),
        }],
        ..manifest
    };
    let identity = Identity {
        source_digest: source.commit.clone(),
        source_ref: config.source_ref.clone(),
        source_repository: config.source_repository.clone(),
        manifest: manifest.clone(),
    };
    let mut files = BTreeMap::new();
    files.insert(PAYLOAD_NAME.to_owned(), archive);
    files.insert(CHECKSUMS_NAME.to_owned(), checksum.into_bytes());
    files.insert(MANIFEST_NAME.to_owned(), json_bytes(&manifest)?);
    files.insert(IDENTITY_NAME.to_owned(), json_bytes(&identity)?);
    Ok(files)
}

fn source_archive(source: &ValidatedSource) -> Result<Vec<u8>> {
    let entries = source_entries(source)?;
    let object_contents = read_blobs(&source.root, &entries)?;
    let root_name = format!("{ARCHIVE_ROOT_PREFIX}{}", source.commit);
    let mut members = Vec::new();
    members.push(ArchiveMember {
        path: format!("{root_name}/"),
        kind: MemberKind::Directory,
        mode: 0o755,
    });

    let mut directories = BTreeSet::new();
    for entry in &entries {
        validate_source_path(&entry.path)?;
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            let text = path.to_str().context("tracked source path is not UTF-8")?;
            if text.is_empty() || text == "." {
                break;
            }
            directories.insert(text.to_owned());
            parent = path.parent();
        }
    }
    for directory in directories {
        members.push(ArchiveMember {
            path: format!("{root_name}/{directory}/"),
            kind: MemberKind::Directory,
            mode: 0o755,
        });
    }

    for entry in entries {
        let bytes = object_contents
            .get(&entry.oid)
            .context("Git object contents are missing")?;
        let (kind, mode) = match entry.mode {
            0o100644 => (MemberKind::File(bytes.clone()), 0o644),
            0o100755 => (MemberKind::File(bytes.clone()), 0o755),
            0o120000 => {
                let link =
                    std::str::from_utf8(bytes).context("tracked symlink target is not UTF-8")?;
                validate_symlink(&entry.path, link)?;
                (MemberKind::Symlink(link.to_owned()), 0o777)
            }
            _ => bail!(
                "unsupported tracked Git mode {:o} for {}",
                entry.mode,
                entry.path
            ),
        };
        ensure!(
            entry.kind == "blob",
            "tracked entry {} is not a Git blob",
            entry.path
        );
        let path = format!("{root_name}/{}", entry.path);
        members.push(ArchiveMember { path, kind, mode });
    }
    let marker_path = format!("{root_name}/{MARKER_NAME}");
    ensure!(
        members.iter().all(|member| member.path != marker_path),
        "source tree already contains the reserved Homebrew identity marker"
    );
    members.push(ArchiveMember {
        path: marker_path,
        kind: MemberKind::File(format!("{}\n", source.commit).into_bytes()),
        mode: 0o644,
    });
    members.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));

    let encoder = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), Compression::default());
    let mut archive = TarBuilder::new(encoder);
    archive.mode(HeaderMode::Deterministic);
    for member in members {
        append_member(&mut archive, member, source.commit_epoch)?;
    }
    let encoder = archive.into_inner().context("finish source tar archive")?;
    encoder.finish().context("finish source gzip archive")
}

fn append_member(
    archive: &mut TarBuilder<GzEncoder<Vec<u8>>>,
    member: ArchiveMember,
    mtime: u64,
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
    header.set_mode(member.mode);
    header.set_username("")?;
    header.set_groupname("")?;
    match member.kind {
        MemberKind::Directory => {
            header.set_entry_type(EntryType::Directory);
            header.set_size(0);
            archive.append_data(&mut header, &member.path, std::io::empty())?;
        }
        MemberKind::File(bytes) => {
            header.set_entry_type(EntryType::Regular);
            header.set_size(bytes.len() as u64);
            archive.append_data(&mut header, &member.path, bytes.as_slice())?;
        }
        MemberKind::Symlink(target) => {
            header.set_entry_type(EntryType::Symlink);
            header.set_size(0);
            archive.append_link(&mut header, &member.path, target)?;
        }
    }
    Ok(())
}

fn source_entries(source: &ValidatedSource) -> Result<Vec<SourceEntry>> {
    let output = git_bytes(
        &source.root,
        &["ls-tree", "-rz", "--full-tree", &source.commit],
    )?;
    let mut entries = Vec::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed git ls-tree record")?;
        let metadata =
            std::str::from_utf8(&record[..separator]).context("Git tree metadata is not UTF-8")?;
        let path = std::str::from_utf8(&record[separator + 1..])
            .context("tracked source path is not UTF-8")?;
        let mut columns = metadata.split_ascii_whitespace();
        let mode = u32::from_str_radix(columns.next().context("Git tree mode is missing")?, 8)?;
        let kind = columns
            .next()
            .context("Git tree object type is missing")?
            .to_owned();
        let oid = columns
            .next()
            .context("Git tree object ID is missing")?
            .to_owned();
        ensure!(columns.next().is_none(), "malformed Git tree metadata");
        ensure!(is_lower_sha(&oid), "tracked Git object ID is invalid");
        validate_source_path(path)?;
        ensure!(
            !path
                .split('/')
                .any(|part| part == "target" || part == ".git"),
            "source archive contains forbidden generated path {path}"
        );
        entries.push(SourceEntry {
            mode,
            kind,
            oid,
            path: path.to_owned(),
        });
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    Ok(entries)
}

fn read_blobs(source: &Path, entries: &[SourceEntry]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut child = Command::new("git")
        .current_dir(source)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start Git object reader")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .context("open Git object reader input")?;
        for entry in entries {
            writeln!(stdin, "{}", entry.oid)?;
        }
    }
    let output = child
        .wait_with_output()
        .context("wait for Git object reader")?;
    ensure!(
        output.status.success(),
        "Git object reader failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let mut reader = BufReader::new(output.stdout.as_slice());
    let mut blobs = BTreeMap::new();
    for entry in entries {
        let mut header = Vec::new();
        reader.read_until(b'\n', &mut header)?;
        ensure!(
            header.last() == Some(&b'\n'),
            "Git object response header is truncated"
        );
        header.pop();
        let header =
            std::str::from_utf8(&header).context("Git object response header is not UTF-8")?;
        let mut fields = header.split_ascii_whitespace();
        ensure!(
            fields.next() == Some(entry.oid.as_str()),
            "Git returned an unexpected object"
        );
        ensure!(
            fields.next() == Some("blob"),
            "Git object {} is not a blob",
            entry.oid
        );
        let size = fields
            .next()
            .context("Git blob size is missing")?
            .parse::<usize>()?;
        ensure!(
            fields.next().is_none(),
            "malformed Git blob response header"
        );
        let mut contents = vec![0; size];
        reader.read_exact(&mut contents)?;
        let mut separator = [0; 1];
        reader.read_exact(&mut separator)?;
        ensure!(separator[0] == b'\n', "Git blob response is not terminated");
        blobs.entry(entry.oid.clone()).or_insert(contents);
    }
    let mut trailing = Vec::new();
    reader.read_to_end(&mut trailing)?;
    ensure!(trailing.is_empty(), "Git returned unexpected extra objects");
    Ok(blobs)
}

fn runner_version(root: &Path) -> Result<String> {
    let manifest_path = root.join("crates/velnor-runner/Cargo.toml");
    let manifest_text =
        fs::read_to_string(&manifest_path).context("read velnor-runner manifest")?;
    let manifest = toml::from_str::<toml::Value>(&manifest_text)
        .context("parse velnor-runner manifest")?;
    let version = manifest
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .context("velnor-runner package version is missing")?
        .to_owned();
    ensure!(
        version.split('.').count() == 3
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        "velnor-runner package version must be numeric major.minor.patch"
    );
    let lock_text = fs::read_to_string(root.join("Cargo.lock")).context("read Cargo.lock")?;
    let lock = toml::from_str::<toml::Value>(&lock_text).context("parse Cargo.lock")?;
    let locked = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package.get("name").and_then(toml::Value::as_str) == Some("velnor-runner")
            })
        })
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str);
    ensure!(
        locked == Some(version.as_str()),
        "Cargo.lock runner version differs from the package manifest"
    );
    Ok(version)
}

fn top_level_file_names(directory: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(directory).context("read package handoff")? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.file_type().is_file(),
            "package handoff contains a directory, symlink, or special file"
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("package filename is not UTF-8"))?;
        names.insert(name);
    }
    Ok(names)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {} without replacing existing bytes", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(root, args)?)
        .context("Git output is not UTF-8")?
        .trim_end()
        .to_owned())
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn path_env(key: &str) -> Result<Option<PathBuf>> {
    env::var_os(key)
        .map(PathBuf::from)
        .map_or(Ok(None), |path| {
            ensure!(!path.as_os_str().is_empty(), "{key} must not be empty");
            Ok(Some(path))
        })
}

fn optional_env(key: &str) -> Result<Option<String>> {
    env::var(key).map(Some).or_else(|error| match error {
        env::VarError::NotPresent => Ok(None),
        env::VarError::NotUnicode(_) => bail!("{key} is not valid Unicode"),
    })
}

fn required_env(key: &str) -> Result<String> {
    optional_env(key)?
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{key} is required"))
}

fn canonical_dir(path: &Path, label: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "{label} must not be a symlink"
    );
    ensure!(metadata.is_dir(), "{label} must be an existing directory");
    path.canonicalize()
        .with_context(|| format!("canonicalize {label}"))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(!path.is_absolute(), "package directory must be relative");
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .context("package path component is not UTF-8")?;
                ensure!(
                    !name.is_empty()
                        && name != "."
                        && name != ".."
                        && name.bytes().all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'.' | b'_' | b'-')),
                    "package directory has a non-portable component"
                );
            }
            _ => bail!("package directory has an unsafe component"),
        }
    }
    Ok(())
}

fn validate_source_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.starts_with('/') && !path.contains('\\'),
        "tracked source path is unsafe"
    );
    for component in path.split('/') {
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "tracked source path has an unsafe component"
        );
    }
    Ok(())
}

fn validate_symlink(path: &str, target: &str) -> Result<()> {
    ensure!(
        !target.is_empty() && !target.starts_with('/') && !target.contains('\\'),
        "tracked symlink target is unsafe"
    );
    let mut resolved = Path::new(path)
        .parent()
        .context("symlink path has no parent")?
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => ensure!(
                !resolved.pop().is_none(),
                "tracked symlink escapes the source archive root"
            ),
            _ => resolved.push(component.to_owned()),
        }
    }
    Ok(())
}

fn normalize_github_repository(remote: &str) -> Option<String> {
    let path = if let Some(rest) = remote.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()? == "github.com").then_some(path)?
    } else if let Some(rest) = remote.strip_prefix("http://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()? == "github.com").then_some(path)?
    } else if let Some(rest) = remote.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };
    if path.contains(['?', '#', '%']) {
        return None;
    }
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn is_lower_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_preview_version(version: &str, base: &str, source_commit: &str) -> bool {
    let Some(rest) = version.strip_prefix(&format!("{base}-preview.")) else {
        return false;
    };
    let Some((count, short)) = rest.split_once('+') else {
        return false;
    };
    !count.is_empty()
        && count.bytes().all(|byte| byte.is_ascii_digit())
        && short == &source_commit[..7]
}

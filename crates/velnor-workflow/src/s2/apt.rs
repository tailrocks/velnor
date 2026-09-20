//! Typed APT feed primitives: the spec §7 capability model.
//!
//! This module is the narrow typed verifier contract over the audited
//! `verify-release.sh` behavior (sourced from the feed repository through the
//! public `gh api` read path; this repository ships no such script). Every
//! value that reaches a command line is a validated scalar: fixed argument
//! vectors are built only from these types, so configuration can never
//! contribute a command, pattern, URL host, ref, or shell fragment.
//!
//! The module links no runner code: the shipped generator never links the
//! runner crate. Claim-check semantics mirror
//! `velnor-runner/src/release.rs` (`ReleaseRecord::verify`, publication
//! binding, fingerprint shape), and the dev-dependency contract tests below
//! prove the mirrored validators agree with the runner's own parsers. Full
//! record-verify parity is intentionally impossible: the runner pins its own
//! source repository while this generic engine must never name a consumer.
//!
//! Product literals stay parameters. Package, source repository, origin,
//! signer, and the consumer manifest schema flow through; nothing here
//! branches on a name.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};
use time::format_description::well_known::Rfc2822;
use time::OffsetDateTime;
use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, fstat, flock,
    fsync, mkdirat, open, openat, renameat_with, statat, unlinkat,
};
use rustix::io::Errno;

use super::GeneratorError;

/// The release-record schema the coherence chain authenticates.
pub(crate) const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";
/// The publication-record schema staged publications emit.
pub(crate) const PUBLICATION_RECORD_SCHEMA: &str = "velnor.publication-record/v1";
/// The channel-state schema channel updates emit.
pub(crate) const PACKAGE_STATE_SCHEMA: &str = "velnor.apt-package-state.v1";
/// The preview APT suite marker used in signed publication records and
/// previous pointers. Source releases use `preview_source_tag` instead.
pub(crate) const PREVIEW_TAG: &str = "preview";
/// The GitHub Actions predicate emitted by the isolated source release signer.
pub(crate) const RELEASE_SOURCE_PREDICATE: &str = "https://velnor.dev/attestations/release-source/v1";
/// The only workflow authorized to attest stable source release artifacts.
const RELEASE_SOURCE_SIGNER_WORKFLOW: &str = ".github/workflows/release.yml";
/// Default preview source ref used by low-level fixtures.
/// Rendered releases override this with the configured workflow branch.
pub(crate) const PREVIEW_SOURCE_REF: &str = "refs/heads/main";
/// The sentinel a successful verification arms. Publication refuses to run
/// without it, so every rejection below lands before any mutation.
pub(crate) const SENTINEL_FILE: &str = ".reprepro-ok";
/// The exact architecture set a coherent release covers.
pub(crate) const REQUIRED_ARCHES: [&str; 2] = ["amd64", "arm64"];
/// The stable suite identity.
pub(crate) const STABLE_SUITE: &str = "stable";
/// The preview suite identity.
pub(crate) const PREVIEW_SUITE: &str = "preview";
/// The repository component both suites publish.
pub(crate) const MAIN_COMPONENT: &str = "main";
/// Stable coherence inputs served by the source release.
pub(crate) const RECORD_FILE: &str = "release-record.json";
/// The detached checksum of the release record.
pub(crate) const RECORD_SIDECAR: &str = "release-record.json.sha256";
/// The compiled manifest served by the source release.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
/// The detached checksum of the compiled manifest.
pub(crate) const MANIFEST_SIDECAR: &str = "manifest.json.sha256";
/// The source-owned coherence record of the rolling preview release.
pub(crate) const PREVIEW_MANIFEST_FILE: &str = "release-manifest.json";
/// The preview checksum list binding both preview debs.
pub(crate) const SHA256SUMS_FILE: &str = "SHA256SUMS";
/// The only implemented previous-version retention count.
pub(crate) const IMPLEMENTED_RETENTION: u32 = 1;
/// The longest accepted feed description line.
const MAX_DESCRIPTION_LEN: usize = 200;

/// Whether a value is exactly `length` lowercase hex characters.
pub(crate) fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Whether a value is a 40-hex source commit.
pub(crate) fn valid_commit(value: &str) -> bool {
    is_lower_hex(value, 40)
}

/// Whether a value is a 64-hex digest.
pub(crate) fn valid_digest(value: &str) -> bool {
    is_lower_hex(value, 64)
}

/// Whether a value is a full signer fingerprint: 40 uppercase hex characters,
/// the shape the runner's publication binding requires.
pub(crate) fn is_full_fingerprint(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'A'..=b'F'))
}

/// Normalize a fingerprint the way the oracle does: strip spaces, uppercase.
pub(crate) fn normalize_fingerprint(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != ' ')
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Whether the live signing key is the pinned publisher identity.
pub(crate) fn fingerprints_match(live: &str, pinned: &str) -> bool {
    normalize_fingerprint(live) == normalize_fingerprint(pinned)
}

/// Whether a value is an `owner/name` repository slug. Both sides are
/// non-empty dot/underscore/hyphen/alphanumeric runs; nothing else — in
/// particular no scheme, no whitespace, no shell metacharacters.
pub(crate) fn valid_repository_slug(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    let solid = |side: &str| {
        !side.is_empty()
            && !matches!(side, "." | "..")
            && side.bytes().any(|byte| byte.is_ascii_alphanumeric())
    };
    solid(owner)
        && solid(name)
        && !name.contains('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

/// Whether a value is a safe Debian package name.
pub(crate) fn valid_package_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Whether a value is a safe installed binary name.
pub(crate) fn valid_binary_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether a value names an environment secret (never a secret value): a
/// shell-safe uppercase identifier the publisher resolves from the
/// `package-feed` environment only.
pub(crate) fn valid_secret_ref(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && value.len() <= 64
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_preview_source_ref(value: &str) -> bool {
    value
        .strip_prefix("refs/heads/")
        .is_some_and(crate::s2::runtime::valid_branch)
}

/// Whether a value is a safe keyring path: a relative single-level-or-deeper
/// path with no parent traversal, no leading slash, and no shell metacharacters.
pub(crate) fn valid_keyring_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
        && !value.split('/').any(|part| part.is_empty() || part == "..")
}

/// Whether a value is a safe packaged-identity directory name.
pub(crate) fn valid_identity_dir(value: &str) -> bool {
    valid_binary_name(value)
}

/// Whether a value is a safe `Origin`/`Label` line: a leading alphanumeric
/// run with interior spaces and punctuation, never a control character.
pub(crate) fn valid_origin(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'.' | b'_' | b'-' | b'+')
        })
}

/// Whether a value is a safe one-line feed description.
pub(crate) fn valid_description(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DESCRIPTION_LEN
        && value.bytes().all(|byte| matches!(byte, 0x20..=0x7e))
}

/// Whether a value is a safe feed base URL: an `https` URL with a host and an
/// optional path, no userinfo, no whitespace, no shell metacharacters. The URL
/// is only ever passed as a single `curl` argument, never interpreted.
pub(crate) fn valid_feed_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    !host.is_empty()
        && !host.contains('@')
        && !host.contains(':')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && rest.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/' | b'~')
        })
}

/// Whether a value is a safe staging directory: a relative path that cannot
/// escape the working tree, so the stable wipe cannot touch anything else.
pub(crate) fn valid_staging_dir(value: &str) -> bool {
    valid_keyring_path(value)
        && value != "."
        && !value.starts_with('.')
        && !value.split('/').any(|part| part == ".")
}

static STAGING_TXN_SEQ: AtomicU64 = AtomicU64::new(0);
const STAGING_MARKER: &str = ".velnor-staging-transaction";

/// Copy and replace a caller-owned staging directory atomically. The target
/// and every copied descendant must be ordinary directories/files below the
/// current working directory; symlinks and special files fail closed.
struct StagingTransaction {
    target: PathBuf,
    working: PathBuf,
    anchor: PathBuf,
    parent: OwnedFd,
    target_name: OsString,
    working_name: OsString,
    working_fd: OwnedFd,
    _target_fd: Option<OwnedFd>,
    working_identity: FileIdentity,
    target_identity: Option<FileIdentity>,
    lock_file: std::fs::File,
    committed: bool,
}

impl StagingTransaction {
    fn new(staging: &Path) -> Result<Self, GeneratorError> {
        let name = staging
            .to_str()
            .ok_or_else(|| GeneratorError::usage("staging directory is not UTF-8"))?;
        if !valid_staging_dir(name) {
            return Err(GeneratorError::usage(
                "staging directory must be a relative path without traversal",
            ));
        }
        let root = std::env::current_dir()
            .map_err(|error| GeneratorError::usage(format!("read working directory: {error}")))?;
        let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        let mut parent = open(".", directory_flags, Mode::empty()).map_err(|error| {
            GeneratorError::usage(format!("open staging root without following links: {error}"))
        })?;
        let mut parent_path = root.clone();
        let mut components = staging.components().peekable();
        let mut final_name = None;
        while let Some(component) = components.next() {
            let std::path::Component::Normal(component) = component else {
                return Err(GeneratorError::usage(
                    "staging directory contains a non-normal path component",
                ));
            };
            if components.peek().is_none() {
                final_name = Some(component.to_owned());
                break;
            }
            let next_path = parent_path.join(component);
            parent = match openat(&parent, component, directory_flags, Mode::empty()) {
                Ok(directory) => directory,
                Err(Errno::NOENT) => {
                    match mkdirat(&parent, component, Mode::from_raw_mode(0o755)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => {
                            return Err(GeneratorError::usage(format!(
                                "create staging parent {}: {error}",
                                next_path.display()
                            )));
                        }
                    }
                    openat(&parent, component, directory_flags, Mode::empty()).map_err(|error| {
                        GeneratorError::usage(format!(
                            "staging parent is not a real directory: {} ({error})",
                            next_path.display()
                        ))
                    })?
                }
                Err(error) => {
                    return Err(GeneratorError::usage(format!(
                        "staging parent is not a real directory: {} ({error})",
                        next_path.display()
                    )));
                }
            };
            parent_path = next_path;
        }
        let final_name = final_name.ok_or_else(|| {
            GeneratorError::usage("staging directory must name a child of the working directory")
        })?;
        let target = parent_path.join(&final_name);
        let target_name = final_name;
        let lock_name = transaction_lock_name(&target_name)?;
        let mut lock_file = open_staging_lock(&parent, &lock_name, &target)?;
        recover_staging_transaction(&parent, &target_name, &mut lock_file, &target)?;
        scavenge_stale_staging_siblings(&parent, &target_name, &target)?;
        let target_fd = match openat(&parent, &target_name, directory_flags, Mode::empty()) {
            Ok(directory) => {
                validate_staging_tree_fd(&directory, &target)?;
                Some(directory)
            }
            Err(Errno::NOENT) => None,
            Err(error) => {
                return Err(GeneratorError::usage(format!(
                    "staging target is not a real directory: {} ({error})",
                    target.display()
                )));
            }
        };
        let target_identity = target_fd
            .as_ref()
            .map(identity_fd)
            .transpose()?;
        let working_name = {
            let mut created = None;
            for _ in 0..128 {
                let sequence = STAGING_TXN_SEQ.fetch_add(1, Ordering::SeqCst);
                let name = transaction_sibling_name(&target_name, "new", sequence)?;
                match mkdirat(&parent, &name, Mode::from_raw_mode(0o700)) {
                    Ok(()) => {
                        created = Some(name.clone());
                        break;
                    }
                    Err(Errno::EXIST) => continue,
                    Err(error) => {
                        return Err(GeneratorError::usage(format!(
                            "create sibling staging tree: {error}"
                        )));
                    }
                }
            }
            created.ok_or_else(|| {
                GeneratorError::usage("could not allocate a unique sibling staging tree")
            })?
        };
        fsync(&parent).map_err(|error| {
            GeneratorError::usage(format!("sync staging parent after create: {error}"))
        })?;
        let working = parent_path.join(&working_name);
        let working_fd = match openat(&parent, &working_name, directory_flags, Mode::empty()) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = unlinkat(&parent, &working_name, AtFlags::REMOVEDIR);
                return Err(GeneratorError::usage(format!(
                    "open sibling staging tree {}: {error}",
                    working.display()
                )));
            }
        };
        let working_identity = identity_fd(&working_fd)?;
        let marker = match openat(
            &working_fd,
            STAGING_MARKER,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(marker) => marker,
            Err(error) => {
                let _ = remove_staging_directory_if_identity(
                    &parent,
                    &working_name,
                    working_identity,
                    &working,
                );
                return Err(GeneratorError::usage(format!(
                    "create staging transaction marker: {error}"
                )));
            }
        };
        let mut marker = std::fs::File::from(marker);
        if let Err(error) = marker
            .write_all(working_name.to_string_lossy().as_bytes())
            .and_then(|()| marker.sync_all())
        {
            drop(marker);
            let _ = remove_staging_directory_if_identity(
                &parent,
                &working_name,
                working_identity,
                &working,
            );
            return Err(GeneratorError::usage(format!("write staging marker: {error}")));
        }
        fsync(&working_fd).map_err(|error| {
            GeneratorError::usage(format!("sync new staging tree: {error}"))
        })?;
        if let Some(target_fd) = target_fd.as_ref() {
            if let Err(error) = copy_staging_tree_fd(target_fd, &working_fd, &target, &working) {
                let _ = remove_staging_directory_if_identity(
                    &parent,
                    &working_name,
                    working_identity,
                    &working,
                );
                return Err(error);
            }
        }
        let fd_root = if Path::new("/proc/self/fd").is_dir() {
            Path::new("/proc/self/fd")
        } else {
            Path::new("/dev/fd")
        };
        let anchor = fd_root.join(working_fd.as_fd().as_raw_fd().to_string());
        Ok(Self {
            target,
            working,
            anchor,
            parent,
            target_name,
            working_name,
            working_fd,
            _target_fd: target_fd,
            working_identity,
            target_identity,
            lock_file,
            committed: false,
        })
    }

    fn path(&self) -> &Path {
        &self.anchor
    }

    fn commit(mut self) -> Result<(), GeneratorError> {
        if identity_at(&self.parent, &self.working_name)? != Some(self.working_identity) {
            return Err(GeneratorError::usage(
                "staging transaction path no longer names its validated directory handle",
            ));
        }
        validate_staging_tree_fd(&self.working_fd, &self.working)?;
        sync_staging_tree_fd(&self.working_fd, &self.working)?;
        let journal = StagingJournal {
            phase: "prepared".to_owned(),
            working_name: self.working_name.to_string_lossy().into_owned(),
            working_identity: self.working_identity,
            target_identity: self.target_identity,
        };
        write_staging_journal(&mut self.lock_file, &journal)?;
        fsync(&self.parent).map_err(|error| {
            GeneratorError::usage(format!("sync staging journal parent: {error}"))
        })?;
        unlinkat(&self.working_fd, STAGING_MARKER, AtFlags::empty()).map_err(|error| {
            GeneratorError::usage(format!("remove staging transaction marker: {error}"))
        })?;
        fsync(&self.working_fd).map_err(|error| {
            GeneratorError::usage(format!("sync staged tree before replacement: {error}"))
        })?;
        validate_staging_tree_fd(&self.working_fd, &self.working)?;

        if identity_at(&self.parent, &self.working_name)? != Some(self.working_identity)
            || identity_at(&self.parent, &self.target_name)? != self.target_identity
        {
            return Err(GeneratorError::usage(
                "staging target or working path changed before atomic replacement",
            ));
        }
        if self.target_identity.is_some() {
            renameat_with(
                &self.parent,
                &self.working_name,
                &self.parent,
                &self.target_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|error| {
                GeneratorError::usage(format!("atomically exchange staging tree: {error}"))
            })?;
            if identity_at(&self.parent, &self.target_name)? != Some(self.working_identity)
                || identity_at(&self.parent, &self.working_name)? != self.target_identity
            {
                return Err(GeneratorError::usage(
                    "atomic exchange did not install the validated staging directory inode",
                ));
            }
        } else {
            renameat_with(
                &self.parent,
                &self.working_name,
                &self.parent,
                &self.target_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                GeneratorError::usage(format!(
                    "atomically install staging tree without replacing a raced target: {error}"
                ))
            })?;
            if identity_at(&self.parent, &self.target_name)? != Some(self.working_identity)
                || identity_at(&self.parent, &self.working_name)?.is_some()
            {
                return Err(GeneratorError::usage(
                    "atomic install did not preserve the validated staging directory inode",
                ));
            }
        }
        fsync(&self.parent).map_err(|error| {
            GeneratorError::usage(format!("sync staging directory replacement: {error}"))
        })?;
        let committed = StagingJournal {
            phase: "committed".to_owned(),
            ..journal
        };
        write_staging_journal(&mut self.lock_file, &committed)?;
        fsync(&self.parent).map_err(|error| {
            GeneratorError::usage(format!("sync committed staging journal: {error}"))
        })?;
        self.committed = true;
        if let Some(old_identity) = self.target_identity {
            remove_staging_directory_if_identity(
                &self.parent,
                &self.working_name,
                old_identity,
                &self.working,
            )?;
        }
        fsync(&self.parent).map_err(|error| {
            GeneratorError::usage(format!("sync staging cleanup before clearing journal: {error}"))
        })?;
        clear_staging_journal(&mut self.lock_file)?;
        fsync(&self.parent).map_err(|error| {
            GeneratorError::usage(format!("sync staging commit cleanup: {error}"))
        })?;
        Ok(())
    }
}

impl Drop for StagingTransaction {
    fn drop(&mut self) {
        if !self.committed {
            if read_staging_journal(&mut self.lock_file)
                .ok()
                .flatten()
                .is_some()
            {
                let _ = recover_staging_transaction(
                    &self.parent,
                    &self.target_name,
                    &mut self.lock_file,
                    &self.target,
                );
            } else {
                let _ = remove_staging_directory_if_identity(
                    &self.parent,
                    &self.working_name,
                    self.working_identity,
                    &self.working,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct StagingJournal {
    phase: String,
    working_name: String,
    working_identity: FileIdentity,
    target_identity: Option<FileIdentity>,
}

fn identity_fd(fd: &impl AsFd) -> Result<FileIdentity, GeneratorError> {
    let metadata = fstat(fd)
        .map_err(|error| GeneratorError::usage(format!("inspect staging directory handle: {error}")))?;
    Ok(FileIdentity {
        device: metadata.st_dev as u64,
        inode: metadata.st_ino as u64,
    })
}

fn identity_at(
    parent: &impl AsFd,
    name: impl rustix::path::Arg,
) -> Result<Option<FileIdentity>, GeneratorError> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => Ok(Some(FileIdentity {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
        })),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(GeneratorError::usage(format!(
            "inspect staging entry identity: {error}"
        ))),
    }
}

fn transaction_lock_name(target_name: &std::ffi::OsStr) -> Result<OsString, GeneratorError> {
    let name = target_name
        .to_str()
        .ok_or_else(|| GeneratorError::usage("staging target name is not UTF-8"))?;
    Ok(OsString::from(format!(".{name}.velnor-lock")))
}

fn transaction_sibling_prefix(target_name: &std::ffi::OsStr) -> Result<String, GeneratorError> {
    let name = target_name
        .to_str()
        .ok_or_else(|| GeneratorError::usage("staging target name is not UTF-8"))?;
    Ok(format!(".{name}.velnor-new-"))
}

fn open_staging_lock(
    parent: &impl AsFd,
    lock_name: &std::ffi::OsStr,
    target: &Path,
) -> Result<std::fs::File, GeneratorError> {
    let descriptor = openat(
        parent,
        lock_name,
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| GeneratorError::usage(format!("open staging transaction lock: {error}")))?;
    let lock = std::fs::File::from(descriptor);
    let metadata = fstat(&lock)
        .map_err(|error| GeneratorError::usage(format!("inspect staging transaction lock: {error}")))?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile || metadata.st_nlink != 1 {
        return Err(GeneratorError::usage(
            "staging transaction lock must be a single-link regular file",
        ));
    }
    fchmod(&lock, Mode::from_raw_mode(0o600))
        .map_err(|error| GeneratorError::usage(format!("secure staging transaction lock: {error}")))?;
    flock(&lock, FlockOperation::LockExclusive)
        .map_err(|error| GeneratorError::usage(format!("lock staging transaction: {error}")))?;
    let lock_identity = FileIdentity {
        device: metadata.st_dev as u64,
        inode: metadata.st_ino as u64,
    };
    if identity_at(parent, lock_name)? != Some(lock_identity) {
        return Err(GeneratorError::usage(format!(
            "staging transaction lock path changed: {}",
            target.display()
        )));
    }
    Ok(lock)
}

fn write_staging_journal(
    lock: &mut std::fs::File,
    journal: &StagingJournal,
) -> Result<(), GeneratorError> {
    let target = journal.target_identity;
    let text = format!(
        "velnor-staging-v1\n{}\n{}\n{}\n{}\n{}\n{}\n",
        journal.phase,
        journal.working_name,
        journal.working_identity.device,
        journal.working_identity.inode,
        target.map_or_else(|| "-".to_owned(), |identity| identity.device.to_string()),
        target.map_or_else(|| "-".to_owned(), |identity| identity.inode.to_string()),
    );
    lock.set_len(0)
        .and_then(|()| lock.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| lock.write_all(text.as_bytes()))
        .and_then(|()| lock.sync_all())
        .map_err(|error| GeneratorError::usage(format!("write staging transaction journal: {error}")))
}

fn read_staging_journal(
    lock: &mut std::fs::File,
) -> Result<Option<StagingJournal>, GeneratorError> {
    lock.seek(SeekFrom::Start(0))
        .map_err(|error| GeneratorError::usage(format!("read staging journal: {error}")))?;
    let mut text = String::new();
    lock.read_to_string(&mut text)
        .map_err(|error| GeneratorError::usage(format!("read staging journal: {error}")))?;
    if text.is_empty() {
        return Ok(None);
    }
    let fields = text.lines().collect::<Vec<_>>();
    if fields.len() != 7 || fields[0] != "velnor-staging-v1" {
        return Err(GeneratorError::usage("staging transaction journal is malformed"));
    }
    let phase = fields[1];
    if !matches!(phase, "prepared" | "committed")
        || fields[2].is_empty()
        || fields[2].contains('/')
    {
        return Err(GeneratorError::usage("staging transaction journal is malformed"));
    }
    let parse_id = |value: &str| {
        value
            .parse::<u64>()
            .map_err(|_| GeneratorError::usage("staging transaction journal identity is malformed"))
    };
    let working_identity = FileIdentity {
        device: parse_id(fields[3])?,
        inode: parse_id(fields[4])?,
    };
    let target_identity = match (fields[5], fields[6]) {
        ("-", "-") => None,
        (device, inode) if device != "-" && inode != "-" => Some(FileIdentity {
            device: parse_id(device)?,
            inode: parse_id(inode)?,
        }),
        _ => return Err(GeneratorError::usage("staging transaction journal is malformed")),
    };
    Ok(Some(StagingJournal {
        phase: phase.to_owned(),
        working_name: fields[2].to_owned(),
        working_identity,
        target_identity,
    }))
}

fn clear_staging_journal(lock: &mut std::fs::File) -> Result<(), GeneratorError> {
    lock.set_len(0)
        .and_then(|()| lock.sync_all())
        .map_err(|error| GeneratorError::usage(format!("clear staging journal: {error}")))
}

fn sync_staging_tree_fd(root: &impl AsFd, display: &Path) -> Result<(), GeneratorError> {
    let mut directory = Dir::read_from(root)
        .map_err(|error| GeneratorError::usage(format!("sync staging tree {}: {error}", display.display())))?;
    loop {
        let Some(entry) = directory.next() else {
            break;
        };
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("list staging tree for sync: {error}")))?;
        let directory_fd = directory
            .fd()
            .map_err(|error| GeneratorError::usage(format!("inspect staging tree for sync: {error}")))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let path = display.join(String::from_utf8_lossy(name.to_bytes()).as_ref());
        let metadata = statat(directory_fd, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| GeneratorError::usage(format!("inspect staging entry {}: {error}", path.display())))?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {
                let child = openat(
                    directory_fd,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| GeneratorError::usage(format!("open staging directory {}: {error}", path.display())))?;
                if identity_fd(&child)? != (FileIdentity { device: metadata.st_dev as u64, inode: metadata.st_ino as u64 }) {
                    return Err(GeneratorError::usage(format!("staging directory changed while syncing: {}", path.display())));
                }
                sync_staging_tree_fd(&child, &path)?;
                fsync(&child).map_err(|error| GeneratorError::usage(format!("sync staging directory {}: {error}", path.display())))?;
            }
            FileType::RegularFile => {
                let file = openat(
                    directory_fd,
                    name,
                    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| GeneratorError::usage(format!("open staging file {}: {error}", path.display())))?;
                let opened = fstat(&file)
                    .map_err(|error| GeneratorError::usage(format!("inspect staging file {}: {error}", path.display())))?;
                if opened.st_dev as u64 != metadata.st_dev as u64
                    || opened.st_ino as u64 != metadata.st_ino as u64
                {
                    return Err(GeneratorError::usage(format!("staging file changed while syncing: {}", path.display())));
                }
                fsync(&file).map_err(|error| GeneratorError::usage(format!("sync staging file {}: {error}", path.display())))?;
            }
            _ => return Err(GeneratorError::usage(format!("staging tree contains a symlink or special file: {}", path.display()))),
        }
    }
    fsync(root).map_err(|error| GeneratorError::usage(format!("sync staging directory {}: {error}", display.display())))
}

fn remove_staging_directory_if_identity(
    parent: &impl AsFd,
    name: &std::ffi::OsStr,
    expected: FileIdentity,
    display: &Path,
) -> Result<(), GeneratorError> {
    let Some(found) = identity_at(parent, name)? else {
        return Ok(());
    };
    if found != expected {
        return Err(GeneratorError::usage(format!(
            "staging cleanup path changed identity: {}",
            display.display()
        )));
    }
    let directory = openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| GeneratorError::usage(format!("open staging cleanup directory: {error}")))?;
    if identity_fd(&directory)? != expected {
        return Err(GeneratorError::usage("staging cleanup directory handle changed identity"));
    }
    let mut entries = Dir::read_from(&directory)
        .map_err(|error| GeneratorError::usage(format!("list staging cleanup directory: {error}")))?;
    loop {
        let Some(entry) = entries.next() else {
            break;
        };
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("list staging cleanup directory: {error}")))?;
        let parent_fd = entries
            .fd()
            .map_err(|error| GeneratorError::usage(format!("inspect staging cleanup directory: {error}")))?;
        let child_name = entry.file_name();
        if matches!(child_name.to_bytes(), b"." | b"..") {
            continue;
        }
        let child_display = display.join(String::from_utf8_lossy(child_name.to_bytes()).as_ref());
        remove_tree_at(&parent_fd, child_name, &child_display)?;
    }
    fsync(&directory)
        .map_err(|error| GeneratorError::usage(format!("sync staging cleanup directory: {error}")))?;
    if identity_at(parent, name)? != Some(expected) {
        return Err(GeneratorError::usage("staging cleanup root changed before unlink"));
    }
    unlinkat(parent, name, AtFlags::REMOVEDIR)
        .map_err(|error| GeneratorError::usage(format!("remove staging cleanup directory: {error}")))
}

fn recover_staging_transaction(
    parent: &impl AsFd,
    target_name: &std::ffi::OsStr,
    lock: &mut std::fs::File,
    target: &Path,
) -> Result<(), GeneratorError> {
    let Some(journal) = read_staging_journal(lock)? else {
        return Ok(());
    };
    let prefix = transaction_sibling_prefix(target_name)?;
    if !journal.working_name.starts_with(&prefix) {
        return Err(GeneratorError::usage("staging journal names an unexpected sibling"));
    }
    let working_name = OsString::from(&journal.working_name);
    let target_identity = identity_at(parent, target_name)?;
    let working_identity = identity_at(parent, &working_name)?;
    match journal.phase.as_str() {
        "prepared" => {
            if target_identity == journal.target_identity
                && working_identity == Some(journal.working_identity)
            {
                remove_staging_directory_if_identity(
                    parent,
                    &working_name,
                    journal.working_identity,
                    &target.with_file_name(&working_name),
                )?;
            } else if journal.target_identity.is_some()
                && target_identity == Some(journal.working_identity)
                && working_identity == journal.target_identity
            {
                renameat_with(
                    parent,
                    &working_name,
                    parent,
                    target_name,
                    RenameFlags::EXCHANGE,
                )
                .map_err(|error| GeneratorError::usage(format!("recover staging exchange: {error}")))?;
                fsync(parent)
                    .map_err(|error| GeneratorError::usage(format!("sync recovered staging exchange: {error}")))?;
                if identity_at(parent, target_name)? != journal.target_identity
                    || identity_at(parent, &working_name)? != Some(journal.working_identity)
                {
                    return Err(GeneratorError::usage("recovered staging exchange has unexpected inodes"));
                }
                remove_staging_directory_if_identity(
                    parent,
                    &working_name,
                    journal.working_identity,
                    &target.with_file_name(&working_name),
                )?;
            } else if journal.target_identity.is_none()
                && target_identity == Some(journal.working_identity)
                && working_identity.is_none()
            {
                remove_staging_directory_if_identity(
                    parent,
                    target_name,
                    journal.working_identity,
                    target,
                )?;
            } else if journal.target_identity.is_none()
                && working_identity == Some(journal.working_identity)
                && target_identity.is_some()
            {
                remove_staging_directory_if_identity(
                    parent,
                    &working_name,
                    journal.working_identity,
                    &target.with_file_name(&working_name),
                )?;
            } else {
                return Err(GeneratorError::usage("staging journal cannot safely recover prepared replacement"));
            }
        }
        "committed" => {
            if target_identity != Some(journal.working_identity) {
                return Err(GeneratorError::usage("staging journal committed inode is not installed at target"));
            }
            if let Some(old_identity) = journal.target_identity {
                if working_identity == Some(old_identity) {
                    remove_staging_directory_if_identity(
                        parent,
                        &working_name,
                        old_identity,
                        &target.with_file_name(&working_name),
                    )?;
                } else if working_identity.is_some() {
                    return Err(GeneratorError::usage("staging journal old target inode is missing"));
                }
            } else if working_identity.is_some() {
                return Err(GeneratorError::usage("staging journal has an unexpected old target"));
            }
        }
        _ => return Err(GeneratorError::usage("staging journal phase is malformed")),
    }
    clear_staging_journal(lock)?;
    fsync(parent).map_err(|error| GeneratorError::usage(format!("sync recovered staging journal: {error}")))
}

fn scavenge_stale_staging_siblings(
    parent: &impl AsFd,
    target_name: &std::ffi::OsStr,
    target: &Path,
) -> Result<(), GeneratorError> {
    let prefix = transaction_sibling_prefix(target_name)?;
    let mut directory = Dir::read_from(parent)
        .map_err(|error| GeneratorError::usage(format!("list staging parent for recovery: {error}")))?;
    let mut stale = Vec::<OsString>::new();
    loop {
        let Some(entry) = directory.next() else {
            break;
        };
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("list staging parent for recovery: {error}")))?;
        let name = entry.file_name();
        if let Ok(name) = name.to_str()
            && name.starts_with(&prefix)
        {
            stale.push(OsString::from(name));
        }
    }
    let mut removed = false;
    for name in stale {
        let display = target.with_file_name(&name);
        let metadata = statat(parent, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| GeneratorError::usage(format!("inspect stale staging sibling: {error}")))?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
            || metadata.st_mode & 0o777 != 0o700
        {
            return Err(GeneratorError::usage(format!(
                "stale staging sibling is not a private directory: {}",
                display.display()
            )));
        }
        let directory = openat(
            parent,
            &name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| GeneratorError::usage(format!("open stale staging sibling: {error}")))?;
        let stale_identity = identity_fd(&directory)?;
        let mut contents = Dir::read_from(&directory)
            .map_err(|error| GeneratorError::usage(format!("list stale staging sibling: {error}")))?;
        let mut empty = true;
        loop {
            let Some(entry) = contents.next() else {
                break;
            };
            let entry = entry
                .map_err(|error| GeneratorError::usage(format!("list stale staging sibling: {error}")))?;
            if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
                empty = false;
                break;
            }
        }
        if empty {
            remove_staging_directory_if_identity(parent, &name, stale_identity, &display)?;
            removed = true;
            continue;
        }
        let marker = openat(
            &directory,
            STAGING_MARKER,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| GeneratorError::usage(format!("stale staging sibling has no ownership marker: {error}")))?;
        let marker_metadata = fstat(&marker)
            .map_err(|error| GeneratorError::usage(format!("inspect stale staging marker: {error}")))?;
        if FileType::from_raw_mode(marker_metadata.st_mode) != FileType::RegularFile
            || marker_metadata.st_nlink != 1
        {
            return Err(GeneratorError::usage("stale staging marker is not a single-link regular file"));
        }
        let mut marker_file = std::fs::File::from(marker);
        let mut marker_text = String::new();
        marker_file
            .read_to_string(&mut marker_text)
            .map_err(|error| GeneratorError::usage(format!("read stale staging marker: {error}")))?;
        if marker_text != name.to_string_lossy() {
            return Err(GeneratorError::usage("stale staging marker does not match its sibling name"));
        }
        remove_staging_directory_if_identity(parent, &name, stale_identity, &display)?;
        removed = true;
    }
    if removed {
        fsync(parent).map_err(|error| {
            GeneratorError::usage(format!("sync stale staging cleanup: {error}"))
        })?;
    }
    Ok(())
}

fn transaction_sibling_name(
    target_name: &std::ffi::OsStr,
    kind: &str,
    sequence: u64,
) -> Result<OsString, GeneratorError> {
    let name = target_name
        .to_str()
        .ok_or_else(|| GeneratorError::usage("staging target name is not UTF-8"))?;
    Ok(OsString::from(format!(
        ".{name}.velnor-{kind}-{}-{sequence}",
        std::process::id()
    )))
}

fn validate_staging_tree_fd(root: &impl AsFd, display: &Path) -> Result<(), GeneratorError> {
    let mut directory = Dir::read_from(root).map_err(|error| {
        GeneratorError::usage(format!("inspect staging tree {}: {error}", display.display()))
    })?;
    loop {
        let Some(entry) = directory.next() else {
            break;
        };
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("list staging tree: {error}")))?;
        let directory_fd = directory
            .fd()
            .map_err(|error| GeneratorError::usage(format!("inspect staging tree: {error}")))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let path = display.join(String::from_utf8_lossy(name.to_bytes()).as_ref());
        let metadata = statat(directory_fd, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| GeneratorError::usage(format!("inspect staging entry {}: {error}", path.display())))?;
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        if file_type == FileType::Symlink {
            return Err(GeneratorError::usage(format!(
                "staging tree contains a symlink: {}",
                path.display()
            )));
        }
        if file_type == FileType::Directory {
            let child = openat(
                directory_fd,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| GeneratorError::usage(format!("inspect staging directory {}: {error}", path.display())))?;
            validate_staging_tree_fd(&child, &path)?;
        } else if file_type != FileType::RegularFile {
            return Err(GeneratorError::usage(format!(
                "staging tree contains a special file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn copy_staging_tree_fd(
    source: &impl AsFd,
    destination: &impl AsFd,
    source_display: &Path,
    destination_display: &Path,
) -> Result<(), GeneratorError> {
    validate_staging_tree_fd(source, source_display)?;
    let mut source_directory = Dir::read_from(source)
        .map_err(|error| GeneratorError::usage(format!("list staging source: {error}")))?;
    loop {
        let Some(entry) = source_directory.next() else {
            break;
        };
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("list staging source: {error}")))?;
        let source_fd = source_directory
            .fd()
            .map_err(|error| GeneratorError::usage(format!("inspect staging source: {error}")))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let source_path = source_display.join(String::from_utf8_lossy(name.to_bytes()).as_ref());
        let destination_path =
            destination_display.join(String::from_utf8_lossy(name.to_bytes()).as_ref());
        let metadata = statat(source_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            GeneratorError::usage(format!("inspect staging source {}: {error}", source_path.display()))
        })?;
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        if file_type == FileType::Directory {
            mkdirat(destination, name, Mode::from_raw_mode(0o755)).map_err(|error| {
                GeneratorError::usage(format!("copy staging directory {}: {error}", destination_path.display()))
            })?;
            let source_child = openat(
                source_fd,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| GeneratorError::usage(format!("open staging source {}: {error}", source_path.display())))?;
            let destination_child = openat(
                destination,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| GeneratorError::usage(format!("open staging copy {}: {error}", destination_path.display())))?;
            copy_staging_tree_fd(
                &source_child,
                &destination_child,
                &source_path,
                &destination_path,
            )?;
        } else if file_type == FileType::RegularFile {
            let mut source_file = std::fs::File::from(
                openat(
                    source_fd,
                    name,
                    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| GeneratorError::usage(format!("open staging source {}: {error}", source_path.display())))?,
            );
            let mut destination_file = std::fs::File::from(
                openat(
                    destination,
                    name,
                    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(|error| GeneratorError::usage(format!("create staging copy {}: {error}", destination_path.display())))?,
            );
            let opened = fstat(&source_file)
                .map_err(|error| GeneratorError::usage(format!("inspect staging source {}: {error}", source_path.display())))?;
            if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
                return Err(GeneratorError::usage(format!(
                    "staging tree contains a non-regular file: {}",
                    source_path.display()
                )));
            }
            std::io::copy(&mut source_file, &mut destination_file).map_err(|error| {
                GeneratorError::usage(format!("copy staging file {}: {error}", destination_path.display()))
            })?;
            fchmod(&destination_file, Mode::from_raw_mode(opened.st_mode)).map_err(|error| {
                GeneratorError::usage(format!("preserve staging file mode {}: {error}", destination_path.display()))
            })?;
        } else {
            return Err(GeneratorError::usage(format!(
                "staging tree contains a symlink or special file: {}",
                source_path.display()
            )));
        }
    }
    Ok(())
}

fn remove_tree_at(
    parent: &impl AsFd,
    name: impl rustix::path::Arg + Copy,
    display: &Path,
) -> Result<(), GeneratorError> {
    let metadata = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(GeneratorError::usage(format!(
                "inspect temporary staging path {}: {error}",
                display.display()
            )));
        }
    };
    let file_type = FileType::from_raw_mode(metadata.st_mode);
    if file_type != FileType::Directory {
        unlinkat(parent, name, AtFlags::empty()).map_err(|error| {
            GeneratorError::usage(format!("remove temporary staging file {}: {error}", display.display()))
        })?;
        return Ok(());
    }
    let directory = openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| GeneratorError::usage(format!("open temporary staging directory {}: {error}", display.display())))?;
    let mut entries = Dir::read_from(&directory)
        .map_err(|error| GeneratorError::usage(format!("list temporary staging directory {}: {error}", display.display())))?;
    loop {
        let Some(entry) = entries.next() else {
            break;
        };
        let entry = entry.map_err(|error| {
            GeneratorError::usage(format!("list temporary staging directory {}: {error}", display.display()))
        })?;
        let directory_fd = entries
            .fd()
            .map_err(|error| GeneratorError::usage(format!("inspect temporary staging directory: {error}")))?;
        let child_name = entry.file_name();
        if matches!(child_name.to_bytes(), b"." | b"..") {
            continue;
        }
        let child_display = display.join(String::from_utf8_lossy(child_name.to_bytes()).as_ref());
        remove_tree_at(&directory_fd, child_name, &child_display)?;
    }
    unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|error| {
        GeneratorError::usage(format!("remove temporary staging directory {}: {error}", display.display()))
    })
}

fn open_real_directory(path: &Path, context: &str) -> Result<OwnedFd, GeneratorError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::Prefix(_)
        )
    }) {
        return Err(GeneratorError::usage(format!(
            "{context} path contains a non-normal component"
        )));
    }
    let Some(name) = path.file_name() else {
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| GeneratorError::io("resolve directory path", path, &error))?;
        return open_canonical_directory(&canonical, context, flags);
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Resolve platform aliases such as macOS `/tmp`, then traverse the
    // physical parent by descriptor and refuse a symlink at the final dir.
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| GeneratorError::io("resolve directory parent", parent, &error))?;
    let parent = open_canonical_directory(&parent, context, flags)?;
    openat(&parent, name, flags, Mode::empty()).map_err(|error| {
        GeneratorError::usage(format!(
            "{context} must be a real directory without a final symlink: {error}"
        ))
    })
}

fn open_canonical_directory(
    path: &Path,
    context: &str,
    flags: OFlags,
) -> Result<OwnedFd, GeneratorError> {
    let mut directory = open(Path::new("/"), flags, Mode::empty()).map_err(|error| {
        GeneratorError::usage(format!("open {context} path root without following links: {error}"))
    })?;
    for component in path.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => name,
            std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(GeneratorError::usage(format!(
                    "{context} canonical parent has a non-normal component"
                )));
            }
        };
        directory = openat(&directory, name, flags, Mode::empty()).map_err(|error| {
            GeneratorError::usage(format!(
                "{context} canonical parent changed or contains a symlink: {error}"
            ))
        })?;
    }
    Ok(directory)
}

/// Arm the verification sentinel with descriptor-relative, no-follow file
/// operations. Existing regular single-link sentinels are replaced safely;
/// a symlink, hard link, or special file is rejected without opening it.
fn write_sentinel_no_follow(incoming: &Path) -> Result<(), GeneratorError> {
    let directory = open_real_directory(incoming, "verification input")?;
    match statat(&directory, SENTINEL_FILE, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata)
            if FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile
                && metadata.st_nlink == 1 =>
        {
            unlinkat(&directory, SENTINEL_FILE, AtFlags::empty()).map_err(|error| {
                GeneratorError::usage(format!("remove prior verification sentinel: {error}"))
            })?;
        }
        Ok(_) => {
            return Err(GeneratorError::usage(
                "verification sentinel path is not a regular single-link file",
            ));
        }
        Err(Errno::NOENT) => {}
        Err(error) => {
            return Err(GeneratorError::usage(format!(
                "inspect verification sentinel without following links: {error}"
            )));
        }
    }
    let file = openat(
        &directory,
        SENTINEL_FILE,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| {
        GeneratorError::usage(format!(
            "create verification sentinel without following links: {error}"
        ))
    })?;
    let metadata = fstat(&file)
        .map_err(|error| GeneratorError::usage(format!("inspect created verification sentinel: {error}")))?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_nlink != 1
    {
        let _ = unlinkat(&directory, SENTINEL_FILE, AtFlags::empty());
        return Err(GeneratorError::usage(
            "created verification sentinel is not a regular single-link file",
        ));
    }
    Ok(())
}

fn sentinel_is_armed(incoming: &Path) -> Result<bool, GeneratorError> {
    let directory = open_real_directory(incoming, "verification input")?;
    match openat(
        &directory,
        SENTINEL_FILE,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(file) => {
            let metadata = fstat(&file).map_err(|error| {
                GeneratorError::usage(format!("inspect verification sentinel without following links: {error}"))
            })?;
            if FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile
                && metadata.st_nlink == 1
                && metadata.st_size == 0
            {
                Ok(true)
            } else {
                Err(GeneratorError::usage(
                    "verification sentinel is not a regular empty single-link file",
                ))
            }
        }
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(GeneratorError::usage(format!(
            "inspect verification sentinel without following links: {error}"
        ))),
    }
}

/// Whether a staged package version is safe for a pool filename: the exact
/// `dpkg` filename charset the oracle enforces.
pub(crate) fn valid_pool_version(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b':' | b'~' | b'-')
        })
}

/// The two suites. One final implementation serves both; no compat wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Suite {
    Stable,
    Preview,
}

/// The only acceptable results from an authenticated-feed HTTP probe.
/// Network failures and every HTTP status except success or 404 are errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HttpProbeResult {
    Present,
    NotFound,
}

impl HttpProbeResult {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value.trim() {
            "present" => Ok(Self::Present),
            "not-found" => Ok(Self::NotFound),
            _ => Err(GeneratorError::usage(
                "APT HTTP probe result must be `present` or `not-found`",
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::NotFound => "not-found",
        }
    }
}

impl Suite {
    /// Parse the locked suite pair, failing closed on anything else.
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            STABLE_SUITE => Ok(Self::Stable),
            PREVIEW_SUITE => Ok(Self::Preview),
            _ => Err(GeneratorError::usage(format!(
                "suite must be `{STABLE_SUITE}` or `{PREVIEW_SUITE}`, found `{value}`"
            ))),
        }
    }

    /// The suite identity used in paths, records, and metadata.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stable => STABLE_SUITE,
            Self::Preview => PREVIEW_SUITE,
        }
    }

    /// The `last-publish` file this suite guards deployment with.
    pub(crate) fn last_publish_file(self) -> &'static str {
        match self {
            Self::Stable => "last-publish",
            Self::Preview => "last-publish-preview",
        }
    }

    /// The publication record this suite emits.
    pub(crate) fn publication_record_file(self) -> &'static str {
        match self {
            Self::Stable => "publication-record.json",
            Self::Preview => "publication-record-preview.json",
        }
    }

    /// The channel-state file this suite emits.
    pub(crate) fn channel_state_file(self) -> &'static str {
        match self {
            Self::Stable => "package-state.json",
            Self::Preview => "package-state-preview.json",
        }
    }
}

/// A stable tag `vX.Y.Z` split into its tag and bare version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StableTag {
    /// The full tag, `vX.Y.Z`.
    pub(crate) tag: String,
    /// The bare version, `X.Y.Z`.
    pub(crate) version: String,
}

/// Parse a stable tag. The `v` prefix is mandatory; the triple is numeric.
pub(crate) fn parse_stable_tag(value: &str) -> Result<StableTag, GeneratorError> {
    let version = value.strip_prefix('v').ok_or_else(|| {
        GeneratorError::usage(format!(
            "stable version must be a vX.Y.Z tag, found `{value}`"
        ))
    })?;
    if !is_bare_version(version) || !canonical_bare_version(version) {
        return Err(GeneratorError::usage(format!(
            "stable version must be a vX.Y.Z tag, found `{value}`"
        )));
    }
    Ok(StableTag {
        tag: value.to_owned(),
        version: version.to_owned(),
    })
}

/// Whether a value is a bare `X.Y.Z` numeric triple.
fn is_bare_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_stable_tag(value: &str) -> bool {
    parse_stable_tag(value).is_ok()
}

/// A preview version `X.Y.Z~preview.N+<7hex>` split into its parts. The `v`
/// prefix is not part of this grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewVersion {
    /// The full version, `X.Y.Z~preview.N+<7hex>`.
    pub(crate) version: String,
    /// The base `X.Y.Z` the packaged identity must carry.
    pub(crate) base: String,
    /// The rolling sequence number.
    pub(crate) seq: String,
    /// The 7-hex source-commit prefix the version pins.
    pub(crate) sha: String,
}

/// A preview GitHub release after its manifest and asset bytes were verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewSourceRelease {
    /// The immutable Git tag, `preview-X.Y.Z.preview.N+<7-hex>`.
    pub(crate) tag: String,
    /// The exact Debian package version carried by the release.
    pub(crate) version: String,
    /// The full 40-hex source commit declared by the signed release manifest.
    pub(crate) source_sha: String,
    /// The exact manifest-pinned asset SHA-256 for each supported architecture.
    pub(crate) assets: BTreeMap<String, PreviewSourceAsset>,
}

/// One exact asset row from a validated preview release manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewSourceAsset {
    /// The exact GitHub release asset name.
    pub(crate) name: String,
    /// The pinned lowercase SHA-256 of the deb bytes.
    pub(crate) sha256: String,
}

/// Parse the source-owned release manifest used by immutable preview source
/// discovery. Asset names, release tag, ref, version and commit must all agree
/// before this row can be offered to the highest-version selector.
pub(crate) fn parse_preview_source_release_manifest(
    document: &serde_json::Value,
    manifest_schema: &str,
    source_repository: &str,
    source_ref: &str,
    tag: &str,
    package: &str,
) -> Result<PreviewSourceRelease, GeneratorError> {
    if manifest_schema.is_empty()
        || !valid_repository_slug(source_repository)
        || !valid_preview_source_ref(source_ref)
        || !valid_package_name(package)
    {
        return Err(GeneratorError::usage(
            "preview release manifest expectations are malformed",
        ));
    }
    if field(document, "schema")? != manifest_schema
        || field(document, "source_repository")? != source_repository
        || field(document, "source_ref")? != source_ref
    {
        return Err(GeneratorError::usage(
            "preview release manifest schema, repository or source ref mismatch",
        ));
    }
    let version = field(document, "version")?.to_owned();
    let parsed = parse_preview_version(&version)?;
    let source_sha = field(document, "source_commit")?.to_owned();
    if !valid_commit(&source_sha) || parsed.sha != source_sha[..7] {
        return Err(GeneratorError::usage(
            "preview release manifest source commit does not match its version suffix",
        ));
    }
    let expected_tag = preview_source_tag(&version)?;
    if tag != expected_tag || field(document, "release_tag")? != expected_tag {
        return Err(GeneratorError::usage(
            "preview release tag does not match the manifest version",
        ));
    }
    let assets = document
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .filter(|assets| assets.len() == REQUIRED_ARCHES.len())
        .ok_or_else(|| {
            GeneratorError::usage("preview release manifest must list exactly two assets")
        })?;
    let dotted = dotted_asset_version(&version);
    let mut parsed_assets = BTreeMap::new();
    for asset in assets {
        let name = field(asset, "name")?;
        let digest = field(asset, "sha256")?;
        if !valid_digest(digest) {
            return Err(GeneratorError::usage(
                "preview release manifest asset SHA-256 is malformed",
            ));
        }
        let arch = REQUIRED_ARCHES
            .iter()
            .find(|arch| {
                name == format!("{package}-preview-{dotted}-{}.deb", arch)
            })
            .copied()
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "preview release manifest has an unexpected asset name: {name}"
                ))
            })?;
        if parsed_assets
            .insert(
                arch.to_owned(),
                PreviewSourceAsset {
                    name: name.to_owned(),
                    sha256: digest.to_owned(),
                },
            )
            .is_some()
        {
            return Err(GeneratorError::usage(format!(
                "preview release manifest repeats the {arch} asset"
            )));
        }
    }
    if parsed_assets.len() != REQUIRED_ARCHES.len()
        || REQUIRED_ARCHES
            .iter()
            .any(|arch| !parsed_assets.contains_key(*arch))
    {
        return Err(GeneratorError::usage(
            "preview release manifest must pin one asset for each supported architecture",
        ));
    }
    Ok(PreviewSourceRelease {
        tag: tag.to_owned(),
        version,
        source_sha,
        assets: parsed_assets,
    })
}

/// Parse a preview version, failing closed on any grammar violation.
pub(crate) fn parse_preview_version(value: &str) -> Result<PreviewVersion, GeneratorError> {
    let error = || {
        GeneratorError::usage(format!(
            "preview version is not X.Y.Z~preview.N+<7-hex>: {value}"
        ))
    };
    let (base, rest) = value.split_once("~preview.").ok_or_else(error)?;
    if !is_bare_version(base) || !canonical_bare_version(base) {
        return Err(error());
    }
    let (seq, sha) = rest.split_once('+').ok_or_else(error)?;
    if seq.is_empty()
        || !seq.bytes().all(|byte| byte.is_ascii_digit())
        || !canonical_decimal(seq)
        || !is_lower_hex(sha, 7)
        || sha.len() + seq.len() + 1 != rest.len()
    {
        return Err(error());
    }
    Ok(PreviewVersion {
        version: value.to_owned(),
        base: base.to_owned(),
        seq: seq.to_owned(),
        sha: sha.to_owned(),
    })
}

/// Resolve the immutable source release tag for one validated preview
/// package version. `PREVIEW_TAG` remains the APT suite marker only.
pub(crate) fn preview_source_tag(version: &str) -> Result<String, GeneratorError> {
    let parsed = parse_preview_version(version)?;
    Ok(format!("preview-{}", dotted_asset_version(&parsed.version)))
}

fn canonical_decimal(value: &str) -> bool {
    value == "0" || (!value.is_empty() && !value.starts_with('0'))
}

fn canonical_bare_version(value: &str) -> bool {
    value.split('.').all(canonical_decimal)
}

/// The asset-name form of a version: GitHub rewrites `~` to `.` on upload, so
/// served filenames carry the dotted form while every version-contract check
/// keeps the tilde form.
pub(crate) fn dotted_asset_version(version: &str) -> String {
    version.replace('~', ".")
}

/// Compare two bare `X.Y.Z` versions numerically per component.
fn cmp_bare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = left.split('.').collect::<Vec<_>>();
    let right = right.split('.').collect::<Vec<_>>();
    if left.len() != 3
        || right.len() != 3
        || left.iter().chain(&right).any(|part| {
            part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    for (left, right) in left.into_iter().zip(right) {
        let order = cmp_decimal_digits(left, right)?;
        if order != std::cmp::Ordering::Equal {
            return Some(order);
        }
    }
    Some(std::cmp::Ordering::Equal)
}

/// Compare arbitrary-width non-negative decimal strings numerically, as dpkg
/// does for digit runs. Leading zeroes do not affect the result.
fn cmp_decimal_digits(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    if left.is_empty()
        || right.is_empty()
        || !left.bytes().all(|byte| byte.is_ascii_digit())
        || !right.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    let left = if left.is_empty() { "0" } else { left };
    let right = if right.is_empty() { "0" } else { right };
    Some(
        left.len()
            .cmp(&right.len())
            .then_with(|| left.cmp(right)),
    )
}

/// Compare two stable tags in version order.
pub(crate) fn cmp_stable_versions(
    left: &str,
    right: &str,
) -> Result<std::cmp::Ordering, GeneratorError> {
    let left = parse_stable_tag(left)?;
    let right = parse_stable_tag(right)?;
    cmp_bare_versions(&left.version, &right.version)
        .ok_or_else(|| GeneratorError::usage("stable versions are not comparable numeric triples"))
}

/// One `dpkg` non-digit character rank, ported from `order` in
/// `lib/dpkg/version.c`: end-of-string and digits rank 0, `~` ranks -1 so it
/// sorts before everything, letters rank by ASCII value, and every other
/// character ranks above the letters.
fn verrevcmp_order(byte: Option<u8>) -> i32 {
    match byte {
        None => 0,
        Some(byte) if byte.is_ascii_digit() => 0,
        Some(byte) if byte.is_ascii_alphabetic() => i32::from(byte),
        Some(b'~') => -1,
        Some(byte) => i32::from(byte) + 256,
    }
}

/// `dpkg` revision-string comparison, ported exactly from `verrevcmp` in
/// `lib/dpkg/version.c` (checked against `dpkg --compare-versions`): the
/// inputs alternate between non-digit runs compared by [`verrevcmp_order`]
/// and digit runs compared numerically (leading zeros skipped, the longer
/// run wins, then the first differing digit decides).
fn verrevcmp(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut left, mut right) = (left.as_bytes(), right.as_bytes());
    let digit = |bytes: &[u8]| bytes.first().is_some_and(u8::is_ascii_digit);
    while !left.is_empty() || !right.is_empty() {
        let mut first_diff = 0;
        while left.first().is_some_and(|byte| !byte.is_ascii_digit())
            || right.first().is_some_and(|byte| !byte.is_ascii_digit())
        {
            let order = verrevcmp_order(left.first().copied())
                .cmp(&verrevcmp_order(right.first().copied()));
            if order != Ordering::Equal {
                return order;
            }
            left = left.get(1..).unwrap_or_default();
            right = right.get(1..).unwrap_or_default();
        }
        while left.first() == Some(&b'0') {
            left = &left[1..];
        }
        while right.first() == Some(&b'0') {
            right = &right[1..];
        }
        while digit(left) && digit(right) {
            if first_diff == 0 {
                first_diff = i32::from(left[0]) - i32::from(right[0]);
            }
            left = &left[1..];
            right = &right[1..];
        }
        if digit(left) {
            return Ordering::Greater;
        }
        if digit(right) {
            return Ordering::Less;
        }
        if first_diff != 0 {
            return first_diff.cmp(&0);
        }
    }
    Ordering::Equal
}

/// Compare two preview versions in `dpkg` order: base triple numerically,
/// then sequence numerically, then the commit suffix by `dpkg` `verrevcmp`
/// order (digit runs count numerically, so `+0000009` sorts after `+000000a`
/// just as `dpkg --compare-versions` reports). Both inputs must satisfy the
/// preview grammar; anything else fails closed instead of guessing an order.
pub(crate) fn cmp_preview_versions(
    left: &str,
    right: &str,
) -> Result<std::cmp::Ordering, GeneratorError> {
    use std::cmp::Ordering;
    let left = parse_preview_version(left)?;
    let right = parse_preview_version(right)?;
    if let Some(order) = cmp_bare_versions(&left.base, &right.base)
        && order != Ordering::Equal
    {
        return Ok(order);
    }
    let seq_order = cmp_decimal_digits(&left.seq, &right.seq).ok_or_else(|| {
        GeneratorError::usage("preview sequence is not numeric")
    })?;
    if seq_order != std::cmp::Ordering::Equal {
        return Ok(seq_order);
    }
    Ok(verrevcmp(&left.sha, &right.sha))
}

/// Select the highest valid immutable preview release. Rows must come from
/// independently authenticated release manifests; this helper enforces the
/// tag/version/commit mapping and ordering, never dates or lexical tags.
/// `after_version` is the authenticated live APT head. Equal allows a
/// same-version metadata refresh; older source releases fail closed.
pub(crate) fn select_preview_source_release(
    rows: &[PreviewSourceRelease],
    tag_prefix: &str,
    after_version: Option<&str>,
) -> Result<PreviewSourceRelease, GeneratorError> {
    if tag_prefix != "preview-" {
        return Err(GeneratorError::usage(
            "preview source tag prefix must be the immutable preview- prefix",
        ));
    }
    let after = after_version.map(parse_preview_version).transpose()?;
    let mut candidates = Vec::new();
    for row in rows.iter().filter(|row| row.tag.starts_with(tag_prefix)) {
        let parsed = parse_preview_version(&row.version)?;
        if preview_source_tag(&row.version)? != row.tag {
            return Err(GeneratorError::usage(format!(
                "preview release tag {} does not map exactly to Debian version {}",
                row.tag, row.version
            )));
        }
        if !valid_commit(&row.source_sha) || parsed.sha != row.source_sha[..7] {
            return Err(GeneratorError::usage(format!(
                "preview release {} source commit does not match its version suffix",
                row.tag
            )));
        }
        let asset_names = row
            .assets
            .values()
            .map(|asset| asset.name.as_str())
            .collect::<BTreeSet<_>>();
        if row.assets.len() != REQUIRED_ARCHES.len()
            || REQUIRED_ARCHES
                .iter()
                .any(|arch| !row.assets.get(*arch).is_some_and(|asset| {
                    valid_digest(&asset.sha256) && is_deb_file(&asset.name)
                }))
            || asset_names.len() != REQUIRED_ARCHES.len()
        {
            return Err(GeneratorError::usage(format!(
                "preview release {} does not carry two distinct authenticated architecture assets",
                row.tag
            )));
        }
        if candidates.iter().any(|existing: &PreviewSourceRelease| {
            existing.tag == row.tag || existing.version == row.version
        }) {
            return Err(GeneratorError::usage(format!(
                "preview source release list repeats tag or version {}",
                row.tag
            )));
        }
        for existing in &candidates {
            if cmp_preview_versions(&row.version, &existing.version)? == std::cmp::Ordering::Equal
            {
                return Err(GeneratorError::usage(format!(
                    "preview source releases {} and {} tie in Debian version order",
                    row.tag, existing.tag
                )));
            }
        }
        candidates.push(row.clone());
    }
    let mut selected = candidates
        .into_iter()
        .next()
        .ok_or_else(|| GeneratorError::usage("no valid immutable preview source releases found"))?;
    for candidate in rows.iter().filter(|row| row.tag.starts_with(tag_prefix)) {
        if cmp_preview_versions(&candidate.version, &selected.version)?
            == std::cmp::Ordering::Greater
        {
            selected = candidate.clone();
        }
    }
    if let Some(after) = after
        && cmp_preview_versions(&selected.version, &after.version)? == std::cmp::Ordering::Less
    {
        return Err(GeneratorError::usage(format!(
            "latest immutable preview {} is older than authenticated live head {}",
            selected.version, after.version
        )));
    }
    Ok(selected)
}

fn highest_version(
    suite: Suite,
    versions: impl IntoIterator<Item = String>,
) -> Result<String, GeneratorError> {
    let mut versions = versions.into_iter();
    let mut highest = versions
        .next()
        .ok_or_else(|| GeneratorError::usage("live Packages index has no package version"))?;
    for version in versions {
        let order = match suite {
            Suite::Stable => {
                cmp_stable_versions(&format!("v{version}"), &format!("v{highest}"))?
            }
            Suite::Preview => cmp_preview_versions(&version, &highest)?,
        };
        if order == std::cmp::Ordering::Equal && version != highest {
            return Err(GeneratorError::usage(format!(
                "live package versions {version} and {highest} tie in Debian version order"
            )));
        }
        if order == std::cmp::Ordering::Greater {
            highest = version;
        }
    }
    Ok(highest)
}

/// Previous-version retention: how many rollback versions each suite index
/// keeps beside the candidate. Only [`IMPLEMENTED_RETENTION`] is implemented;
/// any other count is a usage error naming the implemented policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Retention(u32);

impl Retention {
    /// Parse a configured retention count. Zero means unset and selects the
    /// default of one.
    pub(crate) fn parse(value: i64) -> Result<Self, GeneratorError> {
        match value {
            0 | 1 => Ok(Self(IMPLEMENTED_RETENTION)),
            _ => Err(GeneratorError::usage(format!(
                "apt retention is {IMPLEMENTED_RETENTION} by policy, found `{value}`"
            ))),
        }
    }

    /// The retained version count per architecture, candidate included.
    pub(crate) fn indexed_versions(self) -> usize {
        usize::try_from(self.0 + 1).unwrap_or(2)
    }

    /// The deterministic pool size for the retained set.
    pub(crate) fn pool_debs(self) -> usize {
        self.indexed_versions() * REQUIRED_ARCHES.len()
    }
}

/// The resolved typed APT contract: every capability of spec §7 in validated
/// form. Resolution applies documented defaults (both arches, origin from the
/// package, keyring from the package, identity dir from the source repository
/// name, retention one) and fails closed on any malformed present value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AptContract {
    /// The source repository slug serving coherence inputs.
    pub(crate) source_repo: String,
    /// The Debian package this feed publishes.
    pub(crate) package: String,
    /// The daemon binary the extracted-identity check hashes.
    pub(crate) binary: String,
    /// The consumer feed repository this workflow mutates.
    pub(crate) consumer_repo: String,
    /// The consumer release-manifest schema URN the config declares.
    pub(crate) manifest_schema: String,
    /// The pinned publisher signing-key fingerprint.
    pub(crate) signer: String,
    /// The environment secret holding the signing passphrase (name only).
    pub(crate) passphrase_secret: String,
    /// The environment secret holding the signing-key material (name only).
    pub(crate) signing_key_secret: String,
    /// Optional source-repository read token name for private attestations.
    pub(crate) apt_attestation_secret: Option<String>,
    /// The repository-local keyring the live fingerprint is read from.
    pub(crate) keyring: String,
    /// The `Origin`/`Label` stamped into suite metadata.
    pub(crate) origin: String,
    /// The packaged-identity directory inside the deb.
    pub(crate) identity_dir: String,
    /// The served feed base URL used for prior-pair recovery and the
    /// no-rollback deploy guard.
    pub(crate) feed_url: String,
    /// The configured default-branch ref preview attestations and feed state bind.
    pub(crate) preview_source_ref: String,
    /// The `Description` stamped into suite metadata.
    pub(crate) description: String,
    /// The validated architecture set, always exactly both arches.
    pub(crate) arches: [String; 2],
    /// The validated retention policy.
    pub(crate) retention: Retention,
}

/// The APT-specific fields consumed by [`AptContract`]. S2 config validation,
/// workflow rendering, and runtime publication project their fields here so
/// they share one contract implementation.
pub(crate) struct AptContractInput<'a> {
    pub(crate) kind: &'a str,
    pub(crate) source_repository: &'a str,
    pub(crate) package: &'a str,
    pub(crate) binary: &'a str,
    pub(crate) consumer_repository: &'a str,
    pub(crate) manifest_schema: &'a str,
    pub(crate) signer_fingerprint: &'a str,
    pub(crate) passphrase_secret: &'a str,
    pub(crate) signing_key_secret: &'a str,
    pub(crate) apt_attestation_secret: &'a str,
    pub(crate) keyring_path: &'a str,
    pub(crate) apt_origin: &'a str,
    pub(crate) apt_identity_dir: &'a str,
    pub(crate) apt_feed_url: &'a str,
    pub(crate) preview_source_ref: &'a str,
    pub(crate) description: &'a str,
    pub(crate) apt_arches: &'a [String],
    pub(crate) retention: i64,
}

impl AptContract {
    /// Resolve a release spec into the typed contract. Empty `arches` selects
    /// both arches; a non-empty set must equal exactly both arches — a
    /// missing, duplicate, or foreign arch fails closed.
    /// Resolve either generator schema's explicit APT fields.
    pub(crate) fn resolve_input(spec: AptContractInput<'_>) -> Result<Self, GeneratorError> {
        if spec.kind != "apt" {
            return Err(GeneratorError::usage(format!(
                "apt contract needs kind `apt`, found `{}`",
                spec.kind
            )));
        }
        if !valid_repository_slug(spec.source_repository) {
            return Err(GeneratorError::usage(format!(
                "apt source_repository must be `owner/name`, found `{}`",
                spec.source_repository
            )));
        }
        if !valid_repository_slug(spec.consumer_repository) {
            return Err(GeneratorError::usage(format!(
                "apt consumer_repository must be `owner/name`, found `{}`",
                spec.consumer_repository
            )));
        }
        if !valid_package_name(spec.package) {
            return Err(GeneratorError::usage(format!(
                "apt package is not a safe package name: `{}`",
                spec.package
            )));
        }
        if !valid_binary_name(spec.binary) {
            return Err(GeneratorError::usage(format!(
                "apt binary is not a safe binary name: `{}`",
                spec.binary
            )));
        }
        if spec.manifest_schema.is_empty() || spec.manifest_schema.contains(char::is_whitespace) {
            return Err(GeneratorError::usage(
                "apt manifest_schema must be a non-empty schema URN without whitespace",
            ));
        }
        if !is_full_fingerprint(&normalize_fingerprint(spec.signer_fingerprint)) {
            return Err(GeneratorError::usage(
                "apt signer_fingerprint must be a full 40-hex fingerprint",
            ));
        }
        if !valid_secret_ref(spec.passphrase_secret) {
            return Err(GeneratorError::usage(
                "apt passphrase_secret must name an environment secret (uppercase identifier), never a value",
            ));
        }
        if !valid_secret_ref(spec.signing_key_secret) {
            return Err(GeneratorError::usage(
                "apt signing_key_secret must name an environment secret (uppercase identifier), never a value",
            ));
        }
        if !spec.apt_attestation_secret.is_empty()
            && !valid_secret_ref(spec.apt_attestation_secret)
        {
            return Err(GeneratorError::usage(
                "apt attestation_secret must name an environment secret (uppercase identifier), never a value",
            ));
        }
        let keyring = default_keyring(spec.package, spec.keyring_path)?;
        let origin = default_origin(spec.package, spec.apt_origin)?;
        let identity_dir = default_identity_dir(spec.source_repository, spec.apt_identity_dir)?;
        if !valid_feed_url(spec.apt_feed_url) {
            return Err(GeneratorError::usage(
                "apt feed_url must be an https URL with a host and an optional path",
            ));
        }
        if !valid_preview_source_ref(spec.preview_source_ref) {
            return Err(GeneratorError::usage(
                "apt preview_source_ref must be refs/heads/<safe branch>",
            ));
        }
        let description = default_description(spec.package, spec.description)?;
        let arches = parse_arch_set(spec.apt_arches)?;
        let retention = Retention::parse(spec.retention)?;
        Ok(Self {
            source_repo: spec.source_repository.to_owned(),
            package: spec.package.to_owned(),
            binary: spec.binary.to_owned(),
            consumer_repo: spec.consumer_repository.to_owned(),
            manifest_schema: spec.manifest_schema.to_owned(),
            signer: normalize_fingerprint(spec.signer_fingerprint),
            passphrase_secret: spec.passphrase_secret.to_owned(),
            signing_key_secret: spec.signing_key_secret.to_owned(),
            apt_attestation_secret: (!spec.apt_attestation_secret.is_empty())
                .then(|| spec.apt_attestation_secret.to_owned()),
            keyring,
            origin,
            identity_dir,
            feed_url: spec.apt_feed_url.to_owned(),
            preview_source_ref: spec.preview_source_ref.to_owned(),
            description,
            arches,
            retention,
        })
    }
}

/// The validated keyring path: explicit, or `<package>.gpg` by default.
fn default_keyring(package: &str, keyring_path: &str) -> Result<String, GeneratorError> {
    let keyring = if keyring_path.is_empty() {
        format!("{package}.gpg")
    } else {
        keyring_path.to_owned()
    };
    if !valid_keyring_path(&keyring) {
        return Err(GeneratorError::usage(format!(
            "apt keyring_path must be a relative path without traversal, found `{keyring}`"
        )));
    }
    Ok(keyring)
}

/// The validated origin: explicit, or the package name by default.
fn default_origin(package: &str, apt_origin: &str) -> Result<String, GeneratorError> {
    let origin = if apt_origin.is_empty() {
        package.to_owned()
    } else {
        apt_origin.to_owned()
    };
    if !valid_origin(&origin) {
        return Err(GeneratorError::usage(format!(
            "apt origin is not a safe Origin line: `{origin}`"
        )));
    }
    Ok(origin)
}

/// The validated identity directory: explicit, or the source repository
/// name by default.
fn default_identity_dir(
    source_repository: &str,
    apt_identity_dir: &str,
) -> Result<String, GeneratorError> {
    let identity_dir = if apt_identity_dir.is_empty() {
        source_repository
            .split('/')
            .next_back()
            .unwrap_or_default()
            .to_owned()
    } else {
        apt_identity_dir.to_owned()
    };
    if !valid_identity_dir(&identity_dir) {
        return Err(GeneratorError::usage(format!(
            "apt identity_dir is not a safe directory name: `{identity_dir}`"
        )));
    }
    Ok(identity_dir)
}

/// The validated feed description: explicit, or derived from the package.
fn default_description(package: &str, description: &str) -> Result<String, GeneratorError> {
    let description = if description.is_empty() {
        format!("apt repository for {package}")
    } else {
        description.to_owned()
    };
    if !valid_description(&description) {
        return Err(GeneratorError::usage(
            "apt description must be one printable line without control characters",
        ));
    }
    Ok(description)
}

/// Parse the typed architecture set: empty selects both arches, and any
/// explicit set must equal exactly both arches.
fn parse_arch_set(values: &[String]) -> Result<[String; 2], GeneratorError> {
    if values.is_empty() {
        return Ok([REQUIRED_ARCHES[0].to_owned(), REQUIRED_ARCHES[1].to_owned()]);
    }
    let mut seen = BTreeSet::new();
    for value in values {
        if !REQUIRED_ARCHES.contains(&value.as_str()) {
            return Err(GeneratorError::usage(format!(
                "apt arches must be exactly `amd64` and `arm64`, found `{value}`"
            )));
        }
        if !seen.insert(value.as_str()) {
            return Err(GeneratorError::usage(format!(
                "apt arches names `{value}` twice"
            )));
        }
    }
    if seen.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "apt arches must be exactly `amd64` and `arm64`",
        ));
    }
    Ok([REQUIRED_ARCHES[0].to_owned(), REQUIRED_ARCHES[1].to_owned()])
}

/// The hex SHA-256 of bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 15)] as char);
    }
    out
}

/// The hex SHA-256 of a file.
pub(crate) fn sha256_file(path: &Path) -> Result<String, GeneratorError> {
    let bytes = std::fs::read(path).map_err(|error| GeneratorError::io("read", path, &error))?;
    Ok(sha256_hex(&bytes))
}

/// Read a JSON document, failing closed on IO or syntax errors.
fn read_json(path: &Path) -> Result<serde_json::Value, GeneratorError> {
    let bytes = std::fs::read(path).map_err(|error| GeneratorError::io("read", path, &error))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        GeneratorError::usage(format!("{} is not valid JSON: {error}", path.display()))
    })
}

/// Read a JSON string field, failing closed when it is null, absent, or not a
/// string — the `jq -er` contract.
fn field<'a>(document: &'a serde_json::Value, name: &str) -> Result<&'a str, GeneratorError> {
    document
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            GeneratorError::usage(format!("JSON field `{name}` is missing or not a string"))
        })
}

/// Read a positive-integer JSON field, failing closed on any other shape.
fn positive_field(document: &serde_json::Value, name: &str) -> Result<u64, GeneratorError> {
    document
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            GeneratorError::usage(format!("JSON field `{name}` is not a positive integer"))
        })
}

/// The bare digest a detached sidecar carries: the first whitespace-separated
/// field, which must be 64 lowercase hex.
fn sidecar_digest(path: &Path) -> Result<String, GeneratorError> {
    let bytes = std::fs::read(path).map_err(|error| GeneratorError::io("read", path, &error))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| GeneratorError::usage(format!("{} is not UTF-8", path.display())))?;
    let digest = text
        .split_whitespace()
        .next()
        .ok_or_else(|| GeneratorError::usage(format!("{} carries no digest", path.display())))?;
    if !valid_digest(digest) {
        return Err(GeneratorError::usage(format!(
            "{} does not carry a 64-hex digest",
            path.display()
        )));
    }
    Ok(digest.to_owned())
}

/// Require a coherence input to exist.
fn require_file(path: &Path) -> Result<(), GeneratorError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "required file missing: {}",
            path.display()
        )))
    }
}

/// Whether a directory entry is a `.deb` file. The match is deliberately
/// case-sensitive, like the oracle's glob: a `.DEB` file is not a
/// coherence input.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_deb_file(name: &str) -> bool {
    name.ends_with(".deb")
}

/// List the entries of a directory by file name.
fn dir_names(dir: &Path) -> Result<Vec<String>, GeneratorError> {
    let mut names = Vec::new();
    let entries =
        std::fs::read_dir(dir).map_err(|error| GeneratorError::io("list", dir, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| GeneratorError::io("list", dir, &error))?;
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Run a fixed tool with fixed arguments: no shell, no config-derived program.
/// `stdin_bytes` feeds tools that take secrets on standard input so secrets
/// never appear in an argument vector. Diagnostics name the program and its
/// failure only; argument values stay out of error text.
fn run_fixed(
    program: &str,
    args: &[String],
    stdin_bytes: Option<&[u8]>,
    path_overlay: Option<&Path>,
) -> Result<Vec<u8>, GeneratorError> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = path_overlay {
        let overlay = dir.as_os_str();
        let path = std::env::var_os("PATH").map_or_else(
            || overlay.to_owned(),
            |existing| {
                let mut joined = overlay.to_owned();
                joined.push(":");
                joined.push(existing);
                joined
            },
        );
        command.env("PATH", path);
    }
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| GeneratorError::usage(format!("{program} is not installed or cannot run")))?;
    if let Some(bytes) = stdin_bytes {
        child
            .stdin
            .as_mut()
            .ok_or_else(|| GeneratorError::usage(format!("{program} takes no standard input")))?
            .write_all(bytes)
            .map_err(|_| GeneratorError::usage(format!("{program} refused standard input")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| GeneratorError::usage(format!("{program} did not finish")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "{program} failed with status {}",
            output.status
        )));
    }
    Ok(output.stdout)
}

/// Whether a fixed tool resolves on `PATH` (honoring the test overlay).
fn tool_present(program: &str, path_overlay: Option<&Path>) -> bool {
    let mut dirs = Vec::new();
    if let Some(dir) = path_overlay {
        dirs.push(dir.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.iter().any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
    })
}

/// The public git URL derived from a validated source slug. The host is a
/// code constant; configuration contributes only the validated slug.
pub(crate) fn source_git_url(source_repo: &str) -> Result<String, GeneratorError> {
    if !valid_repository_slug(source_repo) {
        return Err(GeneratorError::usage(
            "source repository must be `owner/name`",
        ));
    }
    Ok(format!("https://github.com/{source_repo}.git"))
}

/// The fixed `git ls-remote` argument vector resolving a tag. The peeled
/// `^{}` ref is tried first so annotated tags resolve to their commit.
pub(crate) fn resolve_commit_argv(source_git: &str, tag: &str, peeled: bool) -> Vec<String> {
    let reference = if peeled {
        format!("refs/tags/{tag}^{{}}")
    } else {
        format!("refs/tags/{tag}")
    };
    vec!["ls-remote".to_owned(), source_git.to_owned(), reference]
}

/// Independently resolve a stable tag to its commit through the public git
/// remote. Never trusts the record; previews refuse — the caller supplies
/// their commit.
pub(crate) fn run_resolve_commit(
    source_repo: &str,
    tag: &str,
    path_overlay: Option<&Path>,
) -> Result<String, GeneratorError> {
    parse_stable_tag(tag)?;
    let source_git = source_git_url(source_repo)?;
    for peeled in [true, false] {
        let argv = resolve_commit_argv(&source_git, tag, peeled);
        let Ok(stdout) = run_fixed("git", &argv, None, path_overlay) else {
            continue;
        };
        let text = std::str::from_utf8(&stdout)
            .map_err(|_| GeneratorError::usage("git ls-remote returned non-UTF-8 output"))?;
        let expected_ref = if peeled {
            format!("refs/tags/{tag}^{{}}")
        } else {
            format!("refs/tags/{tag}")
        };
        let matches = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let (Some(commit), Some(reference), None) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    return None;
                };
                (reference == expected_ref).then_some(commit)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [commit] if valid_commit(commit) => return Ok((*commit).to_owned()),
            [_] => {
                return Err(GeneratorError::usage(format!(
                    "could not resolve {tag} to one valid commit on {source_repo}"
                )));
            }
            [] => {}
            _ => {
                return Err(GeneratorError::usage(format!(
                    "could not resolve {tag} to one exact ref on {source_repo}"
                )));
            }
        }
    }
    Err(GeneratorError::usage(format!(
        "could not resolve {tag} to a commit on {source_repo}"
    )))
}

/// Resolve one configured source branch to its exact current commit. Only an
/// exact `refs/heads/...` row is accepted; `ls-remote` pattern matches for a
/// sibling ref cannot stand in for the configured branch.
pub(crate) fn run_resolve_source_ref(
    source_repo: &str,
    source_ref: &str,
    path_overlay: Option<&Path>,
) -> Result<String, GeneratorError> {
    if !valid_preview_source_ref(source_ref) {
        return Err(GeneratorError::usage(
            "source ref must be refs/heads/<safe branch>",
        ));
    }
    let source_git = source_git_url(source_repo)?;
    let output = run_fixed(
        "git",
        &[
            "ls-remote".to_owned(),
            "--exit-code".to_owned(),
            "--refs".to_owned(),
            source_git,
            source_ref.to_owned(),
        ],
        None,
        path_overlay,
    )?;
    let text = std::str::from_utf8(&output)
        .map_err(|_| GeneratorError::usage("git ls-remote returned non-UTF-8 output"))?;
    let mut resolved = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(commit), Some(reference), None) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if reference != source_ref {
            continue;
        }
        if !valid_commit(commit) || resolved.replace(commit.to_owned()).is_some() {
            return Err(GeneratorError::usage(format!(
                "could not resolve {source_ref} to one exact commit on {source_repo}"
            )));
        }
    }
    resolved.ok_or_else(|| {
        GeneratorError::usage(format!(
            "could not resolve {source_ref} to a commit on {source_repo}"
        ))
    })
}

/// The exact download allowlist for a suite: code constants parameterized
/// only by package, version, and arch. Configuration can never add a pattern.
pub(crate) fn download_patterns(
    suite: Suite,
    package: &str,
    version: &str,
) -> Result<Vec<String>, GeneratorError> {
    if !valid_package_name(package) {
        return Err(GeneratorError::usage("download needs a safe package name"));
    }
    let mut patterns = Vec::new();
    match suite {
        Suite::Stable => {
            let tag = parse_stable_tag(version)?;
            patterns.push(RECORD_FILE.to_owned());
            patterns.push(RECORD_SIDECAR.to_owned());
            patterns.push(MANIFEST_FILE.to_owned());
            patterns.push(MANIFEST_SIDECAR.to_owned());
            for arch in REQUIRED_ARCHES {
                let deb = format!("{package}-{}-{arch}.deb", tag.version);
                patterns.push(format!("{deb}.sha256"));
                patterns.push(deb);
            }
        }
        Suite::Preview => {
            let parsed = parse_preview_version(version)?;
            let dotted = dotted_asset_version(&parsed.version);
            patterns.push(PREVIEW_MANIFEST_FILE.to_owned());
            patterns.push(SHA256SUMS_FILE.to_owned());
            for arch in REQUIRED_ARCHES {
                let deb = format!("{package}-preview-{dotted}-{arch}.deb");
                patterns.push(format!("{deb}.sha256"));
                patterns.push(deb);
            }
        }
    }
    patterns.sort();
    Ok(patterns)
}

/// The fixed `gh release download` argument vector for coherence inputs only.
pub(crate) fn gh_download_argv(
    tag: &str,
    source_repo: &str,
    dir: &Path,
    patterns: &[String],
) -> Result<Vec<String>, GeneratorError> {
    if !valid_repository_slug(source_repo) {
        return Err(GeneratorError::usage(
            "download needs an `owner/name` source repository",
        ));
    }
    let Some(dir) = dir.to_str() else {
        return Err(GeneratorError::usage("download directory is not UTF-8"));
    };
    let mut argv = vec![
        "release".to_owned(),
        "download".to_owned(),
        tag.to_owned(),
        "--repo".to_owned(),
        source_repo.to_owned(),
        "--dir".to_owned(),
        dir.to_owned(),
    ];
    for pattern in patterns {
        argv.push("--pattern".to_owned());
        argv.push(pattern.clone());
    }
    Ok(argv)
}

/// Fetch exactly the coherence inputs for a suite into `dir`.
pub(crate) fn run_fetch(
    suite: Suite,
    source_repo: &str,
    package: &str,
    version: &str,
    dir: &Path,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let tag = match suite {
        Suite::Stable => parse_stable_tag(version)?.tag,
        Suite::Preview => preview_source_tag(version)?,
    };
    let patterns = download_patterns(suite, package, version)?;
    std::fs::create_dir_all(dir).map_err(|error| GeneratorError::io("create", dir, &error))?;
    let argv = gh_download_argv(&tag, source_repo, dir, &patterns)?;
    run_fixed("gh", &argv, None, path_overlay)?;
    Ok(())
}

/// The `.deb` read backend. `Auto` prefers `dpkg-deb` and falls back to
/// portable `ar` + `tar` so verification also runs where `dpkg` is absent;
/// `ArTar` forces the fallback. Both paths take fixed arguments only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DebBackend {
    Auto,
    /// Forces the portable reader so tests pin the fallback on machines
    /// where `dpkg-deb` exists.
    #[cfg_attr(not(test), allow(dead_code))]
    ArTar,
}

/// Read one control field from a `.deb`. Only the identity fields the
/// coherence checks need are readable; anything else fails closed.
pub(crate) fn deb_control_field(
    deb: &Path,
    field_name: &str,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<String, GeneratorError> {
    if !matches!(field_name, "Package" | "Version" | "Architecture") {
        return Err(GeneratorError::usage(format!(
            "deb control field is not readable: {field_name}"
        )));
    }
    let Some(deb_name) = deb.to_str() else {
        return Err(GeneratorError::usage("deb path is not UTF-8"));
    };
    if backend == DebBackend::Auto && tool_present("dpkg-deb", path_overlay) {
        let stdout = run_fixed(
            "dpkg-deb",
            &["-f".to_owned(), deb_name.to_owned(), field_name.to_owned()],
            None,
            path_overlay,
        )?;
        return Ok(String::from_utf8_lossy(&stdout).trim().to_owned());
    }
    let members = run_fixed(
        "ar",
        &["t".to_owned(), deb_name.to_owned()],
        None,
        path_overlay,
    )?;
    let control = String::from_utf8_lossy(&members)
        .lines()
        .find(|line| line.starts_with("control.tar"))
        .ok_or_else(|| {
            GeneratorError::usage(format!("deb {} has no control.tar member", deb.display()))
        })?
        .to_owned();
    let payload = run_fixed(
        "ar",
        &["p".to_owned(), deb_name.to_owned(), control],
        None,
        path_overlay,
    )?;
    // Full extraction into a scratch directory, exactly like the oracle:
    // control members name their file `control` with or without a `./`
    // prefix depending on the producer, and name matching would guess.
    let scratch = scratch_dir("deb-control")?;
    let result = run_tar_stdin(
        &["-x", "-C", scratch.to_str().unwrap_or("."), "-f", "-"],
        &payload,
        path_overlay,
    )
    .and_then(|()| {
        let prefix = format!("{field_name}:");
        std::fs::read_to_string(scratch.join("control"))
            .map_err(|error| GeneratorError::io("read", &scratch.join("control"), &error))
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::trim))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        GeneratorError::usage(format!(
                            "deb {} has no {field_name} control field",
                            deb.display()
                        ))
                    })
            })
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// A unique scratch directory under the system temp dir.
fn scratch_dir(kind: &str) -> Result<PathBuf, GeneratorError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "apt-feed-{kind}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).map_err(|error| GeneratorError::io("create", &dir, &error))?;
    Ok(dir)
}

/// Decompression flag for `tar` from the archive's magic bytes.
///
/// GNU tar — unlike bsdtar — refuses a compressed payload without an
/// explicit flag (`Archive is compressed. Use -z option`), and `.deb`
/// members arrive gzip/xz/zstd-compressed depending on the producer, so
/// the shared extractor sniffs the payload instead of trusting the
/// member name. Returns `None` for an uncompressed tar stream.
fn tar_decompress_flag(payload: &[u8]) -> Option<&'static str> {
    if payload.starts_with(&[0x1f, 0x8b]) {
        Some("-z") // gzip
    } else if payload.starts_with(&[0x42, 0x5a]) {
        Some("-j") // bzip2
    } else if payload.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
        Some("-J") // xz
    } else if payload.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some("--zstd")
    } else {
        None // uncompressed tar
    }
}

/// Run `tar` with fixed arguments and a piped archive payload.
fn run_tar_stdin(
    args: &[&str],
    payload: &[u8],
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let mut extract = Command::new("tar");
    let mut full_args: Vec<&str> = Vec::with_capacity(args.len() + 1);
    if let Some((first, rest)) = args.split_first() {
        full_args.push(*first);
        if let Some(flag) = tar_decompress_flag(payload) {
            full_args.push(flag);
        }
        full_args.extend(rest.iter().copied());
    }
    extract.args(&full_args);
    if let Some(dir) = path_overlay {
        let overlay = dir.as_os_str();
        let path = std::env::var_os("PATH").map_or_else(
            || overlay.to_owned(),
            |existing| {
                let mut joined = overlay.to_owned();
                joined.push(":");
                joined.push(existing);
                joined
            },
        );
        extract.env("PATH", path);
    }
    extract.stdin(Stdio::piped());
    extract.stdout(Stdio::piped());
    extract.stderr(Stdio::piped());
    let mut child = extract
        .spawn()
        .map_err(|_| GeneratorError::usage("tar is not installed or cannot run"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| GeneratorError::usage("tar takes no standard input"))?
        .write_all(payload)
        .map_err(|_| GeneratorError::usage("tar refused standard input"))?;
    let output = child
        .wait_with_output()
        .map_err(|_| GeneratorError::usage("tar did not finish"))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "tar failed with status {}",
            output.status
        )));
    }
    Ok(())
}

/// Extract a `.deb` data tree into `dest`.
pub(crate) fn deb_extract_data(
    deb: &Path,
    dest: &Path,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let (Some(deb_name), Some(dest_name)) = (deb.to_str(), dest.to_str()) else {
        return Err(GeneratorError::usage("deb path is not UTF-8"));
    };
    std::fs::create_dir_all(dest).map_err(|error| GeneratorError::io("create", dest, &error))?;
    if backend == DebBackend::Auto && tool_present("dpkg-deb", path_overlay) {
        run_fixed(
            "dpkg-deb",
            &["-x".to_owned(), deb_name.to_owned(), dest_name.to_owned()],
            None,
            path_overlay,
        )?;
        return Ok(());
    }
    let members = run_fixed(
        "ar",
        &["t".to_owned(), deb_name.to_owned()],
        None,
        path_overlay,
    )?;
    let data = String::from_utf8_lossy(&members)
        .lines()
        .find(|line| line.starts_with("data.tar"))
        .ok_or_else(|| {
            GeneratorError::usage(format!("deb {} has no data.tar member", deb.display()))
        })?
        .to_owned();
    let payload = run_fixed(
        "ar",
        &["p".to_owned(), deb_name.to_owned(), data],
        None,
        path_overlay,
    )?;
    run_tar_stdin(&["-x", "-C", dest_name, "-f", "-"], &payload, path_overlay)
}

/// Inputs to suite verification. `commit` is `None` for a stable run that
/// resolves the tag commit itself; previews always carry the caller-supplied
/// commit.
pub(crate) struct VerifyInputs<'a> {
    /// The suite under verification.
    pub(crate) suite: Suite,
    /// The source repository the coherence inputs must name.
    pub(crate) source_repo: String,
    /// The package under verification.
    pub(crate) package: String,
    /// The daemon binary the extracted-identity check hashes.
    pub(crate) binary: String,
    /// The expected consumer release-manifest schema URN (preview suite).
    pub(crate) manifest_schema: String,
    /// The configured default-branch ref expected by preview manifests.
    pub(crate) preview_source_ref: String,
    /// The packaged-identity directory inside the deb.
    pub(crate) identity_dir: String,
    /// The candidate version: a `vX.Y.Z` tag for stable, the tilde version
    /// for preview.
    pub(crate) version: String,
    /// The resolved (stable) or caller-supplied (preview) 40-hex commit.
    pub(crate) commit: Option<String>,
    /// The fetched coherence inputs.
    pub(crate) incoming: &'a Path,
    /// The live signing-key fingerprint read from the keyring.
    pub(crate) signer_live: String,
    /// The pinned publisher fingerprint from the typed config.
    pub(crate) signer_pinned: String,
    /// Whether to query the live OCI registry (stable only).
    pub(crate) verify_oci: bool,
    /// The `.deb` read backend.
    pub(crate) backend: DebBackend,
    /// Test-only `PATH` overlay resolving fixed tool names.
    pub(crate) path_overlay: Option<&'a Path>,
}

/// Verify a suite's coherence inputs and arm the sentinel. Every check is
/// read-only; the sentinel write is the only effect, and it lands only after
/// every check passes — so any rejection leaves the trusted state intact.
pub(crate) fn verify_suite(inputs: &VerifyInputs<'_>) -> Result<(), GeneratorError> {
    if !valid_repository_slug(&inputs.source_repo) {
        return Err(GeneratorError::usage(
            "verify needs an `owner/name` source repository",
        ));
    }
    if !valid_package_name(&inputs.package) {
        return Err(GeneratorError::usage("verify needs a safe package name"));
    }
    if !valid_binary_name(&inputs.binary) {
        return Err(GeneratorError::usage("verify needs a safe binary name"));
    }
    if !valid_identity_dir(&inputs.identity_dir) {
        return Err(GeneratorError::usage(
            "verify needs a safe identity directory",
        ));
    }
    if !valid_preview_source_ref(&inputs.preview_source_ref) {
        return Err(GeneratorError::usage(
            "verify needs a preview source ref of refs/heads/<safe branch>",
        ));
    }
    if !is_full_fingerprint(&normalize_fingerprint(&inputs.signer_live))
        || !is_full_fingerprint(&normalize_fingerprint(&inputs.signer_pinned))
    {
        return Err(GeneratorError::usage(
            "verify needs full 40-hex live and pinned signer fingerprints",
        ));
    }
    match inputs.suite {
        Suite::Stable => verify_stable(inputs),
        Suite::Preview => verify_preview(inputs),
    }?;
    if !fingerprints_match(&inputs.signer_live, &inputs.signer_pinned) {
        return Err(GeneratorError::usage(
            "APT signer fingerprint does not match the pinned publisher key",
        ));
    }
    write_sentinel_no_follow(inputs.incoming)?;
    Ok(())
}

/// The record architectures joined in sorted order, which must read exactly
/// `amd64 arm64`: any missing, duplicate, or foreign arch fails the join.
fn record_arch_join(document: &serde_json::Value) -> Result<String, GeneratorError> {
    let architectures = document
        .get("architectures")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("record architectures are not an array"))?;
    let mut arches = Vec::new();
    for entry in architectures {
        arches.push(field(entry, "arch")?.to_owned());
    }
    arches.sort();
    Ok(arches.join(" "))
}

/// The per-arch record row for `arch`, failing closed when absent.
fn record_arch_row<'a>(
    document: &'a serde_json::Value,
    arch: &str,
) -> Result<&'a serde_json::Value, GeneratorError> {
    document
        .get("architectures")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("arch").and_then(serde_json::Value::as_str) == Some(arch))
        })
        .ok_or_else(|| GeneratorError::usage(format!("record has no {arch} architecture row")))
}

/// Verify the stable suite: the tagged release-record flow.
fn verify_stable(inputs: &VerifyInputs<'_>) -> Result<(), GeneratorError> {
    let tag = parse_stable_tag(&inputs.version)?;
    let supplied_commit = inputs.commit.as_deref().filter(|commit| !commit.is_empty());
    if supplied_commit.is_some_and(|commit| !valid_commit(commit)) {
        return Err(GeneratorError::usage(
            "resolved commit is not 40 lowercase hex characters",
        ));
    }
    let commit = run_resolve_commit(&inputs.source_repo, &tag.tag, inputs.path_overlay)?;
    if supplied_commit.is_some_and(|supplied| supplied != commit) {
        return Err(GeneratorError::usage(
            "caller-supplied commit does not match the independently resolved current tag commit",
        ));
    }
    let incoming = inputs.incoming;
    let record_path = incoming.join(RECORD_FILE);
    let record_sum_path = incoming.join(RECORD_SIDECAR);
    let manifest_path = incoming.join(MANIFEST_FILE);
    let manifest_sum_path = incoming.join(MANIFEST_SIDECAR);
    require_file(&record_path)?;
    require_file(&record_sum_path)?;
    require_file(&manifest_path)?;
    require_file(&manifest_sum_path)?;

    let deb_prefix = format!("{}-", inputs.package);
    let debs: Vec<String> = dir_names(incoming)?
        .into_iter()
        .filter(|name| name.starts_with(&deb_prefix) && is_deb_file(name))
        .collect();
    if debs.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(format!(
            "expected exactly {} debs in {}, found {} (extra/missing deb)",
            REQUIRED_ARCHES.len(),
            incoming.display(),
            debs.len()
        )));
    }

    let want_record = sidecar_digest(&record_sum_path)?;
    let have_record = sha256_file(&record_path)?;
    if want_record != have_record {
        return Err(GeneratorError::usage("record checksum mismatch"));
    }
    let want_manifest = sidecar_digest(&manifest_sum_path)?;
    let have_manifest = sha256_file(&manifest_path)?;
    if want_manifest != have_manifest {
        return Err(GeneratorError::usage("manifest checksum mismatch"));
    }

    let record = read_json(&record_path)?;
    let (record_manifest_hash, record_manifest_version) =
        verify_stable_record(&record, &tag, &commit, &inputs.source_repo)?;
    verify_stable_manifest(
        &manifest_path,
        &tag,
        &commit,
        record_manifest_hash,
        record_manifest_version,
        &have_manifest,
    )?;

    verify_record_oci(
        &record,
        &tag.version,
        &commit,
        &inputs.source_repo,
        record_manifest_hash,
    )?;
    if inputs.verify_oci {
        verify_oci_live(&record, &tag.version, &commit, inputs.path_overlay)?;
    }

    if record_arch_join(&record)? != REQUIRED_ARCHES.join(" ") {
        return Err(GeneratorError::usage(
            "record architectures are not exactly {amd64, arm64}",
        ));
    }
    for arch in REQUIRED_ARCHES {
        verify_stable_arch(
            inputs,
            &record,
            &tag.version,
            &commit,
            record_manifest_hash,
            arch,
        )?;
    }
    verify_stable_attestations(inputs, &tag, &commit)?;
    Ok(())
}

/// Verify the trusted release-source attestation for both stable deb subjects.
/// GitHub CLI verifies the signature, repository, signer workflow, source ref,
/// and predicate type; this parser binds the signed predicate and every
/// subject digest to this exact release and the downloaded package bytes.
fn verify_stable_attestations(
    inputs: &VerifyInputs<'_>,
    tag: &StableTag,
    commit: &str,
) -> Result<(), GeneratorError> {
    if !tool_present("gh", inputs.path_overlay) {
        return Err(GeneratorError::usage(
            "stable verify: gh is required to authenticate release-source attestations",
        ));
    }
    let signer_workflow = format!("{}/{}", inputs.source_repo, RELEASE_SOURCE_SIGNER_WORKFLOW);
    let mut expected_subjects = BTreeSet::new();
    let mut observed_subjects = BTreeSet::new();
    for arch in REQUIRED_ARCHES {
        let filename = format!("{}-{}-{arch}.deb", inputs.package, tag.version);
        let deb = inputs.incoming.join(&filename);
        let digest = sha256_file(&deb)?;
        expected_subjects.insert(digest.clone());
        let deb_arg = if deb.is_absolute() {
            deb.clone()
        } else {
            std::env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("read working directory: {error}")))?
                .join(&deb)
        };
        let deb_arg = deb_arg
            .to_str()
            .ok_or_else(|| GeneratorError::usage("stable deb path is not UTF-8"))?;
        let output = run_fixed(
            "gh",
            &[
                "attestation".to_owned(),
                "verify".to_owned(),
                deb_arg.to_owned(),
                "--repo".to_owned(),
                inputs.source_repo.clone(),
                "--signer-workflow".to_owned(),
                signer_workflow.clone(),
                "--source-ref".to_owned(),
                inputs.preview_source_ref.clone(),
                "--predicate-type".to_owned(),
                RELEASE_SOURCE_PREDICATE.to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
            None,
            inputs.path_overlay,
        )?;
        let subjects = parse_release_source_attestation(
            &output,
            commit,
            &tag.tag,
            &tag.version,
            &inputs.source_repo,
        )?;
        if !subjects.contains(&digest) {
            return Err(GeneratorError::usage(format!(
                "stable verify: release-source attestation omits {arch} deb digest"
            )));
        }
        observed_subjects.extend(subjects);
    }
    if observed_subjects != expected_subjects {
        return Err(GeneratorError::usage(
            "stable verify: release-source attestation subjects do not exactly match the two debs",
        ));
    }
    Ok(())
}

pub(crate) fn parse_release_source_attestation(
    output: &[u8],
    source_sha: &str,
    release_tag: &str,
    version: &str,
    source_repository: &str,
) -> Result<BTreeSet<String>, GeneratorError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| GeneratorError::usage("gh attestation output is not UTF-8"))?;
    let document: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| GeneratorError::usage(format!("gh attestation output is not JSON: {error}")))?;
    let attestations = document.as_array().filter(|attestations| !attestations.is_empty())
        .ok_or_else(|| GeneratorError::usage("gh attestation output has no verified attestations"))?;
    let mut subjects = BTreeSet::new();
    for attestation in attestations {
        let statement = attestation
            .get("verificationResult")
            .and_then(|result| result.get("statement"))
            .ok_or_else(|| GeneratorError::usage("verified attestation has no in-toto statement"))?;
        if field(statement, "predicateType")? != RELEASE_SOURCE_PREDICATE {
            return Err(GeneratorError::usage(
                "stable verify: release-source attestation predicate type mismatch",
            ));
        }
        let predicate = statement
            .get("predicate")
            .ok_or_else(|| GeneratorError::usage("release-source attestation has no predicate"))?;
        for (name, expected) in [
            ("source_sha", source_sha),
            ("release_tag", release_tag),
            ("version", version),
            ("source_repository", source_repository),
        ] {
            if field(predicate, name)? != expected {
                return Err(GeneratorError::usage(format!(
                    "stable verify: release-source attestation {name} mismatch"
                )));
            }
        }
        let attested_subjects = statement
            .get("subject")
            .and_then(serde_json::Value::as_array)
            .filter(|subjects| !subjects.is_empty())
            .ok_or_else(|| GeneratorError::usage("release-source attestation has no subjects"))?;
        for subject in attested_subjects {
            let digest = field(
                subject
                    .get("digest")
                    .ok_or_else(|| GeneratorError::usage("attestation subject has no digest"))?,
                "sha256",
            )?;
            if !valid_digest(digest) {
                return Err(GeneratorError::usage(
                    "release-source attestation subject SHA-256 is malformed",
                ));
            }
            subjects.insert(digest.to_owned());
        }
    }
    Ok(subjects)
}

/// Verify the stable build identity the record pins, returning the
/// manifest hash and manifest version the record binds.
fn verify_stable_record<'a>(
    record: &'a serde_json::Value,
    tag: &StableTag,
    commit: &str,
    source_repo: &str,
) -> Result<(&'a str, u64), GeneratorError> {
    let build = record
        .get("build")
        .ok_or_else(|| GeneratorError::usage("record has no build identity"))?;
    if field(record, "schema")? != RELEASE_RECORD_SCHEMA {
        return Err(GeneratorError::usage("record schema mismatch"));
    }
    if field(build, "repository")? != source_repo {
        return Err(GeneratorError::usage("record repository mismatch"));
    }
    if field(build, "tag")? != tag.tag {
        return Err(GeneratorError::usage("record tag mismatch"));
    }
    if field(build, "crate_version")? != tag.version {
        return Err(GeneratorError::usage("record crate_version mismatch"));
    }
    if field(build, "debian_version")? != tag.version {
        return Err(GeneratorError::usage("record debian_version mismatch"));
    }
    if field(build, "commit")? != commit {
        return Err(GeneratorError::usage(
            "record commit does not match the independently resolved tag commit",
        ));
    }
    let record_manifest_hash = field(build, "manifest_sha256")?;
    if !valid_digest(record_manifest_hash) {
        return Err(GeneratorError::usage(
            "record manifest_sha256 is not a 64-hex digest",
        ));
    }
    Ok((
        record_manifest_hash,
        positive_field(build, "manifest_version")?,
    ))
}

/// Verify the compiled manifest binds the record: hash, source, crate, and
/// manifest-version agreement.
fn verify_stable_manifest(
    manifest_path: &Path,
    tag: &StableTag,
    commit: &str,
    record_manifest_hash: &str,
    record_manifest_version: u64,
    have_manifest: &str,
) -> Result<(), GeneratorError> {
    if record_manifest_hash != have_manifest {
        return Err(GeneratorError::usage(
            "record manifest hash != sha256(manifest.json)",
        ));
    }
    let manifest = read_json(manifest_path)?;
    if field(&manifest, "source_sha")? != commit {
        return Err(GeneratorError::usage(
            "manifest source_sha != resolved commit",
        ));
    }
    if field(&manifest, "crate_version")? != tag.version {
        return Err(GeneratorError::usage("manifest crate_version mismatch"));
    }
    if positive_field(&manifest, "version")? != record_manifest_version {
        return Err(GeneratorError::usage(
            "record manifest_version != manifest version",
        ));
    }
    Ok(())
}

/// Verify the record-internal OCI coherence: index digest shape, image-ref
/// binding, and label agreement.
fn verify_record_oci(
    record: &serde_json::Value,
    version: &str,
    commit: &str,
    source_repo: &str,
    manifest_hash: &str,
) -> Result<(), GeneratorError> {
    let index_digest = field(record, "oci_index_digest")?;
    let Some(hex) = index_digest.strip_prefix("sha256:") else {
        return Err(GeneratorError::usage(
            "oci_index_digest not a sha256 digest",
        ));
    };
    if !valid_digest(hex) {
        return Err(GeneratorError::usage(
            "oci_index_digest not a sha256 digest",
        ));
    }
    let image_ref = field(record, "oci_image_ref")?;
    if !image_ref.ends_with(index_digest) {
        return Err(GeneratorError::usage(
            "oci_image_ref does not pin the index digest",
        ));
    }
    let labels = record
        .get("oci_labels")
        .ok_or_else(|| GeneratorError::usage("record has no OCI labels"))?;
    if field(labels, "version")? != version {
        return Err(GeneratorError::usage("oci label version mismatch"));
    }
    if field(labels, "revision")? != commit {
        return Err(GeneratorError::usage("oci label revision != commit"));
    }
    if field(labels, "source")? != format!("https://github.com/{source_repo}") {
        return Err(GeneratorError::usage("oci label source mismatch"));
    }
    if field(labels, "manifest_sha256")? != manifest_hash {
        return Err(GeneratorError::usage("oci label manifest hash mismatch"));
    }
    Ok(())
}

/// The fixed `docker buildx imagetools inspect` argument vector.
fn oci_inspect_argv(image_ref: &str) -> Vec<String> {
    vec![
        "buildx".to_owned(),
        "imagetools".to_owned(),
        "inspect".to_owned(),
        image_ref.to_owned(),
        "--format".to_owned(),
        "{{json .}}".to_owned(),
    ]
}

/// Verify the record against the live registry: index digest, both platform
/// digests, and every child config label.
fn verify_oci_live(
    record: &serde_json::Value,
    version: &str,
    commit: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    if !tool_present("docker", path_overlay) {
        return Err(GeneratorError::usage("--verify-oci requires docker/buildx"));
    }
    let index_digest = field(record, "oci_index_digest")?;
    let image_ref = field(record, "oci_image_ref")?;
    let image_repo = image_ref
        .split('@')
        .next()
        .ok_or_else(|| GeneratorError::usage("oci_image_ref does not pin the index digest"))?;
    let stdout = run_fixed("docker", &oci_inspect_argv(image_ref), None, path_overlay)?;
    let index: serde_json::Value = serde_json::from_slice(&stdout)
        .map_err(|_| GeneratorError::usage("could not inspect the live OCI index"))?;
    let live_index = index
        .get("manifest")
        .and_then(|manifest| manifest.get("digest"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if live_index != index_digest {
        return Err(GeneratorError::usage(
            "live OCI index digest != record oci_index_digest",
        ));
    }
    let children = index
        .get("manifest")
        .and_then(|manifest| manifest.get("manifests"))
        .and_then(serde_json::Value::as_array);
    for arch in REQUIRED_ARCHES {
        verify_oci_live_arch(
            record,
            children,
            image_repo,
            version,
            commit,
            arch,
            path_overlay,
        )?;
    }
    Ok(())
}

/// Whether the live index binds exactly one linux child per arch digest.
fn live_child_bound(
    children: Option<&Vec<serde_json::Value>>,
    platform_digest: &str,
    arch: &str,
) -> bool {
    children.is_some_and(|manifests| {
        manifests
            .iter()
            .filter(|child| {
                child.get("digest").and_then(serde_json::Value::as_str) == Some(platform_digest)
                    && child
                        .get("platform")
                        .and_then(|platform| platform.get("os"))
                        .and_then(serde_json::Value::as_str)
                        == Some("linux")
                    && child
                        .get("platform")
                        .and_then(|platform| platform.get("architecture"))
                        .and_then(serde_json::Value::as_str)
                        == Some(arch)
            })
            .count()
            == 1
    })
}

/// Verify one live platform child: digest binding plus every config label.
#[allow(clippy::too_many_arguments)]
fn verify_oci_live_arch(
    record: &serde_json::Value,
    children: Option<&Vec<serde_json::Value>>,
    image_repo: &str,
    version: &str,
    commit: &str,
    arch: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let row = record_arch_row(record, arch)?;
    let platform_digest = field(row, "oci_platform_digest")?;
    if !live_child_bound(children, platform_digest, arch) {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} platform digest mismatch"
        )));
    }
    let child_ref = format!("{image_repo}@{platform_digest}");
    let stdout = run_fixed("docker", &oci_inspect_argv(&child_ref), None, path_overlay)?;
    let child: serde_json::Value = serde_json::from_slice(&stdout).map_err(|_| {
        GeneratorError::usage(format!(
            "could not inspect live OCI {arch} platform manifest"
        ))
    })?;
    let child_digest = child
        .get("manifest")
        .and_then(|manifest| manifest.get("digest"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if child_digest != platform_digest {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} child digest mismatch"
        )));
    }
    let labels = child
        .get("image")
        .and_then(|image| image.get("config"))
        .and_then(|config| config.get("Labels"));
    let label = |name: &str| {
        labels
            .and_then(|labels| labels.get(name))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    if label("org.opencontainers.image.version") != version {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} version label mismatch"
        )));
    }
    if label("org.opencontainers.image.revision") != commit {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} revision label mismatch"
        )));
    }
    if label("org.opencontainers.image.source")
        != format!("https://github.com/{}", record_source_repo(record)?)
    {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} source label mismatch"
        )));
    }
    if label("org.velnor.manifest-sha256") != record_manifest_hash(record)? {
        return Err(GeneratorError::usage(format!(
            "live OCI {arch} manifest-sha256 label mismatch"
        )));
    }
    Ok(())
}

/// The source repository the record's build identity names.
fn record_source_repo(record: &serde_json::Value) -> Result<&str, GeneratorError> {
    let build = record
        .get("build")
        .ok_or_else(|| GeneratorError::usage("record has no build identity"))?;
    field(build, "repository")
}

/// The manifest hash the record's build identity pins.
fn record_manifest_hash(record: &serde_json::Value) -> Result<&str, GeneratorError> {
    let build = record
        .get("build")
        .ok_or_else(|| GeneratorError::usage("record has no build identity"))?;
    field(build, "manifest_sha256")
}

/// Verify one stable architecture: deb sidecar, record binding, packaged
/// identity, and the extracted daemon binary hash.
#[allow(clippy::too_many_arguments)]
fn verify_stable_arch(
    inputs: &VerifyInputs<'_>,
    record: &serde_json::Value,
    version: &str,
    commit: &str,
    manifest_hash: &str,
    arch: &str,
) -> Result<(), GeneratorError> {
    let deb_name = format!("{}-{version}-{arch}.deb", inputs.package);
    let deb = inputs.incoming.join(&deb_name);
    let deb_sum = inputs.incoming.join(format!("{deb_name}.sha256"));
    require_file(&deb)?;
    require_file(&deb_sum)?;
    let want_deb = sidecar_digest(&deb_sum)?;
    let have_deb = sha256_file(&deb)?;
    if want_deb != have_deb {
        return Err(GeneratorError::usage(format!(
            "{arch} deb sidecar checksum mismatch"
        )));
    }
    let row = record_arch_row(record, arch)?;
    if field(row, "deb_sha256")? != have_deb {
        return Err(GeneratorError::usage(format!(
            "{arch} deb hash != record deb_sha256"
        )));
    }
    let extract = inputs
        .incoming
        .join(format!(".extract-{arch}-{}", std::process::id()));
    deb_extract_data(&deb, &extract, inputs.backend, inputs.path_overlay)?;
    let result = verify_stable_extracted(
        inputs,
        record,
        version,
        commit,
        manifest_hash,
        arch,
        &extract,
    );
    let _ = std::fs::remove_dir_all(&extract);
    result
}

/// Verify the extracted stable deb tree: packaged build identity, packaged
/// manifest binding, and the daemon binary digest.
fn verify_stable_extracted(
    inputs: &VerifyInputs<'_>,
    record: &serde_json::Value,
    version: &str,
    commit: &str,
    manifest_hash: &str,
    arch: &str,
    extract: &Path,
) -> Result<(), GeneratorError> {
    let identity = extract
        .join("usr/share")
        .join(&inputs.identity_dir)
        .join("build-identity.json");
    let packaged_manifest = extract
        .join("usr/share")
        .join(&inputs.identity_dir)
        .join("manifest.json");
    require_file(&identity)?;
    require_file(&packaged_manifest)?;
    let build_identity = read_json(&identity)?;
    if field(&build_identity, "source_sha")? != commit {
        return Err(GeneratorError::usage(format!(
            "{arch} deb build-identity source_sha != commit"
        )));
    }
    if field(&build_identity, "crate_version")? != version {
        return Err(GeneratorError::usage(format!(
            "{arch} deb build-identity crate_version mismatch"
        )));
    }
    if sha256_file(&packaged_manifest)? != manifest_hash {
        return Err(GeneratorError::usage(format!(
            "{arch} deb packaged manifest hash != record manifest hash"
        )));
    }
    let daemon = extract.join("usr/bin").join(&inputs.binary);
    require_file(&daemon)?;
    let row = record_arch_row(record, arch)?;
    if sha256_file(&daemon)? != field(row, "binary_sha256")? {
        return Err(GeneratorError::usage(format!(
            "{arch} extracted {} binary hash != record binary_sha256",
            inputs.binary
        )));
    }
    Ok(())
}

/// Verify the preview suite: the caller-supplied commit plus the
/// source-owned release manifest carry the coherence chain — there is no tag
/// and no release record here.
fn verify_preview(inputs: &VerifyInputs<'_>) -> Result<(), GeneratorError> {
    if inputs.verify_oci {
        return Err(GeneratorError::usage(
            "verify: --verify-oci does not apply to the preview suite (previews ship no OCI record)",
        ));
    }
    let Some(commit) = inputs.commit.as_deref().filter(|commit| !commit.is_empty()) else {
        return Err(GeneratorError::usage(
            "verify: --commit is required for the preview suite (a preview has no tag to resolve)",
        ));
    };
    if !valid_commit(commit) {
        return Err(GeneratorError::usage(
            "preview commit is not 40 lowercase hex characters",
        ));
    }
    let parsed = parse_preview_version(&inputs.version)?;
    if parsed.sha != commit[..7] {
        return Err(GeneratorError::usage(format!(
            "preview version suffix {} does not match the source commit",
            parsed.sha
        )));
    }
    let current_head = run_resolve_source_ref(
        &inputs.source_repo,
        &inputs.preview_source_ref,
        inputs.path_overlay,
    )?;
    if current_head != commit {
        return Err(GeneratorError::usage(
            "preview commit does not match the current configured source ref head",
        ));
    }
    if inputs.manifest_schema.is_empty() {
        return Err(GeneratorError::usage(
            "verify needs the expected release-manifest schema URN",
        ));
    }
    let incoming = inputs.incoming;
    let manifest_path = incoming.join(PREVIEW_MANIFEST_FILE);
    let sums_path = incoming.join(SHA256SUMS_FILE);
    require_file(&manifest_path)?;
    require_file(&sums_path)?;

    let dotted = dotted_asset_version(&parsed.version);
    let deb_prefix = format!("{}-", inputs.package);
    let debs: Vec<String> = dir_names(incoming)?
        .into_iter()
        .filter(|name| name.starts_with(&deb_prefix) && is_deb_file(name))
        .collect();
    if debs.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(format!(
            "expected exactly {} preview debs in {}, found {} (extra/missing deb)",
            REQUIRED_ARCHES.len(),
            incoming.display(),
            debs.len()
        )));
    }
    for arch in REQUIRED_ARCHES {
        let expected = format!("{}-preview-{dotted}-{arch}.deb", inputs.package);
        require_file(&incoming.join(&expected))?;
    }

    let manifest = read_json(&manifest_path)?;
    if field(&manifest, "schema")? != inputs.manifest_schema {
        return Err(GeneratorError::usage("release-manifest schema mismatch"));
    }
    if field(&manifest, "source_repository")? != inputs.source_repo {
        return Err(GeneratorError::usage(
            "release-manifest repository mismatch",
        ));
    }
    if field(&manifest, "source_ref")? != inputs.preview_source_ref {
        return Err(GeneratorError::usage(format!(
            "release-manifest source_ref is not {}",
            inputs.preview_source_ref
        )));
    }
    if field(&manifest, "source_commit")? != commit {
        return Err(GeneratorError::usage(
            "release-manifest source_commit does not match the caller-supplied commit",
        ));
    }
    if field(&manifest, "version")? != parsed.version {
        return Err(GeneratorError::usage("release-manifest version mismatch"));
    }
    parse_preview_source_release_manifest(
        &manifest,
        &inputs.manifest_schema,
        &inputs.source_repo,
        &inputs.preview_source_ref,
        &preview_source_tag(&parsed.version)?,
        &inputs.package,
    )?;
    let assets = manifest
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("release-manifest assets are not an array"))?;
    if assets.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "release-manifest must list exactly two assets",
        ));
    }

    let sums_bytes = std::fs::read(&sums_path)
        .map_err(|error| GeneratorError::io("read", &sums_path, &error))?;
    let sums = String::from_utf8(sums_bytes)
        .map_err(|_| GeneratorError::usage(format!("{} is not UTF-8", sums_path.display())))?;
    for arch in REQUIRED_ARCHES {
        verify_preview_arch(inputs, &manifest, &parsed, commit, &sums, arch)?;
    }
    let lines = sums.lines().filter(|line| !line.trim().is_empty()).count();
    if lines != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "SHA256SUMS must contain exactly the two preview deb lines",
        ));
    }
    Ok(())
}

/// Verify one preview architecture: sidecar, `SHA256SUMS` and manifest
/// bindings, control fields, and packaged identity.
fn verify_preview_arch(
    inputs: &VerifyInputs<'_>,
    manifest: &serde_json::Value,
    parsed: &PreviewVersion,
    commit: &str,
    sums: &str,
    arch: &str,
) -> Result<(), GeneratorError> {
    let incoming = inputs.incoming;
    let dotted = dotted_asset_version(&parsed.version);
    let deb_name = format!("{}-preview-{dotted}-{arch}.deb", inputs.package);
    let release_name = format!("{}-preview-{}-{arch}.deb", inputs.package, parsed.version);
    let deb = incoming.join(&deb_name);
    let deb_sum = incoming.join(format!("{deb_name}.sha256"));
    require_file(&deb_sum)?;
    let sidecar_bytes =
        std::fs::read(&deb_sum).map_err(|error| GeneratorError::io("read", &deb_sum, &error))?;
    let sidecar = String::from_utf8(sidecar_bytes)
        .map_err(|_| GeneratorError::usage(format!("{} is not UTF-8", deb_sum.display())))?;
    if sidecar.lines().count() != 1 {
        return Err(GeneratorError::usage(format!(
            "{arch} preview sidecar must be a single line"
        )));
    }
    let mut fields = sidecar.split_whitespace();
    let want_deb = fields.next().ok_or_else(|| {
        GeneratorError::usage(format!("{arch} preview sidecar carries no digest"))
    })?;
    if let Some(name) = fields.next()
        && name != deb_name
    {
        return Err(GeneratorError::usage(format!(
            "{arch} preview sidecar does not name {deb_name}"
        )));
    }
    if !valid_digest(want_deb) {
        return Err(GeneratorError::usage(format!(
            "{arch} preview sidecar digest is not 64 lowercase hex"
        )));
    }
    let have_deb = sha256_file(&deb)?;
    if want_deb != have_deb {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb sidecar checksum mismatch"
        )));
    }
    let pinned = sums.lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next() == Some(have_deb.as_str())
            && matches!(parts.next(), Some(name) if name == release_name || name == deb_name)
    });
    if !pinned {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb hash is not pinned by SHA256SUMS"
        )));
    }
    let assets = manifest
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("release-manifest assets are not an array"))?;
    let manifest_bound = assets.iter().any(|asset| {
        matches!(asset.get("name").and_then(serde_json::Value::as_str), Some(name) if name == release_name || name == deb_name)
            && asset.get("sha256").and_then(serde_json::Value::as_str) == Some(have_deb.as_str())
    });
    if !manifest_bound {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb hash != release-manifest asset sha256"
        )));
    }
    if deb_control_field(&deb, "Package", inputs.backend, inputs.path_overlay)? != inputs.package {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb Package is not {}",
            inputs.package
        )));
    }
    if deb_control_field(&deb, "Version", inputs.backend, inputs.path_overlay)? != parsed.version {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb Version != {}",
            parsed.version
        )));
    }
    if deb_control_field(&deb, "Architecture", inputs.backend, inputs.path_overlay)? != arch {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb Architecture != {arch}"
        )));
    }
    let extract = incoming.join(format!(".extract-{arch}-{}", std::process::id()));
    deb_extract_data(&deb, &extract, inputs.backend, inputs.path_overlay)?;
    let result = verify_preview_extracted(inputs, parsed, commit, arch, &extract);
    let _ = std::fs::remove_dir_all(&extract);
    result
}

/// Verify the extracted preview deb tree: packaged build identity and the
/// daemon binary presence.
fn verify_preview_extracted(
    inputs: &VerifyInputs<'_>,
    parsed: &PreviewVersion,
    commit: &str,
    arch: &str,
    extract: &Path,
) -> Result<(), GeneratorError> {
    let identity = extract
        .join("usr/share")
        .join(&inputs.identity_dir)
        .join("build-identity.json");
    require_file(&identity)?;
    let build_identity = read_json(&identity)?;
    if field(&build_identity, "source_sha")? != commit {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb build-identity source_sha != commit"
        )));
    }
    if field(&build_identity, "crate_version")? != parsed.base {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb build-identity crate_version != {}",
            parsed.base
        )));
    }
    require_file(&extract.join("usr/bin").join(&inputs.binary))?;
    Ok(())
}

/// Inputs to suite publication. Publication writes only into `staging`; the
/// live tree is untouched until the single-writer deploy job uploads it.
pub(crate) struct PublishInputs<'a> {
    /// The suite under publication.
    pub(crate) suite: Suite,
    /// The resolved typed contract.
    pub(crate) contract: AptContract,
    /// The candidate version: a `vX.Y.Z` tag for stable, the tilde version
    /// for preview.
    pub(crate) version: String,
    /// The verified coherence inputs (must carry the sentinel).
    pub(crate) incoming: &'a Path,
    /// Every indexed live version pair, authenticated before rollback
    /// selection and preview-pool pruning. Absent only for bootstrap.
    pub(crate) prev_dir: Option<&'a Path>,
    /// The previous-pointer document, already derived by the typed
    /// `apt-previous-pointer` step.
    pub(crate) previous_pointer: &'a Path,
    /// The signed live publication record used to authenticate the current
    /// suite head and same-version retry; absent only for bootstrap.
    pub(crate) published_record: Option<&'a Path>,
    /// Detached signature for `published_record`.
    pub(crate) published_signature: Option<&'a Path>,
    /// The staging tree publication builds.
    pub(crate) staging: &'a Path,
    /// Whether this run initializes a never-published preview suite.
    pub(crate) bootstrap: bool,
    /// The environment secret name the passphrase was resolved from. Names
    /// the secret in diagnostics; the value itself never appears in errors.
    pub(crate) passphrase_env: String,
    /// The resolved signing passphrase, read by the caller from the
    /// `package-feed` environment. `None` means the secret is unset.
    pub(crate) passphrase: Option<String>,
    /// The environment secret name the signing-key material was resolved
    /// from. Names the secret in diagnostics; the material itself never
    /// appears in errors.
    pub(crate) key_env: String,
    /// The resolved signing-key material, read by the caller from the
    /// `package-feed` environment. `None` means the secret is unset.
    pub(crate) key_material: Option<String>,
    /// The `.deb` read backend.
    pub(crate) backend: DebBackend,
    /// Test-only `PATH` overlay resolving fixed tool names.
    pub(crate) path_overlay: Option<&'a Path>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishOutcome {
    Published,
    AlreadyPublished,
}

/// A rollback deb authorized by the suite's signed live package index.
#[derive(Clone, Debug)]
struct AuthenticatedRollbackDeb {
    source_name: String,
    pool_name: String,
    size: u64,
    sha256: String,
}

/// The highest authenticated live version, plus every indexed pair supplied
/// for rollback selection or retention refresh.
#[derive(Clone, Debug)]
struct AuthenticatedRollback {
    version: String,
    debs: BTreeMap<String, AuthenticatedRollbackDeb>,
    indexed_pairs: BTreeMap<String, BTreeMap<String, AuthenticatedRollbackDeb>>,
    release_date: OffsetDateTime,
    current_record: PublicationRecord,
    refresh_required: bool,
}

static ROLLBACK_VERIFY_SEQ: AtomicU64 = AtomicU64::new(0);
static HTTP_PROBE_SEQ: AtomicU64 = AtomicU64::new(0);
static LIVE_SEED_SEQ: AtomicU64 = AtomicU64::new(0);
static DEPLOY_GUARD_SEQ: AtomicU64 = AtomicU64::new(0);

/// Authenticate the rollback pair against the live signed suite index. The
/// caller's `prev_dir` is only a byte transport; it carries no authority by
/// itself.
fn authenticate_live_rollback(
    inputs: &PublishInputs<'_>,
) -> Result<AuthenticatedRollback, GeneratorError> {
    let prev_dir = inputs.prev_dir.ok_or_else(|| {
        GeneratorError::usage("publish: --prev-dir is required for rollback publication")
    })?;
    let local = inspect_rollback_pairs(inputs, prev_dir)?;
    let current_record = authenticate_published_record_input(inputs)?;
    let allow_expired_current = match inputs.suite {
        Suite::Stable => validate_stable_pointer_record(
            inputs.previous_pointer,
            &current_record,
            &parse_stable_tag(&inputs.version)?.tag,
        )?,
        Suite::Preview => {
            require_preview_current_pointer(inputs.previous_pointer, &current_record)?;
            let candidate = parse_preview_version(&inputs.version)?;
            let candidate_manifest = sha256_file(&inputs.incoming.join(PREVIEW_MANIFEST_FILE))?;
            candidate.version == current_record.crate_version
                && candidate_manifest == current_record.source_record_sha256
        }
    };
    let sequence = ROLLBACK_VERIFY_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = std::env::temp_dir().join(format!(
        "velnor-feed-rollback-verify-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| {
                GeneratorError::io("create rollback verification directory", &work, &error)
            })?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work).map_err(|error| {
        GeneratorError::io("create rollback verification directory", &work, &error)
    })?;

    let result = authenticate_live_rollback_in(
        inputs,
        &local,
        &current_record,
        allow_expired_current,
        &work,
    );
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn inspect_rollback_pairs(
    inputs: &PublishInputs<'_>,
    prev_dir: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, AuthenticatedRollbackDeb>>, GeneratorError> {
    let root_metadata = std::fs::symlink_metadata(prev_dir)
        .map_err(|error| GeneratorError::io("inspect rollback directory", prev_dir, &error))?;
    if !root_metadata.file_type().is_dir() {
        return Err(GeneratorError::usage(
            "publish: rollback directory must be a real directory",
        ));
    }
    let mut pairs = BTreeMap::<String, BTreeMap<String, AuthenticatedRollbackDeb>>::new();
    for name in dir_names(prev_dir)? {
        if !is_deb_file(&name) {
            continue;
        }
        let path = prev_dir.join(&name);
        let file_type = std::fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect rollback package", &path, &error))?
            .file_type();
        if !file_type.is_file() {
            return Err(GeneratorError::usage(
                "publish: rollback pair must contain regular package files",
            ));
        }
        if deb_control_field(&path, "Package", inputs.backend, inputs.path_overlay)?
            != inputs.contract.package
        {
            return Err(GeneratorError::usage(
                "publish: rollback pair contains an unexpected package",
            ));
        }
        let version = deb_control_field(&path, "Version", inputs.backend, inputs.path_overlay)?;
        let arch = deb_control_field(&path, "Architecture", inputs.backend, inputs.path_overlay)?;
        if !valid_pool_version(&version) || !REQUIRED_ARCHES.contains(&arch.as_str()) {
            return Err(GeneratorError::usage(
                "publish: rollback pair has an unsafe version or unsupported architecture",
            ));
        }
        let expected_name =
            rollback_asset_name(inputs.suite, &inputs.contract.package, &version, &arch);
        if name != expected_name {
            return Err(GeneratorError::usage(format!(
                "publish: rollback filename does not match its {arch} package identity"
            )));
        }
        let size = std::fs::metadata(&path)
            .map_err(|error| GeneratorError::io("inspect rollback package", &path, &error))?
            .len();
        let sha256 = sha256_file(&path)?;
        if pairs.entry(version.clone()).or_default().insert(
            arch.clone(),
            AuthenticatedRollbackDeb {
                source_name: name,
                pool_name: canonical_pool_name(&inputs.contract.package, &version, &arch),
                size,
                sha256,
            },
        ).is_some()
        {
            return Err(GeneratorError::usage(format!(
                "publish: rollback pair contains more than one {arch} package"
            )));
        }
    }
    if pairs.is_empty()
        || pairs.len() > inputs.contract.retention.indexed_versions().saturating_add(1)
    {
        return Err(GeneratorError::usage(
            "publish: rollback directory must contain one to three recoverable indexed package versions",
        ));
    }
    for (version, debs) in &pairs {
        match inputs.suite {
            Suite::Stable => {
                parse_stable_tag(&format!("v{version}"))?;
            }
            Suite::Preview => {
                parse_preview_version(version)?;
            }
        }
        if debs.len() != REQUIRED_ARCHES.len()
            || REQUIRED_ARCHES.iter().any(|arch| !debs.contains_key(*arch))
        {
            return Err(GeneratorError::usage(format!(
                "publish: rollback version {version} must contain one deb per architecture"
            )));
        }
    }
    Ok(pairs)
}

fn rollback_asset_name(suite: Suite, package: &str, version: &str, arch: &str) -> String {
    match suite {
        Suite::Stable => format!("{package}-{version}-{arch}.deb"),
        Suite::Preview => canonical_pool_name(package, version, arch),
    }
}

fn authenticate_live_rollback_in(
    inputs: &PublishInputs<'_>,
    local: &BTreeMap<String, BTreeMap<String, AuthenticatedRollbackDeb>>,
    current_record: &PublicationRecord,
    allow_expired_current: bool,
    work: &Path,
) -> Result<AuthenticatedRollback, GeneratorError> {
    let feed = inputs.contract.feed_url.trim_end_matches('/');
    let inrelease = work.join("InRelease");
    let release = work.join("Release");
    let inrelease_url = format!("{feed}/dists/{}/InRelease", inputs.suite.as_str());
    curl_https(&inrelease_url, &inrelease, inputs.path_overlay)?;
    let release_stdout = run_fixed(
        "gpgv",
        &[
            "--status-fd".to_owned(),
            "1".to_owned(),
            "--keyring".to_owned(),
            inputs.contract.keyring.clone(),
            "--output".to_owned(),
            release
                .to_str()
                .ok_or_else(|| GeneratorError::usage("Release verification path is not UTF-8"))?
                .to_owned(),
            inrelease
                .to_str()
                .ok_or_else(|| GeneratorError::usage("InRelease path is not UTF-8"))?
                .to_owned(),
        ],
        None,
        inputs.path_overlay,
    )?;
    let status = String::from_utf8(release_stdout)
        .map_err(|_| GeneratorError::usage("gpgv returned non-UTF-8 signature status"))?;
    verify_inrelease_signer(&status, &inputs.contract.signer)?;
    let release_bytes = std::fs::read(&release)
        .map_err(|error| GeneratorError::io("read verified Release", &release, &error))?;
    let release_text = String::from_utf8(release_bytes)
        .map_err(|_| GeneratorError::usage("verified Release is not UTF-8"))?;
    let release_metadata = parse_signed_release_with_expired_current(
        &release_text,
        inputs.suite,
        &inputs.contract,
        allow_expired_current,
    )?;

    let mut entries_by_arch = BTreeMap::<String, BTreeMap<String, LivePackageEntry>>::new();
    let mut index_hashes = BTreeMap::new();
    for arch in REQUIRED_ARCHES {
        let relative = format!("main/binary-{arch}/Packages");
        let (expected_hash, expected_size) = release_metadata.checksums.get(&relative).ok_or_else(|| {
            GeneratorError::usage(format!(
                "publish: signed Release omits the {arch} Packages checksum"
            ))
        })?;
        let packages = work.join(format!("Packages-{arch}"));
        let packages_url = format!("{feed}/dists/{}/{}", inputs.suite.as_str(), relative);
        curl_https(&packages_url, &packages, inputs.path_overlay)?;
        let package_bytes = std::fs::read(&packages)
            .map_err(|error| GeneratorError::io("read signed Packages index", &packages, &error))?;
        if u64::try_from(package_bytes.len()).ok() != Some(*expected_size)
            || sha256_hex(&package_bytes) != *expected_hash
        {
            return Err(GeneratorError::usage(format!(
                "publish: live {arch} Packages bytes disagree with the signed Release checksum"
            )));
        }
        let package_text = String::from_utf8(package_bytes).map_err(|_| {
            GeneratorError::usage(format!("live {arch} Packages index is not UTF-8"))
        })?;
        let entries = parse_live_package_entries(
            &package_text,
            &inputs.contract,
            inputs.suite,
            arch,
        )?;
        let map = entries
            .into_iter()
            .map(|entry| (entry.version.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        if map.is_empty() {
            return Err(GeneratorError::usage(format!(
                "publish: live {arch} Packages index has no versions"
            )));
        }
        entries_by_arch.insert(arch.to_owned(), map);
        index_hashes.insert(arch.to_owned(), expected_hash.clone());
    }
    let amd64 = entries_by_arch.get("amd64").ok_or_else(|| {
        GeneratorError::usage("publish: authenticated amd64 Packages index is missing")
    })?;
    let arm64 = entries_by_arch.get("arm64").ok_or_else(|| {
        GeneratorError::usage("publish: authenticated arm64 Packages index is missing")
    })?;
    let indexed_versions = amd64.keys().cloned().collect::<BTreeSet<_>>();
    if indexed_versions != arm64.keys().cloned().collect::<BTreeSet<_>>() {
        return Err(GeneratorError::usage(
            "publish: live architecture indexes disagree on versions",
        ));
    }
    let version = highest_version(inputs.suite, indexed_versions.iter().cloned())?;
    if current_record.crate_version != version {
        return Err(GeneratorError::usage(
            "publish: signed publication record is not the highest indexed live version",
        ));
    }
    let current_tag_matches = match inputs.suite {
        Suite::Stable => {
            let tag = parse_stable_tag(&current_record.tag)?;
            tag.version == current_record.crate_version
        }
        Suite::Preview => current_record.tag == PREVIEW_TAG,
    };
    if !current_tag_matches
        || current_record.inrelease_sha256 != sha256_file(&inrelease)?
        || current_record.packages.iter().any(|entry| {
            index_hashes.get(&entry.arch).map(|digest| digest.as_str())
                != Some(entry.sha256.as_str())
        })
    {
        return Err(GeneratorError::usage(
            "publish: signed publication record does not authenticate the live indexes",
        ));
    }
    let last_publish = work.join(inputs.suite.last_publish_file());
    curl_https(
        &format!("{feed}/{}", inputs.suite.last_publish_file()),
        &last_publish,
        inputs.path_overlay,
    )?;
    if last_path_text(&last_publish)?
        != if inputs.suite == Suite::Stable {
            current_record.tag.as_str()
        } else {
            current_record.crate_version.as_str()
        }
    {
        return Err(GeneratorError::usage(
            "publish: live last-publish disagrees with the signed publication record",
        ));
    }
    validate_live_record_previous(
        inputs.suite,
        &current_record,
        &indexed_versions,
    )?;

    let mut authorized_pairs = BTreeMap::new();
    for (local_version, local_debs) in local {
        if !indexed_versions.contains(local_version) {
            return Err(GeneratorError::usage(format!(
                "publish: rollback version {local_version} is not present in both signed indexes"
            )));
        }
        let mut authorized = BTreeMap::new();
        for arch in REQUIRED_ARCHES {
            let entry = entries_by_arch
                .get(arch)
                .and_then(|entries| entries.get(local_version))
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "publish: live {arch} Packages omits rollback version {local_version}"
                    ))
                })?;
            let local_deb = local_debs.get(arch).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "publish: rollback version {local_version} is missing {arch}"
                ))
            })?;
            if entry.size != local_deb.size || entry.sha256 != local_deb.sha256 {
                return Err(GeneratorError::usage(format!(
                    "publish: {arch} rollback deb bytes disagree with the signed Packages stanza"
                )));
            }
            authorized.insert(arch.to_owned(), local_deb.clone());
        }
        authorized_pairs.insert(local_version.clone(), authorized);
    }
    let debs = authorized_pairs.get(&version).cloned().ok_or_else(|| {
        GeneratorError::usage("publish: the highest signed live version is absent from --prev-dir")
    })?;
    Ok(AuthenticatedRollback {
        version: version.to_owned(),
        debs,
        indexed_pairs: authorized_pairs,
        release_date: release_metadata.date,
        current_record: current_record.clone(),
        refresh_required: release_metadata.valid_until <= OffsetDateTime::now_utc(),
    })
}

fn authenticate_published_record_input(
    inputs: &PublishInputs<'_>,
) -> Result<PublicationRecord, GeneratorError> {
    let (Some(document), Some(signature)) = (inputs.published_record, inputs.published_signature)
    else {
        return Err(GeneratorError::usage(
            "publish: signed live publication record and signature are required",
        ));
    };
    let value = read_authenticated_publication_record(
        document,
        signature,
        &inputs.contract.keyring,
        &inputs.contract.signer,
        inputs.path_overlay,
    )?;
    let record = parse_publication_record(&value)?;
    if record.suite.as_deref()
        != match inputs.suite {
            Suite::Stable => None,
            Suite::Preview => Some(PREVIEW_SUITE),
        }
    {
        return Err(GeneratorError::usage(
            "publish: signed live publication record names the wrong suite",
        ));
    }
    Ok(record)
}

/// Return true only when the authenticated pointer identifies an exact
/// already-published stable candidate. Normal pointers bind the current live
/// record digest before any rollback bytes are considered.
fn validate_stable_pointer_record(
    pointer_path: &Path,
    current: &PublicationRecord,
    candidate_tag: &str,
) -> Result<bool, GeneratorError> {
    let pointer = read_json(pointer_path)?;
    if pointer
        .get("already_published")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let object = pointer.as_object().ok_or_else(|| {
            GeneratorError::usage("publish: already-published pointer must be an object")
        })?;
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        if keys != ["already_published", "previous", "source_record_sha256", "tag"]
            || field(&pointer, "tag")? != current.tag
            || field(&pointer, "source_record_sha256")? != current.source_record_sha256
            || pointer.get("previous") != Some(&current.previous)
            || current.tag != candidate_tag
        {
            return Err(GeneratorError::usage(
                "publish: signed current stable publication pointer digest or version disagrees",
            ));
        }
        return Ok(true);
    }
    check_stable_pointer_shape(pointer_path)?;
    if field(&pointer, "tag")? != current.tag
        || field(&pointer, "source_record_sha256")? != current.source_record_sha256
        || current.tag == candidate_tag
    {
        return Err(GeneratorError::usage(
            "publish: signed current stable publication pointer digest or version disagrees",
        ));
    }
    Ok(false)
}

fn require_preview_current_pointer(
    pointer_path: &Path,
    current: &PublicationRecord,
) -> Result<(), GeneratorError> {
    if current.tag != PREVIEW_TAG || read_json(pointer_path)?.as_str() != Some(PREVIEW_TAG) {
        return Err(GeneratorError::usage(
            "publish: preview pointer or signed current publication marker is malformed",
        ));
    }
    Ok(())
}

fn validate_live_record_previous(
    suite: Suite,
    record: &PublicationRecord,
    indexed_versions: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    match suite {
        Suite::Stable => match &record.previous {
            serde_json::Value::Null if indexed_versions.len() == 1 => Ok(()),
            serde_json::Value::Object(previous) if indexed_versions.len() >= 2 => {
                let pointer = serde_json::Value::Object(previous.clone());
                let prior = parse_stable_tag(field(&pointer, "tag")?)?.version;
                if indexed_versions.contains(&prior) && prior != record.crate_version {
                    Ok(())
                } else {
                    Err(GeneratorError::usage(
                        "publish: signed stable previous pointer disagrees with package indexes",
                    ))
                }
            }
            _ => Err(GeneratorError::usage(
                "publish: signed stable previous pointer disagrees with package indexes",
            )),
        },
        Suite::Preview => match record.previous.as_str() {
            None if record.previous.is_null() && indexed_versions.len() == 1 => Ok(()),
            Some(PREVIEW_TAG) if indexed_versions.len() >= 2 => Ok(()),
            _ => Err(GeneratorError::usage(
                "publish: signed preview previous marker disagrees with package indexes",
            )),
        },
    }
}

fn curl_https(url: &str, output: &Path, path_overlay: Option<&Path>) -> Result<(), GeneratorError> {
    let output = output
        .to_str()
        .ok_or_else(|| GeneratorError::usage("feed download path is not UTF-8"))?;
    run_fixed(
        "curl",
        &[
            "--fail".to_owned(),
            "--show-error".to_owned(),
            "--silent".to_owned(),
            "--location".to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--proto-redir".to_owned(),
            "=https".to_owned(),
            "--retry".to_owned(),
            "3".to_owned(),
            "--max-time".to_owned(),
            "60".to_owned(),
            "--output".to_owned(),
            output.to_owned(),
            "--".to_owned(),
            url.to_owned(),
        ],
        None,
        path_overlay,
    )?;
    Ok(())
}

fn classify_http_probe_status(status: &str) -> Result<HttpProbeResult, GeneratorError> {
    let code = status.trim().parse::<u16>().map_err(|_| {
        GeneratorError::usage("APT HTTP probe returned a malformed HTTP status")
    })?;
    match code {
        200..=299 => Ok(HttpProbeResult::Present),
        404 => Ok(HttpProbeResult::NotFound),
        _ => Err(GeneratorError::usage(format!(
            "APT HTTP probe returned HTTP {code}; only 2xx and 404 are accepted"
        ))),
    }
}

/// Fetch a feed resource and classify its final HTTPS status. A real 404 is
/// the sole absence signal; curl/network failures and other statuses fail.
pub(crate) fn probe_https(
    url: &str,
    output: &Path,
    path_overlay: Option<&Path>,
) -> Result<HttpProbeResult, GeneratorError> {
    if !valid_feed_url(url) {
        return Err(GeneratorError::usage(
            "APT HTTP probe URL must be a safe HTTPS feed URL",
        ));
    }
    let output_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let sequence = HTTP_PROBE_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = output_parent.join(format!(
        ".velnor-apt-http-probe-{}-{sequence}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| GeneratorError::io("create APT HTTP probe directory", &work, &error))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work)
        .map_err(|error| GeneratorError::io("create APT HTTP probe directory", &work, &error))?;

    let response = work.join("response");
    let response_path = response
        .to_str()
        .ok_or_else(|| GeneratorError::usage("APT HTTP probe path is not UTF-8"))?;
    let result = run_fixed(
        "curl",
        &[
            "--show-error".to_owned(),
            "--silent".to_owned(),
            "--location".to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--proto-redir".to_owned(),
            "=https".to_owned(),
            "--retry".to_owned(),
            "3".to_owned(),
            "--connect-timeout".to_owned(),
            "15".to_owned(),
            "--max-time".to_owned(),
            "60".to_owned(),
            "--output".to_owned(),
            response_path.to_owned(),
            "--write-out".to_owned(),
            "%{http_code}".to_owned(),
            "--".to_owned(),
            url.to_owned(),
        ],
        None,
        path_overlay,
    )
    .and_then(|bytes| {
        let status = String::from_utf8(bytes)
            .map_err(|_| GeneratorError::usage("APT HTTP probe status is not UTF-8"))?;
        classify_http_probe_status(&status)
    });

    let result = match result {
        Ok(HttpProbeResult::Present) => std::fs::rename(&response, output)
            .map(|()| HttpProbeResult::Present)
            .map_err(|error| GeneratorError::io("save APT HTTP probe response", output, &error)),
        Ok(HttpProbeResult::NotFound) => {
            match std::fs::remove_file(output) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&work);
                    return Err(GeneratorError::io(
                        "clear APT HTTP probe response",
                        output,
                        &error,
                    ));
                }
            }
            Ok(HttpProbeResult::NotFound)
        }
        Err(error) => Err(error),
    };
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| GeneratorError::io("remove APT HTTP probe directory", &work, &error));
    match (result, cleanup) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn verify_gpgv_signer(
    status: &str,
    pinned: &str,
    resource: &str,
) -> Result<(), GeneratorError> {
    let mut valid_signers = Vec::new();
    for line in status.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.get(0) == Some(&"[GNUPG:]") && fields.get(1) == Some(&"VALIDSIG") {
            // VALIDSIG's optional final field is the primary-key fingerprint
            // when the signature uses a signing subkey.
            let signer = fields.get(11).copied().or_else(|| fields.get(2).copied());
            if let Some(signer) = signer {
                valid_signers.push(signer);
            }
        }
    }
    if valid_signers.len() != 1
        || !is_full_fingerprint(&normalize_fingerprint(valid_signers[0]))
        || !fingerprints_match(valid_signers[0], pinned)
    {
        return Err(GeneratorError::usage(format!(
            "publish: {resource} signature does not match the pinned publisher key"
        )));
    }
    Ok(())
}

fn verify_inrelease_signer(status: &str, pinned: &str) -> Result<(), GeneratorError> {
    verify_gpgv_signer(status, pinned, "live InRelease")
}

/// Verify a detached publication-record signature before opening or parsing
/// the JSON bytes, then bind its declared key to the pinned publisher.
pub(crate) fn read_authenticated_publication_record(
    document_path: &Path,
    signature_path: &Path,
    keyring: &str,
    pinned_signer: &str,
    path_overlay: Option<&Path>,
) -> Result<serde_json::Value, GeneratorError> {
    let signature = signature_path
        .to_str()
        .ok_or_else(|| GeneratorError::usage("publication signature path is not UTF-8"))?;
    let document = document_path
        .to_str()
        .ok_or_else(|| GeneratorError::usage("publication record path is not UTF-8"))?;
    let status = run_fixed(
        "gpgv",
        &[
            "--status-fd".to_owned(),
            "1".to_owned(),
            "--keyring".to_owned(),
            keyring.to_owned(),
            "--".to_owned(),
            signature.to_owned(),
            document.to_owned(),
        ],
        None,
        path_overlay,
    )?;
    let status = String::from_utf8(status)
        .map_err(|_| GeneratorError::usage("gpgv returned non-UTF-8 signature status"))?;
    verify_gpgv_signer(&status, pinned_signer, "publication record")?;
    let bytes = std::fs::read(document_path)
        .map_err(|error| GeneratorError::io("read authenticated publication record", document_path, &error))?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        GeneratorError::usage(format!("authenticated publication record is not JSON: {error}"))
    })?;
    let record = parse_publication_record(&parsed)?;
    if !fingerprints_match(&record.signer_fingerprint, pinned_signer) {
        return Err(GeneratorError::usage(
            "publication record signer does not match the pinned publisher key",
        ));
    }
    Ok(parsed)
}

#[derive(Clone, Debug)]
struct LivePackageEntry {
    version: String,
    filename: String,
    size: u64,
    sha256: String,
}

/// Restore every authenticated live suite into a fresh Pages tree. This is
/// the bridge that keeps the stable and preview repositories in the same
/// Pages artifact even though Actions starts each run with an empty artifact.
pub(crate) fn seed_live_feed(
    contract: &AptContract,
    staging: &Path,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let transaction = StagingTransaction::new(staging)?;
    let sequence = LIVE_SEED_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = std::env::temp_dir().join(format!(
        "velnor-apt-seed-{}-{sequence}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| GeneratorError::io("create feed restore directory", &work, &error))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work)
        .map_err(|error| GeneratorError::io("create feed restore directory", &work, &error))?;

    let result = (|| {
        for suite in [Suite::Stable, Suite::Preview] {
            // Seeding may carry a signed but expired current publication only
            // so `apt-publish` can re-sign the exact current source and package
            // bytes. Publishing rejects every changed or newer candidate when
            // the old metadata is expired, and the deploy guard requires fresh
            // staged metadata before any upload.
            restore_live_suite(contract, suite, transaction.path(), &work, path_overlay, true)?;
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| GeneratorError::io("remove feed restore directory", &work, &error));
    result?;
    cleanup?;
    transaction.commit()
}

fn restore_live_suite(
    contract: &AptContract,
    suite: Suite,
    staging: &Path,
    work: &Path,
    path_overlay: Option<&Path>,
    allow_expired_current: bool,
) -> Result<(), GeneratorError> {
    let feed = contract.feed_url.trim_end_matches('/');
    let suite_work = work.join(suite.as_str());
    std::fs::create_dir(&suite_work)
        .map_err(|error| GeneratorError::io("create suite restore directory", &suite_work, &error))?;
    let inrelease = suite_work.join("InRelease");
    let inrelease_url = format!("{feed}/dists/{}/InRelease", suite.as_str());
    match probe_https(&inrelease_url, &inrelease, path_overlay)? {
        HttpProbeResult::NotFound => {
            let mut partial = Vec::new();
            let mut paths = vec![
                format!("dists/{}/Release", suite.as_str()),
                format!("dists/{}/Release.gpg", suite.as_str()),
                suite.last_publish_file().to_owned(),
                suite.publication_record_file().to_owned(),
                format!("{}.sig", suite.publication_record_file()),
                suite.channel_state_file().to_owned(),
            ];
            for arch in REQUIRED_ARCHES {
                paths.push(format!(
                    "dists/{}/main/binary-{arch}/Packages",
                    suite.as_str()
                ));
                paths.push(format!(
                    "dists/{}/main/binary-{arch}/Packages.gz",
                    suite.as_str()
                ));
            }
            let pool_prefix = if suite == Suite::Preview {
                "pool/preview/"
            } else {
                "pool/"
            };
            paths.push(format!(
                "{pool_prefix}main/{}/{}/",
                pool_letter(&contract.package),
                contract.package
            ));
            paths.sort();
            for (index, relative) in paths.iter().enumerate() {
                let output = suite_work.join(format!("absence-{index}"));
                let result = probe_https(&format!("{feed}/{relative}"), &output, path_overlay)?;
                if result == HttpProbeResult::Present {
                    partial.push(relative.as_str());
                }
            }
            if partial.is_empty() {
                return Ok(());
            }
            return Err(GeneratorError::usage(format!(
                "live {} suite is incomplete: InRelease is absent but {} exists",
                suite.as_str(),
                partial.join(", ")
            )));
        }
        HttpProbeResult::Present => {}
    }

    let release = suite_work.join("Release");
    let inrelease_status = run_fixed(
        "gpgv",
        &[
            "--status-fd".to_owned(),
            "1".to_owned(),
            "--keyring".to_owned(),
            contract.keyring.clone(),
            "--output".to_owned(),
            release
                .to_str()
                .ok_or_else(|| GeneratorError::usage("Release restore path is not UTF-8"))?
                .to_owned(),
            inrelease
                .to_str()
                .ok_or_else(|| GeneratorError::usage("InRelease restore path is not UTF-8"))?
                .to_owned(),
        ],
        None,
        path_overlay,
    )?;
    let inrelease_status = String::from_utf8(inrelease_status)
        .map_err(|_| GeneratorError::usage("gpgv returned non-UTF-8 signature status"))?;
    verify_gpgv_signer(&inrelease_status, &contract.signer, "live InRelease")?;
    let release_bytes = std::fs::read(&release)
        .map_err(|error| GeneratorError::io("read verified live Release", &release, &error))?;
    let release_text = String::from_utf8(release_bytes)
        .map_err(|_| GeneratorError::usage("verified live Release is not UTF-8"))?;
    let metadata = parse_signed_release_with_expired_current(
        &release_text,
        suite,
        contract,
        allow_expired_current,
    )?;

    let release_signature = suite_work.join("Release.gpg");
    curl_https(
        &format!("{feed}/dists/{}/Release.gpg", suite.as_str()),
        &release_signature,
        path_overlay,
    )?;
    verify_detached_signature(
        &release_signature,
        &release,
        &contract.keyring,
        &contract.signer,
        "live Release",
        path_overlay,
    )?;

    let dists = staging.join(format!("dists/{}", suite.as_str()));
    std::fs::create_dir_all(&dists)
        .map_err(|error| GeneratorError::io("create suite staging directory", &dists, &error))?;
    std::fs::copy(&inrelease, dists.join("InRelease"))
        .map_err(|error| GeneratorError::io("stage InRelease", &dists, &error))?;
    std::fs::copy(&release, dists.join("Release"))
        .map_err(|error| GeneratorError::io("stage Release", &dists, &error))?;
    std::fs::copy(&release_signature, dists.join("Release.gpg"))
        .map_err(|error| GeneratorError::io("stage Release.gpg", &dists, &error))?;

    let mut expected_indexes = BTreeSet::new();
    for arch in REQUIRED_ARCHES {
        expected_indexes.insert(format!("main/binary-{arch}/Packages"));
        expected_indexes.insert(format!("main/binary-{arch}/Packages.gz"));
    }
    if metadata.checksums.keys().cloned().collect::<BTreeSet<_>>() != expected_indexes {
        return Err(GeneratorError::usage(format!(
            "live {} Release must authenticate exactly the supported Packages indexes",
            suite.as_str()
        )));
    }
    let mut by_arch = BTreeMap::<String, BTreeSet<String>>::new();
    let mut candidate_hashes = BTreeMap::<String, String>::new();
    for arch in REQUIRED_ARCHES {
        let relative = format!("main/binary-{arch}/Packages");
        let (expected_hash, expected_size) = metadata
            .checksums
            .get(&relative)
            .ok_or_else(|| GeneratorError::usage("live Release omits Packages index"))?;
        let packages = suite_work.join(format!("Packages-{arch}"));
        curl_https(
            &format!("{feed}/dists/{}/{relative}", suite.as_str()),
            &packages,
            path_overlay,
        )?;
        let bytes = std::fs::read(&packages)
            .map_err(|error| GeneratorError::io("read live Packages index", &packages, &error))?;
        if bytes.len() as u64 != *expected_size || sha256_hex(&bytes) != *expected_hash {
            return Err(GeneratorError::usage(format!(
                "live {arch} Packages bytes disagree with signed Release"
            )));
        }
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| GeneratorError::usage(format!("live {arch} Packages is not UTF-8")))?;
        let entries = parse_live_package_entries(&text, contract, suite, arch)?;
        for entry in entries {
            by_arch
                .entry(arch.to_owned())
                .or_default()
                .insert(entry.version.clone());
            let pool_file = suite_work.join(format!("deb-{}-{arch}", entry.version));
            curl_https(
                &format!("{feed}/{}", entry.filename),
                &pool_file,
                path_overlay,
            )?;
            let package_bytes = std::fs::read(&pool_file).map_err(|error| {
                GeneratorError::io("read live package bytes", &pool_file, &error)
            })?;
            if package_bytes.len() as u64 != entry.size || sha256_hex(&package_bytes) != entry.sha256 {
                return Err(GeneratorError::usage(format!(
                    "live {arch} package bytes disagree with signed Packages"
                )));
            }
            if entry.version == live_current_version(suite, &text, &contract.package)? {
                candidate_hashes.insert(arch.to_owned(), entry.sha256.clone());
            }
            let destination = staging.join(&entry.filename);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| GeneratorError::io("create live pool path", parent, &error))?;
            }
            std::fs::copy(&pool_file, &destination)
                .map_err(|error| GeneratorError::io("stage live package", &destination, &error))?;
        }
        let package_dst = dists.join(format!("main/binary-{arch}"));
        std::fs::create_dir_all(&package_dst)
            .map_err(|error| GeneratorError::io("create Packages path", &package_dst, &error))?;
        std::fs::write(package_dst.join("Packages"), &bytes)
            .map_err(|error| GeneratorError::io("stage Packages", &package_dst, &error))?;

        let gzip_relative = format!("main/binary-{arch}/Packages.gz");
        let (expected_gzip_hash, expected_gzip_size) = metadata
            .checksums
            .get(&gzip_relative)
            .ok_or_else(|| GeneratorError::usage("live Release omits compressed Packages index"))?;
        let packages_gz = suite_work.join(format!("Packages-{arch}.gz"));
        curl_https(
            &format!("{feed}/dists/{}/{gzip_relative}", suite.as_str()),
            &packages_gz,
            path_overlay,
        )?;
        let gzip_bytes = std::fs::read(&packages_gz)
            .map_err(|error| GeneratorError::io("read compressed Packages", &packages_gz, &error))?;
        if gzip_bytes.len() as u64 != *expected_gzip_size
            || sha256_hex(&gzip_bytes) != *expected_gzip_hash
        {
            return Err(GeneratorError::usage(format!(
                "live {arch} compressed Packages bytes disagree with signed Release"
            )));
        }
        std::fs::write(package_dst.join("Packages.gz"), gzip_bytes)
            .map_err(|error| GeneratorError::io("stage compressed Packages", &package_dst, &error))?;
    }
    let amd64_versions = by_arch.get("amd64").cloned().unwrap_or_default();
    let arm64_versions = by_arch.get("arm64").cloned().unwrap_or_default();
    if amd64_versions != arm64_versions
        || amd64_versions.is_empty()
        || amd64_versions.len() > contract.retention.indexed_versions().saturating_add(1)
    {
        return Err(GeneratorError::usage(format!(
            "live {} Packages indexes disagree on retained versions",
            suite.as_str()
        )));
    }

    let record_path = suite_work.join(suite.publication_record_file());
    let signature_path = suite_work.join(format!("{}.sig", suite.publication_record_file()));
    let state_path = suite_work.join(suite.channel_state_file());
    let last_path = suite_work.join(suite.last_publish_file());
    let metadata_files = [
        (suite.publication_record_file().to_owned(), record_path.as_path()),
        (
            format!("{}.sig", suite.publication_record_file()),
            signature_path.as_path(),
        ),
        (suite.channel_state_file().to_owned(), state_path.as_path()),
        (suite.last_publish_file().to_owned(), last_path.as_path()),
    ];
    for (relative, path) in metadata_files {
        curl_https(&format!("{feed}/{relative}"), path, path_overlay)?;
    }
    let record_value = read_authenticated_publication_record(
        &record_path,
        &signature_path,
        &contract.keyring,
        &contract.signer,
        path_overlay,
    )?;
    let record = parse_publication_record(&record_value)?;
    if record.suite.as_deref() != match suite {
        Suite::Stable => None,
        Suite::Preview => Some(PREVIEW_SUITE),
    } || record.inrelease_sha256 != sha256_file(&inrelease)?
        || !fingerprints_match(&record.signer_fingerprint, &contract.signer)
    {
        return Err(GeneratorError::usage(format!(
            "live {} publication record disagrees with the authenticated suite",
            suite.as_str()
        )));
    }
    let record_packages = record
        .packages
        .iter()
        .map(|entry| (entry.arch.as_str(), entry.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    for arch in REQUIRED_ARCHES {
        let index = dists.join(format!("main/binary-{arch}/Packages"));
        let digest = sha256_file(&index)?;
        if record_packages.get(arch).copied() != Some(digest.as_str()) {
            return Err(GeneratorError::usage(format!(
                "live {} publication record Packages digest mismatch for {arch}",
                suite.as_str()
            )));
        }
    }
    let current = record.crate_version.as_str();
    if !amd64_versions.contains(current)
        || highest_version(suite, amd64_versions.iter().cloned())? != current
    {
        return Err(GeneratorError::usage(format!(
            "live {} signed publication version is not the highest indexed package",
            suite.as_str()
        )));
    }
    validate_live_record_previous(suite, &record, &amd64_versions)?;
    match suite {
        Suite::Stable => {
            let tag = parse_stable_tag(&record.tag)?;
            if tag.version != record.crate_version
                || last_path_text(&last_path)? != record.tag
            {
                return Err(GeneratorError::usage(
                    "live stable last-publish disagrees with signed publication record",
                ));
            }
        }
        Suite::Preview => {
            if record.tag != PREVIEW_TAG || last_path_text(&last_path)? != record.crate_version {
                return Err(GeneratorError::usage(
                    "live preview last-publish or rollback state disagrees with signed publication record",
                ));
            }
        }
    }
    let state_bytes = std::fs::read(&state_path)
        .map_err(|error| GeneratorError::io("read live package state", &state_path, &error))?;
    let state: serde_json::Value = serde_json::from_slice(&state_bytes)
        .map_err(|error| GeneratorError::usage(format!("live package state is not JSON: {error}")))?;
    let expected_state_version = if suite == Suite::Stable {
        record.tag.as_str()
    } else {
        record.crate_version.as_str()
    };
    if field(&state, "schema")? != PACKAGE_STATE_SCHEMA
        || field(&state, "source_repository")? != contract.source_repo
        || field(&state, "source_commit")?.len() != 40
        || !valid_commit(field(&state, "source_commit")?)
        || field(&state, "version")? != expected_state_version
        || field(&state, "source_ref")?
            != if suite == Suite::Stable {
                format!("refs/tags/{}", record.tag)
            } else {
                contract.preview_source_ref.clone()
            }
    {
        return Err(GeneratorError::usage(format!(
            "live {} package state disagrees with the authenticated publication",
            suite.as_str()
        )));
    }
    verify_live_state_candidate(&state, suite, contract, current, &candidate_hashes)?;

    for (source, name) in [
        (record_path, suite.publication_record_file().to_owned()),
        (
            signature_path,
            format!("{}.sig", suite.publication_record_file()),
        ),
        (state_path, suite.channel_state_file().to_owned()),
        (last_path, suite.last_publish_file().to_owned()),
    ] {
        std::fs::copy(&source, staging.join(&name))
            .map_err(|error| GeneratorError::io("stage live suite metadata", staging, &error))?;
    }
    Ok(())
}

fn verify_detached_signature(
    signature_path: &Path,
    document_path: &Path,
    keyring: &str,
    signer: &str,
    resource: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let signature = signature_path
        .to_str()
        .ok_or_else(|| GeneratorError::usage("detached signature path is not UTF-8"))?;
    let document = document_path
        .to_str()
        .ok_or_else(|| GeneratorError::usage("signed document path is not UTF-8"))?;
    let status = run_fixed(
        "gpgv",
        &[
            "--status-fd".to_owned(),
            "1".to_owned(),
            "--keyring".to_owned(),
            keyring.to_owned(),
            "--".to_owned(),
            signature.to_owned(),
            document.to_owned(),
        ],
        None,
        path_overlay,
    )?;
    let status = String::from_utf8(status)
        .map_err(|_| GeneratorError::usage("gpgv returned non-UTF-8 signature status"))?;
    verify_gpgv_signer(&status, signer, resource)
}

fn last_path_text(path: &Path) -> Result<String, GeneratorError> {
    let bytes = std::fs::read(path)
        .map_err(|error| GeneratorError::io("read live last-publish", path, &error))?;
    String::from_utf8(bytes)
        .map(|text| text.trim().to_owned())
        .map_err(|_| GeneratorError::usage("live last-publish is not UTF-8"))
}

fn parse_live_package_entries(
    text: &str,
    contract: &AptContract,
    suite: Suite,
    arch: &str,
) -> Result<Vec<LivePackageEntry>, GeneratorError> {
    let mut entries = Vec::new();
    for stanza in text.split("\n\n").filter(|stanza| !stanza.trim().is_empty()) {
        let mut fields = BTreeMap::new();
        for line in stanza.lines().map(|line| line.trim_end_matches('\r')) {
            if line.starts_with([' ', '\t']) || line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(GeneratorError::usage("live Packages stanza is malformed"));
            };
            if fields.insert(name, value.trim().to_owned()).is_some() {
                return Err(GeneratorError::usage("live Packages stanza repeats a field"));
            }
        }
        let version = fields.get("Version").cloned().unwrap_or_default();
        let filename = fields.get("Filename").cloned().unwrap_or_default();
        let size = fields.get("Size").and_then(|value| value.parse::<u64>().ok());
        let sha256 = fields.get("SHA256").cloned().unwrap_or_default();
        let expected = match suite {
            Suite::Stable => format!(
                "pool/main/{}/{}/{}",
                pool_letter(&contract.package),
                contract.package,
                canonical_pool_name(&contract.package, &version, arch)
            ),
            Suite::Preview => format!(
                "pool/preview/main/{}/{}/{}",
                pool_letter(&contract.package),
                contract.package,
                canonical_pool_name(&contract.package, &version, arch)
            ),
        };
        let valid_version = match suite {
            Suite::Stable => parse_stable_tag(&format!("v{version}")).is_ok(),
            Suite::Preview => parse_preview_version(&version).is_ok(),
        };
        if fields.get("Package").map(String::as_str) != Some(contract.package.as_str())
            || fields.get("Architecture").map(String::as_str) != Some(arch)
            || !valid_version
            || filename != expected
            || size.is_none_or(|size| size == 0)
            || !valid_digest(&sha256)
        {
            return Err(GeneratorError::usage(format!(
                "live {arch} Packages stanza has invalid package identity or bytes"
            )));
        }
        entries.push(LivePackageEntry {
            version,
            filename,
            size: size.unwrap_or_default(),
            sha256,
        });
    }
    let mut versions = BTreeSet::new();
    for entry in &entries {
        if !versions.insert(entry.version.as_str()) {
            return Err(GeneratorError::usage(format!(
                "live {arch} Packages repeats a version"
            )));
        }
    }
    if entries.is_empty()
        || entries.len() > contract.retention.indexed_versions().saturating_add(1)
    {
        return Err(GeneratorError::usage(format!(
            "live {arch} Packages exceeds the recoverable indexed-version limit"
        )));
    }
    Ok(entries)
}

fn live_current_version(
    suite: Suite,
    text: &str,
    package: &str,
) -> Result<String, GeneratorError> {
    highest_version(suite, packages_versions(text, package))
}

fn verify_live_state_candidate(
    state: &serde_json::Value,
    suite: Suite,
    contract: &AptContract,
    current: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<(), GeneratorError> {
    let packages = state
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("live package state packages are not an array"))?;
    if packages.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "live package state must name both candidate architectures",
        ));
    }
    let version = match suite {
        Suite::Stable => format!("v{current}"),
        Suite::Preview => current.to_owned(),
    };
    let mut seen = BTreeSet::new();
    for package in packages {
        let name = field(package, "name")?;
        let digest = field(package, "sha256")?;
        let arch = REQUIRED_ARCHES
            .iter()
            .find(|arch| name.ends_with(&format!("-{arch}.deb")))
            .copied()
            .ok_or_else(|| GeneratorError::usage("live package state names an unsupported arch"))?;
        let expected_name = match suite {
            Suite::Stable => format!("{}-{current}-{arch}.deb", contract.package),
            Suite::Preview => format!(
                "{}-preview-{}-{arch}.deb",
                contract.package,
                dotted_asset_version(current)
            ),
        };
        if name != expected_name
            || !valid_digest(digest)
            || hashes.get(arch).map(String::as_str) != Some(digest)
            || !seen.insert(arch)
        {
            return Err(GeneratorError::usage(format!(
                "live {version} package state candidate bytes disagree for {arch}"
            )));
        }
    }
    if REQUIRED_ARCHES.iter().any(|arch| !seen.contains(arch)) {
        return Err(GeneratorError::usage(
            "live package state omits a candidate architecture",
        ));
    }
    Ok(())
}

struct SignedReleaseMetadata {
    checksums: BTreeMap<String, (String, u64)>,
    date: OffsetDateTime,
    valid_until: OffsetDateTime,
}

fn parse_release_time(value: &str, field: &str) -> Result<OffsetDateTime, GeneratorError> {
    OffsetDateTime::parse(value, &Rfc2822).map_err(|error| {
        GeneratorError::usage(format!(
            "publish: signed Release {field} is not a valid RFC 2822 timestamp: {error}"
        ))
    })
}

fn validate_release_freshness(
    date: OffsetDateTime,
    valid_until: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<(), GeneratorError> {
    validate_release_freshness_with_expiry(date, valid_until, now, false)
}

fn validate_release_freshness_with_expiry(
    date: OffsetDateTime,
    valid_until: OffsetDateTime,
    now: OffsetDateTime,
    allow_expired: bool,
) -> Result<(), GeneratorError> {
    if date > now + time::Duration::minutes(5) {
        return Err(GeneratorError::usage(
            "publish: signed Release Date is too far in the future",
        ));
    }
    if valid_until <= now && !allow_expired {
        return Err(GeneratorError::usage(
            "publish: signed Release Valid-Until has expired",
        ));
    }
    if valid_until <= date || valid_until - date > time::Duration::days(31) {
        return Err(GeneratorError::usage(
            "publish: signed Release validity interval is malformed or exceeds 31 days",
        ));
    }
    Ok(())
}

/// Give a newly generated Release a fresh, monotonic Date and bounded
/// Valid-Until interval before signing it.
fn stamp_release_freshness(
    text: &str,
    previous_date: Option<OffsetDateTime>,
) -> Result<String, GeneratorError> {
    let now = OffsetDateTime::now_utc();
    let date = match previous_date {
        Some(previous) if now <= previous => previous + time::Duration::seconds(1),
        _ => now,
    };
    let valid_until = date + time::Duration::days(7);
    let date_text = date
        .format(&Rfc2822)
        .map_err(|error| GeneratorError::usage(format!("format Release Date: {error}")))?;
    let valid_text = valid_until
        .format(&Rfc2822)
        .map_err(|error| GeneratorError::usage(format!("format Release Valid-Until: {error}")))?;
    let mut body = String::new();
    let mut date_count = 0;
    let mut valid_until_count = 0;
    for line in text.lines() {
        if line.starts_with("Date:") {
            date_count += 1;
        } else if line.starts_with("Valid-Until:") {
            valid_until_count += 1;
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if date_count > 1 || valid_until_count > 1 {
        return Err(GeneratorError::usage(
            "publish: generated Release repeats a freshness field",
        ));
    }
    let mut output = format!("Date: {date_text}\nValid-Until: {valid_text}\n{body}");
    if !text.ends_with('\n') {
        output.pop();
    }
    Ok(output)
}

fn parse_signed_release(
    text: &str,
    suite: Suite,
    contract: &AptContract,
) -> Result<SignedReleaseMetadata, GeneratorError> {
    parse_signed_release_with_expired_current(text, suite, contract, false)
}

fn parse_signed_release_with_expired_current(
    text: &str,
    suite: Suite,
    contract: &AptContract,
    allow_expired_current: bool,
) -> Result<SignedReleaseMetadata, GeneratorError> {
    let mut fields = BTreeMap::new();
    let mut checksums = BTreeMap::new();
    let mut in_sha256 = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if in_sha256 {
                let mut values = line.split_whitespace();
                let digest = values.next().unwrap_or_default();
                let size = values.next().unwrap_or_default().parse::<u64>().ok();
                let path = values.next().unwrap_or_default();
                if values.next().is_some()
                    || !valid_digest(digest)
                    || size.is_none_or(|s| s == 0)
                    || path.is_empty()
                {
                    return Err(GeneratorError::usage(
                        "publish: signed Release SHA256 entry is malformed",
                    ));
                }
                if checksums
                    .insert(
                        path.to_owned(),
                        (digest.to_owned(), size.unwrap_or_default()),
                    )
                    .is_some()
                {
                    return Err(GeneratorError::usage(
                        "publish: signed Release repeats a SHA256 path",
                    ));
                }
            }
            continue;
        }
        in_sha256 = false;
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if matches!(
            name,
            "Origin"
                | "Label"
                | "Suite"
                | "Codename"
                | "Architectures"
                | "Components"
                | "Date"
                | "Valid-Until"
                | "SHA256"
        ) {
            let value = value.trim();
            if name == "SHA256" {
                if !value.is_empty() {
                    return Err(GeneratorError::usage(
                        "publish: signed Release SHA256 header is malformed",
                    ));
                }
                in_sha256 = true;
            } else if fields.insert(name, value.to_owned()).is_some() {
                return Err(GeneratorError::usage(format!(
                    "publish: signed Release repeats {name}"
                )));
            }
        }
    }
    for (name, expected) in [
        ("Origin", contract.origin.as_str()),
        ("Label", contract.origin.as_str()),
        ("Suite", suite.as_str()),
        ("Codename", suite.as_str()),
    ] {
        if fields.get(name).map(String::as_str) != Some(expected) {
            return Err(GeneratorError::usage(format!(
                "publish: signed Release {name} does not match the APT contract"
            )));
        }
    }
    let arches = fields
        .get("Architectures")
        .map(|value| value.split_whitespace().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    if arches.len() != REQUIRED_ARCHES.len()
        || REQUIRED_ARCHES.iter().any(|arch| !arches.contains(arch))
        || fields.get("Components").map(String::as_str) != Some(MAIN_COMPONENT)
    {
        return Err(GeneratorError::usage(
            "publish: signed Release architecture or component set does not match the APT contract",
        ));
    }
    let date = parse_release_time(
        fields.get("Date").map(String::as_str).unwrap_or_default(),
        "Date",
    )?;
    let valid_until = parse_release_time(
        fields
            .get("Valid-Until")
            .map(String::as_str)
            .unwrap_or_default(),
        "Valid-Until",
    )?;
    let now = OffsetDateTime::now_utc();
    if allow_expired_current {
        validate_release_freshness_with_expiry(date, valid_until, now, true)?;
    } else {
        validate_release_freshness(date, valid_until, now)?;
    }
    for arch in REQUIRED_ARCHES {
        if !checksums.contains_key(&format!("main/binary-{arch}/Packages")) {
            return Err(GeneratorError::usage(format!(
                "publish: signed Release omits the {arch} Packages checksum"
            )));
        }
    }
    Ok(SignedReleaseMetadata {
        checksums,
        date,
        valid_until,
    })
}

#[derive(Clone, Debug)]
struct SignedPackageEntry {
    size: u64,
    sha256: String,
}

fn parse_rollback_package_stanza(
    text: &str,
    package: &str,
    version: &str,
    arch: &str,
    suite: Suite,
) -> Result<SignedPackageEntry, GeneratorError> {
    let expected_filename = match suite {
        Suite::Stable => format!(
            "pool/main/{}/{}/{}",
            pool_letter(package),
            package,
            canonical_pool_name(package, version, arch)
        ),
        Suite::Preview => format!(
            "pool/preview/main/{}/{}/{}",
            pool_letter(package),
            package,
            canonical_pool_name(package, version, arch)
        ),
    };
    let mut matches = Vec::new();
    for stanza in text.split("\n\n") {
        let mut fields = BTreeMap::new();
        for line in stanza.lines().map(|line| line.trim_end_matches('\r')) {
            if line.starts_with([' ', '\t']) || line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(GeneratorError::usage(
                    "publish: live Packages stanza is malformed",
                ));
            };
            if fields.insert(name, value.trim().to_owned()).is_some() {
                return Err(GeneratorError::usage(
                    "publish: live Packages stanza repeats a field",
                ));
            }
        }
        let is_target = fields.get("Package").map(String::as_str) == Some(package)
            && fields.get("Version").map(String::as_str) == Some(version)
            && fields.get("Architecture").map(String::as_str) == Some(arch);
        if !is_target {
            continue;
        }
        let filename = fields.get("Filename").cloned().unwrap_or_default();
        let size = fields
            .get("Size")
            .and_then(|value| value.parse::<u64>().ok());
        let sha256 = fields.get("SHA256").cloned().unwrap_or_default();
        if filename != expected_filename
            || size.is_none_or(|value| value == 0)
            || !valid_digest(&sha256)
        {
            return Err(GeneratorError::usage(format!(
                "publish: live {arch} rollback stanza has an invalid filename, size, or SHA256"
            )));
        }
        matches.push(SignedPackageEntry {
            size: size.unwrap_or_default(),
            sha256,
        });
    }
    if matches.len() != 1 {
        return Err(GeneratorError::usage(format!(
            "publish: signed live index must carry exactly one {arch} rollback stanza"
        )));
    }
    Ok(matches.remove(0))
}

fn verify_rollback_bytes(
    path: &Path,
    expected: &AuthenticatedRollbackDeb,
    arch: &str,
) -> Result<(), GeneratorError> {
    let size = std::fs::metadata(path)
        .map_err(|error| GeneratorError::io("inspect rollback package", path, &error))?
        .len();
    if size != expected.size || sha256_file(path)? != expected.sha256 {
        return Err(GeneratorError::usage(format!(
            "publish: {arch} rollback deb no longer matches the authenticated live index"
        )));
    }
    Ok(())
}

fn stage_authenticated_rollback(
    inputs: &PublishInputs<'_>,
    rollback: &AuthenticatedRollback,
    pool: &Path,
) -> Result<(), GeneratorError> {
    stage_authenticated_version(inputs, &rollback.version, &rollback.debs, pool)
}

fn stage_authenticated_version(
    inputs: &PublishInputs<'_>,
    expected_version: &str,
    debs: &BTreeMap<String, AuthenticatedRollbackDeb>,
    pool: &Path,
) -> Result<(), GeneratorError> {
    let prev_dir = inputs.prev_dir.ok_or_else(|| {
        GeneratorError::usage("publish: --prev-dir is required for rollback publication")
    })?;
    for arch in REQUIRED_ARCHES {
        let authorized = debs.get(arch).ok_or_else(|| {
            GeneratorError::usage("publish: authenticated rollback pair is incomplete")
        })?;
        let deb = prev_dir.join(&authorized.source_name);
        verify_rollback_bytes(&deb, authorized, arch)?;
        let package = deb_control_field(&deb, "Package", inputs.backend, inputs.path_overlay)?;
        let deb_version = deb_control_field(&deb, "Version", inputs.backend, inputs.path_overlay)?;
        let architecture =
            deb_control_field(&deb, "Architecture", inputs.backend, inputs.path_overlay)?;
        if package != inputs.contract.package
            || deb_version != expected_version
            || architecture != arch
        {
            return Err(GeneratorError::usage(format!(
                "publish: authenticated {arch} rollback package identity changed"
            )));
        }
        stage_package(
            &deb,
            &pool.join(&authorized.pool_name),
            &inputs.contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
        verify_rollback_bytes(&pool.join(&authorized.pool_name), authorized, arch)?;
    }
    Ok(())
}

fn validate_publish_candidate(inputs: &PublishInputs<'_>) -> Result<String, GeneratorError> {
    let version = match inputs.suite {
        Suite::Stable => parse_stable_tag(&inputs.version)?.version,
        Suite::Preview => parse_preview_version(&inputs.version)?.version,
    };
    let expected_debs = REQUIRED_ARCHES
        .iter()
        .map(|arch| match inputs.suite {
            Suite::Stable => format!("{}-{version}-{arch}.deb", inputs.contract.package),
            Suite::Preview => format!(
                "{}-preview-{}-{arch}.deb",
                inputs.contract.package,
                dotted_asset_version(&version)
            ),
        })
        .collect::<BTreeSet<_>>();
    let prefix = format!("{}-", inputs.contract.package);
    let supplied_debs = dir_names(inputs.incoming)?
        .into_iter()
        .filter(|name| name.starts_with(&prefix) && is_deb_file(name))
        .collect::<BTreeSet<_>>();
    if supplied_debs != expected_debs {
        return Err(GeneratorError::usage(
            "publish: incoming deb set does not match the exact candidate version and architectures",
        ));
    }
    for arch in REQUIRED_ARCHES {
        let name = match inputs.suite {
            Suite::Stable => format!("{}-{version}-{arch}.deb", inputs.contract.package),
            Suite::Preview => format!(
                "{}-preview-{}-{arch}.deb",
                inputs.contract.package,
                dotted_asset_version(&version)
            ),
        };
        let path = inputs.incoming.join(&name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect candidate package", &path, &error))?;
        if !metadata.file_type().is_file() {
            return Err(GeneratorError::usage(format!(
                "publish: candidate package is not a regular file: {name}"
            )));
        }
        if deb_control_field(&path, "Package", inputs.backend, inputs.path_overlay)?
            != inputs.contract.package
            || deb_control_field(&path, "Version", inputs.backend, inputs.path_overlay)? != version
            || deb_control_field(&path, "Architecture", inputs.backend, inputs.path_overlay)?
                != arch
        {
            return Err(GeneratorError::usage(format!(
                "publish: candidate {arch} deb identity does not match the typed version"
            )));
        }
    }
    Ok(version)
}

/// Bootstrap is accepted only when every known suite metadata, index, and
/// candidate pool endpoint returns a typed HTTP 404. The candidate version
/// has already been validated by `validate_publish_candidate`.
fn prove_suite_absent(
    inputs: &PublishInputs<'_>,
    candidate_version: &str,
) -> Result<(), GeneratorError> {
    let feed = inputs.contract.feed_url.trim_end_matches('/');
    let mut resources = vec![
        format!("dists/{}/InRelease", inputs.suite.as_str()),
        format!("dists/{}/Release", inputs.suite.as_str()),
        format!("dists/{}/Release.gpg", inputs.suite.as_str()),
        inputs.suite.publication_record_file().to_owned(),
        format!("{}.sig", inputs.suite.publication_record_file()),
        inputs.suite.last_publish_file().to_owned(),
        inputs.suite.channel_state_file().to_owned(),
    ];
    for arch in REQUIRED_ARCHES {
        resources.push(format!(
            "dists/{}/main/binary-{arch}/Packages",
            inputs.suite.as_str()
        ));
        resources.push(format!(
            "dists/{}/main/binary-{arch}/Packages.gz",
            inputs.suite.as_str()
        ));
    }
    let pool_prefix = if inputs.suite == Suite::Preview {
        "pool/preview/"
    } else {
        "pool/"
    };
    let pool_dir = format!(
        "{pool_prefix}main/{}/{}/",
        pool_letter(&inputs.contract.package),
        inputs.contract.package
    );
    resources.push(pool_dir.clone());
    for arch in REQUIRED_ARCHES {
        resources.push(format!(
            "{pool_dir}{}",
            canonical_pool_name(&inputs.contract.package, candidate_version, arch)
        ));
    }

    let sequence = HTTP_PROBE_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = std::env::temp_dir().join(format!(
        "velnor-feed-bootstrap-probe-{}-{sequence}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| GeneratorError::io("create bootstrap absence proof directory", &work, &error))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work)
        .map_err(|error| GeneratorError::io("create bootstrap absence proof directory", &work, &error))?;
    let result = (|| {
        for (index, relative) in resources.iter().enumerate() {
            let output = work.join(index.to_string());
            match probe_https(
                &format!("{feed}/{relative}"),
                &output,
                inputs.path_overlay,
            )? {
                HttpProbeResult::NotFound => {}
                HttpProbeResult::Present => {
                    return Err(GeneratorError::usage(format!(
                        "publish: bootstrap refused because live {relative} exists"
                    )));
                }
            }
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| GeneratorError::io("remove bootstrap absence proof directory", &work, &error));
    result?;
    cleanup?;
    Ok(())
}

/// Publish a suite into the staging tree: deterministic pool, per-arch
/// indexes, signed metadata, publication record. Every refusal below lands
/// before signing, and stable wipes only the validated staging directory.
pub(crate) fn publish_suite(
    inputs: &PublishInputs<'_>,
) -> Result<PublishOutcome, GeneratorError> {
    let candidate_version = validate_publish_candidate(inputs)?;
    if inputs.bootstrap {
        if inputs.prev_dir.is_some() {
            return Err(GeneratorError::usage(
                "publish: --bootstrap is mutually exclusive with --prev-dir",
            ));
        }
    }
    if !sentinel_is_armed(inputs.incoming)? {
        return Err(GeneratorError::usage(
            "publish: refusing — verify has not armed the reprepro sentinel",
        ));
    }
    let strict_rollback = !inputs.bootstrap;
    for tool in ["apt-ftparchive", "gpg"] {
        if !tool_present(tool, inputs.path_overlay) {
            return Err(GeneratorError::usage(format!(
                "publish: {tool} not installed"
            )));
        }
    }
    if !tool_present("curl", inputs.path_overlay) {
        return Err(GeneratorError::usage(
            "publish: curl is required to prove live bootstrap absence or authenticate rollback",
        ));
    }
    if strict_rollback && !tool_present("gpgv", inputs.path_overlay) {
        return Err(GeneratorError::usage(
            "publish: gpgv is required to authenticate the live rollback index",
        ));
    }
    if inputs.bootstrap {
        prove_suite_absent(inputs, &candidate_version)?;
    }
    // The publisher only reads debs, so the portable `ar`+`tar` reader
    // satisfies the gate where `dpkg-deb` is absent — the same fallback the
    // oracle's reader takes, lifted into the capability check.
    let deb_reader = tool_present("dpkg-deb", inputs.path_overlay)
        || (tool_present("ar", inputs.path_overlay) && tool_present("tar", inputs.path_overlay));
    if !deb_reader {
        return Err(GeneratorError::usage(
            "publish: no deb reader installed (dpkg-deb or ar+tar)",
        ));
    }
    let passphrase = inputs.passphrase.as_deref().ok_or_else(|| {
        GeneratorError::usage(format!("publish: {} is unset", inputs.passphrase_env))
    })?;
    if passphrase.is_empty() {
        return Err(GeneratorError::usage(format!(
            "publish: {} is empty",
            inputs.passphrase_env
        )));
    }
    let key_material = inputs
        .key_material
        .as_deref()
        .ok_or_else(|| GeneratorError::usage(format!("publish: {} is unset", inputs.key_env)))?;
    if key_material.is_empty() {
        return Err(GeneratorError::usage(format!(
            "publish: {} is empty",
            inputs.key_env
        )));
    }
    if inputs.suite == Suite::Stable && !inputs.bootstrap && inputs.prev_dir.is_none() {
        return Err(GeneratorError::usage(
            "publish: --prev-dir is required for stable rollback publication",
        ));
    }
    if inputs.suite == Suite::Preview && !inputs.bootstrap && inputs.prev_dir.is_none() {
        return Err(GeneratorError::usage(
            "publish: --prev-dir is required for the preview suite (the retained preview rollback pair; use --bootstrap to initialize the suite)",
        ));
    }
    let Some(staging_name) = inputs.staging.to_str() else {
        return Err(GeneratorError::usage("staging directory is not UTF-8"));
    };
    if !valid_staging_dir(staging_name) {
        return Err(GeneratorError::usage(
            "staging directory must be a relative path without traversal",
        ));
    }
    // Rollback bytes come from the live feed. Before importing a signing key
    // or touching staging, authenticate InRelease -> Release SHA256 -> both
    // Packages files -> the exact package stanza -> each local rollback deb.
    let transaction = StagingTransaction::new(inputs.staging)?;
    let mut refresh_current = false;
    let rollback = match (inputs.suite, inputs.bootstrap) {
        (Suite::Stable, false) => {
            let tag = parse_stable_tag(&inputs.version)?;
            let rollback = authenticate_live_rollback(inputs)?;
            if rollback.version == tag.version {
                if !stable_candidate_matches_live(inputs, &tag, &rollback)? {
                    return Err(GeneratorError::usage(
                        "publish: stable same-version candidate differs from authenticated current bytes or pointer",
                    ));
                }
                if rollback.refresh_required {
                    refresh_current = true;
                } else {
                    return Ok(PublishOutcome::AlreadyPublished);
                }
            } else if cmp_stable_versions(&tag.tag, &format!("v{}", rollback.version))?
                != std::cmp::Ordering::Greater
            {
                return Err(GeneratorError::usage(
                    "publish: stable candidate is older than the authenticated live version",
                ));
            }
            if !refresh_current {
                check_stable_pointer_shape(inputs.previous_pointer)?;
                check_stable_pointer(inputs.previous_pointer, &format!("v{}", rollback.version))?;
            }
            Some(rollback)
        }
        (Suite::Preview, false) => {
            let candidate = parse_preview_version(&inputs.version)?;
            check_preview_pointer(inputs.previous_pointer, false)?;
            let rollback = authenticate_live_rollback(inputs)?;
            match cmp_preview_versions(&candidate.version, &rollback.version)? {
                std::cmp::Ordering::Less => {
                    return Err(GeneratorError::usage(format!(
                        "publish: preview candidate {} is older than authenticated live head {}",
                        candidate.version, rollback.version
                    )));
                }
                std::cmp::Ordering::Equal => {
                    if !preview_candidate_matches_live(inputs, &candidate, &rollback)? {
                        return Err(GeneratorError::usage(
                            "publish: preview same-version candidate differs from authenticated current bytes or manifest",
                        ));
                    }
                    if rollback.refresh_required {
                        refresh_current = true;
                    } else {
                        return Ok(PublishOutcome::AlreadyPublished);
                    }
                }
                std::cmp::Ordering::Greater => {}
            }
            Some(rollback)
        }
        (Suite::Preview, true) => {
            parse_preview_version(&inputs.version)?;
            check_preview_pointer(inputs.previous_pointer, true)?;
            None
        }
        (Suite::Stable, true) => {
            parse_stable_tag(&inputs.version)?;
            require_null_pointer(inputs.previous_pointer, "stable bootstrap")?;
            None
        }
    };
    // The signing key is imported and proven before any mutation or signing:
    // a missing, unimportable, or disagreeing key fails here, never mid-run.
    let homedir = import_signing_key(
        &inputs.contract.signer,
        &inputs.key_env,
        key_material,
        inputs.path_overlay,
    )?;
    let Some(homedir_name) = homedir.to_str() else {
        // The import succeeded, so the agent holds the key: tear down before
        // refusing, like every other exit path from the import flow.
        teardown_signing_homedir(&homedir, inputs.path_overlay);
        return Err(GeneratorError::usage(
            "publish: signing keyring path is not UTF-8",
        ));
    };
    let outcome = match inputs.suite {
        Suite::Stable => publish_stable(
            inputs,
            transaction.path(),
            rollback.as_ref(),
            refresh_current,
            passphrase,
            homedir_name,
        ),
        Suite::Preview => publish_preview(
            inputs,
            transaction.path(),
            rollback.as_ref(),
            refresh_current,
            passphrase,
            homedir_name,
        ),
    };
    // The isolated keyring leaves with the run, success or failure.
    teardown_signing_homedir(&homedir, inputs.path_overlay);
    outcome?;
    transaction.commit()?;
    Ok(PublishOutcome::Published)
}

fn stable_candidate_matches_live(
    inputs: &PublishInputs<'_>,
    tag: &StableTag,
    rollback: &AuthenticatedRollback,
) -> Result<bool, GeneratorError> {
    let candidate_digest = sidecar_digest(&inputs.incoming.join(RECORD_SIDECAR))?;
    let pointer = read_json(inputs.previous_pointer)?;
    if !pointer
        .get("already_published")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || pointer.get("tag").and_then(serde_json::Value::as_str) != Some(tag.tag.as_str())
        || pointer
            .get("source_record_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(candidate_digest.as_str())
    {
        return Ok(false);
    }
    let (Some(record_path), Some(signature_path)) =
        (inputs.published_record, inputs.published_signature)
    else {
        return Ok(false);
    };
    let record_value = read_authenticated_publication_record(
        record_path,
        signature_path,
        &inputs.contract.keyring,
        &inputs.contract.signer,
        inputs.path_overlay,
    )?;
    let record = parse_publication_record(&record_value)?;
    if record.tag != tag.tag
        || record.crate_version != tag.version
        || record.source_record_sha256 != candidate_digest
    {
        return Ok(false);
    }
    for arch in REQUIRED_ARCHES {
        let expected = rollback.debs.get(arch).ok_or_else(|| {
            GeneratorError::usage("publish: authenticated live package pair is incomplete")
        })?;
        let candidate = inputs
            .incoming
            .join(format!("{}-{}-{arch}.deb", inputs.contract.package, tag.version));
        verify_rollback_bytes(&candidate, expected, arch)?;
        if deb_control_field(&candidate, "Package", inputs.backend, inputs.path_overlay)?
            != inputs.contract.package
            || deb_control_field(&candidate, "Version", inputs.backend, inputs.path_overlay)?
                != tag.version
            || deb_control_field(&candidate, "Architecture", inputs.backend, inputs.path_overlay)?
                != arch
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn preview_candidate_matches_live(
    inputs: &PublishInputs<'_>,
    candidate: &PreviewVersion,
    rollback: &AuthenticatedRollback,
) -> Result<bool, GeneratorError> {
    if rollback.current_record.tag != PREVIEW_TAG
        || rollback.current_record.crate_version != candidate.version
        || rollback.current_record.source_record_sha256
            != sha256_file(&inputs.incoming.join(PREVIEW_MANIFEST_FILE))?
        || read_json(inputs.previous_pointer)?.as_str() != Some(PREVIEW_TAG)
    {
        return Ok(false);
    }
    let dotted = dotted_asset_version(&candidate.version);
    for arch in REQUIRED_ARCHES {
        let expected = rollback.debs.get(arch).ok_or_else(|| {
            GeneratorError::usage("publish: authenticated live preview pair is incomplete")
        })?;
        let deb = inputs
            .incoming
            .join(format!("{}-preview-{dotted}-{arch}.deb", inputs.contract.package));
        verify_rollback_bytes(&deb, expected, arch)?;
        if deb_control_field(&deb, "Package", inputs.backend, inputs.path_overlay)?
            != inputs.contract.package
            || deb_control_field(&deb, "Version", inputs.backend, inputs.path_overlay)?
                != candidate.version
            || deb_control_field(&deb, "Architecture", inputs.backend, inputs.path_overlay)?
                != arch
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The pool subdirectory for a package: the first letter, or the first four
/// for `lib*` packages, per the Debian pool convention.
pub(crate) fn pool_letter(package: &str) -> &str {
    if package.len() >= 4 && package.as_bytes()[..3] == *b"lib" {
        &package[..4]
    } else {
        &package[..1]
    }
}

/// The canonical pool filename for a staged deb.
pub(crate) fn canonical_pool_name(package: &str, version: &str, arch: &str) -> String {
    format!("{package}_{version}_{arch}.deb")
}

/// The suite pool root inside the staging tree.
fn pool_root(staging: &Path, suite: Suite, contract: &AptContract) -> PathBuf {
    let mut root = staging.join("pool");
    if suite == Suite::Preview {
        root.push(PREVIEW_SUITE);
    }
    root.join(MAIN_COMPONENT)
        .join(pool_letter(&contract.package))
        .join(&contract.package)
}

/// Stage one deb into the pool under its canonical name. A colliding name
/// with different bytes fails; identical bytes are idempotent.
#[allow(clippy::too_many_arguments)]
fn stage_package(
    deb: &Path,
    destination: &Path,
    contract: &AptContract,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<(String, String), GeneratorError> {
    if deb_control_field(deb, "Package", backend, path_overlay)? != contract.package {
        return Err(GeneratorError::usage(
            "publish: staged package has unexpected name",
        ));
    }
    let version = deb_control_field(deb, "Version", backend, path_overlay)?;
    let arch = deb_control_field(deb, "Architecture", backend, path_overlay)?;
    if !valid_pool_version(&version) {
        return Err(GeneratorError::usage(
            "publish: staged package version is unsafe",
        ));
    }
    if !REQUIRED_ARCHES.contains(&arch.as_str()) {
        return Err(GeneratorError::usage(
            "publish: staged package architecture is unsupported",
        ));
    }
    let expected = destination
        .parent()
        .unwrap_or(destination)
        .join(canonical_pool_name(&contract.package, &version, &arch));
    if expected.is_file() {
        if sha256_file(&expected)? != sha256_file(deb)? {
            return Err(GeneratorError::usage(
                "publish: canonical package identity collides with different bytes",
            ));
        }
    } else {
        if let Some(parent) = expected.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("create", parent, &error))?;
        }
        std::fs::copy(deb, &expected)
            .map_err(|error| GeneratorError::io("stage", &expected, &error))?;
    }
    Ok((version, arch))
}

/// Count the pool debs.
fn pool_deb_count(pool: &Path) -> Result<usize, GeneratorError> {
    Ok(dir_names(pool)?
        .iter()
        .filter(|name| is_deb_file(name))
        .count())
}

/// The versions a `Packages` index retains for `package`, parsed the way the
/// oracle parses them: the `Version` of every stanza whose `Package` matches.
pub(crate) fn packages_versions(text: &str, package: &str) -> BTreeSet<String> {
    let mut versions = BTreeSet::new();
    let mut current: Option<&str> = None;
    for line in text.lines() {
        if line.is_empty() {
            current = None;
        } else if let Some(name) = line.strip_prefix("Package:") {
            current = Some(name.trim());
        } else if let Some(version) = line.strip_prefix("Version:")
            && current == Some(package)
        {
            versions.insert(version.trim().to_owned());
        }
    }
    versions
}

/// The fixed `apt-ftparchive packages` argument vector.
pub(crate) fn apt_packages_argv(arch: &str, pool: &str) -> Vec<String> {
    vec![
        "-a".to_owned(),
        arch.to_owned(),
        "packages".to_owned(),
        pool.to_owned(),
    ]
}

/// The fixed `apt-ftparchive release` argument vector pinning the suite
/// metadata.
pub(crate) fn apt_release_argv(contract: &AptContract, suite: Suite) -> Vec<String> {
    let description = if suite == Suite::Preview {
        format!("{} (preview suite)", contract.description)
    } else {
        contract.description.clone()
    };
    vec![
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Origin={}", contract.origin),
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Label={}", contract.origin),
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Suite={}", suite.as_str()),
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Codename={}", suite.as_str()),
        "-o".to_owned(),
        "APT::FTPArchive::Release::Architectures=amd64 arm64".to_owned(),
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Components={MAIN_COMPONENT}"),
        "-o".to_owned(),
        format!("APT::FTPArchive::Release::Description={description}"),
        "release".to_owned(),
        format!("dists/{}", suite.as_str()),
    ]
}

/// The fixed `gpg --detach-sign` argument vector.
fn gpg_detach_argv(
    signer: &str,
    homedir: &str,
    output: &str,
    input: &str,
    armor: bool,
) -> Vec<String> {
    let mut argv = vec![
        "--batch".to_owned(),
        "--homedir".to_owned(),
        homedir.to_owned(),
        "--yes".to_owned(),
        "--pinentry-mode".to_owned(),
        "loopback".to_owned(),
        "--passphrase-fd".to_owned(),
        "0".to_owned(),
        "--local-user".to_owned(),
        signer.to_owned(),
    ];
    if armor {
        argv.push("--armor".to_owned());
    }
    argv.push("--output".to_owned());
    argv.push(output.to_owned());
    argv.push("--detach-sign".to_owned());
    argv.push(input.to_owned());
    argv
}

/// The fixed `gpg --clearsign` argument vector.
fn gpg_clearsign_argv(signer: &str, homedir: &str, output: &str, input: &str) -> Vec<String> {
    vec![
        "--batch".to_owned(),
        "--homedir".to_owned(),
        homedir.to_owned(),
        "--yes".to_owned(),
        "--pinentry-mode".to_owned(),
        "loopback".to_owned(),
        "--passphrase-fd".to_owned(),
        "0".to_owned(),
        "--local-user".to_owned(),
        signer.to_owned(),
        "--output".to_owned(),
        output.to_owned(),
        "--clearsign".to_owned(),
        input.to_owned(),
    ]
}

/// Run a fixed tool with the staging tree as its working directory.
fn run_in(
    staging: &Path,
    program: &str,
    args: &[String],
    stdin_bytes: Option<&[u8]>,
    path_overlay: Option<&Path>,
) -> Result<Vec<u8>, GeneratorError> {
    let mut command = Command::new(program);
    command.current_dir(staging);
    command.args(args);
    if let Some(dir) = path_overlay {
        let overlay = dir.as_os_str();
        let path = std::env::var_os("PATH").map_or_else(
            || overlay.to_owned(),
            |existing| {
                let mut joined = overlay.to_owned();
                joined.push(":");
                joined.push(existing);
                joined
            },
        );
        command.env("PATH", path);
    }
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| GeneratorError::usage(format!("{program} is not installed or cannot run")))?;
    if let Some(bytes) = stdin_bytes {
        child
            .stdin
            .as_mut()
            .ok_or_else(|| GeneratorError::usage(format!("{program} takes no standard input")))?
            .write_all(bytes)
            .map_err(|_| GeneratorError::usage(format!("{program} refused standard input")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| GeneratorError::usage(format!("{program} did not finish")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "{program} failed with status {}",
            output.status
        )));
    }
    Ok(output.stdout)
}

/// Process-local sequence distinguishing isolated signing keyrings created
/// within one publisher process.
static SIGNING_KEYRING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Create the isolated keyring directory the publisher imports the signing
/// key into. The directory lives outside the staging tree — key material
/// must never enter the published bytes — and starts empty: stale state
/// from a crashed run is wiped before creation.
fn create_signing_homedir() -> Result<PathBuf, GeneratorError> {
    let seq = SIGNING_KEYRING_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("velnor-feed-signing-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|error| GeneratorError::io("create", &dir, &error))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&dir).map_err(|error| GeneratorError::io("create", &dir, &error))?;
    }
    Ok(dir)
}

/// The first secret-key fingerprint a `gpg --with-colons --list-secret-keys`
/// listing carries: the tenth field of the first `fpr:` record.
fn secret_key_fingerprint(listing: &str) -> Option<String> {
    for line in listing.lines() {
        let mut fields = line.split(':');
        if fields.next() != Some("fpr") {
            continue;
        }
        if let Some(fingerprint) = fields.nth(8)
            && !fingerprint.is_empty()
        {
            return Some(fingerprint.to_owned());
        }
    }
    None
}

/// Import the signing-key material into the isolated keyring and prove the
/// private key agrees with the pinned publisher fingerprint — the same
/// identity the verify step read from the committed public keyring.
fn agree_imported_key(
    homedir_name: &str,
    signer: &str,
    key_env: &str,
    key_material: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    run_fixed(
        "gpg",
        &[
            "--batch".to_owned(),
            "--homedir".to_owned(),
            homedir_name.to_owned(),
            "--import".to_owned(),
        ],
        Some(key_material.as_bytes()),
        path_overlay,
    )
    .map_err(|_| {
        GeneratorError::usage(format!(
            "publish: {key_env} did not import as a signing key"
        ))
    })?;
    let listing = run_fixed(
        "gpg",
        &[
            "--batch".to_owned(),
            "--homedir".to_owned(),
            homedir_name.to_owned(),
            "--with-colons".to_owned(),
            "--list-secret-keys".to_owned(),
        ],
        None,
        path_overlay,
    )
    .map_err(|_| {
        GeneratorError::usage(format!("publish: {key_env} carries no listable secret key"))
    })?;
    let imported = secret_key_fingerprint(&String::from_utf8_lossy(&listing)).ok_or_else(|| {
        GeneratorError::usage(format!(
            "publish: {key_env} carries no secret-key fingerprint"
        ))
    })?;
    if !fingerprints_match(&imported, signer) {
        return Err(GeneratorError::usage(
            "publish: imported signing key disagrees with the pinned publisher key",
        ));
    }
    Ok(())
}

/// Tear down an isolated signing keyring: shut down the agent it spawned,
/// then wipe the directory — best-effort, mirroring the oracle's exit trap.
/// A lingering unlocked agent must not outlive the run, and key material
/// must not linger in the temp tree either. Every exit path from the
/// import/agreement flow funnels through here, success or refusal alike.
fn teardown_signing_homedir(homedir: &Path, path_overlay: Option<&Path>) {
    let _ = run_fixed(
        "gpgconf",
        &[
            "--homedir".to_owned(),
            homedir.to_string_lossy().into_owned(),
            "--kill".to_owned(),
            "gpg-agent".to_owned(),
        ],
        None,
        path_overlay,
    );
    let _ = std::fs::remove_dir_all(homedir);
}

/// Import the signing-key material into an isolated keyring and prove the
/// private key agrees with the pinned publisher fingerprint. A failed import
/// tears down the keyring it created: no agent survives the refusal, and key
/// material never lingers in the temp tree. Diagnostics name the secret,
/// never the material.
fn import_signing_key(
    signer: &str,
    key_env: &str,
    key_material: &str,
    path_overlay: Option<&Path>,
) -> Result<PathBuf, GeneratorError> {
    let homedir = create_signing_homedir()?;
    let Some(homedir_name) = homedir.to_str() else {
        teardown_signing_homedir(&homedir, path_overlay);
        return Err(GeneratorError::usage(
            "publish: signing keyring path is not UTF-8",
        ));
    };
    let outcome = agree_imported_key(homedir_name, signer, key_env, key_material, path_overlay);
    if outcome.is_err() {
        teardown_signing_homedir(&homedir, path_overlay);
    }
    outcome.map(|()| homedir)
}

/// Unlock and cache the exact signing key with one discarded signature after
/// validation and before signing, so the publisher fails on a locked key
/// before it signs — but never masks an input defect with a key error.
fn prime_signer_agent(
    signer: &str,
    homedir: &str,
    passphrase: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    run_fixed(
        "gpg",
        &[
            "--batch".to_owned(),
            "--homedir".to_owned(),
            homedir.to_owned(),
            "--yes".to_owned(),
            "--pinentry-mode".to_owned(),
            "loopback".to_owned(),
            "--passphrase-fd".to_owned(),
            "0".to_owned(),
            "--local-user".to_owned(),
            signer.to_owned(),
            "--output".to_owned(),
            "/dev/null".to_owned(),
            "--detach-sign".to_owned(),
            "/dev/null".to_owned(),
        ],
        Some(passphrase.as_bytes()),
        path_overlay,
    )?;
    Ok(())
}

/// Copy the keyring into the staging tree when the repository carries one.
fn stage_keyring(staging: &Path, contract: &AptContract) -> Result<(), GeneratorError> {
    let keyring = Path::new(&contract.keyring);
    if !keyring.is_file() {
        return Ok(());
    }
    let name = keyring
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GeneratorError::usage("keyring filename is not UTF-8"))?;
    std::fs::copy(keyring, staging.join(name))
        .map_err(|error| GeneratorError::io("stage the keyring", staging, &error))?;
    Ok(())
}

/// Build and check the per-arch indexes for a strict (candidate + rollback)
/// publication, returning the shared rollback version.
#[allow(clippy::too_many_arguments)]
fn build_strict_indexes(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    candidate: &str,
    retention: Retention,
    path_overlay: Option<&Path>,
) -> Result<String, GeneratorError> {
    let pool = if suite == Suite::Preview {
        "pool/preview".to_owned()
    } else {
        "pool".to_owned()
    };
    let mut rollback: Option<String> = None;
    for arch in REQUIRED_ARCHES {
        let relative = format!("dists/{}/main/binary-{arch}/Packages", suite.as_str());
        let packages = staging.join(&relative);
        if let Some(parent) = packages.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("create", parent, &error))?;
        }
        let stdout = run_in(
            staging,
            "apt-ftparchive",
            &apt_packages_argv(arch, &pool),
            None,
            path_overlay,
        )?;
        std::fs::write(&packages, &stdout)
            .map_err(|error| GeneratorError::io("write", &packages, &error))?;
        let text = String::from_utf8_lossy(&stdout);
        let versions = packages_versions(&text, &contract.package);
        if versions.len() != retention.indexed_versions() {
            let observed: Vec<&str> = versions.iter().map(String::as_str).collect();
            return Err(GeneratorError::usage(format!(
                "publish: {arch} index must retain exactly candidate plus rollback version (observed: {})",
                observed.join(",")
            )));
        }
        if !versions.contains(candidate) {
            return Err(GeneratorError::usage(format!(
                "publish: {arch} index lacks candidate version {candidate}"
            )));
        }
        let rest = versions
            .iter()
            .filter(|version| *version != candidate)
            .cloned()
            .collect::<Vec<_>>();
        let arch_rollback = highest_version(suite, rest)?;
        match &rollback {
            None => rollback = Some(arch_rollback.clone()),
            Some(first) if first == &arch_rollback => {}
            _ => {
                return Err(GeneratorError::usage(
                    "publish: architecture rollback versions differ",
                ));
            }
        }
        let gz = run_in(
            staging,
            "gzip",
            &[
                "-n".to_owned(),
                "-9".to_owned(),
                "-c".to_owned(),
                relative.clone(),
            ],
            None,
            path_overlay,
        )?;
        let gz_path = staging.join(format!("{relative}.gz"));
        std::fs::write(&gz_path, gz)
            .map_err(|error| GeneratorError::io("write", &gz_path, &error))?;
    }
    rollback.ok_or_else(|| GeneratorError::usage("publish: rollback version is empty"))
}

/// Publish stable into the shared tree while preserving preview content.
fn publish_stable(
    inputs: &PublishInputs<'_>,
    staging: &Path,
    authenticated_rollback: Option<&AuthenticatedRollback>,
    refresh_current: bool,
    passphrase: &str,
    homedir: &str,
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    let tag = parse_stable_tag(&inputs.version)?;
    if inputs.bootstrap {
        require_null_pointer(inputs.previous_pointer, "stable bootstrap")?;
    } else if refresh_current {
        let rollback = authenticated_rollback.ok_or_else(|| {
            GeneratorError::usage("publish: stable refresh lacks authenticated current state")
        })?;
        if !validate_stable_pointer_record(
            inputs.previous_pointer,
            &rollback.current_record,
            &tag.tag,
        )? {
            return Err(GeneratorError::usage(
                "publish: stable refresh pointer does not authenticate the exact current record",
            ));
        }
    } else {
        check_stable_pointer_shape(inputs.previous_pointer)?;
    }
    let selected_rollback = match authenticated_rollback {
        None => None,
        Some(rollback) if refresh_current => match &rollback.current_record.previous {
            serde_json::Value::Null => None,
            serde_json::Value::Object(previous) => {
                let previous = serde_json::Value::Object(previous.clone());
                let version = parse_stable_tag(field(&previous, "tag")?)?.version;
                let debs = rollback.indexed_pairs.get(&version).ok_or_else(|| {
                    GeneratorError::usage(
                        "publish: signed stable previous version lacks authenticated rollback bytes",
                    )
                })?;
                Some((version, debs))
            }
            _ => {
                return Err(GeneratorError::usage(
                    "publish: signed stable previous pointer is malformed",
                ));
            }
        },
        Some(rollback) => Some((rollback.version.clone(), &rollback.debs)),
    };
    clear_suite_staging(staging, Suite::Stable, contract)?;
    let pool = pool_root(staging, Suite::Stable, contract);
    std::fs::create_dir_all(staging.join("conf"))
        .map_err(|error| GeneratorError::io("create", staging, &error))?;
    ensure_suite_stanza(staging, contract, Suite::Stable)?;
    stage_keyring(staging, contract)?;
    if let Some((version, debs)) = &selected_rollback {
        stage_authenticated_version(inputs, version, debs, &pool)?;
    }
    // Stage the candidate debs by exact name: verification already proved
    // the incoming directory holds exactly this pair.
    for arch in REQUIRED_ARCHES {
        let name = format!("{}-{}-{arch}.deb", contract.package, tag.version);
        let deb = inputs.incoming.join(&name);
        require_file(&deb)?;
        stage_package(
            &deb,
            &pool.join(&name),
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
    }
    let expected_pool_debs = if selected_rollback.is_some() {
        contract.retention.pool_debs()
    } else {
        REQUIRED_ARCHES.len()
    };
    if pool_deb_count(&pool)? != expected_pool_debs {
        return Err(GeneratorError::usage(
            "publish: stable pool does not contain the expected candidate/rollback pair",
        ));
    }
    if let Some((expected_version, debs)) = &selected_rollback {
        let indexed = build_strict_indexes(
            staging,
            Suite::Stable,
            contract,
            &tag.version,
            contract.retention,
            inputs.path_overlay,
        )?;
        if indexed != *expected_version {
            return Err(GeneratorError::usage(
                "publish: built stable index rollback differs from the authenticated signed previous version",
            ));
        }
        for arch in REQUIRED_ARCHES {
            let deb = pool.join(canonical_pool_name(&contract.package, &indexed, arch));
            verify_rollback_bytes(
                &deb,
                debs.get(arch).ok_or_else(|| {
                    GeneratorError::usage("publish: authenticated rollback pair is incomplete")
                })?,
                arch,
            )?;
        }
        if !refresh_current {
            check_stable_pointer(inputs.previous_pointer, &format!("v{indexed}"))?;
        }
    } else {
        check_candidate_pool(&pool, contract, &tag.version, Suite::Stable)?;
        build_bootstrap_indexes(
            staging,
            Suite::Stable,
            contract,
            &tag.version,
            inputs.path_overlay,
        )?;
    }
    prime_signer_agent(&contract.signer, homedir, passphrase, inputs.path_overlay)?;
    let previous = if refresh_current {
        authenticated_rollback
            .ok_or_else(|| GeneratorError::usage("publish: stable refresh state is missing"))?
            .current_record
            .previous
            .clone()
    } else {
        read_json(inputs.previous_pointer)?
    };
    sign_suite_release(
        staging,
        Suite::Stable,
        contract,
        authenticated_rollback.map(|rollback| rollback.release_date),
        passphrase,
        homedir,
        inputs.path_overlay,
    )?;
    let source_record = sidecar_digest(&inputs.incoming.join(RECORD_SIDECAR))?;
    emit_publication_record(
        staging,
        Suite::Stable,
        contract,
        &tag.tag,
        &tag.version,
        &source_record,
        &previous,
        inputs.path_overlay,
        passphrase,
        homedir,
    )?;
    std::fs::write(staging.join("last-publish"), format!("{}\n", tag.tag))
        .map_err(|error| GeneratorError::io("write", staging, &error))?;
    Ok(())
}

/// Publish the preview suite into the shared tree without wiping: only the
/// preview pool, indexes, and metadata are written.
fn publish_preview(
    inputs: &PublishInputs<'_>,
    staging: &Path,
    authenticated_rollback: Option<&AuthenticatedRollback>,
    refresh_current: bool,
    passphrase: &str,
    homedir: &str,
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    let parsed = parse_preview_version(&inputs.version)?;
    if !inputs.bootstrap && inputs.prev_dir.is_none() {
        return Err(GeneratorError::usage(
            "publish: --prev-dir is required for the preview suite (the retained preview rollback pair; use --bootstrap to initialize the suite)",
        ));
    }
    // The preview pointer needs no computed values, so it is rejected before
    // any mutation or signing.
    check_preview_pointer(inputs.previous_pointer, inputs.bootstrap)?;
    let pool = pool_root(staging, Suite::Preview, contract);
    std::fs::create_dir_all(staging.join("conf"))
        .map_err(|error| GeneratorError::io("create", staging, &error))?;
    ensure_suite_stanza(staging, contract, Suite::Preview)?;
    stage_keyring(staging, contract)?;
    if inputs.bootstrap {
        require_empty_preview_pool(&pool)?;
        stage_preview_candidates(inputs, &parsed, &pool)?;
        check_candidate_pool(&pool, contract, &parsed.version, Suite::Preview)?;
        build_bootstrap_indexes(
            staging,
            Suite::Preview,
            contract,
            &parsed.version,
            inputs.path_overlay,
        )?;
    } else {
        let rollback = authenticated_rollback.ok_or_else(|| {
            GeneratorError::usage("publish: authenticated preview rollback pair is missing")
        })?;
        let selected_rollback = if refresh_current {
            match &rollback.current_record.previous {
                serde_json::Value::Null => None,
                serde_json::Value::String(marker) if marker == PREVIEW_TAG => {
                    let versions = rollback
                        .indexed_pairs
                        .keys()
                        .filter(|version| **version != parsed.version)
                        .cloned()
                        .collect::<Vec<_>>();
                    let version = highest_version(Suite::Preview, versions)?;
                    let pair = rollback.indexed_pairs.get(&version).ok_or_else(|| {
                        GeneratorError::usage(
                            "publish: authenticated preview previous marker lacks rollback bytes",
                        )
                    })?;
                    Some((version, pair))
                }
                _ => {
                    return Err(GeneratorError::usage(
                        "publish: signed preview previous marker is malformed",
                    ));
                }
            }
        } else {
            Some((rollback.version.clone(), &rollback.debs))
        };
        prune_preview_pool(inputs, rollback, &pool)?;
        if let Some((version, debs)) = &selected_rollback {
            stage_authenticated_version(inputs, version, debs, &pool)?;
        }
        stage_preview_candidates(inputs, &parsed, &pool)?;
        let expected_pool_debs = if selected_rollback.is_some() {
            contract.retention.pool_debs()
        } else {
            REQUIRED_ARCHES.len()
        };
        if pool_deb_count(&pool)? != expected_pool_debs {
            return Err(GeneratorError::usage(
                "publish: deterministic preview pool does not contain exactly the candidate and authenticated rollback pairs",
            ));
        }
        if let Some((version, debs)) = &selected_rollback {
            let indexed_rollback = build_strict_indexes(
                staging,
                Suite::Preview,
                contract,
                &parsed.version,
                contract.retention,
                inputs.path_overlay,
            )?;
            if indexed_rollback != *version {
                return Err(GeneratorError::usage(
                    "publish: built preview index rollback differs from the authenticated selected version",
                ));
            }
            for arch in REQUIRED_ARCHES {
                let deb = pool.join(canonical_pool_name(
                    &contract.package,
                    version,
                    arch,
                ));
                verify_rollback_bytes(
                    &deb,
                    debs.get(arch).ok_or_else(|| {
                        GeneratorError::usage("publish: authenticated rollback pair is incomplete")
                    })?,
                    arch,
                )?;
            }
        } else {
            check_candidate_pool(&pool, contract, &parsed.version, Suite::Preview)?;
            build_bootstrap_indexes(
                staging,
                Suite::Preview,
                contract,
                &parsed.version,
                inputs.path_overlay,
            )?;
        }
    }
    prime_signer_agent(&contract.signer, homedir, passphrase, inputs.path_overlay)?;
    sign_suite_release(
        staging,
        Suite::Preview,
        contract,
        authenticated_rollback.map(|rollback| rollback.release_date),
        passphrase,
        homedir,
        inputs.path_overlay,
    )?;
    let source_manifest = sha256_file(&inputs.incoming.join(PREVIEW_MANIFEST_FILE))?;
    let previous = if refresh_current {
        authenticated_rollback
            .ok_or_else(|| GeneratorError::usage("publish: preview refresh state is missing"))?
            .current_record
            .previous
            .clone()
    } else {
        read_json(inputs.previous_pointer)?
    };
    emit_publication_record(
        staging,
        Suite::Preview,
        contract,
        PREVIEW_TAG,
        &parsed.version,
        &source_manifest,
        &previous,
        inputs.path_overlay,
        passphrase,
        homedir,
    )?;
    std::fs::write(
        staging.join(Suite::Preview.last_publish_file()),
        format!("{}\n", parsed.version),
    )
    .map_err(|error| GeneratorError::io("write", staging, &error))?;
    Ok(())
}

/// Remove only authenticated package files from a prior preview pool. A
/// partial publication may leave three indexed versions in staging; each old
/// pair must still match the signed live indexes before it is pruned.
fn prune_preview_pool(
    inputs: &PublishInputs<'_>,
    rollback: &AuthenticatedRollback,
    pool: &Path,
) -> Result<(), GeneratorError> {
    let metadata = match std::fs::symlink_metadata(pool) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(GeneratorError::io("inspect preview pool", pool, &error)),
    };
    if !metadata.file_type().is_dir() {
        return Err(GeneratorError::usage(
            "publish: preview pool is not a real directory",
        ));
    }
    let mut seen = BTreeMap::<String, BTreeSet<String>>::new();
    let mut files = Vec::new();
    for name in dir_names(pool)? {
        if !is_deb_file(&name) {
            return Err(GeneratorError::usage(format!(
                "publish: preview pool contains an unexpected entry: {name}"
            )));
        }
        let path = pool.join(&name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect preview pool package", &path, &error))?;
        if !metadata.file_type().is_file() {
            return Err(GeneratorError::usage(
                "publish: preview pool package is not a regular file",
            ));
        }
        let (version, arch) = preview_pool_identity(&name, &inputs.contract.package)?;
        if !rollback.indexed_pairs.contains_key(&version) {
            return Err(GeneratorError::usage(format!(
                "publish: preview pool contains unauthenticated orphan version {version}"
            )));
        }
        let authorized = rollback
            .indexed_pairs
            .get(&version)
            .and_then(|pair| pair.get(&arch))
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "publish: preview pool version {version} lacks an authenticated {arch} package"
                ))
            })?;
        if authorized.pool_name != name {
            return Err(GeneratorError::usage(
                "publish: preview pool filename differs from its authenticated package identity",
            ));
        }
        verify_rollback_bytes(&path, authorized, &arch)?;
        seen.entry(version).or_default().insert(arch);
        files.push(path);
    }
    for (version, arches) in seen {
        if arches.len() != REQUIRED_ARCHES.len()
            || REQUIRED_ARCHES.iter().any(|arch| !arches.contains(*arch))
        {
            return Err(GeneratorError::usage(format!(
                "publish: preview pool contains an incomplete authenticated pair for {version}"
            )));
        }
    }
    for path in files {
        std::fs::remove_file(&path)
            .map_err(|error| GeneratorError::io("prune authenticated preview package", &path, &error))?;
    }
    Ok(())
}

fn preview_pool_identity(name: &str, package: &str) -> Result<(String, String), GeneratorError> {
    let prefix = format!("{package}_");
    let stem = name.strip_prefix(&prefix).ok_or_else(|| {
        GeneratorError::usage("publish: preview pool package has an unexpected filename")
    })?;
    for arch in REQUIRED_ARCHES {
        let suffix = format!("_{arch}.deb");
        if let Some(version) = stem.strip_suffix(&suffix) {
            parse_preview_version(version)?;
            if canonical_pool_name(package, version, arch) != name {
                break;
            }
            return Ok((version.to_owned(), arch.to_owned()));
        }
    }
    Err(GeneratorError::usage(
        "publish: preview pool package filename is malformed",
    ))
}

fn require_empty_preview_pool(pool: &Path) -> Result<(), GeneratorError> {
    let metadata = match std::fs::symlink_metadata(pool) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(GeneratorError::io("inspect preview pool", pool, &error)),
    };
    if !metadata.file_type().is_dir() {
        return Err(GeneratorError::usage(
            "publish: preview pool is not a real directory",
        ));
    }
    if !dir_names(pool)?.is_empty() {
        return Err(GeneratorError::usage(
            "publish: preview bootstrap refuses to replace an existing pool",
        ));
    }
    Ok(())
}

/// Stage the preview candidate pair after checking each deb carries the
/// candidate control version.
fn stage_preview_candidates(
    inputs: &PublishInputs<'_>,
    parsed: &PreviewVersion,
    pool: &Path,
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    for arch in REQUIRED_ARCHES {
        let dotted = dotted_asset_version(&parsed.version);
        let name = format!("{}-preview-{dotted}-{arch}.deb", contract.package);
        let deb = inputs.incoming.join(&name);
        require_file(&deb)?;
        if deb_control_field(&deb, "Version", inputs.backend, inputs.path_overlay)?
            != parsed.version
        {
            return Err(GeneratorError::usage(format!(
                "publish: candidate deb Version != preview candidate version {}",
                parsed.version
            )));
        }
        stage_package(
            &deb,
            &pool.join(&name),
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
    }
    Ok(())
}

/// Check a bootstrap pool holds exactly the freshly staged candidate pair:
/// bootstrap refuses to run over an existing pool.
fn check_candidate_pool(
    pool: &Path,
    contract: &AptContract,
    candidate: &str,
    suite: Suite,
) -> Result<(), GeneratorError> {
    for arch in REQUIRED_ARCHES {
        let expected = pool.join(canonical_pool_name(&contract.package, candidate, arch));
        if !expected.is_file() {
            return Err(GeneratorError::usage(format!(
                "publish: bootstrap must stage the complete candidate pair (missing {})",
                expected
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?")
            )));
        }
    }
    if pool_deb_count(pool)? != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(format!(
            "publish: {} bootstrap refuses to retain an unexpected pool package",
            suite.as_str()
        )));
    }
    Ok(())
}

/// Build and check the per-arch indexes for a first publication: exactly the
/// candidate version in each index.
fn build_bootstrap_indexes(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    candidate: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let pool = if suite == Suite::Preview {
        "pool/preview"
    } else {
        "pool"
    };
    for arch in REQUIRED_ARCHES {
        let relative = format!("dists/{}/main/binary-{arch}/Packages", suite.as_str());
        let packages = staging.join(&relative);
        if let Some(parent) = packages.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("create", parent, &error))?;
        }
        let stdout = run_in(
            staging,
            "apt-ftparchive",
            &apt_packages_argv(arch, pool),
            None,
            path_overlay,
        )?;
        std::fs::write(&packages, &stdout)
            .map_err(|error| GeneratorError::io("write", &packages, &error))?;
        let text = String::from_utf8_lossy(&stdout);
        let versions = packages_versions(&text, &contract.package);
        if versions.len() != 1 || !versions.contains(candidate) {
            let observed: Vec<&str> = versions.iter().map(String::as_str).collect();
            return Err(GeneratorError::usage(format!(
                "publish: {arch} {} bootstrap index must retain exactly the candidate version (observed: {})",
                suite.as_str(),
                observed.join(",")
            )));
        }
        let gz = run_in(
            staging,
            "gzip",
            &[
                "-n".to_owned(),
                "-9".to_owned(),
                "-c".to_owned(),
                relative.clone(),
            ],
            None,
            path_overlay,
        )?;
        let gz_path = staging.join(format!("{relative}.gz"));
        std::fs::write(&gz_path, gz)
            .map_err(|error| GeneratorError::io("write", &gz_path, &error))?;
    }
    Ok(())
}

/// Replace one suite stanza in `conf/distributions`, retaining its peer suite.
fn ensure_suite_stanza(
    staging: &Path,
    contract: &AptContract,
    suite: Suite,
) -> Result<(), GeneratorError> {
    let path = staging.join("conf/distributions");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut stanzas = existing
        .split("\n\n")
        .filter(|stanza| {
            let codename = match suite {
                Suite::Stable => "Codename: stable",
                Suite::Preview => "Codename: preview",
            };
            !stanza.lines().any(|line| line == codename)
        })
        .filter(|stanza| !stanza.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let description = if suite == Suite::Preview {
        format!("{} (preview suite)", contract.description)
    } else {
        contract.description.clone()
    };
    stanzas.push(format!(
        "Origin: {0}\nLabel: {0}\nSuite: {1}\nCodename: {1}\nArchitectures: amd64 arm64\nComponents: main\nDescription: {2}\nSignWith: {3}",
        contract.origin,
        suite.as_str(),
        description,
        contract.signer
    ));
    let mut text = stanzas.join("\n\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|error| GeneratorError::io("write", &path, &error))?;
    Ok(())
}

fn clear_suite_staging(staging: &Path, suite: Suite, contract: &AptContract) -> Result<(), GeneratorError> {
    let pool = pool_root(staging, suite, contract);
    let dists = staging.join(format!("dists/{}", suite.as_str()));
    for path in [pool, dists] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                std::fs::remove_dir_all(&path)
                    .map_err(|error| GeneratorError::io("clear suite staging path", &path, &error))?;
            }
            Ok(_) => {
                return Err(GeneratorError::usage(format!(
                    "suite staging path is not a real directory: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(GeneratorError::io("inspect suite staging path", &path, &error)),
        }
    }
    for name in [
        suite.last_publish_file().to_owned(),
        suite.publication_record_file().to_owned(),
        format!("{}.sig", suite.publication_record_file()),
        suite.channel_state_file().to_owned(),
    ] {
        let path = staging.join(&name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&path)
                    .map_err(|error| GeneratorError::io("clear suite staging file", &path, &error))?;
            }
            Ok(_) => {
                return Err(GeneratorError::usage(format!(
                    "suite staging file is not a regular file: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(GeneratorError::io("inspect suite staging file", &path, &error)),
        }
    }
    Ok(())
}

/// Build the suite `Release` file and sign `Release.gpg` plus `InRelease`.
fn sign_suite_release(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    previous_date: Option<OffsetDateTime>,
    passphrase: &str,
    homedir: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let dir = staging.join(format!("dists/{}", suite.as_str()));
    for name in ["Release", "Release.gpg", "InRelease"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let stdout = run_in(
        staging,
        "apt-ftparchive",
        &apt_release_argv(contract, suite),
        None,
        path_overlay,
    )?;
    let generated = String::from_utf8(stdout)
        .map_err(|_| GeneratorError::usage("apt-ftparchive Release output is not UTF-8"))?;
    let release = stamp_release_freshness(&generated, previous_date)?;
    let release_metadata = parse_signed_release(&release, suite, contract)?;
    if previous_date.is_some_and(|previous| release_metadata.date <= previous) {
        return Err(GeneratorError::usage(
            "publish: generated Release Date did not advance beyond live signed state",
        ));
    }
    std::fs::write(dir.join("Release"), release)
        .map_err(|error| GeneratorError::io("write", &dir, &error))?;
    // Argument paths are staging-relative: the tool runs with the staging
    // tree as its working directory.
    let dists = format!("dists/{}", suite.as_str());
    run_in(
        staging,
        "gpg",
        &gpg_detach_argv(
            &contract.signer,
            homedir,
            &format!("{dists}/Release.gpg"),
            &format!("{dists}/Release"),
            true,
        ),
        Some(passphrase.as_bytes()),
        path_overlay,
    )?;
    run_in(
        staging,
        "gpg",
        &gpg_clearsign_argv(
            &contract.signer,
            homedir,
            &format!("{dists}/InRelease"),
            &format!("{dists}/Release"),
        ),
        Some(passphrase.as_bytes()),
        path_overlay,
    )?;
    Ok(())
}

/// Check the stable previous pointer shape: the final schema only — an object
/// with exactly `tag` and a 64-hex `source_record_sha256`. Shape needs no
/// computed values, so the publisher runs it before any mutation or signing.
/// The legacy string bridge is gone: breaking changes are preferred over
/// compat branches.
fn check_stable_pointer_shape(path: &Path) -> Result<(), GeneratorError> {
    let pointer = read_json(path)?;
    let object = pointer.as_object().ok_or_else(|| {
        GeneratorError::usage("publish: stable previous pointer must be an object")
    })?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["source_record_sha256", "tag"] {
        return Err(GeneratorError::usage(
            "publish: coherent previous pointer is malformed",
        ));
    }
    if !valid_digest(field(&pointer, "source_record_sha256")?) {
        return Err(GeneratorError::usage(
            "publish: coherent previous pointer is malformed",
        ));
    }
    Ok(())
}

fn require_null_pointer(path: &Path, context: &str) -> Result<(), GeneratorError> {
    if !read_json(path)?.is_null() {
        return Err(GeneratorError::usage(format!(
            "publish: {context} previous pointer must be JSON null"
        )));
    }
    Ok(())
}

/// Check the stable previous pointer names the retained rollback tag. The
/// tag agreement needs the rollback only the strict index build computes, so
/// the publisher runs it right after indexing but still before any signing.
fn check_stable_pointer(path: &Path, rollback_tag: &str) -> Result<(), GeneratorError> {
    check_stable_pointer_shape(path)?;
    let pointer = read_json(path)?;
    if field(&pointer, "tag")? != rollback_tag {
        return Err(GeneratorError::usage(
            "publish: previous pointer disagrees with retained rollback version",
        ));
    }
    Ok(())
}

/// Check the preview previous pointer: the JSON string `"preview"` once a
/// rollback pair is retained, JSON null for a bootstrapped suite.
fn check_preview_pointer(path: &Path, bootstrap: bool) -> Result<(), GeneratorError> {
    let pointer = read_json(path)?;
    if bootstrap {
        if !pointer.is_null() {
            return Err(GeneratorError::usage(
                "publish: bootstrap previous pointer must be JSON null",
            ));
        }
    } else if pointer.as_str() != Some(PREVIEW_TAG) {
        return Err(GeneratorError::usage(
            "publish: preview previous pointer must be the JSON string \"preview\"",
        ));
    }
    Ok(())
}

/// Derive the stable previous pointer from a published publication record,
/// implementing the `publication-previous.jq` rules in typed form: when the
/// published record already identifies the prior tag, its own checksum is
/// the pointer; when it identifies the candidate, the candidate bytes must
/// match the immutable source release and the pointer is its recorded
/// rollback.
pub(crate) fn derive_previous_pointer(
    published: &serde_json::Value,
    prior_tag: &str,
    candidate_tag: &str,
    candidate_sha: &str,
) -> Result<serde_json::Value, GeneratorError> {
    if !valid_digest(candidate_sha) {
        return Err(GeneratorError::usage(
            "candidate source-record digest is not 64 lowercase hex",
        ));
    }
    let record = parse_publication_record(published)?;
    if record.suite.is_some() {
        return Err(GeneratorError::usage(
            "stable previous pointer requires a stable publication record",
        ));
    }
    let published_tag = parse_stable_tag(&record.tag)?;
    if published_tag.version != record.crate_version {
        return Err(GeneratorError::usage(
            "stable publication record tag and crate version disagree",
        ));
    }
    if record.tag == candidate_tag {
        if record.source_record_sha256 != candidate_sha {
            return Err(GeneratorError::usage(
                "published candidate differs from immutable source release",
            ));
        }
        return Ok(serde_json::json!({
            "already_published": true,
            "tag": record.tag,
            "source_record_sha256": record.source_record_sha256,
            "previous": record.previous,
        }));
    }
    if record.tag == prior_tag {
        return Ok(serde_json::json!({
            "tag": record.tag,
            "source_record_sha256": record.source_record_sha256,
        }));
    }
    Err(GeneratorError::usage(
        "publication record identifies neither candidate nor rollback",
    ))
}

/// One published package index entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexEntry {
    /// The index architecture.
    pub(crate) arch: String,
    /// The hex SHA-256 of the `Packages` file.
    pub(crate) sha256: String,
}

/// A typed publication record: the stable shape plus the preview variant
/// (`suite: "preview"`, rolling tag, manifest pin, `"preview"`-or-null
/// previous) as a final typed variant, not a compat branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PublicationRecord {
    /// The publication-record schema URN.
    pub(crate) schema: String,
    /// The source-owned coherence digest: record sidecar (stable) or
    /// release-manifest hash (preview).
    pub(crate) source_record_sha256: String,
    /// The published tag: `vX.Y.Z` (stable) or `preview`.
    pub(crate) tag: String,
    /// The published bare version.
    pub(crate) crate_version: String,
    /// The suite identity, present only for preview.
    pub(crate) suite: Option<String>,
    /// The hex SHA-256 of the signed `InRelease`.
    pub(crate) inrelease_sha256: String,
    /// The per-arch published indexes.
    pub(crate) packages: Vec<IndexEntry>,
    /// The signing-key fingerprint.
    pub(crate) signer_fingerprint: String,
    /// The previous pointer document.
    pub(crate) previous: serde_json::Value,
}

/// Parse a publication record, failing closed on any malformed shape.
pub(crate) fn parse_publication_record(
    document: &serde_json::Value,
) -> Result<PublicationRecord, GeneratorError> {
    if field(document, "schema")? != PUBLICATION_RECORD_SCHEMA {
        return Err(GeneratorError::usage(
            "unsupported publication record schema",
        ));
    }
    let source_record_sha256 = field(document, "source_record_sha256")?;
    if !valid_digest(source_record_sha256) {
        return Err(GeneratorError::usage(
            "publication record source digest is not 64 lowercase hex",
        ));
    }
    let inrelease_sha256 = field(document, "inrelease_sha256")?;
    if !valid_digest(inrelease_sha256) {
        return Err(GeneratorError::usage(
            "publication record InRelease digest is not 64 lowercase hex",
        ));
    }
    let packages = document
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("publication record packages are not an array"))?;
    let mut entries = Vec::new();
    let mut seen_arches = BTreeSet::new();
    for package in packages {
        let arch = field(package, "arch")?.to_owned();
        if !seen_arches.insert(arch.clone()) {
            return Err(GeneratorError::usage(
                "publication record repeats an architecture",
            ));
        }
        entries.push(IndexEntry {
            arch,
            sha256: field(package, "sha256")?.to_owned(),
        });
    }
    if entries.len() != REQUIRED_ARCHES.len()
        || entries.iter().any(|entry| {
            !REQUIRED_ARCHES.contains(&entry.arch.as_str()) || !valid_digest(&entry.sha256)
        })
    {
        return Err(GeneratorError::usage(
            "publication record must index exactly both architectures",
        ));
    }
    if !is_full_fingerprint(&normalize_fingerprint(field(
        document,
        "signer_fingerprint",
    )?)) {
        return Err(GeneratorError::usage(
            "publication record signer is not a full fingerprint",
        ));
    }
    let tag = field(document, "tag")?.to_owned();
    let crate_version = field(document, "crate_version")?.to_owned();
    let suite = document
        .get("suite")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    match suite.as_deref() {
        None => {
            let parsed = parse_stable_tag(&tag)?;
            if parsed.version != crate_version {
                return Err(GeneratorError::usage(
                    "stable publication record tag and crate version disagree",
                ));
            }
            let previous = document.get("previous").ok_or_else(|| {
                GeneratorError::usage("publication record has no previous pointer")
            })?;
            if !previous.is_null() {
                let object = previous.as_object().ok_or_else(|| {
                    GeneratorError::usage("stable publication record previous pointer is malformed")
                })?;
                let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
                keys.sort_unstable();
                if keys != ["source_record_sha256", "tag"]
                    || !is_stable_tag(field(previous, "tag")?)
                    || !valid_digest(field(previous, "source_record_sha256")?)
                {
                    return Err(GeneratorError::usage(
                        "stable publication record previous pointer is malformed",
                    ));
                }
            }
        }
        Some(PREVIEW_SUITE) => {
            if tag != PREVIEW_TAG {
                return Err(GeneratorError::usage(
                    "preview publication record tag is malformed",
                ));
            }
            parse_preview_version(&crate_version)?;
            if document.get("previous").is_none_or(|previous| {
                !previous.is_null() && previous.as_str() != Some(PREVIEW_TAG)
            }) {
                return Err(GeneratorError::usage(
                    "preview publication record previous pointer is malformed",
                ));
            }
        }
        Some(_) => {
            return Err(GeneratorError::usage(
                "publication record suite is unsupported",
            ));
        }
    }
    let previous = document
        .get("previous")
        .cloned()
        .ok_or_else(|| GeneratorError::usage("publication record has no previous pointer"))?;
    Ok(PublicationRecord {
        schema: PUBLICATION_RECORD_SCHEMA.to_owned(),
        source_record_sha256: source_record_sha256.to_owned(),
        tag,
        crate_version,
        suite,
        inrelease_sha256: inrelease_sha256.to_owned(),
        packages: entries,
        signer_fingerprint: field(document, "signer_fingerprint")?.to_owned(),
        previous,
    })
}

/// Emit and detached-sign the publication record into the staging tree.
#[allow(clippy::too_many_arguments)]
fn emit_publication_record(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    tag: &str,
    version: &str,
    source_digest: &str,
    previous: &serde_json::Value,
    path_overlay: Option<&Path>,
    passphrase: &str,
    homedir: &str,
) -> Result<(), GeneratorError> {
    let inrelease = sha256_file(&staging.join(format!("dists/{}/InRelease", suite.as_str())))?;
    let mut packages = Vec::new();
    for arch in REQUIRED_ARCHES {
        let index = staging.join(format!(
            "dists/{}/main/binary-{arch}/Packages",
            suite.as_str()
        ));
        require_file(&index)?;
        packages.push(serde_json::json!({
            "arch": arch,
            "sha256": sha256_file(&index)?,
        }));
    }
    let mut record = BTreeMap::new();
    record.insert(
        "crate_version".to_owned(),
        serde_json::Value::String(version.to_owned()),
    );
    record.insert(
        "inrelease_sha256".to_owned(),
        serde_json::Value::String(inrelease),
    );
    record.insert("packages".to_owned(), serde_json::Value::Array(packages));
    record.insert("previous".to_owned(), previous.clone());
    record.insert(
        "schema".to_owned(),
        serde_json::Value::String(PUBLICATION_RECORD_SCHEMA.to_owned()),
    );
    record.insert(
        "signer_fingerprint".to_owned(),
        serde_json::Value::String(contract.signer.clone()),
    );
    record.insert(
        "source_record_sha256".to_owned(),
        serde_json::Value::String(source_digest.to_owned()),
    );
    if suite == Suite::Preview {
        record.insert(
            "suite".to_owned(),
            serde_json::Value::String(PREVIEW_SUITE.to_owned()),
        );
    }
    record.insert("tag".to_owned(), serde_json::Value::String(tag.to_owned()));
    let text = serde_json::to_string_pretty(&record).map_err(|error| {
        GeneratorError::usage(format!("publication record is not serializable: {error}"))
    })?;
    let file = suite.publication_record_file();
    std::fs::write(staging.join(file), format!("{text}\n"))
        .map_err(|error| GeneratorError::io("write", staging, &error))?;
    // Read-back: the emitted bytes must satisfy the typed record shape.
    let emitted: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| GeneratorError::usage(format!("emitted record is not JSON: {error}")))?;
    parse_publication_record(&emitted)?;
    run_in(
        staging,
        "gpg",
        &gpg_detach_argv(
            &contract.signer,
            homedir,
            &format!("{file}.sig"),
            file,
            false,
        ),
        Some(passphrase.as_bytes()),
        path_overlay,
    )?;
    Ok(())
}

/// Inputs to the signed-record deploy guard. It reads staged metadata and
/// authenticates the live feed itself; plain `last-publish` text has no authority.
pub(crate) struct DeployGuardInputs<'a> {
    /// The suite being deployed.
    pub(crate) suite: Suite,
    /// The resolved typed APT contract.
    pub(crate) contract: AptContract,
    /// The staged Pages tree that would be uploaded.
    pub(crate) staged: &'a Path,
    /// Whether this deployment initializes a never-published suite.
    pub(crate) bootstrap: bool,
    /// The exact verified source commit for this candidate.
    pub(crate) source_commit: &'a str,
    /// The exact stable tag ref or configured preview branch ref.
    pub(crate) source_ref: &'a str,
    /// The source tag/version requested by the workflow.
    pub(crate) version: &'a str,
    /// SHA-256 of the verified stable release record or preview manifest.
    pub(crate) source_record_sha256: &'a str,
    /// Test-only `PATH` overlay resolving fixed tool names.
    pub(crate) path_overlay: Option<&'a Path>,
}

struct VerifiedDeploySuite {
    record: PublicationRecord,
    release: SignedReleaseMetadata,
    package_indexes: BTreeMap<String, String>,
}

/// Authenticate the staged signed record and compare it with the authenticated
/// live publication pointer. A 404 permits bootstrap only after every known
/// suite endpoint and the staged candidate objects return real 404s.
pub(crate) fn check_deploy_guard(
    inputs: &DeployGuardInputs<'_>,
) -> Result<(), GeneratorError> {
    if inputs.contract.feed_url.is_empty() || !valid_feed_url(&inputs.contract.feed_url) {
        return Err(GeneratorError::usage(
            "deploy guard needs a valid authenticated HTTPS feed URL",
        ));
    }
    let sequence = DEPLOY_GUARD_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = std::env::temp_dir().join(format!(
        "velnor-apt-deploy-guard-{}-{sequence}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| GeneratorError::io("create deploy-guard directory", &work, &error))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work)
        .map_err(|error| GeneratorError::io("create deploy-guard directory", &work, &error))?;

    let result = (|| {
        let expected_version = validate_deploy_source_inputs(inputs)?;
        let staged = verify_deploy_suite(
            inputs.staged,
            inputs.suite,
            &inputs.contract,
            &work,
            inputs.path_overlay,
            false,
        )?;
        validate_deploy_record_binding(inputs, &staged.record, &expected_version)?;
        verify_staged_package_state(
            inputs.staged,
            inputs.suite,
            &inputs.contract,
            &staged.record,
            inputs.source_commit,
            inputs.source_ref,
            inputs.version,
        )?;
        let staged_last = inputs
            .staged
            .join(inputs.suite.last_publish_file());
        let staged_version = last_path_text(&staged_last)?;
        let candidate_version = if inputs.suite == Suite::Stable {
            parse_stable_tag(&staged_version)?.version
        } else {
            parse_preview_version(&staged_version)?.version
        };
        if candidate_version != staged.record.crate_version {
            return Err(GeneratorError::usage(
                "deploy guard: staged last-publish disagrees with signed publication record",
            ));
        }

        let live_tree = work.join("live-tree");
        std::fs::create_dir(&live_tree)
            .map_err(|error| GeneratorError::io("create live deploy-guard tree", &live_tree, &error))?;
        restore_live_suite(
            &inputs.contract,
            inputs.suite,
            &live_tree,
            &work,
            inputs.path_overlay,
            true,
        )?;
        let live_inrelease = live_tree
            .join(format!("dists/{}/InRelease", inputs.suite.as_str()));
        match std::fs::symlink_metadata(&live_inrelease) {
            Ok(metadata) if metadata.file_type().is_file() => {
                if inputs.bootstrap {
                    return Err(GeneratorError::usage(
                        "deploy guard: bootstrap was requested although an authenticated live suite exists",
                    ));
                }
                let live = verify_deploy_suite(
                    &live_tree,
                    inputs.suite,
                    &inputs.contract,
                    &work,
                    inputs.path_overlay,
                    true,
                )?;
                compare_deploy_heads(inputs.suite, &staged, &live)
            }
            Ok(_) => Err(GeneratorError::usage(
                "deploy guard: live InRelease path is not a regular file",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !inputs.bootstrap {
                    return Err(GeneratorError::usage(
                        "deploy guard: live suite is absent; explicit bootstrap is required",
                    ));
                }
                if !staged.record.previous.is_null() {
                    return Err(GeneratorError::usage(
                        "deploy guard: bootstrap publication record must have a null previous pointer",
                    ));
                }
                prove_deploy_suite_absent(
                    &inputs.contract,
                    inputs.suite,
                    &candidate_version,
                    inputs.path_overlay,
                )
            }
            Err(error) => Err(GeneratorError::io(
                "inspect restored live InRelease",
                &live_inrelease,
                &error,
            )),
        }
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| GeneratorError::io("remove deploy-guard directory", &work, &error));
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}

fn validate_deploy_source_inputs(
    inputs: &DeployGuardInputs<'_>,
) -> Result<String, GeneratorError> {
    if !valid_commit(inputs.source_commit) || !valid_digest(inputs.source_record_sha256) {
        return Err(GeneratorError::usage(
            "deploy guard needs a valid source commit and source-record SHA-256",
        ));
    }
    let expected = match inputs.suite {
        Suite::Stable => {
            let tag = parse_stable_tag(inputs.version)?;
            let expected_ref = format!("refs/tags/{}", tag.tag);
            if inputs.source_ref != expected_ref {
                return Err(GeneratorError::usage(
                    "deploy guard: stable source ref does not name the candidate tag",
                ));
            }
            tag.version
        }
        Suite::Preview => {
            let parsed = parse_preview_version(inputs.version)?;
            if parsed.sha != inputs.source_commit[..7]
                || inputs.source_ref != inputs.contract.preview_source_ref
            {
                return Err(GeneratorError::usage(
                    "deploy guard: preview source ref or commit disagrees with the candidate version",
                ));
            }
            parsed.version
        }
    };
    Ok(expected)
}

fn validate_deploy_record_binding(
    inputs: &DeployGuardInputs<'_>,
    record: &PublicationRecord,
    expected_version: &str,
) -> Result<(), GeneratorError> {
    let expected_tag = match inputs.suite {
        Suite::Stable => inputs.version,
        Suite::Preview => PREVIEW_TAG,
    };
    if record.crate_version != expected_version
        || record.source_record_sha256 != inputs.source_record_sha256
        || record.tag != expected_tag
    {
        return Err(GeneratorError::usage(
            "deploy guard: signed publication record disagrees with the verified source version, tag or digest",
        ));
    }
    Ok(())
}

fn verify_staged_package_state(
    root: &Path,
    suite: Suite,
    contract: &AptContract,
    record: &PublicationRecord,
    source_commit: &str,
    source_ref: &str,
    version: &str,
) -> Result<(), GeneratorError> {
    let path = root.join(suite.channel_state_file());
    require_regular_file_no_follow(&path, "package-state file")?;
    let state = read_json(&path)?;
    if field(&state, "schema")? != PACKAGE_STATE_SCHEMA
        || field(&state, "source_repository")? != contract.source_repo
        || field(&state, "source_commit")? != source_commit
        || field(&state, "source_ref")? != source_ref
        || field(&state, "version")? != version
        || record.crate_version
            != match suite {
                Suite::Stable => parse_stable_tag(version)?.version,
                Suite::Preview => parse_preview_version(version)?.version,
            }
    {
        return Err(GeneratorError::usage(
            "deploy guard: package-state source commit, ref, repository or version disagrees with the verified release",
        ));
    }
    let package_rows = state
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .filter(|rows| rows.len() == REQUIRED_ARCHES.len())
        .ok_or_else(|| {
            GeneratorError::usage("deploy guard: package-state must list exactly two packages")
        })?;
    let bare_version = match suite {
        Suite::Stable => parse_stable_tag(version)?.version,
        Suite::Preview => parse_preview_version(version)?.version,
    };
    let mut observed = BTreeMap::<String, String>::new();
    for row in package_rows {
        let name = field(row, "name")?;
        let digest = field(row, "sha256")?;
        if !valid_digest(digest) {
            return Err(GeneratorError::usage(
                "deploy guard: package-state package SHA-256 is malformed",
            ));
        }
        let arch = REQUIRED_ARCHES
            .iter()
            .find(|arch| match suite {
                Suite::Stable => name == format!("{}-{bare_version}-{}.deb", contract.package, arch),
                Suite::Preview => {
                    name == format!(
                        "{}-preview-{}-{}.deb",
                        contract.package,
                        dotted_asset_version(&bare_version),
                        arch
                    )
                }
            })
            .copied()
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "deploy guard: package-state has an unexpected package name: {name}"
                ))
            })?;
        if observed.insert(arch.to_owned(), digest.to_owned()).is_some() {
            return Err(GeneratorError::usage(format!(
                "deploy guard: package-state repeats the {arch} package"
            )));
        }
        let pool_path = pool_root(root, suite, contract)
            .join(canonical_pool_name(&contract.package, &bare_version, arch));
        require_regular_file_no_follow(&pool_path, "candidate package")?;
        if sha256_file(&pool_path)? != digest {
            return Err(GeneratorError::usage(format!(
                "deploy guard: package-state {arch} digest differs from staged candidate bytes"
            )));
        }
        let index_path = root.join(format!(
            "dists/{}/main/binary-{arch}/Packages",
            suite.as_str()
        ));
        let index = std::fs::read_to_string(&index_path)
            .map_err(|error| GeneratorError::io("read candidate package index", &index_path, &error))?;
        let entries = parse_live_package_entries(&index, contract, suite, arch)?;
        if !entries.iter().any(|entry| {
            entry.version == bare_version && entry.sha256 == digest
        }) {
            return Err(GeneratorError::usage(format!(
                "deploy guard: package-state {arch} digest is not present in signed Packages"
            )));
        }
    }
    if observed.len() != REQUIRED_ARCHES.len()
        || REQUIRED_ARCHES.iter().any(|arch| !observed.contains_key(*arch))
    {
        return Err(GeneratorError::usage(
            "deploy guard: package-state is missing an architecture package",
        ));
    }
    Ok(())
}

fn verify_deploy_suite(
    root: &Path,
    suite: Suite,
    contract: &AptContract,
    work: &Path,
    path_overlay: Option<&Path>,
    allow_expired: bool,
) -> Result<VerifiedDeploySuite, GeneratorError> {
    let root_fd = open_real_directory(root, "deploy tree")?;
    validate_staging_tree_fd(&root_fd, root)?;
    let dists = root.join(format!("dists/{}", suite.as_str()));
    let inrelease = dists.join("InRelease");
    let release = dists.join("Release");
    let detached = dists.join("Release.gpg");
    for path in [&inrelease, &release, &detached] {
        require_regular_file_no_follow(path, "signed suite metadata")?;
    }
    let recovered_release = work.join(format!("{}-verified-Release", suite.as_str()));
    let gpg_status = run_fixed(
        "gpgv",
        &[
            "--status-fd".to_owned(),
            "1".to_owned(),
            "--keyring".to_owned(),
            contract.keyring.clone(),
            "--output".to_owned(),
            recovered_release
                .to_str()
                .ok_or_else(|| GeneratorError::usage("verified Release path is not UTF-8"))?
                .to_owned(),
            inrelease
                .to_str()
                .ok_or_else(|| GeneratorError::usage("InRelease path is not UTF-8"))?
                .to_owned(),
        ],
        None,
        path_overlay,
    )?;
    let gpg_status = String::from_utf8(gpg_status)
        .map_err(|_| GeneratorError::usage("gpgv returned non-UTF-8 signature status"))?;
    verify_gpgv_signer(&gpg_status, &contract.signer, "staged InRelease")?;
    if std::fs::read(&recovered_release)
        .map_err(|error| GeneratorError::io("read authenticated Release", &recovered_release, &error))?
        != std::fs::read(&release)
            .map_err(|error| GeneratorError::io("read staged Release", &release, &error))?
    {
        return Err(GeneratorError::usage(
            "deploy guard: staged Release bytes disagree with signed InRelease",
        ));
    }
    verify_detached_signature(
        &detached,
        &release,
        &contract.keyring,
        &contract.signer,
        "staged Release",
        path_overlay,
    )?;
    let release_text = std::fs::read_to_string(&release)
        .map_err(|error| GeneratorError::io("read staged Release", &release, &error))?;
    let signed_release = parse_signed_release_with_expired_current(
        &release_text,
        suite,
        contract,
        allow_expired,
    )?;

    let record_path = root.join(suite.publication_record_file());
    let signature_path = root.join(format!("{}.sig", suite.publication_record_file()));
    require_regular_file_no_follow(&record_path, "publication record")?;
    require_regular_file_no_follow(&signature_path, "publication-record signature")?;
    let record_value = read_authenticated_publication_record(
        &record_path,
        &signature_path,
        &contract.keyring,
        &contract.signer,
        path_overlay,
    )?;
    let record = parse_publication_record(&record_value)?;
    if record.suite.as_deref()
        != match suite {
            Suite::Stable => None,
            Suite::Preview => Some(PREVIEW_SUITE),
        }
        || record.inrelease_sha256 != sha256_file(&inrelease)?
        || !fingerprints_match(&record.signer_fingerprint, &contract.signer)
    {
        return Err(GeneratorError::usage(
            "deploy guard: signed publication record does not bind the staged suite",
        ));
    }
    let expected_index_paths = REQUIRED_ARCHES
        .iter()
        .flat_map(|arch| {
            [
                format!("main/binary-{arch}/Packages"),
                format!("main/binary-{arch}/Packages.gz"),
            ]
        })
        .collect::<BTreeSet<_>>();
    if signed_release.checksums.keys().cloned().collect::<BTreeSet<_>>()
        != expected_index_paths
    {
        return Err(GeneratorError::usage(
            "deploy guard: signed Release has an unexpected Packages-index set",
        ));
    }
    let record_indexes = record
        .packages
        .iter()
        .map(|entry| (entry.arch.as_str(), entry.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut package_indexes = BTreeMap::new();
    let mut versions_by_arch = BTreeMap::<String, BTreeSet<String>>::new();
    let mut expected_pool_names = BTreeSet::new();
    for arch in REQUIRED_ARCHES {
        let relative = format!("main/binary-{arch}/Packages");
        let (expected_hash, expected_size) = signed_release
            .checksums
            .get(&relative)
            .ok_or_else(|| GeneratorError::usage("deploy guard: Release omits Packages"))?;
        let package_path = dists.join(&relative);
        require_regular_file_no_follow(&package_path, "Packages index")?;
        let bytes = std::fs::read(&package_path)
            .map_err(|error| GeneratorError::io("read staged Packages", &package_path, &error))?;
        let digest = sha256_hex(&bytes);
        if bytes.len() as u64 != *expected_size || digest != *expected_hash {
            return Err(GeneratorError::usage(format!(
                "deploy guard: staged {arch} Packages disagree with signed Release"
            )));
        }
        if record_indexes.get(arch).copied() != Some(digest.as_str()) {
            return Err(GeneratorError::usage(format!(
                "deploy guard: signed publication record does not bind {arch} Packages"
            )));
        }
        package_indexes.insert(arch.to_owned(), digest);

        let gz_relative = format!("{relative}.gz");
        let (expected_gz_hash, expected_gz_size) = signed_release
            .checksums
            .get(&gz_relative)
            .ok_or_else(|| GeneratorError::usage("deploy guard: Release omits Packages.gz"))?;
        let gz_path = dists.join(&gz_relative);
        require_regular_file_no_follow(&gz_path, "compressed Packages index")?;
        let gz_bytes = std::fs::read(&gz_path)
            .map_err(|error| GeneratorError::io("read staged Packages.gz", &gz_path, &error))?;
        if gz_bytes.len() as u64 != *expected_gz_size || sha256_hex(&gz_bytes) != *expected_gz_hash {
            return Err(GeneratorError::usage(format!(
                "deploy guard: staged {arch} Packages.gz disagrees with signed Release"
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| GeneratorError::usage(format!("staged {arch} Packages is not UTF-8")))?;
        let entries = parse_live_package_entries(&text, contract, suite, arch)?;
        for entry in entries {
            if entry.size == 0 || !valid_digest(&entry.sha256) {
                return Err(GeneratorError::usage("deploy guard: package stanza bytes are malformed"));
            }
            let deb = root.join(&entry.filename);
            require_regular_file_no_follow(&deb, "indexed package")?;
            let metadata = std::fs::metadata(&deb)
                .map_err(|error| GeneratorError::io("inspect indexed package", &deb, &error))?;
            if metadata.len() != entry.size || sha256_file(&deb)? != entry.sha256 {
                return Err(GeneratorError::usage(format!(
                    "deploy guard: staged {arch} package bytes disagree with signed index"
                )));
            }
            let filename = Path::new(&entry.filename)
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| GeneratorError::usage("indexed pool filename is not UTF-8"))?;
            expected_pool_names.insert(filename.to_owned());
            versions_by_arch
                .entry(arch.to_owned())
                .or_default()
                .insert(entry.version);
        }
    }
    let amd64 = versions_by_arch.get("amd64").cloned().unwrap_or_default();
    let arm64 = versions_by_arch.get("arm64").cloned().unwrap_or_default();
    if amd64.is_empty()
        || amd64 != arm64
        || amd64.len() > contract.retention.indexed_versions().saturating_add(1)
    {
        return Err(GeneratorError::usage(
            "deploy guard: staged architecture indexes disagree or exceed recovery retention",
        ));
    }
    if highest_version(suite, amd64.iter().cloned())? != record.crate_version {
        return Err(GeneratorError::usage(
            "deploy guard: signed publication record is not the highest staged package version",
        ));
    }
    validate_live_record_previous(suite, &record, &amd64)?;
    let last_path = root.join(suite.last_publish_file());
    require_regular_file_no_follow(&last_path, "last-publish")?;
    let expected_last = if suite == Suite::Stable {
        record.tag.as_str()
    } else {
        record.crate_version.as_str()
    };
    if last_path_text(&last_path)? != expected_last {
        return Err(GeneratorError::usage(
            "deploy guard: staged last-publish disagrees with signed publication record",
        ));
    }
    let pool = pool_root(root, suite, contract);
    require_real_directory_no_follow(&pool, "package pool")?;
    let actual_pool_names = dir_names(&pool)?.into_iter().collect::<BTreeSet<_>>();
    if actual_pool_names != expected_pool_names {
        return Err(GeneratorError::usage(
            "deploy guard: staged package pool has an orphan or missing package",
        ));
    }
    Ok(VerifiedDeploySuite {
        record,
        release: signed_release,
        package_indexes,
    })
}

fn compare_deploy_heads(
    suite: Suite,
    staged: &VerifiedDeploySuite,
    live: &VerifiedDeploySuite,
) -> Result<(), GeneratorError> {
    let order = match suite {
        Suite::Stable => cmp_stable_versions(&staged.record.tag, &live.record.tag)?,
        Suite::Preview => cmp_preview_versions(
            &staged.record.crate_version,
            &live.record.crate_version,
        )?,
    };
    if order == std::cmp::Ordering::Less {
        return Err(GeneratorError::usage(
            "deploy guard: staged publication is older than the authenticated live publication",
        ));
    }
    if order == std::cmp::Ordering::Greater
        && live.release.valid_until <= time::OffsetDateTime::now_utc()
    {
        return Err(GeneratorError::usage(
            "deploy guard: expired live metadata only permits an exact same-version refresh",
        ));
    }
    if order == std::cmp::Ordering::Equal {
        if staged.record.crate_version != live.record.crate_version
            || staged.record.source_record_sha256 != live.record.source_record_sha256
            || staged.record.previous != live.record.previous
            || staged.package_indexes != live.package_indexes
        {
            return Err(GeneratorError::usage(
                "deploy guard: same-version refresh differs from authenticated live source or package bytes",
            ));
        }
        let exact_redeploy = staged.record == live.record
            && staged.record.inrelease_sha256 == live.record.inrelease_sha256
            && staged.release.date == live.release.date;
        if !exact_redeploy && staged.release.date <= live.release.date {
            return Err(GeneratorError::usage(
                "deploy guard: same-version metadata refresh Date must advance monotonically",
            ));
        }
        return Ok(());
    }
    let previous_matches = match suite {
        Suite::Stable => {
            let expected = serde_json::json!({
                "tag": live.record.tag,
                "source_record_sha256": live.record.source_record_sha256,
            });
            staged.record.previous == expected
        }
        Suite::Preview => staged.record.previous.as_str() == Some(PREVIEW_TAG),
    };
    if !previous_matches {
        return Err(GeneratorError::usage(
            "deploy guard: staged signed previous pointer does not authenticate the live publication",
        ));
    }
    if staged.release.date <= live.release.date {
        return Err(GeneratorError::usage(
            "deploy guard: staged Release Date must advance beyond authenticated live metadata",
        ));
    }
    Ok(())
}

fn prove_deploy_suite_absent(
    contract: &AptContract,
    suite: Suite,
    candidate_version: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let feed = contract.feed_url.trim_end_matches('/');
    let mut resources = vec![
        format!("dists/{}/InRelease", suite.as_str()),
        format!("dists/{}/Release", suite.as_str()),
        format!("dists/{}/Release.gpg", suite.as_str()),
        suite.publication_record_file().to_owned(),
        format!("{}.sig", suite.publication_record_file()),
        suite.last_publish_file().to_owned(),
        suite.channel_state_file().to_owned(),
    ];
    for arch in REQUIRED_ARCHES {
        resources.push(format!(
            "dists/{}/main/binary-{arch}/Packages",
            suite.as_str()
        ));
        resources.push(format!(
            "dists/{}/main/binary-{arch}/Packages.gz",
            suite.as_str()
        ));
    }
    let prefix = if suite == Suite::Preview {
        "pool/preview/"
    } else {
        "pool/"
    };
    let pool = format!(
        "{prefix}main/{}/{}/",
        pool_letter(&contract.package),
        contract.package
    );
    resources.push(pool.clone());
    for arch in REQUIRED_ARCHES {
        resources.push(format!(
            "{pool}{}",
            canonical_pool_name(&contract.package, candidate_version, arch)
        ));
    }
    let sequence = HTTP_PROBE_SEQ.fetch_add(1, Ordering::SeqCst);
    let work = std::env::temp_dir().join(format!(
        "velnor-apt-deploy-absence-{}-{sequence}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .map_err(|error| GeneratorError::io("create deploy absence proof directory", &work, &error))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&work)
        .map_err(|error| GeneratorError::io("create deploy absence proof directory", &work, &error))?;
    let result = (|| {
        for (index, relative) in resources.iter().enumerate() {
            let output = work.join(index.to_string());
            if probe_https(&format!("{feed}/{relative}"), &output, path_overlay)?
                != HttpProbeResult::NotFound
            {
                return Err(GeneratorError::usage(format!(
                    "deploy guard: bootstrap refused because live {relative} exists"
                )));
            }
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| GeneratorError::io("remove deploy absence proof directory", &work, &error));
    result?;
    cleanup?;
    Ok(())
}

fn require_regular_file_no_follow(path: &Path, description: &str) -> Result<(), GeneratorError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| GeneratorError::io(&format!("inspect {description}"), path, &error))?;
    if !metadata.file_type().is_file() {
        return Err(GeneratorError::usage(format!(
            "deploy guard: {description} is not a regular file"
        )));
    }
    Ok(())
}

fn require_real_directory_no_follow(path: &Path, description: &str) -> Result<(), GeneratorError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| GeneratorError::io(&format!("inspect {description}"), path, &error))?;
    if !metadata.file_type().is_dir() {
        return Err(GeneratorError::usage(format!(
            "deploy guard: {description} is not a real directory"
        )));
    }
    Ok(())
}

/// Inputs to the channel-update task: the verified channel head plus the
/// staged pool the state file describes.
pub(crate) struct ChannelUpdateInputs<'a> {
    /// The suite whose head this state records.
    pub(crate) suite: Suite,
    /// The source repository the head was verified against.
    pub(crate) source_repo: String,
    /// The source ref: `refs/tags/vX.Y.Z` (stable) or the configured default branch.
    pub(crate) source_ref: String,
    /// The configured default-branch ref expected by preview manifests.
    pub(crate) preview_source_ref: String,
    /// The verified 40-hex source commit.
    pub(crate) commit: String,
    /// The published version.
    pub(crate) version: String,
    /// The package the state file names.
    pub(crate) package: String,
    /// The source-owned manifest the head cross-checks against.
    pub(crate) manifest: &'a Path,
    /// The staging tree holding the pool and receiving the state file.
    pub(crate) staging: &'a Path,
}

/// Emit the machine-readable channel head (`package-state.json`): the index
/// alone does not carry the source commit, so consumers poll this file. The
/// cross-checks mirror `package-update.sh`: manifest shape per suite,
/// identity agreement, and the exact two-asset set.
pub(crate) fn run_channel_update(inputs: &ChannelUpdateInputs<'_>) -> Result<(), GeneratorError> {
    if !valid_repository_slug(&inputs.source_repo) {
        return Err(GeneratorError::usage(
            "channel update needs an `owner/name` source repository",
        ));
    }
    if !valid_package_name(&inputs.package) {
        return Err(GeneratorError::usage(
            "channel update needs a safe package name",
        ));
    }
    if !valid_commit(&inputs.commit) {
        return Err(GeneratorError::usage(
            "channel update needs a 40-hex source commit",
        ));
    }
    if !valid_preview_source_ref(&inputs.preview_source_ref) {
        return Err(GeneratorError::usage(
            "channel update needs a preview source ref of refs/heads/<safe branch>",
        ));
    }
    let manifest = read_json(inputs.manifest)?;
    check_channel_manifest(inputs, &manifest)?;
    // The state records the download keys while hashing the staged
    // candidate pool bytes (canonical `name_version_arch` names).
    let candidate = match inputs.suite {
        Suite::Stable => parse_stable_tag(&inputs.version)?.version,
        Suite::Preview => inputs.version.clone(),
    };
    let mut packages: Vec<BTreeMap<String, String>> = Vec::new();
    for arch in REQUIRED_ARCHES {
        let file = match inputs.suite {
            Suite::Stable => format!("{}-{candidate}-{arch}.deb", inputs.package),
            Suite::Preview => format!(
                "{}-preview-{}-{arch}.deb",
                inputs.package,
                dotted_asset_version(&candidate)
            ),
        };
        let staged = find_staged_deb(
            inputs.staging,
            inputs.suite,
            &inputs.package,
            &candidate,
            arch,
        )?;
        let sha = sha256_file(&staged)?;
        let mut entry = BTreeMap::new();
        entry.insert("name".to_owned(), file);
        entry.insert("sha256".to_owned(), sha);
        packages.push(entry);
    }
    packages.sort_by(|left, right| left["name"].cmp(&right["name"]));
    let mut state = BTreeMap::new();
    state.insert(
        "packages".to_owned(),
        serde_json::Value::Array(
            packages
                .into_iter()
                .map(|entry| {
                    serde_json::Value::Object(
                        entry
                            .into_iter()
                            .map(|(key, value)| (key, serde_json::Value::String(value)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    );
    state.insert(
        "schema".to_owned(),
        serde_json::Value::String(PACKAGE_STATE_SCHEMA.to_owned()),
    );
    state.insert(
        "source_commit".to_owned(),
        serde_json::Value::String(inputs.commit.clone()),
    );
    state.insert(
        "source_ref".to_owned(),
        serde_json::Value::String(inputs.source_ref.clone()),
    );
    state.insert(
        "source_repository".to_owned(),
        serde_json::Value::String(inputs.source_repo.clone()),
    );
    state.insert(
        "version".to_owned(),
        serde_json::Value::String(inputs.version.clone()),
    );
    let text = serde_json::to_string_pretty(&state).map_err(|error| {
        GeneratorError::usage(format!("channel state is not serializable: {error}"))
    })?;
    std::fs::write(
        inputs.staging.join(inputs.suite.channel_state_file()),
        format!("{text}\n"),
    )
    .map_err(|error| GeneratorError::io("write", inputs.staging, &error))?;
    Ok(())
}

/// Cross-check the channel head against the source-owned manifest, per
/// the suite's identity rules.
fn check_channel_manifest(
    inputs: &ChannelUpdateInputs<'_>,
    manifest: &serde_json::Value,
) -> Result<(), GeneratorError> {
    match inputs.suite {
        Suite::Stable => {
            let tag = parse_stable_tag(&inputs.version)?;
            let want_ref = format!("refs/tags/{}", tag.tag);
            if inputs.source_ref != want_ref {
                return Err(GeneratorError::usage(
                    "channel update: stable source_ref must be the tag ref",
                ));
            }
            if field(manifest, "source_sha")? != inputs.commit {
                return Err(GeneratorError::usage(
                    "channel update: manifest source_sha != commit",
                ));
            }
            if field(manifest, "crate_version")? != tag.version {
                return Err(GeneratorError::usage(
                    "channel update: manifest crate_version mismatch",
                ));
            }
        }
        Suite::Preview => {
            let parsed = parse_preview_version(&inputs.version)?;
            if inputs.source_ref != inputs.preview_source_ref {
                return Err(GeneratorError::usage(format!(
                    "channel update: preview source_ref must be {}",
                    inputs.preview_source_ref
                )));
            }
            if parsed.sha != inputs.commit[..7] {
                return Err(GeneratorError::usage(
                    "channel update: preview version suffix does not match the source commit",
                ));
            }
            if field(manifest, "source_repository")? != inputs.source_repo {
                return Err(GeneratorError::usage(
                    "channel update: manifest repository mismatch",
                ));
            }
            if field(manifest, "source_ref")? != inputs.preview_source_ref {
                return Err(GeneratorError::usage(
                    "channel update: manifest source_ref mismatch",
                ));
            }
            if field(manifest, "source_commit")? != inputs.commit {
                return Err(GeneratorError::usage(
                    "channel update: manifest source_commit != commit",
                ));
            }
            if field(manifest, "version")? != parsed.version {
                return Err(GeneratorError::usage(
                    "channel update: manifest version mismatch",
                ));
            }
        }
    }
    Ok(())
}

/// Find the staged candidate deb for `arch` by its exact canonical pool
/// name. The pool also holds the rollback pair; only the candidate's bytes
/// feed the channel state.
fn find_staged_deb(
    staging: &Path,
    suite: Suite,
    package: &str,
    candidate: &str,
    arch: &str,
) -> Result<PathBuf, GeneratorError> {
    let mut root = staging.join("pool");
    if suite == Suite::Preview {
        root.push(PREVIEW_SUITE);
    }
    let pool = root
        .join(MAIN_COMPONENT)
        .join(pool_letter(package))
        .join(package);
    let deb = pool.join(canonical_pool_name(package, candidate, arch));
    if deb.is_file() {
        Ok(deb)
    } else {
        Err(GeneratorError::usage(format!(
            "channel update: staged candidate {arch} deb is missing from the pool"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

    const FIXTURE_SOURCE: &str = "example/app";
    const FIXTURE_PACKAGE: &str = "example";
    const FIXTURE_BINARY: &str = "example";
    const FIXTURE_IDENTITY: &str = "app";
    const FIXTURE_SCHEMA: &str = "example.test/apt-manifest-v1";
    const FIXTURE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const FIXTURE_FPR: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    fn fixture_dir(name: &str) -> PathBuf {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("apt-feed-b1-{name}-{}-{seq}", std::process::id()));
        must(
            std::fs::create_dir_all(&dir),
            "create the fixture directory",
        );
        dir
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            must(std::fs::create_dir_all(parent), "create fixture parents");
        }
        must(std::fs::write(path, bytes), "write fixture file");
    }

    fn write_sidecar(path: &Path, digest: &str) {
        write_bytes(path, format!("{digest}  {}\n", path.display()).as_bytes());
    }

    struct FixtureAptSpec {
        kind: String,
        source_repository: String,
        package: String,
        binary: String,
        consumer_repository: String,
        manifest_schema: String,
        signer_fingerprint: String,
        passphrase_secret: String,
        signing_key_secret: String,
        apt_attestation_secret: String,
        keyring_path: String,
        apt_origin: String,
        apt_identity_dir: String,
        apt_feed_url: String,
        preview_source_ref: String,
        description: String,
        apt_arches: Vec<String>,
        retention: i64,
    }

    impl FixtureAptSpec {
        fn input(&self) -> AptContractInput<'_> {
            AptContractInput {
                kind: &self.kind,
                source_repository: &self.source_repository,
                package: &self.package,
                binary: &self.binary,
                consumer_repository: &self.consumer_repository,
                manifest_schema: &self.manifest_schema,
                signer_fingerprint: &self.signer_fingerprint,
                passphrase_secret: &self.passphrase_secret,
                signing_key_secret: &self.signing_key_secret,
                apt_attestation_secret: &self.apt_attestation_secret,
                keyring_path: &self.keyring_path,
                apt_origin: &self.apt_origin,
                apt_identity_dir: &self.apt_identity_dir,
                apt_feed_url: &self.apt_feed_url,
                preview_source_ref: &self.preview_source_ref,
                description: &self.description,
                apt_arches: &self.apt_arches,
                retention: self.retention,
            }
        }
    }

    fn apt_spec() -> FixtureAptSpec {
        FixtureAptSpec {
            kind: "apt".to_owned(),
            source_repository: FIXTURE_SOURCE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            binary: FIXTURE_BINARY.to_owned(),
            consumer_repository: "example/feed".to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            signer_fingerprint: FIXTURE_FPR.to_owned(),
            passphrase_secret: "B1_TEST_PASSPHRASE".to_owned(),
            signing_key_secret: "B1_TEST_SIGNING_KEY".to_owned(),
            apt_attestation_secret: String::new(),
            keyring_path: String::new(),
            apt_origin: String::new(),
            apt_identity_dir: String::new(),
            apt_feed_url: "https://feed.example.test".to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            description: String::new(),
            apt_arches: Vec::new(),
            retention: 0,
        }
    }

    fn apt_contract() -> AptContract {
        let spec = apt_spec();
        must(
            AptContract::resolve_input(spec.input()),
            "resolve the fixture contract",
        )
    }

    #[test]
    fn tar_decompression_flags_follow_payload_magic() {
        // GNU tar refuses compressed payloads without an explicit flag
        // while bsdtar sniffs them, so the shared extractor must name
        // the codec for every `.deb` member compression.
        assert_eq!(tar_decompress_flag(&[0x1f, 0x8b, 0x08, 0x00]), Some("-z"));
        assert_eq!(tar_decompress_flag(&[0x42, 0x5a, 0x68]), Some("-j"));
        assert_eq!(
            tar_decompress_flag(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]),
            Some("-J")
        );
        assert_eq!(
            tar_decompress_flag(&[0x28, 0xb5, 0x2f, 0xfd]),
            Some("--zstd")
        );
        assert_eq!(tar_decompress_flag(b"ustar payload"), None);
        assert_eq!(tar_decompress_flag(&[]), None);
        assert_eq!(tar_decompress_flag(&[0x1f]), None);
    }

    #[test]
    fn repository_slugs_accept_owner_name_only() {
        for valid in ["example/app", "acme.widget/my_feed-2", "a/b"] {
            assert!(valid_repository_slug(valid), "{valid}");
        }
        for invalid in [
            "",
            "example",
            "/app",
            "example/",
            "example//app",
            "example/app/extra",
            "https://github.com/example/app",
            "example/app.git ",
            "example app",
            "example;rm -rf /",
            "$(id)",
            "`id`",
            "example/app\nrun: evil",
            "../evil",
        ] {
            assert!(!valid_repository_slug(invalid), "{invalid}");
        }
    }

    #[test]
    fn package_binary_and_identity_names_reject_shell_shapes() {
        assert!(valid_package_name("example-2_x"));
        assert!(valid_binary_name("example-2_x.y"));
        assert!(valid_identity_dir("app"));
        for invalid in [
            "", "ex ample", "ex;ample", "$(x)", "`x`", "a/b", "a\nb", "a'b",
        ] {
            assert!(!valid_package_name(invalid), "{invalid}");
            assert!(!valid_binary_name(invalid), "{invalid}");
            assert!(!valid_identity_dir(invalid), "{invalid}");
        }
        assert!(!valid_package_name("a.b"));
    }

    #[test]
    fn secret_refs_name_environment_secrets_never_values() {
        assert!(valid_secret_ref("B1_TEST_PASSPHRASE"));
        assert!(valid_secret_ref("A"));
        for invalid in [
            "",
            "lower",
            "9LIVES",
            "APT PASSPHRASE",
            "APT-PASSPHRASE",
            "s3cret-value!",
            "$SECRET",
            "A".repeat(65).as_str(),
        ] {
            assert!(!valid_secret_ref(invalid), "{invalid}");
        }
    }

    #[test]
    fn keyring_paths_stay_relative_without_traversal() {
        assert!(valid_keyring_path("example.gpg"));
        assert!(valid_keyring_path("keys/example.gpg"));
        for invalid in [
            "",
            "/etc/passwd",
            "../evil.gpg",
            "keys/../evil",
            "a//b",
            "a b",
            "a;rm",
        ] {
            assert!(!valid_keyring_path(invalid), "{invalid}");
        }
    }

    #[test]
    fn origins_descriptions_and_staging_dirs_reject_control_shapes() {
        assert!(valid_origin("Example"));
        assert!(valid_origin("Example Feed 2.0+x"));
        assert!(!valid_origin(""));
        assert!(!valid_origin(" Leading"));
        assert!(!valid_origin("a;rm"));
        assert!(!valid_origin("a\nb"));
        assert!(valid_description("apt repository for example"));
        assert!(!valid_description(""));
        assert!(!valid_description("line one\nline two"));
        assert!(!valid_description("tab\there"));
        assert!(valid_staging_dir("public"));
        assert!(valid_staging_dir("out/staging"));
        for invalid in ["", ".", ".hidden", "/abs", "../out", "a b"] {
            assert!(!valid_staging_dir(invalid), "{invalid}");
        }
    }

    #[test]
    fn feed_urls_accept_https_hosts_only() {
        assert!(valid_feed_url("https://feed.example.test"));
        assert!(valid_feed_url("https://feed.example.test/debian"));
        for invalid in [
            "",
            "http://feed.example.test",
            "https://",
            "https:///path",
            "https://user@host",
            "https://host:8443",
            "https://host/a b",
            "https://host/a;b",
            "$(curl evil)",
            "https://host/a?b",
        ] {
            assert!(!valid_feed_url(invalid), "{invalid}");
        }
    }

    #[test]
    fn apt_http_probe_classifies_only_success_and_real_404() {
        for status in ["200", "204", "299"] {
            assert_eq!(
                must(
                    classify_http_probe_status(status),
                    "classify successful HTTP status"
                ),
                HttpProbeResult::Present
            );
        }
        assert_eq!(
            must(classify_http_probe_status("404"), "classify not-found status"),
            HttpProbeResult::NotFound
        );
        for status in ["301", "403", "500", "503", "000"] {
            let error = must_fail(
                classify_http_probe_status(status),
                "reject an untrusted or unsuccessful HTTP status",
            );
            assert!(
                error.contains("only 2xx and 404 are accepted"),
                "{status}: {error}"
            );
        }
        let error = must_fail(
            classify_http_probe_status("unknown"),
            "reject a malformed HTTP status",
        );
        assert!(error.contains("malformed HTTP status"), "{error}");
        let error = must_fail(
            HttpProbeResult::parse("unknown"),
            "reject an untyped probe result",
        );
        assert!(error.contains("APT HTTP probe result"), "{error}");
    }

    #[test]
    fn apt_http_probe_propagates_network_errors_and_accepts_only_final_404() {
        let root = fixture_dir("http-probe");
        let bin = root.join("bin");
        must(std::fs::create_dir_all(&bin), "create curl stub directory");
        let output = root.join("live-version");
        let curl = bin.join("curl");
        let write_status_stub = |status: &str| {
            write_bytes(
                &curl,
                format!(
                    "#!/bin/sh\noutput=\nprevious=\nfor arg do\n  case \"$previous\" in --output) output=\"$arg\" ;; esac\n  previous=\"$arg\"\ndone\n[ -n \"$output\" ] || exit 2\nprintf '%s\\n' 'probe response' > \"$output\"\nprintf '%s' '{status}'\n"
                )
                .as_bytes(),
            );
            make_executable(&curl);
        };

        write_status_stub("404");
        assert_eq!(
            must(
                probe_https("https://feed.example.test/apt/last-publish", &output, Some(&bin)),
                "probe a genuine 404"
            ),
            HttpProbeResult::NotFound
        );
        assert!(!output.exists(), "404 must not leave a live-version file");

        for status in ["503", "302"] {
            write_status_stub(status);
            let error = must_fail(
                probe_https("https://feed.example.test/apt/last-publish", &output, Some(&bin)),
                "reject server and redirect statuses",
            );
            let expected = format!("HTTP {status}");
            assert!(error.contains(expected.as_str()), "{error}");
        }

        write_bytes(&curl, b"#!/bin/sh\nexit 7\n");
        make_executable(&curl);
        let error = must_fail(
            probe_https("https://feed.example.test/apt/last-publish", &output, Some(&bin)),
            "propagate a curl transport failure",
        );
        assert!(error.contains("curl failed"), "{error}");

        write_status_stub("200");
        assert_eq!(
            must(
                probe_https("https://feed.example.test/apt/last-publish", &output, Some(&bin)),
                "probe an existing live version"
            ),
            HttpProbeResult::Present
        );
        assert_eq!(
            must(std::fs::read_to_string(&output), "read probed response"),
            "probe response\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn signed_release_fixture(
        contract: &AptContract,
        date: OffsetDateTime,
        valid_until: OffsetDateTime,
    ) -> String {
        let date = must(
            date.format(&time::format_description::well_known::Rfc2822),
            "format fixture Release Date",
        );
        let valid_until = must(
            valid_until.format(&time::format_description::well_known::Rfc2822),
            "format fixture Release Valid-Until",
        );
        let digest = "ab".repeat(32);
        format!(
            "Origin: {origin}\nLabel: {origin}\nSuite: stable\nCodename: stable\nArchitectures: amd64 arm64\nComponents: main\nDate: {date}\nValid-Until: {valid_until}\nSHA256:\n  {digest} 1 main/binary-amd64/Packages\n  {digest} 1 main/binary-amd64/Packages.gz\n  {digest} 1 main/binary-arm64/Packages\n  {digest} 1 main/binary-arm64/Packages.gz\n",
            origin = contract.origin.as_str(),
        )
    }

    #[test]
    fn signed_release_freshness_is_parsed_bounded_and_monotonic() {
        let contract = apt_contract();
        let now = must(
            OffsetDateTime::from_unix_timestamp(OffsetDateTime::now_utc().unix_timestamp()),
            "read current test time",
        );
        let date = now - time::Duration::minutes(10);
        let valid_until = now + time::Duration::hours(2);
        let text = signed_release_fixture(&contract, date, valid_until);
        let metadata = must(
            parse_signed_release(&text, Suite::Stable, &contract),
            "parse a fresh signed Release",
        );
        assert_eq!(metadata.date, date);

        let future_date = now + time::Duration::minutes(6);
        let future = signed_release_fixture(
            &contract,
            future_date,
            future_date + time::Duration::days(7),
        );
        let error = must_fail(
            parse_signed_release(&future, Suite::Stable, &contract),
            "reject future signed Release Date",
        );
        assert!(error.contains("too far in the future"), "{error}");

        let expired = signed_release_fixture(
            &contract,
            now - time::Duration::hours(2),
            now - time::Duration::minutes(1),
        );
        let error = must_fail(
            parse_signed_release(&expired, Suite::Stable, &contract),
            "reject expired signed Release Valid-Until",
        );
        assert!(error.contains("Valid-Until has expired"), "{error}");

        let excessive = signed_release_fixture(
            &contract,
            now - time::Duration::minutes(1),
            now + time::Duration::days(32),
        );
        let error = must_fail(
            parse_signed_release(&excessive, Suite::Stable, &contract),
            "reject an unbounded signed Release validity interval",
        );
        assert!(error.contains("exceeds 31 days"), "{error}");

        let previous = now + time::Duration::minutes(1);
        let generated = must(
            stamp_release_freshness("Origin: Example\n", Some(previous)),
            "stamp a monotonic Release",
        );
        let generated_date = generated
            .lines()
            .find_map(|line| line.strip_prefix("Date: "))
            .unwrap_or_else(|| panic!("generated Release has no Date: {generated}"));
        let generated_valid_until = generated
            .lines()
            .find_map(|line| line.strip_prefix("Valid-Until: "))
            .unwrap_or_else(|| panic!("generated Release has no Valid-Until: {generated}"));
        let generated_date = must(
            parse_release_time(generated_date, "Date"),
            "parse generated Release Date",
        );
        let generated_valid_until = must(
            parse_release_time(generated_valid_until, "Valid-Until"),
            "parse generated Release Valid-Until",
        );
        assert_eq!(generated_date, previous + time::Duration::seconds(1));
        assert_eq!(
            generated_valid_until - generated_date,
            time::Duration::days(7)
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_transaction_rejects_symlinked_components_and_targets() {
        let root = fixture_dir("staging-symlink-root");
        let outside_name = format!(
            "{}-outside",
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("apt-staging")
        );
        let outside = root.with_file_name(outside_name);
        must(std::fs::create_dir_all(&outside), "create external target");
        must(
            std::os::unix::fs::symlink(&outside, root.join("linked-parent")),
            "create symlinked staging parent",
        );
        must(
            std::os::unix::fs::symlink(&outside, root.join("public")),
            "create symlinked staging target",
        );

        let parent_error = in_fixture_root(&root, || {
            must_fail(
                StagingTransaction::new(Path::new("linked-parent/public")),
                "reject a symlinked staging component",
            )
        });
        assert!(parent_error.contains("staging parent is not a real directory"), "{parent_error}");

        let target_error = in_fixture_root(&root, || {
            must_fail(
                StagingTransaction::new(Path::new("public")),
                "reject a symlinked staging target",
            )
        });
        assert!(target_error.contains("staging target is not a real directory"), "{target_error}");
        assert!(
            std::fs::read_dir(&outside)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
            "refusal must leave the external target untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn staging_transaction_swaps_only_after_commit_and_preserves_the_other_suite() {
        let root = fixture_dir("staging-atomic");
        write_bytes(
            &root.join("public/dists/stable/Release"),
            b"old stable Release",
        );
        write_bytes(
            &root.join("public/dists/preview/Release"),
            b"authenticated preview Release",
        );

        in_fixture_root(&root, || {
            let transaction = must(
                StagingTransaction::new(Path::new("public")),
                "start an uncommitted staging transaction",
            );
            write_bytes(
                &transaction.path().join("dists/stable/Release"),
                b"uncommitted stable Release",
            );
            assert_eq!(
                must(
                    std::fs::read("public/dists/stable/Release"),
                    "read stable tree before commit"
                ),
                b"old stable Release"
            );
            drop(transaction);
        });
        assert_eq!(
            must(
                std::fs::read(root.join("public/dists/stable/Release")),
                "read stable tree after aborted transaction"
            ),
            b"old stable Release"
        );

        in_fixture_root(&root, || {
            let transaction = must(
                StagingTransaction::new(Path::new("public")),
                "start a replacement staging transaction",
            );
            write_bytes(
                &transaction.path().join("dists/stable/Release"),
                b"new stable Release",
            );
            assert_eq!(
                must(
                    std::fs::read("public/dists/stable/Release"),
                    "read old tree while replacement is prepared"
                ),
                b"old stable Release"
            );
            must(transaction.commit(), "atomically commit replacement staging tree");
        });
        assert_eq!(
            must(
                std::fs::read(root.join("public/dists/stable/Release")),
                "read committed stable tree"
            ),
            b"new stable Release"
        );
        assert_eq!(
            must(
                std::fs::read(root.join("public/dists/preview/Release")),
                "read preserved preview tree"
            ),
            b"authenticated preview Release"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn staging_transaction_does_not_replace_a_target_created_after_absent_preflight() {
        let root = fixture_dir("staging-noreplace-race");
        in_fixture_root(&root, || {
            let transaction = must(
                StagingTransaction::new(Path::new("public")),
                "start transaction with absent target",
            );
            write_bytes(Path::new("public/raced.txt"), b"raced target");
            write_bytes(&transaction.path().join("candidate.txt"), b"candidate tree");
            let error = must_fail(
                transaction.commit(),
                "refuse a target created after the absent-target preflight",
            );
            assert!(error.contains("staging target or working path changed"), "{error}");
            assert_eq!(
                must(std::fs::read("public/raced.txt"), "read raced target"),
                b"raced target"
            );
            assert!(!Path::new("public/candidate.txt").exists());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn staging_transaction_anchors_writes_and_rejects_a_replaced_working_name() {
        let root = fixture_dir("staging-anchor-race");
        let outside = root.with_file_name("staging-anchor-outside");
        write_bytes(&outside.join("victim"), b"untouched");
        let outcome = in_fixture_root(&root, || {
            let transaction = must(
                StagingTransaction::new(Path::new("public")),
                "start anchored staging transaction",
            );
            let moved = root.join("moved-working-tree");
            let working_link = transaction.working.clone();
            must(
                std::fs::rename(&transaction.working, &moved),
                "move staging pathname after opening its directory handle",
            );
            must(
                std::os::unix::fs::symlink(&outside, &transaction.working),
                "replace staging pathname with an external symlink",
            );
            write_bytes(&transaction.path().join("anchored-write"), b"inside opened inode");
            let error = must_fail(transaction.commit(), "reject a changed staging directory name");
            assert!(error.contains("no longer names its validated directory handle"), "{error}");
            (moved, working_link)
        });
        assert_eq!(
            must(std::fs::read(outside.join("victim")), "read external victim"),
            b"untouched"
        );
        assert_eq!(
            must(
                std::fs::read(outcome.0.join("anchored-write")),
                "read descriptor-anchored write"
            ),
            b"inside opened inode"
        );
        assert_eq!(
            must(std::fs::read_link(&outcome.1), "read raced working link"),
            outside
        );
        let _ = std::fs::remove_file(&outcome.1);
        let _ = std::fs::remove_dir_all(&outcome.0);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn staging_transaction_recovers_interrupted_exchange_and_scavenges_stale_tree() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = fixture_dir("staging-recover");
        write_bytes(&root.join("public/old.txt"), b"old");
        let stale = root.join(".public.velnor-new-999999-1");
        must(std::fs::create_dir_all(&stale), "create stale sibling");
        must(
            std::fs::set_permissions(
                &stale,
                std::fs::Permissions::from_mode(0o700),
            ),
            "make stale sibling private",
        );
        write_bytes(&stale.join(STAGING_MARKER), b".public.velnor-new-999999-1");
        write_bytes(&stale.join("partial.txt"), b"partial");
        in_fixture_root(&root, || {
            let mut transaction = must(
                StagingTransaction::new(Path::new("public")),
                "recover stale sibling before beginning a new transaction",
            );
            assert!(!stale.exists(), "startup scavenger removes only marked stale siblings");
            write_bytes(&transaction.path().join("new.txt"), b"new");
            let journal = StagingJournal {
                phase: "prepared".to_owned(),
                working_name: transaction.working_name.to_string_lossy().into_owned(),
                working_identity: transaction.working_identity,
                target_identity: transaction.target_identity,
            };
            must(
                write_staging_journal(&mut transaction.lock_file, &journal),
                "persist prepared transaction journal",
            );
            must(
                unlinkat(&transaction.working_fd, STAGING_MARKER, AtFlags::empty()),
                "remove transaction marker before exchange",
            );
            must(
                renameat_with(
                    &transaction.parent,
                    &transaction.working_name,
                    &transaction.parent,
                    &transaction.target_name,
                    RenameFlags::EXCHANGE,
                ),
                "simulate exchange before interrupted process exit",
            );
            must(
                recover_staging_transaction(
                    &transaction.parent,
                    &transaction.target_name,
                    &mut transaction.lock_file,
                    &transaction.target,
                ),
                "restore original tree after interrupted exchange",
            );
            assert_eq!(
                must(std::fs::read("public/old.txt"), "read restored old tree"),
                b"old"
            );
            assert!(!Path::new("public/new.txt").exists());
            transaction.committed = true;
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn staging_transaction_recovers_after_old_tree_cleanup_before_journal_clear() {
        let root = fixture_dir("staging-recover-cleanup");
        write_bytes(&root.join("public/old.txt"), b"old");
        in_fixture_root(&root, || {
            let mut transaction = must(
                StagingTransaction::new(Path::new("public")),
                "start staging transaction for cleanup recovery",
            );
            write_bytes(&transaction.path().join("new.txt"), b"new");
            let prepared = StagingJournal {
                phase: "prepared".to_owned(),
                working_name: transaction.working_name.to_string_lossy().into_owned(),
                working_identity: transaction.working_identity,
                target_identity: transaction.target_identity,
            };
            must(
                write_staging_journal(&mut transaction.lock_file, &prepared),
                "persist prepared staging journal",
            );
            must(
                unlinkat(&transaction.working_fd, STAGING_MARKER, AtFlags::empty()),
                "remove transaction marker before exchange",
            );
            must(
                renameat_with(
                    &transaction.parent,
                    &transaction.working_name,
                    &transaction.parent,
                    &transaction.target_name,
                    RenameFlags::EXCHANGE,
                ),
                "simulate staging exchange",
            );
            must(
                write_staging_journal(
                    &mut transaction.lock_file,
                    &StagingJournal {
                        phase: "committed".to_owned(),
                        ..prepared
                    },
                ),
                "persist committed staging journal",
            );
            let old_identity = transaction
                .target_identity
                .ok_or("fixture target identity is missing");
            must(
                remove_staging_directory_if_identity(
                    &transaction.parent,
                    &transaction.working_name,
                    must(old_identity, "read fixture target identity"),
                    &transaction.working,
                ),
                "simulate cleanup completed before journal clear",
            );
            must(
                recover_staging_transaction(
                    &transaction.parent,
                    &transaction.target_name,
                    &mut transaction.lock_file,
                    &transaction.target,
                ),
                "recover committed exchange after old-tree cleanup",
            );
            assert_eq!(
                must(std::fs::read("public/new.txt"), "read installed staged tree"),
                b"new"
            );
            transaction.committed = true;
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hex_and_fingerprint_shapes_match_the_runner() {
        assert!(valid_commit(FIXTURE_COMMIT));
        assert!(!valid_commit("0123456789ABCDEF0123456789ABCDEF01234567"));
        assert!(!valid_commit("0123456"));
        assert!(!valid_commit(""));
        assert!(valid_digest(&"ab".repeat(32)));
        assert!(!valid_digest(&"AB".repeat(32)));
        assert!(is_full_fingerprint(FIXTURE_FPR));
        assert!(!is_full_fingerprint(FIXTURE_COMMIT));
        assert!(!is_full_fingerprint("0123"));
        assert_eq!(
            normalize_fingerprint("0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567"),
            "0123456789ABCDEF0123456789ABCDEF01234567"
        );
        assert!(fingerprints_match(
            "0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567",
            FIXTURE_FPR
        ));
        assert!(!fingerprints_match(
            FIXTURE_FPR,
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
        ));
    }

    #[test]
    fn secret_key_fingerprint_reads_the_first_fpr_record() {
        let listing = "sec:-:2048:1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:0:\n\
             fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:\n\
             uid:::::::::Example <example@test>:\n";
        assert_eq!(
            secret_key_fingerprint(listing).as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
        assert_eq!(secret_key_fingerprint(""), None);
        assert_eq!(secret_key_fingerprint("tru::1:0:0:0:0:\n"), None);
        assert_eq!(secret_key_fingerprint("fpr::::::::::\n"), None);
    }

    #[test]
    fn suites_parse_the_locked_pair_only() {
        assert_eq!(must(Suite::parse("stable"), "stable"), Suite::Stable);
        assert_eq!(must(Suite::parse("preview"), "preview"), Suite::Preview);
        assert_eq!(Suite::Stable.as_str(), "stable");
        assert_eq!(Suite::Preview.as_str(), "preview");
        assert_eq!(Suite::Stable.last_publish_file(), "last-publish");
        assert_eq!(Suite::Preview.last_publish_file(), "last-publish-preview");
        assert_eq!(
            Suite::Stable.publication_record_file(),
            "publication-record.json"
        );
        assert_eq!(
            Suite::Preview.publication_record_file(),
            "publication-record-preview.json"
        );
        assert_eq!(Suite::Stable.channel_state_file(), "package-state.json");
        assert_eq!(
            Suite::Preview.channel_state_file(),
            "package-state-preview.json"
        );
        for invalid in ["", "Stable", "PREVIEW", "testing", "stable\npreview"] {
            let error = must_fail(Suite::parse(invalid), "unknown suite");
            assert!(error.contains("suite must be"), "{error}");
        }
    }

    #[test]
    fn stable_tags_require_the_v_prefix_and_a_numeric_triple() {
        let tag = must(parse_stable_tag("v1.2.3"), "parse v1.2.3");
        assert_eq!(tag.tag, "v1.2.3");
        assert_eq!(tag.version, "1.2.3");
        for invalid in [
            "", "1.2.3", "v1.2", "v1.2.3.4", "vv1.2.3", "v1.2.x", "v 1.2.3",
            "v01.2.3", "v1.02.3", "v1.2.03",
        ] {
            let error = must_fail(parse_stable_tag(invalid), "bad stable tag");
            assert!(error.contains("vX.Y.Z"), "{error}");
        }
        assert!(
            must(parse_stable_tag("v18446744073709551616.2.3"), "parse arbitrary-width stable version")
                .version
                == "18446744073709551616.2.3"
        );
        assert_eq!(
            must(
                cmp_stable_versions(
                    "v18446744073709551616.2.3",
                    "v18446744073709551615.2.3",
                ),
                "compare arbitrary-width stable versions",
            ),
            std::cmp::Ordering::Greater,
        );
    }

    #[test]
    fn preview_versions_require_the_tilde_grammar() {
        let parsed = must(
            parse_preview_version("1.2.3~preview.41+0123456"),
            "parse preview",
        );
        assert_eq!(parsed.base, "1.2.3");
        assert_eq!(parsed.seq, "41");
        assert_eq!(parsed.sha, "0123456");
        for invalid in [
            "",
            "v1.2.3~preview.41+0123456",
            "1.2.3",
            "1.2.3~preview.41",
            "1.2.3~preview.+0123456",
            "1.2.3~preview.41+012345",
            "1.2.3~preview.41+01234567",
            "1.2.3~preview.41+012345G",
            "1.2.3-preview.41+0123456",
            "1.2.3~preview.41+0123456+extra",
            "1.2~preview.41+0123456",
            "01.2.3~preview.41+0123456",
            "1.2.3~preview.041+0123456",
        ] {
            let error = must_fail(parse_preview_version(invalid), "bad preview version");
            assert!(error.contains("X.Y.Z~preview.N"), "{error}");
        }
        assert_eq!(
            dotted_asset_version("1.2.3~preview.41+0123456"),
            "1.2.3.preview.41+0123456"
        );
    }

    fn preview_source_release(version: &str) -> PreviewSourceRelease {
        let parsed = must(parse_preview_version(version), "parse preview row");
        let mut assets = BTreeMap::new();
        let dotted = dotted_asset_version(version);
        for (index, arch) in REQUIRED_ARCHES.iter().enumerate() {
            assets.insert(
                (*arch).to_owned(),
                PreviewSourceAsset {
                    name: format!("{FIXTURE_PACKAGE}-preview-{dotted}-{arch}.deb"),
                    sha256: if index == 0 {
                        "a".repeat(64)
                    } else {
                        "b".repeat(64)
                    },
                },
            );
        }
        PreviewSourceRelease {
            tag: must(preview_source_tag(version), "derive preview tag"),
            version: version.to_owned(),
            source_sha: format!("{}{}", parsed.sha, "0".repeat(33)),
            assets,
        }
    }

    #[test]
    fn preview_source_tags_and_discovery_use_immutable_dpkg_order() {
        assert_eq!(
            must(
                preview_source_tag(PREVIEW_CANDIDATE),
                "derive immutable source release tag"
            ),
            "preview-1.2.3.preview.41+0123456"
        );
        let rows = [
            preview_source_release("1.2.3~preview.40+abcdef0"),
            preview_source_release(PREVIEW_CANDIDATE),
            preview_source_release("1.2.4~preview.1+abcdef0"),
        ];
        let selected = must(
            select_preview_source_release(&rows, "preview-", Some(PREVIEW_CANDIDATE)),
            "select highest preview source release",
        );
        assert_eq!(selected.version, "1.2.4~preview.1+abcdef0");
        assert_eq!(
            selected.tag,
            "preview-1.2.4.preview.1+abcdef0"
        );
        assert_eq!(selected.assets.len(), REQUIRED_ARCHES.len());
        let equal = must(
            select_preview_source_release(
                &[preview_source_release(PREVIEW_CANDIDATE)],
                "preview-",
                Some(PREVIEW_CANDIDATE),
            ),
            "allow same-version preview metadata refresh",
        );
        assert_eq!(equal.version, PREVIEW_CANDIDATE);
        let error = must_fail(
            select_preview_source_release(
                &[preview_source_release("1.2.3~preview.40+abcdef0")],
                "preview-",
                Some(PREVIEW_CANDIDATE),
            ),
            "reject stale preview source release",
        );
        assert!(error.contains("older than authenticated live head"), "{error}");
        let error = must_fail(
            select_preview_source_release(&rows, "preview", None),
            "reject mutable preview release prefix",
        );
        assert!(error.contains("immutable preview- prefix"), "{error}");

        let tied = [
            preview_source_release("1.2.3~preview.41+000000a"),
            preview_source_release("1.2.3~preview.41+00000a0"),
        ];
        assert_eq!(
            must(
                cmp_preview_versions(&tied[0].version, &tied[1].version),
                "compare dpkg-equivalent preview versions"
            ),
            std::cmp::Ordering::Equal
        );
        let error = must_fail(
            select_preview_source_release(&tied, "preview-", None),
            "reject dpkg-equivalent preview source releases",
        );
        assert!(error.contains("tie in Debian version order"), "{error}");

        let malformed = PreviewSourceRelease {
            tag: "preview-mutable".to_owned(),
            ..preview_source_release(PREVIEW_CANDIDATE)
        };
        let error = must_fail(
            select_preview_source_release(&[malformed], "preview-", None),
            "reject noncanonical source tag mapping",
        );
        assert!(error.contains("does not map exactly"), "{error}");

        let huge_sequence = format!("1.2.3~preview.{}+abcdef0", "9".repeat(128));
        let larger_sequence = format!("1.2.3~preview.1{}+abcdef0", "0".repeat(128));
        assert_eq!(
            must(
                cmp_preview_versions(&larger_sequence, &huge_sequence),
                "compare arbitrary-width preview sequences",
            ),
            std::cmp::Ordering::Greater,
        );
        let huge_rows = [
            preview_source_release(&huge_sequence),
            preview_source_release(&larger_sequence),
        ];
        assert_eq!(
            must(
                select_preview_source_release(&huge_rows, "preview-", None),
                "select arbitrary-width preview release",
            )
            .version,
            larger_sequence,
        );
    }

    #[test]
    fn preview_release_manifest_parser_binds_ref_tag_commit_and_assets() {
        let tag = must(
            preview_source_tag(PREVIEW_CANDIDATE),
            "derive manifest source tag",
        );
        let dotted = dotted_asset_version(PREVIEW_CANDIDATE);
        let manifest = serde_json::json!({
            "schema": FIXTURE_SCHEMA,
            "source_repository": FIXTURE_SOURCE,
            "source_ref": PREVIEW_SOURCE_REF,
            "source_commit": FIXTURE_COMMIT,
            "release_tag": tag,
            "version": PREVIEW_CANDIDATE,
            "assets": [
                {"name": format!("{FIXTURE_PACKAGE}-preview-{dotted}-amd64.deb"), "sha256": "a".repeat(64)},
                {"name": format!("{FIXTURE_PACKAGE}-preview-{dotted}-arm64.deb"), "sha256": "b".repeat(64)},
            ],
        });
        let parsed = must(
            parse_preview_source_release_manifest(
                &manifest,
                FIXTURE_SCHEMA,
                FIXTURE_SOURCE,
                PREVIEW_SOURCE_REF,
                &tag,
                FIXTURE_PACKAGE,
            ),
            "parse authenticated preview release manifest",
        );
        assert_eq!(parsed.version, PREVIEW_CANDIDATE);
        assert_eq!(parsed.tag, tag);
        assert_eq!(parsed.source_sha, FIXTURE_COMMIT);
        assert_eq!(parsed.assets["amd64"].name, format!("{FIXTURE_PACKAGE}-preview-{dotted}-amd64.deb"));
        assert_eq!(parsed.assets["arm64"].sha256, "b".repeat(64));

        let mut changed = manifest.clone();
        changed["release_tag"] = serde_json::Value::String("preview".to_owned());
        let error = must_fail(
            parse_preview_source_release_manifest(
                &changed,
                FIXTURE_SCHEMA,
                FIXTURE_SOURCE,
                PREVIEW_SOURCE_REF,
                &tag,
                FIXTURE_PACKAGE,
            ),
            "reject mutable release tag in manifest",
        );
        assert!(error.contains("does not match the manifest version"), "{error}");

        let mut changed = manifest.clone();
        changed["assets"][1]["name"] = serde_json::Value::String("unexpected.deb".to_owned());
        let error = must_fail(
            parse_preview_source_release_manifest(
                &changed,
                FIXTURE_SCHEMA,
                FIXTURE_SOURCE,
                PREVIEW_SOURCE_REF,
                &tag,
                FIXTURE_PACKAGE,
            ),
            "reject unrecognized manifest asset",
        );
        assert!(error.contains("unexpected asset name"), "{error}");

        let mut changed = manifest.clone();
        changed["assets"][1]["name"] = serde_json::Value::String(
            format!("{FIXTURE_PACKAGE}-preview-{PREVIEW_CANDIDATE}-arm64.deb"),
        );
        let error = must_fail(
            parse_preview_source_release_manifest(
                &changed,
                FIXTURE_SCHEMA,
                FIXTURE_SOURCE,
                PREVIEW_SOURCE_REF,
                &tag,
                FIXTURE_PACKAGE,
            ),
            "reject tilde-form preview asset name",
        );
        assert!(error.contains("unexpected asset name"), "{error}");
    }

    #[test]
    fn deploy_guard_binds_package_state_to_source_commit_and_version() {
        let root = fixture_dir("deploy-state-binding");
        let contract = apt_contract();
        let candidate = PREVIEW_CANDIDATE;
        let source_record_sha256 = "c".repeat(64);
        let guard_inputs = DeployGuardInputs {
            suite: Suite::Preview,
            contract: contract.clone(),
            staged: &root,
            bootstrap: false,
            source_commit: FIXTURE_COMMIT,
            source_ref: PREVIEW_SOURCE_REF,
            version: candidate,
            source_record_sha256: &source_record_sha256,
            path_overlay: None,
        };
        assert_eq!(
            must(
                validate_deploy_source_inputs(&guard_inputs),
                "validate preview deploy source identity"
            ),
            candidate
        );
        let stale_commit = "f".repeat(40);
        let stale_inputs = DeployGuardInputs {
            suite: Suite::Preview,
            contract: contract.clone(),
            staged: &root,
            bootstrap: false,
            source_commit: &stale_commit,
            source_ref: PREVIEW_SOURCE_REF,
            version: candidate,
            source_record_sha256: &source_record_sha256,
            path_overlay: None,
        };
        let error = must_fail(
            validate_deploy_source_inputs(&stale_inputs),
            "reject a deploy source commit that differs from the preview version suffix",
        );
        assert!(error.contains("source ref or commit disagrees"), "{error}");

        let wrong_version = "1.2.3~preview.42+abcdef0";
        let wrong_version_inputs = DeployGuardInputs {
            contract: contract.clone(),
            staged: &root,
            bootstrap: false,
            source_commit: FIXTURE_COMMIT,
            source_ref: PREVIEW_SOURCE_REF,
            version: wrong_version,
            source_record_sha256: &source_record_sha256,
            path_overlay: None,
            suite: Suite::Preview,
        };
        let error = must_fail(
            validate_deploy_source_inputs(&wrong_version_inputs),
            "reject a deploy version from a different source commit",
        );
        assert!(error.contains("source ref or commit disagrees"), "{error}");

        let mut package_rows = Vec::new();
        let mut index_text = BTreeMap::new();
        for arch in REQUIRED_ARCHES {
            let pool_name = canonical_pool_name(&contract.package, candidate, arch);
            let pool_path = pool_root(&root, Suite::Preview, &contract).join(&pool_name);
            let package_bytes = format!("fixture package bytes for {arch}").into_bytes();
            write_bytes(&pool_path, &package_bytes);
            let package_sha = sha256_hex(&package_bytes);
            let release_name = format!(
                "{}-preview-{}-{arch}.deb",
                contract.package,
                dotted_asset_version(candidate)
            );
            package_rows.push(serde_json::json!({
                "name": release_name,
                "sha256": package_sha,
            }));
            let pool_path = format!(
                "pool/preview/main/{}/{}/{}",
                pool_letter(&contract.package),
                contract.package,
                pool_name
            );
            index_text.insert(
                arch.to_owned(),
                format!(
                    "Package: {}\nVersion: {candidate}\nArchitecture: {arch}\nFilename: {pool_path}\nSize: {}\nSHA256: {package_sha}\n\n",
                    contract.package,
                    package_bytes.len()
                ),
            );
        }
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &root.join(format!(
                    "dists/preview/main/binary-{arch}/Packages"
                )),
                index_text[arch].as_bytes(),
            );
        }
        let state = serde_json::json!({
            "schema": PACKAGE_STATE_SCHEMA,
            "source_repository": contract.source_repo.clone(),
            "source_commit": FIXTURE_COMMIT,
            "source_ref": PREVIEW_SOURCE_REF,
            "version": candidate,
            "packages": package_rows,
        });
        let state_path = root.join(Suite::Preview.channel_state_file());
        write_bytes(&state_path, format!("{state}\n").as_bytes());
        let record = PublicationRecord {
            schema: PUBLICATION_RECORD_SCHEMA.to_owned(),
            source_record_sha256: source_record_sha256.clone(),
            tag: PREVIEW_TAG.to_owned(),
            crate_version: candidate.to_owned(),
            suite: Some(PREVIEW_SUITE.to_owned()),
            inrelease_sha256: "e".repeat(64),
            packages: REQUIRED_ARCHES
                .iter()
                .map(|arch| IndexEntry {
                    arch: (*arch).to_owned(),
                    sha256: "f".repeat(64),
                })
                .collect(),
            signer_fingerprint: FIXTURE_FPR.to_owned(),
            previous: serde_json::Value::String(PREVIEW_TAG.to_owned()),
        };
        must(
            validate_deploy_record_binding(&guard_inputs, &record, candidate),
            "bind the signed publication record to the expected source digest and version",
        );
        let mut changed_record = record.clone();
        changed_record.source_record_sha256 = "d".repeat(64);
        let error = must_fail(
            validate_deploy_record_binding(&guard_inputs, &changed_record, candidate),
            "reject a signed record from a different source manifest",
        );
        assert!(error.contains("verified source version, tag or digest"), "{error}");
        changed_record = record.clone();
        changed_record.crate_version = "1.2.3~preview.42+abcdef0".to_owned();
        let error = must_fail(
            validate_deploy_record_binding(&guard_inputs, &changed_record, candidate),
            "reject a signed record for a different package version",
        );
        assert!(error.contains("verified source version, tag or digest"), "{error}");

        must(
            verify_staged_package_state(
                &root,
                Suite::Preview,
                &contract,
                &record,
                FIXTURE_COMMIT,
                PREVIEW_SOURCE_REF,
                candidate,
            ),
            "accept package state bound to the verified preview source",
        );

        let mut changed = state.clone();
        changed["source_commit"] = serde_json::Value::String("f".repeat(40));
        write_bytes(&state_path, format!("{changed}\n").as_bytes());
        let error = must_fail(
            verify_staged_package_state(
                &root,
                Suite::Preview,
                &contract,
                &record,
                FIXTURE_COMMIT,
                PREVIEW_SOURCE_REF,
                candidate,
            ),
            "reject package state from a different source commit",
        );
        assert!(error.contains("source commit, ref, repository or version"), "{error}");

        changed = state;
        changed["version"] = serde_json::Value::String("1.2.3~preview.40+abcdef0".to_owned());
        write_bytes(&state_path, format!("{changed}\n").as_bytes());
        let error = must_fail(
            verify_staged_package_state(
                &root,
                Suite::Preview,
                &contract,
                &record,
                FIXTURE_COMMIT,
                PREVIEW_SOURCE_REF,
                candidate,
            ),
            "reject package state from a different version",
        );
        assert!(error.contains("source commit, ref, repository or version"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn release_source_attestation_parser_binds_predicate_and_subject_digests() {
        let expected_subjects = BTreeSet::from([
            "a".repeat(64),
            "b".repeat(64),
        ]);
        let output = serde_json::json!([{
            "verificationResult": {
                "statement": {
                    "predicateType": RELEASE_SOURCE_PREDICATE,
                    "predicate": {
                        "source_sha": FIXTURE_COMMIT,
                        "release_tag": "v1.2.3",
                        "version": "1.2.3",
                        "source_repository": FIXTURE_SOURCE,
                    },
                    "subject": [
                        {"name": "amd64.deb", "digest": {"sha256": "a".repeat(64)}},
                        {"name": "arm64.deb", "digest": {"sha256": "b".repeat(64)}},
                    ],
                }
            }
        }]);
        let output_bytes = must(serde_json::to_vec(&output), "serialize attestation fixture");
        let parsed = must(
            parse_release_source_attestation(
                &output_bytes,
                FIXTURE_COMMIT,
                "v1.2.3",
                "1.2.3",
                FIXTURE_SOURCE,
            ),
            "parse stable release-source attestation",
        );
        assert_eq!(parsed, expected_subjects);

        let mut changed = output.clone();
        changed[0]["verificationResult"]["statement"]["predicate"]["source_sha"] =
            serde_json::Value::String("f".repeat(40));
        let changed_bytes = must(serde_json::to_vec(&changed), "serialize changed attestation");
        let error = must_fail(
            parse_release_source_attestation(
                &changed_bytes,
                FIXTURE_COMMIT,
                "v1.2.3",
                "1.2.3",
                FIXTURE_SOURCE,
            ),
            "reject source commit mismatch",
        );
        assert!(error.contains("source_sha mismatch"), "{error}");
    }

    #[test]
    fn stable_versions_compare_numerically_per_component() {
        use std::cmp::Ordering;
        assert_eq!(
            must(cmp_stable_versions("v1.2.3", "v1.2.10"), "compare"),
            Ordering::Less
        );
        assert_eq!(
            must(cmp_stable_versions("v1.10.0", "v1.2.0"), "compare"),
            Ordering::Greater
        );
        assert_eq!(
            must(cmp_stable_versions("v2.0.0", "v2.0.0"), "compare"),
            Ordering::Equal
        );
        let error = must_fail(cmp_stable_versions("v1.2.3", "1.2.4"), "bad tag");
        assert!(error.contains("vX.Y.Z"), "{error}");
    }

    #[test]
    fn preview_versions_compare_base_then_sequence_then_suffix() {
        use std::cmp::Ordering;
        let less: &[(&str, &str)] = &[
            ("1.2.3~preview.1+0000000", "1.2.4~preview.1+0000000"),
            ("1.2.3~preview.1+0000000", "1.2.3~preview.2+0000000"),
            ("1.2.3~preview.9+0000000", "1.2.3~preview.10+0000000"),
            ("1.2.3~preview.1+0000000", "1.2.3~preview.1+0000001"),
            ("1.2.3~preview.1+9ffffff", "1.2.3~preview.1+affffff"),
        ];
        for (left, right) in less {
            assert_eq!(
                must(cmp_preview_versions(left, right), "compare"),
                Ordering::Less,
                "{left} vs {right}"
            );
            assert_eq!(
                must(cmp_preview_versions(right, left), "compare"),
                Ordering::Greater,
                "{right} vs {left}"
            );
        }
        assert_eq!(
            must(
                cmp_preview_versions("1.2.3~preview.1+abcdef0", "1.2.3~preview.1+abcdef0"),
                "compare"
            ),
            Ordering::Equal
        );
        let error = must_fail(
            cmp_preview_versions("1.2.3~preview.1+abcdef0", "v1.2.3"),
            "mixed suites",
        );
        assert!(error.contains("X.Y.Z~preview.N"), "{error}");
    }

    #[test]
    fn preview_order_agrees_with_dpkg_when_dpkg_exists() {
        let pairs: &[(&str, &str)] = &[
            ("1.2.3~preview.1+0000000", "1.2.3~preview.2+0000000"),
            ("1.2.3~preview.9+ffffff0", "1.2.3~preview.10+0000000"),
            // `verrevcmp` reads the leading digit run numerically: `0000009`
            // is 9 while `000000a` is 0-then-letter, so the letter suffix
            // sorts first (confirmed via `dpkg --compare-versions`).
            ("1.2.3~preview.1+000000a", "1.2.3~preview.1+0000009"),
            ("1.2.3~preview.1+9aaaaaa", "1.2.3~preview.1+10aaaaa"),
            ("0.1.9~preview.3+abc1234", "0.1.10~preview.1+0000000"),
        ];
        let Ok(dpkg) = which_dpkg() else {
            for (left, right) in pairs {
                assert_eq!(
                    must(cmp_preview_versions(left, right), "pure order"),
                    std::cmp::Ordering::Less,
                    "{left} vs {right}"
                );
            }
            return;
        };
        for (left, right) in pairs {
            let status = std::process::Command::new(&dpkg)
                .args(["--compare-versions", left, "lt", right])
                .status();
            let matches = must(status, "run dpkg").success();
            assert!(matches, "dpkg disagrees: {left} lt {right}");
            assert_eq!(
                must(cmp_preview_versions(left, right), "pure order"),
                std::cmp::Ordering::Less,
                "{left} vs {right}"
            );
        }
    }

    #[test]
    fn preview_digit_run_suffix_sorts_after_letter_suffix() {
        use std::cmp::Ordering;
        // Regression pin for the `0000009` vs `000000a` dispute: a byte-wise
        // digits-before-letters comparison reports Less here, but `dpkg`
        // reads the leading digit run numerically (9 > 0) and reports
        // Greater. Both directions plus the multi-digit numeric case below
        // were confirmed with `dpkg --compare-versions`.
        assert_eq!(
            must(
                cmp_preview_versions("1.2.3~preview.1+0000009", "1.2.3~preview.1+000000a"),
                "compare"
            ),
            Ordering::Greater,
        );
        assert_eq!(
            must(
                cmp_preview_versions("1.2.3~preview.1+000000a", "1.2.3~preview.1+0000009"),
                "compare"
            ),
            Ordering::Less,
        );
        assert_eq!(
            must(
                cmp_preview_versions("1.2.3~preview.1+10aaaaa", "1.2.3~preview.1+9aaaaaa"),
                "compare"
            ),
            Ordering::Greater,
        );
    }

    fn which_dpkg() -> Result<PathBuf, ()> {
        let path = std::env::var_os("PATH").ok_or(())?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("dpkg"))
            .find(|candidate| candidate.is_file())
            .ok_or(())
    }

    #[test]
    fn retention_accepts_the_implemented_policy_only() {
        assert_eq!(must(Retention::parse(0), "default"), Retention(1));
        assert_eq!(must(Retention::parse(1), "one"), Retention(1));
        assert_eq!(Retention(1).indexed_versions(), 2);
        assert_eq!(Retention(1).pool_debs(), 4);
        for invalid in [-1, 2, 3, 99] {
            let error = must_fail(Retention::parse(invalid), "bad retention");
            assert!(error.contains("by policy"), "{error}");
        }
    }

    #[test]
    fn contract_resolution_applies_documented_defaults() {
        let contract = apt_contract();
        assert_eq!(contract.source_repo, FIXTURE_SOURCE);
        assert_eq!(contract.package, FIXTURE_PACKAGE);
        assert_eq!(contract.binary, FIXTURE_BINARY);
        assert_eq!(contract.consumer_repo, "example/feed");
        assert_eq!(contract.manifest_schema, FIXTURE_SCHEMA);
        assert_eq!(contract.signer, FIXTURE_FPR);
        assert_eq!(contract.passphrase_secret, "B1_TEST_PASSPHRASE");
        assert_eq!(contract.apt_attestation_secret, None);
        assert_eq!(contract.keyring, "example.gpg");
        assert_eq!(contract.origin, FIXTURE_PACKAGE);
        assert_eq!(contract.identity_dir, FIXTURE_IDENTITY);
        assert_eq!(contract.feed_url, "https://feed.example.test");
        assert_eq!(contract.preview_source_ref, PREVIEW_SOURCE_REF);
        assert_eq!(contract.description, "apt repository for example");
        assert_eq!(contract.arches, ["amd64".to_owned(), "arm64".to_owned()]);
        assert_eq!(contract.retention, Retention(1));
    }

    #[test]
    fn contract_resolution_honors_explicit_values() {
        let mut spec = apt_spec();
        spec.apt_arches = vec!["arm64".to_owned(), "amd64".to_owned()];
        spec.keyring_path = "keys/feed.gpg".to_owned();
        spec.apt_origin = "Example Feed".to_owned();
        spec.apt_identity_dir = "example-id".to_owned();
        spec.description = "Custom feed description".to_owned();
        spec.signer_fingerprint = FIXTURE_FPR.to_ascii_lowercase();
        spec.apt_attestation_secret = "SOURCE_ATTESTATION_TOKEN".to_owned();
        spec.retention = 1;
        let contract = must(AptContract::resolve_input(spec.input()), "resolve explicit");
        assert_eq!(contract.signing_key_secret, "B1_TEST_SIGNING_KEY");
        assert_eq!(contract.arches, ["amd64".to_owned(), "arm64".to_owned()]);
        assert_eq!(contract.keyring, "keys/feed.gpg");
        assert_eq!(contract.origin, "Example Feed");
        assert_eq!(contract.identity_dir, "example-id");
        assert_eq!(contract.description, "Custom feed description");
        assert_eq!(contract.signer, FIXTURE_FPR);
        assert_eq!(
            contract.apt_attestation_secret.as_deref(),
            Some("SOURCE_ATTESTATION_TOKEN")
        );
    }

    type SpecMutation = Box<dyn Fn(&mut FixtureAptSpec)>;

    #[test]
    fn contract_resolution_rejects_every_malformed_field() {
        let cases: &[(&str, SpecMutation)] = &[
            ("kind", Box::new(|spec| spec.kind = "pages".to_owned())),
            (
                "source",
                Box::new(|spec| spec.source_repository = "not-a-slug".to_owned()),
            ),
            (
                "consumer",
                Box::new(|spec| spec.consumer_repository = String::new()),
            ),
            (
                "package",
                Box::new(|spec| spec.package = "has space".to_owned()),
            ),
            ("binary", Box::new(|spec| spec.binary = "a/b".to_owned())),
            (
                "preview source ref",
                Box::new(|spec| spec.preview_source_ref = "refs/tags/main".to_owned()),
            ),
            (
                "schema",
                Box::new(|spec| spec.manifest_schema = "has space".to_owned()),
            ),
            (
                "signer",
                Box::new(|spec| spec.signer_fingerprint = "short".to_owned()),
            ),
            (
                "secret",
                Box::new(|spec| spec.passphrase_secret = "lowercase".to_owned()),
            ),
            (
                "attestation secret",
                Box::new(|spec| spec.apt_attestation_secret = "source token".to_owned()),
            ),
            (
                "secret-value",
                Box::new(|spec| spec.passphrase_secret = "s3cret!".to_owned()),
            ),
            (
                "key-secret",
                Box::new(|spec| spec.signing_key_secret = "lowercase".to_owned()),
            ),
            (
                "key-secret-empty",
                Box::new(|spec| spec.signing_key_secret = String::new()),
            ),
            (
                "keyring",
                Box::new(|spec| spec.keyring_path = "/abs.gpg".to_owned()),
            ),
            (
                "origin",
                Box::new(|spec| spec.apt_origin = "a\nb".to_owned()),
            ),
            (
                "identity",
                Box::new(|spec| spec.apt_identity_dir = "a/b".to_owned()),
            ),
            (
                "feed",
                Box::new(|spec| spec.apt_feed_url = "http://plain".to_owned()),
            ),
            (
                "description",
                Box::new(|spec| spec.description = "a\nb".to_owned()),
            ),
            (
                "arches-missing",
                Box::new(|spec| spec.apt_arches = vec!["amd64".to_owned()]),
            ),
            (
                "arches-dup",
                Box::new(|spec| {
                    spec.apt_arches =
                        vec!["amd64".to_owned(), "amd64".to_owned(), "arm64".to_owned()];
                }),
            ),
            (
                "arches-foreign",
                Box::new(|spec| spec.apt_arches = vec!["amd64".to_owned(), "i386".to_owned()]),
            ),
            ("retention", Box::new(|spec| spec.retention = 2)),
        ];
        for (name, mutate) in cases {
            let mut spec = apt_spec();
            mutate(&mut spec);
            let error = must_fail(AptContract::resolve_input(spec.input()), name);
            assert!(!error.is_empty(), "{name}");
        }
    }

    #[test]
    fn fetch_patterns_are_fixed_allowists_per_suite() {
        let stable = must(
            download_patterns(Suite::Stable, "example", "v1.2.3"),
            "stable patterns",
        );
        assert_eq!(
            stable,
            vec![
                "example-1.2.3-amd64.deb",
                "example-1.2.3-amd64.deb.sha256",
                "example-1.2.3-arm64.deb",
                "example-1.2.3-arm64.deb.sha256",
                "manifest.json",
                "manifest.json.sha256",
                "release-record.json",
                "release-record.json.sha256",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        let preview = must(
            download_patterns(Suite::Preview, "example", "1.2.3~preview.41+0123456"),
            "preview patterns",
        );
        assert_eq!(preview.len(), 6);
        assert!(preview.contains(&"release-manifest.json".to_owned()));
        assert!(preview.contains(&"SHA256SUMS".to_owned()));
        assert!(preview.contains(&"example-preview-1.2.3.preview.41+0123456-amd64.deb".to_owned()));
        assert!(preview
            .contains(&"example-preview-1.2.3.preview.41+0123456-arm64.deb.sha256".to_owned()));
        for (suite, version) in [
            (Suite::Stable, "1.2.3"),
            (Suite::Preview, "v1.2.3~preview.1+abcdef0"),
        ] {
            let error = must_fail(download_patterns(suite, "example", version), "bad version");
            assert!(!error.is_empty());
        }
        let error = must_fail(
            download_patterns(Suite::Stable, "has space", "v1.2.3"),
            "bad package",
        );
        assert!(error.contains("package"), "{error}");
    }

    #[test]
    fn fetch_argv_is_fixed_and_slug_validated() {
        let argv = must(
            gh_download_argv(
                "v1.2.3",
                "example/app",
                Path::new("incoming"),
                &["a".to_owned()],
            ),
            "gh argv",
        );
        assert_eq!(
            argv,
            vec![
                "release",
                "download",
                "v1.2.3",
                "--repo",
                "example/app",
                "--dir",
                "incoming",
                "--pattern",
                "a",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        let error = must_fail(
            gh_download_argv("v1.2.3", "not-a-slug", Path::new("incoming"), &[]),
            "bad slug",
        );
        assert!(error.contains("owner/name"), "{error}");
        assert_eq!(
            resolve_commit_argv("https://github.com/example/app.git", "v1.2.3", true),
            vec![
                "ls-remote",
                "https://github.com/example/app.git",
                "refs/tags/v1.2.3^{}"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            must(source_git_url("example/app"), "source git"),
            "https://github.com/example/app.git"
        );
        let error = must_fail(source_git_url("https://evil.example/x"), "evil slug");
        assert!(error.contains("owner/name"), "{error}");
    }

    #[test]
    fn resolve_commit_invokes_git_ls_remote() {
        // The resolver must hand git the full fixed argv — `ls-remote` first.
        // Slicing the subcommand off makes git read the URL as its command,
        // so every scheduled and empty-commit discovery fails closed while
        // the explicit-commit path (which never calls the resolver) works.
        let dir = fixture_dir("resolve-commit-git");
        let bin = dir.join("bin");
        must(std::fs::create_dir_all(&bin), "stub bin");
        write_bytes(
            &bin.join("git"),
            format!(
                "#!/bin/sh\n[ \"$1\" = \"ls-remote\" ] || exit 1\nprintf '%s\\n' \"git $*\" >> \"{}/git.log\"\nprintf '{FIXTURE_COMMIT}\\trefs/tags/v1.2.3^{{}}'\\n",
                dir.display()
            )
            .as_bytes(),
        );
        make_executable(&bin.join("git"));
        let commit = must(
            run_resolve_commit(FIXTURE_SOURCE, "v1.2.3", Some(&bin)),
            "resolve the tag commit",
        );
        assert_eq!(commit, FIXTURE_COMMIT);
        let log = must(
            std::fs::read_to_string(dir.join("git.log")),
            "read the git log",
        );
        assert!(
            log.contains("git ls-remote https://github.com/example/app.git refs/tags/v1.2.3^{}"),
            "{log}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Craft a minimal but valid `.deb` with portable `ar`/`tar`/`gzip` so
    /// the fixture is readable both by `dpkg-deb` (where present) and by the
    /// `ar`+`tar` fallback.
    #[allow(clippy::too_many_arguments)]
    fn make_deb(
        dir: &Path,
        name: &str,
        package: &str,
        version: &str,
        arch: &str,
        binary: &str,
        identity_dir: &str,
        commit: &str,
        crate_version: &str,
        manifest_bytes: &[u8],
        binary_bytes: &[u8],
    ) -> PathBuf {
        let work = dir.join(format!(".debwork-{name}"));
        let control_dir = work.join("control");
        let data_dir = work.join("data");
        must(std::fs::create_dir_all(&control_dir), "control dir");
        must(std::fs::create_dir_all(data_dir.join("usr/bin")), "bin dir");
        must(
            std::fs::create_dir_all(data_dir.join("usr/share").join(identity_dir)),
            "identity dir",
        );
        write_bytes(
            &control_dir.join("control"),
            format!(
                "Package: {package}\nVersion: {version}\nArchitecture: {arch}\nMaintainer: Fixture <fixture@example.test>\nDescription: fixture\n"
            )
            .as_bytes(),
        );
        write_bytes(&data_dir.join("usr/bin").join(binary), binary_bytes);
        write_bytes(
            &data_dir
                .join("usr/share")
                .join(identity_dir)
                .join("build-identity.json"),
            format!("{{\"source_sha\": \"{commit}\", \"crate_version\": \"{crate_version}\"}}\n")
                .as_bytes(),
        );
        write_bytes(
            &data_dir
                .join("usr/share")
                .join(identity_dir)
                .join("manifest.json"),
            manifest_bytes,
        );
        for (member, source) in [("control.tar.gz", &control_dir), ("data.tar.gz", &data_dir)] {
            let status = must(
                std::process::Command::new("tar")
                    .args(["-czf"])
                    .arg(work.join(member))
                    .args(["-C"])
                    .arg(source)
                    .arg(".")
                    .status(),
                "tar the deb member",
            );
            assert!(status.success(), "tar {member} failed");
        }
        write_bytes(&work.join("debian-binary"), b"2.0\n");
        let deb = dir.join(name);
        // `S` suppresses the archive symbol table: BSD `ar` otherwise
        // rewrites the members into a `__.SYMDEF`-only archive, and GNU `ar`
        // accepts the flag with the same meaning.
        let status = must(
            std::process::Command::new("ar")
                .args(["rcS"])
                .arg(&deb)
                .arg(work.join("debian-binary"))
                .arg(work.join("control.tar.gz"))
                .arg(work.join("data.tar.gz"))
                .status(),
            "ar the deb",
        );
        assert!(status.success(), "ar {name} failed");
        must(std::fs::remove_dir_all(&work), "clean deb workdir");
        deb
    }

    struct StableIncoming {
        dir: PathBuf,
        tag: String,
        commit: String,
        source: String,
        package: String,
        binary: String,
        identity: String,
        bin: PathBuf,
    }

    /// Build a coherent stable incoming directory for the default fixture
    /// identity, returning the paths and digests the checks bind.
    fn stable_incoming(root: &str) -> StableIncoming {
        stable_incoming_renamed(
            root,
            FIXTURE_SOURCE,
            FIXTURE_PACKAGE,
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
        )
    }

    fn stable_incoming_renamed(
        root: &str,
        source: &str,
        package: &str,
        binary: &str,
        identity: &str,
    ) -> StableIncoming {
        let dir = fixture_dir(root);
        let tag = "v1.2.3".to_owned();
        let version = "1.2.3".to_owned();
        let commit = FIXTURE_COMMIT.to_owned();
        let manifest = format!(
            "{{\"source_sha\": \"{commit}\", \"crate_version\": \"{version}\", \"version\": 1}}\n"
        );
        write_bytes(&dir.join(MANIFEST_FILE), manifest.as_bytes());
        let manifest_sha = sha256_hex(manifest.as_bytes());
        write_sidecar(&dir.join(MANIFEST_SIDECAR), &manifest_sha);
        let binary_bytes: &[u8] = b"fixture-daemon-bytes";
        let binary_sha = sha256_hex(binary_bytes);
        let mut architectures = Vec::new();
        for (arch, target, platform) in [
            ("amd64", "x86_64-unknown-linux-gnu", "aa"),
            ("arm64", "aarch64-unknown-linux-gnu", "bb"),
        ] {
            let deb_name = format!("{package}-{version}-{arch}.deb");
            let deb = make_deb(
                &dir,
                &deb_name,
                package,
                &version,
                arch,
                binary,
                identity,
                &commit,
                &version,
                manifest.as_bytes(),
                binary_bytes,
            );
            let deb_sha = must(sha256_file(&deb), "hash the fixture deb");
            write_sidecar(&dir.join(format!("{deb_name}.sha256")), &deb_sha);
            architectures.push(format!(
                "{{\"arch\": \"{arch}\", \"target\": \"{target}\", \"binary_sha256\": \"{binary_sha}\", \"deb_sha256\": \"{deb_sha}\", \"oci_platform_digest\": \"sha256:{}\"}}",
                platform.repeat(32)
            ));
        }
        let index_hex = "cc".repeat(32);
        let record = format!(
            "{{\"schema\": \"{RELEASE_RECORD_SCHEMA}\", \"build\": {{\"repository\": \"{source}\", \"tag\": \"{tag}\", \"commit\": \"{commit}\", \"crate_version\": \"{version}\", \"debian_version\": \"{version}\", \"manifest_version\": 1, \"manifest_sha256\": \"{manifest_sha}\"}}, \"architectures\": [{}], \"oci_index_digest\": \"sha256:{index_hex}\", \"oci_image_ref\": \"ghcr.io/{source}/app@sha256:{index_hex}\", \"oci_labels\": {{\"version\": \"{version}\", \"revision\": \"{commit}\", \"source\": \"https://github.com/{source}\", \"manifest_sha256\": \"{manifest_sha}\"}}, \"apt\": {{\"origin\": \"Example\", \"suite\": \"stable\", \"component\": \"main\"}}}}",
            architectures.join(", ")
        );
        write_bytes(&dir.join(RECORD_FILE), record.as_bytes());
        write_sidecar(&dir.join(RECORD_SIDECAR), &sha256_hex(record.as_bytes()));
        let incoming = StableIncoming {
            bin: dir.join(".attestation-bin"),
            dir,
            tag,
            commit,
            source: source.to_owned(),
            package: package.to_owned(),
            binary: binary.to_owned(),
            identity: identity.to_owned(),
        };
        install_stable_gh_stub(&incoming, source, package);
        incoming
    }

    fn install_git_ref_stub(bin: &Path, source_ref: &str, commit: &str) {
        must(std::fs::create_dir_all(bin), "create git stub directory");
        write_bytes(
            &bin.join("git"),
            format!("#!/bin/sh\nprintf '{}\\t{}\\n'\n", commit, source_ref).as_bytes(),
        );
        make_executable(&bin.join("git"));
    }

    fn install_stable_gh_stub(incoming: &StableIncoming, source: &str, package: &str) {
        let mut subjects = Vec::new();
        for arch in REQUIRED_ARCHES {
            let deb = incoming
                .dir
                .join(format!("{package}-1.2.3-{arch}.deb"));
            subjects.push(serde_json::json!({
                "name": deb.file_name().and_then(|name| name.to_str()).unwrap_or(""),
                "digest": {"sha256": must(sha256_file(&deb), "hash attested deb")},
            }));
        }
        let output = serde_json::json!([{
            "verificationResult": {
                "statement": {
                    "predicateType": RELEASE_SOURCE_PREDICATE,
                    "predicate": {
                        "source_sha": incoming.commit.as_str(),
                        "release_tag": incoming.tag.as_str(),
                        "version": "1.2.3",
                        "source_repository": source,
                    },
                    "subject": subjects,
                }
            }
        }]);
        must(
            std::fs::create_dir_all(&incoming.bin),
            "create gh attestation stub directory",
        );
        let output_path = incoming.bin.join("attestation.json");
        write_bytes(
            &output_path,
            must(serde_json::to_string(&output), "serialize attestation stub").as_bytes(),
        );
        let script = format!(
            "#!/bin/sh\ncat '{}'\n",
            output_path.to_string_lossy()
        );
        write_bytes(&incoming.bin.join("gh"), script.as_bytes());
        make_executable(&incoming.bin.join("gh"));
        install_git_ref_stub(
            &incoming.bin,
            &format!("refs/tags/{}", incoming.tag),
            &incoming.commit,
        );
    }

    fn stable_verify_inputs(incoming: &StableIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Stable,
            source_repo: incoming.source.clone(),
            package: incoming.package.clone(),
            binary: incoming.binary.clone(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            identity_dir: incoming.identity.clone(),
            version: incoming.tag.clone(),
            commit: Some(incoming.commit.clone()),
            incoming: &incoming.dir,
            signer_live: FIXTURE_FPR.to_owned(),
            signer_pinned: FIXTURE_FPR.to_owned(),
            verify_oci: false,
            backend: DebBackend::Auto,
            path_overlay: Some(&incoming.bin),
        }
    }

    #[test]
    fn deb_control_fields_read_through_both_backends() {
        let dir = fixture_dir("deb-read");
        let deb = make_deb(
            &dir,
            "example-1.2.3-amd64.deb",
            "example",
            "1.2.3",
            "amd64",
            "example",
            "app",
            FIXTURE_COMMIT,
            "1.2.3",
            b"{}",
            b"bytes",
        );
        for backend in [DebBackend::Auto, DebBackend::ArTar] {
            assert_eq!(
                must(deb_control_field(&deb, "Package", backend, None), "package"),
                "example"
            );
            assert_eq!(
                must(deb_control_field(&deb, "Version", backend, None), "version"),
                "1.2.3"
            );
            assert_eq!(
                must(
                    deb_control_field(&deb, "Architecture", backend, None),
                    "arch"
                ),
                "amd64"
            );
        }
        let error = must_fail(
            deb_control_field(&deb, "Maintainer", DebBackend::ArTar, None),
            "bad field",
        );
        assert!(error.contains("not readable"), "{error}");
        let extract = dir.join("extracted");
        must(
            deb_extract_data(&deb, &extract, DebBackend::ArTar, None),
            "extract",
        );
        assert!(extract.join("usr/bin/example").is_file());
        assert!(extract.join("usr/share/app/build-identity.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stable_verify_accepts_a_coherent_release_and_arms_the_sentinel() {
        for backend in [DebBackend::Auto, DebBackend::ArTar] {
            let incoming = stable_incoming("stable-good");
            let mut inputs = stable_verify_inputs(&incoming);
            inputs.backend = backend;
            must(verify_suite(&inputs), "verify the coherent release");
            assert!(incoming.dir.join(SENTINEL_FILE).is_file());
            let _ = std::fs::remove_dir_all(&incoming.dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn verification_sentinel_rejects_symlinks_and_hardlinks_without_touching_targets() {
        let incoming = stable_incoming("sentinel-symlink");
        let victim = incoming.dir.with_file_name("sentinel-symlink-victim");
        write_bytes(&victim, b"outside sentinel target");
        must(
            std::os::unix::fs::symlink(&victim, incoming.dir.join(SENTINEL_FILE)),
            "plant symlink sentinel",
        );
        let error = must_fail(
            verify_suite(&stable_verify_inputs(&incoming)),
            "reject symlink sentinel",
        );
        assert!(error.contains("regular single-link file"), "{error}");
        assert!(std::fs::symlink_metadata(incoming.dir.join(SENTINEL_FILE))
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false));
        assert_eq!(
            must(std::fs::read(&victim), "read symlink target"),
            b"outside sentinel target"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_file(&victim);

        let incoming = stable_incoming("sentinel-hardlink");
        let victim = incoming.dir.with_file_name("sentinel-hardlink-victim");
        write_bytes(&victim, b"outside sentinel target");
        must(
            std::fs::hard_link(&victim, incoming.dir.join(SENTINEL_FILE)),
            "plant hardlink sentinel",
        );
        let error = must_fail(
            verify_suite(&stable_verify_inputs(&incoming)),
            "reject hardlink sentinel",
        );
        assert!(error.contains("regular single-link file"), "{error}");
        assert_eq!(
            must(std::fs::read(&victim), "read hardlink target"),
            b"outside sentinel target"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_file(&victim);
    }

    #[test]
    fn stable_verify_rejects_bad_source_identity_before_mutation() {
        let cases: &[(&str, &str, &str)] = &[
            ("record repository mismatch", "repository", "example/other"),
            ("record tag mismatch", "tag", "v9.9.9"),
            ("record crate_version mismatch", "crate_version", "9.9.9"),
            ("record debian_version mismatch", "debian_version", "9.9.9"),
            ("record schema mismatch", "schema", "other.schema/v9"),
        ];
        for (want, pointer, replacement) in cases {
            let incoming = stable_incoming("stable-identity");
            let path = incoming.dir.join(RECORD_FILE);
            let mut record: serde_json::Value = must(
                serde_json::from_slice(&must(std::fs::read(&path), "read record")),
                "parse",
            );
            if *pointer == "schema" {
                record["schema"] = serde_json::Value::String(replacement.to_string());
            } else {
                record["build"][pointer] = serde_json::Value::String(replacement.to_string());
            }
            // Re-sign the sidecar so the identity check (not the checksum) is
            // what fails: the tamper must be caught by coherence, not luck.
            let bytes = must(serde_json::to_vec(&record), "serialize");
            must(std::fs::write(&path, &bytes), "rewrite record");
            write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
            let inputs = stable_verify_inputs(&incoming);
            let error = must_fail(verify_suite(&inputs), want);
            assert!(error.contains(want), "{error}");
            assert!(
                !incoming.dir.join(SENTINEL_FILE).exists(),
                "rejection must leave no sentinel"
            );
            let _ = std::fs::remove_dir_all(&incoming.dir);
        }
        // A commit that disagrees with the resolved tag commit.
        let incoming = stable_incoming("stable-commit");
        let inputs = VerifyInputs {
            commit: Some("f".repeat(40)),
            ..stable_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "commit disagreement");
        assert!(error.contains("resolved tag commit"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn stable_verify_rejects_tampered_and_missing_inputs() {
        // Tampered record bytes vs the sidecar.
        let incoming = stable_incoming("stable-tamper");
        must(
            std::fs::write(incoming.dir.join(RECORD_FILE), b"{\"tampered\": true}"),
            "tamper",
        );
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "tampered record");
        assert!(error.contains("record checksum mismatch"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Tampered manifest bytes vs the sidecar.
        let incoming = stable_incoming("stable-tamper-manifest");
        must(
            std::fs::write(incoming.dir.join(MANIFEST_FILE), b"{\"tampered\": true}"),
            "tamper",
        );
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "tampered manifest");
        assert!(error.contains("manifest checksum mismatch"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Tampered deb bytes vs the sidecar.
        let incoming = stable_incoming("stable-tamper-deb");
        let deb = incoming.dir.join("example-1.2.3-amd64.deb");
        must(std::fs::write(&deb, b"not-a-deb"), "tamper deb");
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "tampered deb");
        assert!(error.contains("sidecar checksum mismatch"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Missing record.
        let incoming = stable_incoming("stable-missing");
        must(
            std::fs::remove_file(incoming.dir.join(RECORD_FILE)),
            "remove record",
        );
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "missing record");
        assert!(error.contains("required file missing"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Extra third deb.
        let incoming = stable_incoming("stable-extra");
        must(
            std::fs::copy(
                incoming.dir.join("example-1.2.3-amd64.deb"),
                incoming.dir.join("example-9.9.9-amd64.deb"),
            ),
            "plant extra deb",
        );
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "extra deb");
        assert!(error.contains("exactly 2 debs"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn stable_verify_rejects_manifest_and_oci_incoherence() {
        // Record manifest hash != sha256(manifest.json): rewrite the record
        // with a wrong hash and re-sign its sidecar.
        let incoming = stable_incoming("stable-manifest-hash");
        let path = incoming.dir.join(RECORD_FILE);
        let mut record: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&path), "read")),
            "parse",
        );
        record["build"]["manifest_sha256"] = serde_json::Value::String("00".repeat(32));
        record["oci_labels"]["manifest_sha256"] = serde_json::Value::String("00".repeat(32));
        let bytes = must(serde_json::to_vec(&record), "serialize");
        must(std::fs::write(&path, &bytes), "rewrite");
        write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "manifest hash");
        assert!(error.contains("record manifest hash"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // OCI ref that does not pin the index digest.
        let incoming = stable_incoming("stable-oci-ref");
        let path = incoming.dir.join(RECORD_FILE);
        let mut record: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&path), "read")),
            "parse",
        );
        record["oci_image_ref"] =
            serde_json::Value::String("ghcr.io/example/app:latest".to_owned());
        let bytes = must(serde_json::to_vec(&record), "serialize");
        must(std::fs::write(&path, &bytes), "rewrite");
        write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "oci ref");
        assert!(error.contains("does not pin the index digest"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // OCI label mismatches, one per label.
        for (label, value, want) in [
            ("version", "9.9.9", "oci label version mismatch"),
            ("revision", "f".repeat(40).as_str(), "oci label revision"),
            (
                "source",
                "https://github.com/example/other",
                "oci label source",
            ),
        ] {
            let incoming = stable_incoming("stable-oci-label");
            let path = incoming.dir.join(RECORD_FILE);
            let mut record: serde_json::Value = must(
                serde_json::from_slice(&must(std::fs::read(&path), "read")),
                "parse",
            );
            record["oci_labels"][label] = serde_json::Value::String(value.to_owned());
            let bytes = must(serde_json::to_vec(&record), "serialize");
            must(std::fs::write(&path, &bytes), "rewrite");
            write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
            let inputs = stable_verify_inputs(&incoming);
            let error = must_fail(verify_suite(&inputs), want);
            assert!(error.contains(want), "{error}");
            let _ = std::fs::remove_dir_all(&incoming.dir);
        }
    }

    #[test]
    fn stable_verify_rejects_arch_and_packaged_identity_defects() {
        // Record architectures that are not exactly both arches.
        let incoming = stable_incoming("stable-arches");
        let path = incoming.dir.join(RECORD_FILE);
        let mut record: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&path), "read")),
            "parse",
        );
        record["architectures"] = serde_json::json!([
            {"arch": "amd64", "target": "x86_64-unknown-linux-gnu", "binary_sha256": "00".repeat(32), "deb_sha256": "00".repeat(32), "oci_platform_digest": "sha256:00"}
        ]);
        let bytes = must(serde_json::to_vec(&record), "serialize");
        must(std::fs::write(&path, &bytes), "rewrite");
        write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "arch set");
        assert!(error.contains("not exactly {amd64, arm64}"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Packaged build-identity sha disagreement: rebuild one deb with a
        // foreign commit but keep every sidecar, so extraction catches it.
        let incoming = stable_incoming("stable-packaged");
        let manifest = must(
            std::fs::read(incoming.dir.join(MANIFEST_FILE)),
            "read manifest",
        );
        let deb = make_deb(
            &incoming.dir,
            "example-1.2.3-amd64.deb",
            FIXTURE_PACKAGE,
            "1.2.3",
            "amd64",
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            &"f".repeat(40),
            "1.2.3",
            &manifest,
            b"fixture-daemon-bytes",
        );
        // Re-sign the deb sidecar AND the record deb hash so the packaged
        // identity (not the checksum) is what fails.
        let deb_sha = must(sha256_file(&deb), "hash deb");
        write_sidecar(
            &incoming.dir.join("example-1.2.3-amd64.deb.sha256"),
            &deb_sha,
        );
        let path = incoming.dir.join(RECORD_FILE);
        let mut record: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&path), "read")),
            "parse",
        );
        record["architectures"][0]["deb_sha256"] = serde_json::Value::String(deb_sha);
        let bytes = must(serde_json::to_vec(&record), "serialize");
        must(std::fs::write(&path, &bytes), "rewrite");
        write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "packaged identity");
        assert!(error.contains("build-identity source_sha"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Extracted daemon binary hash disagreement.
        let incoming = stable_incoming("stable-extracted");
        let manifest = must(
            std::fs::read(incoming.dir.join(MANIFEST_FILE)),
            "read manifest",
        );
        let deb = make_deb(
            &incoming.dir,
            "example-1.2.3-arm64.deb",
            FIXTURE_PACKAGE,
            "1.2.3",
            "arm64",
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            FIXTURE_COMMIT,
            "1.2.3",
            &manifest,
            b"different-daemon-bytes",
        );
        let deb_sha = must(sha256_file(&deb), "hash deb");
        write_sidecar(
            &incoming.dir.join("example-1.2.3-arm64.deb.sha256"),
            &deb_sha,
        );
        let path = incoming.dir.join(RECORD_FILE);
        let mut record: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&path), "read")),
            "parse",
        );
        record["architectures"][1]["deb_sha256"] = serde_json::Value::String(deb_sha);
        let bytes = must(serde_json::to_vec(&record), "serialize");
        must(std::fs::write(&path, &bytes), "rewrite");
        write_sidecar(&incoming.dir.join(RECORD_SIDECAR), &sha256_hex(&bytes));
        let inputs = stable_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "extracted binary");
        assert!(
            error.contains("binary hash != record binary_sha256"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn stable_verify_rejects_a_rotated_signer() {
        let incoming = stable_incoming("stable-signer");
        let inputs = VerifyInputs {
            signer_live: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF".to_owned(),
            ..stable_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "signer mismatch");
        assert!(error.contains("pinned publisher key"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn stable_verify_resolves_the_tag_commit_through_a_git_stub() {
        let incoming = stable_incoming("stable-resolve");
        let bin = fixture_dir("git-stub");
        let log = bin.join("argv.log");
        write_bytes(
            &bin.join("git"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$3\" = \"refs/tags/v1.2.3^{{}}\" ]; then\n  printf '{}\\trefs/tags/v1.2.3^{{}}\\n'\nelse\n  printf '{}\\trefs/tags/v1.2.3\\n'\nfi\n",
                log.display(),
                FIXTURE_COMMIT,
                FIXTURE_COMMIT,
            )
            .as_bytes(),
        );
        must(
            std::process::Command::new("chmod")
                .args(["+x"])
                .arg(bin.join("git"))
                .status(),
            "chmod git stub",
        );
        must(
            std::fs::copy(incoming.bin.join("gh"), bin.join("gh")),
            "copy gh attestation stub",
        );
        let mut inputs = stable_verify_inputs(&incoming);
        inputs.commit = None;
        inputs.path_overlay = Some(&bin);
        must(verify_suite(&inputs), "verify with resolved commit");
        let argv = must(std::fs::read_to_string(&log), "read argv log");
        let first = argv.lines().next().unwrap_or_default();
        assert!(
            first.contains("refs/tags/v1.2.3^{}"),
            "peeled ref resolves first: {argv}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn stable_verify_rejects_a_moved_tag_even_when_the_caller_supplies_its_old_commit() {
        let incoming = stable_incoming("stable-moved-tag");
        let moved = format!("{}{}", "f".repeat(7), &FIXTURE_COMMIT[7..]);
        install_git_ref_stub(
            &incoming.bin,
            "refs/tags/v1.2.3",
            &moved,
        );
        let error = must_fail(
            verify_suite(&stable_verify_inputs(&incoming)),
            "reject a moved stable tag",
        );
        assert!(error.contains("independently resolved current tag commit"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn stable_verify_rejects_a_malformed_commit_before_any_fetch() {
        let incoming = stable_incoming("stable-bad-commit");
        let inputs = VerifyInputs {
            commit: Some("not-hex".to_owned()),
            ..stable_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "malformed commit");
        assert!(error.contains("40 lowercase hex"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    struct PreviewIncoming {
        dir: PathBuf,
        bin: PathBuf,
        version: String,
        commit: String,
    }

    /// Build a coherent preview incoming directory for the default fixture
    /// identity.
    fn preview_incoming(root: &str) -> PreviewIncoming {
        preview_incoming_for_ref(
            root,
            FIXTURE_SOURCE,
            FIXTURE_PACKAGE,
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            PREVIEW_SOURCE_REF,
        )
    }

    fn preview_incoming_renamed(
        root: &str,
        source: &str,
        package: &str,
        binary: &str,
        identity: &str,
    ) -> PreviewIncoming {
        preview_incoming_for_ref(
            root,
            source,
            package,
            binary,
            identity,
            PREVIEW_SOURCE_REF,
        )
    }

    fn preview_incoming_for_ref(
        root: &str,
        source: &str,
        package: &str,
        binary: &str,
        identity: &str,
        source_ref: &str,
    ) -> PreviewIncoming {
        let dir = fixture_dir(root);
        let version = "1.2.3~preview.41+0123456".to_owned();
        let commit = FIXTURE_COMMIT.to_owned();
        let dotted = dotted_asset_version(&version);
        let mut assets = Vec::new();
        let mut sums = Vec::new();
        for arch in REQUIRED_ARCHES {
            let deb_name = format!("{package}-preview-{dotted}-{arch}.deb");
            let deb = make_deb(
                &dir,
                &deb_name,
                package,
                &version,
                arch,
                binary,
                identity,
                &commit,
                "1.2.3",
                b"{}",
                b"fixture-daemon-bytes",
            );
            let deb_sha = must(sha256_file(&deb), "hash the fixture deb");
            // Digest-only sidecars, the rolling-release shape: the digest
            // binds the file while SHA256SUMS pins the name.
            write_bytes(
                &dir.join(format!("{deb_name}.sha256")),
                format!("{deb_sha}\n").as_bytes(),
            );
            let release_name = format!("{package}-preview-{version}-{arch}.deb");
            assets.push(format!(
                "{{\"name\": \"{release_name}\", \"sha256\": \"{deb_sha}\"}}"
            ));
            sums.push(format!("{deb_sha}  {release_name}"));
        }
        sums.sort();
        let manifest = format!(
            "{{\"schema\": \"{FIXTURE_SCHEMA}\", \"source_repository\": \"{source}\", \"source_ref\": \"{source_ref}\", \"source_commit\": \"{commit}\", \"release_tag\": \"preview-{}\", \"version\": \"{version}\", \"assets\": [{}]}}\n",
            dotted_asset_version(&version),
            assets.join(", ")
        );
        write_bytes(&dir.join(PREVIEW_MANIFEST_FILE), manifest.as_bytes());
        write_bytes(
            &dir.join(SHA256SUMS_FILE),
            format!("{}\n", sums.join("\n")).as_bytes(),
        );
        let incoming = PreviewIncoming {
            bin: dir.join(".git-bin"),
            dir,
            version,
            commit,
        };
        install_git_ref_stub(&incoming.bin, source_ref, &incoming.commit);
        incoming
    }

    fn preview_verify_inputs(incoming: &PreviewIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Preview,
            source_repo: FIXTURE_SOURCE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            binary: FIXTURE_BINARY.to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            identity_dir: FIXTURE_IDENTITY.to_owned(),
            version: incoming.version.clone(),
            commit: Some(incoming.commit.clone()),
            incoming: &incoming.dir,
            signer_live: FIXTURE_FPR.to_owned(),
            signer_pinned: FIXTURE_FPR.to_owned(),
            verify_oci: false,
            backend: DebBackend::Auto,
            path_overlay: Some(&incoming.bin),
        }
    }

    fn preview_verify_inputs_for_ref(
        incoming: &PreviewIncoming,
        preview_source_ref: &str,
    ) -> VerifyInputs<'_> {
        let mut inputs = preview_verify_inputs(incoming);
        inputs.preview_source_ref = preview_source_ref.to_owned();
        inputs
    }

    #[test]
    fn preview_verify_accepts_a_coherent_rolling_release() {
        for backend in [DebBackend::Auto, DebBackend::ArTar] {
            let incoming = preview_incoming("preview-good");
            let mut inputs = preview_verify_inputs(&incoming);
            inputs.backend = backend;
            must(verify_suite(&inputs), "verify the coherent preview");
            assert!(incoming.dir.join(SENTINEL_FILE).is_file());
            let _ = std::fs::remove_dir_all(&incoming.dir);
        }
    }

    #[test]
    fn preview_verify_rejects_a_moved_source_ref_with_the_same_version_prefix() {
        let incoming = preview_incoming("preview-moved-ref");
        let moved = format!("{}{}", &FIXTURE_COMMIT[..7], "f".repeat(33));
        install_git_ref_stub(&incoming.bin, PREVIEW_SOURCE_REF, &moved);
        let error = must_fail(
            verify_suite(&preview_verify_inputs(&incoming)),
            "reject a moved preview source ref",
        );
        assert!(error.contains("current configured source ref head"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn preview_verification_and_channel_state_share_the_configured_branch() {
        let source_ref = "refs/heads/trunk";
        let incoming = preview_incoming_for_ref(
            "preview-trunk",
            FIXTURE_SOURCE,
            FIXTURE_PACKAGE,
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            source_ref,
        );
        let inputs = preview_verify_inputs_for_ref(&incoming, source_ref);
        must(
            verify_suite(&inputs),
            "verify a preview pinned to the configured non-main branch",
        );

        let root = fixture_dir("channel-preview-trunk");
        staged_pool_with_candidate(&root, Suite::Preview, PREVIEW_CANDIDATE);
        let update = ChannelUpdateInputs {
            suite: Suite::Preview,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: source_ref.to_owned(),
            preview_source_ref: source_ref.to_owned(),
            commit: FIXTURE_COMMIT.to_owned(),
            version: PREVIEW_CANDIDATE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(PREVIEW_MANIFEST_FILE),
            staging: &root,
        };
        must(
            run_channel_update(&update),
            "write channel state with the same preview source ref",
        );
        let state: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join(Suite::Preview.channel_state_file())),
                "read preview channel state",
            )),
            "parse preview channel state",
        );
        assert_eq!(state["source_ref"], source_ref);

        let wrong_incoming = preview_incoming_for_ref(
            "preview-trunk-rejected",
            FIXTURE_SOURCE,
            FIXTURE_PACKAGE,
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            source_ref,
        );
        let wrong_ref = preview_verify_inputs(&wrong_incoming);
        let error = must_fail(
            verify_suite(&wrong_ref),
            "the hardcoded main ref must not verify a trunk manifest",
        );
        assert!(error.contains("source_ref"), "{error}");
        assert!(
            !wrong_incoming.dir.join(SENTINEL_FILE).exists(),
            "a ref mismatch must not arm publication"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&wrong_incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_verify_rejects_identity_and_grammar_defects() {
        // Missing commit.
        let incoming = preview_incoming("preview-no-commit");
        let inputs = VerifyInputs {
            commit: None,
            ..preview_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "missing commit");
        assert!(error.contains("--commit is required"), "{error}");
        assert!(!incoming.dir.join(SENTINEL_FILE).exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Suffix that does not match the commit.
        let incoming = preview_incoming("preview-suffix");
        let inputs = VerifyInputs {
            commit: Some(format!("fffffff{}", &FIXTURE_COMMIT[7..])),
            ..preview_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "suffix mismatch");
        assert!(
            error.contains("does not match the source commit"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Grammar violation.
        let incoming = preview_incoming("preview-grammar");
        let inputs = VerifyInputs {
            version: "v1.2.3~preview.41+0123456".to_owned(),
            ..preview_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "grammar");
        assert!(error.contains("X.Y.Z~preview.N"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Manifest version/source/commit mismatches.
        for (pointer, replacement, want) in [
            (
                "version",
                "9.9.9~preview.1+0123456",
                "release-manifest version mismatch",
            ),
            (
                "source_repository",
                "example/other",
                "release-manifest repository mismatch",
            ),
            (
                "source_ref",
                "refs/heads/other",
                "release-manifest source_ref",
            ),
            (
                "source_commit",
                "f".repeat(40).as_str(),
                "release-manifest source_commit",
            ),
            (
                "schema",
                "other.schema/v9",
                "release-manifest schema mismatch",
            ),
        ] {
            let incoming = preview_incoming("preview-manifest");
            let path = incoming.dir.join(PREVIEW_MANIFEST_FILE);
            let mut manifest: serde_json::Value = must(
                serde_json::from_slice(&must(std::fs::read(&path), "read")),
                "parse",
            );
            manifest[pointer] = serde_json::Value::String(replacement.to_owned());
            must(
                std::fs::write(&path, must(serde_json::to_vec(&manifest), "serialize")),
                "rewrite",
            );
            let inputs = preview_verify_inputs(&incoming);
            let error = must_fail(verify_suite(&inputs), want);
            assert!(error.contains(want), "{error}");
            assert!(!incoming.dir.join(SENTINEL_FILE).exists());
            let _ = std::fs::remove_dir_all(&incoming.dir);
        }

        // Live OCI verification does not apply to previews.
        let incoming = preview_incoming("preview-oci");
        let inputs = VerifyInputs {
            verify_oci: true,
            ..preview_verify_inputs(&incoming)
        };
        let error = must_fail(verify_suite(&inputs), "preview oci");
        assert!(error.contains("--verify-oci does not apply"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn preview_verify_rejects_sidecar_and_control_defects() {
        // Sidecar that misnames its deb.
        let incoming = preview_incoming("preview-misname");
        let sum = incoming
            .dir
            .join("example-preview-1.2.3.preview.41+0123456-amd64.deb.sha256");
        let digest = must(sidecar_digest(&sum), "read sidecar");
        write_bytes(&sum, format!("{digest}  wrong-name.deb\n").as_bytes());
        let inputs = preview_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "misnamed sidecar");
        assert!(error.contains("does not name"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Multi-line sidecar.
        let incoming = preview_incoming("preview-multiline");
        let sum = incoming
            .dir
            .join("example-preview-1.2.3.preview.41+0123456-amd64.deb.sha256");
        let digest = must(sidecar_digest(&sum), "read sidecar");
        write_bytes(&sum, format!("{digest}\n{digest}\n").as_bytes());
        let inputs = preview_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "multi-line sidecar");
        assert!(error.contains("must be a single line"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Non-hex sidecar digest.
        let incoming = preview_incoming("preview-nonhex");
        let sum = incoming
            .dir
            .join("example-preview-1.2.3.preview.41+0123456-amd64.deb.sha256");
        write_bytes(&sum, b"not-a-digest\n");
        let inputs = preview_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "non-hex sidecar");
        assert!(error.contains("not 64 lowercase hex"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);

        // Control Architecture mismatch: rebuild one deb for the wrong arch.
        let incoming = preview_incoming("preview-control");
        let name = "example-preview-1.2.3.preview.41+0123456-amd64.deb";
        let deb = make_deb(
            &incoming.dir,
            name,
            FIXTURE_PACKAGE,
            "1.2.3~preview.41+0123456",
            "arm64",
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            FIXTURE_COMMIT,
            "1.2.3",
            b"{}",
            b"fixture-daemon-bytes",
        );
        // Rebind every checksum so the control field is what fails.
        let deb_sha = must(sha256_file(&deb), "hash deb");
        write_bytes(
            &incoming.dir.join(format!("{name}.sha256")),
            format!("{deb_sha}\n").as_bytes(),
        );
        let sums = incoming.dir.join(SHA256SUMS_FILE);
        let tilde = "example-preview-1.2.3~preview.41+0123456-amd64.deb";
        let text = must(std::fs::read_to_string(&sums), "read sums");
        let rewritten: Vec<String> = text
            .lines()
            .map(|line| {
                if line.ends_with(tilde) {
                    format!("{deb_sha}  {tilde}")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        write_bytes(&sums, format!("{}\n", rewritten.join("\n")).as_bytes());
        let manifest_path = incoming.dir.join(PREVIEW_MANIFEST_FILE);
        let mut manifest: serde_json::Value = must(
            serde_json::from_slice(&must(std::fs::read(&manifest_path), "read")),
            "parse",
        );
        if let Some(assets) = manifest["assets"].as_array_mut() {
            for asset in assets {
                if asset["name"].as_str() == Some(tilde) {
                    asset["sha256"] = serde_json::Value::String(deb_sha.clone());
                }
            }
        }
        must(
            std::fs::write(
                &manifest_path,
                must(serde_json::to_vec(&manifest), "serialize"),
            ),
            "rewrite",
        );
        let inputs = preview_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "control arch");
        assert!(error.contains("Architecture != amd64"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn preview_verify_rejects_a_truncated_sums_list() {
        // SHA256SUMS with the wrong line count.
        let incoming = preview_incoming("preview-sums");
        must(
            std::fs::write(
                incoming.dir.join(SHA256SUMS_FILE),
                format!("{}  only-one.deb\n", "00".repeat(32)),
            ),
            "truncate sums",
        );
        let inputs = preview_verify_inputs(&incoming);
        let error = must_fail(verify_suite(&inputs), "sums count");
        assert!(
            error.contains("not pinned by SHA256SUMS") || error.contains("exactly the two"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    #[test]
    fn renamed_fixtures_verify_identically() {
        // The genericity proof: a renamed package, source repository,
        // binary, and identity directory flow through with zero name-keyed
        // branches — the same checks pass on renamed bytes.
        let incoming = stable_incoming_renamed(
            "renamed-stable",
            "acme/widget",
            "widget",
            "widgetd",
            "wident",
        );
        let inputs = VerifyInputs {
            source_repo: "acme/widget".to_owned(),
            package: "widget".to_owned(),
            binary: "widgetd".to_owned(),
            identity_dir: "wident".to_owned(),
            ..stable_verify_inputs(&incoming)
        };
        must(verify_suite(&inputs), "verify the renamed stable release");
        assert!(incoming.dir.join(SENTINEL_FILE).is_file());
        let _ = std::fs::remove_dir_all(&incoming.dir);

        let incoming = preview_incoming_renamed(
            "renamed-preview",
            "acme/widget",
            "widget",
            "widgetd",
            "wident",
        );
        let inputs = VerifyInputs {
            source_repo: "acme/widget".to_owned(),
            package: "widget".to_owned(),
            binary: "widgetd".to_owned(),
            identity_dir: "wident".to_owned(),
            ..preview_verify_inputs(&incoming)
        };
        must(verify_suite(&inputs), "verify the renamed preview");
        assert!(incoming.dir.join(SENTINEL_FILE).is_file());
        let _ = std::fs::remove_dir_all(&incoming.dir);
    }

    // -- publication fixtures: hermetic tool stubs ---------------------------
    //
    // The stubs below emulate `apt-ftparchive`/`gpg`/`gpgconf` just far
    // enough to prove MY argv construction, output parsing, staging, and
    // refusal logic: canned outputs come from files inside the test's own
    // log directory (embedded in the stub, never environment variables, so
    // parallel tests cannot race), and every invocation is appended to a
    // per-test argv log. The real tools' own correctness is Debian's and
    // GnuPG's business, not this module's.

    use std::sync::{Mutex, OnceLock};

    static CWD_LOCK: Mutex<()> = Mutex::new(());
    static ORIG_CWD: OnceLock<PathBuf> = OnceLock::new();

    /// Run `run` with the process working directory set to `root`. Serialized
    /// by a mutex because the working directory is process-global; only
    /// publication tests (which require relative staging paths) use this.
    fn in_fixture_root<T>(root: &Path, run: impl FnOnce() -> T) -> T {
        let _guard = must(CWD_LOCK.lock().map_err(|_| "cwd lock poisoned"), "lock cwd");
        let orig = ORIG_CWD.get_or_init(|| must(std::env::current_dir(), "read cwd"));
        must(std::env::set_current_dir(root), "enter fixture root");
        let result = run();
        must(std::env::set_current_dir(orig), "leave fixture root");
        result
    }

    struct ToolStubs {
        bin: PathBuf,
        log: PathBuf,
    }

    fn make_executable(path: &Path) {
        let status = must(
            std::process::Command::new("chmod")
                .args(["+x"])
                .arg(path)
                .status(),
            "chmod the stub",
        );
        assert!(status.success(), "chmod {}", path.display());
    }

    fn tool_stubs(root: &str) -> ToolStubs {
        let base = fixture_dir(root);
        let bin = base.join("bin");
        let log = base.join("log");
        must(std::fs::create_dir_all(&bin), "stub bin");
        must(std::fs::create_dir_all(&log), "stub log");
        write_bytes(
            &bin.join("apt-ftparchive"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"apt-ftparchive $*\" >> \"{}/apt.log\"\nif [ \"$1\" = \"-a\" ]; then cat \"{}/packages-$2\"; exit \"$?\"; fi\ncat \"{}/release\"\n",
                log.display(),
                log.display(),
                log.display()
            )
            .as_bytes(),
        );
        write_bytes(
            &bin.join("gpg"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"gpg $*\" >> \"{0}/gpg.log\"\n",
                    "case \" $* \" in\n",
                    "  *\" --import \"*) cat >/dev/null; exit 0 ;;\n",
                    "  *\" --list-secret-keys \"*) printf 'sec:-:2048:1:{1}:0:\\n'; printf 'fpr:::::::::{1}:\\n'; exit 0 ;;\n",
                    "esac\n",
                    "# Every remaining shape carries piped stdin (passphrase on\n",
                    "# fd 0): drain it like the real tool so the parent's write\n",
                    "# never EPIPEs against an already-exited stub.\n",
                    "cat >/dev/null\n",
                    "output=\"\"; input=\"\"; clearsign=0; prev=\"\"\n",
                    "for arg in \"$@\"; do\n",
                    "  case \"$prev\" in\n",
                    "    --output) output=\"$arg\" ;;\n",
                    "    --detach-sign) input=\"$arg\" ;;\n",
                    "    --clearsign) input=\"$arg\"; clearsign=1 ;;\n",
                    "  esac\n",
                    "  prev=\"$arg\"\n",
                    "done\n",
                    "[ \"$output\" = \"/dev/null\" ] && exit 0\n",
                    "[ -n \"$output\" ] || exit 1\n",
                    "[ -f \"$input\" ] || exit 1\n",
                    "if [ \"$clearsign\" = 1 ]; then\n",
                    "  {{ printf '-----BEGIN PGP SIGNED MESSAGE-----\\n\\n'; cat \"$input\"; printf '\\n-----BEGIN PGP SIGNATURE-----\\nfixture\\n-----END PGP SIGNATURE-----\\n'; }} > \"$output\"\n",
                    "else\n",
                    "  {{ printf 'fixture-signature:'; cat \"$input\"; }} > \"$output\"\n",
                    "fi\n",
                    "exit 0\n",
                ),
                log.display(),
                FIXTURE_FPR
            )
            .as_bytes(),
        );
        write_bytes(
            &bin.join("gpgconf"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"gpgconf $*\" >> \"{}/gpgconf.log\"\nexit 0\n",
                log.display()
            )
            .as_bytes(),
        );
        write_bytes(
            &bin.join("curl"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"curl $*\" >> \"{0}/curl.log\"\n",
                    "output=; url=; write_out=0; previous=\n",
                    "for arg in \"$@\"; do\n",
                    "  case \"$previous\" in --output) output=\"$arg\" ;; --write-out) write_out=1 ;; esac\n",
                    "  if [ \"$previous\" = -- ]; then url=\"$arg\"; fi\n",
                    "  previous=\"$arg\"\n",
                    "done\n",
                    "case \"$url\" in https://feed.example.test/*) relative=\"${{url#https://feed.example.test/}}\" ;; *) echo \"unexpected curl URL: $url\" >&2; exit 2 ;; esac\n",
                    "source=\"{0}/feed/$relative\"\n",
                    "if [ \"$write_out\" = 1 ]; then\n",
                    "  if [ -f \"$source\" ]; then cp \"$source\" \"$output\" || exit 1; printf '200'; else rm -f \"$output\"; printf '404'; fi\n",
                    "  exit 0\n",
                    "fi\n",
                    "[ -n \"$output\" ] && [ -f \"$source\" ] || exit 22\n",
                    "cp \"$source\" \"$output\"\n"
                ),
                log.display()
            )
            .as_bytes(),
        );
        write_bytes(
            &bin.join("gpgv"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"gpgv $*\" >> \"{0}/gpgv.log\"\n",
                    "output=; previous=\n",
                    "for arg in \"$@\"; do\n",
                    "  case \"$previous\" in --output) output=\"$arg\" ;; esac\n",
                    "  previous=\"$arg\"\n",
                    "done\n",
                    "if [ -n \"$output\" ]; then [ -f \"{0}/signed-release\" ] || exit 1; cp \"{0}/signed-release\" \"$output\" || exit 1; fi\n",
                    "printf '[GNUPG:] VALIDSIG %s 20240101 0 0 4 0 1 8 00\\n' \"$(cat \"{0}/signing-fpr\")\"\n"
                ),
                log.display()
            )
            .as_bytes(),
        );
        make_executable(&bin.join("apt-ftparchive"));
        make_executable(&bin.join("gpg"));
        make_executable(&bin.join("gpgconf"));
        make_executable(&bin.join("curl"));
        make_executable(&bin.join("gpgv"));
        ToolStubs { bin, log }
    }

    fn install_feed_fetch_stubs(stubs: &ToolStubs) {
        write_bytes(
            &stubs.bin.join("curl"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"curl $*\" >> \"{0}/feed-curl.log\"\n",
                    "output=; url=; write_out=0; previous=\n",
                    "for arg in \"$@\"; do\n",
                    "  case \"$previous\" in --output) output=\"$arg\" ;; --write-out) write_out=1 ;; esac\n",
                    "  if [ \"$previous\" = -- ]; then url=\"$arg\"; fi\n",
                    "  previous=\"$arg\"\n",
                    "done\n",
                    "case \"$url\" in https://feed.example.test/*) relative=\"${{url#https://feed.example.test/}}\" ;; *) exit 2 ;; esac\n",
                    "source=\"{0}/feed/$relative\"\n",
                    "if [ \"$write_out\" = 1 ]; then\n",
                    "  if [ -f \"$source\" ]; then cp \"$source\" \"$output\" || exit 1; printf '200'; else rm -f \"$output\"; printf '404'; fi\n",
                    "  exit 0\n",
                    "fi\n",
                    "[ -n \"$output\" ] && [ -f \"$source\" ] || exit 22\n",
                    "cp \"$source\" \"$output\"\n"
                ),
                stubs.log.display()
            )
            .as_bytes(),
        );
        write_bytes(
            &stubs.bin.join("gpgv"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"gpgv $*\" >> \"{0}/feed-gpgv.log\"\n",
                    "output=; previous=\n",
                    "for arg in \"$@\"; do case \"$previous\" in --output) output=\"$arg\" ;; esac; previous=\"$arg\"; done\n",
                    "if [ -n \"$output\" ]; then cp \"{0}/signed-release\" \"$output\" || exit 1; fi\n",
                    "printf '[GNUPG:] VALIDSIG %s 20240101 0 0 4 0 1 8 00\\n' \"$(cat \"{0}/signing-fpr\")\"\n"
                ),
                stubs.log.display()
            )
            .as_bytes(),
        );
        make_executable(&stubs.bin.join("curl"));
        make_executable(&stubs.bin.join("gpgv"));
    }

    fn install_dynamic_stable_index_stub(stubs: &ToolStubs, contract: &AptContract) {
        write_bytes(
            &stubs.bin.join("apt-ftparchive"),
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"apt-ftparchive $*\" >> \"{0}/apt.log\"\n",
                    "if [ \"$1\" = -a ]; then cat \"{0}/packages-$2\"; exit \"$?\"; fi\n",
                    "printf 'Origin: {1}\\nLabel: {1}\\nSuite: stable\\nCodename: stable\\nArchitectures: amd64 arm64\\nComponents: main\\nSHA256:\\n'\n",
                    "for arch in amd64 arm64; do\n",
                    "  for leaf in Packages Packages.gz; do\n",
                    "    file=\"dists/stable/main/binary-$arch/$leaf\"\n",
                    "    digest=$(shasum -a 256 \"$file\" | awk '{{print $1}}') || exit 1\n",
                    "    size=$(wc -c < \"$file\" | tr -d '[:space:]') || exit 1\n",
                    "    printf ' %s %s main/binary-%s/%s\\n' \"$digest\" \"$size\" \"$arch\" \"$leaf\"\n",
                    "  done\n",
                    "done\n"
                ),
                stubs.log.display(),
                contract.origin
            )
            .as_bytes(),
        );
        make_executable(&stubs.bin.join("apt-ftparchive"));
    }

    /// Prove no signature was attempted: the key import and agreement listing
    /// are pre-mutation validation and may have run, but no signing verb may
    /// appear in the stub log.
    fn assert_no_signing_attempted(stubs: &ToolStubs) {
        let log_path = stubs.log.join("gpg.log");
        if !log_path.exists() {
            return;
        }
        let log = must(std::fs::read_to_string(&log_path), "read gpg log");
        assert!(
            !log.contains("--detach-sign") && !log.contains("--clearsign"),
            "no signing may be attempted: {log}"
        );
    }

    #[test]
    fn gpg_stub_drains_piped_standard_input() {
        // The stub must consume stdin exactly like the real tool: `gpg
        // --import` reads key material to EOF and `--passphrase-fd 0`
        // reads the passphrase. A stub that exits without reading races
        // the parent's write — under CI load the write lands after the
        // exit, EPIPEs, and the publish flow fails spuriously ("gpg
        // refused standard input"). Input past the 64 KiB pipe buffer
        // makes the race deterministic: the buffer fills and the rest
        // has nowhere to go once a non-draining stub exits.
        let stubs = tool_stubs("gpg-stub-drains-stdin");
        let flood = vec![b'k'; 1024 * 1024];
        must(
            run_fixed(
                "gpg",
                &[
                    "--batch".to_owned(),
                    "--homedir".to_owned(),
                    "unused".to_owned(),
                    "--import".to_owned(),
                ],
                Some(&flood),
                Some(&stubs.bin),
            ),
            "the import stub drains stdin",
        );
        must(
            run_fixed(
                "gpg",
                &[
                    "--batch".to_owned(),
                    "--homedir".to_owned(),
                    "unused".to_owned(),
                    "--yes".to_owned(),
                    "--pinentry-mode".to_owned(),
                    "loopback".to_owned(),
                    "--passphrase-fd".to_owned(),
                    "0".to_owned(),
                    "--local-user".to_owned(),
                    FIXTURE_IDENTITY.to_owned(),
                    "--output".to_owned(),
                    "/dev/null".to_owned(),
                    "--detach-sign".to_owned(),
                    "/dev/null".to_owned(),
                ],
                Some(&flood),
                Some(&stubs.bin),
            ),
            "the prime stub drains stdin",
        );
        // The file-signing shape drains too: passphrase on stdin, the
        // release on a file argument.
        let input = stubs.log.join("Release");
        write_bytes(&input, b"release-bytes");
        let output = stubs.log.join("Release.gpg");
        let input_name = must(input.to_str().ok_or("input utf8"), "input utf8");
        let output_name = must(output.to_str().ok_or("output utf8"), "output utf8");
        must(
            run_fixed(
                "gpg",
                &gpg_detach_argv(FIXTURE_IDENTITY, "unused", output_name, input_name, true),
                Some(&flood),
                Some(&stubs.bin),
            ),
            "the signing stub drains stdin",
        );
        assert!(output.is_file(), "the stub still signs");
        let base = must(stubs.log.parent().ok_or("stub base"), "stub base").to_path_buf();
        let _ = std::fs::remove_dir_all(&base);
    }

    fn canned_packages(package: &str, arch: &str, versions: &[&str]) -> String {
        versions
            .iter()
            .map(|version| {
                format!(
                    "Package: {package}\nVersion: {version}\nArchitecture: {arch}\nFilename: pool/main/e/{package}/{package}_{version}_{arch}.deb\n"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Build a rollback prev pair of asset-named debs under `root/prev`.
    fn rollback_prev_dir(root: &Path, version: &str, commit: &str) -> PathBuf {
        let prev = root.join("prev");
        must(std::fs::create_dir_all(&prev), "prev dir");
        for arch in REQUIRED_ARCHES {
            make_deb(
                &prev,
                &format!("{FIXTURE_PACKAGE}-{version}-{arch}.deb"),
                FIXTURE_PACKAGE,
                version,
                arch,
                FIXTURE_BINARY,
                FIXTURE_IDENTITY,
                commit,
                "1.2.2",
                b"{}",
                b"rollback-daemon-bytes",
            );
        }
        prev
    }

    /// Write a coherent stable previous pointer naming `tag`.
    fn stable_pointer_file(root: &Path, tag: &str) {
        write_bytes(
            &root.join("previous-pointer.json"),
            format!(
                "{{\"tag\": \"{tag}\", \"source_record_sha256\": \"{}\"}}\n",
                "dd".repeat(32)
            )
            .as_bytes(),
        );
    }

    /// The deterministic stable pool, indexes, and signatures.
    fn assert_stable_pool(root: &Path) {
        for arch in REQUIRED_ARCHES {
            for version in ["1.2.3", "1.2.2"] {
                assert!(
                    root.join(format!(
                        "public/pool/main/e/example/example_{version}_{arch}.deb"
                    ))
                    .is_file(),
                    "pool holds {version}/{arch}"
                );
            }
            assert!(root
                .join(format!("public/dists/stable/main/binary-{arch}/Packages"))
                .is_file());
            assert!(root
                .join(format!(
                    "public/dists/stable/main/binary-{arch}/Packages.gz"
                ))
                .is_file());
        }
        assert!(root.join("public/dists/stable/Release").is_file());
        assert!(root.join("public/dists/stable/InRelease").is_file());
        assert!(root.join("public/dists/stable/Release.gpg").is_file());
        assert!(root.join("public/publication-record.json.sig").is_file());
        assert!(root.join("public/example.gpg").is_file(), "keyring staged");
    }

    /// The emitted stable publication record, last-publish, and stanza.
    fn assert_stable_records(root: &Path) {
        let record: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join("public/publication-record.json")),
                "read record",
            )),
            "parse record",
        );
        let parsed = must(
            parse_publication_record(&record),
            "parse the emitted record",
        );
        assert_eq!(parsed.tag, "v1.2.3");
        assert_eq!(parsed.crate_version, "1.2.3");
        assert_eq!(parsed.suite, None);
        assert_eq!(parsed.signer_fingerprint, FIXTURE_FPR);
        assert_eq!(
            must(
                std::fs::read_to_string(root.join("public/last-publish")),
                "read last-publish"
            ),
            "v1.2.3\n"
        );
        let distributions = must(
            std::fs::read_to_string(root.join("public/conf/distributions")),
            "read distributions",
        );
        assert!(distributions.contains("Origin: example"), "{distributions}");
        assert!(
            distributions.contains("Codename: stable"),
            "{distributions}"
        );
        assert!(
            distributions.contains(&format!("SignWith: {FIXTURE_FPR}")),
            "{distributions}"
        );
    }

    /// The fixed tool invocations behind the stable publication.
    fn assert_stable_tool_calls(stubs: &ToolStubs) {
        let apt_log = must(
            std::fs::read_to_string(stubs.log.join("apt.log")),
            "read apt log",
        );
        assert!(apt_log.contains("-a amd64 packages pool"), "{apt_log}");
        assert!(
            apt_log.contains("APT::FTPArchive::Release::Origin=example"),
            "{apt_log}"
        );
        assert!(
            apt_log.contains("APT::FTPArchive::Release::Suite=stable"),
            "{apt_log}"
        );
        let gpg_log = must(
            std::fs::read_to_string(stubs.log.join("gpg.log")),
            "read gpg log",
        );
        assert!(
            gpg_log.contains(&format!("--local-user {FIXTURE_FPR}")),
            "{gpg_log}"
        );
        assert!(gpg_log.contains("--passphrase-fd 0"), "{gpg_log}");
    }

    /// Can the same index for both arches plus a trivial Release.
    fn can_both_arches(stubs: &ToolStubs, versions: &[&str]) {
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages(FIXTURE_PACKAGE, arch, versions).as_bytes(),
            );
        }
        write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
    }

    /// Configure a hermetic live feed whose signature covers Release, each
    /// Packages file, and each rollback deb's exact size and digest.
    fn configure_signed_rollback_feed(
        stubs: &ToolStubs,
        suite: Suite,
        contract: &AptContract,
        candidate: &str,
        prev_dir: &Path,
    ) {
        let candidate_version = match suite {
            Suite::Stable => must(parse_stable_tag(candidate), "parse candidate tag").version,
            Suite::Preview => candidate.to_owned(),
        };
        let log = &stubs.log;
        let feed = log.join("feed");
        let inrelease = b"fixture signed InRelease bytes";
        write_bytes(&log.join("InRelease"), inrelease);
        write_bytes(
            &feed.join(format!("dists/{}/InRelease", suite.as_str())),
            inrelease,
        );
        write_bytes(&log.join("signing-fpr"), FIXTURE_FPR.as_bytes());
        let release_date = OffsetDateTime::now_utc() - time::Duration::minutes(10);
        let valid_until = OffsetDateTime::now_utc() + time::Duration::days(7);
        let date_text = must(release_date.format(&Rfc2822), "format fixture Release Date");
        let valid_text = must(valid_until.format(&Rfc2822), "format fixture Valid-Until");
        let mut release = format!(
            "Origin: {0}\nLabel: {0}\nSuite: {1}\nCodename: {1}\nArchitectures: amd64 arm64\nComponents: main\nDate: {2}\nValid-Until: {3}\nSHA256:\n",
            contract.origin,
            suite.as_str(),
            date_text,
            valid_text,
        );
        for arch in REQUIRED_ARCHES {
            let mut selected: Option<(PathBuf, String)> = None;
            for name in dir_names(prev_dir).unwrap_or_default() {
                if !is_deb_file(&name)
                    || name
                        == rollback_asset_name(suite, &contract.package, &candidate_version, arch)
                {
                    continue;
                }
                let path = prev_dir.join(&name);
                let Ok(file_type) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !file_type.file_type().is_file() {
                    continue;
                }
                let Ok(package) =
                    deb_control_field(&path, "Package", DebBackend::Auto, Some(&stubs.bin))
                else {
                    continue;
                };
                let Ok(version) =
                    deb_control_field(&path, "Version", DebBackend::Auto, Some(&stubs.bin))
                else {
                    continue;
                };
                let Ok(found_arch) =
                    deb_control_field(&path, "Architecture", DebBackend::Auto, Some(&stubs.bin))
                else {
                    continue;
                };
                if package == contract.package && version != candidate_version && found_arch == arch
                {
                    selected = Some((path, version));
                    break;
                }
            }
            let (path, version) = must(
                selected.ok_or("fixture rollback deb missing"),
                "select rollback deb",
            );
            let size = must(std::fs::metadata(&path), "rollback size").len();
            let deb_sha = must(sha256_file(&path), "rollback SHA256");
            let pool_name = canonical_pool_name(&contract.package, &version, arch);
            let filename = match suite {
                Suite::Stable => format!(
                    "pool/main/{}/{}/{}",
                    pool_letter(&contract.package),
                    contract.package,
                    pool_name
                ),
                Suite::Preview => format!(
                    "pool/preview/main/{}/{}/{}",
                    pool_letter(&contract.package),
                    contract.package,
                    pool_name
                ),
            };
            let packages = format!(
                "Package: {}\nVersion: {}\nArchitecture: {}\nFilename: {}\nSize: {}\nSHA256: {}\n\n",
                contract.package, version, arch, filename, size, deb_sha
            );
            let packages_path = log.join(format!("live-packages-{arch}"));
            write_bytes(&packages_path, packages.as_bytes());
            write_bytes(
                &feed.join(format!(
                    "dists/{}/main/binary-{arch}/Packages",
                    suite.as_str()
                )),
                packages.as_bytes(),
            );
            write_bytes(
                &feed.join(&filename),
                &must(std::fs::read(&path), "read fixture rollback package"),
            );
            let package_bytes = must(std::fs::read(&packages_path), "read fixture Packages");
            let index_sha = sha256_hex(&package_bytes);
            let index_size = package_bytes.len();
            let _ = writeln!(
                release,
                " {index_sha} {index_size} main/binary-{arch}/Packages"
            );
        }
        write_bytes(&log.join("signed-release"), release.as_bytes());
    }

    fn write_fixture_live_publication_record(
        stubs: &ToolStubs,
        root: &Path,
        suite: Suite,
        contract: &AptContract,
        pointer_path: &Path,
    ) -> (&'static Path, &'static Path) {
        let packages = must(
            std::fs::read_to_string(stubs.log.join("live-packages-amd64")),
            "read fixture live amd64 Packages",
        );
        let version = must(
            highest_version(suite, packages_versions(&packages, &contract.package)),
            "select fixture live version",
        );
        let pointer_path = if pointer_path.is_absolute() {
            pointer_path.to_path_buf()
        } else {
            root.join(pointer_path)
        };
        let pointer = read_json(&pointer_path).ok();
        let source_record_sha256 = pointer
            .as_ref()
            .and_then(|pointer| pointer.get("source_record_sha256"))
            .and_then(serde_json::Value::as_str)
            .filter(|digest| valid_digest(digest))
            .map(str::to_owned)
            .unwrap_or_else(|| "d".repeat(64));
        let record = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "source_record_sha256": source_record_sha256,
            "tag": if suite == Suite::Stable {
                format!("v{version}")
            } else {
                PREVIEW_TAG.to_owned()
            },
            "crate_version": version,
            "previous": serde_json::Value::Null,
            "inrelease_sha256": must(sha256_file(&stubs.log.join("InRelease")), "hash fixture InRelease"),
            "packages": REQUIRED_ARCHES.iter().map(|arch| serde_json::json!({
                "arch": arch,
                "sha256": must(
                    sha256_file(&stubs.log.join(format!("live-packages-{arch}"))),
                    "hash fixture live Packages",
                ),
            })).collect::<Vec<_>>(),
            "signer_fingerprint": FIXTURE_FPR,
        });
        let mut record = record;
        if suite == Suite::Preview {
            record["suite"] = serde_json::Value::String(PREVIEW_SUITE.to_owned());
        }
        let bytes = must(serde_json::to_vec(&record), "serialize fixture live publication record");
        let signature = b"fixture live publication record signature";
        let record_name = suite.publication_record_file();
        let signature_name = format!("{record_name}.sig");
        let feed = stubs.log.join("feed");
        write_bytes(&feed.join(record_name), &bytes);
        write_bytes(&feed.join(&signature_name), signature);
        write_bytes(
            &feed.join(suite.last_publish_file()),
            format!(
                "{}\n",
                if suite == Suite::Stable {
                    format!("v{version}")
                } else {
                    version.clone()
                }
            )
            .as_bytes(),
        );

        let (record_path, signature_path) = match suite {
            Suite::Stable => (
                Path::new(".apt-live-publication-record.json"),
                Path::new(".apt-live-publication-record.json.sig"),
            ),
            Suite::Preview => (
                Path::new(".apt-live-publication-record-preview.json"),
                Path::new(".apt-live-publication-record-preview.json.sig"),
            ),
        };
        write_bytes(&root.join(record_path), &bytes);
        write_bytes(&root.join(signature_path), signature);
        (record_path, signature_path)
    }

    fn write_expired_live_stable_feed(
        stubs: &ToolStubs,
        contract: &AptContract,
        incoming: &StableIncoming,
        prev_dir: &Path,
    ) -> (String, serde_json::Value, OffsetDateTime) {
        must(std::fs::create_dir_all(prev_dir), "create all-version rollback input");
        let feed = stubs.log.join("feed");
        let inrelease = b"fixture expired signed InRelease bytes";
        let inrelease_sha = sha256_hex(inrelease);
        let source_record_sha = must(
            sidecar_digest(&incoming.dir.join(RECORD_SIDECAR)),
            "read candidate source-record digest",
        );
        let old_date = OffsetDateTime::now_utc() - time::Duration::days(10);
        let old_valid_until = OffsetDateTime::now_utc() - time::Duration::days(2);
        let date_text = must(old_date.format(&Rfc2822), "format expired Release Date");
        let valid_until_text = must(
            old_valid_until.format(&Rfc2822),
            "format expired Release Valid-Until",
        );
        let mut package_indexes = BTreeMap::<String, (String, String)>::new();
        let mut candidate_hashes = BTreeMap::<String, String>::new();
        for arch in REQUIRED_ARCHES {
            let candidate_path = incoming
                .dir
                .join(format!("{}-1.2.3-{arch}.deb", contract.package));
            let candidate_copy = prev_dir.join(format!("{}-1.2.3-{arch}.deb", contract.package));
            must(
                std::fs::copy(&candidate_path, &candidate_copy),
                "copy candidate package into rollback set",
            );
            let older_path = make_deb(
                prev_dir,
                &format!("{}-1.2.2-{arch}.deb", contract.package),
                &contract.package,
                "1.2.2",
                arch,
                &contract.binary,
                &contract.identity_dir,
                FIXTURE_COMMIT,
                "1.2.2",
                b"{}",
                format!("old rollback package bytes {arch}").as_bytes(),
            );

            let mut index_text = String::new();
            for (version, package_path) in [
                ("1.2.3", candidate_copy.as_path()),
                ("1.2.2", older_path.as_path()),
            ] {
                let size = must(std::fs::metadata(package_path), "read feed package size").len();
                let digest = must(sha256_file(package_path), "hash feed package bytes");
                let pool_name = canonical_pool_name(&contract.package, version, arch);
                let pool_path = format!(
                    "pool/main/{}/{}/{}",
                    pool_letter(&contract.package),
                    contract.package,
                    pool_name
                );
                write_bytes(&feed.join(&pool_path), &must(std::fs::read(package_path), "read feed package"));
                index_text.push_str(&format!(
                    "Package: {}\nVersion: {version}\nArchitecture: {arch}\nFilename: {pool_path}\nSize: {size}\nSHA256: {digest}\n\n",
                    contract.package
                ));
                if version == "1.2.3" {
                    candidate_hashes.insert(arch.to_owned(), digest);
                }
            }
            let index_path = feed.join(format!(
                "dists/stable/main/binary-{arch}/Packages"
            ));
            write_bytes(&index_path, index_text.as_bytes());
            let index_bytes = must(std::fs::read(&index_path), "read signed feed index");
            let index_digest = sha256_hex(&index_bytes);
            let gzip_path = feed.join(format!(
                "dists/stable/main/binary-{arch}/Packages.gz"
            ));
            let gzip_bytes = format!("fixture Packages gzip payload {arch}").into_bytes();
            write_bytes(&gzip_path, &gzip_bytes);
            package_indexes.insert(
                arch.to_owned(),
                (index_digest, sha256_hex(&gzip_bytes)),
            );
        }
        let mut release = format!(
            "Origin: {}\nLabel: {}\nSuite: stable\nCodename: stable\nArchitectures: amd64 arm64\nComponents: main\nDate: {date_text}\nValid-Until: {valid_until_text}\nSHA256:\n",
            contract.origin, contract.origin
        );
        for arch in REQUIRED_ARCHES {
            let index_path = feed.join(format!(
                "dists/stable/main/binary-{arch}/Packages"
            ));
            let index_bytes = must(std::fs::read(&index_path), "read feed Packages bytes");
            let gzip_path = feed.join(format!(
                "dists/stable/main/binary-{arch}/Packages.gz"
            ));
            let gzip_bytes = must(std::fs::read(&gzip_path), "read feed gzip bytes");
            let (index_digest, gzip_digest) = must(package_indexes
                .get(arch)
                .map(|(index, gzip)| (index.clone(), gzip.clone()))
                .ok_or("feed index digest missing"), "read feed index digests");
            release.push_str(&format!(
                " {index_digest} {} main/binary-{arch}/Packages\n {gzip_digest} {} main/binary-{arch}/Packages.gz\n",
                index_bytes.len(),
                gzip_bytes.len()
            ));
        }
        write_bytes(&stubs.log.join("signed-release"), release.as_bytes());
        write_bytes(&stubs.log.join("signing-fpr"), FIXTURE_FPR.as_bytes());
        write_bytes(&feed.join("dists/stable/InRelease"), inrelease);
        write_bytes(&feed.join("dists/stable/Release.gpg"), b"fixture Release signature");

        let previous = serde_json::json!({
            "tag": "v1.2.2",
            "source_record_sha256": "a".repeat(64),
        });
        let record = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "source_record_sha256": source_record_sha.clone(),
            "tag": "v1.2.3",
            "crate_version": "1.2.3",
            "previous": previous.clone(),
            "inrelease_sha256": inrelease_sha,
            "packages": REQUIRED_ARCHES.iter().map(|arch| serde_json::json!({
                "arch": arch,
                "sha256": package_indexes.get(*arch).map(|pair| pair.0.as_str()).unwrap_or_default(),
            })).collect::<Vec<_>>(),
            "signer_fingerprint": FIXTURE_FPR,
        });
        write_bytes(
            &feed.join("publication-record.json"),
            &must(serde_json::to_vec(&record), "serialize expired live publication record"),
        );
        write_bytes(
            &feed.join("publication-record.json.sig"),
            b"fixture publication record signature",
        );
        let state = serde_json::json!({
            "schema": PACKAGE_STATE_SCHEMA,
            "source_repository": contract.source_repo.clone(),
            "source_commit": incoming.commit.clone(),
            "source_ref": "refs/tags/v1.2.3",
            "version": "v1.2.3",
            "packages": REQUIRED_ARCHES.iter().map(|arch| serde_json::json!({
                "name": format!("{}-1.2.3-{arch}.deb", contract.package),
                "sha256": candidate_hashes.get(*arch).map(String::as_str).unwrap_or_default(),
            })).collect::<Vec<_>>(),
        });
        write_bytes(
            &feed.join("package-state.json"),
            &must(serde_json::to_vec(&state), "serialize expired live package state"),
        );
        write_bytes(&feed.join("last-publish"), b"v1.2.3\n");
        (source_record_sha, previous, old_date)
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_inputs<'a>(
        suite: Suite,
        contract: AptContract,
        version: &str,
        incoming: &'a Path,
        prev_dir: Option<&'a Path>,
        previous_pointer: &'a Path,
        staging: &'a Path,
        bootstrap: bool,
        path_overlay: Option<&'a Path>,
    ) -> PublishInputs<'a> {
        let (published_record, published_signature) =
            if let (Some(bin), Some(prev_dir)) = (path_overlay, prev_dir)
                && let Some(base) = bin.parent()
            {
                let stubs = ToolStubs {
                    bin: bin.to_path_buf(),
                    log: base.join("log"),
                };
                configure_signed_rollback_feed(&stubs, suite, &contract, version, prev_dir);
                let (record, signature) = write_fixture_live_publication_record(
                    &stubs,
                    prev_dir.parent().unwrap_or(prev_dir),
                    suite,
                    &contract,
                    previous_pointer,
                );
                (Some(record), Some(signature))
            } else {
                (None, None)
            };
        let passphrase_env = contract.passphrase_secret.clone();
        let key_env = contract.signing_key_secret.clone();
        PublishInputs {
            suite,
            contract,
            version: version.to_owned(),
            incoming,
            prev_dir,
            published_record,
            published_signature,
            previous_pointer,
            staging,
            bootstrap,
            passphrase_env,
            passphrase: Some("fixture-passphrase-value".to_owned()),
            key_env,
            key_material: Some("fixture-key-material".to_owned()),
            backend: DebBackend::Auto,
            path_overlay,
        }
    }

    #[test]
    fn expired_signed_feed_seeds_then_refreshes_the_exact_stable_candidate() {
        let incoming = stable_incoming("expired-seed-refresh");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify current stable candidate",
        );
        let stubs = tool_stubs("expired-seed-refresh-tools");
        install_feed_fetch_stubs(&stubs);
        let root = fixture_dir("expired-seed-refresh-root");
        let contract = apt_contract();
        let prev = root.join("prev");
        let (source_record_sha, previous, previous_date) =
            write_expired_live_stable_feed(&stubs, &contract, &incoming, &prev);
        write_bytes(&root.join("example.gpg"), b"fixture publisher keyring");
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages(FIXTURE_PACKAGE, arch, &["1.2.3", "1.2.2"]).as_bytes(),
            );
        }
        install_dynamic_stable_index_stub(&stubs, &contract);
        let pointer = serde_json::json!({
            "already_published": true,
            "tag": "v1.2.3",
            "previous": previous,
            "source_record_sha256": source_record_sha,
        });
        let pointer_path = root.join("previous-pointer.json");
        write_bytes(&pointer_path, format!("{pointer}\n").as_bytes());
        let record_path = root.join("public/publication-record.json");
        let signature_path = root.join("public/publication-record.json.sig");
        let staging = PathBuf::from("public");
        let inputs = PublishInputs {
            suite: Suite::Stable,
            contract: contract.clone(),
            version: incoming.tag.clone(),
            incoming: &incoming.dir,
            prev_dir: Some(&prev),
            previous_pointer: &pointer_path,
            published_record: Some(&record_path),
            published_signature: Some(&signature_path),
            staging: &staging,
            bootstrap: false,
            passphrase_env: contract.passphrase_secret.clone(),
            passphrase: Some("fixture-passphrase-value".to_owned()),
            key_env: contract.signing_key_secret.clone(),
            key_material: Some("fixture-key-material".to_owned()),
            backend: DebBackend::Auto,
            path_overlay: Some(&stubs.bin),
        };

        in_fixture_root(&root, || {
            must(
                seed_live_feed(&contract, &staging, Some(&stubs.bin)),
                "seed the signed but expired live stable feed",
            );
            let seeded_release = must(
                std::fs::read_to_string(root.join("public/dists/stable/Release")),
                "read seeded expired Release",
            );
            let seeded_metadata = must(
                parse_signed_release_with_expired_current(
                    &seeded_release,
                    Suite::Stable,
                    &contract,
                    true,
                ),
                "accept the signed expired Release for exact refresh",
            );
            assert!(seeded_metadata.valid_until <= OffsetDateTime::now_utc());
            let strict_error = must_fail(
                parse_signed_release(&seeded_release, Suite::Stable, &contract),
                "reject expired metadata under normal deployment rules",
            );
            assert!(strict_error.contains("Valid-Until has expired"), "{strict_error}");
            assert_eq!(
                must(publish_suite(&inputs), "refresh expired stable metadata"),
                PublishOutcome::Published
            );
        });

        let refreshed_release = must(
            std::fs::read_to_string(root.join("public/dists/stable/Release")),
            "read refreshed stable Release",
        );
        let refreshed = must(
            parse_signed_release(&refreshed_release, Suite::Stable, &contract),
            "require fresh signed metadata after refresh",
        );
        assert!(refreshed.date > previous_date);
        assert!(refreshed.valid_until > OffsetDateTime::now_utc());
        for arch in REQUIRED_ARCHES {
            let candidate = incoming
                .dir
                .join(format!("{}-1.2.3-{arch}.deb", contract.package));
            let staged = root.join(format!(
                "public/pool/main/{}/{}/{}",
                pool_letter(&contract.package),
                contract.package,
                canonical_pool_name(&contract.package, "1.2.3", arch)
            ));
            assert_eq!(
                must(sha256_file(&candidate), "hash verified candidate deb"),
                must(sha256_file(&staged), "hash refreshed candidate deb")
            );
        }
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
        let base = must(stubs.log.parent().ok_or("stub base"), "stub base");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn stable_publish_stages_signs_and_records() {
        let incoming = stable_incoming("pub-stable");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-stable-tools");
        let root = fixture_dir("pub-stable-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        write_bytes(&root.join("example.gpg"), b"fixture-keyring");
        // Stable replaces only its suite; the shared Pages tree keeps preview.
        write_bytes(
            &root.join("public/dists/preview/Release"),
            b"authenticated preview Release",
        );
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must(publish_suite(&inputs), "publish stable");
        });
        assert_eq!(
            must(
                std::fs::read(root.join("public/dists/preview/Release")),
                "read preserved preview Release"
            ),
            b"authenticated preview Release"
        );
        assert_stable_pool(&root);
        assert_stable_records(&root);
        assert_stable_tool_calls(&stubs);
        let gpg_log = must(
            std::fs::read_to_string(stubs.log.join("gpg.log")),
            "read gpg log",
        );
        assert!(
            !gpg_log.contains("fixture-passphrase-value"),
            "passphrase must never appear in argv: {gpg_log}"
        );
        assert!(
            !gpg_log.contains("fixture-key-material"),
            "key material must never appear in argv: {gpg_log}"
        );
        assert!(
            gpg_log.contains("--homedir"),
            "every signing call must run in the isolated keyring: {gpg_log}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_rollback_bytes_require_the_live_signed_index_before_signing() {
        // Tampering with the fetched deb after the fixture feed was built
        // must fail against the signed package stanza, before key import.
        let incoming = stable_incoming("pub-rollback-bytes");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-rollback-bytes-tools");
        let root = fixture_dir("pub-rollback-bytes-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            make_deb(
                &prev,
                "example-1.2.2-amd64.deb",
                FIXTURE_PACKAGE,
                "1.2.2",
                "amd64",
                FIXTURE_BINARY,
                FIXTURE_IDENTITY,
                FIXTURE_COMMIT,
                "1.2.2",
                b"{}",
                b"tampered-rollback-bytes",
            );
            must_fail(publish_suite(&inputs), "tampered rollback bytes")
        });
        assert!(error.contains("signed Packages stanza"), "{error}");
        assert!(
            !stubs.log.join("gpg.log").exists(),
            "unauthenticated rollback must be rejected before key import"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // A live Packages response changed after the signed Release was
        // produced fails the Release SHA256 link, even if its stanza looks
        // structurally valid.
        let incoming = stable_incoming("pub-rollback-index");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-rollback-index-tools");
        let root = fixture_dir("pub-rollback-index-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            let path = stubs.log.join("live-packages-amd64");
            let mut bytes = must(std::fs::read(&path), "read live Packages fixture");
            bytes.extend_from_slice(b"# changed after signing\n");
            write_bytes(&path, &bytes);
            must_fail(publish_suite(&inputs), "tampered signed Packages")
        });
        assert!(
            error.contains("disagree with the signed Release checksum"),
            "{error}"
        );
        assert!(
            !stubs.log.join("gpg.log").exists(),
            "a broken Release-to-Packages link must fail before key import"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // gpgv success alone is insufficient: its VALIDSIG identity must be
        // the contract's pinned publisher key.
        let incoming = stable_incoming("pub-rollback-signer");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-rollback-signer-tools");
        let root = fixture_dir("pub-rollback-signer-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            write_bytes(
                &stubs.log.join("signing-fpr"),
                b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            );
            must_fail(publish_suite(&inputs), "foreign InRelease signer")
        });
        assert!(error.contains("pinned publisher key"), "{error}");
        assert!(
            !stubs.log.join("gpg.log").exists(),
            "foreign InRelease signer must be rejected before key import"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_rejects_a_pointer_digest_that_does_not_match_the_signed_current_record() {
        let incoming = stable_incoming("pub-pointer-digest");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify candidate",
        );
        let stubs = tool_stubs("pub-pointer-digest-tools");
        let root = fixture_dir("pub-pointer-digest-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            let mut pointer = must(
                read_json(Path::new("previous-pointer.json")),
                "read current pointer fixture",
            );
            pointer["source_record_sha256"] = serde_json::Value::String("e".repeat(64));
            write_bytes(
                Path::new("previous-pointer.json"),
                format!("{pointer}\n").as_bytes(),
            );
            must_fail(
                publish_suite(&inputs),
                "reject a pointer digest that differs from the signed record",
            )
        });
        assert!(error.contains("current stable publication pointer digest"), "{error}");
        assert!(
            !stubs.log.join("gpg.log").exists(),
            "a mismatched current-record pointer must fail before signing-key import"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_refuses_before_any_signing() {
        // No sentinel.
        let incoming = stable_incoming("pub-no-sentinel");
        let stubs = tool_stubs("pub-no-sentinel-tools");
        let root = fixture_dir("pub-no-sentinel-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "no sentinel")
        });
        assert!(error.contains("sentinel"), "{error}");
        assert!(!stubs.log.join("gpg.log").exists(), "no signing attempted");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // Unset passphrase secret: the diagnostic names the secret, never a
        // value.
        let incoming = stable_incoming("pub-no-passphrase");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-no-passphrase-tools");
        let root = fixture_dir("pub-no-passphrase-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        let mut spec = apt_spec();
        spec.passphrase_secret = "B1_TEST_NEVER_SET_PASSPHRASE".to_owned();
        let contract = must(AptContract::resolve_input(spec.input()), "resolve");
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let mut inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            inputs.passphrase = None;
            must_fail(publish_suite(&inputs), "no passphrase")
        });
        assert!(error.contains("B1_TEST_NEVER_SET_PASSPHRASE"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // Bootstrap is preview-only.
        let incoming = stable_incoming("pub-bootstrap-stable");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-bootstrap-stable-tools");
        let root = fixture_dir("pub-bootstrap-stable-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                true,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "bootstrap stable")
        });
        assert!(error.contains("only to --suite preview"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Copy a directory tree the way an artifact round trip moves it: every
    /// entry recurses, and dotfiles survive only when the upload carries
    /// hidden files.
    fn copy_tree_filtered(source: &Path, target: &Path, keep_hidden: bool) {
        must(std::fs::create_dir_all(target), "create the handoff target");
        let entries = must(std::fs::read_dir(source), "list the handoff source");
        for entry in entries {
            let entry = must(entry, "read a handoff entry");
            let name = entry.file_name();
            let hidden = name.to_str().is_some_and(|name| name.starts_with('.'));
            if hidden && !keep_hidden {
                continue;
            }
            let from = entry.path();
            let to = target.join(&name);
            if must(entry.file_type(), "type a handoff entry").is_dir() {
                copy_tree_filtered(&from, &to, keep_hidden);
            } else {
                must(std::fs::copy(&from, &to), "copy a handoff file");
            }
        }
    }

    #[test]
    fn publish_accepts_a_verify_produced_tree_across_the_artifact_handoff() {
        // The verify→publish handoff crosses an upload-artifact round trip,
        // which drops dotfiles unless the upload opts into hidden files.
        // A handoff that drops the hidden sentinel must still refuse
        // before any signing; one that carries it must publish.
        let incoming = stable_incoming("handoff-verify");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        assert!(incoming.dir.join(SENTINEL_FILE).is_file());

        let dropped = fixture_dir("handoff-dropped");
        copy_tree_filtered(&incoming.dir, &dropped, false);
        assert!(!dropped.join(SENTINEL_FILE).exists());
        let stubs = tool_stubs("handoff-dropped-tools");
        let root = fixture_dir("handoff-dropped-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &dropped,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "dropped sentinel")
        });
        assert!(error.contains("sentinel"), "{error}");
        assert!(!stubs.log.join("gpg.log").exists(), "no signing attempted");
        let _ = std::fs::remove_dir_all(&dropped);
        let _ = std::fs::remove_dir_all(&root);

        let carried = fixture_dir("handoff-carried");
        copy_tree_filtered(&incoming.dir, &carried, true);
        assert!(carried.join(SENTINEL_FILE).is_file());
        let stubs = tool_stubs("handoff-carried-tools");
        let root = fixture_dir("handoff-carried-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        write_bytes(&root.join("example.gpg"), b"fixture-keyring");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &carried,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must(publish_suite(&inputs), "publish the carried tree");
        });
        assert_stable_pool(&root);
        assert_stable_records(&root);
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&carried);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Publish with the key material overridden: the key-secret negatives
    /// fail pre-mutation, naming the secret — never the material.
    fn publish_key_material_error(name: &str, material: Option<String>) -> String {
        let incoming = stable_incoming(name);
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs(&format!("{name}-tools"));
        let root = fixture_dir(&format!("{name}-root"));
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let mut inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            inputs.key_material = material;
            must_fail(publish_suite(&inputs), name)
        });
        assert!(
            !stubs.log.join("gpg.log").exists(),
            "no key import attempted: {name}"
        );
        assert!(
            !root.join("public").exists(),
            "rejected pre-mutation: {name}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
        error
    }

    #[test]
    fn stable_publish_refuses_without_key_material() {
        let error = publish_key_material_error("pub-no-key-unset", None);
        assert!(error.contains("B1_TEST_SIGNING_KEY is unset"), "{error}");
    }

    #[test]
    fn stable_publish_refuses_empty_key_material() {
        let error = publish_key_material_error("pub-no-key-empty", Some(String::new()));
        assert!(error.contains("B1_TEST_SIGNING_KEY is empty"), "{error}");
    }

    #[test]
    fn stable_publish_rejects_pool_and_index_defects() {
        // Stable publication cannot proceed from candidate-only inputs.
        let incoming = stable_incoming("pub-pool-count");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-pool-count-tools");
        let root = fixture_dir("pub-pool-count-root");
        write_bytes(
            &root.join("previous-pointer.json"),
            format!(
                "{{\"tag\": \"v1.2.2\", \"source_record_sha256\": \"{}\"}}\n",
                "dd".repeat(32)
            )
            .as_bytes(),
        );
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "pool count")
        });
        assert!(error.contains("--prev-dir is required"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // Index retaining the wrong version set.
        for (name, amd64, arm64, want) in [
            (
                "three-versions",
                &["1.2.3", "1.2.2", "1.2.1"] as &[&str],
                &["1.2.3", "1.2.2", "1.2.1"] as &[&str],
                "must retain exactly candidate plus rollback",
            ),
            (
                "missing-candidate",
                &["1.2.2", "1.2.1"] as &[&str],
                &["1.2.2", "1.2.1"] as &[&str],
                "lacks candidate version",
            ),
            (
                "divergent-rollback",
                &["1.2.3", "1.2.2"] as &[&str],
                &["1.2.3", "1.2.1"] as &[&str],
                "architecture rollback versions differ",
            ),
        ] {
            let incoming = stable_incoming("pub-index");
            must(
                verify_suite(&stable_verify_inputs(&incoming)),
                "verify first",
            );
            let stubs = tool_stubs("pub-index-tools");
            let root = fixture_dir("pub-index-root");
            let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
            stable_pointer_file(&root, "v1.2.2");
            write_bytes(
                &stubs.log.join("packages-amd64"),
                canned_packages("example", "amd64", amd64).as_bytes(),
            );
            write_bytes(
                &stubs.log.join("packages-arm64"),
                canned_packages("example", "arm64", arm64).as_bytes(),
            );
            write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
            let contract = apt_contract();
            let staging = PathBuf::from("public");
            let error = in_fixture_root(&root, || {
                let inputs = publish_inputs(
                    Suite::Stable,
                    contract,
                    "v1.2.3",
                    &incoming.dir,
                    Some(&prev),
                    Path::new("previous-pointer.json"),
                    &staging,
                    false,
                    Some(&stubs.bin),
                );
                must_fail(publish_suite(&inputs), name)
            });
            assert!(error.contains(want), "{name}: {error}");
            let _ = std::fs::remove_dir_all(&incoming.dir);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn stable_publish_rejects_pointer_defects() {
        // Previous pointer disagreeing with the retained rollback.
        let incoming = stable_incoming("pub-pointer");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-pointer-tools");
        let root = fixture_dir("pub-pointer-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v9.9.9");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "pointer disagreement")
        });
        assert!(
            error.contains("disagrees with retained rollback"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // Malformed pointer keys.
        let incoming = stable_incoming("pub-pointer-keys");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-pointer-keys-tools");
        let root = fixture_dir("pub-pointer-keys-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        write_bytes(
            &root.join("previous-pointer.json"),
            b"{\"tag\": \"v1.2.2\", \"extra\": 1}\n",
        );
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "pointer keys")
        });
        assert!(error.contains("previous pointer is malformed"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_disagreement_rejected_before_any_signing() {
        // The strict index build must run to learn the retained rollback,
        // but no signature may precede the rejection.
        let incoming = stable_incoming("pub-order-stable");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-order-stable-tools");
        let root = fixture_dir("pub-order-stable-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v9.9.9");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "pointer disagreement")
        });
        assert!(
            error.contains("disagrees with retained rollback"),
            "{error}"
        );
        assert_no_signing_attempted(&stubs);
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_refuses_a_disagreeing_signing_key() {
        // The import succeeds but the private key is not the pinned
        // publisher identity: publication fails pre-mutation, before any
        // signature, naming the disagreement — never the material.
        let incoming = stable_incoming("pub-key-disagree");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-key-disagree-tools");
        let script = must(
            std::fs::read_to_string(stubs.bin.join("gpg")),
            "read the gpg stub",
        );
        assert!(
            script.contains(FIXTURE_FPR),
            "the stub lists the pinned key"
        );
        must(
            std::fs::write(
                stubs.bin.join("gpg"),
                script.replace(FIXTURE_FPR, "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            ),
            "re-point the stub at a foreign key",
        );
        let root = fixture_dir("pub-key-disagree-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "disagreeing key")
        });
        assert!(
            error.contains("disagrees with the pinned publisher key"),
            "{error}"
        );
        let log = must(
            std::fs::read_to_string(stubs.log.join("gpg.log")),
            "read the gpg log",
        );
        assert!(
            log.contains("--import"),
            "the flow must reach the key import: {log}"
        );
        assert_no_signing_attempted(&stubs);
        assert!(
            !root.join("public").exists(),
            "disagreement rejected pre-mutation"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disagreeing_key_refusal_leaves_no_agent_or_key_residue() {
        // The refusal path must tear down the isolated keyring exactly like
        // the success path: the agent holding the imported key is killed,
        // then the directory is wiped. A bare directory wipe would orphan a
        // live gpg-agent/scdaemon pair holding key material.
        let incoming = stable_incoming("pub-key-residue");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-key-residue-tools");
        let script = must(
            std::fs::read_to_string(stubs.bin.join("gpg")),
            "read the gpg stub",
        );
        assert!(
            script.contains(FIXTURE_FPR),
            "the stub lists the pinned key"
        );
        must(
            std::fs::write(
                stubs.bin.join("gpg"),
                script.replace(FIXTURE_FPR, "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            ),
            "re-point the stub at a foreign key",
        );
        let root = fixture_dir("pub-key-residue-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "disagreeing key")
        });
        assert!(
            error.contains("disagrees with the pinned publisher key"),
            "{error}"
        );
        assert!(
            !error.contains("fixture-key-material"),
            "the refusal names the secret, never the material: {error}"
        );
        let gpg_log = must(
            std::fs::read_to_string(stubs.log.join("gpg.log")),
            "read the gpg log",
        );
        let homedir = must(
            gpg_log
                .lines()
                .find(|line| line.contains("--import"))
                .and_then(|line| {
                    let mut args = line.split_whitespace();
                    args.position(|arg| arg == "--homedir")
                        .and_then(|_| args.next())
                })
                .ok_or_else(|| format!("the flow must reach the key import: {gpg_log}")),
            "locate the imported keyring",
        );
        let kill_log = must(
            std::fs::read_to_string(stubs.log.join("gpgconf.log")),
            "the refusal must kill the agent it spawned",
        );
        assert!(
            kill_log.contains(&format!("--homedir {homedir} --kill gpg-agent")),
            "the refusal must kill this run's agent: {kill_log}"
        );
        assert!(
            !Path::new(homedir).exists(),
            "the refusal must wipe the keyring holding the material"
        );
        assert_no_signing_attempted(&stubs);
        assert!(
            !root.join("public").exists(),
            "disagreement rejected pre-mutation"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_malformed_pointer_rejected_pre_mutation() {
        // Rejected before any mutation or signing: the staging tree the
        // publisher would wipe is untouched.
        let incoming = stable_incoming("pub-order-keys");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-order-keys-tools");
        let root = fixture_dir("pub-order-keys-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        write_bytes(
            &root.join("previous-pointer.json"),
            b"{\"tag\": \"v1.2.2\", \"extra\": 1}\n",
        );
        write_bytes(&root.join("public/junk"), b"junk");
        can_both_arches(&stubs, &["1.2.3", "1.2.2"]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "pointer keys")
        });
        assert!(error.contains("previous pointer is malformed"), "{error}");
        assert_no_signing_attempted(&stubs);
        assert!(
            root.join("public/junk").is_file(),
            "malformed pointer rejected pre-mutation"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_publish_wrong_pointer_rejected_pre_mutation() {
        // Rejected before any mutation or signing: the staging tree is never
        // even created.
        let incoming = preview_incoming("pub-order-preview");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-order-preview-tools");
        let root = fixture_dir("pub-order-preview-root");
        let prev = preview_prev_dir(&root);
        write_bytes(&root.join("previous-pointer.json"), b"\"stable\"\n");
        can_both_arches(&stubs, &[PREVIEW_CANDIDATE, PREVIEW_ROLLBACK]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "preview pointer")
        });
        assert!(
            error.contains("preview previous pointer must be"),
            "{error}"
        );
        assert_no_signing_attempted(&stubs);
        assert!(
            !root.join("public").exists(),
            "preview pointer rejected pre-mutation"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_rejects_extra_candidate_debs_in_rollback_input() {
        let incoming = stable_incoming("pub-collision");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-collision-tools");
        let root = fixture_dir("pub-collision-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        must(
            std::fs::copy(
                incoming.dir.join("example-1.2.3-arm64.deb"),
                prev.join("example-1.2.3-arm64.deb"),
            ),
            "add candidate deb to rollback input",
        );
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Stable,
                contract,
                "v1.2.3",
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "collision")
        });
        assert!(error.contains("more than one arm64 package"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_publish_rejects_pool_collisions() {
        // A rollback and candidate with the same canonical package identity
        // must never replace one another when their package bytes differ.
        let root = fixture_dir("stable-pool-byte-collision");
        let rollback = make_deb(
            &root,
            "rollback.deb",
            FIXTURE_PACKAGE,
            "1.2.3",
            "amd64",
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            FIXTURE_COMMIT,
            "1.2.3",
            b"{}",
            b"rollback package bytes",
        );
        let candidate = make_deb(
            &root,
            "candidate.deb",
            FIXTURE_PACKAGE,
            "1.2.3",
            "amd64",
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
            FIXTURE_COMMIT,
            "1.2.3",
            b"{}",
            b"candidate package bytes",
        );
        let contract = apt_contract();
        let destination = root
            .join("pool")
            .join(canonical_pool_name(FIXTURE_PACKAGE, "1.2.3", "amd64"));
        must(
            stage_package(
                &rollback,
                &destination,
                &contract,
                DebBackend::ArTar,
                None,
            ),
            "stage rollback bytes",
        );
        let error = must_fail(
            stage_package(
                &candidate,
                &destination,
                &contract,
                DebBackend::ArTar,
                None,
            ),
            "reject candidate bytes that collide with the rollback pool identity",
        );
        assert!(
            error.contains("canonical package identity collides with different bytes"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    const PREVIEW_CANDIDATE: &str = "1.2.3~preview.41+0123456";
    const PREVIEW_ROLLBACK: &str = "1.2.3~preview.40+abcdef0";

    fn preview_prev_dir(root: &Path) -> PathBuf {
        preview_prev_dir_at(root, PREVIEW_ROLLBACK)
    }

    fn preview_prev_dir_at(root: &Path, version: &str) -> PathBuf {
        preview_prev_dir_versions(root, &[version])
    }

    fn preview_prev_dir_versions(root: &Path, versions: &[&str]) -> PathBuf {
        let prev = root.join("prev");
        must(std::fs::create_dir_all(&prev), "prev dir");
        for version in versions {
            for arch in REQUIRED_ARCHES {
                make_deb(
                    &prev,
                    &format!("example_{version}_{arch}.deb"),
                    FIXTURE_PACKAGE,
                    version,
                    arch,
                    FIXTURE_BINARY,
                    FIXTURE_IDENTITY,
                    "abcdef0123456789abcdef0123456789abcdef01",
                    "1.2.3",
                    b"{}",
                    format!("rollback-daemon-bytes-{version}-{arch}").as_bytes(),
                );
            }
        }
        prev
    }

    #[test]
    fn preview_publish_prunes_an_authenticated_third_indexed_version() {
        let incoming = preview_incoming("pub-preview-prune-third");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify candidate",
        );
        let stubs = tool_stubs("pub-preview-prune-third-tools");
        let root = fixture_dir("pub-preview-prune-third-root");
        let older = [
            PREVIEW_ROLLBACK,
            "1.2.3~preview.39+abcdef0",
            "1.2.3~preview.38+abcdef0",
        ];
        let prev = preview_prev_dir_versions(&root, &older);
        write_bytes(&root.join("previous-pointer.json"), b"\"preview\"\n");
        can_both_arches(&stubs, &[PREVIEW_CANDIDATE, PREVIEW_ROLLBACK]);
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let inputs = publish_inputs(
            Suite::Preview,
            contract.clone(),
            PREVIEW_CANDIDATE,
            &incoming.dir,
            Some(&prev),
            Path::new("previous-pointer.json"),
            &staging,
            false,
            Some(&stubs.bin),
        );
        let indexed_pairs = must(
            inspect_rollback_pairs(&inputs, &prev),
            "inspect all authenticated fixture pairs",
        );
        let highest_pair = must(
            indexed_pairs
                .get(PREVIEW_ROLLBACK)
                .cloned()
                .ok_or("highest fixture rollback pair is missing"),
            "select highest fixture rollback pair",
        );
        let rollback = AuthenticatedRollback {
            version: PREVIEW_ROLLBACK.to_owned(),
            debs: highest_pair,
            indexed_pairs,
            release_date: OffsetDateTime::now_utc() - time::Duration::minutes(10),
            current_record: PublicationRecord {
                schema: PUBLICATION_RECORD_SCHEMA.to_owned(),
                source_record_sha256: "a".repeat(64),
                tag: PREVIEW_TAG.to_owned(),
                crate_version: PREVIEW_ROLLBACK.to_owned(),
                suite: Some(PREVIEW_SUITE.to_owned()),
                inrelease_sha256: "b".repeat(64),
                packages: REQUIRED_ARCHES
                    .iter()
                    .map(|arch| IndexEntry {
                        arch: (*arch).to_owned(),
                        sha256: "c".repeat(64),
                    })
                    .collect(),
                signer_fingerprint: FIXTURE_FPR.to_owned(),
                previous: serde_json::Value::String(PREVIEW_TAG.to_owned()),
            },
            refresh_required: false,
        };

        in_fixture_root(&root, || {
            let pool = pool_root(&staging, Suite::Preview, &contract);
            must(std::fs::create_dir_all(&pool), "create seeded preview pool");
            for version in older {
                for arch in REQUIRED_ARCHES {
                    let name = canonical_pool_name(&contract.package, version, arch);
                    must(
                        std::fs::copy(prev.join(&name), pool.join(&name)),
                        "seed existing preview pool pair",
                    );
                }
            }
            must(
                publish_preview(
                    &inputs,
                    &staging,
                    Some(&rollback),
                    false,
                    "fixture-passphrase",
                    "unused-homedir",
                ),
                "publish third preview version",
            );

            assert_eq!(must(pool_deb_count(&pool), "count pruned pool"), 4);
            for version in [PREVIEW_CANDIDATE, PREVIEW_ROLLBACK] {
                for arch in REQUIRED_ARCHES {
                    assert!(pool.join(canonical_pool_name(&contract.package, version, arch)).is_file());
                }
            }
            for version in &older[1..] {
                for arch in REQUIRED_ARCHES {
                    assert!(!pool.join(canonical_pool_name(&contract.package, version, arch)).exists());
                }
            }
        });
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
        let base = must(stubs.log.parent().ok_or("stub base"), "stub base");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn preview_publish_strict_keeps_the_shared_tree_and_enforces_order() {
        let incoming = preview_incoming("pub-preview");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-preview-tools");
        let root = fixture_dir("pub-preview-root");
        let prev = preview_prev_dir(&root);
        write_bytes(&root.join("previous-pointer.json"), b"\"preview\"\n");
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages("example", arch, &[PREVIEW_CANDIDATE, PREVIEW_ROLLBACK]).as_bytes(),
            );
        }
        write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
        // Preview never wipes: prove it with a stable-tree marker.
        write_bytes(&root.join("public/dists/stable/Release"), b"stable-marker");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must(publish_suite(&inputs), "publish preview");
        });
        assert_eq!(
            must(
                std::fs::read(root.join("public/dists/stable/Release")),
                "read preserved stable Release"
            ),
            b"stable-marker",
            "preview must preserve the stable suite in the shared tree"
        );
        for arch in REQUIRED_ARCHES {
            for version in [PREVIEW_CANDIDATE, PREVIEW_ROLLBACK] {
                assert!(
                    root.join(format!(
                        "public/pool/preview/main/e/example/example_{version}_{arch}.deb"
                    ))
                    .is_file(),
                    "preview pool holds {version}/{arch}"
                );
            }
            assert!(root
                .join(format!("public/dists/preview/main/binary-{arch}/Packages"))
                .is_file());
        }
        assert!(root.join("public/dists/preview/InRelease").is_file());
        let record: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join("public/publication-record-preview.json")),
                "read record",
            )),
            "parse record",
        );
        let parsed = must(
            parse_publication_record(&record),
            "parse the preview record",
        );
        assert_eq!(parsed.tag, "preview");
        assert_eq!(parsed.crate_version, PREVIEW_CANDIDATE);
        assert_eq!(parsed.suite.as_deref(), Some("preview"));
        assert_eq!(
            parsed.previous,
            serde_json::Value::String("preview".to_owned())
        );
        assert_eq!(
            must(
                std::fs::read_to_string(root.join("public/last-publish-preview")),
                "read last-publish"
            ),
            format!("{PREVIEW_CANDIDATE}\n")
        );
        let distributions = must(
            std::fs::read_to_string(root.join("public/conf/distributions")),
            "read distributions",
        );
        assert!(
            distributions.contains("Codename: preview"),
            "{distributions}"
        );
        // A re-run never duplicates the stanza.
        let stanza_count = distributions
            .lines()
            .filter(|line| *line == "Codename: preview")
            .count();
        assert_eq!(stanza_count, 1);
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_publish_rejects_a_backward_candidate() {
        let incoming = preview_incoming("pub-preview-backward");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-preview-backward-tools");
        let root = fixture_dir("pub-preview-backward-root");
        let prev = preview_prev_dir_at(&root, "1.2.3~preview.42+ffffff0");
        write_bytes(&root.join("previous-pointer.json"), b"\"preview\"\n");
        // The retained rollback is NEWER than the candidate.
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages(
                    "example",
                    arch,
                    &[PREVIEW_CANDIDATE, "1.2.3~preview.42+ffffff0"],
                )
                .as_bytes(),
            );
        }
        write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "backward candidate")
        });
        assert!(
            error.contains("not newer than the retained rollback"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_publish_bootstrap_initializes_once() {
        let incoming = preview_incoming("pub-bootstrap");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-bootstrap-tools");
        let root = fixture_dir("pub-bootstrap-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages("example", arch, &[PREVIEW_CANDIDATE]).as_bytes(),
            );
        }
        write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                true,
                Some(&stubs.bin),
            );
            must(publish_suite(&inputs), "bootstrap preview");
        });
        for arch in REQUIRED_ARCHES {
            assert!(
                root.join(format!(
                    "public/pool/preview/main/e/example/example_{PREVIEW_CANDIDATE}_{arch}.deb"
                ))
                .is_file(),
                "bootstrap stages {arch}"
            );
        }
        let record: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join("public/publication-record-preview.json")),
                "read record",
            )),
            "parse record",
        );
        let parsed = must(
            parse_publication_record(&record),
            "parse the bootstrap record",
        );
        assert!(parsed.previous.is_null());
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_bootstrap_refuses_an_existing_pool() {
        // Bootstrap refuses to run over an existing preview pool.
        let incoming = preview_incoming("pub-bootstrap-existing");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-bootstrap-existing-tools");
        let root = fixture_dir("pub-bootstrap-existing-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        for arch in REQUIRED_ARCHES {
            write_bytes(
                &stubs.log.join(format!("packages-{arch}")),
                canned_packages("example", arch, &[PREVIEW_CANDIDATE]).as_bytes(),
            );
        }
        write_bytes(&stubs.log.join("release"), b"Origin: Example\n");
        let pool = root.join("public/pool/preview/main/e/example");
        must(std::fs::create_dir_all(&pool), "existing pool");
        must(
            std::fs::copy(
                incoming
                    .dir
                    .join("example-preview-1.2.3.preview.41+0123456-amd64.deb"),
                pool.join(format!("example_{PREVIEW_ROLLBACK}_amd64.deb")),
            ),
            "plant an existing pool deb",
        );
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                true,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "bootstrap over existing")
        });
        assert!(
            error.contains("refuses to run over an existing preview pool"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_publish_modes_are_mutually_exclusive() {
        // Bootstrap is mutually exclusive with --prev-dir, and the strict
        // path requires it.
        let incoming = preview_incoming("pub-preview-modes");
        must(
            verify_suite(&preview_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-preview-modes-tools");
        let root = fixture_dir("pub-preview-modes-root");
        write_bytes(&root.join("previous-pointer.json"), b"null\n");
        let contract = apt_contract();
        let staging = PathBuf::from("public");
        let prev = preview_prev_dir(&root);
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                true,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "bootstrap with prev-dir")
        });
        assert!(error.contains("mutually exclusive"), "{error}");
        let contract = apt_contract();
        let error = in_fixture_root(&root, || {
            let inputs = publish_inputs(
                Suite::Preview,
                contract,
                PREVIEW_CANDIDATE,
                &incoming.dir,
                None,
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "strict without prev-dir")
        });
        assert!(error.contains("--prev-dir is required"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn previous_pointer_derivation_implements_the_jq_rules() {
        let prior_sha = "dd".repeat(32);
        let candidate_sha = "ee".repeat(32);
        // The published record already identifies the prior tag.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.2",
            "source_record_sha256": prior_sha,
        });
        let pointer = must(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "prior case",
        );
        assert_eq!(
            pointer,
            serde_json::json!({"tag": "v1.2.2", "source_record_sha256": prior_sha})
        );
        // The published record identifies the candidate: the pointer is its
        // recorded rollback once the bytes agree.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.3",
            "source_record_sha256": candidate_sha,
            "previous": {"tag": "v1.2.2", "source_record_sha256": prior_sha},
        });
        let pointer = must(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "candidate case",
        );
        assert_eq!(
            pointer,
            serde_json::json!({"tag": "v1.2.2", "source_record_sha256": prior_sha})
        );
        // Neither tag.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v9.9.9",
            "source_record_sha256": prior_sha,
        });
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "neither tag",
        );
        assert!(error.contains("neither candidate nor rollback"), "{error}");
        // Candidate bytes disagree with the immutable source release.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.3",
            "source_record_sha256": "ff".repeat(32),
            "previous": {"tag": "v1.2.2", "source_record_sha256": prior_sha},
        });
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "candidate bytes",
        );
        assert!(error.contains("differs from immutable source"), "{error}");
        // Rollback tag disagrees with the signed pair.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.3",
            "source_record_sha256": candidate_sha,
            "previous": {"tag": "v1.2.0", "source_record_sha256": prior_sha},
        });
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "rollback tag",
        );
        assert!(
            error.contains("differs from signed package pair"),
            "{error}"
        );
        // Invalid rollback checksum.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.3",
            "source_record_sha256": candidate_sha,
            "previous": {"tag": "v1.2.2", "source_record_sha256": "short"},
        });
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "rollback sha",
        );
        assert!(error.contains("rollback checksum is invalid"), "{error}");
        // Bad schema and bad candidate digest fail closed first.
        let published = serde_json::json!({"schema": "other/v9", "tag": "v1.2.2"});
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "bad schema",
        );
        assert!(
            error.contains("unsupported publication record schema"),
            "{error}"
        );
        let published = serde_json::json!({"schema": PUBLICATION_RECORD_SCHEMA, "tag": "v1.2.2"});
        let error = must_fail(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", "short"),
            "bad candidate sha",
        );
        assert!(error.contains("not 64 lowercase hex"), "{error}");
    }

    #[test]
    fn publication_records_parse_both_suite_shapes() {
        let stable = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "source_record_sha256": "ee".repeat(32),
            "tag": "v1.2.3",
            "crate_version": "1.2.3",
            "inrelease_sha256": "aa".repeat(32),
            "packages": [
                {"arch": "amd64", "sha256": "bb".repeat(32)},
                {"arch": "arm64", "sha256": "cc".repeat(32)},
            ],
            "signer_fingerprint": FIXTURE_FPR,
            "previous": {"tag": "v1.2.2", "source_record_sha256": "dd".repeat(32)},
        });
        let parsed = must(parse_publication_record(&stable), "stable record");
        assert_eq!(parsed.suite, None);
        assert_eq!(parsed.packages.len(), 2);
        let preview = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "source_record_sha256": "ee".repeat(32),
            "tag": "preview",
            "crate_version": PREVIEW_CANDIDATE,
            "suite": "preview",
            "inrelease_sha256": "aa".repeat(32),
            "packages": [
                {"arch": "amd64", "sha256": "bb".repeat(32)},
                {"arch": "arm64", "sha256": "cc".repeat(32)},
            ],
            "signer_fingerprint": FIXTURE_FPR,
            "previous": "preview",
        });
        let parsed = must(parse_publication_record(&preview), "preview record");
        assert_eq!(parsed.suite.as_deref(), Some("preview"));
        for (name, mutate) in [
            ("schema", serde_json::json!({"schema": "other/v9"})),
            (
                "digest",
                serde_json::json!({"source_record_sha256": "short"}),
            ),
            (
                "arches",
                serde_json::json!({"packages": [{"arch": "amd64", "sha256": "bb".repeat(32)}]}),
            ),
            ("signer", serde_json::json!({"signer_fingerprint": "short"})),
        ] {
            let mut document = stable.clone();
            for (key, value) in mutate
                .as_object()
                .map(|map| map.iter())
                .into_iter()
                .flatten()
            {
                document[key] = value.clone();
            }
            let error = must_fail(parse_publication_record(&document), name);
            assert!(!error.is_empty(), "{name}");
        }
        let mut missing = stable.clone();
        missing.as_object_mut().map(|map| map.remove("previous"));
        let error = must_fail(parse_publication_record(&missing), "missing previous");
        assert!(error.contains("no previous pointer"), "{error}");
    }

    #[test]
    fn publication_record_signature_is_checked_before_json_parsing() {
        let root = fixture_dir("publication-signature-order");
        let bin = root.join("bin");
        must(std::fs::create_dir_all(&bin), "create signature stub directory");
        let document = root.join("publication-record.json");
        let signature = root.join("publication-record.json.sig");
        let calls = root.join("gpgv.calls");
        write_bytes(&document, b"not-json");
        write_bytes(&signature, b"invalid signature");
        write_bytes(
            &bin.join("gpgv"),
            format!(
                "#!/bin/sh\nprintf called >> \"{}\"\nexit 1\n",
                calls.display()
            )
            .as_bytes(),
        );
        make_executable(&bin.join("gpgv"));

        let error = must_fail(
            read_authenticated_publication_record(
                &document,
                &signature,
                "keys/publisher.gpg",
                FIXTURE_FPR,
                Some(&bin),
            ),
            "reject an unauthenticated publication record",
        );
        assert!(error.contains("gpgv failed"), "{error}");
        assert!(!error.contains("not JSON"), "parse must follow signature verification: {error}");
        assert_eq!(
            must(std::fs::read_to_string(&calls), "read gpgv call marker"),
            "called"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn verified_head(
        suite: Suite,
        version: &str,
        source_digest: &str,
        inrelease_digest: &str,
        previous: serde_json::Value,
        release_date: i64,
        package_digest: &str,
    ) -> VerifiedDeploySuite {
        let (tag, crate_version, suite_name) = match suite {
            Suite::Stable => (version.to_owned(), version.trim_start_matches('v').to_owned(), None),
            Suite::Preview => (PREVIEW_TAG.to_owned(), version.to_owned(), Some(PREVIEW_SUITE.to_owned())),
        };
        VerifiedDeploySuite {
            record: PublicationRecord {
                schema: PUBLICATION_RECORD_SCHEMA.to_owned(),
                source_record_sha256: source_digest.to_owned(),
                tag,
                crate_version,
                suite: suite_name,
                inrelease_sha256: inrelease_digest.to_owned(),
                packages: REQUIRED_ARCHES
                    .iter()
                    .map(|arch| IndexEntry {
                        arch: (*arch).to_owned(),
                        sha256: package_digest.to_owned(),
                    })
                    .collect(),
                signer_fingerprint: FIXTURE_FPR.to_owned(),
                previous,
            },
            release: SignedReleaseMetadata {
                checksums: BTreeMap::new(),
                date: time::OffsetDateTime::from_unix_timestamp(release_date)
                    .expect("fixture timestamp is valid"),
                valid_until: time::OffsetDateTime::from_unix_timestamp(release_date + 86_400)
                    .expect("fixture timestamp is valid"),
            },
            package_indexes: REQUIRED_ARCHES
                .iter()
                .map(|arch| ((*arch).to_owned(), package_digest.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn deploy_guard_comparison_rejects_rollback_and_unbound_pointer() {
        let live = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"b".repeat(64),
            serde_json::Value::Null,
            100,
            &"c".repeat(64),
        );
        let older = verified_head(
            Suite::Stable,
            "v1.2.2",
            &"d".repeat(64),
            &"e".repeat(64),
            serde_json::Value::Null,
            200,
            &"f".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &older, &live),
            "stable rollback",
        );
        assert!(error.contains("older than the authenticated live publication"), "{error}");

        let forward = verified_head(
            Suite::Stable,
            "v1.2.4",
            &"d".repeat(64),
            &"e".repeat(64),
            serde_json::json!({
                "tag": "v1.2.3",
                "source_record_sha256": "a".repeat(64),
            }),
            200,
            &"f".repeat(64),
        );
        must(
            compare_deploy_heads(Suite::Stable, &forward, &live),
            "stable forward publication with authenticated prior pointer",
        );
        let unbound = verified_head(
            Suite::Stable,
            "v1.2.4",
            &"d".repeat(64),
            &"e".repeat(64),
            serde_json::json!({"tag": "v1.2.3", "source_record_sha256": "0".repeat(64)}),
            200,
            &"f".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &unbound, &live),
            "stable unbound previous pointer",
        );
        assert!(error.contains("does not authenticate the live publication"), "{error}");

        let preview_live = verified_head(
            Suite::Preview,
            PREVIEW_CANDIDATE,
            &"a".repeat(64),
            &"b".repeat(64),
            serde_json::Value::Null,
            100,
            &"c".repeat(64),
        );
        let preview_older = verified_head(
            Suite::Preview,
            PREVIEW_ROLLBACK,
            &"d".repeat(64),
            &"e".repeat(64),
            serde_json::Value::Null,
            200,
            &"f".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Preview, &preview_older, &preview_live),
            "preview rollback",
        );
        assert!(error.contains("older than the authenticated live publication"), "{error}");
    }

    #[test]
    fn deploy_guard_same_version_refresh_requires_exact_source_and_monotonic_date() {
        let previous = serde_json::json!({
            "tag": "v1.2.2",
            "source_record_sha256": "9".repeat(64),
        });
        let live = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"b".repeat(64),
            previous.clone(),
            100,
            &"c".repeat(64),
        );
        let refresh = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"d".repeat(64),
            previous.clone(),
            101,
            &"c".repeat(64),
        );
        must(
            compare_deploy_heads(Suite::Stable, &refresh, &live),
            "same-version exact-byte refresh with advancing Date",
        );
        let changed_source = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"e".repeat(64),
            &"d".repeat(64),
            previous.clone(),
            101,
            &"c".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &changed_source, &live),
            "same-version changed source",
        );
        assert!(error.contains("same-version refresh differs"), "{error}");
        let changed_bytes = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"d".repeat(64),
            previous,
            101,
            &"0".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &changed_bytes, &live),
            "same-version changed package bytes",
        );
        assert!(error.contains("same-version refresh differs"), "{error}");
        let stale_date = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"d".repeat(64),
            live.record.previous.clone(),
            100,
            &"c".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &stale_date, &live),
            "non-advancing refresh Date",
        );
        assert!(error.contains("Date must advance monotonically"), "{error}");
    }

    #[test]
    fn deploy_guard_expired_live_metadata_only_allows_same_version_refresh() {
        let previous = serde_json::json!({
            "tag": "v1.2.2",
            "source_record_sha256": "9".repeat(64),
        });
        let mut live = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"b".repeat(64),
            previous.clone(),
            100,
            &"c".repeat(64),
        );
        live.release.valid_until = time::OffsetDateTime::UNIX_EPOCH;

        let refresh = verified_head(
            Suite::Stable,
            "v1.2.3",
            &"a".repeat(64),
            &"d".repeat(64),
            previous.clone(),
            101,
            &"c".repeat(64),
        );
        must(
            compare_deploy_heads(Suite::Stable, &refresh, &live),
            "expired live metadata may receive an exact same-version refresh",
        );

        let forward = verified_head(
            Suite::Stable,
            "v1.2.4",
            &"d".repeat(64),
            &"e".repeat(64),
            serde_json::json!({
                "tag": "v1.2.3",
                "source_record_sha256": "a".repeat(64),
            }),
            200,
            &"f".repeat(64),
        );
        let error = must_fail(
            compare_deploy_heads(Suite::Stable, &forward, &live),
            "expired live metadata with a newer candidate",
        );
        assert!(error.contains("expired live metadata"), "{error}");
    }

    fn staged_pool_with_candidate(staging: &Path, suite: Suite, version: &str) {
        let mut root = staging.join("pool");
        if suite == Suite::Preview {
            root.push(PREVIEW_SUITE);
        }
        let pool = root.join(MAIN_COMPONENT).join("e").join(FIXTURE_PACKAGE);
        must(std::fs::create_dir_all(&pool), "pool dir");
        for arch in REQUIRED_ARCHES {
            make_deb(
                &pool,
                &canonical_pool_name(FIXTURE_PACKAGE, version, arch),
                FIXTURE_PACKAGE,
                version,
                arch,
                FIXTURE_BINARY,
                FIXTURE_IDENTITY,
                FIXTURE_COMMIT,
                "1.2.3",
                b"{}",
                b"fixture-daemon-bytes",
            );
        }
    }

    #[test]
    fn channel_update_emits_the_typed_channel_head() {
        // Stable head.
        let incoming = stable_incoming("channel-stable");
        let root = fixture_dir("channel-stable-root");
        staged_pool_with_candidate(&root, Suite::Stable, "1.2.3");
        let inputs = ChannelUpdateInputs {
            suite: Suite::Stable,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: "refs/tags/v1.2.3".to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            commit: FIXTURE_COMMIT.to_owned(),
            version: "v1.2.3".to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(MANIFEST_FILE),
            staging: &root,
        };
        must(run_channel_update(&inputs), "stable channel update");
        let state: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join("package-state.json")),
                "read state",
            )),
            "parse state",
        );
        assert_eq!(
            state["schema"],
            serde_json::Value::String(PACKAGE_STATE_SCHEMA.to_owned())
        );
        assert_eq!(
            state["version"],
            serde_json::Value::String("v1.2.3".to_owned())
        );
        assert_eq!(
            state["source_commit"],
            serde_json::Value::String(FIXTURE_COMMIT.to_owned())
        );
        let packages = state["packages"].as_array();
        let packages: &[serde_json::Value] = packages.map_or(&[], Vec::as_slice);
        assert_eq!(packages.len(), 2);
        assert_eq!(
            packages[0]["name"],
            serde_json::Value::String("example-1.2.3-amd64.deb".to_owned())
        );
        assert_eq!(
            packages[1]["name"],
            serde_json::Value::String("example-1.2.3-arm64.deb".to_owned())
        );
        let mut names: Vec<&str> = packages
            .iter()
            .filter_map(|package| package["name"].as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["example-1.2.3-amd64.deb", "example-1.2.3-arm64.deb"]
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);

        // Preview head records the dotted download keys.
        let incoming = preview_incoming("channel-preview");
        let root = fixture_dir("channel-preview-root");
        staged_pool_with_candidate(&root, Suite::Preview, PREVIEW_CANDIDATE);
        let inputs = ChannelUpdateInputs {
            suite: Suite::Preview,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: PREVIEW_SOURCE_REF.to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            commit: FIXTURE_COMMIT.to_owned(),
            version: PREVIEW_CANDIDATE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(PREVIEW_MANIFEST_FILE),
            staging: &root,
        };
        must(run_channel_update(&inputs), "preview channel update");
        let state: serde_json::Value = must(
            serde_json::from_slice(&must(
                std::fs::read(root.join("package-state-preview.json")),
                "read state",
            )),
            "parse state",
        );
        let packages = state["packages"].as_array();
        let packages: &[serde_json::Value] = packages.map_or(&[], Vec::as_slice);
        assert_eq!(packages.len(), 2);
        assert_eq!(
            packages[0]["name"],
            serde_json::Value::String(
                "example-preview-1.2.3.preview.41+0123456-amd64.deb".to_owned()
            )
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn channel_update_rejects_incoherent_heads() {
        let incoming = stable_incoming("channel-bad");
        let root = fixture_dir("channel-bad-root");
        staged_pool_with_candidate(&root, Suite::Stable, "1.2.3");
        // Wrong source ref for the suite.
        let inputs = ChannelUpdateInputs {
            suite: Suite::Stable,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: "refs/tags/v9.9.9".to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            commit: FIXTURE_COMMIT.to_owned(),
            version: "v1.2.3".to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(MANIFEST_FILE),
            staging: &root,
        };
        let error = must_fail(run_channel_update(&inputs), "bad ref");
        assert!(error.contains("must be the tag ref"), "{error}");
        // Manifest commit disagreement.
        let inputs = ChannelUpdateInputs {
            suite: Suite::Stable,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: "refs/tags/v1.2.3".to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            commit: "f".repeat(40),
            version: "v1.2.3".to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(MANIFEST_FILE),
            staging: &root,
        };
        let error = must_fail(run_channel_update(&inputs), "bad commit");
        assert!(error.contains("source_sha != commit"), "{error}");
        // Missing staged candidate.
        must(
            std::fs::remove_file(root.join("pool/main/e/example/example_1.2.3_amd64.deb")),
            "remove staged deb",
        );
        let inputs = ChannelUpdateInputs {
            suite: Suite::Stable,
            source_repo: FIXTURE_SOURCE.to_owned(),
            source_ref: "refs/tags/v1.2.3".to_owned(),
            preview_source_ref: PREVIEW_SOURCE_REF.to_owned(),
            commit: FIXTURE_COMMIT.to_owned(),
            version: "v1.2.3".to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            manifest: &incoming.dir.join(MANIFEST_FILE),
            staging: &root,
        };
        let error = must_fail(run_channel_update(&inputs), "missing pool deb");
        assert!(error.contains("missing from the pool"), "{error}");
        assert!(!root.join("package-state.json").exists());
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pool_naming_follows_the_debian_convention() {
        assert_eq!(pool_letter("example"), "e");
        assert_eq!(pool_letter("libexample"), "libe");
        assert_eq!(pool_letter("x"), "x");
        assert_eq!(
            canonical_pool_name("example", "1.2.3", "amd64"),
            "example_1.2.3_amd64.deb"
        );
        assert!(valid_pool_version("1.2.3"));
        assert!(valid_pool_version("1.2.3~preview.41+0123456"));
        assert!(valid_pool_version("2:1.0-1"));
        assert!(!valid_pool_version(""));
        assert!(!valid_pool_version("1.2.3;rm"));
        assert!(!valid_pool_version("a b"));
    }
}

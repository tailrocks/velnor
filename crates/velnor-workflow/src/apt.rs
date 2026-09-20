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
#[cfg(unix)]
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::fd::AsFd as _;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use sha2::{Digest as _, Sha256};

use crate::{GeneratorError, ReleaseSpec};

/// The release-record schema the coherence chain authenticates.
pub(crate) const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";
/// The publication-record schema staged publications emit.
pub(crate) const PUBLICATION_RECORD_SCHEMA: &str = "velnor.publication-record/v1";
/// The channel-state schema channel updates emit.
pub(crate) const PACKAGE_STATE_SCHEMA: &str = "velnor.apt-package-state.v1";
/// The producer-owned application manifest schema consumed by every
/// distribution projection.  This is deliberately distinct from the
/// subordinate package-release schema carried by `ReleaseSpec::manifest_schema`.
pub(crate) const PRODUCT_MANIFEST_SCHEMA: &str = "velnor.product-manifest/v1";
/// The one canonical application manifest asset selected by discovery.
pub(crate) const PRODUCT_MANIFEST_ASSET: &str = "product-manifest.json";
/// The persisted discovery result copied into the incoming artifact set.
pub(crate) const DISCOVERY_SELECTION_FILE: &str = "discovery.json";
/// The rolling release tag that carries preview coherence inputs.
pub(crate) const PREVIEW_TAG: &str = "preview";
/// The only source ref a preview manifest may name.
pub(crate) const PREVIEW_SOURCE_REF: &str = "refs/heads/main";
/// The sentinel a successful verification arms. Publication refuses to run
/// without it, so every rejection below lands before any mutation.
pub(crate) const SENTINEL_FILE: &str = ".reprepro-ok";
/// The exact architecture set a coherent release covers.
pub(crate) const REQUIRED_ARCHES: [&str; 2] = ["amd64", "arm64"];
/// The canonical product target census shared by the native producer and
/// distribution projections. APT consumes only the two Linux rows but must
/// reject a parent manifest that silently drops either Apple target.
const PRODUCT_TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];
/// The canonical component census shared by the native producer and both
/// distribution projections. A projection may filter artifact rows, but it
/// cannot silently publish a different product sibling set.
const PRODUCT_COMPONENTS: [&str; 3] = ["velnor-runner", "velnor-workflow", "velnorctl"];
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
    valid_keyring_path(value) && value != "." && !value.starts_with('.')
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
    if !is_bare_version(version) {
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
        && parts.iter().all(|part| {
            !part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && (value.len() == 1 || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit())
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

/// Parse a preview version, failing closed on any grammar violation.
pub(crate) fn parse_preview_version(value: &str) -> Result<PreviewVersion, GeneratorError> {
    let error = || {
        GeneratorError::usage(format!(
            "preview version is not X.Y.Z~preview.N+<7-hex>: {value}"
        ))
    };
    let (base, rest) = value.split_once("~preview.").ok_or_else(error)?;
    if !is_bare_version(base) {
        return Err(error());
    }
    let (seq, sha) = rest.split_once('+').ok_or_else(error)?;
    if !is_canonical_decimal(seq)
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

/// The asset-name form of a version: GitHub rewrites `~` to `.` on upload, so
/// served filenames carry the dotted form while every version-contract check
/// keeps the tilde form.
pub(crate) fn dotted_asset_version(version: &str) -> String {
    version.replace('~', ".")
}

/// Compare two bare `X.Y.Z` versions numerically per component.
fn cmp_bare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let parse = |value: &str| {
        value
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    };
    parse(left)?.partial_cmp(&parse(right)?)
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
    let left_seq = left.seq.parse::<u64>().map_err(|_| {
        GeneratorError::usage(format!("preview sequence is not numeric: {}", left.version))
    })?;
    let right_seq = right.seq.parse::<u64>().map_err(|_| {
        GeneratorError::usage(format!(
            "preview sequence is not numeric: {}",
            right.version
        ))
    })?;
    if left_seq != right_seq {
        return Ok(left_seq.cmp(&right_seq));
    }
    Ok(verrevcmp(&left.sha, &right.sha))
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
    /// Repository-relative immutable release selector. Empty is retained for
    /// schema-1 callers; schema-2 contracts must declare it explicitly.
    pub(crate) discovery_script: String,
    /// Canonical application manifest asset selected by the discovery script.
    pub(crate) canonical_manifest_asset: String,
    /// Exact schema URN of the canonical application manifest.
    pub(crate) canonical_manifest_schema: String,
    /// The pinned publisher signing-key fingerprint.
    pub(crate) signer: String,
    /// The environment secret holding the signing passphrase (name only).
    pub(crate) passphrase_secret: String,
    /// The environment secret holding the signing-key material (name only).
    pub(crate) signing_key_secret: String,
    /// The repository-local keyring the live fingerprint is read from.
    pub(crate) keyring: String,
    /// The `Origin`/`Label` stamped into suite metadata.
    pub(crate) origin: String,
    /// The packaged-identity directory inside the deb.
    pub(crate) identity_dir: String,
    /// The served feed base URL used for prior-pair recovery and the
    /// no-rollback deploy guard.
    pub(crate) feed_url: String,
    /// The `Description` stamped into suite metadata.
    pub(crate) description: String,
    /// The validated architecture set, always exactly both arches.
    pub(crate) arches: [String; 2],
    /// The validated retention policy.
    pub(crate) retention: Retention,
}

impl AptContract {
    /// Resolve a release spec into the typed contract. Empty `arches` selects
    /// both arches; a non-empty set must equal exactly both arches — a
    /// missing, duplicate, or foreign arch fails closed.
    pub(crate) fn resolve(spec: &ReleaseSpec) -> Result<Self, GeneratorError> {
        if spec.kind != "apt" {
            return Err(GeneratorError::usage(format!(
                "apt contract needs kind `apt`, found `{}`",
                spec.kind
            )));
        }
        if !valid_repository_slug(&spec.source_repository) {
            return Err(GeneratorError::usage(format!(
                "apt source_repository must be `owner/name`, found `{}`",
                spec.source_repository
            )));
        }
        if !valid_repository_slug(&spec.consumer_repository) {
            return Err(GeneratorError::usage(format!(
                "apt consumer_repository must be `owner/name`, found `{}`",
                spec.consumer_repository
            )));
        }
        if !valid_package_name(&spec.package) {
            return Err(GeneratorError::usage(format!(
                "apt package is not a safe package name: `{}`",
                spec.package
            )));
        }
        if !valid_binary_name(&spec.binary) {
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
        if !is_full_fingerprint(&normalize_fingerprint(&spec.signer_fingerprint)) {
            return Err(GeneratorError::usage(
                "apt signer_fingerprint must be a full 40-hex fingerprint",
            ));
        }
        if !valid_secret_ref(&spec.passphrase_secret) {
            return Err(GeneratorError::usage(
                "apt passphrase_secret must name an environment secret (uppercase identifier), never a value",
            ));
        }
        if !valid_secret_ref(&spec.signing_key_secret) {
            return Err(GeneratorError::usage(
                "apt signing_key_secret must name an environment secret (uppercase identifier), never a value",
            ));
        }
        let keyring = default_keyring(spec)?;
        let origin = default_origin(spec)?;
        let identity_dir = default_identity_dir(spec)?;
        if !valid_feed_url(&spec.apt_feed_url) {
            return Err(GeneratorError::usage(
                "apt feed_url must be an https URL with a host and an optional path",
            ));
        }
        let description = default_description(spec)?;
        let arches = parse_arch_set(&spec.apt_arches)?;
        let retention = Retention::parse(i64::from(spec.retention))?;
        Ok(Self {
            source_repo: spec.source_repository.clone(),
            package: spec.package.clone(),
            binary: spec.binary.clone(),
            consumer_repo: spec.consumer_repository.clone(),
            manifest_schema: spec.manifest_schema.clone(),
            discovery_script: String::new(),
            canonical_manifest_asset: String::new(),
            canonical_manifest_schema: String::new(),
            signer: normalize_fingerprint(&spec.signer_fingerprint),
            passphrase_secret: spec.passphrase_secret.clone(),
            signing_key_secret: spec.signing_key_secret.clone(),
            keyring,
            origin,
            identity_dir,
            feed_url: spec.apt_feed_url.clone(),
            description,
            arches,
            retention,
        })
    }

    /// Resolve the schema-2 APT contract through the same validator used by
    /// schema 1, then require the immutable application-selection seam that
    /// schema 2 owns. The adapter carries no behavior of its own: all package,
    /// signer, URL, architecture, and retention validation remains centralized
    /// in `resolve`.
    pub(crate) fn resolve_s2(spec: &crate::s2::ReleaseSpec) -> Result<Self, GeneratorError> {
        let legacy = crate::ReleaseSpec {
            kind: spec.kind.clone(),
            package: spec.package.clone(),
            packages: spec.packages.clone(),
            binary: spec.binary.clone(),
            targets: spec.targets.clone(),
            image: spec.image.clone(),
            image_package: spec.image_package.clone(),
            source_repository: spec.source_repository.clone(),
            consumer_repository: spec.consumer_repository.clone(),
            artifact_path: spec.artifact_path.clone(),
            description: spec.description.clone(),
            manifest_schema: spec.manifest_schema.clone(),
            apt_arches: spec.apt_arches.clone(),
            signer_fingerprint: spec.signer_fingerprint.clone(),
            passphrase_secret: spec.passphrase_secret.clone(),
            signing_key_secret: spec.signing_key_secret.clone(),
            keyring_path: spec.keyring_path.clone(),
            apt_origin: spec.apt_origin.clone(),
            apt_identity_dir: spec.apt_identity_dir.clone(),
            apt_feed_url: spec.apt_feed_url.clone(),
            retention: spec.retention,
            dockerfile: spec.dockerfile.clone(),
            context: spec.context.clone(),
            platforms: spec.platforms.clone(),
            producer_workflow: spec.producer_workflow.clone(),
            producer_conclusion: spec.producer_conclusion.clone(),
            modes: spec.modes.clone(),
            archive_members: spec.archive_members.clone(),
            archive_checksum: spec.archive_checksum.clone(),
            archive_retention_days: spec.archive_retention_days,
            credentials: Vec::new(),
            tag_pattern: spec.tag_pattern.clone(),
            registry: spec.registry.clone(),
            registry_username_secret: spec.registry_username_secret.clone(),
            registry_password_secret: spec.registry_password_secret.clone(),
            jobs: Vec::new(),
        };
        let mut contract = Self::resolve(&legacy)?;
        if !valid_discovery_script(&spec.discovery_script) {
            return Err(GeneratorError::usage(
                "apt discovery_script must be a safe repository-relative path",
            ));
        }
        if !valid_manifest_asset(&spec.canonical_manifest_asset) {
            return Err(GeneratorError::usage(
                "apt canonical_manifest_asset must be a safe asset file name",
            ));
        }
        if spec.canonical_manifest_asset != PRODUCT_MANIFEST_ASSET {
            return Err(GeneratorError::usage(format!(
                "apt canonical_manifest_asset must be {PRODUCT_MANIFEST_ASSET}"
            )));
        }
        if spec.canonical_manifest_schema.is_empty()
            || spec.canonical_manifest_schema.contains(char::is_whitespace)
        {
            return Err(GeneratorError::usage(
                "apt canonical_manifest_schema must be a non-empty schema URN without whitespace",
            ));
        }
        if spec.canonical_manifest_schema != PRODUCT_MANIFEST_SCHEMA {
            return Err(GeneratorError::usage(format!(
                "apt canonical_manifest_schema must be {PRODUCT_MANIFEST_SCHEMA}"
            )));
        }
        contract.discovery_script.clone_from(&spec.discovery_script);
        contract
            .canonical_manifest_asset
            .clone_from(&spec.canonical_manifest_asset);
        contract
            .canonical_manifest_schema
            .clone_from(&spec.canonical_manifest_schema);
        Ok(contract)
    }
}

fn valid_discovery_script(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

/// Validate the checked-in producer selector before a schema-2 config can
/// render a workflow that invokes it.  The source-owned path is opened with
/// the same component-wise no-follow and single-link rules as incoming
/// release assets; generation therefore cannot bind a symlink, parent swap,
/// or outside hard link as the discovery authority.
pub(crate) fn validate_discovery_script(root: &Path, relative: &str) -> Result<(), String> {
    if !valid_discovery_script(relative) {
        return Err("apt discovery_script must be a safe repository-relative path".to_owned());
    }
    let path = root.join(relative);
    let file = open_regular_file(&path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    if file
        .metadata()
        .map_err(|error| format!("stat discovery script {}: {error}", path.display()))?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err(format!(
            "apt discovery_script is not executable: {}",
            path.display()
        ));
    }
    Ok(())
}

fn valid_manifest_asset(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'~' | b'-')
        })
}

/// The validated keyring path: explicit, or `<package>.gpg` by default.
fn default_keyring(spec: &ReleaseSpec) -> Result<String, GeneratorError> {
    let keyring = if spec.keyring_path.is_empty() {
        format!("{}.gpg", spec.package)
    } else {
        spec.keyring_path.clone()
    };
    if !valid_keyring_path(&keyring) {
        return Err(GeneratorError::usage(format!(
            "apt keyring_path must be a relative path without traversal, found `{keyring}`"
        )));
    }
    Ok(keyring)
}

/// The validated origin: explicit, or the package name by default.
fn default_origin(spec: &ReleaseSpec) -> Result<String, GeneratorError> {
    let origin = if spec.apt_origin.is_empty() {
        spec.package.clone()
    } else {
        spec.apt_origin.clone()
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
fn default_identity_dir(spec: &ReleaseSpec) -> Result<String, GeneratorError> {
    let identity_dir = if spec.apt_identity_dir.is_empty() {
        spec.source_repository
            .split('/')
            .next_back()
            .unwrap_or_default()
            .to_owned()
    } else {
        spec.apt_identity_dir.clone()
    };
    if !valid_identity_dir(&identity_dir) {
        return Err(GeneratorError::usage(format!(
            "apt identity_dir is not a safe directory name: `{identity_dir}`"
        )));
    }
    Ok(identity_dir)
}

/// The validated feed description: explicit, or derived from the package.
fn default_description(spec: &ReleaseSpec) -> Result<String, GeneratorError> {
    let description = if spec.description.is_empty() {
        format!("apt repository for {}", spec.package)
    } else {
        spec.description.clone()
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
    let bytes = read_regular_file(path)?;
    Ok(sha256_hex(&bytes))
}

/// Read a JSON document, failing closed on IO or syntax errors.
fn read_json(path: &Path) -> Result<serde_json::Value, GeneratorError> {
    let bytes = read_regular_file(path)?;
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
    let bytes = read_regular_file(path)?;
    sidecar_digest_bytes(&bytes, path)
}

fn sidecar_digest_bytes(bytes: &[u8], path: &Path) -> Result<String, GeneratorError> {
    let text = String::from_utf8(bytes.to_owned())
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

/// Parse the canonical product-manifest sidecar shared with Homebrew. The
/// producer emits one GNU checksum row: digest plus the basename, never a
/// consumer-local path and never a digest-only shorthand.
fn product_manifest_sidecar_digest_bytes(
    bytes: &[u8],
    path: &Path,
) -> Result<String, GeneratorError> {
    let text = String::from_utf8(bytes.to_owned())
        .map_err(|_| GeneratorError::usage(format!("{} is not UTF-8", path.display())))?;
    let fields = text.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 || fields[1] != PRODUCT_MANIFEST_ASSET || !valid_digest(fields[0]) {
        return Err(GeneratorError::usage(format!(
            "{} must contain one digest row for {}",
            path.display(),
            PRODUCT_MANIFEST_ASSET
        )));
    }
    Ok(fields[0].to_owned())
}

fn product_manifest_sidecar_digest(path: &Path) -> Result<String, GeneratorError> {
    let bytes = read_regular_file(path)?;
    product_manifest_sidecar_digest_bytes(&bytes, path)
}

/// Require a coherence input to exist.
fn require_file(path: &Path) -> Result<(), GeneratorError> {
    let _ = open_regular_file(path)?;
    Ok(())
}

#[cfg(unix)]
fn open_path_nofollow(path: &Path, final_flags: rustix::fs::OFlags) -> std::io::Result<File> {
    // macOS exposes temporary directories through the stable system aliases
    // `/var` and `/tmp` (both may resolve to `/private/...`). Normalize only
    // those fixed aliases; all caller-owned components still traverse with
    // `O_NOFOLLOW` below and therefore cannot hide a parent symlink.
    let mut normalized = path.to_owned();
    for prefix in ["/var", "/tmp"] {
        let prefix_path = Path::new(prefix);
        if path.starts_with(prefix_path) {
            let Some(rest) = path.strip_prefix(prefix_path).ok() else {
                continue;
            };
            normalized = prefix_path.canonicalize()?.join(rest);
            break;
        }
    }
    let path = normalized.as_path();
    let directory_flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW;
    let base = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut parent = File::from(
        rustix::fs::openat(
            rustix::fs::CWD,
            base,
            directory_flags,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => names.push(name),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "path traversal is not permitted",
                ));
            }
        }
    }
    for (index, name) in names.iter().enumerate() {
        let flags = if index + 1 == names.len() {
            final_flags | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW
        } else {
            directory_flags
        };
        parent = File::from(
            rustix::fs::openat(&parent, *name, flags, rustix::fs::Mode::empty())
                .map_err(std::io::Error::from)?,
        );
    }
    Ok(parent)
}

#[cfg(unix)]
fn open_directory_nofollow(path: &Path) -> std::io::Result<File> {
    open_path_nofollow(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
    )
}

fn require_directory(path: &Path) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    let file = open_directory_nofollow(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GeneratorError::usage(format!("required directory missing: {}", path.display()))
        } else if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            GeneratorError::usage(format!(
                "required directory is a symlink: {}",
                path.display()
            ))
        } else {
            GeneratorError::io("open required directory", path, &error)
        }
    })?;
    #[cfg(not(unix))]
    let file = OpenOptions::new().read(true).open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GeneratorError::usage(format!("required directory missing: {}", path.display()))
        } else {
            GeneratorError::io("open required directory", path, &error)
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| GeneratorError::io("stat required directory", path, &error))?;
    if !metadata.is_dir() {
        return Err(GeneratorError::usage(format!(
            "required path is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Open one input once, without following the final path component. The
/// verifier consumes the returned descriptor, not a later path reopen. A
/// selected artifact must be owned by this handoff directory (`nlink == 1`);
/// accepting a hard link would let an outside writer mutate bytes after the
/// path check and before verification.
fn open_regular_file(path: &Path) -> Result<File, GeneratorError> {
    #[cfg(unix)]
    let file = open_path_nofollow(path, rustix::fs::OFlags::RDONLY).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GeneratorError::usage(format!("required file missing: {}", path.display()))
        } else if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            GeneratorError::usage(format!("required file is a symlink: {}", path.display()))
        } else {
            GeneratorError::io("open required file", path, &error)
        }
    })?;
    #[cfg(not(unix))]
    let mut options = OpenOptions::new();
    #[cfg(not(unix))]
    options.read(true);
    #[cfg(not(unix))]
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GeneratorError::usage(format!("required file missing: {}", path.display()))
        } else if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            GeneratorError::usage(format!("required file is a symlink: {}", path.display()))
        } else {
            GeneratorError::io("open required file", path, &error)
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| GeneratorError::io("stat required file", path, &error))?;
    if !metadata.is_file() {
        return Err(GeneratorError::usage(format!(
            "required path is not a regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(GeneratorError::usage(format!(
            "required file has multiple links: {}",
            path.display()
        )));
    }
    Ok(file)
}

/// Read one regular input through the descriptor that was validated for it.
fn read_regular_file(path: &Path) -> Result<Vec<u8>, GeneratorError> {
    let mut file = open_regular_file(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| GeneratorError::io("read required file", path, &error))?;
    Ok(bytes)
}

/// Read one regular input relative to an already-open directory. The parent
/// descriptor is the capability boundary: replacing the incoming pathname or
/// one of its ancestors cannot redirect this open.
#[cfg(unix)]
fn read_regular_file_at(
    directory: &File,
    name: &str,
    display: &Path,
) -> Result<Vec<u8>, GeneratorError> {
    let file = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        let error = std::io::Error::from(error);
        if error.kind() == std::io::ErrorKind::NotFound {
            GeneratorError::usage(format!("required file missing: {}", display.display()))
        } else if error.kind() == std::io::ErrorKind::InvalidInput {
            GeneratorError::usage(format!("required file is not safe: {}", display.display()))
        } else {
            GeneratorError::io("open required file", display, &error)
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| GeneratorError::io("stat required file", display, &error))?;
    if !metadata.is_file() {
        return Err(GeneratorError::usage(format!(
            "required path is not a regular file: {}",
            display.display()
        )));
    }
    if metadata.nlink() != 1 {
        return Err(GeneratorError::usage(format!(
            "required file has multiple links: {}",
            display.display()
        )));
    }
    let mut bytes = Vec::new();
    (&file)
        .read_to_end(&mut bytes)
        .map_err(|error| GeneratorError::io("read required file", display, &error))?;
    Ok(bytes)
}

/// The sentinel is a fresh proof for one incoming handoff. Schema-2 binds it
/// to the exact persisted selection bytes and every selected asset's bytes.
/// There is no legacy fixed-marker mode: an incoming handoff without the
/// producer-owned application selection is not publishable. A pre-existing
/// marker is never overwritten, so failed or stale verification cannot arm
/// publication.
fn expected_sentinel(incoming: &Path) -> Result<Vec<u8>, GeneratorError> {
    let selection = incoming.join(DISCOVERY_SELECTION_FILE);
    match std::fs::symlink_metadata(&selection) {
        Ok(_) => {
            let selection_bytes = read_regular_file(&selection)?;
            let document =
                serde_json::from_slice::<serde_json::Value>(&selection_bytes).map_err(|error| {
                    GeneratorError::usage(format!(
                        "{} is not valid JSON: {error}",
                        selection.display()
                    ))
                })?;
            let parsed = parse_discovery_selection(&document)?;
            let mut proof = format!("selection:{}\n", sha256_hex(&selection_bytes));
            for asset in parsed.release_assets {
                let path = incoming.join(&asset.name);
                let bytes = read_regular_file(&path)?;
                if bytes.len() as u64 != asset.size {
                    return Err(GeneratorError::usage(format!(
                        "incoming asset {} size differs from discovery",
                        asset.name
                    )));
                }
                proof.push_str("asset:");
                proof.push_str(&asset.name);
                proof.push(':');
                proof.push_str(&sha256_hex(&bytes));
                proof.push('\n');
            }
            Ok(proof.into_bytes())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(GeneratorError::usage(
            "APT verification requires the producer-owned discovery selection; legacy handoff removed",
        )),
        Err(error) => Err(GeneratorError::io(
            "stat discovery selection",
            &selection,
            &error,
        )),
    }
}

fn check_sentinel(incoming: &Path) -> Result<(), GeneratorError> {
    let path = incoming.join(SENTINEL_FILE);
    let actual = read_regular_file(&path)?;
    if actual != expected_sentinel(incoming)? {
        return Err(GeneratorError::usage(
            "APT verification sentinel is stale or bound to a different selection",
        ));
    }
    Ok(())
}

fn arm_sentinel(incoming: &Path) -> Result<(), GeneratorError> {
    let path = incoming.join(SENTINEL_FILE);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists
            || std::fs::symlink_metadata(&path).is_ok()
        {
            GeneratorError::usage(
                "APT verification sentinel already exists; refusing to reuse stale proof",
            )
        } else {
            GeneratorError::io("create verification sentinel", &path, &error)
        }
    })?;
    file.write_all(&expected_sentinel(incoming)?)
        .map_err(|error| GeneratorError::io("write verification sentinel", &path, &error))?;
    file.sync_all()
        .map_err(|error| GeneratorError::io("sync verification sentinel", &path, &error))?;
    #[cfg(unix)]
    if file.metadata().map_or(0, |metadata| metadata.nlink()) != 1 {
        return Err(GeneratorError::usage(
            "APT verification sentinel has multiple links",
        ));
    }
    Ok(())
}

fn create_new_regular_file(
    path: &Path,
    bytes: &[u8],
    operation: &'static str,
) -> Result<(), GeneratorError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists
            || std::fs::symlink_metadata(path).is_ok()
        {
            GeneratorError::usage(format!("{operation} destination already exists"))
        } else {
            GeneratorError::io(operation, path, &error)
        }
    })?;
    file.write_all(bytes)
        .map_err(|error| GeneratorError::io(operation, path, &error))?;
    file.sync_all()
        .map_err(|error| GeneratorError::io("sync file", path, &error))?;
    #[cfg(unix)]
    if file.metadata().map_or(0, |metadata| metadata.nlink()) != 1 {
        return Err(GeneratorError::usage(format!(
            "{operation} destination has multiple links"
        )));
    }
    Ok(())
}

/// Open an incoming directory, creating only its final component relative to
/// an already-open parent. `create_dir_all` and pathname `mkdir` leave a
/// parent replacement race between checking and creation.
#[cfg(unix)]
fn open_or_create_directory(path: &Path) -> Result<File, GeneratorError> {
    match open_directory_nofollow(path) {
        Ok(directory) => return Ok(directory),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(GeneratorError::io("open directory", path, &error));
        }
        Err(_) => {}
    }
    let parent = path
        .parent()
        .ok_or_else(|| GeneratorError::usage("directory path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| GeneratorError::usage("directory path has no name"))?;
    let parent_file = open_directory_nofollow(parent)
        .map_err(|error| GeneratorError::io("open directory parent", parent, &error))?;
    match rustix::fs::mkdirat(&parent_file, name, rustix::fs::Mode::from_raw_mode(0o700)) {
        Ok(()) => {}
        Err(error) => {
            let error = std::io::Error::from(error);
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(GeneratorError::io("create directory", path, &error));
            }
        }
    }
    let directory = File::from(
        rustix::fs::openat(
            &parent_file,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            let error = std::io::Error::from(error);
            GeneratorError::io("open created directory", path, &error)
        })?,
    );
    let metadata = directory
        .metadata()
        .map_err(|error| GeneratorError::io("stat created directory", path, &error))?;
    if !metadata.is_dir() {
        return Err(GeneratorError::usage(format!(
            "created path is not a directory: {}",
            path.display()
        )));
    }
    Ok(directory)
}

/// Create one regular file relative to a held directory descriptor.
#[cfg(unix)]
fn create_new_regular_file_at(
    directory: &File,
    name: &str,
    bytes: &[u8],
    operation: &'static str,
    display: &Path,
) -> Result<(), GeneratorError> {
    let mut file = File::from(
        rustix::fs::openat(
            directory,
            name,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o644),
        )
        .map_err(|error| {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                GeneratorError::usage(format!("{operation} destination already exists"))
            } else {
                GeneratorError::io(operation, display, &error)
            }
        })?,
    );
    file.write_all(bytes)
        .map_err(|error| GeneratorError::io(operation, display, &error))?;
    file.sync_all()
        .map_err(|error| GeneratorError::io("sync file", display, &error))?;
    if file.metadata().map_or(0, |metadata| metadata.nlink()) != 1 {
        return Err(GeneratorError::usage(format!(
            "{operation} destination has multiple links"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn rename_at(
    directory: &File,
    old_name: &str,
    new_name: &str,
    display: &Path,
) -> Result<(), GeneratorError> {
    rustix::fs::renameat(directory, old_name, directory, new_name).map_err(|error| {
        GeneratorError::io(
            "install downloaded asset",
            display,
            &std::io::Error::from(error),
        )
    })
}

/// Install bytes without following a pre-existing destination link. A staged
/// immutable asset may already be present only when its bytes are identical;
/// every other existing destination is a collision or an unsafe path.
fn install_regular_file(
    path: &Path,
    bytes: &[u8],
    operation: &'static str,
) -> Result<(), GeneratorError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_new_regular_file(path, bytes, operation)
        }
        Err(error) => Err(GeneratorError::io("stat destination", path, &error)),
        Ok(_) => {
            let existing = read_regular_file(path)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(GeneratorError::usage(format!(
                    "{operation} destination differs from immutable input"
                )))
            }
        }
    }
}

/// Materialize a descriptor-verified input inside a private scratch directory
/// before handing it to a pathname-based archive tool. The tool therefore
/// cannot reopen the caller's mutable incoming path.
fn materialize_verified_file(path: &Path) -> Result<(PathBuf, PathBuf), GeneratorError> {
    let bytes = read_regular_file(path)?;
    materialize_verified_bytes(&bytes)
}

fn materialize_verified_bytes(bytes: &[u8]) -> Result<(PathBuf, PathBuf), GeneratorError> {
    let scratch = scratch_dir("verified-input")?;
    let materialized = scratch.join("input");
    #[cfg(unix)]
    let result = open_directory_nofollow(&scratch)
        .map_err(|error| GeneratorError::io("open materialization directory", &scratch, &error))
        .and_then(|directory| {
            create_new_regular_file_at(
                &directory,
                "input",
                bytes,
                "materialize verified input",
                &materialized,
            )
        });
    #[cfg(not(unix))]
    let result = create_new_regular_file(&materialized, bytes, "materialize verified input");
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&scratch);
        return Err(error);
    }
    Ok((scratch, materialized))
}

/// Whether a directory entry is a `.deb` file. The match is deliberately
/// case-sensitive, like the oracle's glob: a `.DEB` file is not a
/// coherence input.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_deb_file(name: &str) -> bool {
    name.ends_with(".deb")
}

/// List the entries of a directory by file name.
#[cfg(unix)]
fn directory_entry_name(raw_name: &[u8], dir: &Path) -> Result<String, GeneratorError> {
    OsString::from_vec(raw_name.to_owned())
        .into_string()
        .map_err(|_| {
            GeneratorError::usage(format!(
                "directory {} contains a non-UTF-8 entry",
                dir.display()
            ))
        })
}

#[cfg(unix)]
fn dir_names_from_file(directory: &File, dir: &Path) -> Result<Vec<String>, GeneratorError> {
    let mut names = Vec::new();
    let mut entries = rustix::fs::Dir::read_from(directory)
        .map_err(|error| GeneratorError::io("list", dir, &std::io::Error::from(error)))?;
    while let Some(entry) = entries.read() {
        let entry =
            entry.map_err(|error| GeneratorError::io("list", dir, &std::io::Error::from(error)))?;
        let raw_name = entry.file_name().to_bytes();
        if raw_name == b"." || raw_name == b".." {
            continue;
        }
        let name = directory_entry_name(raw_name, dir)?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

#[allow(clippy::needless_return)]
fn dir_names(dir: &Path) -> Result<Vec<String>, GeneratorError> {
    #[cfg(unix)]
    {
        let directory = open_directory_nofollow(dir)
            .map_err(|error| GeneratorError::io("open directory for listing", dir, &error))?;
        return dir_names_from_file(&directory, dir);
    }
    #[cfg(not(unix))]
    {
        let mut names = Vec::new();
        require_directory(dir)?;
        let entries =
            std::fs::read_dir(dir).map_err(|error| GeneratorError::io("list", dir, &error))?;
        for entry in entries {
            let entry = entry.map_err(|error| GeneratorError::io("list", dir, &error))?;
            let name = entry.file_name().into_string().map_err(|_| {
                GeneratorError::usage(format!(
                    "directory {} contains a non-UTF-8 entry",
                    dir.display()
                ))
            })?;
            names.push(name);
        }
        names.sort();
        return Ok(names);
    }
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

/// Extract one VALIDSIG and bind its signing key to the configured primary.
/// GnuPG reports the signing-subkey fingerprint first and the primary-key
/// fingerprint last; accepting any valid signing subkey under the pinned
/// primary keeps normal keyrings usable without broadening publisher trust.
fn gpgv_signer(output: &[u8], expected: &str, label: &str) -> Result<(), GeneratorError> {
    let text = String::from_utf8(output.to_vec())
        .map_err(|_| GeneratorError::usage(format!("{label} signer status is not UTF-8")))?;
    let signers = text
        .lines()
        .filter_map(|line| line.strip_prefix("[GNUPG:] VALIDSIG "))
        .map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 10 {
                return Err(GeneratorError::usage(format!(
                    "{label} VALIDSIG status has an unexpected field count"
                )));
            }
            let signer = normalize_fingerprint(fields[0]);
            let primary = normalize_fingerprint(fields[9]);
            if !is_full_fingerprint(&signer) || !is_full_fingerprint(&primary) {
                return Err(GeneratorError::usage(format!(
                    "{label} VALIDSIG status has an invalid fingerprint"
                )));
            }
            Ok((signer, primary))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if signers.len() != 1 || signers[0].1 != expected {
        return Err(GeneratorError::usage(format!(
            "{label} signature signer does not match the pinned publisher key"
        )));
    }
    Ok(())
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

/// One immutable GitHub release asset carried by the discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveryAsset {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) state: String,
    pub(crate) browser_download_url: String,
}

/// The persisted application-release selection. This is a consumer of the
/// source-owned discovery script, never another release selector: every
/// source/ref/version and asset identity used by the S2 runtime comes from
/// this value and is validated before any download or feed mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoverySelection {
    pub(crate) channel: String,
    pub(crate) product_id: String,
    pub(crate) source_repository: String,
    pub(crate) package: String,
    pub(crate) tag: String,
    pub(crate) release_tag: String,
    pub(crate) version: String,
    pub(crate) source_ref: String,
    pub(crate) source_commit: String,
    pub(crate) manifest_asset: String,
    pub(crate) manifest_schema: String,
    pub(crate) manifest_sha256: String,
    pub(crate) release_id: String,
    pub(crate) provider_release_id: u64,
    pub(crate) release_url: String,
    pub(crate) release_assets: Vec<DiscoveryAsset>,
    pub(crate) manifest: serde_json::Value,
}

impl DiscoverySelection {
    /// The suite identity carried by the canonical product channel.
    pub(crate) fn suite(&self) -> Result<Suite, GeneratorError> {
        Suite::parse(&self.channel)
    }

    /// Convert the producer's canonical product version into the Debian
    /// version consumed by the shared APT verifier. Stable product versions
    /// are bare X.Y.Z; APT tags them as vX.Y.Z. Product preview versions use
    /// the canonical -preview. separator while Debian uses ~preview. for
    /// ordering. No current release or mutable pointer is consulted.
    pub(crate) fn apt_version(&self) -> Result<String, GeneratorError> {
        match self.channel.as_str() {
            "stable" => {
                let tag = parse_stable_tag(&self.tag)?;
                if self.version != tag.version || self.release_tag != self.tag {
                    return Err(GeneratorError::usage(
                        "discovery stable tag and product version disagree",
                    ));
                }
                Ok(tag.tag)
            }
            "preview" => {
                let (apt, _) = parse_product_preview_version(&self.version)?;
                Ok(apt)
            }
            _ => Err(GeneratorError::usage(format!(
                "discovery channel is unsupported: {}",
                self.channel
            ))),
        }
    }
}

/// The provider release ID is serialized as a canonical positive decimal
/// string in the producer manifest. This is shared with Homebrew: leading
/// zeroes, slashes, and alternate provider namespaces are not equivalent
/// release identities.
pub(crate) fn valid_release_id(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() && *byte != b'0')
        && value.bytes().skip(1).all(|byte| byte.is_ascii_digit())
}

fn valid_discovery_asset_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'~' | b'-')
        })
}

fn exact_object_keys(
    document: &serde_json::Value,
    expected: &[&str],
    label: &str,
) -> Result<(), GeneratorError> {
    let object = document
        .as_object()
        .ok_or_else(|| GeneratorError::usage(format!("{label} is not an object")))?;
    let actual = object.keys().cloned().collect::<BTreeSet<_>>();
    let expected = expected
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(GeneratorError::usage(format!(
            "{label} has an unexpected field set"
        )));
    }
    Ok(())
}

fn parse_product_preview_version(value: &str) -> Result<(String, String), GeneratorError> {
    let error = || {
        GeneratorError::usage(format!(
            "discovery preview version is not X.Y.Z-preview.N+<7-hex>: {value}"
        ))
    };
    let (base, rest) = value.split_once("-preview.").ok_or_else(error)?;
    if !is_bare_version(base) {
        return Err(error());
    }
    let (sequence, sha) = rest.split_once('+').ok_or_else(error)?;
    if !is_canonical_decimal(sequence) || !is_lower_hex(sha, 7) {
        return Err(error());
    }
    Ok((format!("{base}~preview.{sequence}+{sha}"), sha.to_owned()))
}

#[allow(
    clippy::too_many_lines,
    reason = "canonical product schema validation stays one auditable gate"
)]
fn validate_product_manifest_selection(
    manifest: &serde_json::Value,
    selection: &serde_json::Value,
) -> Result<(), GeneratorError> {
    exact_object_keys(
        manifest,
        &[
            "artifacts",
            "channel",
            "components",
            "product_id",
            "release_id",
            "release_tag",
            "schema",
            "source_commit",
            "source_ref",
            "source_repository",
            "version",
        ],
        "discovery product manifest",
    )?;
    let channel = field(selection, "channel")?;
    if field(manifest, "schema")? != PRODUCT_MANIFEST_SCHEMA
        || field(manifest, "product_id")? != field(selection, "product_id")?
        || field(manifest, "channel")? != channel
        || field(manifest, "source_repository")? != field(selection, "source_repository")?
        || field(manifest, "source_ref")? != field(selection, "source_ref")?
        || field(manifest, "source_commit")? != field(selection, "source_commit")?
        || field(manifest, "release_tag")? != field(selection, "release_tag")?
        || field(manifest, "version")? != field(selection, "version")?
        || field(manifest, "release_id")? != field(selection, "release_id")?
    {
        return Err(GeneratorError::usage(
            "discovery product manifest identity does not match selection",
        ));
    }
    let components = manifest
        .get("components")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery components are not an array"))?;
    if components.len() != 3 {
        return Err(GeneratorError::usage(
            "discovery product manifest must carry exactly three components",
        ));
    }
    let mut component_names = BTreeSet::new();
    for component in components {
        exact_object_keys(
            component,
            &[
                "binary", "crate", "feature", "identity", "name", "targets", "version",
            ],
            "discovery product component",
        )?;
        let name = field(component, "name")?;
        let crate_name = field(component, "crate")?;
        let binary = field(component, "binary")?;
        let feature = component
            .get("feature")
            .ok_or_else(|| GeneratorError::usage("discovery component feature is missing"))?;
        let identity = field(component, "identity")?;
        let version = field(component, "version")?;
        let targets = component
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| GeneratorError::usage("discovery component targets are not an array"))?;
        if !valid_package_name(name)
            || !valid_package_name(crate_name)
            || !valid_binary_name(binary)
            || name != crate_name
            || name != binary
            || !(feature.is_null() || feature.as_str() == Some("release-build"))
            || !matches!(identity, "version" | "revision")
            || !is_bare_version(version)
            || targets.len() != PRODUCT_TARGETS.len()
            || !component_names.insert(name)
        {
            return Err(GeneratorError::usage(
                "discovery product component identity is invalid or duplicated",
            ));
        }
        for target in targets {
            let target = target.as_str().ok_or_else(|| {
                GeneratorError::usage("discovery component target is not a string")
            })?;
            if !PRODUCT_TARGETS.contains(&target) {
                return Err(GeneratorError::usage(
                    "discovery component target is outside the canonical product census",
                ));
            }
        }
        let target_set = targets
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        if target_set.len() != PRODUCT_TARGETS.len()
            || PRODUCT_TARGETS
                .iter()
                .any(|target| !target_set.contains(target))
        {
            return Err(GeneratorError::usage(
                "discovery component target census is incomplete",
            ));
        }
    }
    if component_names != PRODUCT_COMPONENTS.iter().copied().collect() {
        return Err(GeneratorError::usage(
            "discovery product component census differs from the canonical product",
        ));
    }
    let artifacts = manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery artifacts are not an array"))?;
    if artifacts.is_empty() {
        return Err(GeneratorError::usage(
            "discovery product manifest has no artifact inventory",
        ));
    }
    let mut artifact_names = BTreeSet::new();
    for artifact in artifacts {
        exact_object_keys(
            artifact,
            &["kind", "name", "sha256", "size", "target"],
            "discovery product artifact",
        )?;
        let name = field(artifact, "name")?;
        if !valid_discovery_asset_name(name) {
            return Err(GeneratorError::usage(
                "discovery product artifact has an unsafe name",
            ));
        }
        if name == DISCOVERY_SELECTION_FILE
            || name == PRODUCT_MANIFEST_ASSET
            || !artifact_names.insert(name)
        {
            return Err(GeneratorError::usage(
                "discovery product artifact name is reserved or duplicated",
            ));
        }
        let size = positive_field(artifact, "size")?;
        if size == 0 || !valid_digest(field(artifact, "sha256")?) {
            return Err(GeneratorError::usage(
                "discovery product artifact has an invalid size or digest",
            ));
        }
        let kind = field(artifact, "kind")?;
        let target = field(artifact, "target")?;
        if !matches!(
            kind,
            "binary" | "archive" | "homebrew-archive" | "apt-package"
        ) || !PRODUCT_TARGETS.contains(&target)
        {
            return Err(GeneratorError::usage(
                "discovery product artifact kind or target is outside the canonical census",
            ));
        }
    }
    let component_targets = components
        .iter()
        .flat_map(|component| {
            component
                .get("targets")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    let artifact_rows = artifacts
        .iter()
        .filter_map(|artifact| {
            Some((
                field(artifact, "name").ok()?,
                field(artifact, "kind").ok()?,
                field(artifact, "target").ok()?,
            ))
        })
        .collect::<Vec<_>>();
    if component_targets.len() != PRODUCT_TARGETS.len()
        || PRODUCT_TARGETS
            .iter()
            .any(|target| !component_targets.contains(target))
        || artifact_rows.len() != 18
        || artifact_rows
            .iter()
            .filter(|(_, kind, _)| *kind == "binary")
            .count()
            != 12
        || artifact_rows
            .iter()
            .filter(|(_, kind, _)| *kind == "apt-package")
            .count()
            != 2
        || artifact_rows
            .iter()
            .filter(|(_, kind, _)| matches!(*kind, "archive" | "homebrew-archive"))
            .count()
            != 4
    {
        return Err(GeneratorError::usage(
            "discovery product artifact inventory is not the canonical 18-row census",
        ));
    }
    for component in components {
        let binary = field(component, "binary")?;
        for target in PRODUCT_TARGETS {
            let suffixed = format!("{binary}-{target}");
            if !artifact_rows.iter().any(|(name, kind, row_target)| {
                *kind == "binary" && *row_target == target && (*name == binary || *name == suffixed)
            }) {
                return Err(GeneratorError::usage(
                    "discovery product manifest is missing a component binary artifact",
                ));
            }
        }
    }
    for target in PRODUCT_TARGETS {
        let archive_kind = if target.ends_with("-apple-darwin") {
            "homebrew-archive"
        } else {
            "archive"
        };
        if artifact_rows
            .iter()
            .filter(|(_, kind, row_target)| *kind == archive_kind && *row_target == target)
            .count()
            != 1
        {
            return Err(GeneratorError::usage(
                "discovery product manifest is missing a target archive",
            ));
        }
    }
    if artifact_rows
        .iter()
        .any(|(_, kind, target)| *kind == "apt-package" && !target.ends_with("-unknown-linux-gnu"))
    {
        return Err(GeneratorError::usage(
            "discovery APT artifacts must target Linux architectures",
        ));
    }
    for (name, _kind, target) in artifact_rows
        .iter()
        .filter(|(_, kind, _)| *kind == "apt-package")
    {
        let expected_target = if name.ends_with("-amd64.deb") || name.ends_with("_amd64.deb") {
            "x86_64-unknown-linux-gnu"
        } else if name.ends_with("-arm64.deb") || name.ends_with("_arm64.deb") {
            "aarch64-unknown-linux-gnu"
        } else {
            return Err(GeneratorError::usage(
                "discovery APT artifact name must end in -amd64.deb, _amd64.deb, -arm64.deb, or _arm64.deb",
            ));
        };
        if *target != expected_target {
            return Err(GeneratorError::usage(
                "discovery APT artifact name and target disagree",
            ));
        }
    }
    if artifact_rows
        .iter()
        .filter(|(_, kind, target)| *kind == "apt-package" && *target == "x86_64-unknown-linux-gnu")
        .count()
        != 1
        || artifact_rows
            .iter()
            .filter(|(_, kind, target)| {
                *kind == "apt-package" && *target == "aarch64-unknown-linux-gnu"
            })
            .count()
            != 1
    {
        return Err(GeneratorError::usage(
            "discovery APT artifact census must contain exactly one amd64 and one arm64 package",
        ));
    }
    Ok(())
}

fn validate_source_ref_resolution(
    document: &serde_json::Value,
    channel: &str,
    tag: &str,
    commit: &str,
) -> Result<(), GeneratorError> {
    let resolution = document
        .get("source_ref_resolution")
        .ok_or_else(|| GeneratorError::usage("discovery source-ref proof is missing"))?;
    let expected = if channel == "preview" {
        [
            "declared_ref_provenance",
            "method",
            "proof_ref",
            "resolved_commit",
        ]
        .as_slice()
    } else {
        ["method", "proof_ref", "resolved_commit"].as_slice()
    };
    exact_object_keys(resolution, expected, "discovery source-ref proof")?;
    if field(resolution, "proof_ref")? != format!("refs/tags/{tag}")
        || field(resolution, "resolved_commit")? != commit
        || field(resolution, "method")? != "github-git-ref"
    {
        return Err(GeneratorError::usage(
            "discovery source-ref proof does not bind the selected tag",
        ));
    }
    if channel == "preview" {
        let declared = resolution
            .get("declared_ref_provenance")
            .ok_or_else(|| GeneratorError::usage("preview branch provenance is missing"))?;
        exact_object_keys(
            declared,
            &[
                "base_commit",
                "head_commit",
                "merge_base_commit",
                "method",
                "ref",
                "relation",
                "status",
            ],
            "preview branch provenance",
        )?;
        if field(declared, "ref")? != PREVIEW_SOURCE_REF
            || field(declared, "method")? != "github-compare-ancestry"
            || field(declared, "head_commit")? != commit
            || field(declared, "merge_base_commit")? != commit
            || !valid_commit(field(declared, "base_commit")?)
            || !matches!(field(declared, "relation")?, "ancestor" | "tip")
            || !matches!(field(declared, "status")?, "behind" | "identical")
        {
            return Err(GeneratorError::usage(
                "preview branch provenance does not prove main ancestry",
            ));
        }
    }
    Ok(())
}

/// The `.deb` read backend. `Auto` prefers `dpkg-deb` and falls back to
/// portable `ar` + `tar` so verification also runs where `dpkg` is absent;
/// `ArTar` forces the fallback. Both paths take fixed arguments only.
/// Parse and validate one source-owned discovery result. The exact top-level
/// shape is intentional: accepting a subset would let a later consumer omit
/// the provenance fields that make an immutable selection auditable.
#[allow(
    clippy::too_many_lines,
    reason = "selection parsing is one exact immutable contract gate"
)]
pub(crate) fn read_discovery_selection(path: &Path) -> Result<DiscoverySelection, GeneratorError> {
    let document = read_json(path)?;
    parse_discovery_selection(&document)
}

#[allow(
    clippy::too_many_lines,
    reason = "selection parsing is one exact immutable contract gate"
)]
fn parse_discovery_selection(
    document: &serde_json::Value,
) -> Result<DiscoverySelection, GeneratorError> {
    exact_object_keys(
        document,
        &[
            "channel",
            "manifest",
            "manifest_asset",
            "manifest_schema",
            "manifest_sha256",
            "package",
            "product_id",
            "provider_release_id",
            "published_at",
            "release_assets",
            "release_id",
            "release_tag",
            "release_url",
            "source_commit",
            "source_ref",
            "source_ref_resolution",
            "source_repository",
            "tag",
            "target_commitish",
            "version",
        ],
        "discovery selection",
    )?;
    let channel = field(document, "channel")?.to_owned();
    if !matches!(channel.as_str(), "stable" | "preview") {
        return Err(GeneratorError::usage(
            "discovery channel must be stable or preview",
        ));
    }
    let product_id = field(document, "product_id")?.to_owned();
    if product_id.is_empty() {
        return Err(GeneratorError::usage(
            "discovery product_id must be non-empty",
        ));
    }
    let source_repository = field(document, "source_repository")?.to_owned();
    if !valid_repository_slug(&source_repository) {
        return Err(GeneratorError::usage(
            "discovery source repository is not an owner/name slug",
        ));
    }
    let package = field(document, "package")?.to_owned();
    if !valid_package_name(&package) {
        return Err(GeneratorError::usage(
            "discovery package is not a safe package name",
        ));
    }
    let tag = field(document, "tag")?.to_owned();
    let release_tag = field(document, "release_tag")?.to_owned();
    if tag != release_tag {
        return Err(GeneratorError::usage(
            "discovery tag and release_tag differ",
        ));
    }
    let version = field(document, "version")?.to_owned();
    let source_ref = field(document, "source_ref")?.to_owned();
    let source_commit = field(document, "source_commit")?.to_owned();
    if !valid_commit(&source_commit) {
        return Err(GeneratorError::usage(
            "discovery source_commit is not 40 lowercase hex characters",
        ));
    }
    match channel.as_str() {
        "stable" => {
            let stable = parse_stable_tag(&tag)?;
            if stable.version != version || source_ref != format!("refs/tags/{tag}") {
                return Err(GeneratorError::usage(
                    "discovery stable version or source ref is inconsistent",
                ));
            }
        }
        "preview" => {
            let (_, preview_sha) = parse_product_preview_version(&version)?;
            if tag.strip_prefix("preview-").unwrap_or_default() != source_commit
                || preview_sha != source_commit[..7]
                || source_ref != PREVIEW_SOURCE_REF
            {
                return Err(GeneratorError::usage(
                    "discovery preview tag, version, or source ref is inconsistent",
                ));
            }
        }
        _ => {
            return Err(GeneratorError::usage("discovery channel is unsupported"));
        }
    }
    validate_source_ref_resolution(document, &channel, &tag, &source_commit)?;
    let manifest_asset = field(document, "manifest_asset")?.to_owned();
    if manifest_asset != PRODUCT_MANIFEST_ASSET {
        return Err(GeneratorError::usage(format!(
            "discovery manifest asset must be {PRODUCT_MANIFEST_ASSET}"
        )));
    }
    let manifest_schema = field(document, "manifest_schema")?.to_owned();
    if manifest_schema != PRODUCT_MANIFEST_SCHEMA {
        return Err(GeneratorError::usage(format!(
            "discovery manifest schema must be {PRODUCT_MANIFEST_SCHEMA}"
        )));
    }
    let manifest_sha256 = field(document, "manifest_sha256")?.to_owned();
    if !valid_digest(&manifest_sha256) {
        return Err(GeneratorError::usage(
            "discovery manifest_sha256 is not a lowercase SHA-256 digest",
        ));
    }
    let release_id = field(document, "release_id")?.to_owned();
    if !valid_release_id(&release_id) {
        return Err(GeneratorError::usage(
            "discovery release_id has an invalid shared grammar",
        ));
    }
    let provider_release_id = positive_field(document, "provider_release_id")?;
    let parsed_release_id = release_id.parse::<u64>().map_err(|_| {
        GeneratorError::usage("discovery release_id does not fit the provider ID type")
    })?;
    if parsed_release_id != provider_release_id {
        return Err(GeneratorError::usage(
            "discovery release_id differs from provider_release_id",
        ));
    }
    let expected_release_url = format!("https://github.com/{source_repository}/releases/tag/{tag}");
    if field(document, "release_url")? != expected_release_url {
        return Err(GeneratorError::usage(
            "discovery release_url is not the canonical GitHub release URL",
        ));
    }
    let _ = field(document, "target_commitish")?;
    let _ = field(document, "published_at")?;
    let manifest = document
        .get("manifest")
        .cloned()
        .ok_or_else(|| GeneratorError::usage("discovery product manifest is missing"))?;
    validate_product_manifest_selection(&manifest, document)?;
    let assets = document
        .get("release_assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery release_assets is not an array"))?;
    if assets.is_empty() {
        return Err(GeneratorError::usage("discovery release_assets is empty"));
    }
    let mut seen_names = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    let mut release_assets = Vec::with_capacity(assets.len());
    for asset in assets {
        exact_object_keys(
            asset,
            &["browser_download_url", "id", "name", "size", "state"],
            "discovery release asset",
        )?;
        let id = positive_field(asset, "id")?;
        let name = field(asset, "name")?.to_owned();
        if !valid_discovery_asset_name(&name)
            || name == DISCOVERY_SELECTION_FILE
            || !seen_names.insert(name.clone())
            || !seen_ids.insert(id)
        {
            return Err(GeneratorError::usage(
                "discovery release asset names and IDs must be unique and safe",
            ));
        }
        let size = positive_field(asset, "size")?;
        let state = field(asset, "state")?.to_owned();
        if state != "uploaded" {
            return Err(GeneratorError::usage(
                "discovery release asset is not uploaded",
            ));
        }
        let expected_url =
            format!("https://github.com/{source_repository}/releases/download/{tag}/{name}");
        let browser_download_url = field(asset, "browser_download_url")?.to_owned();
        if browser_download_url != expected_url {
            return Err(GeneratorError::usage(
                "discovery release asset URL is not canonical",
            ));
        }
        release_assets.push(DiscoveryAsset {
            id,
            name,
            size,
            state,
            browser_download_url,
        });
    }
    let manifest_artifacts = manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery product artifacts are not an array"))?;
    for artifact in manifest_artifacts {
        let name = field(artifact, "name")?;
        if !seen_names.contains(name) {
            return Err(GeneratorError::usage(format!(
                "discovery product artifact is absent from release assets: {name}"
            )));
        }
    }
    for required in [
        PRODUCT_MANIFEST_ASSET,
        "product-manifest.json.sha256",
        RECORD_FILE,
        RECORD_SIDECAR,
        MANIFEST_FILE,
        MANIFEST_SIDECAR,
        PREVIEW_MANIFEST_FILE,
        SHA256SUMS_FILE,
    ] {
        if !seen_names.contains(required) {
            return Err(GeneratorError::usage(format!(
                "discovery release asset inventory lacks {required}"
            )));
        }
    }
    let mut allowed_names = manifest_artifacts
        .iter()
        .map(|artifact| field(artifact, "name").map(str::to_owned))
        .collect::<Result<BTreeSet<_>, _>>()?;
    for artifact in manifest_artifacts {
        if field(artifact, "kind")? == "apt-package" {
            allowed_names.insert(format!("{}.sha256", field(artifact, "name")?));
        }
    }
    allowed_names.extend([
        PRODUCT_MANIFEST_ASSET.to_owned(),
        "product-manifest.json.sha256".to_owned(),
        RECORD_FILE.to_owned(),
        RECORD_SIDECAR.to_owned(),
        MANIFEST_FILE.to_owned(),
        MANIFEST_SIDECAR.to_owned(),
        PREVIEW_MANIFEST_FILE.to_owned(),
        SHA256SUMS_FILE.to_owned(),
    ]);
    if seen_names != allowed_names {
        return Err(GeneratorError::usage(
            "discovery release asset census differs from the canonical manifest and allowed subordinate records",
        ));
    }
    Ok(DiscoverySelection {
        channel,
        product_id,
        source_repository,
        package,
        tag,
        release_tag,
        version,
        source_ref,
        source_commit,
        manifest_asset,
        manifest_schema,
        manifest_sha256,
        release_id,
        provider_release_id,
        release_url: expected_release_url,
        release_assets,
        manifest,
    })
}

/// Check a subordinate producer record against the externally hashed parent
/// manifest. The producer owns each subordinate schema; this boundary only
/// enforces the cross-record edge and its detached digest.
fn verify_subordinate_record_digest(
    incoming: &Path,
    payload_name: &str,
    sidecar_name: &str,
    expected_parent: &str,
) -> Result<(), GeneratorError> {
    let payload_path = incoming.join(payload_name);
    let payload_bytes = read_regular_file(&payload_path)?;
    let sidecar = sidecar_digest(&incoming.join(sidecar_name))?;
    if sidecar != sha256_hex(&payload_bytes) {
        return Err(GeneratorError::usage(format!(
            "discovery subordinate {payload_name} checksum differs from its sidecar"
        )));
    }
    let document =
        serde_json::from_slice::<serde_json::Value>(&payload_bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "discovery subordinate {payload_name} is not valid JSON: {error}"
            ))
        })?;
    if field(&document, "parent_manifest_sha256")? != expected_parent {
        return Err(GeneratorError::usage(format!(
            "discovery subordinate {payload_name} does not bind the canonical product manifest"
        )));
    }
    Ok(())
}

/// Check a package projection's parent release identity. Keeping this as a
/// release ID rather than embedding the product-manifest digest avoids a
/// digest cycle: the canonical manifest hashes the `.deb`, whose packaged
/// `manifest.json` must therefore not contain that digest.
fn verify_subordinate_record_id(
    incoming: &Path,
    payload_name: &str,
    sidecar_name: &str,
    expected_parent: &str,
) -> Result<(), GeneratorError> {
    let payload_path = incoming.join(payload_name);
    let payload_bytes = read_regular_file(&payload_path)?;
    let sidecar = sidecar_digest(&incoming.join(sidecar_name))?;
    if sidecar != sha256_hex(&payload_bytes) {
        return Err(GeneratorError::usage(format!(
            "discovery subordinate {payload_name} checksum differs from its sidecar"
        )));
    }
    let document =
        serde_json::from_slice::<serde_json::Value>(&payload_bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "discovery subordinate {payload_name} is not valid JSON: {error}"
            ))
        })?;
    if field(&document, "parent_manifest_id")? != expected_parent {
        return Err(GeneratorError::usage(format!(
            "discovery subordinate {payload_name} does not bind the canonical release ID"
        )));
    }
    Ok(())
}

/// Verify the release-owned subordinate byte edges which are not represented
/// as product artifact rows. This is deliberately a projection check: the
/// producer remains the authority for each record's full schema.
fn verify_discovery_subordinates(
    selection: &DiscoverySelection,
    incoming: &Path,
) -> Result<(), GeneratorError> {
    verify_subordinate_record_digest(
        incoming,
        RECORD_FILE,
        RECORD_SIDECAR,
        &selection.manifest_sha256,
    )?;
    verify_subordinate_record_id(
        incoming,
        MANIFEST_FILE,
        MANIFEST_SIDECAR,
        &selection.release_id,
    )?;
    let release_manifest = read_json(&incoming.join(PREVIEW_MANIFEST_FILE))?;
    if field(&release_manifest, "parent_manifest_sha256")? != selection.manifest_sha256 {
        return Err(GeneratorError::usage(
            "discovery release-manifest does not bind the canonical product manifest",
        ));
    }
    let sums_bytes = read_regular_file(&incoming.join(SHA256SUMS_FILE))?;
    let sums = String::from_utf8(sums_bytes)
        .map_err(|_| GeneratorError::usage("discovery SHA256SUMS is not UTF-8"))?;
    let mut sums_by_name = BTreeMap::new();
    for line in sums.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let digest = fields
            .next()
            .ok_or_else(|| GeneratorError::usage("discovery SHA256SUMS has no digest"))?;
        let name = fields
            .next()
            .ok_or_else(|| GeneratorError::usage("discovery SHA256SUMS has no asset name"))?;
        if fields.next().is_some() || !valid_digest(digest) || !valid_discovery_asset_name(name) {
            return Err(GeneratorError::usage(
                "discovery SHA256SUMS has an invalid row",
            ));
        }
        if sums_by_name
            .insert(name.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(GeneratorError::usage(
                "discovery SHA256SUMS names an asset more than once",
            ));
        }
    }
    let artifacts = selection
        .manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery product artifact inventory is missing"))?;
    let apt_artifacts = artifacts
        .iter()
        .filter(|artifact| field(artifact, "kind").ok() == Some("apt-package"))
        .collect::<Vec<_>>();
    if apt_artifacts.len() != 2 || sums_by_name.len() != apt_artifacts.len() {
        return Err(GeneratorError::usage(
            "discovery SHA256SUMS is not the exact two-package census",
        ));
    }
    for artifact in apt_artifacts {
        let name = field(artifact, "name")?;
        let expected = field(artifact, "sha256")?;
        if sums_by_name.get(name).map(String::as_str) != Some(expected) {
            return Err(GeneratorError::usage(format!(
                "discovery SHA256SUMS does not bind {name}"
            )));
        }
        let sidecar = sidecar_digest(&incoming.join(format!("{name}.sha256")))?;
        if sidecar != expected {
            return Err(GeneratorError::usage(format!(
                "discovery package sidecar does not bind {name}"
            )));
        }
    }
    Ok(())
}

/// Download precisely the assets named by a validated discovery result. The
/// old tag/pointer download selector is intentionally not reachable here:
/// every request is keyed by the immutable provider asset ID.
#[allow(clippy::too_many_lines)]
pub(crate) fn run_fetch_selection(
    selection_path: &Path,
    dir: &Path,
    path_overlay: Option<&Path>,
) -> Result<DiscoverySelection, GeneratorError> {
    let selected_document = read_json(selection_path)?;
    let selection = parse_discovery_selection(&selected_document)?;
    #[cfg(unix)]
    let directory = open_or_create_directory(dir)?;
    #[cfg(unix)]
    {
        let existing = dir_names_from_file(&directory, dir)?;
        if existing.iter().any(|name| name == SENTINEL_FILE) {
            return Err(GeneratorError::usage(
                "selection fetch refuses a pre-existing verification sentinel",
            ));
        }
        if existing.iter().any(|name| name != DISCOVERY_SELECTION_FILE) {
            return Err(GeneratorError::usage(
                "selection fetch requires an empty incoming directory",
            ));
        }
        if existing.iter().any(|name| name == DISCOVERY_SELECTION_FILE) {
            let persisted = read_regular_file_at(
                &directory,
                DISCOVERY_SELECTION_FILE,
                &dir.join(DISCOVERY_SELECTION_FILE),
            )?;
            let persisted = serde_json::from_slice::<serde_json::Value>(&persisted)
                .map_err(|_| GeneratorError::usage("incoming discovery.json is not valid JSON"))?;
            if persisted != selected_document {
                return Err(GeneratorError::usage(
                    "incoming discovery.json differs from the selected release",
                ));
            }
        }
    }
    #[cfg(not(unix))]
    {
        if dir.exists() {
            require_directory(dir)?;
            let existing = dir_names(dir)?;
            if existing.iter().any(|name| name == SENTINEL_FILE) {
                return Err(GeneratorError::usage(
                    "selection fetch refuses a pre-existing verification sentinel",
                ));
            }
            if existing.iter().any(|name| name != DISCOVERY_SELECTION_FILE) {
                return Err(GeneratorError::usage(
                    "selection fetch requires an empty incoming directory",
                ));
            }
            if dir.join(DISCOVERY_SELECTION_FILE).exists()
                && read_json(&dir.join(DISCOVERY_SELECTION_FILE))? != selected_document
            {
                return Err(GeneratorError::usage(
                    "incoming discovery.json differs from the selected release",
                ));
            }
        } else {
            std::fs::create_dir(dir)
                .map_err(|error| GeneratorError::io("create incoming directory", dir, &error))?;
        }
    }
    for asset in &selection.release_assets {
        let endpoint = format!(
            "repos/{}/releases/assets/{}",
            selection.source_repository, asset.id
        );
        let bytes = run_fixed(
            "gh",
            &[
                "api".to_owned(),
                "--header".to_owned(),
                "Accept: application/octet-stream".to_owned(),
                endpoint,
            ],
            None,
            path_overlay,
        )?;
        if bytes.len() as u64 != asset.size {
            return Err(GeneratorError::usage(format!(
                "downloaded asset {} size differs from discovery",
                asset.name
            )));
        }
        #[cfg(unix)]
        {
            let temporary = format!(".{}.part", asset.id);
            create_new_regular_file_at(
                &directory,
                &temporary,
                &bytes,
                "write downloaded asset",
                &dir.join(&temporary),
            )?;
            rename_at(&directory, &temporary, &asset.name, &dir.join(&asset.name))?;
        }
        #[cfg(not(unix))]
        {
            let temporary = dir.join(format!(".{}.part", asset.id));
            create_new_regular_file(&temporary, &bytes, "write downloaded asset")?;
            std::fs::rename(&temporary, dir.join(&asset.name)).map_err(|error| {
                GeneratorError::io("install downloaded asset", &dir.join(&asset.name), &error)
            })?;
        }
    }
    let persisted = serde_json::to_vec(&selected_document).map_err(|error| {
        GeneratorError::usage(format!("serialize discovery selection: {error}"))
    })?;
    #[cfg(unix)]
    if !dir_names_from_file(&directory, dir)?
        .iter()
        .any(|name| name == DISCOVERY_SELECTION_FILE)
    {
        create_new_regular_file_at(
            &directory,
            DISCOVERY_SELECTION_FILE,
            &persisted,
            "persist discovery selection",
            &dir.join(DISCOVERY_SELECTION_FILE),
        )?;
    }
    #[cfg(not(unix))]
    {
        let selection_destination = dir.join(DISCOVERY_SELECTION_FILE);
        if !selection_destination.exists() {
            create_new_regular_file(
                &selection_destination,
                &persisted,
                "persist discovery selection",
            )?;
        }
    }
    Ok(selection)
}

/// Verify that incoming bytes still represent the exact immutable selection.
/// This runs immediately before the existing APT verifier and again before
/// publication; mutation of IDs, names, sizes, canonical bytes, or selected
/// artifacts cannot cross either boundary.
pub(crate) fn verify_discovery_incoming(
    selection_path: &Path,
    incoming: &Path,
) -> Result<DiscoverySelection, GeneratorError> {
    require_directory(incoming)?;
    let selection_document = read_json(selection_path)?;
    let selection = parse_discovery_selection(&selection_document)?;
    let persisted = incoming.join(DISCOVERY_SELECTION_FILE);
    if read_json(&persisted)? != selection_document {
        return Err(GeneratorError::usage(
            "incoming discovery.json differs from the selected release",
        ));
    }
    if std::fs::symlink_metadata(incoming.join(SENTINEL_FILE)).is_ok() {
        check_sentinel(incoming)?;
    }
    let mut expected = selection
        .release_assets
        .iter()
        .map(|asset| asset.name.clone())
        .collect::<BTreeSet<_>>();
    expected.insert(DISCOVERY_SELECTION_FILE.to_owned());
    expected.insert(SENTINEL_FILE.to_owned());
    for name in dir_names(incoming)? {
        if !expected.contains(&name) {
            return Err(GeneratorError::usage(format!(
                "incoming contains an asset absent from discovery: {name}"
            )));
        }
    }
    for asset in &selection.release_assets {
        let path = incoming.join(&asset.name);
        let observed = read_regular_file(&path)?.len() as u64;
        if observed != asset.size {
            return Err(GeneratorError::usage(format!(
                "incoming asset {} size differs from discovery",
                asset.name
            )));
        }
    }
    let manifest_path = incoming.join(&selection.manifest_asset);
    let manifest_bytes = read_regular_file(&manifest_path)?;
    if sha256_hex(&manifest_bytes) != selection.manifest_sha256 {
        return Err(GeneratorError::usage(
            "incoming canonical product manifest digest differs from discovery",
        ));
    }
    let manifest_document =
        serde_json::from_slice::<serde_json::Value>(&manifest_bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "{} is not valid JSON: {error}",
                manifest_path.display()
            ))
        })?;
    if manifest_document != selection.manifest {
        return Err(GeneratorError::usage(
            "incoming canonical product manifest differs from discovery",
        ));
    }
    let manifest_sidecar = incoming.join(format!("{}.sha256", selection.manifest_asset));
    if product_manifest_sidecar_digest(&manifest_sidecar)? != selection.manifest_sha256 {
        return Err(GeneratorError::usage(
            "canonical product manifest sidecar differs from discovery",
        ));
    }
    let artifacts = selection
        .manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("discovery product artifact inventory is missing"))?;
    for artifact in artifacts {
        let name = field(artifact, "name")?;
        let path = incoming.join(name);
        let expected_size = positive_field(artifact, "size")?;
        let bytes = read_regular_file(&path)?;
        let observed_size = bytes.len() as u64;
        if observed_size != expected_size || sha256_hex(&bytes) != field(artifact, "sha256")? {
            return Err(GeneratorError::usage(format!(
                "incoming product artifact {name} differs from canonical inventory"
            )));
        }
    }
    verify_discovery_subordinates(&selection, incoming)?;
    Ok(selection)
}

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
    let (verified_root, verified_deb) = materialize_verified_file(deb)?;
    let result = (|| {
        let Some(deb_name) = verified_deb.to_str() else {
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
        #[cfg(unix)]
        let scratch_directory = open_directory_nofollow(&scratch).map_err(|error| {
            GeneratorError::io("open control extraction directory", &scratch, &error)
        })?;
        validate_tar_payload(&payload, path_overlay)?;
        #[cfg(unix)]
        let result =
            extract_tar_payload_in_directory(&payload, &scratch_directory, path_overlay, &scratch)
                .and_then(|()| {
                    let control_path = scratch.join("control");
                    let text = String::from_utf8(read_regular_file_at(
                        &scratch_directory,
                        "control",
                        &control_path,
                    )?)
                    .map_err(|_| {
                        GeneratorError::usage(format!("{} is not UTF-8", control_path.display()))
                    })?;
                    let prefix = format!("{field_name}:");
                    text.lines()
                        .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::to_owned))
                        .map(|value| value.trim().to_owned())
                        .ok_or_else(|| {
                            GeneratorError::usage(format!(
                                "deb {} has no {field_name} control field",
                                deb.display()
                            ))
                        })
                });
        #[cfg(not(unix))]
        let result = run_tar_stdin(
            &["-x", "-C", scratch.to_str().unwrap_or("."), "-f", "-"],
            &payload,
            path_overlay,
        )
        .and_then(|()| {
            let control_path = scratch.join("control");
            let text = String::from_utf8(read_regular_file(&control_path)?).map_err(|_| {
                GeneratorError::usage(format!("{} is not UTF-8", control_path.display()))
            })?;
            let prefix = format!("{field_name}:");
            text.lines()
                .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::trim))
                .map(str::to_owned)
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "deb {} has no {field_name} control field",
                        deb.display()
                    ))
                })
        });
        let _ = std::fs::remove_dir_all(&scratch);
        result
    })();
    let _ = std::fs::remove_dir_all(&verified_root);
    result
}

/// A unique scratch directory under the system temp dir.
fn scratch_dir(kind: &str) -> Result<PathBuf, GeneratorError> {
    let dir = std::env::temp_dir().join(format!("apt-feed-{kind}-{}", uuid::Uuid::new_v4()));
    #[cfg(unix)]
    {
        let parent = dir
            .parent()
            .ok_or_else(|| GeneratorError::usage("scratch directory has no parent"))?;
        let parent_file = open_directory_nofollow(parent)
            .map_err(|error| GeneratorError::io("open scratch parent", parent, &error))?;
        rustix::fs::mkdirat(
            &parent_file,
            dir.file_name()
                .ok_or_else(|| GeneratorError::usage("scratch directory has no name"))?,
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .map_err(|error| {
            GeneratorError::io(
                "create scratch directory",
                &dir,
                &std::io::Error::from(error),
            )
        })?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&dir)
        .map_err(|error| GeneratorError::io("create scratch directory", &dir, &error))?;
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

/// Decode a package archive stream before the descriptor-bound Rust extractor
/// consumes it. Compression tools only transform bytes; they never receive a
/// destination path or permission to create files.
fn tar_uncompressed_payload(
    payload: &[u8],
    path_overlay: Option<&Path>,
) -> Result<Vec<u8>, GeneratorError> {
    let Some(flag) = tar_decompress_flag(payload) else {
        return Ok(payload.to_owned());
    };
    let (program, args): (&str, &[&str]) = match flag {
        "-z" => ("gzip", &["-d", "-c"]),
        "-j" => ("bzip2", &["-d", "-c"]),
        "-J" => ("xz", &["-d", "-c"]),
        "--zstd" => ("zstd", &["-d", "-c"]),
        _ => return Err(GeneratorError::usage("unsupported archive compression")),
    };
    run_fixed(
        program,
        &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        Some(payload),
        path_overlay,
    )
}

#[cfg(unix)]
fn open_archive_child_directory(
    parent: &File,
    name: &str,
    display: &Path,
) -> Result<File, GeneratorError> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW;
    match rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty()) {
        Ok(file) => Ok(File::from(file)),
        Err(error) => {
            let error = std::io::Error::from(error);
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(GeneratorError::io(
                    "open archive directory",
                    display,
                    &error,
                ));
            }
            rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o755)).map_err(
                |error| {
                    GeneratorError::io(
                        "create archive directory",
                        display,
                        &std::io::Error::from(error),
                    )
                },
            )?;
            File::from(
                rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty()).map_err(
                    |error| {
                        GeneratorError::io(
                            "open archive directory",
                            display,
                            &std::io::Error::from(error),
                        )
                    },
                )?,
            )
            .metadata()
            .map_err(|error| GeneratorError::io("stat archive directory", display, &error))
            .and_then(|metadata| {
                if metadata.is_dir() {
                    Ok(())
                } else {
                    Err(GeneratorError::usage(format!(
                        "archive parent is not a directory: {}",
                        display.display()
                    )))
                }
            })
            .and_then(|()| {
                // The parent descriptor is the stable capability; reopen via
                // it, never through the mutable destination pathname.
                rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
                    .map(File::from)
                    .map_err(|error| {
                        GeneratorError::io(
                            "reopen archive directory",
                            display,
                            &std::io::Error::from(error),
                        )
                    })
            })
        }
    }
}

#[cfg(unix)]
fn archive_parent_directory(
    destination: &File,
    components: &[&str],
    display: &Path,
) -> Result<File, GeneratorError> {
    let mut parent = File::from(rustix::io::dup(destination.as_fd()).map_err(|error| {
        GeneratorError::io(
            "duplicate archive destination",
            display,
            &std::io::Error::from(error),
        )
    })?);
    for component in components {
        parent = open_archive_child_directory(&parent, component, display)?;
    }
    Ok(parent)
}

#[cfg(unix)]
fn archive_path_components<'a>(
    path: &'a Path,
    display: &Path,
) -> Result<Vec<&'a str>, GeneratorError> {
    let text = path
        .to_str()
        .ok_or_else(|| GeneratorError::usage("archive member path is not UTF-8"))?;
    let dot_root = text == "./";
    let text = text
        .strip_prefix("./")
        .unwrap_or(text)
        .trim_end_matches('/');
    if dot_root || text == "." {
        return Ok(Vec::new());
    }
    if text.is_empty() || text.starts_with('/') {
        return Err(GeneratorError::usage(format!(
            "archive member path is not confined: {text}"
        )));
    }
    let components = text
        .split('/')
        .filter(|component| *component != ".")
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "..")
    {
        return Err(GeneratorError::usage(format!(
            "archive member path is not confined: {text} ({})",
            display.display()
        )));
    }
    Ok(components)
}

/// Extract a validated tar stream relative to a held directory descriptor.
/// No archive member is ever handed to a pathname-based unpacker.
#[cfg(unix)]
fn extract_tar_payload_in_directory(
    payload: &[u8],
    destination: &File,
    path_overlay: Option<&Path>,
    display: &Path,
) -> Result<(), GeneratorError> {
    let payload = tar_uncompressed_payload(payload, path_overlay)?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(payload));
    let entries = archive.entries().map_err(|error| {
        GeneratorError::usage(format!("could not read archive entries: {error}"))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            GeneratorError::usage(format!("could not read archive entry: {error}"))
        })?;
        let path = entry
            .path()
            .map_err(|error| {
                GeneratorError::usage(format!("could not read archive path: {error}"))
            })?
            .into_owned();
        let components = archive_path_components(&path, display)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            let _ = archive_parent_directory(destination, &components, display)?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(GeneratorError::usage(
                "archive member type is not a regular file or directory",
            ));
        }
        let (name, parents) = components
            .split_last()
            .ok_or_else(|| GeneratorError::usage("archive member has no name"))?;
        let parent = archive_parent_directory(destination, parents, display)?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|error| {
            GeneratorError::usage(format!("could not read archive member: {error}"))
        })?;
        create_new_regular_file_at(&parent, name, &bytes, "extract archive member", display)?;
    }
    Ok(())
}

/// Run `tar` with fixed arguments and a piped archive payload.
fn run_tar_stdin_output(
    args: &[&str],
    payload: &[u8],
    path_overlay: Option<&Path>,
) -> Result<Vec<u8>, GeneratorError> {
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
    Ok(output.stdout)
}

#[cfg(not(unix))]
fn run_tar_stdin(
    args: &[&str],
    payload: &[u8],
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    run_tar_stdin_output(args, payload, path_overlay).map(|_| ())
}

/// Reject archive names and member types before any pathname-based extraction.
/// Only relative regular files and directories are accepted; symlinks,
/// hardlinks, device nodes, FIFOs, and traversal names are never needed by a
/// package identity tree and would make tar's destination semantics unsafe.
fn validate_tar_payload(payload: &[u8], path_overlay: Option<&Path>) -> Result<(), GeneratorError> {
    let names = run_tar_stdin_output(&["-t", "-f", "-"], payload, path_overlay)?;
    let names = String::from_utf8(names)
        .map_err(|_| GeneratorError::usage("archive member listing is not UTF-8"))?;
    for raw_name in names.lines() {
        let name = raw_name.strip_suffix('/').unwrap_or(raw_name);
        let name = name.strip_prefix("./").unwrap_or(name);
        if name.is_empty()
            || name.starts_with('/')
            || name
                .split('/')
                .any(|component| component.is_empty() || component == "..")
        {
            return Err(GeneratorError::usage(
                "archive member path is not confined to the extraction directory",
            ));
        }
    }
    let details = run_tar_stdin_output(&["-t", "-v", "-f", "-"], payload, path_overlay)?;
    let details = String::from_utf8(details)
        .map_err(|_| GeneratorError::usage("archive member metadata is not UTF-8"))?;
    for line in details.lines() {
        let kind = line.as_bytes().first().copied();
        if !matches!(kind, Some(b'd' | b'-')) {
            return Err(GeneratorError::usage(
                "archive member type is not a regular file or directory",
            ));
        }
    }
    Ok(())
}

/// Create a fresh extraction destination while rejecting a symlink or hard
/// link at every parent boundary. Unix creation is relative to an opened
/// parent descriptor, so a concurrent replacement cannot redirect mkdir.
#[allow(clippy::needless_return)]
fn prepare_extraction_destination(dest: &Path) -> Result<File, GeneratorError> {
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(GeneratorError::usage(
            "archive extraction destination already exists",
        ));
    }
    let parent = dest
        .parent()
        .ok_or_else(|| GeneratorError::usage("archive extraction destination has no parent"))?;
    let name = dest
        .file_name()
        .ok_or_else(|| GeneratorError::usage("archive extraction destination has no name"))?;
    #[cfg(unix)]
    {
        let parent_file = open_directory_nofollow(parent)
            .map_err(|error| GeneratorError::io("open extraction parent", parent, &error))?;
        rustix::fs::mkdirat(&parent_file, name, rustix::fs::Mode::from_raw_mode(0o700)).map_err(
            |error| {
                let error = std::io::Error::from(error);
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    GeneratorError::usage("archive extraction destination already exists")
                } else {
                    GeneratorError::io("create extraction destination", dest, &error)
                }
            },
        )?;
        let directory = File::from(
            rustix::fs::openat(
                &parent_file,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| {
                GeneratorError::io(
                    "open extraction destination",
                    dest,
                    &std::io::Error::from(error),
                )
            })?,
        );
        return Ok(directory);
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(dest)
            .map_err(|error| GeneratorError::io("create extraction destination", dest, &error))?;
        return File::open(dest)
            .map_err(|error| GeneratorError::io("open extraction destination", dest, &error));
    }
}

/// Extract a `.deb` data tree into `dest`.
pub(crate) fn deb_extract_data(
    deb: &Path,
    dest: &Path,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    let (verified_root, verified_deb) = materialize_verified_file(deb)?;
    let result = (|| {
        let (Some(deb_name), Some(dest_name)) = (verified_deb.to_str(), dest.to_str()) else {
            return Err(GeneratorError::usage("deb path is not UTF-8"));
        };
        let payload = if backend == DebBackend::Auto && tool_present("dpkg-deb", path_overlay) {
            run_fixed(
                "dpkg-deb",
                &["--fsys-tarfile".to_owned(), deb_name.to_owned()],
                None,
                path_overlay,
            )?
        } else {
            let members = run_fixed(
                "ar",
                &["t".to_owned(), deb_name.to_owned()],
                None,
                path_overlay,
            )?;
            let data = String::from_utf8(members)
                .map_err(|_| GeneratorError::usage("deb member listing is not UTF-8"))?
                .lines()
                .find(|line| line.starts_with("data.tar"))
                .ok_or_else(|| {
                    GeneratorError::usage(format!("deb {} has no data.tar member", deb.display()))
                })?
                .to_owned();
            run_fixed(
                "ar",
                &["p".to_owned(), deb_name.to_owned(), data],
                None,
                path_overlay,
            )?
        };
        validate_tar_payload(&payload, path_overlay)?;
        let destination = prepare_extraction_destination(dest)?;
        #[cfg(unix)]
        {
            let _ = dest_name;
            extract_tar_payload_in_directory(&payload, &destination, path_overlay, dest)
        }
        #[cfg(not(unix))]
        run_tar_stdin(&["-x", "-C", dest_name, "-f", "-"], &payload, path_overlay)
    })();
    let _ = std::fs::remove_dir_all(&verified_root);
    result
}

/// Inputs to suite verification. The producer-owned discovery selection is
/// mandatory and supplies source repository, package, version, and commit.
/// Keeping those identities in one typed value prevents a caller from mixing
/// a verified incoming selection with independently supplied release fields.
pub(crate) struct VerifyInputs<'a> {
    /// The suite under verification.
    pub(crate) suite: Suite,
    /// The immutable producer-owned application selection.
    pub(crate) selection: &'a DiscoverySelection,
    /// The daemon binary the extracted-identity check hashes.
    pub(crate) binary: String,
    /// The expected consumer release-manifest schema URN (preview suite).
    pub(crate) manifest_schema: String,
    /// The packaged-identity directory inside the deb.
    pub(crate) identity_dir: String,
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
    if inputs.selection.suite()? != inputs.suite {
        return Err(GeneratorError::usage(
            "verify selection channel does not match the requested suite",
        ));
    }
    if !valid_repository_slug(&inputs.selection.source_repository) {
        return Err(GeneratorError::usage(
            "verify needs an `owner/name` source repository",
        ));
    }
    if !valid_package_name(&inputs.selection.package) {
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
    arm_sentinel(inputs.incoming)?;
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
    let version = inputs.selection.apt_version()?;
    let tag = parse_stable_tag(&version)?;
    let commit = inputs.selection.source_commit.clone();
    let incoming = inputs.incoming;
    let record_path = incoming.join(RECORD_FILE);
    let record_sum_path = incoming.join(RECORD_SIDECAR);
    let manifest_path = incoming.join(MANIFEST_FILE);
    let manifest_sum_path = incoming.join(MANIFEST_SIDECAR);
    require_file(&record_path)?;
    require_file(&record_sum_path)?;
    require_file(&manifest_path)?;
    require_file(&manifest_sum_path)?;

    let deb_prefix = format!("{}-", inputs.selection.package);
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
        verify_stable_record(&record, &tag, &commit, &inputs.selection.source_repository)?;
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
        &inputs.selection.source_repository,
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
    Ok(())
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
            "record commit does not match the producer-selected source commit",
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
            "manifest source_sha != producer-selected source commit",
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
    let deb_name = format!("{}-{version}-{arch}.deb", inputs.selection.package);
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

/// Verify the preview suite: the producer-selected commit plus the
/// source-owned release manifest carry the coherence chain — there is no tag
/// and no release record here.
fn verify_preview(inputs: &VerifyInputs<'_>) -> Result<(), GeneratorError> {
    if inputs.verify_oci {
        return Err(GeneratorError::usage(
            "verify: --verify-oci does not apply to the preview suite (previews ship no OCI record)",
        ));
    }
    let commit = inputs.selection.source_commit.as_str();
    let parsed = parse_preview_version(&inputs.selection.apt_version()?)?;
    if parsed.sha != commit[..7] {
        return Err(GeneratorError::usage(format!(
            "preview version suffix {} does not match the source commit",
            parsed.sha
        )));
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
    let deb_prefix = format!("{}-", inputs.selection.package);
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
        let expected = format!("{}-preview-{dotted}-{arch}.deb", inputs.selection.package);
        require_file(&incoming.join(&expected))?;
    }

    let manifest = read_json(&manifest_path)?;
    if field(&manifest, "schema")? != inputs.manifest_schema {
        return Err(GeneratorError::usage("release-manifest schema mismatch"));
    }
    if field(&manifest, "source_repository")? != inputs.selection.source_repository {
        return Err(GeneratorError::usage(
            "release-manifest repository mismatch",
        ));
    }
    if field(&manifest, "source_ref")? != PREVIEW_SOURCE_REF {
        return Err(GeneratorError::usage(format!(
            "release-manifest source_ref is not {PREVIEW_SOURCE_REF}"
        )));
    }
    if field(&manifest, "source_commit")? != commit {
        return Err(GeneratorError::usage(
            "release-manifest source_commit does not match the producer-selected commit",
        ));
    }
    if field(&manifest, "version")? != parsed.version {
        return Err(GeneratorError::usage("release-manifest version mismatch"));
    }
    let assets = manifest
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("release-manifest assets are not an array"))?;
    if assets.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "release-manifest must list exactly two assets",
        ));
    }

    let sums_bytes = read_regular_file(&sums_path)?;
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
    let deb_name = format!("{}-preview-{dotted}-{arch}.deb", inputs.selection.package);
    let release_name = format!(
        "{}-preview-{}-{arch}.deb",
        inputs.selection.package, parsed.version
    );
    let deb = incoming.join(&deb_name);
    let deb_sum = incoming.join(format!("{deb_name}.sha256"));
    require_file(&deb_sum)?;
    let sidecar_bytes = read_regular_file(&deb_sum)?;
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
    if deb_control_field(&deb, "Package", inputs.backend, inputs.path_overlay)?
        != inputs.selection.package
    {
        return Err(GeneratorError::usage(format!(
            "{arch} preview deb Package is not {}",
            inputs.selection.package
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

fn verify_snapshot_subordinate(
    files: &BTreeMap<String, Vec<u8>>,
    payload_name: &str,
    sidecar_name: &str,
    expected_parent: &str,
    parent_field: &str,
) -> Result<(), GeneratorError> {
    let payload = files.get(payload_name).ok_or_else(|| {
        GeneratorError::usage(format!("publication incoming is missing {payload_name}"))
    })?;
    let sidecar = sidecar_digest_bytes(
        files.get(sidecar_name).ok_or_else(|| {
            GeneratorError::usage(format!("publication incoming is missing {sidecar_name}"))
        })?,
        Path::new(sidecar_name),
    )?;
    if sidecar != sha256_hex(payload) {
        return Err(GeneratorError::usage(format!(
            "publication subordinate {payload_name} checksum differs from its sidecar"
        )));
    }
    let document = serde_json::from_slice::<serde_json::Value>(payload).map_err(|error| {
        GeneratorError::usage(format!(
            "publication subordinate {payload_name} is not valid JSON: {error}"
        ))
    })?;
    if field(&document, parent_field)? != expected_parent {
        return Err(GeneratorError::usage(format!(
            "publication subordinate {payload_name} does not bind the canonical product manifest"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_snapshot_selection(
    selection: &DiscoverySelection,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), GeneratorError> {
    let manifest_bytes = files.get(&selection.manifest_asset).ok_or_else(|| {
        GeneratorError::usage("publication incoming is missing canonical manifest")
    })?;
    if sha256_hex(manifest_bytes) != selection.manifest_sha256 {
        return Err(GeneratorError::usage(
            "publication canonical product manifest digest differs from discovery",
        ));
    }
    let manifest =
        serde_json::from_slice::<serde_json::Value>(manifest_bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "publication canonical manifest is not valid JSON: {error}"
            ))
        })?;
    if manifest != selection.manifest {
        return Err(GeneratorError::usage(
            "publication canonical product manifest differs from discovery",
        ));
    }
    let manifest_sidecar = format!("{}.sha256", selection.manifest_asset);
    if product_manifest_sidecar_digest_bytes(
        files.get(&manifest_sidecar).ok_or_else(|| {
            GeneratorError::usage("publication incoming is missing manifest sidecar")
        })?,
        Path::new(&manifest_sidecar),
    )? != selection.manifest_sha256
    {
        return Err(GeneratorError::usage(
            "publication canonical manifest sidecar differs from discovery",
        ));
    }
    let artifacts = selection
        .manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GeneratorError::usage("publication product artifact inventory is missing")
        })?;
    for artifact in artifacts {
        let name = field(artifact, "name")?;
        let bytes = files.get(name).ok_or_else(|| {
            GeneratorError::usage(format!("publication incoming is missing artifact {name}"))
        })?;
        if bytes.len() as u64 != positive_field(artifact, "size")?
            || sha256_hex(bytes) != field(artifact, "sha256")?
        {
            return Err(GeneratorError::usage(format!(
                "publication product artifact {name} differs from canonical inventory"
            )));
        }
    }
    verify_snapshot_subordinate(
        files,
        RECORD_FILE,
        RECORD_SIDECAR,
        &selection.manifest_sha256,
        "parent_manifest_sha256",
    )?;
    verify_snapshot_subordinate(
        files,
        MANIFEST_FILE,
        MANIFEST_SIDECAR,
        &selection.release_id,
        "parent_manifest_id",
    )?;
    let release_manifest = files
        .get(PREVIEW_MANIFEST_FILE)
        .ok_or_else(|| GeneratorError::usage("publication incoming is missing release manifest"))?;
    let release_manifest =
        serde_json::from_slice::<serde_json::Value>(release_manifest).map_err(|error| {
            GeneratorError::usage(format!(
                "publication release manifest is not valid JSON: {error}"
            ))
        })?;
    if field(&release_manifest, "parent_manifest_sha256")? != selection.manifest_sha256 {
        return Err(GeneratorError::usage(
            "publication release manifest does not bind the canonical product manifest",
        ));
    }
    let sums = files
        .get(SHA256SUMS_FILE)
        .ok_or_else(|| GeneratorError::usage("publication incoming is missing SHA256SUMS"))?;
    let sums = String::from_utf8(sums.clone())
        .map_err(|_| GeneratorError::usage("publication SHA256SUMS is not UTF-8"))?;
    let mut sums_by_name = BTreeMap::new();
    for line in sums.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let digest = fields
            .next()
            .ok_or_else(|| GeneratorError::usage("publication SHA256SUMS has no digest"))?;
        let name = fields
            .next()
            .ok_or_else(|| GeneratorError::usage("publication SHA256SUMS has no asset name"))?;
        if fields.next().is_some() || !valid_digest(digest) || !valid_discovery_asset_name(name) {
            return Err(GeneratorError::usage(
                "publication SHA256SUMS has an invalid row",
            ));
        }
        if sums_by_name
            .insert(name.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(GeneratorError::usage(
                "publication SHA256SUMS names an asset more than once",
            ));
        }
    }
    let apt_artifacts = artifacts
        .iter()
        .filter(|artifact| field(artifact, "kind").ok() == Some("apt-package"))
        .collect::<Vec<_>>();
    if apt_artifacts.len() != 2 || sums_by_name.len() != apt_artifacts.len() {
        return Err(GeneratorError::usage(
            "publication SHA256SUMS is not the exact two-package census",
        ));
    }
    for artifact in apt_artifacts {
        let name = field(artifact, "name")?;
        let expected = field(artifact, "sha256")?;
        if sums_by_name.get(name).map(String::as_str) != Some(expected) {
            return Err(GeneratorError::usage(format!(
                "publication SHA256SUMS does not bind {name}"
            )));
        }
        let sidecar_name = format!("{name}.sha256");
        if sidecar_digest_bytes(
            files.get(&sidecar_name).ok_or_else(|| {
                GeneratorError::usage(format!("publication incoming is missing {sidecar_name}"))
            })?,
            Path::new(&sidecar_name),
        )? != expected
        {
            return Err(GeneratorError::usage(format!(
                "publication package sidecar does not bind {name}"
            )));
        }
    }
    Ok(())
}

/// A private publication snapshot. Every source byte consumed after the
/// verify-to-publish boundary is copied through one held incoming-directory
/// descriptor, then read from this owned map; later pathname replacement is
/// outside the publisher's capability set.
struct IncomingSnapshot {
    files: BTreeMap<String, Vec<u8>>,
    selection: DiscoverySelection,
}

impl IncomingSnapshot {
    fn capture(incoming: &Path) -> Result<Self, GeneratorError> {
        #[cfg(unix)]
        let directory = open_directory_nofollow(incoming)
            .map_err(|error| GeneratorError::io("open publication incoming", incoming, &error))?;
        #[cfg(unix)]
        let names = dir_names_from_file(&directory, incoming)?;
        #[cfg(not(unix))]
        let names = dir_names(incoming)?;
        let mut files = BTreeMap::new();
        for name in names {
            #[cfg(unix)]
            let bytes = read_regular_file_at(&directory, &name, &incoming.join(&name))?;
            #[cfg(not(unix))]
            let bytes = read_regular_file(&incoming.join(&name))?;
            if files.insert(name.clone(), bytes).is_some() {
                return Err(GeneratorError::usage(format!(
                    "publication incoming contains duplicate entry: {name}"
                )));
            }
        }
        let selection_bytes = files.get(DISCOVERY_SELECTION_FILE).ok_or_else(|| {
            GeneratorError::usage(
                "publication incoming has no producer-owned discovery selection; legacy handoff removed",
            )
        })?;
        let selection_document = serde_json::from_slice::<serde_json::Value>(selection_bytes)
            .map_err(|error| {
                GeneratorError::usage(format!(
                    "{} is not valid JSON: {error}",
                    incoming.join(DISCOVERY_SELECTION_FILE).display()
                ))
            })?;
        let selection = parse_discovery_selection(&selection_document)?;
        let sentinel = files.get(SENTINEL_FILE).ok_or_else(|| {
            GeneratorError::usage("publication incoming has no verification sentinel")
        })?;
        let expected = expected_sentinel_from_files(&files, &selection)?;
        if sentinel != &expected {
            return Err(GeneratorError::usage(
                "publication incoming sentinel does not bind its captured bytes",
            ));
        }
        verify_snapshot_selection(&selection, &files)?;
        let mut expected_names = selection
            .release_assets
            .iter()
            .map(|asset| asset.name.clone())
            .collect::<BTreeSet<_>>();
        expected_names.insert(DISCOVERY_SELECTION_FILE.to_owned());
        expected_names.insert(SENTINEL_FILE.to_owned());
        if files.keys().any(|name| !expected_names.contains(name)) {
            return Err(GeneratorError::usage(
                "publication incoming contains an asset absent from discovery",
            ));
        }
        for asset in &selection.release_assets {
            let bytes = files.get(&asset.name).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "publication incoming is missing selected asset {}",
                    asset.name
                ))
            })?;
            if bytes.len() as u64 != asset.size {
                return Err(GeneratorError::usage(format!(
                    "publication asset {} size differs from discovery",
                    asset.name
                )));
            }
        }
        Ok(Self { files, selection })
    }
}

fn expected_sentinel_from_files(
    files: &BTreeMap<String, Vec<u8>>,
    selection: &DiscoverySelection,
) -> Result<Vec<u8>, GeneratorError> {
    let selection_bytes = files
        .get(DISCOVERY_SELECTION_FILE)
        .ok_or_else(|| GeneratorError::usage("publication incoming has no discovery selection"))?;
    let mut proof = format!("selection:{}\n", sha256_hex(selection_bytes));
    for asset in &selection.release_assets {
        let bytes = files.get(&asset.name).ok_or_else(|| {
            GeneratorError::usage(format!("publication incoming is missing {}", asset.name))
        })?;
        proof.push_str("asset:");
        proof.push_str(&asset.name);
        proof.push(':');
        proof.push_str(&sha256_hex(bytes));
        proof.push('\n');
    }
    Ok(proof.into_bytes())
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
    /// The recovered rollback pair, absent only for preview bootstrap.
    pub(crate) prev_dir: Option<&'a Path>,
    /// The previous-pointer document, already derived by the typed
    /// `apt-previous-pointer` step.
    pub(crate) previous_pointer: &'a Path,
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
    /// The immutable application selection which produced these inputs.
    ///
    /// This is mandatory: a publication input without a typed producer
    /// selection would recreate the removed legacy direct-publisher route.
    pub(crate) selection: &'a DiscoverySelection,
    /// The source selection path. Schema-2 publication re-reads and compares
    /// it immediately before staging so a changed handoff cannot cross the
    /// verify-to-publish boundary.
    pub(crate) selection_path: &'a Path,
}

/// The complete publication control snapshot. Incoming release bytes,
/// pointer metadata, and retained rollback packages are all captured before
/// staging or signing. Publication never reopens these mutable pathnames
/// after this boundary.
struct PublicationSnapshot {
    files: BTreeMap<String, Vec<u8>>,
    selection: DiscoverySelection,
    previous_pointer: serde_json::Value,
    retained_debs: BTreeMap<String, Vec<u8>>,
}

impl PublicationSnapshot {
    fn capture(
        inputs: &PublishInputs<'_>,
        incoming: IncomingSnapshot,
    ) -> Result<Self, GeneratorError> {
        let previous_pointer = read_json(inputs.previous_pointer)?;
        let rollback_digests = rollback_package_digests(
            &previous_pointer,
            inputs.suite,
            inputs.bootstrap,
            &inputs.contract.package,
        )?;
        let retained_debs =
            capture_retained_debs(inputs.prev_dir, &inputs.contract.package, &rollback_digests)?;
        Ok(Self {
            files: incoming.files,
            selection: incoming.selection,
            previous_pointer,
            retained_debs,
        })
    }

    fn bytes(&self, name: &str) -> Result<&[u8], GeneratorError> {
        self.files
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| GeneratorError::usage(format!("publication incoming is missing {name}")))
    }
}

/// Capture retained package bytes through one held directory descriptor. A
/// package-looking symlink or hard link is rejected; unrelated entries retain
/// the existing behavior of being ignored by the rollback projection.
fn capture_retained_debs(
    dir: Option<&Path>,
    package: &str,
    expected: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Vec<u8>>, GeneratorError> {
    let Some(dir) = dir else {
        if !expected.is_empty() {
            return Err(GeneratorError::usage(
                "publish: rollback directory is required for the retained package identities",
            ));
        }
        return Ok(BTreeMap::new());
    };
    #[cfg(unix)]
    let directory = open_directory_nofollow(dir)
        .map_err(|error| GeneratorError::io("open rollback directory", dir, &error))?;
    #[cfg(unix)]
    let names = dir_names_from_file(&directory, dir)?;
    #[cfg(not(unix))]
    let names = dir_names(dir)?;
    let mut retained = BTreeMap::new();
    for name in names {
        if !name.starts_with(package) || !is_deb_file(&name) {
            continue;
        }
        let Some(expected_sha256) = expected.get(&name) else {
            return Err(GeneratorError::usage(format!(
                "rollback directory contains an unexpected package entry: {name}"
            )));
        };
        #[cfg(unix)]
        let bytes = read_regular_file_at(&directory, &name, &dir.join(&name))?;
        #[cfg(not(unix))]
        let bytes = read_regular_file(&dir.join(&name))?;
        if sha256_hex(&bytes).as_str() != expected_sha256 {
            return Err(GeneratorError::usage(format!(
                "rollback package bytes differ from the signed/live identity: {name}"
            )));
        }
        if retained.insert(name.clone(), bytes).is_some() {
            return Err(GeneratorError::usage(format!(
                "rollback directory contains duplicate package entry: {name}"
            )));
        }
    }
    if retained.len() != expected.len() || expected.keys().any(|name| !retained.contains_key(name))
    {
        return Err(GeneratorError::usage(
            "rollback directory is missing a package named by the signed/live identity",
        ));
    }
    Ok(retained)
}

/// Publish a suite into the staging tree: deterministic pool, per-arch
/// indexes, signed metadata, publication record. Every refusal below lands
/// before signing, and stable wipes only the validated staging directory.
#[allow(clippy::too_many_lines)]
pub(crate) fn publish_suite(inputs: &PublishInputs<'_>) -> Result<(), GeneratorError> {
    if inputs.bootstrap {
        if inputs.suite != Suite::Preview {
            return Err(GeneratorError::usage(
                "publish: --bootstrap applies only to --suite preview",
            ));
        }
        if inputs.prev_dir.is_some() {
            return Err(GeneratorError::usage(
                "publish: --bootstrap is mutually exclusive with --prev-dir",
            ));
        }
    }
    if inputs.suite == Suite::Preview && !inputs.bootstrap && inputs.prev_dir.is_none() {
        return Err(GeneratorError::usage(
            "publish: --prev-dir is required for the preview suite (the retained preview rollback pair; use --bootstrap to initialize the suite)",
        ));
    }
    let incoming = IncomingSnapshot::capture(inputs.incoming).map_err(|error| {
        GeneratorError::usage(format!(
            "publish: refusing — verify has not armed the reprepro sentinel: {error}"
        ))
    })?;
    let selection_path = inputs.selection_path;
    let expected_selection = inputs.selection;
    let current = read_discovery_selection(selection_path)?;
    if &current != expected_selection {
        return Err(GeneratorError::usage(
            "publish: discovery selection changed after verification",
        ));
    }
    if expected_selection != &incoming.selection {
        return Err(GeneratorError::usage(
            "publish: captured incoming selection differs from verification",
        ));
    }
    if incoming.selection.suite()? != inputs.suite {
        return Err(GeneratorError::usage(
            "publish: discovery selection channel does not match suite",
        ));
    }
    if incoming.selection.package != inputs.contract.package {
        return Err(GeneratorError::usage(
            "publish: discovery selection package does not match the APT contract",
        ));
    }
    if incoming.selection.apt_version()? != inputs.version {
        return Err(GeneratorError::usage(
            "publish: discovery selection version does not match the requested APT version",
        ));
    }
    for tool in ["apt-ftparchive", "gpg"] {
        if !tool_present(tool, inputs.path_overlay) {
            return Err(GeneratorError::usage(format!(
                "publish: {tool} not installed"
            )));
        }
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
    let Some(staging_name) = inputs.staging.to_str() else {
        return Err(GeneratorError::usage("staging directory is not UTF-8"));
    };
    if !valid_staging_dir(staging_name) {
        return Err(GeneratorError::usage(
            "staging directory must be a relative path without traversal",
        ));
    }
    // Capture the previous pointer and retained rollback bytes after cheap
    // secret/configuration refusals, but before any key import, staging, or
    // signing mutation. All later publication reads use this snapshot.
    let snapshot = PublicationSnapshot::capture(inputs, incoming)?;
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
        Suite::Stable => publish_stable(inputs, passphrase, homedir_name, &snapshot),
        Suite::Preview => publish_preview(inputs, passphrase, homedir_name, &snapshot),
    };
    // The isolated keyring leaves with the run, success or failure.
    teardown_signing_homedir(&homedir, inputs.path_overlay);
    outcome
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
fn selection_artifact_digest(
    selection: &DiscoverySelection,
    name: &str,
) -> Result<String, GeneratorError> {
    let artifacts = selection
        .manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("publish: product artifact inventory is missing"))?;
    let mut digest = None;
    for artifact in artifacts {
        if field(artifact, "name")? != name {
            continue;
        }
        if digest.is_some() {
            return Err(GeneratorError::usage(format!(
                "publish: product artifact {name} is duplicated"
            )));
        }
        let value = field(artifact, "sha256")?;
        if !valid_digest(value) {
            return Err(GeneratorError::usage(format!(
                "publish: product artifact {name} has an invalid digest"
            )));
        }
        digest = Some(value.to_owned());
    }
    digest.ok_or_else(|| {
        GeneratorError::usage(format!(
            "publish: candidate package {name} is absent from immutable discovery"
        ))
    })
}

#[allow(clippy::too_many_arguments)]
fn stage_package(
    deb: &Path,
    destination: &Path,
    contract: &AptContract,
    backend: DebBackend,
    path_overlay: Option<&Path>,
    expected_sha256: Option<&str>,
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
    let deb_bytes = read_regular_file(deb)?;
    if let Some(expected_sha256) = expected_sha256
        && sha256_hex(&deb_bytes) != expected_sha256
    {
        return Err(GeneratorError::usage(
            "publish: staged package differs from immutable discovery",
        ));
    }
    if std::fs::symlink_metadata(&expected).is_ok() {
        if sha256_hex(&deb_bytes) != sha256_file(&expected)? {
            return Err(GeneratorError::usage(
                "publish: canonical package identity collides with different bytes",
            ));
        }
    } else {
        if let Some(parent) = expected.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("create", parent, &error))?;
        }
        install_regular_file(&expected, &deb_bytes, "stage")?;
    }
    Ok((version, arch))
}

#[allow(clippy::too_many_arguments)]
fn stage_package_bytes(
    deb_bytes: &[u8],
    destination: &Path,
    contract: &AptContract,
    backend: DebBackend,
    path_overlay: Option<&Path>,
    expected_sha256: Option<&str>,
) -> Result<(String, String), GeneratorError> {
    let (scratch, deb) = materialize_verified_bytes(deb_bytes)?;
    let result = stage_package(
        &deb,
        destination,
        contract,
        backend,
        path_overlay,
        expected_sha256,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// Stage every retained rollback `.deb` captured before publication, skipping
/// names the candidate already carries (after a byte-equality check).
fn stage_dir_debs(
    retained_debs: &BTreeMap<String, Vec<u8>>,
    pool: &Path,
    snapshot: &PublicationSnapshot,
    contract: &AptContract,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    for (name, deb_bytes) in retained_debs {
        if let Some(candidate) = snapshot.files.get(name) {
            if sha256_hex(candidate) != sha256_hex(deb_bytes) {
                return Err(GeneratorError::usage(format!(
                    "published package name collides with different candidate bytes: {name}"
                )));
            }
            continue;
        }
        stage_package_bytes(
            deb_bytes,
            &pool.join(name),
            contract,
            backend,
            path_overlay,
            None,
        )?;
    }
    Ok(())
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

/// One package identity read from a live `Packages` index.  The index is the
/// authority for the rollback bytes only after its enclosing signed Release
/// metadata has been verified by the caller; this parser binds the exact
/// package path and digest once that boundary is crossed.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LivePackageEntry {
    version: String,
    name: String,
    sha256: String,
}

fn parse_live_package_stanza(
    fields: &BTreeMap<String, String>,
    package: &str,
    arch: &str,
    suite: Suite,
) -> Result<Option<LivePackageEntry>, GeneratorError> {
    if fields.get("Package").map(String::as_str) != Some(package) {
        return Ok(None);
    }
    let value = |name: &str| {
        fields
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| GeneratorError::usage(format!("live Packages stanza lacks {name}")))
    };
    if value("Architecture")? != arch {
        return Err(GeneratorError::usage(
            "live Packages package architecture differs from its index",
        ));
    }
    let version = value("Version")?.to_owned();
    if !valid_pool_version(&version) {
        return Err(GeneratorError::usage(
            "live Packages package version is unsafe",
        ));
    }
    let name = canonical_pool_name(package, &version, arch);
    let filename = value("Filename")?;
    let prefix = match suite {
        Suite::Stable => "pool/main",
        Suite::Preview => "pool/preview/main",
    };
    let expected_filename = format!("{prefix}/{}/{package}/{name}", pool_letter(package));
    if filename != expected_filename {
        return Err(GeneratorError::usage(
            "live Packages package filename is not the canonical pool identity",
        ));
    }
    let sha256 = value("SHA256")?.to_owned();
    if !valid_digest(&sha256) {
        return Err(GeneratorError::usage(
            "live Packages package SHA256 is not a lowercase digest",
        ));
    }
    Ok(Some(LivePackageEntry {
        version,
        name,
        sha256,
    }))
}

/// Parse the package rows for one architecture from a live `Packages` file.
/// Only the selected package is projected; every selected stanza must carry
/// an exact canonical pool filename and SHA256.  A path-bearing filename or
/// a duplicate field cannot silently become a rollback identity.
fn parse_live_packages(
    text: &str,
    package: &str,
    arch: &str,
    suite: Suite,
) -> Result<Vec<LivePackageEntry>, GeneratorError> {
    if !valid_package_name(package) || !REQUIRED_ARCHES.contains(&arch) {
        return Err(GeneratorError::usage(
            "live Packages selection has an unsafe package or architecture",
        ));
    }
    let mut rows = Vec::new();
    let mut fields = BTreeMap::new();
    let flush = |fields: &mut BTreeMap<String, String>,
                 rows: &mut Vec<LivePackageEntry>|
     -> Result<(), GeneratorError> {
        if fields.is_empty() {
            return Ok(());
        }
        if let Some(row) = parse_live_package_stanza(fields, package, arch, suite)? {
            rows.push(row);
        }
        fields.clear();
        Ok(())
    };
    for line in text.lines() {
        if line.is_empty() {
            flush(&mut fields, &mut rows)?;
            continue;
        }
        // Description continuations are not identity fields.  They are
        // accepted only as continuations, never as new field names.
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| GeneratorError::usage("live Packages contains a malformed field"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || fields
                .insert(name.to_owned(), value.trim().to_owned())
                .is_some()
        {
            return Err(GeneratorError::usage(
                "live Packages contains a duplicate or unsafe field",
            ));
        }
    }
    flush(&mut fields, &mut rows)?;
    Ok(rows)
}

fn live_rollback_entry(
    text: &str,
    package: &str,
    arch: &str,
    suite: Suite,
    candidate: &str,
    expected_rollback: Option<&str>,
) -> Result<LivePackageEntry, GeneratorError> {
    if !valid_pool_version(candidate) {
        return Err(GeneratorError::usage(
            "live Packages candidate version is unsafe",
        ));
    }
    let rows = parse_live_packages(text, package, arch, suite)?;
    let mut by_version = BTreeMap::new();
    for row in rows {
        if by_version.insert(row.version.clone(), row).is_some() {
            return Err(GeneratorError::usage(
                "live Packages contains duplicate package versions",
            ));
        }
    }
    if by_version.len() != IMPLEMENTED_RETENTION as usize + 1 {
        return Err(GeneratorError::usage(
            "live Packages must contain exactly the candidate and one rollback version",
        ));
    }
    if !by_version.contains_key(candidate) {
        return Err(GeneratorError::usage(
            "live Packages does not contain the immutable candidate version",
        ));
    }
    let row = if let Some(expected) = expected_rollback {
        by_version.remove(expected).ok_or_else(|| {
            GeneratorError::usage(
                "live Packages rollback version differs from the immutable prior tag",
            )
        })?
    } else {
        let mut rollback = by_version
            .into_iter()
            .filter(|(version, _)| version != candidate)
            .map(|(_, row)| row);
        let row = rollback.next().ok_or_else(|| {
            GeneratorError::usage("live Packages has no retained rollback version")
        })?;
        if rollback.next().is_some() {
            return Err(GeneratorError::usage(
                "live Packages contains more than one rollback version",
            ));
        }
        row
    };
    Ok(row)
}

/// Replace the rollback identities in a typed previous pointer with the rows
/// from both verified live indexes. Stable pointers retain their source-record
/// digest; preview pointers must already be the object form and cannot use the
/// removed string-only legacy representation.
pub(crate) fn bind_live_rollback_packages(
    pointer: &serde_json::Value,
    suite: Suite,
    package: &str,
    candidate: &str,
    expected_rollback: Option<&str>,
    amd64_packages: &str,
    arm64_packages: &str,
) -> Result<serde_json::Value, GeneratorError> {
    let amd64 = live_rollback_entry(
        amd64_packages,
        package,
        "amd64",
        suite,
        candidate,
        expected_rollback,
    )?;
    let arm64 = live_rollback_entry(
        arm64_packages,
        package,
        "arm64",
        suite,
        candidate,
        expected_rollback,
    )?;
    if amd64.version != arm64.version {
        return Err(GeneratorError::usage(
            "live Packages rollback versions differ by architecture",
        ));
    }
    let rollback_packages = serde_json::json!([
        {"name": amd64.name, "sha256": amd64.sha256},
        {"name": arm64.name, "sha256": arm64.sha256},
    ]);
    match suite {
        Suite::Stable => {
            validate_rollback_pointer_shape(pointer)?;
            let expected = expected_rollback.ok_or_else(|| {
                GeneratorError::usage(
                    "stable live rollback binding requires the immutable prior version",
                )
            })?;
            if field(pointer, "tag")? != format!("v{expected}") {
                return Err(GeneratorError::usage(
                    "live Packages rollback tag differs from the immutable prior tag",
                ));
            }
        }
        Suite::Preview => {
            exact_object_keys(
                pointer,
                &["tag", ROLLBACK_PACKAGES_FIELD],
                "preview live rollback pointer",
            )?;
            if field(pointer, "tag")? != PREVIEW_TAG {
                return Err(GeneratorError::usage(
                    "preview live rollback pointer has an invalid tag",
                ));
            }
            if !pointer
                .get(ROLLBACK_PACKAGES_FIELD)
                .is_some_and(serde_json::Value::is_array)
            {
                return Err(GeneratorError::usage(
                    "preview live rollback pointer has no package array",
                ));
            }
        }
    }
    let mut bound = pointer.clone();
    bound[ROLLBACK_PACKAGES_FIELD] = rollback_packages;
    Ok(bound)
}

/// The signed live publication plus the exact index bytes it authenticated.
/// The bytes stay in this value so selection cannot re-open mutable index
/// pathnames after the signature/digest boundary.
pub(crate) struct VerifiedLivePublication {
    pub(crate) document: serde_json::Value,
    pub(crate) amd64_packages: String,
    pub(crate) arm64_packages: String,
}

/// Verify the signed live publication record, its clearsigned `InRelease`,
/// and both index digests before any package row can become a rollback
/// identity. The caller still projects package rows through
/// `bind_live_rollback_packages`; this function establishes the external
/// signature/digest authority for those rows.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "live publication verification keeps signatures, digests, and immutable bytes together"
)]
pub(crate) fn read_verified_live_publication(
    suite: Suite,
    publication: &Path,
    publication_signature: &Path,
    inrelease: &Path,
    amd64_packages: &Path,
    arm64_packages: &Path,
    keyring: &Path,
    expected_signer: &str,
    path_overlay: Option<&Path>,
) -> Result<VerifiedLivePublication, GeneratorError> {
    let expected_signer = normalize_fingerprint(expected_signer);
    if !is_full_fingerprint(&expected_signer) {
        return Err(GeneratorError::usage(
            "live publication expected signer is not a full fingerprint",
        ));
    }
    let keyring_name = keyring
        .to_str()
        .ok_or_else(|| GeneratorError::usage("live publication keyring is not UTF-8"))?;
    if !valid_keyring_path(keyring_name) {
        return Err(GeneratorError::usage(
            "live publication keyring path is unsafe",
        ));
    }
    let keyring_bytes = read_regular_file(keyring)?;
    let publication_bytes = read_regular_file(publication)?;
    let signature_bytes = read_regular_file(publication_signature)?;
    let inrelease_bytes = read_regular_file(inrelease)?;
    let amd64_bytes = read_regular_file(amd64_packages)?;
    let arm64_bytes = read_regular_file(arm64_packages)?;
    let mut scratch_dirs = Vec::new();
    let result = (|| {
        let (scratch, keyring_path) = materialize_verified_bytes(&keyring_bytes)?;
        scratch_dirs.push(scratch);
        let (scratch, publication_path) = materialize_verified_bytes(&publication_bytes)?;
        scratch_dirs.push(scratch);
        let (scratch, signature_path) = materialize_verified_bytes(&signature_bytes)?;
        scratch_dirs.push(scratch);
        let (scratch, inrelease_path) = materialize_verified_bytes(&inrelease_bytes)?;
        scratch_dirs.push(scratch);
        let keyring_name = keyring_path
            .to_str()
            .ok_or_else(|| GeneratorError::usage("materialized keyring path is not UTF-8"))?;
        let publication_name = publication_path
            .to_str()
            .ok_or_else(|| GeneratorError::usage("materialized publication path is not UTF-8"))?;
        let signature_name = signature_path.to_str().ok_or_else(|| {
            GeneratorError::usage("materialized publication signature path is not UTF-8")
        })?;
        let inrelease_name = inrelease_path
            .to_str()
            .ok_or_else(|| GeneratorError::usage("materialized InRelease path is not UTF-8"))?;
        let signature_status = run_fixed(
            "gpgv",
            &[
                "--status-fd".to_owned(),
                "1".to_owned(),
                "--no-default-keyring".to_owned(),
                "--keyring".to_owned(),
                keyring_name.to_owned(),
                signature_name.to_owned(),
                publication_name.to_owned(),
            ],
            None,
            path_overlay,
        )?;
        gpgv_signer(
            &signature_status,
            &expected_signer,
            "live publication record",
        )?;
        let inrelease_status = run_fixed(
            "gpgv",
            &[
                "--status-fd".to_owned(),
                "1".to_owned(),
                "--no-default-keyring".to_owned(),
                "--keyring".to_owned(),
                keyring_name.to_owned(),
                inrelease_name.to_owned(),
            ],
            None,
            path_overlay,
        )?;
        gpgv_signer(&inrelease_status, &expected_signer, "live InRelease")?;
        let document =
            serde_json::from_slice::<serde_json::Value>(&publication_bytes).map_err(|error| {
                GeneratorError::usage(format!("live publication is not valid JSON: {error}"))
            })?;
        let record = parse_publication_record(&document)?;
        if !fingerprints_match(&record.signer_fingerprint, &expected_signer) {
            return Err(GeneratorError::usage(
                "live publication record signer does not match the pinned publisher key",
            ));
        }
        match suite {
            Suite::Stable => {
                if record.suite.is_some()
                    || parse_stable_tag(&record.tag)?.version != record.crate_version
                {
                    return Err(GeneratorError::usage(
                        "live stable publication identity is inconsistent",
                    ));
                }
            }
            Suite::Preview => {
                if record.suite.as_deref() != Some(PREVIEW_SUITE)
                    || record.tag != PREVIEW_TAG
                    || parse_preview_version(&record.crate_version).is_err()
                {
                    return Err(GeneratorError::usage(
                        "live preview publication identity is inconsistent",
                    ));
                }
            }
        }
        if sha256_hex(&inrelease_bytes) != record.inrelease_sha256 {
            return Err(GeneratorError::usage(
                "live InRelease bytes differ from the signed publication record",
            ));
        }
        let mut index_text = BTreeMap::new();
        for (arch, bytes) in [("amd64", &amd64_bytes), ("arm64", &arm64_bytes)] {
            let expected = record
                .packages
                .iter()
                .find(|entry| entry.arch == arch)
                .map(|entry| entry.sha256.as_str())
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "live publication record has no {arch} Packages digest"
                    ))
                })?;
            if sha256_hex(bytes) != expected {
                return Err(GeneratorError::usage(format!(
                    "live {arch} Packages bytes differ from the signed publication record"
                )));
            }
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| GeneratorError::usage(format!("live {arch} Packages is not UTF-8")))?;
            index_text.insert(arch, text);
        }
        Ok(VerifiedLivePublication {
            document,
            amd64_packages: index_text
                .remove("amd64")
                .ok_or_else(|| GeneratorError::usage("live amd64 Packages text is missing"))?,
            arm64_packages: index_text
                .remove("arm64")
                .ok_or_else(|| GeneratorError::usage("live arm64 Packages text is missing"))?,
        })
    })();
    for scratch in scratch_dirs {
        let _ = std::fs::remove_dir_all(scratch);
    }
    result
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
    let bytes = match std::fs::symlink_metadata(keyring) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(GeneratorError::io("stat keyring", keyring, &error)),
        Ok(_) => read_regular_file(keyring)?,
    };
    let name = keyring
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GeneratorError::usage("keyring filename is not UTF-8"))?;
    install_regular_file(&staging.join(name), &bytes, "stage the keyring")?;
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
        let mut rest: Vec<&str> = versions
            .iter()
            .map(String::as_str)
            .filter(|version| *version != candidate)
            .collect();
        rest.sort_unstable();
        let Some(arch_rollback) = rest.last().copied() else {
            return Err(GeneratorError::usage(format!(
                "publish: {arch} rollback version is empty"
            )));
        };
        if arch_rollback.is_empty() {
            return Err(GeneratorError::usage(format!(
                "publish: {arch} rollback version is empty"
            )));
        }
        match &rollback {
            None => rollback = Some(arch_rollback.to_owned()),
            Some(first) if first == arch_rollback => {}
            _ => {
                return Err(GeneratorError::usage(
                    "publish: architecture rollback versions differ",
                ));
            }
        }
        let gz = run_fixed(
            "gzip",
            &[
                "-n".to_owned(),
                "-9".to_owned(),
                "-c".to_owned(),
                packages
                    .to_str()
                    .ok_or_else(|| GeneratorError::usage("Packages path is not UTF-8"))?
                    .to_owned(),
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

/// Publish the stable suite: wipe and rebuild the staging tree with the
/// deterministic candidate-plus-rollback pool, then sign and record.
fn publish_stable(
    inputs: &PublishInputs<'_>,
    passphrase: &str,
    homedir: &str,
    snapshot: &PublicationSnapshot,
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    let tag = parse_stable_tag(&inputs.version)?;
    // Malformed pointers are rejected before any mutation or signing: only
    // the tag-agreement half waits for the retained rollback, which the
    // strict index build below computes.
    check_stable_pointer_shape(&snapshot.previous_pointer, &contract.package)?;
    if inputs.staging.exists() {
        std::fs::remove_dir_all(inputs.staging)
            .map_err(|error| GeneratorError::io("wipe", inputs.staging, &error))?;
    }
    let pool = pool_root(inputs.staging, Suite::Stable, contract);
    std::fs::create_dir_all(inputs.staging.join("conf"))
        .map_err(|error| GeneratorError::io("create", inputs.staging, &error))?;
    let distributions = format!(
        "Origin: {0}\nLabel: {0}\nCodename: stable\nArchitectures: amd64 arm64\nComponents: main\nDescription: {1}\nSignWith: {2}\n",
        contract.origin, contract.description, contract.signer
    );
    std::fs::write(inputs.staging.join("conf/distributions"), distributions)
        .map_err(|error| GeneratorError::io("write", inputs.staging, &error))?;
    stage_keyring(inputs.staging, contract)?;
    if inputs.prev_dir.is_some() {
        stage_dir_debs(
            &snapshot.retained_debs,
            &pool,
            snapshot,
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
    }
    // Stage the candidate debs by exact name: verification already proved
    // the incoming directory holds exactly this pair.
    for arch in REQUIRED_ARCHES {
        let name = format!("{}-{}-{arch}.deb", contract.package, tag.version);
        if snapshot.files.contains_key(&name) {
            let expected_sha256 = selection_artifact_digest(&snapshot.selection, &name)?;
            stage_package_bytes(
                snapshot.bytes(&name)?,
                &pool.join(&name),
                contract,
                inputs.backend,
                inputs.path_overlay,
                Some(&expected_sha256),
            )?;
        }
    }
    if pool_deb_count(&pool)? != contract.retention.pool_debs() {
        return Err(GeneratorError::usage(
            "publish: deterministic pool must contain exactly four package files",
        ));
    }
    let rollback = build_strict_indexes(
        inputs.staging,
        Suite::Stable,
        contract,
        &tag.version,
        contract.retention,
        inputs.path_overlay,
    )?;
    check_stable_pointer(
        &snapshot.previous_pointer,
        &format!("v{rollback}"),
        &contract.package,
    )?;
    prime_signer_agent(&contract.signer, homedir, passphrase, inputs.path_overlay)?;
    sign_suite_release(
        inputs.staging,
        Suite::Stable,
        contract,
        passphrase,
        homedir,
        inputs.path_overlay,
    )?;
    let source_record = sidecar_digest_bytes(
        snapshot.bytes(RECORD_SIDECAR)?,
        &inputs.incoming.join(RECORD_SIDECAR),
    )?;
    emit_publication_record(
        inputs.staging,
        Suite::Stable,
        contract,
        &tag.tag,
        &tag.version,
        &source_record,
        &snapshot.previous_pointer,
        inputs.path_overlay,
        passphrase,
        homedir,
        &snapshot.selection,
    )?;
    std::fs::write(
        inputs.staging.join("last-publish"),
        format!("{}\n", tag.tag),
    )
    .map_err(|error| GeneratorError::io("write", inputs.staging, &error))?;
    Ok(())
}

/// Publish the preview suite into the shared tree without wiping: only the
/// preview pool, indexes, and metadata are written.
fn publish_preview(
    inputs: &PublishInputs<'_>,
    passphrase: &str,
    homedir: &str,
    snapshot: &PublicationSnapshot,
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
    check_preview_pointer(
        &snapshot.previous_pointer,
        inputs.bootstrap,
        &contract.package,
    )?;
    let pool = pool_root(inputs.staging, Suite::Preview, contract);
    std::fs::create_dir_all(inputs.staging.join("conf"))
        .map_err(|error| GeneratorError::io("create", inputs.staging, &error))?;
    ensure_preview_stanza(inputs.staging, contract)?;
    stage_keyring(inputs.staging, contract)?;
    if inputs.bootstrap {
        stage_preview_candidates(inputs, &parsed, &pool, snapshot)?;
        check_bootstrap_pool(&pool, contract, &parsed.version)?;
        build_bootstrap_indexes(
            inputs.staging,
            contract,
            &parsed.version,
            inputs.path_overlay,
        )?;
    } else {
        if inputs.prev_dir.is_none() {
            return Err(GeneratorError::usage("publish: --prev-dir is required"));
        }
        stage_dir_debs(
            &snapshot.retained_debs,
            &pool,
            snapshot,
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
        stage_preview_candidates(inputs, &parsed, &pool, snapshot)?;
        if pool_deb_count(&pool)? != contract.retention.pool_debs() {
            return Err(GeneratorError::usage(
                "publish: deterministic preview pool must contain exactly four package files",
            ));
        }
        let rollback = build_strict_indexes(
            inputs.staging,
            Suite::Preview,
            contract,
            &parsed.version,
            contract.retention,
            inputs.path_overlay,
        )?;
        if cmp_preview_versions(&parsed.version, &rollback)? != std::cmp::Ordering::Greater {
            return Err(GeneratorError::usage(format!(
                "publish: preview candidate {} is not newer than the retained rollback {rollback}",
                parsed.version
            )));
        }
    }
    prime_signer_agent(&contract.signer, homedir, passphrase, inputs.path_overlay)?;
    sign_suite_release(
        inputs.staging,
        Suite::Preview,
        contract,
        passphrase,
        homedir,
        inputs.path_overlay,
    )?;
    let source_manifest = sha256_hex(snapshot.bytes(PREVIEW_MANIFEST_FILE)?);
    emit_publication_record(
        inputs.staging,
        Suite::Preview,
        contract,
        PREVIEW_TAG,
        &parsed.version,
        &source_manifest,
        &snapshot.previous_pointer,
        inputs.path_overlay,
        passphrase,
        homedir,
        &snapshot.selection,
    )?;
    std::fs::write(
        inputs.staging.join(Suite::Preview.last_publish_file()),
        format!("{}\n", parsed.version),
    )
    .map_err(|error| GeneratorError::io("write", inputs.staging, &error))?;
    Ok(())
}

/// Stage the preview candidate pair after checking each deb carries the
/// candidate control version.
fn stage_preview_candidates(
    inputs: &PublishInputs<'_>,
    parsed: &PreviewVersion,
    pool: &Path,
    snapshot: &PublicationSnapshot,
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    for arch in REQUIRED_ARCHES {
        let dotted = dotted_asset_version(&parsed.version);
        let name = format!("{}-preview-{dotted}-{arch}.deb", contract.package);
        let deb = snapshot.bytes(&name)?;
        let (scratch, deb_path) = materialize_verified_bytes(deb)?;
        let version_result =
            deb_control_field(&deb_path, "Version", inputs.backend, inputs.path_overlay);
        let _ = std::fs::remove_dir_all(&scratch);
        if version_result? != parsed.version {
            return Err(GeneratorError::usage(format!(
                "publish: candidate deb Version != preview candidate version {}",
                parsed.version
            )));
        }
        let expected_sha256 = selection_artifact_digest(&snapshot.selection, &name)?;
        stage_package_bytes(
            deb,
            &pool.join(&name),
            contract,
            inputs.backend,
            inputs.path_overlay,
            Some(&expected_sha256),
        )?;
    }
    Ok(())
}

/// Check a bootstrap pool holds exactly the freshly staged candidate pair:
/// bootstrap refuses to run over an existing pool.
fn check_bootstrap_pool(
    pool: &Path,
    contract: &AptContract,
    candidate: &str,
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
        return Err(GeneratorError::usage(
            "publish: bootstrap refuses to run over an existing preview pool",
        ));
    }
    Ok(())
}

/// Build and check the per-arch indexes for a bootstrap publication: exactly
/// the candidate version in each index.
fn build_bootstrap_indexes(
    staging: &Path,
    contract: &AptContract,
    candidate: &str,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    for arch in REQUIRED_ARCHES {
        let relative = format!("dists/preview/main/binary-{arch}/Packages");
        let packages = staging.join(&relative);
        if let Some(parent) = packages.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("create", parent, &error))?;
        }
        let stdout = run_in(
            staging,
            "apt-ftparchive",
            &apt_packages_argv(arch, "pool/preview"),
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
                "publish: {arch} bootstrap index must retain exactly the candidate version (observed: {})",
                observed.join(",")
            )));
        }
        let gz = run_fixed(
            "gzip",
            &[
                "-n".to_owned(),
                "-9".to_owned(),
                "-c".to_owned(),
                packages
                    .to_str()
                    .ok_or_else(|| GeneratorError::usage("Packages path is not UTF-8"))?
                    .to_owned(),
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

/// Append the preview stanza to `conf/distributions` unless it is already
/// there, so a re-run never duplicates it.
fn ensure_preview_stanza(staging: &Path, contract: &AptContract) -> Result<(), GeneratorError> {
    let path = staging.join("conf/distributions");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|line| line == "Codename: preview") {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    let _ = writeln!(
        text,
        "Origin: {0}\nLabel: {0}\nSuite: preview\nCodename: preview\nArchitectures: amd64 arm64\nComponents: main\nDescription: {1} (preview suite)\nSignWith: {2}",
        contract.origin, contract.description, contract.signer
    );
    std::fs::write(&path, text).map_err(|error| GeneratorError::io("write", &path, &error))?;
    Ok(())
}

/// Build the suite `Release` file and sign `Release.gpg` plus `InRelease`.
fn sign_suite_release(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
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
    std::fs::write(dir.join("Release"), stdout)
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
/// with exactly `tag` and a 64-hex `source_record_sha256`. The caller passes
/// the immutable pointer snapshot, so validation cannot race a replacement.
const ROLLBACK_PACKAGES_FIELD: &str = "rollback_packages";

/// Parse the exact retained package identity carried by the signed/live
/// previous-state handoff. A pathname snapshot alone is not authoritative:
/// every retained byte must match one of these external SHA-256 values.
fn rollback_package_digests(
    pointer: &serde_json::Value,
    suite: Suite,
    bootstrap: bool,
    package: &str,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    if suite == Suite::Preview && bootstrap {
        if !pointer.is_null() {
            return Err(GeneratorError::usage(
                "publish: bootstrap previous pointer must be JSON null",
            ));
        }
        return Ok(BTreeMap::new());
    }
    let object = pointer.as_object().ok_or_else(|| {
        if suite == Suite::Preview {
            GeneratorError::usage(
                "publish: preview previous pointer must be an object carrying rollback identities",
            )
        } else {
            GeneratorError::usage("publish: coherent previous pointer is malformed")
        }
    })?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let expected_keys = if suite == Suite::Stable {
        vec![ROLLBACK_PACKAGES_FIELD, "source_record_sha256", "tag"]
    } else {
        vec![ROLLBACK_PACKAGES_FIELD, "tag"]
    };
    if keys != expected_keys {
        return Err(GeneratorError::usage(if suite == Suite::Preview {
            "publish: preview previous pointer must carry rollback identities"
        } else {
            "publish: coherent previous pointer is malformed"
        }));
    }
    if suite == Suite::Stable && !valid_digest(field(pointer, "source_record_sha256")?) {
        return Err(GeneratorError::usage(
            "publish: coherent previous pointer is malformed",
        ));
    }
    if suite == Suite::Preview && field(pointer, "tag")? != PREVIEW_TAG {
        return Err(GeneratorError::usage(
            "publish: preview previous pointer tag is malformed",
        ));
    }
    let rows = object
        .get(ROLLBACK_PACKAGES_FIELD)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GeneratorError::usage("publish: previous pointer rollback packages are not an array")
        })?;
    if rows.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "publish: previous pointer must identify exactly both rollback packages",
        ));
    }
    let mut digests = BTreeMap::new();
    let mut arches = BTreeSet::new();
    for row in rows {
        exact_object_keys(row, &["name", "sha256"], "rollback package")?;
        let name = field(row, "name")?;
        if !valid_discovery_asset_name(name)
            || !is_deb_file(name)
            || !(name.starts_with(&format!("{package}-"))
                || name.starts_with(&format!("{package}_")))
        {
            return Err(GeneratorError::usage(
                "publish: previous pointer rollback package name is unsafe",
            ));
        }
        let arch = REQUIRED_ARCHES.iter().find(|arch| {
            name.ends_with(&format!("-{arch}.deb")) || name.ends_with(&format!("_{arch}.deb"))
        });
        let Some(arch) = arch else {
            return Err(GeneratorError::usage(
                "publish: previous pointer rollback package architecture is unsupported",
            ));
        };
        if !arches.insert(*arch) {
            return Err(GeneratorError::usage(
                "publish: previous pointer identifies a duplicate rollback architecture",
            ));
        }
        let digest = field(row, "sha256")?;
        if !valid_digest(digest) || digests.insert(name.to_owned(), digest.to_owned()).is_some() {
            return Err(GeneratorError::usage(
                "publish: previous pointer rollback package digest is invalid",
            ));
        }
    }
    if arches.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "publish: previous pointer must identify both rollback architectures",
        ));
    }
    Ok(digests)
}

fn check_stable_pointer_shape(
    pointer: &serde_json::Value,
    package: &str,
) -> Result<(), GeneratorError> {
    let _ = rollback_package_digests(pointer, Suite::Stable, false, package)?;
    Ok(())
}

/// Check the stable previous pointer names the retained rollback tag. The
/// tag agreement needs the rollback only the strict index build computes, so
/// the publisher runs it right after indexing but still before any signing.
fn check_stable_pointer(
    pointer: &serde_json::Value,
    rollback_tag: &str,
    package: &str,
) -> Result<(), GeneratorError> {
    check_stable_pointer_shape(pointer, package)?;
    if field(pointer, "tag")? != rollback_tag {
        return Err(GeneratorError::usage(
            "publish: previous pointer disagrees with retained rollback version",
        ));
    }
    Ok(())
}

/// Check the preview previous pointer: the JSON string `"preview"` once a
/// rollback pair is retained, JSON null for a bootstrapped suite.
fn check_preview_pointer(
    pointer: &serde_json::Value,
    bootstrap: bool,
    package: &str,
) -> Result<(), GeneratorError> {
    let _ = rollback_package_digests(pointer, Suite::Preview, bootstrap, package)?;
    Ok(())
}

fn rollback_pointer_from_publication(
    published: &serde_json::Value,
    prior_tag: &str,
) -> Result<serde_json::Value, GeneratorError> {
    let source_record_sha256 = field(published, "source_record_sha256")?;
    if !valid_digest(source_record_sha256) {
        return Err(GeneratorError::usage(
            "published rollback source-record checksum is invalid",
        ));
    }
    let version = parse_stable_tag(prior_tag)?.version;
    let entries = parse_deb_entries(
        published.get("deb_packages").ok_or_else(|| {
            GeneratorError::usage("published record has no deb package identities")
        })?,
        "published deb packages",
    )?;
    if entries.len() != REQUIRED_ARCHES.len() * 2 {
        return Err(GeneratorError::usage(
            "published record must identify candidate and rollback package pairs",
        ));
    }
    let rollback_packages = entries
        .into_iter()
        .filter(|entry| {
            entry.name.contains(&format!("_{version}_"))
                || entry.name.contains(&format!("-{version}-"))
        })
        .map(|entry| serde_json::json!({"name": entry.name, "sha256": entry.sha256}))
        .collect::<Vec<_>>();
    if rollback_packages.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "published record has no complete retained package pair for the prior tag",
        ));
    }
    Ok(serde_json::json!({
        "tag": prior_tag,
        "source_record_sha256": source_record_sha256,
        ROLLBACK_PACKAGES_FIELD: rollback_packages
    }))
}

fn validate_rollback_pointer_shape(pointer: &serde_json::Value) -> Result<(), GeneratorError> {
    let object = pointer
        .as_object()
        .ok_or_else(|| GeneratorError::usage("published rollback pointer is not an object"))?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != [ROLLBACK_PACKAGES_FIELD, "source_record_sha256", "tag"] {
        return Err(GeneratorError::usage(
            "published rollback pointer has an unsupported shape",
        ));
    }
    if !valid_digest(field(pointer, "source_record_sha256")?) {
        return Err(GeneratorError::usage(
            "published rollback checksum is invalid",
        ));
    }
    let rows = object
        .get(ROLLBACK_PACKAGES_FIELD)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GeneratorError::usage("published rollback packages are not an array"))?;
    if rows.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "published rollback pointer must contain both package rows",
        ));
    }
    let mut arches = BTreeSet::new();
    for row in rows {
        exact_object_keys(row, &["name", "sha256"], "published rollback package")?;
        let name = field(row, "name")?;
        if !valid_discovery_asset_name(name) || !is_deb_file(name) {
            return Err(GeneratorError::usage(
                "published rollback package name is invalid",
            ));
        }
        let arch = REQUIRED_ARCHES.iter().find(|arch| {
            name.ends_with(&format!("-{arch}.deb")) || name.ends_with(&format!("_{arch}.deb"))
        });
        if let Some(arch) = arch {
            arches.insert(*arch);
        } else {
            return Err(GeneratorError::usage(
                "published rollback package architecture is invalid",
            ));
        }
        if !valid_digest(field(row, "sha256")?) {
            return Err(GeneratorError::usage(
                "published rollback package digest is invalid",
            ));
        }
    }
    if arches.len() != REQUIRED_ARCHES.len() {
        return Err(GeneratorError::usage(
            "published rollback pointer must cover both architectures",
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
    if field(published, "schema")? != PUBLICATION_RECORD_SCHEMA {
        return Err(GeneratorError::usage(
            "unsupported publication record schema",
        ));
    }
    if !valid_digest(candidate_sha) {
        return Err(GeneratorError::usage(
            "candidate source-record digest is not 64 lowercase hex",
        ));
    }
    let tag = field(published, "tag")?;
    if tag == prior_tag {
        return rollback_pointer_from_publication(published, prior_tag);
    }
    if tag == candidate_tag {
        if field(published, "source_record_sha256")? != candidate_sha {
            return Err(GeneratorError::usage(
                "published candidate differs from immutable source release",
            ));
        }
        let previous = published
            .get("previous")
            .ok_or_else(|| GeneratorError::usage("published rollback checksum is invalid"))?;
        validate_rollback_pointer_shape(previous)?;
        if field(previous, "tag")? != prior_tag {
            return Err(GeneratorError::usage(
                "published rollback tag differs from signed package pair",
            ));
        }
        let sha = field(previous, "source_record_sha256")?;
        if !valid_digest(sha) {
            return Err(GeneratorError::usage(
                "published rollback checksum is invalid",
            ));
        }
        return Ok(previous.clone());
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

/// One exact staged package identity carried by a publication record. Index
/// digests authenticate the index bytes; these rows authenticate the retained
/// package bytes used for the next rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DebEntry {
    /// The canonical pool filename.
    pub(crate) name: String,
    /// The package SHA-256.
    pub(crate) sha256: String,
}

fn parse_deb_entries(
    value: &serde_json::Value,
    context: &str,
) -> Result<Vec<DebEntry>, GeneratorError> {
    let rows = value
        .as_array()
        .ok_or_else(|| GeneratorError::usage(format!("{context} is not an array")))?;
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    for package in rows {
        exact_object_keys(package, &["name", "sha256"], context)?;
        let name = field(package, "name")?;
        if !valid_discovery_asset_name(name) || !is_deb_file(name) || !names.insert(name) {
            return Err(GeneratorError::usage(format!(
                "{context} contains an invalid or duplicated package name"
            )));
        }
        let sha256 = field(package, "sha256")?;
        if !valid_digest(sha256) {
            return Err(GeneratorError::usage(format!(
                "{context} contains an invalid package digest"
            )));
        }
        entries.push(DebEntry {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
        });
    }
    Ok(entries)
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
    /// The exact candidate and rollback package identities.
    pub(crate) deb_packages: Vec<DebEntry>,
    /// The signing-key fingerprint.
    pub(crate) signer_fingerprint: String,
    /// The previous pointer document.
    pub(crate) previous: serde_json::Value,
    /// The canonical product-manifest digest, when emitted by schema 2.
    pub(crate) canonical_manifest_sha256: Option<String>,
    /// The canonical producer release identity, when emitted by schema 2.
    pub(crate) release_id: Option<String>,
    /// The provider numeric release identity, when emitted by schema 2.
    pub(crate) provider_release_id: Option<u64>,
}

/// Parse a publication record, failing closed on any malformed shape.
#[allow(
    clippy::too_many_lines,
    reason = "the publication record is one exact typed contract gate"
)]
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
        let arch = field(package, "arch")?;
        if !seen_arches.insert(arch) {
            return Err(GeneratorError::usage(
                "publication record repeats a Packages architecture",
            ));
        }
        entries.push(IndexEntry {
            arch: arch.to_owned(),
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
    let deb_entries = parse_deb_entries(
        document
            .get("deb_packages")
            .ok_or_else(|| GeneratorError::usage("publication record deb packages are missing"))?,
        "publication deb packages",
    )?;
    if !is_full_fingerprint(&normalize_fingerprint(field(
        document,
        "signer_fingerprint",
    )?)) {
        return Err(GeneratorError::usage(
            "publication record signer is not a full fingerprint",
        ));
    }
    let previous = document
        .get("previous")
        .cloned()
        .ok_or_else(|| GeneratorError::usage("publication record has no previous pointer"))?;
    let bootstrap = document.get("suite").and_then(serde_json::Value::as_str)
        == Some(PREVIEW_SUITE)
        && previous.is_null();
    let expected_deb_count = if bootstrap {
        REQUIRED_ARCHES.len()
    } else {
        REQUIRED_ARCHES.len() * 2
    };
    if deb_entries.len() != expected_deb_count {
        return Err(GeneratorError::usage(if bootstrap {
            "bootstrap publication record must identify the candidate package pair"
        } else {
            "publication record must identify candidate and rollback package pairs"
        }));
    }
    let canonical_manifest_sha256 = match document.get("canonical_manifest_sha256") {
        None => None,
        Some(value) => {
            let digest = value.as_str().ok_or_else(|| {
                GeneratorError::usage("publication canonical manifest digest is not a string")
            })?;
            if !valid_digest(digest) {
                return Err(GeneratorError::usage(
                    "publication canonical manifest digest is not valid",
                ));
            }
            Some(digest.to_owned())
        }
    };
    let release_id = match document.get("release_id") {
        None => None,
        Some(value) => {
            let release_id = value
                .as_str()
                .ok_or_else(|| GeneratorError::usage("publication release ID is not a string"))?;
            if !valid_release_id(release_id) {
                return Err(GeneratorError::usage(
                    "publication release ID has an invalid grammar",
                ));
            }
            Some(release_id.to_owned())
        }
    };
    let provider_release_id = match document.get("provider_release_id") {
        None => None,
        Some(value) => Some(value.as_u64().filter(|id| *id > 0).ok_or_else(|| {
            GeneratorError::usage("publication provider release ID is not positive")
        })?),
    };
    Ok(PublicationRecord {
        schema: PUBLICATION_RECORD_SCHEMA.to_owned(),
        source_record_sha256: source_record_sha256.to_owned(),
        tag: field(document, "tag")?.to_owned(),
        crate_version: field(document, "crate_version")?.to_owned(),
        suite: document
            .get("suite")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        inrelease_sha256: inrelease_sha256.to_owned(),
        packages: entries,
        deb_packages: deb_entries,
        signer_fingerprint: field(document, "signer_fingerprint")?.to_owned(),
        previous,
        canonical_manifest_sha256,
        release_id,
        provider_release_id,
    })
}

fn staged_deb_entries(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    expected_count: usize,
) -> Result<Vec<DebEntry>, GeneratorError> {
    let pool = pool_root(staging, suite, contract);
    let mut entries = Vec::new();
    for name in dir_names(&pool)? {
        if !is_deb_file(&name) {
            continue;
        }
        entries.push(DebEntry {
            sha256: sha256_file(&pool.join(&name))?,
            name,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    if entries.len() != expected_count {
        return Err(GeneratorError::usage(
            "publication record must identify the complete candidate and rollback package pairs",
        ));
    }
    Ok(entries)
}

/// Emit and detached-sign the publication record into the staging tree.
#[allow(
    clippy::too_many_lines,
    reason = "publication emission keeps the signed record assembly auditable"
)]
#[allow(clippy::too_many_arguments)]
fn emit_publication_record(
    staging: &Path,
    suite: Suite,
    contract: &AptContract,
    tag: &str,
    version: &str,
    source_digest: &str,
    previous_pointer: &serde_json::Value,
    path_overlay: Option<&Path>,
    passphrase: &str,
    homedir: &str,
    selection: &DiscoverySelection,
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
    record.insert(
        "deb_packages".to_owned(),
        serde_json::Value::Array(
            staged_deb_entries(
                staging,
                suite,
                contract,
                if suite == Suite::Preview && previous_pointer.is_null() {
                    REQUIRED_ARCHES.len()
                } else {
                    contract.retention.pool_debs()
                },
            )?
            .into_iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "sha256": entry.sha256
                })
            })
            .collect(),
        ),
    );
    record.insert("previous".to_owned(), previous_pointer.clone());
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
    if selection.suite()? != suite {
        return Err(GeneratorError::usage(
            "publication selection channel does not match suite",
        ));
    }
    record.insert(
        "canonical_manifest_sha256".to_owned(),
        serde_json::Value::String(selection.manifest_sha256.clone()),
    );
    record.insert(
        "provider_release_id".to_owned(),
        serde_json::Value::from(selection.provider_release_id),
    );
    record.insert(
        "release_id".to_owned(),
        serde_json::Value::String(selection.release_id.clone()),
    );
    record.insert(
        "release_tag".to_owned(),
        serde_json::Value::String(selection.release_tag.clone()),
    );
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

/// The no-rollback deploy guard: an older staged publication must never
/// clobber a newer live candidate. `live` is `None` when no live
/// `last-publish` exists yet (first deploy) or the probe failed closed
/// upstream — both deploy. Equal versions redeploy idempotently.
pub(crate) fn check_deploy_guard(
    suite: Suite,
    staged_text: &str,
    live: Option<&str>,
) -> Result<(), GeneratorError> {
    let staged = staged_text.trim();
    if staged.is_empty() {
        return Err(GeneratorError::usage(
            "deploy guard: staged last-publish is empty",
        ));
    }
    let Some(live) = live.map(str::trim).filter(|live| !live.is_empty()) else {
        return Ok(());
    };
    let order = match suite {
        Suite::Stable => cmp_stable_versions(staged, live)?,
        Suite::Preview => cmp_preview_versions(staged, live)?,
    };
    if order == std::cmp::Ordering::Less {
        return Err(GeneratorError::usage(format!(
            "deploy guard: staged {staged} is older than live {live}; refusing to roll back"
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
    /// The source ref: `refs/tags/vX.Y.Z` (stable) or `refs/heads/main`.
    pub(crate) source_ref: String,
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

/// Bind the canonical application identity to channel state after the shared
/// package-state checks pass. Existing feed fields remain unchanged; these
/// fields make the state joinable with the exact discovery/publication record
/// without replacing the retained stable or preview pair.
pub(crate) fn bind_selection_channel_state(
    selection: &DiscoverySelection,
    staging: &Path,
) -> Result<(), GeneratorError> {
    let suite = selection.suite()?;
    let path = staging.join(suite.channel_state_file());
    let mut document = read_json(&path)?;
    let object = document
        .as_object_mut()
        .ok_or_else(|| GeneratorError::usage("channel state is not an object"))?;
    let values = [
        (
            "canonical_manifest_sha256",
            serde_json::Value::String(selection.manifest_sha256.clone()),
        ),
        (
            "product_version",
            serde_json::Value::String(selection.version.clone()),
        ),
        (
            "release_id",
            serde_json::Value::String(selection.release_id.clone()),
        ),
        (
            "release_tag",
            serde_json::Value::String(selection.release_tag.clone()),
        ),
        (
            "provider_release_id",
            serde_json::Value::from(selection.provider_release_id),
        ),
    ];
    for (key, value) in values {
        if object.get(key).is_some_and(|existing| existing != &value) {
            return Err(GeneratorError::usage(format!(
                "channel state {key} differs from immutable discovery"
            )));
        }
        object.insert(key.to_owned(), value);
    }
    let text = serde_json::to_string_pretty(&document).map_err(|error| {
        GeneratorError::usage(format!("channel state is not serializable: {error}"))
    })?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|error| GeneratorError::io("bind channel state", &path, &error))?;
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
            if inputs.source_ref != PREVIEW_SOURCE_REF {
                return Err(GeneratorError::usage(format!(
                    "channel update: preview source_ref must be {PREVIEW_SOURCE_REF}"
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
            if field(manifest, "source_ref")? != PREVIEW_SOURCE_REF {
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
    const NATIVE_ASSEMBLED_PRODUCT_MANIFEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/native-product/product-manifest.json"
    ));
    const NATIVE_ASSEMBLED_PRODUCT_MANIFEST_SIDECAR: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/native-product/product-manifest.json.sha256"
    ));

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

    /// Add the canonical producer selection to the legacy-shaped package
    /// fixtures. The package verifier still exercises its historical
    /// subordinate schemas, but publication now requires this complete
    /// product inventory and immutable asset proof in every mode.
    #[allow(clippy::too_many_lines)]
    fn attach_product_selection(
        dir: &Path,
        channel: &str,
        source: &str,
        package: &str,
        binary: &str,
        identity: &str,
        product_version: &str,
        commit: &str,
        tag: &str,
    ) {
        let apt_names = must(
            dir_names(dir),
            "list fixture assets before attaching product selection",
        )
        .into_iter()
        .filter(|name| name.starts_with(package) && is_deb_file(name))
        .collect::<Vec<_>>();
        assert_eq!(apt_names.len(), REQUIRED_ARCHES.len());
        let base_version = product_version
            .split("~preview.")
            .next()
            .unwrap_or(product_version);
        let release_id = "123";
        let package_manifest_path = dir.join(MANIFEST_FILE);
        let package_manifest = if package_manifest_path.is_file() {
            must(
                serde_json::from_slice::<serde_json::Value>(&must(
                    std::fs::read(&package_manifest_path),
                    "read fixture package manifest",
                )),
                "parse fixture package manifest",
            )
        } else {
            serde_json::json!({
                "schema": "velnor.package-release.v1",
                "source_sha": commit,
                "crate_version": base_version,
                "version": 1
            })
        };
        let mut package_manifest_object = package_manifest
            .as_object()
            .cloned()
            .expect("fixture package manifest object");
        package_manifest_object.insert(
            "parent_manifest_id".to_owned(),
            serde_json::Value::String(release_id.to_owned()),
        );
        let package_manifest_bytes = must(
            serde_json::to_vec(&serde_json::Value::Object(package_manifest_object)),
            "serialize fixture package manifest",
        );
        write_bytes(&package_manifest_path, &package_manifest_bytes);
        write_sidecar(
            &dir.join(MANIFEST_SIDECAR),
            &sha256_hex(&package_manifest_bytes),
        );
        if channel == "stable" {
            for name in &apt_names {
                let arch = if name.ends_with("-amd64.deb") {
                    "amd64"
                } else {
                    "arm64"
                };
                let _ = make_deb(
                    dir,
                    name,
                    package,
                    product_version,
                    arch,
                    binary,
                    identity,
                    commit,
                    product_version,
                    &package_manifest_bytes,
                    b"fixture-daemon-bytes",
                );
                let digest = must(sha256_file(&dir.join(name)), "hash rebuilt fixture deb");
                write_sidecar(&dir.join(format!("{name}.sha256")), &digest);
            }
        }
        let mut artifact_values = Vec::new();
        let mut artifact_assets = Vec::new();
        let components = serde_json::json!([
            {
                "name": "velnorctl",
                "crate": "velnorctl",
                "version": base_version,
                "binary": "velnorctl",
                "feature": "release-build",
                "identity": "version",
                "targets": PRODUCT_TARGETS
            },
            {
                "name": "velnor-runner",
                "crate": "velnor-runner",
                "version": base_version,
                "binary": "velnor-runner",
                "feature": "release-build",
                "identity": "version",
                "targets": PRODUCT_TARGETS
            },
            {
                "name": "velnor-workflow",
                "crate": "velnor-workflow",
                "version": base_version,
                "binary": "velnor-workflow",
                "feature": null,
                "identity": "revision",
                "targets": PRODUCT_TARGETS
            }
        ]);
        for component in ["velnorctl", "velnor-runner", "velnor-workflow"] {
            for target in PRODUCT_TARGETS {
                let name = format!("{component}-{target}");
                let bytes = format!("fixture-{channel}-{name}").into_bytes();
                artifact_values.push(serde_json::json!({
                    "name": name,
                    "target": target,
                    "kind": "binary",
                    "sha256": sha256_hex(&bytes),
                    "size": bytes.len()
                }));
                artifact_assets.push((name, bytes));
            }
        }
        for target in PRODUCT_TARGETS {
            let name = format!("velnor-{target}.tar");
            let bytes = format!("fixture-archive-{channel}-{target}").into_bytes();
            let kind = if target.ends_with("-apple-darwin") {
                "homebrew-archive"
            } else {
                "archive"
            };
            artifact_values.push(serde_json::json!({
                "name": name,
                "target": target,
                "kind": kind,
                "sha256": sha256_hex(&bytes),
                "size": bytes.len()
            }));
            artifact_assets.push((name, bytes));
        }
        for name in &apt_names {
            let target = if name.ends_with("-amd64.deb") {
                "x86_64-unknown-linux-gnu"
            } else {
                "aarch64-unknown-linux-gnu"
            };
            let bytes = must(std::fs::read(dir.join(name)), "read fixture deb");
            artifact_values.push(serde_json::json!({
                "name": name,
                "target": target,
                "kind": "apt-package",
                "sha256": sha256_hex(&bytes),
                "size": bytes.len()
            }));
        }
        let source_ref = if channel == "stable" {
            format!("refs/tags/{tag}")
        } else {
            PREVIEW_SOURCE_REF.to_owned()
        };
        let release_tag = if channel == "stable" {
            tag.to_owned()
        } else {
            format!("preview-{commit}")
        };
        let manifest = serde_json::json!({
            "schema": PRODUCT_MANIFEST_SCHEMA,
            "product_id": "velnor",
            "channel": channel,
            "version": product_version.replace("~preview.", "-preview."),
            "source_repository": source,
            "source_ref": source_ref,
            "source_commit": commit,
            "release_tag": release_tag,
            "release_id": release_id,
            "artifacts": artifact_values.clone(),
            "components": components
        });
        let manifest_bytes = must(
            serde_json::to_vec(&manifest),
            "serialize fixture product manifest",
        );
        let manifest_sha = sha256_hex(&manifest_bytes);
        write_bytes(&dir.join(PRODUCT_MANIFEST_ASSET), &manifest_bytes);
        write_bytes(
            &dir.join("product-manifest.json.sha256"),
            format!("{manifest_sha}  {PRODUCT_MANIFEST_ASSET}\n").as_bytes(),
        );

        // Bind both package-specific subordinate records to the external
        // product digest, preserving their existing role-specific fields.
        for name in [RECORD_FILE, MANIFEST_FILE] {
            let document = if dir.join(name).is_file() {
                must(
                    serde_json::from_slice::<serde_json::Value>(&must(
                        std::fs::read(dir.join(name)),
                        "read fixture subordinate",
                    )),
                    "parse fixture subordinate",
                )
            } else {
                serde_json::json!({
                    "schema": "velnor.package-release.v1",
                    "source_sha": commit,
                    "crate_version": base_version,
                    "version": 1
                })
            };
            let mut object = document
                .as_object()
                .cloned()
                .expect("fixture subordinate object");
            let (parent_field, parent_value) = if name == MANIFEST_FILE {
                ("parent_manifest_id", release_id.to_owned())
            } else {
                ("parent_manifest_sha256", manifest_sha.clone())
            };
            object.insert(
                parent_field.to_owned(),
                serde_json::Value::String(parent_value),
            );
            let bytes = must(
                serde_json::to_vec(&serde_json::Value::Object(object)),
                "serialize fixture subordinate",
            );
            write_bytes(&dir.join(name), &bytes);
            write_sidecar(&dir.join(format!("{name}.sha256")), &sha256_hex(&bytes));
        }

        let release_assets_json = apt_names
            .iter()
            .map(|name| {
                let bytes = must(std::fs::read(dir.join(name)), "read fixture package");
                serde_json::json!({"name": name, "sha256": sha256_hex(&bytes)})
            })
            .collect::<Vec<_>>();
        let release_manifest = if channel == "preview" {
            let mut document = must(
                serde_json::from_slice::<serde_json::Value>(&must(
                    std::fs::read(dir.join(PREVIEW_MANIFEST_FILE)),
                    "read fixture release manifest",
                )),
                "parse fixture release manifest",
            );
            document["parent_manifest_sha256"] = serde_json::Value::String(manifest_sha.clone());
            document["assets"] = serde_json::Value::Array(release_assets_json);
            document
        } else {
            serde_json::json!({
                "schema": FIXTURE_SCHEMA,
                "source_repository": source,
                "source_ref": source_ref,
                "source_commit": commit,
                "version": product_version,
                "parent_manifest_sha256": manifest_sha,
                "assets": release_assets_json
            })
        };
        let release_manifest_bytes = must(
            serde_json::to_vec(&release_manifest),
            "serialize fixture release manifest",
        );
        write_bytes(&dir.join(PREVIEW_MANIFEST_FILE), &release_manifest_bytes);
        let mut sums = apt_names
            .iter()
            .map(|name| {
                let bytes = must(std::fs::read(dir.join(name)), "read fixture package");
                format!("{}  {name}\n", sha256_hex(&bytes))
            })
            .collect::<Vec<_>>();
        sums.sort();
        write_bytes(&dir.join(SHA256SUMS_FILE), sums.concat().as_bytes());
        let record_path = dir.join(RECORD_FILE);
        let mut record = must(
            serde_json::from_slice::<serde_json::Value>(&must(
                std::fs::read(&record_path),
                "read fixture release record",
            )),
            "parse fixture release record",
        );
        if record.get("build").is_some() {
            let package_manifest_sha = must(
                sha256_file(&dir.join(MANIFEST_FILE)),
                "hash fixture package manifest",
            );
            record["build"]["manifest_sha256"] =
                serde_json::Value::String(package_manifest_sha.clone());
            if record.get("oci_labels").is_some() {
                record["oci_labels"]["manifest_sha256"] =
                    serde_json::Value::String(package_manifest_sha);
            }
            if let Some(rows) = record
                .get_mut("architectures")
                .and_then(serde_json::Value::as_array_mut)
            {
                for row in rows {
                    let arch = must(field(row, "arch"), "read fixture architecture");
                    let name = format!("{package}-{product_version}-{arch}.deb");
                    row["deb_sha256"] = serde_json::Value::String(must(
                        sha256_file(&dir.join(name)),
                        "hash rebuilt fixture architecture deb",
                    ));
                }
            }
        }
        let record_bytes = must(
            serde_json::to_vec(&record),
            "serialize fixture release record",
        );
        write_bytes(&record_path, &record_bytes);
        write_sidecar(&dir.join(RECORD_SIDECAR), &sha256_hex(&record_bytes));
        for (name, bytes) in artifact_assets {
            write_bytes(&dir.join(&name), &bytes);
        }

        let mut raw_assets = vec![
            (PRODUCT_MANIFEST_ASSET.to_owned(), manifest_bytes),
            (
                "product-manifest.json.sha256".to_owned(),
                format!("{manifest_sha}  {PRODUCT_MANIFEST_ASSET}\n").into_bytes(),
            ),
        ];
        for name in [RECORD_FILE, RECORD_SIDECAR, MANIFEST_FILE, MANIFEST_SIDECAR] {
            raw_assets.push((
                name.to_owned(),
                must(std::fs::read(dir.join(name)), "read subordinate asset"),
            ));
        }
        raw_assets.push((PREVIEW_MANIFEST_FILE.to_owned(), release_manifest_bytes));
        raw_assets.push((
            SHA256SUMS_FILE.to_owned(),
            must(
                std::fs::read(dir.join(SHA256SUMS_FILE)),
                "read fixture sums",
            ),
        ));
        for (name, bytes) in artifact_assets_from_manifest(&artifact_values, dir) {
            raw_assets.push((name, bytes));
        }
        let mut release_assets = Vec::new();
        for (offset, (name, bytes)) in raw_assets.into_iter().enumerate() {
            release_assets.push(serde_json::json!({
                "id": 100 + offset as u64,
                "name": name,
                "size": bytes.len(),
                "state": "uploaded",
                "browser_download_url": format!(
                    "https://github.com/{source}/releases/download/{release_tag}/{name}"
                )
            }));
        }
        let source_ref_resolution = if channel == "preview" {
            serde_json::json!({
                "declared_ref_provenance": {
                    "base_commit": commit,
                    "head_commit": commit,
                    "merge_base_commit": commit,
                    "method": "github-compare-ancestry",
                    "ref": PREVIEW_SOURCE_REF,
                    "relation": "tip",
                    "status": "identical"
                },
                "method": "github-git-ref",
                "proof_ref": format!("refs/tags/{release_tag}"),
                "resolved_commit": commit
            })
        } else {
            serde_json::json!({
                "method": "github-git-ref",
                "proof_ref": format!("refs/tags/{release_tag}"),
                "resolved_commit": commit
            })
        };
        let selection = serde_json::json!({
            "channel": channel,
            "manifest": manifest,
            "manifest_asset": PRODUCT_MANIFEST_ASSET,
            "manifest_schema": PRODUCT_MANIFEST_SCHEMA,
            "manifest_sha256": manifest_sha,
            "package": package,
            "product_id": "velnor",
            "provider_release_id": 123,
            "published_at": "2026-09-20T00:00:00Z",
            "release_assets": release_assets,
            "release_id": release_id,
            "release_tag": release_tag,
            "release_url": format!("https://github.com/{source}/releases/tag/{release_tag}"),
            "source_commit": commit,
            "source_ref": source_ref,
            "source_ref_resolution": source_ref_resolution,
            "source_repository": source,
            "tag": release_tag,
            "target_commitish": release_tag,
            "version": product_version.replace("~preview.", "-preview.")
        });
        let selection_bytes = must(
            serde_json::to_vec(&selection),
            "serialize fixture discovery selection",
        );
        write_bytes(&dir.join(DISCOVERY_SELECTION_FILE), &selection_bytes);
    }

    fn artifact_assets_from_manifest(
        artifacts: &[serde_json::Value],
        dir: &Path,
    ) -> Vec<(String, Vec<u8>)> {
        let mut assets = Vec::new();
        for artifact in artifacts {
            let name = must(field(artifact, "name"), "fixture artifact name");
            let bytes = if name.ends_with(".deb") {
                must(std::fs::read(dir.join(name)), "read fixture deb artifact")
            } else {
                must(std::fs::read(dir.join(name)), "read fixture artifact")
            };
            assets.push((name.to_owned(), bytes));
            if name.ends_with(".deb") {
                assets.push((
                    format!("{name}.sha256"),
                    must(
                        std::fs::read(dir.join(format!("{name}.sha256"))),
                        "read fixture deb sidecar",
                    ),
                ));
            }
        }
        assets
    }

    struct DiscoveryFixture {
        root: PathBuf,
        selection_path: PathBuf,
        incoming: PathBuf,
        document: serde_json::Value,
        assets: Vec<(u64, String, Vec<u8>)>,
    }

    #[allow(clippy::too_many_lines)]
    fn discovery_fixture(name: &str) -> DiscoveryFixture {
        let root = fixture_dir(name);
        let selection_path = root.join("selection.json");
        let incoming = root.join("incoming");
        let components = serde_json::json!([
            {
                "name": "velnorctl",
                "crate": "velnorctl",
                "version": "0.1.0",
                "binary": "velnorctl",
                "feature": "release-build",
                "identity": "version",
                "targets": PRODUCT_TARGETS
            },
            {
                "name": "velnor-runner",
                "crate": "velnor-runner",
                "version": "0.1.0",
                "binary": "velnor-runner",
                "feature": "release-build",
                "identity": "version",
                "targets": PRODUCT_TARGETS
            },
            {
                "name": "velnor-workflow",
                "crate": "velnor-workflow",
                "version": "0.1.0",
                "binary": "velnor-workflow",
                "feature": null,
                "identity": "revision",
                "targets": PRODUCT_TARGETS
            }
        ]);
        let mut artifact_values = Vec::new();
        let mut artifact_assets = Vec::new();
        for component in ["velnorctl", "velnor-runner", "velnor-workflow"] {
            for target in PRODUCT_TARGETS {
                let asset_name = format!("{component}-{target}");
                let bytes = format!("binary-{component}-{target}").into_bytes();
                artifact_values.push(serde_json::json!({
                    "name": asset_name,
                    "target": target,
                    "kind": "binary",
                    "sha256": sha256_hex(&bytes),
                    "size": bytes.len()
                }));
                artifact_assets.push((asset_name, bytes));
            }
        }
        for target in PRODUCT_TARGETS {
            let kind = if target.ends_with("-apple-darwin") {
                "homebrew-archive"
            } else {
                "archive"
            };
            let asset_name = format!("velnor-{target}.tar");
            let bytes = format!("archive-{target}").into_bytes();
            artifact_values.push(serde_json::json!({
                "name": asset_name,
                "target": target,
                "kind": kind,
                "sha256": sha256_hex(&bytes),
                "size": bytes.len()
            }));
            artifact_assets.push((asset_name, bytes));
        }
        for (arch, target) in [
            ("amd64", "x86_64-unknown-linux-gnu"),
            ("arm64", "aarch64-unknown-linux-gnu"),
        ] {
            let asset_name = format!("example-1.2.3-{arch}.deb");
            let bytes = format!("{arch}-deb").into_bytes();
            artifact_values.push(serde_json::json!({
                "name": asset_name,
                "target": target,
                "kind": "apt-package",
                "sha256": sha256_hex(&bytes),
                "size": bytes.len()
            }));
            artifact_assets.push((asset_name, bytes));
        }
        let manifest = serde_json::json!({
            "schema": PRODUCT_MANIFEST_SCHEMA,
            "product_id": "velnor",
            "channel": "stable",
            "version": "1.2.3",
            "source_repository": FIXTURE_SOURCE,
            "source_ref": "refs/tags/v1.2.3",
            "source_commit": FIXTURE_COMMIT,
            "release_tag": "v1.2.3",
            "release_id": "123",
            "artifacts": artifact_values,
            "components": components
        });
        let manifest_bytes = must(
            serde_json::to_vec(&manifest),
            "serialize discovery product manifest",
        );
        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let record_bytes = must(
            serde_json::to_vec(&serde_json::json!({
                "parent_manifest_sha256": manifest_sha256,
                "schema": RELEASE_RECORD_SCHEMA
            })),
            "serialize fixture release record",
        );
        let package_bytes = must(
            serde_json::to_vec(&serde_json::json!({
                "parent_manifest_id": "123",
                "schema": "velnor.package-release.v1"
            })),
            "serialize fixture package record",
        );
        let release_manifest_bytes = must(
            serde_json::to_vec(&serde_json::json!({
                "parent_manifest_sha256": manifest_sha256,
                "schema": "velnor.package-release.v1",
                "assets": artifact_values
                    .iter()
                    .filter(|artifact| artifact["kind"] == "apt-package")
                    .map(|artifact| serde_json::json!({
                        "name": artifact["name"],
                        "sha256": artifact["sha256"]
                    }))
                    .collect::<Vec<_>>()
            })),
            "serialize fixture release manifest",
        );
        let sums = artifact_values
            .iter()
            .filter(|artifact| artifact["kind"] == "apt-package")
            .map(|artifact| {
                format!(
                    "{}  {}\n",
                    artifact["sha256"].as_str().unwrap_or_default(),
                    artifact["name"].as_str().unwrap_or_default()
                )
            })
            .collect::<String>();
        let mut raw_assets = vec![
            (PRODUCT_MANIFEST_ASSET.to_owned(), manifest_bytes),
            (
                "product-manifest.json.sha256".to_owned(),
                format!("{manifest_sha256}  product-manifest.json\n").into_bytes(),
            ),
            (RECORD_FILE.to_owned(), record_bytes.clone()),
            (
                RECORD_SIDECAR.to_owned(),
                format!("{}  {RECORD_FILE}\n", sha256_hex(&record_bytes)).into_bytes(),
            ),
            (MANIFEST_FILE.to_owned(), package_bytes.clone()),
            (
                MANIFEST_SIDECAR.to_owned(),
                format!("{}  {MANIFEST_FILE}\n", sha256_hex(&package_bytes)).into_bytes(),
            ),
            (PREVIEW_MANIFEST_FILE.to_owned(), release_manifest_bytes),
            (SHA256SUMS_FILE.to_owned(), sums.into_bytes()),
        ];
        for (asset_name, bytes) in artifact_assets {
            raw_assets.push((asset_name.clone(), bytes.clone()));
            if asset_name.ends_with(".deb") {
                raw_assets.push((
                    format!("{asset_name}.sha256"),
                    format!("{}  {asset_name}\n", sha256_hex(&bytes)).into_bytes(),
                ));
            }
        }
        let mut release_assets = Vec::new();
        let mut assets = Vec::new();
        for (offset, (name, bytes)) in raw_assets.drain(..).enumerate() {
            let id = 100 + offset as u64;
            release_assets.push(serde_json::json!({
                "id": id,
                "name": name,
                "size": bytes.len(),
                "state": "uploaded",
                "browser_download_url": format!(
                    "https://github.com/{FIXTURE_SOURCE}/releases/download/v1.2.3/{name}"
                )
            }));
            assets.push((id, name, bytes));
        }
        let document = serde_json::json!({
            "channel": "stable",
            "manifest": manifest,
            "manifest_asset": PRODUCT_MANIFEST_ASSET,
            "manifest_schema": PRODUCT_MANIFEST_SCHEMA,
            "manifest_sha256": manifest_sha256,
            "package": FIXTURE_PACKAGE,
            "product_id": "velnor",
            "provider_release_id": 123,
            "published_at": "2026-09-20T00:00:00Z",
            "release_assets": release_assets,
            "release_id": "123",
            "release_tag": "v1.2.3",
            "release_url": format!("https://github.com/{FIXTURE_SOURCE}/releases/tag/v1.2.3"),
            "source_commit": FIXTURE_COMMIT,
            "source_ref": "refs/tags/v1.2.3",
            "source_ref_resolution": {
                "method": "github-git-ref",
                "proof_ref": "refs/tags/v1.2.3",
                "resolved_commit": FIXTURE_COMMIT
            },
            "source_repository": FIXTURE_SOURCE,
            "tag": "v1.2.3",
            "target_commitish": "v1.2.3",
            "version": "1.2.3"
        });
        let document_bytes = must(
            serde_json::to_vec(&document),
            "serialize discovery selection",
        );
        write_bytes(&selection_path, &document_bytes);
        write_bytes(&incoming.join(DISCOVERY_SELECTION_FILE), &document_bytes);
        for (_, name, bytes) in &assets {
            write_bytes(&incoming.join(name), bytes);
        }
        DiscoveryFixture {
            root,
            selection_path,
            incoming,
            document,
            assets,
        }
    }

    fn discovery_gh_stub(fixture: &DiscoveryFixture) -> PathBuf {
        let bin = fixture.root.join("bin");
        let asset_root = fixture.root.join("asset-by-id");
        must(std::fs::create_dir_all(&bin), "create gh stub bin");
        must(std::fs::create_dir_all(&asset_root), "create gh asset root");
        let log = fixture.root.join("gh.log");
        let mut script = format!(
            "#!/bin/sh\nendpoint=\"\"\nfor arg in \"$@\"; do endpoint=\"$arg\"; done\nprintf '%s\\n' \"$endpoint\" >> \"{}\"\ncase \"$endpoint\" in\n",
            log.display()
        );
        for (id, _, bytes) in &fixture.assets {
            write_bytes(&asset_root.join(format!("asset-{id}")), bytes);
            must(
                writeln!(
                    &mut script,
                    "  repos/{FIXTURE_SOURCE}/releases/assets/{id}) cat \"{}/asset-{id}\" ;;",
                    asset_root.display()
                ),
                "append gh stub case",
            );
        }
        script.push_str("  *) exit 1 ;;\nesac\n");
        write_bytes(&bin.join("gh"), script.as_bytes());
        make_executable(&bin.join("gh"));
        bin
    }

    #[test]
    fn native_assembly_manifest_is_consumable_by_apt_projection() {
        // These bytes were emitted by the native producer's rendered
        // assembly at checkpoint 384e2e4e. Provider release metadata below is
        // synthetic fixture input only; it is never treated as live authority.
        let manifest: serde_json::Value = must(
            serde_json::from_slice(NATIVE_ASSEMBLED_PRODUCT_MANIFEST),
            "parse native assembly manifest fixture",
        );
        assert_eq!(
            manifest["components"][0]["feature"],
            serde_json::json!("release-build")
        );
        assert_eq!(
            manifest["components"][1]["identity"],
            serde_json::json!("revision")
        );
        let manifest_sha256 = sha256_hex(NATIVE_ASSEMBLED_PRODUCT_MANIFEST);
        assert_eq!(
            must(
                product_manifest_sidecar_digest_bytes(
                    NATIVE_ASSEMBLED_PRODUCT_MANIFEST_SIDECAR.as_bytes(),
                    Path::new("product-manifest.json.sha256"),
                ),
                "parse native assembly manifest sidecar",
            ),
            manifest_sha256
        );

        let source_repository = must(
            field(&manifest, "source_repository"),
            "read source repository",
        );
        let release_tag = must(field(&manifest, "release_tag"), "read release tag");
        let mut asset_sizes = BTreeMap::new();
        for artifact in manifest
            .get("artifacts")
            .and_then(serde_json::Value::as_array)
            .expect("native assembly artifacts array")
        {
            asset_sizes.insert(
                must(field(artifact, "name"), "read native artifact name").to_owned(),
                must(
                    artifact
                        .get("size")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| GeneratorError::usage("native artifact size is invalid")),
                    "read native artifact size",
                ),
            );
        }
        for name in [
            PRODUCT_MANIFEST_ASSET,
            "product-manifest.json.sha256",
            RECORD_FILE,
            RECORD_SIDECAR,
            MANIFEST_FILE,
            MANIFEST_SIDECAR,
            PREVIEW_MANIFEST_FILE,
            SHA256SUMS_FILE,
        ] {
            asset_sizes.insert(name.to_owned(), 1);
        }
        let deb_names = asset_sizes
            .keys()
            .filter(|name| name.ends_with(".deb"))
            .cloned()
            .collect::<Vec<_>>();
        for name in deb_names {
            asset_sizes.insert(format!("{name}.sha256"), 1);
        }
        let release_assets = asset_sizes
            .into_iter()
            .enumerate()
            .map(|(offset, (name, size))| {
                serde_json::json!({
                    "id": 5000 + offset as u64,
                    "name": name,
                    "size": size,
                    "state": "uploaded",
                    "browser_download_url": format!(
                        "https://github.com/{source_repository}/releases/download/{release_tag}/{name}"
                    )
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(release_assets.len(), 28);
        let source_commit = must(field(&manifest, "source_commit"), "read source commit");
        let source_ref = must(field(&manifest, "source_ref"), "read source ref");
        let version = must(field(&manifest, "version"), "read product version");
        let product_id = must(field(&manifest, "product_id"), "read product ID");
        let release_id = must(field(&manifest, "release_id"), "read release ID");
        let selection = serde_json::json!({
            "channel": "stable",
            "manifest": manifest,
            "manifest_asset": PRODUCT_MANIFEST_ASSET,
            "manifest_schema": PRODUCT_MANIFEST_SCHEMA,
            "manifest_sha256": manifest_sha256,
            "package": "velnor-runner",
            "product_id": product_id,
            "provider_release_id": 12345,
            "published_at": "2026-09-20T00:00:00Z",
            "release_assets": release_assets,
            "release_id": release_id,
            "release_tag": release_tag,
            "release_url": format!("https://github.com/{source_repository}/releases/tag/{release_tag}"),
            "source_commit": source_commit,
            "source_ref": source_ref,
            "source_ref_resolution": {
                "method": "github-git-ref",
                "proof_ref": format!("refs/tags/{release_tag}"),
                "resolved_commit": source_commit
            },
            "source_repository": source_repository,
            "tag": release_tag,
            "target_commitish": release_tag,
            "version": version
        });
        let root = fixture_dir("native-assembly-selection");
        let path = root.join(DISCOVERY_SELECTION_FILE);
        write_bytes(
            &path,
            &must(
                serde_json::to_vec(&selection),
                "serialize native assembly selection",
            ),
        );
        let parsed = must(
            read_discovery_selection(&path),
            "parse native producer output through APT selection contract",
        );
        assert_eq!(parsed.manifest_sha256, manifest_sha256);
        assert_eq!(parsed.release_id, "12345");
        assert_eq!(parsed.release_assets.len(), 28);
        assert_eq!(
            parsed
                .manifest
                .get("artifacts")
                .expect("parsed native artifacts"),
            selection
                .get("manifest")
                .and_then(|value| value.get("artifacts"))
                .expect("selection native artifacts")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn make_symlink(target: &Path, link: &Path) {
        must(
            std::os::unix::fs::symlink(target, link),
            "create fixture symlink",
        );
    }

    #[cfg(unix)]
    fn make_hard_link(target: &Path, link: &Path) {
        must(std::fs::hard_link(target, link), "create fixture hard link");
    }

    #[test]
    fn discovery_selection_binds_asset_ids_and_preserves_hidden_sentinel() {
        let fixture = discovery_fixture("discovery-immutable");
        let bin = discovery_gh_stub(&fixture);
        let fetched = fixture.root.join("fetched");
        let selected = must(
            run_fetch_selection(&fixture.selection_path, &fetched, Some(&bin)),
            "fetch immutable selection",
        );
        let log = must(
            std::fs::read_to_string(fixture.root.join("gh.log")),
            "read gh asset log",
        );
        let expected = selected
            .release_assets
            .iter()
            .map(|asset| format!("repos/{}/releases/assets/{}", FIXTURE_SOURCE, asset.id))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(log.trim_end(), expected);

        must(
            verify_discovery_incoming(&fixture.selection_path, &fetched),
            "verify fetched selection",
        );
        assert!(!fetched.join(SENTINEL_FILE).exists());

        let mut tampered = fixture.document.clone();
        tampered["release_assets"][0]["id"] = serde_json::json!(9999);
        write_bytes(
            &fetched.join(DISCOVERY_SELECTION_FILE),
            &must(serde_json::to_vec(&tampered), "serialize ID tamper"),
        );
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fetched),
            "reject persisted asset ID tamper",
        );
        assert!(
            error.contains("differs from the selected release"),
            "{error}"
        );
        write_bytes(
            &fetched.join(DISCOVERY_SELECTION_FILE),
            &must(
                serde_json::to_vec(&fixture.document),
                "restore persisted selection",
            ),
        );

        write_bytes(&fetched.join("example-1.2.3-amd64.deb"), b"tamperxxx");
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fetched),
            "reject selected artifact byte tamper",
        );
        assert!(
            error.contains("differs from canonical inventory"),
            "{error}"
        );
        write_bytes(&fetched.join("example-1.2.3-amd64.deb"), b"amd64-deb");
        write_bytes(&fetched.join(".unexpected"), b"hidden");
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fetched),
            "reject unknown hidden asset",
        );
        assert!(error.contains("absent from discovery"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_incoming_rejects_symlinked_selection_paths() {
        for (index, name) in [
            DISCOVERY_SELECTION_FILE,
            PRODUCT_MANIFEST_ASSET,
            "product-manifest.json.sha256",
            "example-1.2.3-amd64.deb",
        ]
        .into_iter()
        .enumerate()
        {
            let fixture = discovery_fixture(&format!("discovery-symlink-{index}"));
            let selected_path = fixture.incoming.join(name);
            let outside = fixture.root.join(format!("outside-{index}"));
            let bytes = must(std::fs::read(&selected_path), "read symlink target bytes");
            write_bytes(&outside, &bytes);
            must(
                std::fs::remove_file(&selected_path),
                "remove selected regular file",
            );
            make_symlink(&outside, &selected_path);
            let error = must_fail(
                verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
                "reject selected symlink",
            );
            assert!(error.contains("symlink"), "{error}");
            let _ = std::fs::remove_dir_all(&fixture.root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovery_incoming_rejects_hardlinks_and_stale_sentinel() {
        let fixture = discovery_fixture("discovery-hardlink");
        let selected_path = fixture.incoming.join(PRODUCT_MANIFEST_ASSET);
        let outside = fixture.root.join("outside-manifest");
        let bytes = must(std::fs::read(&selected_path), "read hardlink source");
        write_bytes(&outside, &bytes);
        must(
            std::fs::remove_file(&selected_path),
            "remove selected regular manifest",
        );
        make_hard_link(&outside, &selected_path);
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
            "reject hardlinked selected manifest",
        );
        assert!(error.contains("multiple links"), "{error}");
        must(
            std::fs::remove_file(&selected_path),
            "remove hardlinked selected manifest",
        );
        write_bytes(&selected_path, &bytes);
        write_bytes(&fixture.incoming.join(SENTINEL_FILE), b"verified\n");
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
            "reject stale sentinel",
        );
        assert!(error.contains("stale"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_incoming_rejects_hardlinked_sentinel() {
        let fixture = discovery_fixture("discovery-sentinel-hardlink");
        let outside = fixture.root.join("outside-sentinel");
        write_bytes(&outside, b"selection:stale\n");
        make_hard_link(&outside, &fixture.incoming.join(SENTINEL_FILE));
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
            "reject hardlinked sentinel",
        );
        assert!(error.contains("multiple links"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn fixed_legacy_sentinel_cannot_arm_publication() {
        let incoming = fixture_dir("legacy-fixed-sentinel");
        write_bytes(&incoming.join(SENTINEL_FILE), b"verified\n");
        let error = must_fail(
            IncomingSnapshot::capture(&incoming),
            "reject fixed legacy sentinel",
        );
        assert!(
            error.contains("producer-owned discovery selection"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&incoming);
    }

    #[test]
    fn discovery_sentinel_binds_candidate_bytes_after_verification() {
        let fixture = discovery_fixture("discovery-sentinel-bytes");
        must(
            verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
            "verify immutable incoming selection",
        );
        must(
            arm_sentinel(&fixture.incoming),
            "arm immutable selection sentinel",
        );
        let original = must(
            std::fs::read(fixture.incoming.join("example-1.2.3-amd64.deb")),
            "read original candidate bytes",
        );
        assert_eq!(original.len(), b"tamperxxx".len());
        write_bytes(
            &fixture.incoming.join("example-1.2.3-amd64.deb"),
            b"tamperxxx",
        );
        let error = must_fail(
            check_sentinel(&fixture.incoming),
            "reject candidate replacement after verification",
        );
        assert!(error.contains("stale"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn discovery_selection_requires_every_manifest_artifact_asset() {
        let fixture = discovery_fixture("discovery-inventory");
        let mut tampered = fixture.document.clone();
        let assets = tampered["release_assets"]
            .as_array_mut()
            .ok_or("fixture release assets array");
        let assets = must(assets, "fixture release assets array");
        assets.retain(|asset| asset["name"] != "example-1.2.3-arm64.deb");
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize incomplete release inventory",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject incomplete release inventory",
        );
        assert!(error.contains("absent from release assets"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn discovery_product_manifest_requires_distinct_linux_apt_targets() {
        let fixture = discovery_fixture("discovery-apt-target-census");
        let mut tampered = fixture.document.clone();
        let artifacts = tampered["manifest"]["artifacts"]
            .as_array_mut()
            .expect("fixture manifest artifacts array");
        let arm = artifacts
            .iter_mut()
            .find(|artifact| artifact["name"] == "example-1.2.3-arm64.deb")
            .expect("fixture arm64 artifact");
        arm["target"] = serde_json::json!("x86_64-unknown-linux-gnu");
        let manifest_bytes = must(
            serde_json::to_vec(&tampered["manifest"]),
            "serialize duplicate Linux target manifest",
        );
        tampered["manifest_sha256"] = serde_json::json!(sha256_hex(&manifest_bytes));
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize duplicate Linux target selection",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject duplicate Linux target census",
        );
        assert!(
            error.contains("APT artifact") || error.contains("APT") || error.contains("census"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[cfg(unix)]
    #[test]
    fn incoming_directory_listing_rejects_non_utf8_entries() {
        let fixture = discovery_fixture("discovery-non-utf8");
        let error = must_fail(
            directory_entry_name(b"invalid-\xff", &fixture.incoming),
            "reject non-UTF-8 incoming entry",
        );
        assert!(error.contains("non-UTF-8"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn incoming_directory_listing_rejects_a_real_non_utf8_entry() {
        let fixture = discovery_fixture("discovery-real-non-utf8");
        let raw_name = OsString::from_vec(b"invalid-\xff".to_vec());
        let raw_path = fixture.incoming.join(&raw_name);
        must(
            std::fs::write(&raw_path, b"fixture"),
            "write invalid UTF-8 entry",
        );
        let error = must_fail(
            dir_names(&fixture.incoming),
            "reject a real non-UTF-8 incoming entry",
        );
        assert!(error.contains("non-UTF-8"), "{error}");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[cfg(unix)]
    #[test]
    fn archive_extracted_parent_symlink_and_hardlink_are_rejected() {
        let root = fixture_dir("archive-parent-boundary");
        let real = root.join("real");
        must(
            std::fs::create_dir(&real),
            "create extracted real directory",
        );
        write_bytes(&real.join("identity.json"), b"{}");

        let symlink_parent = root.join("symlink-parent");
        make_symlink(&real, &symlink_parent);
        assert!(
            require_file(&symlink_parent.join("identity.json")).is_err(),
            "archive verifier must reject a symlinked extracted parent"
        );

        let outside = root.join("outside");
        write_bytes(&outside, b"not a directory");
        let hardlink_parent = root.join("hardlink-parent");
        make_hard_link(&outside, &hardlink_parent);
        assert!(
            require_file(&hardlink_parent.join("identity.json")).is_err(),
            "archive verifier must reject a hardlinked regular parent"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "hostile provenance mutations stay in one auditable fixture"
    )]
    #[test]
    fn discovery_selection_rejects_component_and_release_url_drift() {
        let fixture = discovery_fixture("discovery-identity");
        let mut tampered = fixture.document.clone();
        tampered["manifest"]["components"][0]["binary"] = serde_json::json!("other");
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize component identity tamper",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject component identity tamper",
        );
        assert!(error.contains("component identity"), "{error}");

        tampered = fixture.document.clone();
        tampered["release_url"] = serde_json::json!("https://evil.example/release");
        write_bytes(
            &fixture.selection_path,
            &must(serde_json::to_vec(&tampered), "serialize URL tamper"),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject release URL tamper",
        );
        assert!(error.contains("canonical GitHub release URL"), "{error}");

        tampered = fixture.document.clone();
        tampered["release_id"] = serde_json::json!("bad?release");
        write_bytes(
            &fixture.selection_path,
            &must(serde_json::to_vec(&tampered), "serialize release ID tamper"),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject release ID grammar tamper",
        );
        assert!(error.contains("release_id"), "{error}");

        tampered = fixture.document.clone();
        tampered["release_id"] = serde_json::json!("0123");
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize leading-zero release ID tamper",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject leading-zero release ID",
        );
        assert!(error.contains("release_id"), "{error}");

        tampered = fixture.document.clone();
        tampered["release_id"] = serde_json::json!("124");
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize mismatched release ID tamper",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject mismatched provider release ID",
        );
        assert!(error.contains("provider_release_id"), "{error}");

        tampered = fixture.document.clone();
        tampered["source_ref"] = serde_json::json!("refs/tags/v9.9.9");
        write_bytes(
            &fixture.selection_path,
            &must(serde_json::to_vec(&tampered), "serialize source ref tamper"),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject source ref tamper",
        );
        assert!(
            error.contains("source ref") || error.contains("source-ref"),
            "{error}"
        );

        tampered = fixture.document.clone();
        tampered["source_commit"] = serde_json::json!("f".repeat(40));
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&tampered),
                "serialize source commit tamper",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject source commit tamper",
        );
        assert!(
            error.contains("identity") || error.contains("source-ref"),
            "{error}"
        );

        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&fixture.document),
                "restore selected release",
            ),
        );
        let mut selected_extra = fixture.document.clone();
        let assets = selected_extra["release_assets"]
            .as_array_mut()
            .ok_or("fixture release assets array");
        let assets = must(assets, "fixture release assets array");
        assets.push(serde_json::json!({
            "id": 999,
            "name": "unlisted-extra.tar",
            "size": 1,
            "state": "uploaded",
            "browser_download_url": format!(
                "https://github.com/{FIXTURE_SOURCE}/releases/download/v1.2.3/unlisted-extra.tar"
            )
        }));
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&selected_extra),
                "serialize extra selected asset",
            ),
        );
        let error = must_fail(
            read_discovery_selection(&fixture.selection_path),
            "reject extra selected release asset",
        );
        assert!(error.contains("census"), "{error}");
        write_bytes(
            &fixture.selection_path,
            &must(
                serde_json::to_vec(&fixture.document),
                "restore selected release after census test",
            ),
        );
        let mut persisted = fixture.document.clone();
        let assets = persisted["release_assets"]
            .as_array_mut()
            .ok_or("fixture release assets array");
        let assets = must(assets, "fixture release assets array");
        assets.push(serde_json::json!({
            "id": 999,
            "name": "unlisted-extra.tar",
            "size": 1,
            "state": "uploaded",
            "browser_download_url": format!(
                "https://github.com/{FIXTURE_SOURCE}/releases/download/v1.2.3/unlisted-extra.tar"
            )
        }));
        write_bytes(
            &fixture.incoming.join(DISCOVERY_SELECTION_FILE),
            &must(
                serde_json::to_vec(&persisted),
                "serialize extra persisted asset",
            ),
        );
        let error = must_fail(
            verify_discovery_incoming(&fixture.selection_path, &fixture.incoming),
            "reject extra persisted release asset",
        );
        assert!(
            error.contains("differs from the selected release"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn discovery_manifest_sidecar_requires_canonical_basename() {
        let fixture = discovery_fixture("discovery-manifest-sidecar");
        let manifest_digest =
            field(&fixture.document, "manifest_sha256").expect("fixture manifest digest");
        let sidecar = fixture.incoming.join("product-manifest.json.sha256");
        for contents in [
            format!("{manifest_digest}\n"),
            format!("{manifest_digest}  product-assets/product-manifest.json\n"),
            format!("{manifest_digest}  product-manifest.json\nextra\n"),
        ] {
            write_bytes(&sidecar, contents.as_bytes());
            let error = must_fail(
                product_manifest_sidecar_digest(&sidecar),
                "reject noncanonical product manifest sidecar",
            );
            assert!(error.contains("digest row"), "{error}");
        }
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn preview_source_proof_must_bind_main_ancestry() {
        let tag = format!("preview-{FIXTURE_COMMIT}");
        let proof = serde_json::json!({
            "method": "github-git-ref",
            "proof_ref": format!("refs/tags/{tag}"),
            "resolved_commit": FIXTURE_COMMIT,
            "declared_ref_provenance": {
                "base_commit": "fedcba9876543210fedcba9876543210fedcba98",
                "head_commit": FIXTURE_COMMIT,
                "merge_base_commit": FIXTURE_COMMIT,
                "method": "github-compare-ancestry",
                "ref": PREVIEW_SOURCE_REF,
                "relation": "ancestor",
                "status": "behind"
            }
        });
        let valid = serde_json::json!({"source_ref_resolution": proof});
        must(
            validate_source_ref_resolution(&valid, "preview", &tag, FIXTURE_COMMIT),
            "accept proven preview ancestry",
        );
        let mut tampered = valid;
        tampered["source_ref_resolution"]["declared_ref_provenance"]["merge_base_commit"] =
            serde_json::json!("fedcba9876543210fedcba9876543210fedcba98");
        let error = must_fail(
            validate_source_ref_resolution(&tampered, "preview", &tag, FIXTURE_COMMIT),
            "reject unproven preview branch",
        );
        assert!(error.contains("main ancestry"), "{error}");
    }

    fn apt_spec() -> ReleaseSpec {
        ReleaseSpec {
            kind: "apt".to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            packages: Vec::new(),
            binary: FIXTURE_BINARY.to_owned(),
            targets: Vec::new(),
            image: String::new(),
            image_package: String::new(),
            source_repository: FIXTURE_SOURCE.to_owned(),
            consumer_repository: "example/feed".to_owned(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            apt_arches: Vec::new(),
            signer_fingerprint: FIXTURE_FPR.to_owned(),
            passphrase_secret: "B1_TEST_PASSPHRASE".to_owned(),
            signing_key_secret: "B1_TEST_SIGNING_KEY".to_owned(),
            keyring_path: String::new(),
            apt_origin: String::new(),
            apt_identity_dir: String::new(),
            apt_feed_url: "https://feed.example.test".to_owned(),
            retention: 0,
            dockerfile: String::new(),
            context: String::new(),
            platforms: Vec::new(),
            producer_workflow: String::new(),
            producer_conclusion: String::new(),
            modes: Vec::new(),
            archive_members: Vec::new(),
            archive_checksum: String::new(),
            archive_retention_days: 0,
            credentials: Vec::new(),
            tag_pattern: String::new(),
            registry: String::new(),
            registry_username_secret: String::new(),
            registry_password_secret: String::new(),
            jobs: Vec::new(),
        }
    }

    fn apt_contract() -> AptContract {
        must(
            AptContract::resolve(&apt_spec()),
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
    fn gpgv_status_requires_one_pinned_primary() {
        // This is the exact VALIDSIG field shape emitted by gpgv for a real
        // signing subkey: the subkey fingerprint is first and its primary
        // fingerprint is the final field.
        const SUBKEY: &str = "11223344556677889900AABBCCDDEEFF00112233";
        const STATUS_FIELDS: &str = "20260920 1789883126 0 4 0 22 10 00";
        let valid_subkey =
            format!("[GNUPG:] NEWSIG\n[GNUPG:] VALIDSIG {SUBKEY} {STATUS_FIELDS} {FIXTURE_FPR}\n");
        must(
            gpgv_signer(valid_subkey.as_bytes(), FIXTURE_FPR, "record"),
            "accept signing subkey bound to pinned primary",
        );
        let valid_primary = format!(
            "[GNUPG:] NEWSIG\n[GNUPG:] VALIDSIG {FIXTURE_FPR} {STATUS_FIELDS} {FIXTURE_FPR}\n"
        );
        must(
            gpgv_signer(valid_primary.as_bytes(), FIXTURE_FPR, "record"),
            "accept direct primary signature",
        );

        let foreign_primary = format!(
            "[GNUPG:] VALIDSIG {SUBKEY} {STATUS_FIELDS} FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF\n"
        );
        let error = must_fail(
            gpgv_signer(foreign_primary.as_bytes(), FIXTURE_FPR, "record"),
            "reject foreign primary",
        );
        assert!(error.contains("does not match"), "{error}");
        let duplicate = format!("{valid_subkey}{valid_primary}");
        let error = must_fail(
            gpgv_signer(duplicate.as_bytes(), FIXTURE_FPR, "record"),
            "reject duplicate signer status",
        );
        assert!(error.contains("does not match"), "{error}");
        let malformed = format!("[GNUPG:] VALIDSIG {FIXTURE_FPR}\n");
        let error = must_fail(
            gpgv_signer(malformed.as_bytes(), FIXTURE_FPR, "record"),
            "reject malformed signer status",
        );
        assert!(error.contains("field count"), "{error}");
        let error = must_fail(
            gpgv_signer(b"\xff", FIXTURE_FPR, "record"),
            "reject non-UTF-8 signer status",
        );
        assert!(error.contains("not UTF-8"), "{error}");
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
            "", "1.2.3", "v1.2", "v1.2.3.4", "vv1.2.3", "v1.2.x", "v 1.2.3", "v01.2.3", "v1.02.3",
            "v1.2.03",
        ] {
            let error = must_fail(parse_stable_tag(invalid), "bad stable tag");
            assert!(error.contains("vX.Y.Z"), "{error}");
        }
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
            "01.2.3-preview.1+0123456",
            "1.02.3-preview.1+0123456",
            "1.2.03-preview.1+0123456",
            "1.2.3-preview.01+0123456",
        ] {
            let error = must_fail(
                parse_product_preview_version(invalid),
                "bad product preview version",
            );
            assert!(error.contains("X.Y.Z-preview.N"), "{error}");
        }
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
            "1.02.3~preview.41+0123456",
            "1.2.03~preview.41+0123456",
            "1.2.3~preview.01+0123456",
        ] {
            let error = must_fail(parse_preview_version(invalid), "bad preview version");
            assert!(error.contains("X.Y.Z~preview.N"), "{error}");
        }
        assert_eq!(
            dotted_asset_version("1.2.3~preview.41+0123456"),
            "1.2.3.preview.41+0123456"
        );
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
        assert_eq!(contract.keyring, "example.gpg");
        assert_eq!(contract.origin, FIXTURE_PACKAGE);
        assert_eq!(contract.identity_dir, FIXTURE_IDENTITY);
        assert_eq!(contract.feed_url, "https://feed.example.test");
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
        spec.retention = 1;
        let contract = must(AptContract::resolve(&spec), "resolve explicit");
        assert_eq!(contract.signing_key_secret, "B1_TEST_SIGNING_KEY");
        assert_eq!(contract.arches, ["amd64".to_owned(), "arm64".to_owned()]);
        assert_eq!(contract.keyring, "keys/feed.gpg");
        assert_eq!(contract.origin, "Example Feed");
        assert_eq!(contract.identity_dir, "example-id");
        assert_eq!(contract.description, "Custom feed description");
        assert_eq!(contract.signer, FIXTURE_FPR);
    }

    type SpecMutation = Box<dyn Fn(&mut ReleaseSpec)>;

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
            let error = must_fail(AptContract::resolve(&spec), name);
            assert!(!error.is_empty(), "{name}");
        }
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

    #[cfg(unix)]
    fn make_deb_with_parent_entry(dir: &Path, name: &str, symlink_parent: bool) -> PathBuf {
        let work = dir.join(format!(".debwork-{name}"));
        let control_dir = work.join("control");
        let data_dir = work.join("data");
        must(
            std::fs::create_dir_all(&control_dir),
            "malicious control dir",
        );
        must(
            std::fs::create_dir_all(data_dir.join("usr/share")),
            "malicious share dir",
        );
        write_bytes(
            &control_dir.join("control"),
            b"Package: example\nVersion: 1.2.3\nArchitecture: amd64\nMaintainer: Fixture <fixture@example.test>\nDescription: fixture\n",
        );
        if symlink_parent {
            write_bytes(
                &data_dir.join("outside/build-identity.json"),
                b"outside identity\n",
            );
            must(
                std::os::unix::fs::symlink("../../outside", data_dir.join("usr/share/app")),
                "create malicious archive parent symlink",
            );
        } else {
            write_bytes(&data_dir.join("usr/share/reference"), b"not a directory\n");
            must(
                std::fs::hard_link(
                    data_dir.join("usr/share/reference"),
                    data_dir.join("usr/share/app"),
                ),
                "create malicious archive parent hard link",
            );
        }
        for (member, source) in [("control.tar.gz", &control_dir), ("data.tar.gz", &data_dir)] {
            let status = must(
                std::process::Command::new("tar")
                    .args(["-czf"])
                    .arg(work.join(member))
                    .args(["-C"])
                    .arg(source)
                    .arg(".")
                    .status(),
                "tar malicious deb member",
            );
            assert!(status.success(), "tar {member} failed");
        }
        write_bytes(&work.join("debian-binary"), b"2.0\n");
        let deb = dir.join(name);
        let status = must(
            std::process::Command::new("ar")
                .args(["rcS"])
                .arg(&deb)
                .arg(work.join("debian-binary"))
                .arg(work.join("control.tar.gz"))
                .arg(work.join("data.tar.gz"))
                .status(),
            "ar malicious deb",
        );
        assert!(status.success(), "ar {name} failed");
        must(
            std::fs::remove_dir_all(&work),
            "clean malicious deb workdir",
        );
        deb
    }

    struct StableIncoming {
        dir: PathBuf,
        selection: DiscoverySelection,
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
        attach_product_selection(
            &dir, "stable", source, package, binary, identity, &version, &commit, &tag,
        );
        let selection = must(
            read_discovery_selection(&dir.join(DISCOVERY_SELECTION_FILE)),
            "read stable fixture selection",
        );
        StableIncoming { dir, selection }
    }

    fn stable_verify_inputs(incoming: &StableIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Stable,
            selection: &incoming.selection,
            binary: FIXTURE_BINARY.to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            identity_dir: FIXTURE_IDENTITY.to_owned(),
            incoming: &incoming.dir,
            signer_live: FIXTURE_FPR.to_owned(),
            signer_pinned: FIXTURE_FPR.to_owned(),
            verify_oci: false,
            backend: DebBackend::Auto,
            path_overlay: None,
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

    #[cfg(unix)]
    #[test]
    fn extracted_archive_symlink_and_hardlink_parents_fail_closed() {
        for (name, symlink_parent) in [
            ("example-parent-symlink.deb", true),
            ("example-parent-hardlink.deb", false),
        ] {
            let dir = fixture_dir(if symlink_parent {
                "deb-parent-symlink"
            } else {
                "deb-parent-hardlink"
            });
            let deb = make_deb_with_parent_entry(&dir, name, symlink_parent);
            let extract = dir.join("extracted");
            let error = must_fail(
                deb_extract_data(&deb, &extract, DebBackend::ArTar, None),
                "reject malicious parent archive",
            );
            assert!(error.contains("member type"), "{error}");
            assert!(
                !extract.exists(),
                "unsafe archive created an extraction root"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn archive_extraction_rejects_a_symlink_destination() {
        let dir = fixture_dir("deb-destination-symlink");
        let deb = make_deb(
            &dir,
            "example-destination-symlink.deb",
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
        let outside = dir.join("outside");
        must(std::fs::create_dir(&outside), "create extraction outside");
        write_bytes(&outside.join("marker"), b"untouched");
        let destination = dir.join("extracted");
        make_symlink(&outside, &destination);
        let error = must_fail(
            deb_extract_data(&deb, &destination, DebBackend::ArTar, None),
            "reject symlink extraction destination",
        );
        assert!(error.contains("destination already exists"), "{error}");
        assert_eq!(
            must(std::fs::read(outside.join("marker")), "read marker"),
            b"untouched"
        );
        assert!(!outside.join("usr").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn tar_payload_with_member(name: &str) -> Vec<u8> {
        let mut header = [0_u8; 512];
        assert!(name.len() < 100);
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000001\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        let mut payload = header.to_vec();
        payload.push(b'x');
        payload.extend(std::iter::repeat_n(0, 511));
        payload.extend(std::iter::repeat_n(0, 1024));
        payload
    }

    #[cfg(unix)]
    #[test]
    fn archive_member_parent_traversal_is_rejected_before_extraction() {
        let payload = tar_payload_with_member("../escaped");
        let error = must_fail(
            validate_tar_payload(&payload, None),
            "reject traversal archive member",
        );
        assert!(error.contains("not confined"), "{error}");
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

    struct PreviewIncoming {
        dir: PathBuf,
        selection: DiscoverySelection,
    }

    /// Build a coherent preview incoming directory for the default fixture
    /// identity.
    fn preview_incoming(root: &str) -> PreviewIncoming {
        preview_incoming_renamed(
            root,
            FIXTURE_SOURCE,
            FIXTURE_PACKAGE,
            FIXTURE_BINARY,
            FIXTURE_IDENTITY,
        )
    }

    fn preview_incoming_renamed(
        root: &str,
        source: &str,
        package: &str,
        binary: &str,
        identity: &str,
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
            "{{\"schema\": \"{FIXTURE_SCHEMA}\", \"source_repository\": \"{source}\", \"source_ref\": \"{PREVIEW_SOURCE_REF}\", \"source_commit\": \"{commit}\", \"version\": \"{version}\", \"assets\": [{}]}}\n",
            assets.join(", ")
        );
        write_bytes(&dir.join(PREVIEW_MANIFEST_FILE), manifest.as_bytes());
        write_bytes(
            &dir.join(SHA256SUMS_FILE),
            format!("{}\n", sums.join("\n")).as_bytes(),
        );
        attach_product_selection(
            &dir,
            "preview",
            source,
            package,
            binary,
            identity,
            &version,
            &commit,
            &format!("preview-{commit}"),
        );
        let selection = must(
            read_discovery_selection(&dir.join(DISCOVERY_SELECTION_FILE)),
            "read preview fixture selection",
        );
        PreviewIncoming { dir, selection }
    }

    fn preview_verify_inputs(incoming: &PreviewIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Preview,
            selection: &incoming.selection,
            binary: FIXTURE_BINARY.to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            identity_dir: FIXTURE_IDENTITY.to_owned(),
            incoming: &incoming.dir,
            signer_live: FIXTURE_FPR.to_owned(),
            signer_pinned: FIXTURE_FPR.to_owned(),
            verify_oci: false,
            backend: DebBackend::Auto,
            path_overlay: None,
        }
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
    fn preview_verify_rejects_identity_and_grammar_defects() {
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
                if line.ends_with(tilde) || line.ends_with(name) {
                    let pinned_name = line.split_whitespace().nth(1).unwrap_or(name);
                    format!("{deb_sha}  {pinned_name}")
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
                if matches!(asset["name"].as_str(), Some(value) if value == tilde || value == name)
                {
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
        make_executable(&bin.join("apt-ftparchive"));
        make_executable(&bin.join("gpg"));
        make_executable(&bin.join("gpgconf"));
        ToolStubs { bin, log }
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

    fn live_packages(
        suite: Suite,
        package: &str,
        arch: &str,
        candidate: &str,
        rollback: &str,
    ) -> String {
        let prefix = match suite {
            Suite::Stable => "pool/main",
            Suite::Preview => "pool/preview/main",
        };
        let stanza = |version: &str, digest: &str| {
            let name = canonical_pool_name(package, version, arch);
            format!(
                "Package: {package}\nVersion: {version}\nArchitecture: {arch}\nFilename: {prefix}/e/{package}/{name}\nSize: 17\nSHA256: {digest}\nDescription: test\n continuation\n"
            )
        };
        format!(
            "{}\n{}",
            stanza(candidate, &"aa".repeat(32)),
            stanza(rollback, &"bb".repeat(32))
        )
    }

    #[test]
    fn live_packages_bind_exact_rollback_identity_for_both_arches() {
        let pointer = serde_json::json!({
            "tag": "v1.2.2",
            "source_record_sha256": "cc".repeat(32),
            ROLLBACK_PACKAGES_FIELD: [
                {"name": "example_1.2.2_amd64.deb", "sha256": "dd".repeat(32)},
                {"name": "example_1.2.2_arm64.deb", "sha256": "dd".repeat(32)}
            ]
        });
        let bound = must(
            bind_live_rollback_packages(
                &pointer,
                Suite::Stable,
                "example",
                "1.2.3",
                Some("1.2.2"),
                &live_packages(Suite::Stable, "example", "amd64", "1.2.3", "1.2.2"),
                &live_packages(Suite::Stable, "example", "arm64", "1.2.3", "1.2.2"),
            ),
            "bind live rollback identity",
        );
        assert_eq!(
            bound
                .get(ROLLBACK_PACKAGES_FIELD)
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(REQUIRED_ARCHES.len())
        );
        assert_eq!(
            bound[ROLLBACK_PACKAGES_FIELD][0]["name"],
            "example_1.2.2_amd64.deb"
        );
        assert_eq!(
            bound[ROLLBACK_PACKAGES_FIELD][0]["sha256"],
            serde_json::Value::String("bb".repeat(32))
        );
        assert_eq!(
            bound[ROLLBACK_PACKAGES_FIELD][1]["name"],
            "example_1.2.2_arm64.deb"
        );
    }

    #[test]
    fn live_packages_reject_path_or_digest_tampering() {
        let pointer = serde_json::json!({
            "tag": PREVIEW_TAG,
            ROLLBACK_PACKAGES_FIELD: []
        });
        let mut hostile = live_packages(
            Suite::Preview,
            "example",
            "amd64",
            PREVIEW_CANDIDATE,
            PREVIEW_ROLLBACK,
        );
        hostile = hostile.replace(
            "Filename: pool/preview/main/e/example/example_1.2.3~preview.40+abcdef0_amd64.deb",
            "Filename: pool/preview/main/e/example/../escape.deb",
        );
        let error = must_fail(
            bind_live_rollback_packages(
                &pointer,
                Suite::Preview,
                "example",
                PREVIEW_CANDIDATE,
                None,
                &hostile,
                &live_packages(
                    Suite::Preview,
                    "example",
                    "arm64",
                    PREVIEW_CANDIDATE,
                    PREVIEW_ROLLBACK,
                ),
            ),
            "reject hostile live path",
        );
        assert!(error.contains("canonical pool identity"), "{error}");

        let mut hostile = live_packages(
            Suite::Preview,
            "example",
            "amd64",
            PREVIEW_CANDIDATE,
            PREVIEW_ROLLBACK,
        );
        hostile = hostile.replace(&"bb".repeat(32), &"zz".repeat(32));
        let error = must_fail(
            bind_live_rollback_packages(
                &pointer,
                Suite::Preview,
                "example",
                PREVIEW_CANDIDATE,
                None,
                &hostile,
                &live_packages(
                    Suite::Preview,
                    "example",
                    "arm64",
                    PREVIEW_CANDIDATE,
                    PREVIEW_ROLLBACK,
                ),
            ),
            "reject hostile live digest",
        );
        assert!(error.contains("SHA256"), "{error}");
    }

    #[test]
    fn live_packages_reject_legacy_preview_pointer_and_mixed_versions() {
        let amd64 = live_packages(
            Suite::Preview,
            "example",
            "amd64",
            PREVIEW_CANDIDATE,
            PREVIEW_ROLLBACK,
        );
        let arm64 = live_packages(
            Suite::Preview,
            "example",
            "arm64",
            PREVIEW_CANDIDATE,
            PREVIEW_ROLLBACK,
        );
        let error = must_fail(
            bind_live_rollback_packages(
                &serde_json::json!(PREVIEW_TAG),
                Suite::Preview,
                "example",
                PREVIEW_CANDIDATE,
                None,
                &amd64,
                &arm64,
            ),
            "reject legacy preview pointer",
        );
        assert!(
            error.contains("not an object")
                || error.contains("object form")
                || error.contains("unexpected field"),
            "{error}"
        );

        let error = must_fail(
            bind_live_rollback_packages(
                &serde_json::json!({
                    "tag": PREVIEW_TAG,
                    ROLLBACK_PACKAGES_FIELD: []
                }),
                Suite::Preview,
                "example",
                PREVIEW_CANDIDATE,
                None,
                &amd64,
                &live_packages(
                    Suite::Preview,
                    "example",
                    "arm64",
                    PREVIEW_CANDIDATE,
                    "1.2.3~preview.39+abcdef0",
                ),
            ),
            "reject mixed live rollback versions",
        );
        assert!(error.contains("differ by architecture"), "{error}");
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
        let rollback_packages = REQUIRED_ARCHES
            .iter()
            .map(|arch| {
                let name = format!("{FIXTURE_PACKAGE}-1.2.2-{arch}.deb");
                serde_json::json!({
                    "name": name,
                    "sha256": must(
                        sha256_file(&root.join("prev").join(&name)),
                        "hash rollback package"
                    )
                })
            })
            .collect::<Vec<_>>();
        let pointer = serde_json::json!({
            "tag": tag,
            "source_record_sha256": "dd".repeat(32),
            ROLLBACK_PACKAGES_FIELD: rollback_packages
        });
        write_bytes(
            &root.join("previous-pointer.json"),
            format!(
                "{}\n",
                must(
                    serde_json::to_string(&pointer),
                    "serialize previous pointer"
                )
            )
            .as_bytes(),
        );
    }

    fn preview_pointer_file(root: &Path) {
        let rollback_packages = REQUIRED_ARCHES
            .iter()
            .map(|arch| {
                let name = format!("example_{PREVIEW_ROLLBACK}_{arch}.deb");
                serde_json::json!({
                    "name": name,
                    "sha256": must(
                        sha256_file(&root.join("prev").join(&name)),
                        "hash preview rollback package"
                    )
                })
            })
            .collect::<Vec<_>>();
        let pointer = serde_json::json!({
            "tag": PREVIEW_TAG,
            ROLLBACK_PACKAGES_FIELD: rollback_packages
        });
        write_bytes(
            &root.join("previous-pointer.json"),
            format!(
                "{}\n",
                must(serde_json::to_string(&pointer), "serialize preview pointer")
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
        let passphrase_env = contract.passphrase_secret.clone();
        let key_env = contract.signing_key_secret.clone();
        let selection_path = Box::leak(incoming.join(DISCOVERY_SELECTION_FILE).into_boxed_path());
        let selection = Box::leak(Box::new(must(
            read_discovery_selection(selection_path),
            "read fixture discovery selection",
        )));
        PublishInputs {
            suite,
            contract,
            version: version.to_owned(),
            incoming,
            prev_dir,
            previous_pointer,
            staging,
            bootstrap,
            passphrase_env,
            passphrase: Some("fixture-passphrase-value".to_owned()),
            key_env,
            key_material: Some("fixture-key-material".to_owned()),
            backend: DebBackend::Auto,
            path_overlay,
            selection,
            selection_path,
        }
    }

    #[test]
    fn publication_snapshot_freezes_pointer_and_rollback_bytes() {
        let incoming = stable_incoming("snapshot-controls-incoming");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify snapshot controls incoming",
        );
        let root = fixture_dir("snapshot-controls-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        let contract = apt_contract();
        let inputs = publish_inputs(
            Suite::Stable,
            contract,
            "v1.2.3",
            &incoming.dir,
            Some(&prev),
            Path::new("previous-pointer.json"),
            Path::new("public"),
            false,
            None,
        );
        in_fixture_root(&root, || {
            let incoming_snapshot = must(
                IncomingSnapshot::capture(inputs.incoming),
                "capture incoming for controls",
            );
            let snapshot = must(
                PublicationSnapshot::capture(&inputs, incoming_snapshot),
                "capture publication controls",
            );
            let pointer_before = snapshot.previous_pointer.clone();
            let rollback_name = snapshot
                .retained_debs
                .keys()
                .next()
                .cloned()
                .expect("rollback package");
            let rollback_before = snapshot
                .retained_debs
                .get(&rollback_name)
                .cloned()
                .expect("rollback bytes");

            let replacement = serde_json::json!({
                "tag": "v9.9.9",
                "source_record_sha256": "dd".repeat(32)
            });
            write_bytes(
                Path::new("previous-pointer.json"),
                format!(
                    "{}\n",
                    must(serde_json::to_string(&replacement), "serialize pointer")
                )
                .as_bytes(),
            );
            let mut same_size = rollback_before.clone();
            if let Some(first) = same_size.first_mut() {
                *first ^= 0xff;
            }
            write_bytes(&prev.join(&rollback_name), &same_size);

            assert_eq!(snapshot.previous_pointer, pointer_before);
            assert_eq!(
                snapshot.retained_debs.get(&rollback_name),
                Some(&rollback_before)
            );
            assert_ne!(
                must(
                    std::fs::read("previous-pointer.json"),
                    "read replacement pointer"
                ),
                must(
                    serde_json::to_vec(&pointer_before),
                    "serialize original pointer"
                )
            );
            assert_ne!(
                must(
                    std::fs::read(prev.join(&rollback_name)),
                    "read replacement rollback"
                ),
                rollback_before
            );
        });
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publication_rejects_rollback_bytes_without_external_digest_match() {
        let incoming = stable_incoming("rollback-digest-incoming");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify rollback digest incoming",
        );
        let root = fixture_dir("rollback-digest-root");
        let prev = rollback_prev_dir(&root, "1.2.2", FIXTURE_COMMIT);
        stable_pointer_file(&root, "v1.2.2");
        let rollback = prev.join("example-1.2.2-arm64.deb");
        let mut tampered = must(std::fs::read(&rollback), "read rollback package");
        tampered[0] ^= 0xff;
        write_bytes(&rollback, &tampered);
        let stubs = tool_stubs("rollback-digest-tools");
        let contract = apt_contract();
        let inputs = publish_inputs(
            Suite::Stable,
            contract,
            "v1.2.3",
            &incoming.dir,
            Some(&prev),
            Path::new("previous-pointer.json"),
            Path::new("public"),
            false,
            Some(&stubs.bin),
        );
        let error = in_fixture_root(&root, || {
            must_fail(
                publish_suite(&inputs),
                "reject rollback bytes without external digest match",
            )
        });
        assert!(error.contains("signed/live identity"), "{error}");
        assert!(
            !root.join("public").exists(),
            "digest mismatch must fail before staging"
        );
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
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
        // Stable wipes the staging tree: prove it by planting junk.
        write_bytes(&root.join("public/junk"), b"junk");
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
        assert!(!root.join("public/junk").exists(), "stable wipes staging");
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
        let contract = must(AptContract::resolve(&spec), "resolve");
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
        // Pool without the rollback pair: two debs, not four.
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
        assert!(error.contains("coherent previous pointer"), "{error}");
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
                "rollback versions differ",
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
    fn stable_publish_rejects_pool_collisions() {
        // Prior bytes colliding with different candidate bytes under one name.
        let incoming = stable_incoming("pub-collision");
        must(
            verify_suite(&stable_verify_inputs(&incoming)),
            "verify first",
        );
        let stubs = tool_stubs("pub-collision-tools");
        let root = fixture_dir("pub-collision-root");
        let prev = root.join("prev");
        must(std::fs::create_dir_all(&prev), "prev dir");
        must(
            std::fs::copy(
                incoming.dir.join("example-1.2.3-amd64.deb"),
                prev.join("example-1.2.2-amd64.deb"),
            ),
            "rollback amd64",
        );
        must(
            std::fs::copy(
                incoming.dir.join("example-1.2.3-arm64.deb"),
                prev.join("example-1.2.2-arm64.deb"),
            ),
            "rollback arm64",
        );
        must(
            std::fs::copy(
                incoming.dir.join("example-1.2.3-arm64.deb"),
                prev.join("example-1.2.3-arm64.deb"),
            ),
            "candidate collision name",
        );
        // Corrupt the unexpected candidate-named copy.
        must(
            std::fs::write(prev.join("example-1.2.3-arm64.deb"), b"different-bytes"),
            "corrupt the collision",
        );
        stable_pointer_file(&root, "v1.2.2");
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
        assert!(error.contains("unexpected package entry"), "{error}");
        let _ = std::fs::remove_dir_all(&incoming.dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    const PREVIEW_CANDIDATE: &str = "1.2.3~preview.41+0123456";
    const PREVIEW_ROLLBACK: &str = "1.2.3~preview.40+abcdef0";

    fn preview_prev_dir(root: &Path) -> PathBuf {
        let prev = root.join("prev");
        must(std::fs::create_dir_all(&prev), "prev dir");
        for arch in REQUIRED_ARCHES {
            make_deb(
                &prev,
                &format!("example_{PREVIEW_ROLLBACK}_{arch}.deb"),
                FIXTURE_PACKAGE,
                PREVIEW_ROLLBACK,
                arch,
                FIXTURE_BINARY,
                FIXTURE_IDENTITY,
                "abcdef0123456789abcdef0123456789abcdef01",
                "1.2.3",
                b"{}",
                b"rollback-daemon-bytes",
            );
        }
        prev
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
        preview_pointer_file(&root);
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
        assert!(
            root.join("public/dists/stable/Release").is_file(),
            "preview never wipes the shared tree"
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
            parsed
                .previous
                .get("tag")
                .and_then(serde_json::Value::as_str),
            Some(PREVIEW_TAG)
        );
        assert_eq!(
            parsed
                .previous
                .get(ROLLBACK_PACKAGES_FIELD)
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(REQUIRED_ARCHES.len())
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
        let prev = preview_prev_dir(&root);
        preview_pointer_file(&root);
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
        let rollback_packages = serde_json::json!([
            {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
            {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
        ]);
        let deb_packages = serde_json::json!([
            {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
            {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
            {"name": "example_1.2.1_amd64.deb", "sha256": "55".repeat(32)},
            {"name": "example_1.2.1_arm64.deb", "sha256": "66".repeat(32)},
        ]);
        // The published record already identifies the prior tag.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.2",
            "source_record_sha256": prior_sha,
            "deb_packages": deb_packages,
        });
        let pointer = must(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "prior case",
        );
        assert_eq!(
            pointer,
            serde_json::json!({
                "tag": "v1.2.2",
                "source_record_sha256": prior_sha,
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            })
        );
        // The published record identifies the candidate: the pointer is its
        // recorded rollback once the bytes agree.
        let published = serde_json::json!({
            "schema": PUBLICATION_RECORD_SCHEMA,
            "tag": "v1.2.3",
            "source_record_sha256": candidate_sha,
            "previous": {
                "tag": "v1.2.2",
                "source_record_sha256": prior_sha,
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            },
        });
        let pointer = must(
            derive_previous_pointer(&published, "v1.2.2", "v1.2.3", &candidate_sha),
            "candidate case",
        );
        assert_eq!(
            pointer,
            serde_json::json!({
                "tag": "v1.2.2",
                "source_record_sha256": prior_sha,
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            })
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
            "previous": {
                "tag": "v1.2.2",
                "source_record_sha256": prior_sha,
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            },
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
            "previous": {
                "tag": "v1.2.0",
                "source_record_sha256": prior_sha,
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            },
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
            "previous": {
                "tag": "v1.2.2",
                "source_record_sha256": "short",
                ROLLBACK_PACKAGES_FIELD: rollback_packages
            },
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
            "deb_packages": [
                {"name": "example_1.2.3_amd64.deb", "sha256": "11".repeat(32)},
                {"name": "example_1.2.3_arm64.deb", "sha256": "22".repeat(32)},
                {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
                {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
            ],
            "signer_fingerprint": FIXTURE_FPR,
            "previous": {
                "tag": "v1.2.2",
                "source_record_sha256": "dd".repeat(32),
                "rollback_packages": [
                    {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
                    {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
                ],
            },
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
            "deb_packages": [
                {"name": "example_1.2.3_amd64.deb", "sha256": "11".repeat(32)},
                {"name": "example_1.2.3_arm64.deb", "sha256": "22".repeat(32)},
                {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
                {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
            ],
            "signer_fingerprint": FIXTURE_FPR,
            "previous": {
                "tag": "preview",
                "rollback_packages": [
                    {"name": "example_1.2.2_amd64.deb", "sha256": "33".repeat(32)},
                    {"name": "example_1.2.2_arm64.deb", "sha256": "44".repeat(32)},
                ],
            },
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
    fn deploy_guard_refuses_older_publications_only() {
        // Stable: older refuses, equal and newer deploy, first deploy deploys.
        let error = must_fail(
            check_deploy_guard(Suite::Stable, "v1.2.2\n", Some("v1.2.3\n")),
            "stable rollback",
        );
        assert!(error.contains("refusing to roll back"), "{error}");
        must(
            check_deploy_guard(Suite::Stable, "v1.2.3\n", Some("v1.2.3\n")),
            "stable redeploy",
        );
        must(
            check_deploy_guard(Suite::Stable, "v1.2.4\n", Some("v1.2.3\n")),
            "stable forward",
        );
        must(
            check_deploy_guard(Suite::Stable, "v1.2.3\n", None),
            "stable first deploy",
        );
        must(
            check_deploy_guard(Suite::Stable, "v1.2.3\n", Some("  \n")),
            "stable blank live",
        );
        let error = must_fail(
            check_deploy_guard(Suite::Stable, "  \n", None),
            "empty staged",
        );
        assert!(error.contains("staged last-publish is empty"), "{error}");
        let error = must_fail(
            check_deploy_guard(Suite::Stable, "not-a-version", Some("v1.2.3")),
            "malformed staged",
        );
        assert!(error.contains("vX.Y.Z"), "{error}");
        // Preview: dpkg order governs.
        let error = must_fail(
            check_deploy_guard(Suite::Preview, PREVIEW_ROLLBACK, Some(PREVIEW_CANDIDATE)),
            "preview rollback",
        );
        assert!(error.contains("refusing to roll back"), "{error}");
        must(
            check_deploy_guard(Suite::Preview, PREVIEW_CANDIDATE, Some(PREVIEW_CANDIDATE)),
            "preview redeploy",
        );
        must(
            check_deploy_guard(Suite::Preview, PREVIEW_CANDIDATE, Some(PREVIEW_ROLLBACK)),
            "preview forward",
        );
        must(
            check_deploy_guard(Suite::Preview, PREVIEW_CANDIDATE, None),
            "preview first deploy",
        );
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

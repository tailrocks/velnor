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
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::{GeneratorError, ReleaseSpec};

/// The release-record schema the coherence chain authenticates.
pub(crate) const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";
/// The publication-record schema staged publications emit.
pub(crate) const PUBLICATION_RECORD_SCHEMA: &str = "velnor.publication-record/v1";
/// The channel-state schema channel updates emit.
pub(crate) const PACKAGE_STATE_SCHEMA: &str = "velnor.apt-package-state.v1";
/// The rolling release tag that carries preview coherence inputs.
pub(crate) const PREVIEW_TAG: &str = "preview";
/// The only source ref a preview manifest may name.
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
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
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
    if seq.is_empty()
        || !seq.bytes().all(|byte| byte.is_ascii_digit())
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
        if spec.canonical_manifest_schema.is_empty()
            || spec.canonical_manifest_schema.contains(char::is_whitespace)
        {
            return Err(GeneratorError::usage(
                "apt canonical_manifest_schema must be a non-empty schema URN without whitespace",
            ));
        }
        contract.discovery_script = spec.discovery_script.clone();
        contract.canonical_manifest_asset = spec.canonical_manifest_asset.clone();
        contract.canonical_manifest_schema = spec.canonical_manifest_schema.clone();
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
        let text = String::from_utf8_lossy(&stdout);
        if let Some(commit) = text.split_whitespace().next()
            && valid_commit(commit)
        {
            return Ok(commit.to_owned());
        }
    }
    Err(GeneratorError::usage(format!(
        "could not resolve {tag} to a commit on {source_repo}"
    )))
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
        Suite::Preview => {
            parse_preview_version(version)?;
            PREVIEW_TAG.to_owned()
        }
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
    std::fs::write(inputs.incoming.join(SENTINEL_FILE), []).map_err(|error| {
        GeneratorError::io(
            "arm the verification sentinel",
            &inputs.incoming.join(SENTINEL_FILE),
            &error,
        )
    })?;
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
    let commit = match &inputs.commit {
        Some(commit) if !commit.is_empty() => {
            if !valid_commit(commit) {
                return Err(GeneratorError::usage(
                    "resolved commit is not 40 lowercase hex characters",
                ));
            }
            commit.clone()
        }
        _ => run_resolve_commit(&inputs.source_repo, &tag.tag, inputs.path_overlay)?,
    };
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
    if field(&manifest, "source_ref")? != PREVIEW_SOURCE_REF {
        return Err(GeneratorError::usage(format!(
            "release-manifest source_ref is not {PREVIEW_SOURCE_REF}"
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
}

/// Publish a suite into the staging tree: deterministic pool, per-arch
/// indexes, signed metadata, publication record. Every refusal below lands
/// before signing, and stable wipes only the validated staging directory.
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
    if !inputs.incoming.join(SENTINEL_FILE).is_file() {
        return Err(GeneratorError::usage(
            "publish: refusing — verify has not armed the reprepro sentinel",
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
        Suite::Stable => publish_stable(inputs, passphrase, homedir_name),
        Suite::Preview => publish_preview(inputs, passphrase, homedir_name),
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

/// Stage every `*.deb` directly inside `dir` whose name starts with the
/// package prefix, skipping names the candidate already carries (after a
/// byte-equality check).
fn stage_dir_debs(
    dir: &Path,
    pool: &Path,
    incoming: &Path,
    contract: &AptContract,
    backend: DebBackend,
    path_overlay: Option<&Path>,
) -> Result<(), GeneratorError> {
    for name in dir_names(dir)? {
        if !name.starts_with(&contract.package) || !is_deb_file(&name) {
            continue;
        }
        let deb = dir.join(&name);
        if !deb.is_file() {
            continue;
        }
        let candidate = incoming.join(&name);
        if candidate.is_file() {
            if sha256_file(&candidate)? != sha256_file(&deb)? {
                return Err(GeneratorError::usage(format!(
                    "published package name collides with different candidate bytes: {name}"
                )));
            }
            continue;
        }
        stage_package(&deb, &pool.join(&name), contract, backend, path_overlay)?;
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
) -> Result<(), GeneratorError> {
    let contract = &inputs.contract;
    let tag = parse_stable_tag(&inputs.version)?;
    // Malformed pointers are rejected before any mutation or signing: only
    // the tag-agreement half waits for the retained rollback, which the
    // strict index build below computes.
    check_stable_pointer_shape(inputs.previous_pointer)?;
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
    if let Some(prev_dir) = inputs.prev_dir {
        stage_dir_debs(
            prev_dir,
            &pool,
            inputs.incoming,
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
    }
    // Stage the candidate debs by exact name: verification already proved
    // the incoming directory holds exactly this pair.
    for arch in REQUIRED_ARCHES {
        let name = format!("{}-{}-{arch}.deb", contract.package, tag.version);
        let deb = inputs.incoming.join(&name);
        if deb.is_file() {
            stage_package(
                &deb,
                &pool.join(&name),
                contract,
                inputs.backend,
                inputs.path_overlay,
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
    check_stable_pointer(inputs.previous_pointer, &format!("v{rollback}"))?;
    prime_signer_agent(&contract.signer, homedir, passphrase, inputs.path_overlay)?;
    sign_suite_release(
        inputs.staging,
        Suite::Stable,
        contract,
        passphrase,
        homedir,
        inputs.path_overlay,
    )?;
    let source_record = sidecar_digest(&inputs.incoming.join(RECORD_SIDECAR))?;
    emit_publication_record(
        inputs.staging,
        Suite::Stable,
        contract,
        &tag.tag,
        &tag.version,
        &source_record,
        inputs.previous_pointer,
        inputs.path_overlay,
        passphrase,
        homedir,
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
    let pool = pool_root(inputs.staging, Suite::Preview, contract);
    std::fs::create_dir_all(inputs.staging.join("conf"))
        .map_err(|error| GeneratorError::io("create", inputs.staging, &error))?;
    ensure_preview_stanza(inputs.staging, contract)?;
    stage_keyring(inputs.staging, contract)?;
    if inputs.bootstrap {
        stage_preview_candidates(inputs, &parsed, &pool)?;
        check_bootstrap_pool(&pool, contract, &parsed.version)?;
        build_bootstrap_indexes(
            inputs.staging,
            contract,
            &parsed.version,
            inputs.path_overlay,
        )?;
    } else {
        let Some(prev_dir) = inputs.prev_dir else {
            return Err(GeneratorError::usage("publish: --prev-dir is required"));
        };
        stage_dir_debs(
            prev_dir,
            &pool,
            inputs.incoming,
            contract,
            inputs.backend,
            inputs.path_overlay,
        )?;
        stage_preview_candidates(inputs, &parsed, &pool)?;
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
    let source_manifest = sha256_file(&inputs.incoming.join(PREVIEW_MANIFEST_FILE))?;
    emit_publication_record(
        inputs.staging,
        Suite::Preview,
        contract,
        PREVIEW_TAG,
        &parsed.version,
        &source_manifest,
        inputs.previous_pointer,
        inputs.path_overlay,
        passphrase,
        homedir,
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
        let sha = field(published, "source_record_sha256")?;
        if !valid_digest(sha) {
            return Err(GeneratorError::usage(
                "prior publication record checksum is invalid",
            ));
        }
        return Ok(serde_json::json!({
            "tag": tag,
            "source_record_sha256": sha,
        }));
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
        return Ok(serde_json::json!({
            "tag": prior_tag,
            "source_record_sha256": sha,
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
    for package in packages {
        entries.push(IndexEntry {
            arch: field(package, "arch")?.to_owned(),
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
    let previous = document
        .get("previous")
        .cloned()
        .ok_or_else(|| GeneratorError::usage("publication record has no previous pointer"))?;
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
    previous_pointer: &Path,
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
    let previous = read_json(previous_pointer)?;
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
    record.insert("previous".to_owned(), previous);
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
        StableIncoming { dir, tag, commit }
    }

    fn stable_verify_inputs(incoming: &StableIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Stable,
            source_repo: FIXTURE_SOURCE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            binary: FIXTURE_BINARY.to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            identity_dir: FIXTURE_IDENTITY.to_owned(),
            version: incoming.tag.clone(),
            commit: Some(incoming.commit.clone()),
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
        version: String,
        commit: String,
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
        PreviewIncoming {
            dir,
            version,
            commit,
        }
    }

    fn preview_verify_inputs(incoming: &PreviewIncoming) -> VerifyInputs<'_> {
        VerifyInputs {
            suite: Suite::Preview,
            source_repo: FIXTURE_SOURCE.to_owned(),
            package: FIXTURE_PACKAGE.to_owned(),
            binary: FIXTURE_BINARY.to_owned(),
            manifest_schema: FIXTURE_SCHEMA.to_owned(),
            identity_dir: FIXTURE_IDENTITY.to_owned(),
            version: incoming.version.clone(),
            commit: Some(incoming.commit.clone()),
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
        }
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
        assert!(error.contains("exactly four package files"), "{error}");
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
                prev.join("example-1.2.3-arm64.deb"),
            ),
            "colliding arm64 name",
        );
        // Corrupt the colliding copy so its bytes differ.
        must(
            std::fs::write(prev.join("example-1.2.3-arm64.deb"), b"different-bytes"),
            "corrupt the collision",
        );
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
                Some(&prev),
                Path::new("previous-pointer.json"),
                &staging,
                false,
                Some(&stubs.bin),
            );
            must_fail(publish_suite(&inputs), "collision")
        });
        assert!(error.contains("different candidate bytes"), "{error}");
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
        let prev = preview_prev_dir(&root);
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

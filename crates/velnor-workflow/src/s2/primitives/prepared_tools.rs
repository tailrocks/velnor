//! Verified prepared-tool handoff: one identity from request to install.
//!
//! A prepared tool is a bundle a producer job built and a consumer job needs:
//! one tool id, one inputs digest, one platform ABI, one producer identity,
//! one outcome, and a file list with digests. The handoff reasons about that
//! shape only — never about repository names, lanes, tools, or backends — so
//! any repository's prepared tools fit the same verifier.
//!
//! The structural rule: identity, transport, and install share one type.
//! [`ToolRequest`] names what the consumer needs, [`ToolManifest`] names what
//! a producer built, and [`VerifiedBundle`] is the only value that reaches an
//! install. It is constructible only through [`verify`], which checks tool id,
//! inputs digest, platform ABI, authorized producer, success outcome, and
//! manifest/byte integrity all-or-nothing: there is no path from "bytes
//! arrived" to "tool on PATH" that bypasses it.
//!
//! Requested and resolved keys are distinct types ([`RequestedKey`] vs
//! [`ResolvedKey`]). A consumer asks under its current run; a historical
//! fallback resolves under the producer run that built it. [`save_is_legal`]
//! refuses any save whose key names a run the manifest does not, so fallback
//! bytes can never be stored under the requested exact key.
//!
//! Every failure is one [`HandoffFailure`]: [`HandoffFailure::Miss`],
//! [`HandoffFailure::Corrupt`], [`HandoffFailure::Denied`], or
//! [`HandoffFailure::Transient`]. Only `Transient` is retryable
//! ([`HandoffFailure::is_retryable`]), so callers handle each outcome
//! explicitly instead of collapsing them into "retry" or "rebuild".

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use super::{Args, Primitive, RenderCtx, Rendered, PREPARED_TOOL};
use crate::s2::{GeneratorError, RustToolchain, Unit};

/// The prepared-tool key schema. Bumping it abandons every previously saved
/// bundle: entries saved under an older schema are unreachable by design,
/// because the schema is part of the key.
pub(crate) const PREPARED_TOOL_SCHEMA: &str = "v1";

/// Hex characters of the inputs digest rendered into a key. Twelve characters
/// are 48 bits of the SHA-256 over the canonical inputs — far more than the
/// collision resistance a handful of bundles per repository needs, and short
/// enough to keep keys readable.
const INPUTS_DIGEST_CHARS: usize = 12;

/// The key namespace the prepared-tool family owns.
const KEY_NAMESPACE: &str = "prepared-tool";

/// One file inside a tool bundle: a workspace-relative path, its lowercase
/// hex SHA-256, and whether the install marks it executable.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) struct ToolFile {
    /// The workspace-relative install path. Lexically validated on verify:
    /// no absolute paths, no `..`, no empty components, no glob characters.
    pub(crate) path: String,
    /// The lowercase hex SHA-256 the installed bytes must match.
    pub(crate) sha256: String,
    /// Whether the install marks the file executable.
    pub(crate) executable: bool,
}

/// The outcome the producer run recorded for the bundle. Only
/// [`ToolOutcome::Success`] is installable; anything else fails closed as
/// [`HandoffFailure::Denied`], never as a rebuild hint.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolOutcome {
    /// The producer run built the bundle successfully.
    Success,
    /// The producer run failed. The bundle exists only as a record and is
    /// never installed.
    Failed,
}

/// The producer identity that built a bundle: the producer slug the
/// repository authorized, and the run that produced these exact bytes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProducerIdentity {
    /// The producer slug, as listed in the request's authorized set.
    pub(crate) producer: String,
    /// The producer run id: decimal digits, naming the exact run.
    pub(crate) run_id: String,
}

/// A producer's claim about the tool bundle it built.
///
/// The manifest is verified struct-first: unknown fields are rejected on
/// parse (shape before membership), then [`verify`] checks every field
/// against the request and the bytes. `manifest_sha256` binds the claim to
/// its canonical bytes (see [`ToolManifest::canonical_digest`]).
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolManifest {
    /// The tool id this bundle provides.
    pub(crate) tool_id: String,
    /// The lowercase hex SHA-256 of the canonical inputs the bundle was
    /// built from.
    pub(crate) inputs_digest: String,
    /// The platform ABI the bundle runs on, `OS-ARCH` shaped.
    pub(crate) platform_abi: String,
    /// Who built these bytes.
    pub(crate) producer: ProducerIdentity,
    /// What the producer run recorded.
    pub(crate) outcome: ToolOutcome,
    /// Every file the install lays down, with digests.
    pub(crate) files: Vec<ToolFile>,
    /// The lowercase hex SHA-256 over [`ToolManifest::canonical_digest`].
    pub(crate) manifest_sha256: String,
}

/// What a consumer job needs: the tool id, inputs digest, and platform ABI,
/// plus the authorized-producer set from the repo-owned generation config
/// and the current run id exact matches are preferred under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolRequest {
    /// The tool id the consumer needs.
    pub(crate) tool_id: String,
    /// The inputs digest the consumer needs it built from.
    pub(crate) inputs_digest: String,
    /// The platform ABI the consumer runs on.
    pub(crate) platform_abi: String,
    /// The producer slugs the repository authorized for this tool.
    pub(crate) authorized_producers: BTreeSet<String>,
    /// The current run id: exact current-run output is preferred over any
    /// historical bundle.
    pub(crate) run_id: String,
}

/// The key a consumer asks under: the requested identity. Derived from the
/// request's own run id, never from a bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestedKey(String);

/// The key a bundle resolves under: the resolved identity. Derived from the
/// bundle's own producer identity, never from the request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedKey(String);

impl RequestedKey {
    /// The rendered key text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl ResolvedKey {
    /// The rendered key text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One bound on every transfer loop: attempts, result pages, and total wait
/// in seconds. The transports render these as literals; exceeding any bound
/// is a typed [`HandoffFailure::Transient`], never a hang.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransferBounds {
    /// The transfer attempts allowed, 1-based: attempt 1 always runs.
    pub(crate) attempts: u32,
    /// The result pages one listing may consume.
    pub(crate) pages: u32,
    /// The total wait allowed across backoffs, in seconds.
    pub(crate) wait_seconds: u64,
}

impl TransferBounds {
    /// The default bounds: three attempts, ten pages, five minutes of wait.
    pub(crate) const DEFAULT: Self = Self {
        attempts: 3,
        pages: 10,
        wait_seconds: 300,
    };

    /// Refuse attempt `attempt` (1-based) past the bound.
    ///
    /// # Errors
    /// Returns [`HandoffFailure::Transient`] when `attempt` exceeds
    /// `attempts`.
    pub(crate) fn check_attempt(&self, attempt: u32) -> Result<(), HandoffFailure> {
        if attempt == 0 || attempt > self.attempts {
            return Err(HandoffFailure::Transient {
                detail: format!(
                    "attempt {attempt} exceeds the transfer bound of {}",
                    self.attempts
                ),
            });
        }
        Ok(())
    }

    /// Refuse a listing past the page bound.
    ///
    /// # Errors
    /// Returns [`HandoffFailure::Transient`] when `pages` exceeds the bound.
    pub(crate) fn check_pages(&self, pages: u32) -> Result<(), HandoffFailure> {
        if pages > self.pages {
            return Err(HandoffFailure::Transient {
                detail: format!("{pages} pages exceed the transfer bound of {}", self.pages),
            });
        }
        Ok(())
    }

    /// Refuse further waiting past the wait bound.
    ///
    /// # Errors
    /// Returns [`HandoffFailure::Transient`] when `waited_seconds` exceeds
    /// `wait_seconds`.
    pub(crate) fn check_wait(&self, waited_seconds: u64) -> Result<(), HandoffFailure> {
        if waited_seconds > self.wait_seconds {
            return Err(HandoffFailure::Transient {
                detail: format!(
                    "{waited_seconds}s of wait exceed the transfer bound of {}s",
                    self.wait_seconds
                ),
            });
        }
        Ok(())
    }
}

/// How a prepared-tool handoff can fail. The taxonomy forces callers to
/// handle each outcome explicitly: only [`HandoffFailure::Transient`] is
/// retryable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HandoffFailure {
    /// No bundle matched the request: nothing to install, and — when the
    /// consumer is also a producer — the signal to build.
    Miss {
        /// Why no candidate matched, naming the request.
        detail: String,
    },
    /// A bundle claimed the identity but failed integrity: a field mismatch,
    /// a manifest digest mismatch, a missing or digested-wrong file, or an
    /// unsafe path. Never installed, never retried as-is.
    Corrupt {
        /// The exact check that failed.
        detail: String,
    },
    /// A bundle matched by content but is refused by policy: an unauthorized
    /// producer or a non-success outcome.
    Denied {
        /// The policy that refused it.
        detail: String,
    },
    /// The transfer exceeded its [`TransferBounds`]. The only retryable
    /// outcome, and only with fresh bounds.
    Transient {
        /// The bound that fired.
        detail: String,
    },
}

impl HandoffFailure {
    /// Whether the caller may retry the handoff. Only a bound firing is
    /// retryable: a miss needs a build, corruption needs an investigation,
    /// and a denial needs a policy change.
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }
}

impl std::fmt::Display for HandoffFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Miss { detail } => write!(f, "prepared-tool miss: {detail}"),
            Self::Corrupt { detail } => write!(f, "prepared-tool corrupt: {detail}"),
            Self::Denied { detail } => write!(f, "prepared-tool denied: {detail}"),
            Self::Transient { detail } => write!(f, "prepared-tool transient: {detail}"),
        }
    }
}

impl std::error::Error for HandoffFailure {}

/// Whether `value` is a full lowercase hex SHA-256 digest.
pub(crate) fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Whether `value` is a slug: lowercase ASCII, starting alphanumeric, then
/// alphanumeric plus `-` and `_`. Tool ids and producer names are slugs so
/// keys stay lexical and portable.
fn is_slug(value: &str) -> bool {
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
    })
}

/// Whether `value` is a run id: decimal digits, as the platform reports.
fn is_run_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `value` is a platform ABI: two `OS-ARCH`-shaped segments of ASCII
/// letters, digits, and `_`, joined by one `-` (for example `Linux-X64`).
/// Case is preserved so the ABI matches the platform's own report.
fn is_platform_abi(value: &str) -> bool {
    let mut segments = value.split('-');
    match (segments.next(), segments.next(), segments.next()) {
        (Some(os), Some(arch), None) => {
            fn segment_ok(segment: &str) -> bool {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            }
            segment_ok(os) && segment_ok(arch)
        }
        _ => false,
    }
}

/// Whether `path` is a safe workspace-relative install path: nonempty, no
/// absolute paths, no empty components, no `.` or `..`, no backslashes, and
/// no glob characters. Same lexical discipline as the cache transport's path
/// validation: classification must not turn normalization into an
/// authorization boundary.
fn install_path_is_lexically_valid(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// The lowercase hex SHA-256 of `bytes`.
fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

impl ToolManifest {
    /// The canonical bytes the manifest digest covers: the field set is
    /// fixed, files are ordered by path (manifest order is not an identity
    /// fact), and every string is JSON-escaped, so the same manifest digests
    /// the same value on every machine and across generator versions.
    fn canonical_bytes(&self) -> String {
        let mut files = self.files.clone();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut canonical = String::new();
        canonical.push('{');
        for (key, value) in [
            ("inputs_digest", self.inputs_digest.clone()),
            ("platform_abi", self.platform_abi.clone()),
            ("producer", self.producer.producer.clone()),
            ("run_id", self.producer.run_id.clone()),
            (
                "outcome",
                match self.outcome {
                    ToolOutcome::Success => "success".to_owned(),
                    ToolOutcome::Failed => "failed".to_owned(),
                },
            ),
            ("tool_id", self.tool_id.clone()),
        ] {
            if !canonical.ends_with('{') {
                canonical.push(',');
            }
            let _ = write!(
                canonical,
                "{}:{}",
                serde_json::to_string(key).unwrap_or_default(),
                serde_json::to_string(&value).unwrap_or_default()
            );
        }
        canonical.push_str(",\"files\":[");
        for (index, file) in files.iter().enumerate() {
            if index > 0 {
                canonical.push(',');
            }
            let _ = write!(
                canonical,
                "[{},{},{}]",
                serde_json::to_string(&file.path).unwrap_or_default(),
                serde_json::to_string(&file.sha256).unwrap_or_default(),
                file.executable
            );
        }
        canonical.push_str("]}");
        canonical
    }

    /// The manifest digest: lowercase hex SHA-256 over
    /// [`ToolManifest::canonical_bytes`]. A producer stamps this into
    /// `manifest_sha256`; [`verify`] recomputes it, so any tampering with a
    /// claimed field fails closed as [`HandoffFailure::Corrupt`].
    pub(crate) fn canonical_digest(&self) -> String {
        hex_digest(self.canonical_bytes().as_bytes())
    }

    /// Parse a manifest from JSON bytes, rejecting unknown fields: shape
    /// before membership, so a manifest from a newer schema never verifies
    /// against an older reader.
    ///
    /// # Errors
    /// Returns [`HandoffFailure::Corrupt`] when the bytes are not a manifest
    /// of this shape.
    pub(crate) fn from_json(bytes: &[u8]) -> Result<Self, HandoffFailure> {
        serde_json::from_slice(bytes).map_err(|error| HandoffFailure::Corrupt {
            detail: format!("manifest is not a tool manifest: {error}"),
        })
    }
}

/// The key text for a tool identity: namespace, schema, tool slug, platform
/// ABI, the inputs digest prefix, and the run that built the bytes.
fn key_text(tool_id: &str, platform_abi: &str, inputs_digest: &str, run_id: &str) -> String {
    let prefix = inputs_digest.get(..INPUTS_DIGEST_CHARS).unwrap_or("");
    format!("{KEY_NAMESPACE}-{PREPARED_TOOL_SCHEMA}-{tool_id}-{platform_abi}-{prefix}-run-{run_id}")
}

/// The key the request asks under: the requested identity, naming the
/// current run.
pub(crate) fn requested_key(request: &ToolRequest) -> RequestedKey {
    RequestedKey(key_text(
        &request.tool_id,
        &request.platform_abi,
        &request.inputs_digest,
        &request.run_id,
    ))
}

/// The key a bundle resolves under: the resolved identity, naming the
/// producer run that built it.
pub(crate) fn resolved_key(manifest: &ToolManifest) -> ResolvedKey {
    ResolvedKey(key_text(
        &manifest.tool_id,
        &manifest.platform_abi,
        &manifest.inputs_digest,
        &manifest.producer.run_id,
    ))
}

/// Whether saving `manifest`'s bytes under `save_key` is legal: the key must
/// name the run the manifest's own producer identity names. A fallback bundle
/// saved under the requested exact key — the current run instead of the
/// producer run — is refused, so historical bytes can never shadow the exact
/// entry a future run restores.
pub(crate) fn save_is_legal(save_key: &str, manifest: &ToolManifest) -> bool {
    save_key
        .rsplit_once("-run-")
        .is_some_and(|(_, run)| run == manifest.producer.run_id)
}

/// A resolution: the manifest selected for a request, carrying the requested
/// and resolved keys as two distinct values. They agree on an exact
/// current-run hit and differ on a historical fallback — and tests pin them
/// apart, so the two identities can never collapse into one field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Resolution {
    /// The key the consumer asked under.
    pub(crate) requested: RequestedKey,
    /// The key the selected bundle resolves under.
    pub(crate) resolved: ResolvedKey,
    /// Whether the resolved bundle is the current run's own output.
    pub(crate) is_exact: bool,
}

/// Select the bundle a request resolves to: the exact current-run output when
/// a candidate names this run with a success outcome from an authorized
/// producer, otherwise the first fully matching historical candidate.
/// Candidates arrive newest-first; selection compares identity fields only —
/// byte integrity is proven later by [`verify`], never assumed here.
///
/// # Errors
/// Returns [`HandoffFailure::Miss`] when no candidate matches the request's
/// tool id, inputs digest, platform ABI, authorized producers, and success
/// outcome.
pub(crate) fn resolve<'a, I>(
    request: &ToolRequest,
    candidates: I,
) -> Result<(Resolution, &'a ToolManifest), HandoffFailure>
where
    I: IntoIterator<Item = &'a ToolManifest>,
{
    fn matches(request: &ToolRequest, manifest: &ToolManifest) -> bool {
        manifest.tool_id == request.tool_id
            && manifest.inputs_digest == request.inputs_digest
            && manifest.platform_abi == request.platform_abi
            && manifest.outcome == ToolOutcome::Success
            && request
                .authorized_producers
                .contains(&manifest.producer.producer)
    }

    let mut fallback: Option<&'a ToolManifest> = None;
    for manifest in candidates {
        if !matches(request, manifest) {
            continue;
        }
        if manifest.producer.run_id == request.run_id {
            return Ok((
                Resolution {
                    requested: requested_key(request),
                    resolved: resolved_key(manifest),
                    is_exact: true,
                },
                manifest,
            ));
        }
        if fallback.is_none() {
            fallback = Some(manifest);
        }
    }
    fallback.map_or_else(
        || {
            Err(HandoffFailure::Miss {
                detail: format!(
                    "no bundle for tool `{}` at inputs `{}` on `{}` from an authorized producer",
                    request.tool_id, request.inputs_digest, request.platform_abi
                ),
            })
        },
        |manifest| {
            Ok((
                Resolution {
                    requested: requested_key(request),
                    resolved: resolved_key(manifest),
                    is_exact: false,
                },
                manifest,
            ))
        },
    )
}

/// A tool bundle proven from request to bytes: the only value that reaches
/// an install. Constructible only through [`verify`], which checks tool id,
/// inputs digest, platform ABI, authorized producer, success outcome, and
/// manifest/byte integrity all-or-nothing, and carries the verified bytes so
/// the install cannot substitute others.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedBundle {
    manifest: ToolManifest,
    files: BTreeMap<String, FileBytes>,
}

/// Verified file bytes: the digest already proven, the executable bit as the
/// manifest claimed it.
#[derive(Clone, Debug)]
struct FileBytes {
    bytes: Vec<u8>,
    executable: bool,
}

impl VerifiedBundle {
    /// The verified manifest.
    pub(crate) fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    /// The install plan: every validated path with its proven digest, ordered
    /// by path. Paths are workspace-relative and lexically safe; the caller
    /// joins them under the install root.
    pub(crate) fn install_plan(&self) -> Vec<(&str, &str, bool)> {
        self.manifest
            .files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.sha256.as_str(),
                    self.files
                        .get(&file.path)
                        .is_some_and(|proven| proven.executable),
                )
            })
            .collect()
    }

    /// Install the verified bytes under `destination`: stage every file into
    /// a fresh sibling directory, re-prove each staged digest, then rename
    /// atomically. The destination is never mutated in place and never
    /// partially satisfied: it either appears whole or not at all, and an
    /// existing destination is refused rather than overwritten.
    ///
    /// # Errors
    /// Returns an I/O error when the destination or staging directory already
    /// exists, when a file cannot be written, or when a staged digest
    /// re-proof fails.
    pub(crate) fn install_to(&self, destination: &Path) -> std::io::Result<()> {
        if destination.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("refusing to overwrite {}", destination.display()),
            ));
        }
        let staging = staging_sibling(destination)?;
        if staging.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("staging directory {} already exists", staging.display()),
            ));
        }
        let installed = self.stage_and_commit(&staging, destination);
        if installed.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        installed
    }

    /// Stage every verified file, re-prove the staged digests, and rename the
    /// staging directory onto the destination.
    fn stage_and_commit(&self, staging: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(staging)?;
        for (path, proven) in &self.files {
            let target = staging.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &proven.bytes)?;
            set_executable(&target, proven.executable)?;
            let staged = std::fs::read(&target)?;
            if hex_digest(&staged) != hex_digest(&proven.bytes) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("staged digest mismatch for {path}"),
                ));
            }
        }
        std::fs::rename(staging, destination)
    }
}

/// The staging sibling of a destination: the destination file name with a
/// `.staging` suffix, beside it so the commit rename stays on one
/// filesystem.
fn staging_sibling(destination: &Path) -> std::io::Result<std::path::PathBuf> {
    let name = destination.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("destination {} has no file name", destination.display()),
        )
    })?;
    let mut staging = name.to_os_string();
    staging.push(".staging");
    Ok(destination.with_file_name(staging))
}

/// Mark `path` executable or not, per the manifest's claim. Only Unix
/// carries execute bits; elsewhere the bytes install without mode changes.
#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Mark `path` executable or not, per the manifest's claim. Only Unix
/// carries execute bits; elsewhere the bytes install without mode changes.
#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// Verify a manifest against a request and its bytes, all-or-nothing: tool
/// id, inputs digest, platform ABI, authorized producer, success outcome,
/// and manifest digest must all hold; then every listed file must be present
/// with matching bytes at a lexically safe path. The returned
/// [`VerifiedBundle`] carries the proven bytes to the install, so no later
/// step can substitute others.
///
/// `files` maps manifest paths to the bytes that arrived for them. Extra
/// entries beyond the manifest are ignored: the manifest alone decides what
/// installs.
///
/// # Errors
/// Returns [`HandoffFailure::Corrupt`] for any identity, integrity, or path
/// failure, and [`HandoffFailure::Denied`] for an unauthorized producer or a
/// non-success outcome.
pub(crate) fn verify(
    request: &ToolRequest,
    manifest: &ToolManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<VerifiedBundle, HandoffFailure> {
    check_identity(request, manifest)?;
    let verified = check_files(manifest, files)?;
    Ok(VerifiedBundle {
        manifest: manifest.clone(),
        files: verified,
    })
}

/// Prove the manifest claims the request's identity: well-formed tool id,
/// inputs digest, platform ABI, and producer identity, all matching the
/// request; the manifest digest covering the claimed fields; a success
/// outcome from an authorized producer.
fn check_identity(request: &ToolRequest, manifest: &ToolManifest) -> Result<(), HandoffFailure> {
    let corrupt = |detail: String| HandoffFailure::Corrupt { detail };
    let denied = |detail: String| HandoffFailure::Denied { detail };
    if !is_slug(&manifest.tool_id) {
        return Err(corrupt(format!(
            "tool id `{}` is not a slug",
            manifest.tool_id
        )));
    }
    if manifest.tool_id != request.tool_id {
        return Err(corrupt(format!(
            "tool id `{}` does not match request `{}`",
            manifest.tool_id, request.tool_id
        )));
    }
    if !is_digest(&manifest.inputs_digest) {
        return Err(corrupt(format!(
            "inputs digest `{}` is not a full digest",
            manifest.inputs_digest
        )));
    }
    if manifest.inputs_digest != request.inputs_digest {
        return Err(corrupt(format!(
            "inputs digest `{}` does not match request `{}`",
            manifest.inputs_digest, request.inputs_digest
        )));
    }
    if !is_platform_abi(&manifest.platform_abi) {
        return Err(corrupt(format!(
            "platform ABI `{}` is not `OS-ARCH` shaped",
            manifest.platform_abi
        )));
    }
    if manifest.platform_abi != request.platform_abi {
        return Err(corrupt(format!(
            "platform ABI `{}` does not match request `{}`",
            manifest.platform_abi, request.platform_abi
        )));
    }
    if !is_slug(&manifest.producer.producer) {
        return Err(corrupt(format!(
            "producer `{}` is not a slug",
            manifest.producer.producer
        )));
    }
    if !is_run_id(&manifest.producer.run_id) {
        return Err(corrupt(format!(
            "producer run id `{}` is not decimal",
            manifest.producer.run_id
        )));
    }
    if manifest.manifest_sha256 != manifest.canonical_digest() {
        return Err(corrupt(
            "manifest digest does not cover the claimed fields".to_owned(),
        ));
    }
    if manifest.outcome != ToolOutcome::Success {
        return Err(denied(format!(
            "tool `{}` from run {} records a failed outcome",
            manifest.tool_id, manifest.producer.run_id
        )));
    }
    if !request
        .authorized_producers
        .contains(&manifest.producer.producer)
    {
        return Err(denied(format!(
            "producer `{}` is not authorized for tool `{}`",
            manifest.producer.producer, manifest.tool_id
        )));
    }
    Ok(())
}

/// Prove the arrived bytes satisfy the manifest: a nonempty file list, every
/// path lexically safe and listed once, every file present with matching
/// bytes. Returns the proven bytes the install carries.
fn check_files(
    manifest: &ToolManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, FileBytes>, HandoffFailure> {
    let corrupt = |detail: String| HandoffFailure::Corrupt { detail };
    if manifest.files.is_empty() {
        return Err(corrupt("manifest lists no files".to_owned()));
    }
    let mut verified: BTreeMap<String, FileBytes> = BTreeMap::new();
    for file in &manifest.files {
        if !install_path_is_lexically_valid(&file.path) {
            return Err(corrupt(format!(
                "manifest path `{}` is not a safe relative path",
                file.path
            )));
        }
        if !is_digest(&file.sha256) {
            return Err(corrupt(format!(
                "digest for `{}` is not a full digest",
                file.path
            )));
        }
        let Some(bytes) = files.get(&file.path) else {
            return Err(corrupt(format!("manifest file `{}` is missing", file.path)));
        };
        if hex_digest(bytes) != file.sha256 {
            return Err(corrupt(format!(
                "bytes for `{}` do not match the manifest digest",
                file.path
            )));
        }
        if verified
            .insert(
                file.path.clone(),
                FileBytes {
                    bytes: bytes.clone(),
                    executable: file.executable,
                },
            )
            .is_some()
        {
            return Err(corrupt(format!("manifest lists `{}` twice", file.path)));
        }
    }
    Ok(verified)
}

/// A lockfile kind the scan already knows, named by filename only. Discovery
/// matches final path components against this vocabulary — never a
/// repository path — so workspace, standalone, and nested layouts resolve
/// without literals.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum LockKind {
    /// `mise.lock`: the mise tool pins.
    Mise,
    /// `Cargo.lock`: the Cargo resolution.
    Cargo,
    /// `bun.lock` / `bun.lockb`: the Bun resolution.
    Bun,
    /// `package-lock.json`: the npm resolution.
    Npm,
    /// `Package.resolved`: the `SwiftPM` resolution.
    Swift,
    /// `.terraform.lock.hcl`: the provider pins.
    OpenTofu,
}

/// The filename vocabulary, in tie-break order: when two filenames of one
/// kind govern from the same directory, the earlier entry wins.
const LOCK_FILENAMES: &[(&str, LockKind)] = &[
    ("mise.lock", LockKind::Mise),
    ("Cargo.lock", LockKind::Cargo),
    ("bun.lock", LockKind::Bun),
    ("bun.lockb", LockKind::Bun),
    ("package-lock.json", LockKind::Npm),
    ("Package.resolved", LockKind::Swift),
    (".terraform.lock.hcl", LockKind::OpenTofu),
];

/// The kind a filename governs as, with its vocabulary index for ties.
fn lock_kind_of(filename: &str) -> Option<(LockKind, usize)> {
    LOCK_FILENAMES
        .iter()
        .position(|(known, _)| *known == filename)
        .map(|index| (LOCK_FILENAMES[index].1, index))
}

/// The parent directory of a walked path: `""` for a root-level file.
fn split_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// The final component of a walked path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether the lockfile parent `parent` governs the unit root `unit` (both
/// `""`-normalized): the lock sits at the root or above it. The one below
/// case is `SwiftPM`'s tool-owned subdirectory, `<unit>/.swiftpm`, which the
/// scan reads like the package root; anything deeper is another unit's own
/// tree, never this unit's governing input.
fn lock_parent_governs(parent: &str, unit: &str, kind: LockKind) -> bool {
    if parent == unit || parent.is_empty() {
        return true;
    }
    if unit.len() > parent.len()
        && unit.starts_with(parent)
        && unit.as_bytes().get(parent.len()) == Some(&b'/')
    {
        return true;
    }
    if kind == LockKind::Swift {
        let owned = if unit.is_empty() {
            ".swiftpm".to_owned()
        } else {
            format!("{unit}/.swiftpm")
        };
        if parent == owned {
            return true;
        }
    }
    false
}

/// The depth of a parent directory: path components below the root.
fn parent_depth(parent: &str) -> usize {
    if parent.is_empty() {
        0
    } else {
        parent.split('/').count()
    }
}

/// The lockfiles governing the unit rooted at `unit_root`: the nearest
/// ancestor-or-self lock per kind over the scan's file list. `unit_root` is
/// `"."` for a root unit, else the unit's repository-relative directory. A
/// unit is governed by its nearest lock per kind, so a nested unit binds its
/// own lock while inheriting the kinds it does not pin from above.
pub(crate) fn governing_locks(files: &[String], unit_root: &str) -> BTreeMap<LockKind, String> {
    let unit = if unit_root == "." { "" } else { unit_root };
    let mut best: BTreeMap<LockKind, (usize, usize, String)> = BTreeMap::new();
    for file in files {
        let Some((kind, vocabulary)) = lock_kind_of(file_name(file)) else {
            continue;
        };
        let parent = split_parent(file);
        if !lock_parent_governs(parent, unit, kind) {
            continue;
        }
        let depth = parent_depth(parent);
        let replace = match best.get(&kind) {
            // Nearest wins; vocabulary order, then path, breaks ties so the
            // choice is deterministic whatever order the walk produced.
            Some((best_depth, best_vocabulary, best_path)) => {
                (
                    depth,
                    std::cmp::Reverse(vocabulary),
                    std::cmp::Reverse(file),
                ) > (
                    *best_depth,
                    std::cmp::Reverse(*best_vocabulary),
                    std::cmp::Reverse(best_path),
                )
            }
            None => true,
        };
        if replace {
            best.insert(kind, (depth, vocabulary, file.clone()));
        }
    }
    best.into_iter()
        .map(|(kind, (_, _, path))| (kind, path))
        .collect()
}

/// The generation-known facts an inputs digest binds: the compilation inputs
/// (governing lockfile bytes by repository-relative path), the transitive
/// local source/build-script/configuration closure by repository-relative
/// path, the build recipe as the repository declared it (or the adapter
/// operation identity for scanned native products), and the toolchain pin.
/// Platform ABI and trust boundary are runtime facts: the ABI renders into
/// the key as a runner expression and the manifest check proves it, while
/// saves stay trusted-event gated so untrusted bytes never enter the
/// namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputsFacts {
    /// Governing lockfiles: repository-relative path and committed bytes.
    pub(crate) locks: Vec<(String, Vec<u8>)>,
    /// Closure sources: repository-relative path and bytes, empty for tools
    /// whose local source is not part of identity.
    pub(crate) sources: Vec<(String, Vec<u8>)>,
    /// The tool's build recipe: the declared commands that produce it.
    pub(crate) recipe: Vec<String>,
    /// The toolchain pin facts, `key=value` shaped.
    pub(crate) toolchain: Vec<String>,
}

/// The toolchain pin facts for a digest: the Rust channel, components,
/// targets, and profile when the scan parsed a pin, plus the kind-level tool
/// version when the repository declares one.
pub(crate) fn toolchain_facts(
    toolchain: Option<&RustToolchain>,
    tool_version: Option<&str>,
) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(pinned) = toolchain {
        facts.push(format!("channel={}", pinned.channel()));
        for component in pinned.components() {
            facts.push(format!("component={component}"));
        }
        for target in pinned.targets() {
            facts.push(format!("target={target}"));
        }
        if let Some(profile) = pinned.profile() {
            facts.push(format!("profile={profile}"));
        }
    }
    if let Some(version) = tool_version {
        facts.push(format!("tool-version={version}"));
    }
    facts
}

/// The inputs digest: lowercase hex SHA-256 over the canonical inputs
/// bytes. Locks and sources sort by path (walk order is not an identity
/// fact) and carry content digests; recipe and toolchain lines keep declared
/// order. Any compilation-input, recipe, or toolchain change mints a new
/// digest, so a bundle built from other inputs can never share the key. An
/// empty source list emits no lines, so tools without a source closure keep
/// their historical digests byte for byte.
pub(crate) fn inputs_digest(facts: &InputsFacts) -> String {
    let mut locks: Vec<(&str, String)> = facts
        .locks
        .iter()
        .map(|(path, bytes)| (path.as_str(), hex_digest(bytes)))
        .collect();
    locks.sort();
    let mut sources: Vec<(&str, String)> = facts
        .sources
        .iter()
        .map(|(path, bytes)| (path.as_str(), hex_digest(bytes)))
        .collect();
    sources.sort();
    let mut canonical = String::from("prepared-tool-inputs-v1\n");
    for (path, digest) in &locks {
        canonical.push_str("lock ");
        canonical.push_str(&serde_json::to_string(path).unwrap_or_default());
        canonical.push(' ');
        canonical.push_str(digest);
        canonical.push('\n');
    }
    for (path, digest) in &sources {
        canonical.push_str("source ");
        canonical.push_str(&serde_json::to_string(path).unwrap_or_default());
        canonical.push(' ');
        canonical.push_str(digest);
        canonical.push('\n');
    }
    for line in &facts.recipe {
        canonical.push_str("recipe ");
        canonical.push_str(&serde_json::to_string(line).unwrap_or_default());
        canonical.push('\n');
    }
    for field in &facts.toolchain {
        canonical.push_str("toolchain ");
        canonical.push_str(&serde_json::to_string(field).unwrap_or_default());
        canonical.push('\n');
    }
    hex_digest(canonical.as_bytes())
}

/// One tool a unit's jobs need, as the declaration row recorded it: the tool
/// id, the authorized producers, and the inputs digest binding the
/// compilation inputs, recipe, and toolchain the bundle must be built from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedToolNeed {
    /// The tool id the consumer needs.
    pub(crate) tool_id: String,
    /// The producer slugs the repository authorized for this tool.
    pub(crate) authorized_producers: BTreeSet<String>,
    /// The inputs digest the bundle must be built from.
    pub(crate) inputs_digest: String,
}

/// One declared tool before its inputs digest is computed: the digest needs
/// the consuming unit's governing locks, so parsing and binding are two
/// steps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolDeclaration {
    /// The tool id the consumer needs.
    pub(crate) tool_id: String,
    /// The producer slugs the repository authorized for this tool.
    pub(crate) authorized_producers: BTreeSet<String>,
    /// The tool's build recipe, when the row declares one.
    pub(crate) recipe: Vec<String>,
}

/// Parse the `prepared-tool` row's `tools` table (tool id to authorized
/// producers) and its `recipes` table (tool id to build recipe). Tool ids
/// and producers are slugs; every tool needs at least one producer; every
/// recipe must name a declared tool.
///
/// # Errors
/// Returns a usage error for an empty `tools` table, a non-slug tool id or
/// producer, a tool with no producers, or a recipe for an undeclared tool.
pub(crate) fn parse_tool_declarations(
    tools: &BTreeMap<String, Vec<String>>,
    recipes: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<ToolDeclaration>, GeneratorError> {
    if tools.is_empty() {
        return Err(GeneratorError::usage(
            "`[[declare]]` primitive `prepared-tool` declares no tools; map tool ids to authorized producers under `tools`",
        ));
    }
    for name in recipes.keys() {
        if !tools.contains_key(name) {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `prepared-tool` gives a recipe for `{name}`, which `tools` does not declare"
            )));
        }
    }
    let mut declarations = Vec::new();
    for (tool_id, producers) in tools {
        if !is_slug(tool_id) {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `prepared-tool` names tool `{tool_id}`, which is not a slug"
            )));
        }
        if producers.is_empty() {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `prepared-tool` authorizes no producers for tool `{tool_id}`; name the producer jobs whose bundles may install"
            )));
        }
        let mut authorized = BTreeSet::new();
        for producer in producers {
            if !is_slug(producer) {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `prepared-tool` authorizes producer `{producer}` for tool `{tool_id}`, which is not a slug"
                )));
            }
            authorized.insert(producer.clone());
        }
        declarations.push(ToolDeclaration {
            tool_id: tool_id.clone(),
            authorized_producers: authorized,
            recipe: recipes.get(tool_id).cloned().unwrap_or_default(),
        });
    }
    Ok(declarations)
}

/// Classify one restored manifest against its request, then prove it:
/// [`resolve`] over the single candidate decides exact current-run hit vs
/// historical fallback and yields the requested/resolved key pair, and
/// [`verify`] proves identity and byte integrity. When resolve refuses a
/// manifest that exists, verify still runs so the caller gets the precise
/// verdict — wrong ABI, unauthorized producer, tampered bytes — instead of a
/// generic miss.
///
/// # Errors
/// Returns [`HandoffFailure::Corrupt`] for any identity, integrity, or path
/// failure, and [`HandoffFailure::Denied`] for an unauthorized producer or a
/// non-success outcome.
pub(crate) fn classify_and_verify(
    request: &ToolRequest,
    manifest: &ToolManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(Resolution, VerifiedBundle), HandoffFailure> {
    let classified = resolve(request, [manifest])
        .ok()
        .map(|(resolution, _)| resolution);
    let bundle = verify(request, manifest, files)?;
    // A verify success after a resolve refusal contradicts resolve's match
    // predicate, which is weaker than verify's proof; recompute the keys
    // rather than assume the contradiction impossible.
    let resolution = classified.unwrap_or_else(|| Resolution {
        requested: requested_key(request),
        resolved: resolved_key(manifest),
        is_exact: manifest.producer.run_id == request.run_id,
    });
    Ok((resolution, bundle))
}

/// The machine `outcome` name for a failure: the runtime verb records it
/// beside the human message so consumer steps branch on taxonomy, not text.
pub(crate) fn outcome_name(failure: &HandoffFailure) -> &'static str {
    match failure {
        HandoffFailure::Miss { .. } => "miss",
        HandoffFailure::Corrupt { .. } => "corrupt",
        HandoffFailure::Denied { .. } => "denied",
        HandoffFailure::Transient { .. } => "transient",
    }
}

/// The step outputs a successful install records: the requested and resolved
/// keys as two distinct values, the exact-hit flag, and the save key — the
/// resolved identity, the only key these bytes may ever be saved under.
pub(crate) fn format_install_outputs(resolution: &Resolution) -> String {
    format!(
        "requested-key={}\nresolved-key={}\nis-exact={}\nsave-key={}\noutcome=installed\n",
        resolution.requested.as_str(),
        resolution.resolved.as_str(),
        resolution.is_exact,
        resolution.resolved.as_str(),
    )
}

/// The step output a failed install records: the taxonomy outcome alone, so
/// a later producer step can tell a miss (build) from a refusal (fail).
pub(crate) fn format_failure_output(failure: &HandoffFailure) -> String {
    format!("outcome={}\n", outcome_name(failure))
}

/// The step outputs a successful save-key check records: the checked key,
/// proven legal for the verified bytes, and the check outcome. A save step
/// consumes `save-key` instead of rebuilding the key, so fallback bytes can
/// never be saved under the requested exact key by construction.
pub(crate) fn format_save_check_output(save_key: &str) -> String {
    format!("save-key={save_key}\noutcome=save-allowed\n")
}

/// One Actions API response: the status, the raw headers, and the raw body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApiResponse {
    /// The HTTP status code.
    pub(crate) status: u16,
    /// The raw response headers.
    pub(crate) headers: String,
    /// The raw response body.
    pub(crate) body: String,
}

/// Whether a response is rate limited: an explicit 429, or a 403 whose
/// headers show the rate budget exhausted. GitHub answers quota exhaustion
/// as 403 with `x-ratelimit-remaining: 0` on some endpoints, so the headers
/// decide — a plain 403 is a policy denial, a limited one is transient.
pub(crate) fn response_is_rate_limited(status: u16, headers: &str) -> bool {
    if status == 429 {
        return true;
    }
    headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("x-ratelimit-remaining") && value.trim() == "0"
        })
    })
}

/// Map an API outcome to the taxonomy: 2xx is not a failure; 404/410 (the
/// producer run is pruned or never existed) is a miss — provenance is gone,
/// so the bundle is unprovable and a rebuild is the answer; 429, a
/// rate-limited response, or a 5xx is transient; anything else (401, a plain
/// 403, 422, …) is denied. `rate_limited` comes from
/// [`response_is_rate_limited`].
pub(crate) fn api_failure(status: u16, rate_limited: bool) -> Option<HandoffFailure> {
    if (200..300).contains(&status) {
        return None;
    }
    let detail = format!("the actions api answered {status}");
    if status == 404 || status == 410 {
        return Some(HandoffFailure::Miss { detail });
    }
    if status == 429 || rate_limited || (500..600).contains(&status) {
        return Some(HandoffFailure::Transient { detail });
    }
    Some(HandoffFailure::Denied { detail })
}

/// Why a producer-outcome fetch failed: the taxonomy, or a broken transfer
/// executor (a missing HTTP client, an unreadable spawn). Executor breakage
/// is a usage error, never taxonomy: retrying a missing binary is pointless.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OutcomeFetchError {
    /// The transfer answered with a taxonomy outcome.
    Failure(HandoffFailure),
    /// The transfer executor itself failed.
    Transport(String),
}

impl std::fmt::Display for OutcomeFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failure(failure) => write!(f, "{failure}"),
            Self::Transport(detail) => write!(f, "prepared-tool transport: {detail}"),
        }
    }
}

impl std::error::Error for OutcomeFetchError {}

/// The backoff before retry `attempt` (1-based): 1s, 2s, 4s, …, capped at
/// 30s. The wait budget — not the backoff — decides when retries stop.
fn backoff_for_retry(attempt: u32) -> Duration {
    Duration::from_secs(
        1_u64
            .saturating_mul(2_u64.saturating_pow(attempt.saturating_sub(1)))
            .min(30),
    )
}

/// The producer run's conclusion from a workflow-run API body: the
/// `conclusion` field, or `in_progress` when the run has none yet. A body
/// that is not a run object is unreadable, which the fetch treats as
/// transient — a flaked transfer, retried within bounds.
fn run_conclusion(body: &str) -> Result<String, ()> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| ())?;
    let object = value.as_object().ok_or(())?;
    match object.get("conclusion") {
        None | Some(serde_json::Value::Null) => Ok("in_progress".to_owned()),
        Some(serde_json::Value::String(conclusion)) => Ok(conclusion.clone()),
        Some(_) => Err(()),
    }
}

/// Fetch a historical bundle's producer-run conclusion: the outcome half of
/// historical validation. The executor performs one API transfer; the loop
/// retries transient outcomes within `bounds` and returns the first
/// terminal outcome. A single-run fetch consumes one result page, so the
/// page bound refuses a zero-page configuration up front. `elapsed` reports
/// the time since the fetch started, so tests pin the wait budget without
/// sleeping.
pub(crate) fn fetch_producer_conclusion(
    bounds: &TransferBounds,
    run_id: &str,
    executor: &mut dyn FnMut() -> Result<ApiResponse, String>,
    sleeper: &mut dyn FnMut(Duration),
    elapsed: &mut dyn FnMut() -> Duration,
) -> Result<String, OutcomeFetchError> {
    // A single-run fetch consumes one result page: the bound refuses a
    // transfer configured for none, the same way it refuses zero attempts.
    bounds.check_pages(1).map_err(OutcomeFetchError::Failure)?;
    let mut attempt = 1_u32;
    loop {
        bounds
            .check_attempt(attempt)
            .map_err(OutcomeFetchError::Failure)?;
        let response = executor().map_err(OutcomeFetchError::Transport)?;
        let rate_limited = response_is_rate_limited(response.status, &response.headers);
        match api_failure(response.status, rate_limited) {
            None => {
                return run_conclusion(&response.body).map_err(|()| {
                    OutcomeFetchError::Failure(HandoffFailure::Transient {
                        detail: format!(
                            "the actions api answered {} for run {run_id} with an unreadable run body",
                            response.status
                        ),
                    })
                });
            }
            // The retry decision reads the taxonomy, not the variant: only
            // transient outcomes loop, whatever future variants exist. Both
            // remaining budgets are checked before sleeping, so an
            // exhausted loop returns instead of sleeping for a retry that
            // will never run.
            Some(failure) if failure.is_retryable() => {
                let backoff = backoff_for_retry(attempt);
                let waited = elapsed().checked_add(backoff).unwrap_or(Duration::MAX);
                if let Err(failure) = bounds.check_wait(waited.as_secs()) {
                    return Err(OutcomeFetchError::Failure(failure));
                }
                if let Err(failure) = bounds.check_attempt(attempt + 1) {
                    return Err(OutcomeFetchError::Failure(failure));
                }
                sleeper(backoff);
                attempt += 1;
            }
            Some(failure) => return Err(OutcomeFetchError::Failure(failure)),
        }
    }
}

/// The workspace-relative directory a consumer job restores prepared tools
/// into: one subdirectory per tool, holding the manifest and the bundle.
const PREPARED_TOOL_CACHE_DIR: &str = ".velnor-prepared-tools";

/// The job-scoped home a consumer job installs prepared tools under: one
/// subdirectory per tool. Job-scoped and never shared with a cache path, so
/// an install can never be re-saved as cache state under another key.
const PREPARED_TOOL_INSTALL_HOME: &str = "$RUNNER_TEMP/velnor-prepared-tools";

/// The caller-input record for one need: tool, full inputs digest, and
/// `+`-joined producers, `:`-separated. Slugs, hex, and `+` never contain
/// `,` or `:`, so records join with `,` and a `contains` gate over the
/// comma-wrapped input cannot partially match.
pub(crate) fn need_record(need: &PreparedToolNeed) -> String {
    format!(
        "{}:{}:{}",
        need.tool_id,
        need.inputs_digest,
        need.authorized_producers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("+"),
    )
}

/// The caller-input records for every need, in declaration order.
pub(crate) fn need_records(needs: &[PreparedToolNeed]) -> Vec<String> {
    needs.iter().map(need_record).collect()
}

/// The exact-key expression a consumer restores: the requested identity with
/// the platform ABI and the current run id interpolated by the runner. An
/// exact current-run producer output hits this key.
pub(crate) fn exact_key_expression(tool_id: &str, inputs_digest: &str) -> String {
    format!(
        "{KEY_NAMESPACE}-{PREPARED_TOOL_SCHEMA}-{tool_id}-{}-{}-run-{}",
        "${{ runner.os }}-${{ runner.arch }}",
        inputs_digest.get(..INPUTS_DIGEST_CHARS).unwrap_or(""),
        "${{ github.run_id }}",
    )
}

/// The fallback restore prefix a consumer restores: the resolved identity
/// without a run, so the cache service returns the newest historical bundle
/// for this tool, ABI, and inputs. Requested and resolved stay two distinct
/// rendered values — the prefix can never name the current run.
pub(crate) fn restore_prefix_expression(tool_id: &str, inputs_digest: &str) -> String {
    format!(
        "{KEY_NAMESPACE}-{PREPARED_TOOL_SCHEMA}-{tool_id}-{}-{}-run-",
        "${{ runner.os }}-${{ runner.arch }}",
        inputs_digest.get(..INPUTS_DIGEST_CHARS).unwrap_or(""),
    )
}

/// Render one need's consumer steps: restore the exact current-run key with
/// the historical fallback prefix, then verify and install through the
/// runtime verb — the same [`verify`] the unit tests prove, running in CI.
/// A restore that brings no manifest fails closed as a miss: the producer
/// slice turns that outcome into a build, but until it lands a consumer
/// without its tool cannot proceed. Installed binaries land on `PATH` via
/// the tool's `bin/` directory, the layout every bundle carries its
/// executables under.
pub(crate) fn render_consumer_steps(
    output: &mut String,
    cache_restore_pin: &str,
    needs: &[PreparedToolNeed],
) {
    for need in needs {
        let exact = exact_key_expression(&need.tool_id, &need.inputs_digest);
        let prefix = restore_prefix_expression(&need.tool_id, &need.inputs_digest);
        let inputs_prefix = need.inputs_digest.get(..INPUTS_DIGEST_CHARS).unwrap_or("");
        let producers = need
            .authorized_producers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        let _ = writeln!(
            output,
            "      - name: Restore prepared tool {}\n        id: prepared-tool-{}\n        uses: {}\n        with:\n          path: {PREPARED_TOOL_CACHE_DIR}/{}\n          key: {exact}\n          restore-keys: |\n            {prefix}",
            need.tool_id, need.tool_id, cache_restore_pin, need.tool_id,
        );
        let _ = writeln!(
            output,
            "      - name: Install prepared tool {}\n        id: prepared-tool-{}-install\n        shell: bash\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          dir=\"{PREPARED_TOOL_CACHE_DIR}/{}\"\n          if [[ ! -f \"$dir/manifest.json\" ]]; then\n            echo \"::error::prepared-tool miss: no bundle for tool `{}` at inputs `{inputs_prefix}` (exact key and fallback prefix both missed)\" >&2\n            exit 1\n          fi\n          dest=\"{PREPARED_TOOL_INSTALL_HOME}/{}\"\n          velnor-workflow prepared-tool-install \\\n            --manifest \"$dir/manifest.json\" \\\n            --dir \"$dir\" \\\n            --dest \"$dest\" \\\n            --tool \"{}\" \\\n            --inputs \"{}\" \\\n            --abi \"$RUNNER_OS-$RUNNER_ARCH\" \\\n            --producers \"{producers}\" \\\n            --run-id \"$GITHUB_RUN_ID\" \\\n            --repo \"$GITHUB_REPOSITORY\"\n          echo \"$dest/bin\" >> \"$GITHUB_PATH\"",
            need.tool_id,
            need.tool_id,
            need.tool_id,
            need.tool_id,
            need.tool_id,
            need.tool_id,
            need.inputs_digest,
        );
    }
}

/// Read the generation-known inputs facts for one declaration against one
/// unit: the governing lockfile bytes, the declared recipe, and the
/// toolchain pin.
///
/// # Errors
/// Returns an I/O error naming the lockfile that cannot be read.
fn inputs_facts_for(
    root: &Path,
    locks: &BTreeMap<LockKind, String>,
    recipe: &[String],
    unit: &Unit,
) -> Result<InputsFacts, GeneratorError> {
    let mut lock_inputs = Vec::new();
    for path in locks.values() {
        let full = root.join(path);
        let bytes = std::fs::read(&full)
            .map_err(|error| GeneratorError::io("read governing lockfile", &full, &error))?;
        lock_inputs.push((path.clone(), bytes));
    }
    Ok(InputsFacts {
        locks: lock_inputs,
        sources: Vec::new(),
        recipe: recipe.to_vec(),
        toolchain: toolchain_facts(unit.toolchain.as_ref(), unit.tool_version.as_deref()),
    })
}

/// Declare the repository's prepared tools: a unit-contract row recording
/// one [`PreparedToolNeed`] per declared tool on every scoped unit. Needs
/// bind at declaration time — governing lockfiles discovered over the scan,
/// recipe from the row, toolchain from the unit — so the pipeline render
/// later emits consumer steps from stored facts without rereading the disk.
pub(crate) struct PreparedTool;

impl Primitive for PreparedTool {
    fn id(&self) -> &'static str {
        PREPARED_TOOL
    }

    fn schema(&self) -> &'static [&'static str] {
        &["tools", "recipes"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let tools = args.string_tables("tools")?.unwrap_or_default();
        let recipes = args.string_tables("recipes")?.unwrap_or_default();
        let declarations = parse_tool_declarations(&tools, &recipes)?;
        let mut units = Vec::new();
        for unit in ctx.units {
            let mut unit = (*unit).clone();
            let mut needs = Vec::new();
            for declaration in &declarations {
                let locks = governing_locks(ctx.shape.files(), &unit.root);
                let facts = inputs_facts_for(ctx.root, &locks, &declaration.recipe, &unit)?;
                needs.push(PreparedToolNeed {
                    tool_id: declaration.tool_id.clone(),
                    authorized_producers: declaration.authorized_producers.clone(),
                    inputs_digest: inputs_digest(&facts),
                });
            }
            unit.prepared_tools = needs;
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use std::time::Duration;

    use super::*;

    const TOOL: &str = "test-runner";
    const PRODUCER: &str = "producer-job";
    const ABI: &str = "Linux-X64";
    const INPUTS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_INPUTS: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}: missing value"),
        }
    }

    fn request(run_id: &str) -> ToolRequest {
        ToolRequest {
            tool_id: TOOL.to_owned(),
            inputs_digest: INPUTS.to_owned(),
            platform_abi: ABI.to_owned(),
            authorized_producers: BTreeSet::from([PRODUCER.to_owned()]),
            run_id: run_id.to_owned(),
        }
    }

    fn manifest_for(run_id: &str) -> ToolManifest {
        let files = vec![
            ToolFile {
                path: "bin/test-runner".to_owned(),
                sha256: hex_digest(b"runner-bytes"),
                executable: true,
            },
            ToolFile {
                path: "share/policy.json".to_owned(),
                sha256: hex_digest(b"{\"deny\":[]}"),
                executable: false,
            },
        ];
        let mut manifest = ToolManifest {
            tool_id: TOOL.to_owned(),
            inputs_digest: INPUTS.to_owned(),
            platform_abi: ABI.to_owned(),
            producer: ProducerIdentity {
                producer: PRODUCER.to_owned(),
                run_id: run_id.to_owned(),
            },
            outcome: ToolOutcome::Success,
            files,
            manifest_sha256: String::new(),
        };
        manifest.manifest_sha256 = manifest.canonical_digest();
        manifest
    }

    fn arrived_bytes() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            ("bin/test-runner".to_owned(), b"runner-bytes".to_vec()),
            ("share/policy.json".to_owned(), b"{\"deny\":[]}".to_vec()),
        ])
    }

    fn restamp(manifest: &mut ToolManifest) {
        manifest.manifest_sha256 = manifest.canonical_digest();
    }

    #[test]
    fn exact_current_run_verifies_and_keys_agree() {
        let request = request("42");
        let manifest = manifest_for("42");
        let verified = must(
            verify(&request, &manifest, &arrived_bytes()),
            "exact bundle verifies",
        );
        assert_eq!(verified.manifest(), &manifest);
        assert_eq!(
            requested_key(&request).as_str(),
            resolved_key(&manifest).as_str()
        );
        assert!(save_is_legal(resolved_key(&manifest).as_str(), &manifest));
        let (resolution, selected) = must(resolve(&request, [&manifest]), "exact resolves");
        assert!(resolution.is_exact);
        assert_eq!(selected.producer.run_id, "42");
        assert_eq!(resolution.requested.as_str(), resolution.resolved.as_str());
    }

    #[test]
    fn historical_fallback_keeps_requested_and_resolved_apart() {
        let request = request("99");
        let manifest = manifest_for("42");
        let (resolution, selected) = must(resolve(&request, [&manifest]), "fallback resolves");
        assert!(!resolution.is_exact);
        assert_eq!(selected.producer.run_id, "42");
        assert_ne!(resolution.requested.as_str(), resolution.resolved.as_str());
        assert!(resolution.requested.as_str().ends_with("-run-99"));
        assert!(resolution.resolved.as_str().ends_with("-run-42"));
        // The historical bug, pinned shut: fallback bytes saved under the
        // requested exact key would shadow the current run's own entry.
        assert!(!save_is_legal(resolution.requested.as_str(), &manifest));
        assert!(save_is_legal(resolution.resolved.as_str(), &manifest));
        must(
            verify(&request, &manifest, &arrived_bytes()),
            "fallback bytes still verify",
        );
    }

    #[test]
    fn exact_run_wins_over_a_newer_historical_candidate() {
        let request = request("42");
        let historical = manifest_for("7");
        let exact = manifest_for("42");
        let (_, selected) = must(
            resolve(&request, [&historical, &exact]),
            "resolve prefers exact",
        );
        assert_eq!(selected.producer.run_id, "42");
    }

    #[test]
    fn name_only_acceptance_is_impossible() {
        // The downloader flaw under test: accepting by tool name (and at
        // best expiry) while ignoring identity. Every neighboring field
        // fails closed on its own.
        fn retarget_tool(manifest: &mut ToolManifest) {
            manifest.tool_id = "other-tool".to_owned();
            restamp(manifest);
        }
        fn retarget_inputs(manifest: &mut ToolManifest) {
            manifest.inputs_digest = OTHER_INPUTS.to_owned();
            restamp(manifest);
        }
        fn retarget_abi(manifest: &mut ToolManifest) {
            manifest.platform_abi = "Linux-ARM64".to_owned();
            restamp(manifest);
        }
        fn retarget_producer(manifest: &mut ToolManifest) {
            manifest.producer.producer = "intruder-job".to_owned();
            restamp(manifest);
        }
        let request = request("42");
        for (field, mutate) in [
            ("tool id", retarget_tool as fn(&mut ToolManifest)),
            ("inputs digest", retarget_inputs as fn(&mut ToolManifest)),
            ("platform ABI", retarget_abi as fn(&mut ToolManifest)),
            ("producer", retarget_producer as fn(&mut ToolManifest)),
        ] {
            let mut manifest = manifest_for("42");
            mutate(&mut manifest);
            let Err(failure) = verify(&request, &manifest, &arrived_bytes()) else {
                panic!("{field} mismatch verified")
            };
            assert!(
                matches!(
                    failure,
                    HandoffFailure::Corrupt { .. } | HandoffFailure::Denied { .. }
                ),
                "{field} mismatch must fail closed, got {failure}"
            );
        }
    }

    #[test]
    fn tampered_manifest_fields_fail_the_digest() {
        let request = request("42");
        let mut manifest = manifest_for("42");
        manifest.producer.producer = "intruder-job".to_owned();
        let Err(failure) = verify(&request, &manifest, &arrived_bytes()) else {
            panic!("tampered manifest verified")
        };
        // The digest fires before the authorization check: tampering is
        // corruption, not a policy question.
        assert!(
            matches!(failure, HandoffFailure::Corrupt { .. }),
            "tampered manifest must be corrupt, got {failure}"
        );
    }

    #[test]
    fn failed_outcome_and_foreign_producer_are_denied() {
        let request = request("42");
        let mut failed = manifest_for("42");
        failed.outcome = ToolOutcome::Failed;
        restamp(&mut failed);
        let Err(failure) = verify(&request, &failed, &arrived_bytes()) else {
            panic!("failed outcome verified")
        };
        assert!(
            matches!(failure, HandoffFailure::Denied { .. }),
            "failed outcome must be denied, got {failure}"
        );
        let mut foreign = manifest_for("42");
        foreign.producer.producer = "intruder-job".to_owned();
        restamp(&mut foreign);
        let Err(failure) = verify(&request, &foreign, &arrived_bytes()) else {
            panic!("foreign producer verified")
        };
        assert!(
            matches!(failure, HandoffFailure::Denied { .. }),
            "foreign producer must be denied, got {failure}"
        );
    }

    #[test]
    fn byte_failures_are_corrupt() {
        let request = request("42");
        let manifest = manifest_for("42");
        let mut missing = arrived_bytes();
        must_some(missing.remove("bin/test-runner"), "fixture file exists");
        assert!(matches!(
            must_fail(&request, &manifest, &missing),
            HandoffFailure::Corrupt { .. }
        ));
        let mut swapped = arrived_bytes();
        swapped.insert("bin/test-runner".to_owned(), b"other-bytes".to_vec());
        assert!(matches!(
            must_fail(&request, &manifest, &swapped),
            HandoffFailure::Corrupt { .. }
        ));
        let mut extra = arrived_bytes();
        extra.insert("stowaway".to_owned(), b"unlisted".to_vec());
        must(
            verify(&request, &manifest, &extra),
            "unlisted arrivals are ignored",
        );
    }

    fn must_fail(
        request: &ToolRequest,
        manifest: &ToolManifest,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> HandoffFailure {
        let Err(failure) = verify(request, manifest, files) else {
            panic!("invalid bundle verified")
        };
        failure
    }

    #[test]
    fn unsafe_and_duplicate_paths_are_corrupt() {
        let request = request("42");
        for path in [
            "/absolute/path",
            "../escape",
            "nested/../../escape",
            "empty//component",
            "glob/**",
            "question?",
            "bracket[0]",
            "brace{a}",
            "back\\slash",
            ".",
            "",
        ] {
            let mut manifest = manifest_for("42");
            manifest.files = vec![ToolFile {
                path: path.to_owned(),
                sha256: hex_digest(b"x"),
                executable: false,
            }];
            restamp(&mut manifest);
            let files = BTreeMap::from([(path.to_owned(), b"x".to_vec())]);
            assert!(
                matches!(
                    must_fail(&request, &manifest, &files),
                    HandoffFailure::Corrupt { .. }
                ),
                "unsafe path `{path}` must be corrupt"
            );
        }
        let mut manifest = manifest_for("42");
        manifest.files.push(manifest.files[0].clone());
        restamp(&mut manifest);
        assert!(matches!(
            must_fail(&request, &manifest, &arrived_bytes()),
            HandoffFailure::Corrupt { .. }
        ));
        let mut empty = manifest_for("42");
        empty.files.clear();
        restamp(&mut empty);
        assert!(matches!(
            must_fail(&request, &empty, &arrived_bytes()),
            HandoffFailure::Corrupt { .. }
        ));
    }

    #[test]
    fn malformed_identity_fields_are_corrupt() {
        let request = request("42");
        for (field, mutate) in [
            ("tool slug", |m: &mut ToolManifest| {
                m.tool_id = "Not A Slug!".to_owned();
            }),
            ("inputs digest", |m: &mut ToolManifest| {
                m.inputs_digest = "xyz".to_owned();
            }),
            ("platform ABI", |m: &mut ToolManifest| {
                m.platform_abi = "linux".to_owned();
            }),
            ("producer slug", |m: &mut ToolManifest| {
                m.producer.producer = "UPPER".to_owned();
            }),
            ("run id", |m: &mut ToolManifest| {
                m.producer.run_id = "run-42".to_owned();
            }),
        ] as [(&str, fn(&mut ToolManifest)); 5]
        {
            let mut manifest = manifest_for("42");
            mutate(&mut manifest);
            // Restamping proves the shape check fires on its own: even a
            // self-consistent manifest with a malformed field fails.
            restamp(&mut manifest);
            assert!(
                matches!(
                    must_fail(&request, &manifest, &arrived_bytes()),
                    HandoffFailure::Corrupt { .. }
                ),
                "malformed {field} must be corrupt"
            );
        }
        assert!(is_platform_abi("Linux-X64"));
        assert!(!is_platform_abi("Linux"));
        assert!(!is_platform_abi("Linux-X64-extra"));
        assert!(!is_platform_abi("Linux X64"));
    }

    #[test]
    fn no_matching_candidate_is_a_miss() {
        let request = request("42");
        let mut wrong_inputs = manifest_for("41");
        wrong_inputs.inputs_digest = OTHER_INPUTS.to_owned();
        restamp(&mut wrong_inputs);
        let mut failed = manifest_for("42");
        failed.outcome = ToolOutcome::Failed;
        restamp(&mut failed);
        let Err(failure) = resolve(&request, [&wrong_inputs, &failed]) else {
            panic!("non-matching candidates resolved")
        };
        assert!(
            matches!(failure, HandoffFailure::Miss { .. }),
            "no match must be a miss, got {failure}"
        );
        let Err(failure) = resolve(&request, []) else {
            panic!("empty candidates resolved")
        };
        assert!(matches!(failure, HandoffFailure::Miss { .. }));
    }

    #[test]
    fn only_transient_is_retryable() {
        assert!(!HandoffFailure::Miss {
            detail: String::new()
        }
        .is_retryable());
        assert!(!HandoffFailure::Corrupt {
            detail: String::new()
        }
        .is_retryable());
        assert!(!HandoffFailure::Denied {
            detail: String::new()
        }
        .is_retryable());
        assert!(HandoffFailure::Transient {
            detail: String::new()
        }
        .is_retryable());
    }

    #[test]
    fn transfer_bounds_fire_as_transient() {
        let bounds = TransferBounds::DEFAULT;
        must(bounds.check_attempt(1), "first attempt runs");
        must(bounds.check_attempt(3), "last attempt runs");
        must(bounds.check_pages(10), "last page runs");
        must(bounds.check_wait(300), "final second runs");
        for result in [
            bounds.check_attempt(0),
            bounds.check_attempt(4),
            bounds.check_pages(11),
            bounds.check_wait(301),
        ] {
            let failure = match result {
                Ok(()) => panic!("bound overrun passed"),
                Err(failure) => failure,
            };
            assert!(
                matches!(failure, HandoffFailure::Transient { .. }),
                "bound overrun must be transient, got {failure}"
            );
            assert!(failure.is_retryable());
        }
    }

    #[test]
    fn canonical_digest_is_order_independent_and_field_complete() {
        let manifest = manifest_for("42");
        let mut reordered = manifest.clone();
        reordered.files.reverse();
        assert_eq!(
            manifest.canonical_digest(),
            reordered.canonical_digest(),
            "file order is not an identity fact"
        );
        let digest = manifest.canonical_digest();
        assert_eq!(digest.len(), 64);
        for mutate in [
            |m: &mut ToolManifest| m.tool_id.push('x'),
            |m: &mut ToolManifest| m.inputs_digest.push('x'),
            |m: &mut ToolManifest| m.platform_abi.push('x'),
            |m: &mut ToolManifest| m.producer.producer.push('x'),
            |m: &mut ToolManifest| m.producer.run_id.push('9'),
            |m: &mut ToolManifest| m.outcome = ToolOutcome::Failed,
            |m: &mut ToolManifest| m.files[0].path.push('x'),
            |m: &mut ToolManifest| m.files[0].sha256.push('x'),
            |m: &mut ToolManifest| m.files[0].executable = !m.files[0].executable,
        ] as [fn(&mut ToolManifest); 9]
        {
            let mut changed = manifest.clone();
            mutate(&mut changed);
            assert_ne!(
                changed.canonical_digest(),
                digest,
                "every field participates in the digest"
            );
        }
    }

    #[test]
    fn manifest_json_rejects_unknown_fields() {
        let manifest = manifest_for("42");
        let mut value = must(serde_json::to_value(&manifest), "manifest serializes");
        let object = must_some(value.as_object_mut(), "manifest is an object");
        object.insert("expiry".to_owned(), serde_json::json!("tomorrow"));
        let bytes = must(serde_json::to_vec(&value), "extended manifest serializes");
        assert!(matches!(
            ToolManifest::from_json(&bytes),
            Err(HandoffFailure::Corrupt { .. })
        ));
        // And the unstamped round trip verifies: parse is a faithful reader.
        let bytes = must(serde_json::to_vec(&manifest), "manifest serializes");
        let parsed = must(ToolManifest::from_json(&bytes), "manifest parses");
        assert_eq!(parsed, manifest);
        assert!(serde_json::from_slice::<ToolManifest>(b"not json").is_err());
    }

    #[test]
    fn install_lays_down_verified_bytes_atomically() {
        let request = request("42");
        let manifest = manifest_for("42");
        let verified = must(
            verify(&request, &manifest, &arrived_bytes()),
            "bundle verifies",
        );
        let plan = verified.install_plan();
        assert_eq!(plan.len(), 2);
        assert!(plan
            .iter()
            .any(|(path, _, executable)| { *path == "bin/test-runner" && *executable }));
        let root =
            std::env::temp_dir().join(format!("velnor-prepared-tool-{}", crate::unique_suffix()));
        let _ = std::fs::remove_dir_all(&root);
        must(std::fs::create_dir_all(&root), "fixture root");
        let destination = root.join("tool");
        must(verified.install_to(&destination), "install lands");
        assert_eq!(
            must(
                std::fs::read(destination.join("bin/test-runner")),
                "read installed binary"
            ),
            b"runner-bytes"
        );
        assert_eq!(
            must(
                std::fs::read(destination.join("share/policy.json")),
                "read installed data"
            ),
            b"{\"deny\":[]}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = must(
                std::fs::metadata(destination.join("bin/test-runner")),
                "stat binary",
            )
            .permissions()
            .mode()
                & 0o777;
            assert_eq!(mode, 0o755, "executable bit follows the manifest");
            let mode = must(
                std::fs::metadata(destination.join("share/policy.json")),
                "stat data",
            )
            .permissions()
            .mode()
                & 0o777;
            assert_eq!(mode, 0o644, "data stays non-executable");
        }
        // No overwrite, no partial state: a second install is refused and
        // the first tree is untouched.
        let refused = verified.install_to(&destination);
        assert!(refused.is_err());
        assert_eq!(
            must(
                std::fs::read(destination.join("bin/test-runner")),
                "tree intact"
            ),
            b"runner-bytes"
        );
        assert!(
            !destination.with_extension("staging").exists() && !root.join("tool.staging").exists(),
            "no staging residue escapes a refused install"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn walked(paths: &[&str]) -> Vec<String> {
        paths.iter().map(ToString::to_string).collect()
    }

    fn governed(files: &[String], unit_root: &str, kind: LockKind) -> Option<String> {
        governing_locks(files, unit_root).get(&kind).cloned()
    }

    #[test]
    fn discovery_binds_the_nearest_ancestor_lock_per_kind() {
        let files = walked(&[
            "Cargo.lock",
            "mise.lock",
            "crates/alpha/Cargo.lock",
            "crates/alpha/mise.lock",
            "crates/alpha/tools/Cargo.lock",
            "crates/beta/Cargo.toml",
        ]);
        // A root unit binds the root locks only.
        assert_eq!(
            governed(&files, ".", LockKind::Cargo).as_deref(),
            Some("Cargo.lock")
        );
        assert_eq!(
            governed(&files, ".", LockKind::Mise).as_deref(),
            Some("mise.lock")
        );
        assert_eq!(governing_locks(&files, ".").len(), 2);
        // A nested unit binds its own locks, not the root's and not a deeper
        // tree's.
        assert_eq!(
            governed(&files, "crates/alpha", LockKind::Cargo).as_deref(),
            Some("crates/alpha/Cargo.lock")
        );
        assert_eq!(
            governed(&files, "crates/alpha", LockKind::Mise).as_deref(),
            Some("crates/alpha/mise.lock")
        );
        // A sibling without its own locks inherits from above.
        assert_eq!(
            governed(&files, "crates/beta", LockKind::Cargo).as_deref(),
            Some("Cargo.lock")
        );
        assert_eq!(
            governed(&files, "crates/beta", LockKind::Mise).as_deref(),
            Some("mise.lock")
        );
    }

    #[test]
    fn discovery_ignores_unknown_filenames_and_sibling_trees() {
        let files = walked(&[
            "crates/alpha/Cargo.lock",
            "crates/alpha/vendored.lock",
            "crates/alpha/Gemfile.lock",
            "README.md",
        ]);
        assert!(governing_locks(&files, "crates/beta").is_empty());
        let nested = governing_locks(&files, "crates/alpha");
        assert_eq!(nested.len(), 1);
        assert_eq!(
            nested.get(&LockKind::Cargo).map(String::as_str),
            Some("crates/alpha/Cargo.lock")
        );
    }

    #[test]
    fn discovery_sees_swift_tool_owned_subdirectories() {
        let files = walked(&["pkg/.swiftpm/Package.resolved", "other/Package.resolved"]);
        assert_eq!(
            governed(&files, "pkg", LockKind::Swift).as_deref(),
            Some("pkg/.swiftpm/Package.resolved")
        );
        // Sibling trees and deeper nests never govern.
        assert_eq!(governed(&files, ".", LockKind::Swift), None);
        assert_eq!(
            governed(&files, "other/nested", LockKind::Swift).as_deref(),
            Some("other/Package.resolved")
        );
        let legacy = walked(&[".swiftpm/Package.resolved"]);
        assert_eq!(
            governed(&legacy, ".", LockKind::Swift).as_deref(),
            Some(".swiftpm/Package.resolved")
        );
    }

    #[test]
    fn discovery_breaks_same_kind_ties_by_vocabulary() {
        for order in [
            walked(&["web/bun.lockb", "web/bun.lock"]),
            walked(&["web/bun.lock", "web/bun.lockb"]),
        ] {
            assert_eq!(
                governed(&order, "web", LockKind::Bun).as_deref(),
                Some("web/bun.lock"),
                "walk order cannot flip the choice"
            );
        }
    }

    fn facts() -> InputsFacts {
        InputsFacts {
            locks: vec![("Cargo.lock".to_owned(), b"lock-bytes".to_vec())],
            sources: Vec::new(),
            recipe: vec!["cargo build --locked".to_owned()],
            toolchain: vec!["channel=1.91.1".to_owned()],
        }
    }

    #[test]
    fn inputs_digest_is_stable_and_field_complete() {
        let digest = inputs_digest(&facts());
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, inputs_digest(&facts()));
        // Lock order is not an identity fact.
        let mut reordered = facts();
        reordered
            .locks
            .push(("mise.lock".to_owned(), b"mise-bytes".to_vec()));
        let mut flipped = reordered.clone();
        flipped.locks.reverse();
        assert_eq!(inputs_digest(&reordered), inputs_digest(&flipped));
        // Every fact class participates: content, path, recipe, toolchain.
        let mut content = facts();
        content.locks[0].1 = b"other-bytes".to_vec();
        assert_ne!(inputs_digest(&content), digest);
        let mut renamed = facts();
        renamed.locks[0].0 = "crates/alpha/Cargo.lock".to_owned();
        assert_ne!(inputs_digest(&renamed), digest);
        let mut recipe = facts();
        recipe.recipe.push("cargo test --locked".to_owned());
        assert_ne!(inputs_digest(&recipe), digest);
        let mut toolchain = facts();
        toolchain.toolchain[0] = "channel=1.90.0".to_owned();
        assert_ne!(inputs_digest(&toolchain), digest);
        // Closure sources participate: path, bytes (even same-size edits),
        // and membership. Source order is not an identity fact.
        let mut sourced = facts();
        sourced.sources = vec![
            ("libs/b/src/lib.rs".to_owned(), b"fn b() {}".to_vec()),
            ("libs/a/src/lib.rs".to_owned(), b"fn a() {}".to_vec()),
        ];
        let sourced_digest = inputs_digest(&sourced);
        assert_ne!(sourced_digest, digest);
        let mut flipped_sources = sourced.clone();
        flipped_sources.sources.reverse();
        assert_eq!(inputs_digest(&flipped_sources), sourced_digest);
        let mut same_size = sourced.clone();
        same_size.sources[0].1 = b"fn c() {}".to_vec();
        assert_eq!(same_size.sources[0].1.len(), sourced.sources[0].1.len());
        assert_ne!(inputs_digest(&same_size), sourced_digest);
        let mut dropped = sourced.clone();
        dropped.sources.pop();
        assert_ne!(inputs_digest(&dropped), sourced_digest);
    }

    #[test]
    fn inputs_digest_without_sources_keeps_historical_bytes() {
        // Golden pin: an empty source list must emit no canonical lines, so
        // every pre-closure digest keeps its historical value byte for byte.
        assert_eq!(
            inputs_digest(&facts()),
            "7936e704637eee9ae49ef0dbd920c4a9122751301c0b56f3208d2fb237ea4067"
        );
    }

    #[test]
    fn toolchain_facts_flatten_the_pin() {
        let pinned = crate::s2::RustToolchain {
            channel: "1.91.1".to_owned(),
            components: vec!["rustfmt".to_owned()],
            targets: vec!["x86_64-unknown-linux-gnu".to_owned()],
            profile: Some("minimal".to_owned()),
        };
        assert_eq!(
            toolchain_facts(Some(&pinned), Some("bun@1.2.0")),
            vec![
                "channel=1.91.1".to_owned(),
                "component=rustfmt".to_owned(),
                "target=x86_64-unknown-linux-gnu".to_owned(),
                "profile=minimal".to_owned(),
                "tool-version=bun@1.2.0".to_owned(),
            ]
        );
        assert!(toolchain_facts(None, None).is_empty());
    }

    #[test]
    fn tool_declarations_validate_shapes() {
        let tools = BTreeMap::from([(
            TOOL.to_owned(),
            vec![PRODUCER.to_owned(), "second-producer".to_owned()],
        )]);
        let recipes = BTreeMap::from([(TOOL.to_owned(), vec!["cargo build --locked".to_owned()])]);
        let parsed = must(
            parse_tool_declarations(&tools, &recipes),
            "valid declarations parse",
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].tool_id, TOOL);
        assert_eq!(parsed[0].authorized_producers.len(), 2);
        assert_eq!(parsed[0].recipe, vec!["cargo build --locked".to_owned()]);
        // A tool without a recipe binds an empty one.
        let parsed = must(
            parse_tool_declarations(&tools, &BTreeMap::new()),
            "a missing recipe defaults",
        );
        assert!(parsed[0].recipe.is_empty());
        // Every malformed row fails closed.
        assert!(parse_tool_declarations(&BTreeMap::new(), &BTreeMap::new()).is_err());
        let unslung = BTreeMap::from([("Not A Slug!".to_owned(), vec![PRODUCER.to_owned()])]);
        assert!(parse_tool_declarations(&unslung, &BTreeMap::new()).is_err());
        let unmanned = BTreeMap::from([(TOOL.to_owned(), Vec::new())]);
        assert!(parse_tool_declarations(&unmanned, &BTreeMap::new()).is_err());
        let intruder = BTreeMap::from([(TOOL.to_owned(), vec!["UPPER".to_owned()])]);
        assert!(parse_tool_declarations(&intruder, &BTreeMap::new()).is_err());
        let orphan = BTreeMap::from([("other-tool".to_owned(), vec!["x".to_owned()])]);
        assert!(parse_tool_declarations(&tools, &orphan).is_err());
    }

    #[test]
    fn classify_and_verify_names_precise_verdicts() {
        let request = request("99");
        let manifest = manifest_for("42");
        let (resolution, _) = must(
            classify_and_verify(&request, &manifest, &arrived_bytes()),
            "fallback classifies",
        );
        assert!(!resolution.is_exact);
        assert!(resolution.requested.as_str().ends_with("-run-99"));
        assert!(resolution.resolved.as_str().ends_with("-run-42"));
        // A manifest that exists but mismatches is never a miss: the caller
        // gets the exact break — producer, ABI, bytes, or completeness.
        let mut foreign = manifest_for("42");
        foreign.producer.producer = "intruder-job".to_owned();
        restamp(&mut foreign);
        assert!(matches!(
            must_classify_fail(&request, &foreign, &arrived_bytes()),
            HandoffFailure::Denied { .. }
        ));
        let mut abi = manifest_for("42");
        abi.platform_abi = "Linux-ARM64".to_owned();
        restamp(&mut abi);
        assert!(matches!(
            must_classify_fail(&request, &abi, &arrived_bytes()),
            HandoffFailure::Corrupt { .. }
        ));
        let mut incomplete = arrived_bytes();
        must_some(
            incomplete.remove("share/policy.json"),
            "fixture file exists",
        );
        assert!(matches!(
            must_classify_fail(&request, &manifest, &incomplete),
            HandoffFailure::Corrupt { .. }
        ));
        let mut tampered = arrived_bytes();
        tampered.insert("bin/test-runner".to_owned(), b"forged-bytes".to_vec());
        assert!(matches!(
            must_classify_fail(&request, &manifest, &tampered),
            HandoffFailure::Corrupt { .. }
        ));
    }

    fn must_classify_fail(
        request: &ToolRequest,
        manifest: &ToolManifest,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> HandoffFailure {
        let Err(failure) = classify_and_verify(request, manifest, files) else {
            panic!("invalid bundle classified clean")
        };
        failure
    }

    #[test]
    fn api_outcomes_map_to_the_taxonomy() {
        assert!(api_failure(200, false).is_none());
        assert!(api_failure(204, false).is_none());
        assert!(matches!(
            api_failure(404, false),
            Some(HandoffFailure::Miss { .. })
        ));
        assert!(matches!(
            api_failure(410, false),
            Some(HandoffFailure::Miss { .. })
        ));
        assert!(matches!(
            api_failure(401, false),
            Some(HandoffFailure::Denied { .. })
        ));
        assert!(matches!(
            api_failure(403, false),
            Some(HandoffFailure::Denied { .. })
        ));
        assert!(matches!(
            api_failure(422, false),
            Some(HandoffFailure::Denied { .. })
        ));
        // Rate limits are transient, whether explicit or header-advertised,
        // and so is every server error.
        assert!(matches!(
            api_failure(429, false),
            Some(HandoffFailure::Transient { .. })
        ));
        assert!(matches!(
            api_failure(403, true),
            Some(HandoffFailure::Transient { .. })
        ));
        assert!(matches!(
            api_failure(500, false),
            Some(HandoffFailure::Transient { .. })
        ));
        assert!(matches!(
            api_failure(503, false),
            Some(HandoffFailure::Transient { .. })
        ));
        // Retryability follows the taxonomy, not the status text.
        assert!(!must_some(api_failure(403, false), "403 maps").is_retryable());
        assert!(must_some(api_failure(403, true), "limited 403 maps").is_retryable());
        assert!(!must_some(api_failure(404, false), "404 maps").is_retryable());
    }

    #[test]
    fn rate_limit_detection_reads_the_budget_headers() {
        assert!(response_is_rate_limited(429, ""));
        assert!(response_is_rate_limited(
            403,
            "HTTP/2 403\nx-ratelimit-remaining: 0\n"
        ));
        assert!(response_is_rate_limited(403, "X-RateLimit-Remaining: 0"));
        assert!(!response_is_rate_limited(403, "x-ratelimit-remaining: 42"));
        assert!(!response_is_rate_limited(403, ""));
        assert!(!response_is_rate_limited(200, ""));
    }

    fn api_response(status: u16, body: &str) -> ApiResponse {
        ApiResponse {
            status,
            headers: String::new(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn fetch_returns_the_conclusion_without_sleeping_on_success() {
        let bounds = TransferBounds::DEFAULT;
        let mut sleeps = Vec::new();
        let mut calls = 0;
        let mut executor = || -> Result<ApiResponse, String> {
            calls += 1;
            Ok(api_response(200, r#"{"conclusion":"success"}"#))
        };
        let conclusion = must(
            fetch_producer_conclusion(
                &bounds,
                "42",
                &mut executor,
                &mut |wait: Duration| sleeps.push(wait),
                &mut || Duration::ZERO,
            ),
            "fetch succeeds",
        );
        assert_eq!(conclusion, "success");
        assert_eq!(calls, 1);
        assert!(sleeps.is_empty());
    }

    #[test]
    fn fetch_retries_rate_limits_with_backoff() {
        let bounds = TransferBounds::DEFAULT;
        let mut statuses = vec![200_u16, 429, 429];
        let mut sleeps = Vec::new();
        let mut calls = 0;
        let mut executor = || -> Result<ApiResponse, String> {
            calls += 1;
            let status = statuses.pop().unwrap_or(200);
            Ok(api_response(status, r#"{"conclusion":"success"}"#))
        };
        let conclusion = must(
            fetch_producer_conclusion(
                &bounds,
                "42",
                &mut executor,
                &mut |wait: Duration| sleeps.push(wait),
                &mut || Duration::ZERO,
            ),
            "fetch retries",
        );
        assert_eq!(conclusion, "success");
        assert_eq!(calls, 3);
        assert_eq!(sleeps, vec![Duration::from_secs(1), Duration::from_secs(2)]);
    }

    #[test]
    fn fetch_exhausts_attempts_as_transient() {
        let bounds = TransferBounds::DEFAULT;
        let mut sleeps = Vec::new();
        let mut calls = 0;
        let mut executor = || -> Result<ApiResponse, String> {
            calls += 1;
            Ok(api_response(500, "flaked"))
        };
        let Err(OutcomeFetchError::Failure(failure)) = fetch_producer_conclusion(
            &bounds,
            "42",
            &mut executor,
            &mut |wait: Duration| sleeps.push(wait),
            &mut || Duration::ZERO,
        ) else {
            panic!("a flapping api must exhaust as transient")
        };
        assert!(matches!(failure, HandoffFailure::Transient { .. }));
        assert_eq!(calls, 3);
        assert_eq!(sleeps, vec![Duration::from_secs(1), Duration::from_secs(2)]);
    }

    #[test]
    fn fetch_refuses_denials_misses_and_broken_executors_immediately() {
        let bounds = TransferBounds::DEFAULT;
        for (status, name) in [(403_u16, "denied"), (404_u16, "miss")] {
            let mut sleeps = Vec::new();
            let mut calls = 0;
            let mut executor = || {
                calls += 1;
                Ok(api_response(status, "{}"))
            };
            let Err(OutcomeFetchError::Failure(failure)) = fetch_producer_conclusion(
                &bounds,
                "42",
                &mut executor,
                &mut |wait: Duration| sleeps.push(wait),
                &mut || Duration::ZERO,
            ) else {
                panic!("a {name} must not retry")
            };
            assert_eq!(outcome_name(&failure), name);
            assert_eq!(calls, 1);
            assert!(sleeps.is_empty());
        }
        let mut executor = || Err::<ApiResponse, String>("curl is missing".to_owned());
        let Err(OutcomeFetchError::Transport(detail)) =
            fetch_producer_conclusion(&bounds, "42", &mut executor, &mut |_| {}, &mut || {
                Duration::ZERO
            })
        else {
            panic!("a broken executor is a transport error, not taxonomy")
        };
        assert!(detail.contains("curl is missing"), "{detail}");
    }

    #[test]
    fn fetch_honors_the_wait_budget_and_the_page_floor() {
        let bounds = TransferBounds::DEFAULT;
        let mut sleeps = Vec::new();
        let mut calls = 0;
        let mut executor = || -> Result<ApiResponse, String> {
            calls += 1;
            Ok(api_response(500, "flaked"))
        };
        let Err(OutcomeFetchError::Failure(failure)) = fetch_producer_conclusion(
            &bounds,
            "42",
            &mut executor,
            &mut |wait: Duration| sleeps.push(wait),
            &mut || Duration::from_secs(1_000),
        ) else {
            panic!("an exhausted wait budget must refuse the sleep")
        };
        assert!(matches!(failure, HandoffFailure::Transient { .. }));
        assert_eq!(calls, 1);
        assert!(sleeps.is_empty());
        // A transfer configured for no pages refuses before any attempt.
        let pageless = TransferBounds {
            pages: 0,
            ..TransferBounds::DEFAULT
        };
        let mut executor = || Ok::<ApiResponse, String>(api_response(200, "{}"));
        let Err(OutcomeFetchError::Failure(failure)) =
            fetch_producer_conclusion(&pageless, "42", &mut executor, &mut |_| {}, &mut || {
                Duration::ZERO
            })
        else {
            panic!("a zero-page transfer must refuse")
        };
        assert!(matches!(failure, HandoffFailure::Transient { .. }));
    }

    #[test]
    fn fetch_treats_unreadable_run_bodies_as_transient() {
        let bounds = TransferBounds::DEFAULT;
        let mut calls = 0;
        let mut executor = || -> Result<ApiResponse, String> {
            calls += 1;
            Ok(api_response(200, "not json"))
        };
        let Err(OutcomeFetchError::Failure(failure)) =
            fetch_producer_conclusion(&bounds, "42", &mut executor, &mut |_| {}, &mut || {
                Duration::ZERO
            })
        else {
            panic!("an unreadable body is transient")
        };
        assert!(matches!(failure, HandoffFailure::Transient { .. }));
        assert_eq!(calls, 1);
        // Conclusions parse; a run without one is still running.
        assert_eq!(
            run_conclusion(r#"{"conclusion":"failure"}"#),
            Ok("failure".to_owned())
        );
        assert_eq!(
            run_conclusion(r#"{"conclusion":null}"#),
            Ok("in_progress".to_owned())
        );
        assert_eq!(run_conclusion("{}"), Ok("in_progress".to_owned()));
        assert!(run_conclusion("not json").is_err());
        assert!(run_conclusion("[]").is_err());
        assert!(run_conclusion(r#"{"conclusion":7}"#).is_err());
    }

    #[test]
    fn need_records_join_tool_digest_and_producers() {
        assert_eq!(
            need_record(&need()),
            format!("test-runner:{INPUTS}:producer-job")
        );
        let mut multi = need();
        multi.authorized_producers =
            BTreeSet::from(["first-job".to_owned(), "second-job".to_owned()]);
        let record = need_record(&multi);
        assert!(record.ends_with(":first-job+second-job"), "{record}");
        assert!(!record.contains(','));
        assert!(!record.contains(' '));
    }

    #[test]
    fn key_expressions_keep_requested_and_resolved_apart() {
        let exact = exact_key_expression(TOOL, INPUTS);
        let prefix = restore_prefix_expression(TOOL, INPUTS);
        assert!(exact.starts_with("prepared-tool-v1-test-runner-"));
        assert!(exact.contains(&INPUTS[..12]));
        assert!(exact.contains("${{ runner.os }}-${{ runner.arch }}"));
        assert!(exact.ends_with("-run-${{ github.run_id }}"));
        assert!(!prefix.contains("github.run_id"));
        assert_eq!(format!("{prefix}${{{{ github.run_id }}}}"), exact);
    }

    fn need() -> PreparedToolNeed {
        PreparedToolNeed {
            tool_id: TOOL.to_owned(),
            authorized_producers: BTreeSet::from([PRODUCER.to_owned()]),
            inputs_digest: INPUTS.to_owned(),
        }
    }

    #[test]
    fn consumer_steps_carry_the_request_and_the_pin_table() {
        let mut output = String::new();
        render_consumer_steps(&mut output, "CACHE_RESTORE_PIN", &[need()]);
        assert!(
            output.contains("uses: CACHE_RESTORE_PIN"),
            "the restore pin flows through, never a literal"
        );
        assert!(output.contains("Restore prepared tool test-runner"));
        assert!(output.contains("Install prepared tool test-runner"));
        assert!(output.contains(&exact_key_expression(TOOL, INPUTS)));
        assert!(output.contains(&restore_prefix_expression(TOOL, INPUTS)));
        assert!(output.contains("velnor-workflow prepared-tool-install"));
        assert!(output.contains("--tool \"test-runner\""));
        assert!(output.contains(&format!("--inputs \"{INPUTS}\"")));
        assert!(output.contains("--producers \"producer-job\""));
        assert!(output.contains("--abi \"$RUNNER_OS-$RUNNER_ARCH\""));
        assert!(output.contains("prepared-tool miss:"));
        assert!(output.contains("GITHUB_PATH"));
    }

    #[test]
    fn undeclared_units_render_no_consumer_steps() {
        let mut output = String::new();
        render_consumer_steps(&mut output, "CACHE_RESTORE_PIN", &[]);
        assert!(output.is_empty());
    }

    #[test]
    fn install_outputs_name_the_resolved_key_as_the_save_key() {
        let request = request("99");
        let manifest = manifest_for("42");
        let (resolution, _) = must(resolve(&request, [&manifest]), "fallback resolves");
        let outputs = format_install_outputs(&resolution);
        assert!(outputs.contains(&format!("save-key={}\n", resolution.resolved.as_str())));
        assert!(outputs.contains("is-exact=false\n"));
        assert!(outputs.contains("outcome=installed\n"));
        assert!(outputs.contains(&format!(
            "requested-key={}\n",
            resolution.requested.as_str()
        )));
        let check = format_save_check_output(resolution.resolved.as_str());
        assert!(check.contains("outcome=save-allowed\n"));
    }

    #[test]
    fn failure_outputs_name_the_taxonomy() {
        for (failure, name) in [
            (
                HandoffFailure::Miss {
                    detail: String::new(),
                },
                "miss",
            ),
            (
                HandoffFailure::Corrupt {
                    detail: String::new(),
                },
                "corrupt",
            ),
            (
                HandoffFailure::Denied {
                    detail: String::new(),
                },
                "denied",
            ),
            (
                HandoffFailure::Transient {
                    detail: String::new(),
                },
                "transient",
            ),
        ] {
            assert_eq!(format_failure_output(&failure), format!("outcome={name}\n"));
            assert_eq!(outcome_name(&failure), name);
        }
    }

    /// A throwaway repository with a generation config: the only way to prove
    /// the declaration row reaches the rendered surface.
    fn prepared_tool_fixture(name: &str, declare: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-prepared-tool-e2e-{name}-{}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        must(std::fs::create_dir_all(&root), "create fixture repository");
        must(
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            ),
            "write fixture manifest",
        );
        must(
            std::fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.91.1\"\n",
            ),
            "write fixture toolchain pin",
        );
        must(
            std::fs::write(root.join("Cargo.lock"), "# fixture lock\n"),
            "write fixture lock",
        );
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation input directory",
        );
        must(
            std::fs::write(
                root.join(crate::s2::config::GENERATION_CONFIG_PATH),
                format!(
                    "schema = 2\n\n[generator]\nrepository = \"example/prepared-tool-fixture\"\n{declare}"
                ),
            ),
            "write generation config",
        );
        root
    }

    fn generated_surface(root: &std::path::Path) -> crate::s2::primitives::Surface {
        let shape = must(
            crate::s2::scan::scan_shape(
                root,
                &crate::s2::provider::ProviderId::ALL.into_iter().collect(),
                "main",
                &[],
            ),
            "scan fixture repository",
        );
        let config = crate::s2::ProjectConfig::from(shape.clone());
        let path = root.join(crate::s2::config::GENERATION_CONFIG_PATH);
        let bytes = must(std::fs::read(&path), "read generation config");
        let generation = must(
            crate::s2::config::parse(&path, &bytes),
            "parse generation config",
        );
        must(
            crate::s2::primitives::generate(root, &shape, &config, Some(&generation)),
            "generate declared surface",
        )
    }

    const PREPARED_TOOL_ROW: &str = "[[declare]]\nprimitive = \"prepared-tool\"\n\n[declare.args.tools]\ntest-runner = [\"producer-job\"]\n\n[declare.args.recipes]\ntest-runner = [\"cargo build --locked\"]\n";

    #[test]
    fn declared_row_records_bound_needs_on_scanned_units() {
        let root = prepared_tool_fixture("declared", PREPARED_TOOL_ROW);
        let surface = generated_surface(&root);
        assert_eq!(surface.units.len(), 1);
        let needs = &surface.units[0].prepared_tools;
        assert_eq!(needs.len(), 1);
        assert_eq!(needs[0].tool_id, TOOL);
        assert_eq!(
            needs[0].authorized_producers,
            BTreeSet::from([PRODUCER.to_owned()])
        );
        assert_eq!(needs[0].inputs_digest.len(), 64);
        // The binding follows the governing lockfile: different committed
        // inputs mint a different digest for the same declaration.
        let other = prepared_tool_fixture("declared-relocked", PREPARED_TOOL_ROW);
        must(
            std::fs::write(other.join("Cargo.lock"), "# other lock\n"),
            "rewrite fixture lock",
        );
        let rebound = generated_surface(&other);
        assert_ne!(
            rebound.units[0].prepared_tools[0].inputs_digest,
            needs[0].inputs_digest
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn undeclared_surface_mentions_no_prepared_tool() {
        let root = prepared_tool_fixture("undeclared", "");
        let first = generated_surface(&root);
        let second = generated_surface(&root);
        assert_eq!(first.files, second.files);
        for (path, content) in &first.files {
            assert!(
                !content.contains("prepared-tool"),
                "{} mentions prepared tools without a declaration",
                path.display()
            );
        }
        assert!(first
            .units
            .iter()
            .all(|unit| unit.prepared_tools.is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }
}

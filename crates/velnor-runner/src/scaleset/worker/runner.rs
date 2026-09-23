//! Official runner container provisioning + tool-content verification.
//!
//! Homogeneous lane: every worker runs the SAME digest-pinned official
//! runner image (`ghcr.io/actions/actions-runner`) against its own private
//! DinD. Image references are [`PinnedImage`]s — `repo@sha256:<64 hex>`
//! only. Tags (`:latest`, `:v2`, any tag) cannot be constructed, so an
//! unpinned image can never reach a `docker run` argv; the type makes the
//! "never latest" rule structural, not procedural.
//!
//! The runner container:
//! * joins its DinD's network namespace (`--network container:<dind>`),
//!   so inner ports can never collide across workers and no bridge
//!   attachment is needed on the runner side;
//! * mounts the state dir, workspace volume, and tool/file-command dirs
//!   at IDENTICAL absolute paths to the DinD side (same bind sources,
//!   same guest paths), so a path minted inside one container names the
//!   same file in the other;
//! * receives the JIT config blob via a `0600` `--env-file` carrying the
//!   image's own `ACTIONS_RUNNER_INPUT_JITCONFIG` input, deleted
//!   immediately after `docker create`. The blob never appears in host
//!   argv (no process-list exposure), and GitHub App keys never enter the
//!   container: only the single-job JIT blob. The copy Docker keeps in
//!   the container config is unavoidable (same as ARC) and lives only
//!   until teardown removes the container first.
//!
//! Before any provision, the [`ToolContentHook`] proves the pulled bytes
//! are exactly the pinned content: `RepoDigests` must contain the pinned
//! `repo@digest`, and declared provenance labels must match. Admission then
//! requires independent platform and attestation proof for both images, plus
//! GitHub's cryptographic artifact-provenance verification for the official
//! runner image. An unavailable or incomplete proof fails closed before Docker
//! objects are created. The official runner image is passed through unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::dind::{verify_volume_holder_reference, BUILDKIT_CACHE_DIR, DIND_SOCKET, STATE_MOUNT};
use super::ownership::{
    WorkerIdentity, OWNERSHIP_LABEL, ROLE_RUNNER, RUNNER_LABEL, SCALE_SET_LABEL, WORKER_ROLE_LABEL,
};
use super::WorkerRunner;
use crate::docker::client::ContainerState;

/// Official runner image repository.
pub const RUNNER_REPOSITORY: &str = "ghcr.io/actions/actions-runner";
/// Runner version this lane provisions (recorded on every worker).
pub const RUNNER_VERSION: &str = "2.337.0";
/// `ghcr.io/actions/actions-runner:2.337.0` multi-arch index digest,
/// resolved 2026-09-17 via `docker buildx imagetools inspect` + raw sha256.
pub const RUNNER_INDEX_DIGEST: &str =
    "sha256:e5496277be5d09bc968b3d64911b74e219ac4a3f2edce956a3ecf9271bea1ef4";
/// Per-arch manifest digests under the index above.
pub const RUNNER_DIGEST_AMD64: &str =
    "sha256:5036480998280bb21e32ade9fe1b02b493861ac314b62ba1aea320b94f56ec97";
pub const RUNNER_DIGEST_ARM64: &str =
    "sha256:f5a0d9a3d857315f2aed7075a02a29f46927ad198221c3b1c66585ae9fe36c0d";
/// Provenance label the runner image must carry (verified live 2026-09-17).
pub const RUNNER_SOURCE_LABEL: &str = "org.opencontainers.image.source";
pub const RUNNER_SOURCE: &str = "https://github.com/actions/runner";
/// Repository identity enforced by GitHub's artifact-attestation verifier.
const RUNNER_ATTESTATION_REPOSITORY: &str = "actions/runner";
/// Trusted workflow identity that publishes the official runner image.
const RUNNER_ATTESTATION_WORKFLOW: &str = "actions/runner/.github/workflows/release.yml";
/// The provenance predicate emitted by `actions/attest-build-provenance`.
const RUNNER_ATTESTATION_PREDICATE: &str = "https://slsa.dev/provenance/v1";
/// GitHub Actions' Fulcio OIDC issuer.
const GITHUB_ACTIONS_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Official DinD image repository.
pub const DIND_REPOSITORY: &str = "docker";
/// DinD version this lane provisions (recorded on every worker).
pub const DIND_VERSION: &str = "28.5.2-dind";
/// `docker:28.5.2-dind` multi-arch index digest, resolved 2026-09-17.
/// (Identical to what `docker:28-dind` resolved to that day; the pin
/// names the exact patch tag so the next minor roll is a deliberate diff.)
pub const DIND_INDEX_DIGEST: &str =
    "sha256:2a232a42256f70d78e3cc5d2b5d6b3276710a0de0596c145f627ecfae90282ac";
pub const DIND_DIGEST_AMD64: &str =
    "sha256:9a06753d2401cd049b34cd27dbbc3e0db717d4c1db7bc7f2efad1c187e00bf5a";
pub const DIND_DIGEST_ARM64: &str =
    "sha256:145184796e8717376e73eaf29e16ede8ede2fd75e947a3fae7c05298e5e20d28";

/// OCI platform required for one worker pair.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImagePlatform {
    os: String,
    architecture: String,
}

impl ImagePlatform {
    /// Parse the canonical OCI form `os/architecture`.
    pub fn parse(raw: &str) -> Result<Self> {
        let mut parts = raw.split('/');
        let os = parts.next().unwrap_or_default();
        let architecture = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || os.is_empty()
            || architecture.is_empty()
            || os.chars().any(char::is_whitespace)
            || architecture.chars().any(char::is_whitespace)
        {
            anyhow::bail!("invalid OCI platform {raw:?}; expected os/architecture");
        }
        Ok(Self {
            os: os.to_string(),
            architecture: architecture.to_string(),
        })
    }

    /// Construct the supported Linux platform for a host architecture.
    #[must_use]
    pub fn linux_for_arch(arch: &str) -> Option<Self> {
        let architecture = match arch {
            "x86_64" | "amd64" => "amd64",
            "aarch64" | "arm64" => "arm64",
            _ => return None,
        };
        Some(Self {
            os: "linux".to_string(),
            architecture: architecture.to_string(),
        })
    }

    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    #[must_use]
    pub fn label(&self) -> String {
        format!("{}/{}", self.os, self.architecture)
    }
}

/// Env var carrying the JIT blob into the runner container (the image's
/// own input contract; cf. ARC's runner container).
pub const JIT_CONFIG_ENV: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";
/// Runner name env var (the image's own input contract).
pub const RUNNER_NAME_ENV: &str = "ACTIONS_RUNNER_INPUT_NAME";
/// Explicit command for the official runner image.
///
/// `actions/runner:2.337.0` has no entrypoint and its image default command
/// is `/bin/bash`; the JIT runner must be started explicitly instead.
pub const RUNNER_START_COMMAND: &str = "/home/runner/run.sh";
/// Work folder inside the runner container (guest-absolute).
pub const RUNNER_WORK_DIR: &str = "/home/runner/_work";
/// Tool cache dir, identical absolute path in both containers of a pair.
pub const TOOL_CACHE_DIR: &str = "/opt/hostedtoolcache";
/// File-command dir, identical absolute path in both containers.
pub const FILE_COMMAND_DIR: &str = "/velnor/file-commands";

/// A digest-pinned image reference: `repository@sha256:<64 hex>`.
///
/// Construction rejects everything else — tags (including `latest`),
/// short digests, non-sha256 algorithms, flag-shaped input — so every
/// provisioning path that takes a `PinnedImage` is pinned by construction.
/// [`crate::docker_argv::ImageReference`] stays the general parser for
/// workflow-controlled references; this type is the lane's stricter gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedImage {
    repository: String,
    digest: String,
}

impl PinnedImage {
    /// Parse `repository@sha256:<64 hex>`. Anything else is an error.
    pub fn parse(raw: &str) -> Result<Self, InvalidPinnedImage> {
        if raw.is_empty() || raw.starts_with('-') {
            return Err(InvalidPinnedImage::malformed(raw));
        }
        let Some((repository, digest)) = raw.split_once('@') else {
            return Err(InvalidPinnedImage::unpinned(raw));
        };
        if repository.is_empty() {
            return Err(InvalidPinnedImage::malformed(raw));
        }
        // A `:` past the registry host is a tag. (A port lives before the
        // first `/`; `ImageReference::parse` below re-checks the shape.)
        let path = repository
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(repository);
        if path.contains(':') {
            return Err(InvalidPinnedImage::unpinned(raw));
        }
        let Some(hex) = digest.strip_prefix("sha256:") else {
            return Err(InvalidPinnedImage::malformed(raw));
        };
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidPinnedImage::malformed(raw));
        }
        // Reuse the general reference parser as a second opinion on the
        // repository shape (lowercase, no whitespace/flags).
        if crate::docker_argv::ImageReference::parse(raw).is_err() {
            return Err(InvalidPinnedImage::malformed(raw));
        }
        Ok(Self {
            repository: repository.to_string(),
            digest: digest.to_string(),
        })
    }

    /// Full pinned reference for `docker pull`/`run`: `repo@sha256:...`.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}@{}", self.repository, self.digest)
    }

    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Why a [`PinnedImage`] parse failed. Every variant fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidPinnedImage {
    /// A tag reference (or bare name): content-addressing absent.
    Unpinned(String),
    /// Not even shaped like a reference.
    Malformed(String),
}

impl InvalidPinnedImage {
    fn unpinned(raw: &str) -> Self {
        Self::Unpinned(raw.to_string())
    }

    fn malformed(raw: &str) -> Self {
        Self::Malformed(raw.to_string())
    }
}

impl std::fmt::Display for InvalidPinnedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unpinned(raw) => write!(
                f,
                "image {raw:?} is not digest-pinned; the scale-set lane runs repo@sha256:<64 hex> only (never tags, never latest)"
            ),
            Self::Malformed(raw) => write!(f, "image {raw:?} is not a valid pinned reference"),
        }
    }
}

impl std::error::Error for InvalidPinnedImage {}

/// The homogeneous worker profile: the two pinned images + versions.
///
/// One lane, one profile: every worker provisions from the same pins, so
/// any worker can serve any acquired job and tool drift between workers
/// is impossible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomogeneousProfile {
    runner: PinnedImage,
    dind: PinnedImage,
    platform: ImagePlatform,
}

impl HomogeneousProfile {
    /// Profile for `arch` (`x86_64`/`aarch64`). The configured image
    /// references remain digest-only; the engine platform is admitted
    /// separately and must match this profile exactly.
    pub fn for_arch(arch: &str) -> Option<Self> {
        Some(Self::pinned(ImagePlatform::linux_for_arch(arch)?))
    }

    /// Profile for the host the daemon runs on. `None` on unprovisioned
    /// architectures: refusing is correct, guessing a foreign arch's
    /// bytes is not.
    #[must_use]
    pub fn host() -> Option<Self> {
        Self::for_arch(std::env::consts::ARCH)
    }

    /// The pinned profile. Infallible: the constants above are valid by
    /// construction (proven by `production_pins_parse`).
    fn pinned(platform: ImagePlatform) -> Self {
        // Proof: `PinnedImage::parse` on a literal either holds for every
        // build or fails every build; the unit test pins the literals, so
        // a bad constant breaks the build at test time, not in production.
        let runner = PinnedImage {
            repository: RUNNER_REPOSITORY.to_string(),
            digest: RUNNER_INDEX_DIGEST.to_string(),
        };
        let dind = PinnedImage {
            repository: DIND_REPOSITORY.to_string(),
            digest: DIND_INDEX_DIGEST.to_string(),
        };
        Self {
            runner,
            dind,
            platform,
        }
    }

    #[must_use]
    pub fn runner(&self) -> &PinnedImage {
        &self.runner
    }

    #[must_use]
    pub fn dind(&self) -> &PinnedImage {
        &self.dind
    }

    #[must_use]
    pub fn platform(&self) -> &ImagePlatform {
        &self.platform
    }

    /// Recorded versions: `(runner_version, dind_version)`.
    #[must_use]
    pub fn versions(&self) -> (&'static str, &'static str) {
        (RUNNER_VERSION, DIND_VERSION)
    }
}

/// What the tool-content hook proved about one pulled image.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolContentAttestation {
    /// Pinned reference that was pulled.
    pub reference: String,
    /// Engine-resolved image id (`{{.Id}}`).
    pub image_id: String,
    /// Declared content version from the pin (runner) or `dind` version.
    pub content_version: String,
    /// Provenance source label when the pin declares one.
    pub source: Option<String>,
}

/// Content expectations for one pinned image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContentExpectation {
    /// Required `org.opencontainers.image.source` value, if any.
    pub source: Option<String>,
    /// Version recorded on the attestation (pin metadata).
    pub content_version: String,
    /// OCI platform the engine must prove for the pulled image.
    pub platform: ImagePlatform,
}

impl ToolContentExpectation {
    #[must_use]
    pub fn runner() -> Self {
        Self::runner_for(&ImagePlatform::linux_for_arch("amd64").expect("amd64 platform"))
    }

    #[must_use]
    pub fn runner_for(platform: &ImagePlatform) -> Self {
        Self {
            source: Some(RUNNER_SOURCE.to_string()),
            content_version: RUNNER_VERSION.to_string(),
            platform: platform.clone(),
        }
    }

    #[must_use]
    pub fn dind() -> Self {
        Self::dind_for(&ImagePlatform::linux_for_arch("amd64").expect("amd64 platform"))
    }

    #[must_use]
    pub fn dind_for(platform: &ImagePlatform) -> Self {
        // The dind index carries no config labels (`null` at index level,
        // verified live); its content proof is the digest match alone.
        Self {
            source: None,
            content_version: DIND_VERSION.to_string(),
            platform: platform.clone(),
        }
    }

    #[must_use]
    pub fn platform(&self) -> &ImagePlatform {
        &self.platform
    }
}

/// Proves pulled bytes are exactly the pinned content, before provisioning.
///
/// The hook runs `pull → RepoDigests match → provenance-label match` and
/// fails closed on any mismatch, missing field, or transport error. A
/// custom implementation can add signature verification; the default
/// below is the digest+provenance proof.
pub trait ToolContentHook {
    /// Verify `image` against `expected`; returns the attestation the
    /// provision record stores, or an error that blocks provisioning.
    fn verify(
        &self,
        runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
    ) -> Result<ToolContentAttestation>;

    /// Prove the engine selected the exact configured OCI platform.
    ///
    /// The default is deliberately rejecting: a content hook that does not
    /// implement platform proof cannot admit a worker.
    fn verify_platform(
        &self,
        _runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ImagePlatform,
    ) -> Result<()> {
        anyhow::bail!(
            "platform proof hook missing for {} (expected {})",
            image.reference(),
            expected.label()
        );
    }

    /// Prove the attestation binds to the exact configured image and pin.
    ///
    /// The default is deliberately rejecting so custom hooks must opt into
    /// this boundary explicitly.
    fn verify_attestation(
        &self,
        _runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> Result<()> {
        anyhow::bail!("attestation proof hook missing for {}", image.reference());
    }

    /// Prove the image signature or equivalent trusted release proof when the
    /// expectation declares a signed release source. The admission driver
    /// fails closed for that image class when this hook is absent.
    fn verify_signature(
        &self,
        _runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> Result<()> {
        anyhow::bail!("signature proof hook missing for {}", image.reference());
    }
}

/// Default hook: digest match on `RepoDigests` + declared label match, exact
/// platform selection, and GitHub artifact-provenance verification for the
/// official runner image.
///
/// Mirrors the release-activation image proof
/// (`release::verify_and_tag_release_image`): pull, then compare the
/// Engine's own `RepoDigests` report against the pinned `repo@digest`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DockerToolContentHook;

impl ToolContentHook for DockerToolContentHook {
    fn verify(
        &self,
        runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
    ) -> Result<ToolContentAttestation> {
        let reference = image.reference();
        let pulled = runner
            .run("docker", &pull_args(&reference))
            .with_context(|| format!("pull pinned image {reference}"))?;
        if pulled.code != 0 {
            anyhow::bail!(
                "pull pinned image {reference} exited {}: {}",
                pulled.code,
                pulled.stderr.trim()
            );
        }
        let digests_out = runner
            .run("docker", &repo_digests_args(&reference))
            .with_context(|| format!("inspect RepoDigests of {reference}"))?;
        if digests_out.code != 0 {
            anyhow::bail!(
                "inspect RepoDigests of {reference} exited {}: {}",
                digests_out.code,
                digests_out.stderr.trim()
            );
        }
        let digests: Vec<String> = serde_json::from_str(digests_out.stdout.trim())
            .with_context(|| format!("parse RepoDigests of {reference}"))?;
        // The Engine reports what IT pulled; the pin is what WE asked for.
        // Both spellings (`docker@...` and `docker.io/library/docker@...`)
        // name the same content, so compare digest suffixes, not prefixes.
        let digest_match = digests.iter().any(|digest| {
            digest == &reference
                || digest
                    .strip_prefix("docker.io/library/")
                    .is_some_and(|rest| rest == reference)
        });
        if !digest_match {
            anyhow::bail!(
                "pulled image {reference} digest disagrees with pin: engine reports {digests:?}"
            );
        }
        let labels_out = runner
            .run("docker", &config_labels_args(&reference))
            .with_context(|| format!("inspect labels of {reference}"))?;
        if labels_out.code != 0 {
            anyhow::bail!(
                "inspect labels of {reference} exited {}: {}",
                labels_out.code,
                labels_out.stderr.trim()
            );
        }
        let labels: Option<BTreeMap<String, String>> =
            serde_json::from_str(labels_out.stdout.trim())
                .with_context(|| format!("parse labels of {reference}"))?;
        if let Some(expected_source) = &expected.source {
            let found = labels
                .as_ref()
                .and_then(|labels| labels.get(RUNNER_SOURCE_LABEL));
            if found != Some(expected_source) {
                anyhow::bail!(
                    "pulled image {reference} provenance disagrees with pin: \
                     {RUNNER_SOURCE_LABEL} is {found:?}, expected {expected_source:?}"
                );
            }
        }
        let id_out = runner
            .run("docker", &crate::docker::client::image_id_args(&reference))
            .with_context(|| format!("inspect id of {reference}"))?;
        if id_out.code != 0 {
            anyhow::bail!(
                "inspect id of {reference} exited {}: {}",
                id_out.code,
                id_out.stderr.trim()
            );
        }
        Ok(ToolContentAttestation {
            reference,
            image_id: id_out.stdout.trim().to_string(),
            content_version: expected.content_version.clone(),
            source: expected.source.clone(),
        })
    }

    fn verify_platform(
        &self,
        runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ImagePlatform,
    ) -> Result<()> {
        let reference = image.reference();
        let inspected = runner
            .run("docker", &platform_args(&reference))
            .with_context(|| format!("inspect platform of {reference}"))?;
        if inspected.code != 0 {
            anyhow::bail!(
                "inspect platform of {reference} exited {}: {}",
                inspected.code,
                inspected.stderr.trim()
            );
        }
        let actual = ImagePlatform::parse(inspected.stdout.trim())
            .with_context(|| format!("parse platform of {reference}"))?;
        if actual != *expected {
            anyhow::bail!(
                "image {reference} platform disagrees with configured platform: engine reports {}, expected {}",
                actual.label(),
                expected.label()
            );
        }
        Ok(())
    }

    fn verify_attestation(
        &self,
        _runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
        attestation: &ToolContentAttestation,
    ) -> Result<()> {
        validate_tool_content_attestation(image, expected, attestation)
    }

    fn verify_signature(
        &self,
        runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> Result<()> {
        if image.repository() != RUNNER_REPOSITORY
            || expected.source.as_deref() != Some(RUNNER_SOURCE)
        {
            anyhow::bail!(
                "GitHub artifact-provenance policy is defined only for the official runner image; refusing {}",
                image.reference()
            );
        }

        let reference = image.reference();
        let output = runner
            .run("gh", &github_attestation_verify_args(image))
            .map_err(|error| {
                anyhow::anyhow!(
                    "GitHub artifact-provenance verification is unknown for {reference}: {error:#}"
                )
            })?;
        if output.code != 0 {
            anyhow::bail!(
                "GitHub artifact-provenance verification rejected {reference}: {}",
                output.stderr.trim()
            );
        }
        validate_github_attestation_output(&output.stdout, image).with_context(|| {
            format!("GitHub artifact-provenance verification is unknown for {reference}")
        })
    }
}

/// Validate the hook's returned proof before any worker-owned Docker object
/// is created. In particular, a custom hook cannot attest a different image
/// or silently omit the engine identity/provenance fields.
pub(crate) fn validate_tool_content_attestation(
    image: &PinnedImage,
    expected: &ToolContentExpectation,
    attestation: &ToolContentAttestation,
) -> Result<()> {
    let reference = image.reference();
    if attestation.reference != reference {
        anyhow::bail!(
            "attestation reference disagrees with configured image: got {:?}, expected {:?}",
            attestation.reference,
            reference
        );
    }
    if attestation.image_id.trim().is_empty() {
        anyhow::bail!("attestation for {reference} is missing the engine image id");
    }
    if attestation.content_version != expected.content_version {
        anyhow::bail!(
            "attestation content version for {reference} disagrees with configured version: got {:?}, expected {:?}",
            attestation.content_version,
            expected.content_version
        );
    }
    if attestation.source != expected.source {
        anyhow::bail!(
            "attestation provenance for {reference} disagrees with configured source: got {:?}, expected {:?}",
            attestation.source,
            expected.source
        );
    }
    Ok(())
}

/// Run every worker-admission proof before creating a network or container.
///
/// The order is intentional: the configured digest is checked by `verify`,
/// the hook result is bound to that exact reference, then platform and
/// attestation proofs are required. Signed release proof is additionally
/// required when the expectation declares a source contract. Any missing
/// applicable proof stops admission before worker-owned Docker state exists.
pub(crate) fn admit_tool_content(
    hook: &dyn ToolContentHook,
    runner: &mut dyn WorkerRunner,
    image: &PinnedImage,
    expected: &ToolContentExpectation,
) -> Result<ToolContentAttestation> {
    let attestation = hook.verify(runner, image, expected)?;
    validate_tool_content_attestation(image, expected, &attestation)?;
    hook.verify_platform(runner, image, expected.platform())?;
    hook.verify_attestation(runner, image, expected, &attestation)?;
    // The official runner image has a signed GitHub artifact-provenance
    // contract. DinD has no equivalent upstream contract, so do not claim a
    // signature proof for it; its immutable digest, selected platform, and
    // engine-bound attestation remain mandatory above.
    if expected.source.is_some() {
        hook.verify_signature(runner, image, expected, &attestation)?;
    }
    Ok(attestation)
}

fn github_attestation_verify_args(image: &PinnedImage) -> Vec<String> {
    vec![
        "attestation".to_string(),
        "verify".to_string(),
        format!("oci://{}", image.reference()),
        "--repo".to_string(),
        RUNNER_ATTESTATION_REPOSITORY.to_string(),
        "--signer-workflow".to_string(),
        RUNNER_ATTESTATION_WORKFLOW.to_string(),
        "--predicate-type".to_string(),
        RUNNER_ATTESTATION_PREDICATE.to_string(),
        "--cert-oidc-issuer".to_string(),
        GITHUB_ACTIONS_OIDC_ISSUER.to_string(),
        "--deny-self-hosted-runners".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ]
}

/// Validate the structured result emitted by `gh attestation verify`.
///
/// The verifier's exit status is not treated as a configurable boolean. A
/// successful result must also contain a non-empty, cryptographically verified
/// certificate/timestamp record and an in-toto subject bound to this exact
/// image repository and digest. Any schema drift is unknown and rejects.
fn validate_github_attestation_output(stdout: &str, image: &PinnedImage) -> Result<()> {
    let document: serde_json::Value = serde_json::from_str(stdout.trim())
        .context("parse GitHub artifact-provenance verifier JSON")?;
    let Some(entries) = document.as_array() else {
        anyhow::bail!("GitHub artifact-provenance verifier returned a non-array result");
    };
    if entries.is_empty() {
        anyhow::bail!("GitHub artifact-provenance verifier returned no verified attestations");
    }

    let Some(expected_digest) = image.digest().strip_prefix("sha256:") else {
        anyhow::bail!("configured runner image digest is not sha256");
    };
    let exact_subject = entries.iter().any(|entry| {
        let Some(verification) = entry.get("verificationResult") else {
            return false;
        };
        if verification
            .pointer("/signature/certificate")
            .and_then(serde_json::Value::as_object)
            .is_none()
        {
            return false;
        }
        let Some(timestamps) = verification
            .get("verifiedTimestamps")
            .and_then(serde_json::Value::as_array)
        else {
            return false;
        };
        if timestamps.is_empty() {
            return false;
        }
        let Some(statement) = verification.get("statement") else {
            return false;
        };
        if statement
            .get("predicateType")
            .and_then(serde_json::Value::as_str)
            != Some(RUNNER_ATTESTATION_PREDICATE)
        {
            return false;
        }
        let Some(subjects) = statement
            .get("subject")
            .and_then(serde_json::Value::as_array)
        else {
            return false;
        };
        subjects.iter().any(|subject| {
            subject.get("name").and_then(serde_json::Value::as_str) == Some(image.repository())
                && subject
                    .pointer("/digest/sha256")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected_digest)
        })
    });
    if !exact_subject {
        anyhow::bail!(
            "GitHub artifact-provenance verifier result is not bound to {}",
            image.reference()
        );
    }
    Ok(())
}

fn pull_args(reference: &str) -> Vec<String> {
    vec!["pull".to_string(), "--".to_string(), reference.to_string()]
}

fn repo_digests_args(reference: &str) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{json .RepoDigests}}".to_string(),
        "--".to_string(),
        reference.to_string(),
    ]
}

fn config_labels_args(reference: &str) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{json .Config.Labels}}".to_string(),
        "--".to_string(),
        reference.to_string(),
    ]
}

fn platform_args(reference: &str) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{.Os}}/{{.Architecture}}".to_string(),
        "--".to_string(),
        reference.to_string(),
    ]
}

/// Fully-derived runner provision spec.
///
/// `Debug` never prints the JIT blob: presence-only, mirroring
/// [`ActionsAuth`][crate::scaleset::ActionsAuth].
#[derive(Clone)]
pub struct RunnerSpec {
    identity: WorkerIdentity,
    image: PinnedImage,
    state_dir: PathBuf,
    /// Encoded JIT config blob (secret-adjacent: never logged, never
    /// recorded — passed to `docker create --env-file` only).
    jit_config: String,
}

impl std::fmt::Debug for RunnerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerSpec")
            .field("identity", &self.identity)
            .field("image", &self.image)
            .field("state_dir", &self.state_dir)
            .field("jit_config", &"<redacted>")
            .finish()
    }
}

/// Name of the JIT env file in a private host-only directory. Written
/// `0600` just before `docker create`, deleted right after.
const JIT_ENV_FILE: &str = "jit.env";

impl RunnerSpec {
    /// Derive the spec. The JIT blob travels into the container via a
    /// `0600` host-only `--env-file` outside the runner's state bind mount,
    /// deleted right after `docker create`; it never appears in host argv
    /// or the journal (fingerprints only). The copy Docker keeps in the
    /// container config is unavoidable and lives only until teardown
    /// removes the container.
    #[must_use]
    pub fn new(
        identity: WorkerIdentity,
        image: PinnedImage,
        state_dir: &Path,
        jit_config: &str,
    ) -> Self {
        Self {
            identity,
            image,
            state_dir: state_dir.to_path_buf(),
            jit_config: jit_config.to_string(),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &WorkerIdentity {
        &self.identity
    }

    #[must_use]
    pub fn image(&self) -> &PinnedImage {
        &self.image
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Write the JIT blob to a private host-only `0600` env file for
    /// `--env-file`. The path is a sibling of the bind-mounted state dir,
    /// never inside the runner-visible mount.
    ///
    /// The blob is line-oriented secret material: a value containing
    /// `\n` or `\r` fails closed instead of corrupting the file or
    /// injecting variables. The caller scrubs it immediately after
    /// `docker create` (see [`ensure_runner`]).
    pub fn write_env_file(&self) -> Result<PathBuf> {
        if self.jit_config.bytes().any(|b| b == b'\n' || b == b'\r') {
            anyhow::bail!("JIT config blob must be a single line for --env-file");
        }
        self.scrub_jit_env_files()?;
        let path = self.jit_env_file_path()?;
        let dir = path.parent().context("JIT env file path has no parent")?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create private JIT env file dir {}", dir.display()));
            }
        }
        let metadata = std::fs::symlink_metadata(dir)
            .with_context(|| format!("inspect private JIT env file dir {}", dir.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            anyhow::bail!(
                "refusing JIT env file dir {}: not a real directory",
                dir.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("restrict private JIT env file dir {}", dir.display()))?;
        }
        let contents = format!("{JIT_CONFIG_ENV}={}\n", self.jit_config);
        let write_result = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            return match self.scrub_jit_env_files() {
                Ok(()) => {
                    Err(error).with_context(|| format!("write JIT env file {}", path.display()))
                }
                Err(scrub_error) => anyhow::bail!(
                    "write JIT env file {} failed: {error}; cleanup failed: {scrub_error:#}",
                    path.display()
                ),
            };
        }
        Ok(path)
    }

    fn jit_env_file_path(&self) -> Result<PathBuf> {
        let parent = self
            .state_dir
            .parent()
            .context("worker state dir has no parent for private JIT env file")?;
        let state_name = self
            .state_dir
            .file_name()
            .context("worker state dir has no final path component")?
            .to_string_lossy();
        Ok(parent
            .join(format!(".velnor-jit-{state_name}"))
            .join(JIT_ENV_FILE))
    }

    /// Remove both current host-only files and the legacy env file that
    /// older provisioners placed inside the runner-visible state mount.
    fn scrub_jit_env_files(&self) -> Result<()> {
        let external = self.jit_env_file_path()?;
        if let Some(dir) = external.parent() {
            scrub_private_env_dir(dir)?;
        }
        remove_secret_file(&self.state_dir.join(JIT_ENV_FILE))?;
        Ok(())
    }

    pub(crate) fn scrub_jit_env_files_for_state(state_dir: &Path) -> Result<()> {
        let parent = state_dir
            .parent()
            .context("worker state dir has no parent for private JIT env file")?;
        let state_name = state_dir
            .file_name()
            .context("worker state dir has no final path component")?
            .to_string_lossy();
        let external_dir = parent.join(format!(".velnor-jit-{state_name}"));
        scrub_private_env_dir(&external_dir)?;
        remove_secret_file(&state_dir.join(JIT_ENV_FILE))?;
        Ok(())
    }

    /// `docker create` argv for the runner container.
    ///
    /// Invariants (proven by unit tests on this vector):
    /// * `--network container:<dind>`: the runner shares ONLY its own
    ///   DinD's network namespace — no bridge, no published ports, no
    ///   way to observe another worker's inner ports;
    /// * state dir, workspace, tool cache, and file-command dirs mount
    ///   at identical absolute paths to the DinD side;
    /// * the host Docker socket is never mounted;
    /// * App keys never appear, and the JIT blob never appears in argv:
    ///   it travels via `--env-file` only.
    #[must_use]
    pub fn create_args_with_env_file(&self, env_file: &Path) -> Vec<String> {
        let cargo_registry = self
            .state_dir
            .parent()
            .unwrap_or(&self.state_dir)
            .join("cargo/registry");
        let cargo_git = self
            .state_dir
            .parent()
            .unwrap_or(&self.state_dir)
            .join("cargo/git");
        let _ = std::fs::create_dir_all(&cargo_registry);
        let _ = std::fs::create_dir_all(&cargo_git);
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            self.identity.runner_container(),
            "--network".to_string(),
            format!("container:{}", self.identity.dind_container()),
            "--env-file".to_string(),
            env_file.display().to_string(),
            "--env".to_string(),
            format!("{RUNNER_NAME_ENV}={}", self.identity.runner_name()),
            "--env".to_string(),
            format!("DOCKER_HOST=unix://{DIND_SOCKET}"),
            "--env".to_string(),
            "RUNNER_WORK_FOLDER=".to_string() + RUNNER_WORK_DIR,
            "--volume".to_string(),
            format!("{}:{STATE_MOUNT}", self.state_dir.display()),
            "--volumes-from".to_string(),
            self.identity.volume_holder_container(),
            "--volume".to_string(),
            format!(
                "{}:{BUILDKIT_CACHE_DIR}",
                self.state_dir.join("buildkit-cache").display()
            ),
            "--volume".to_string(),
            format!("{}:/home/runner/.cargo/registry", cargo_registry.display()),
            "--volume".to_string(),
            format!("{}:/home/runner/.cargo/git", cargo_git.display()),
        ];
        args.extend(self.identity.label_args(ROLE_RUNNER));
        args.push("--".to_string());
        args.push(self.image.reference().to_string());
        // The official image has no ENTRYPOINT and defaults to /bin/bash.
        // Invoke its JIT runner explicitly. Toolchain installation, curl, and
        // floating `latest` resolution remain forbidden on the worker path;
        // only the pinned image's published runner script is invoked.
        args.push(RUNNER_START_COMMAND.to_string());
        args
    }
}

fn scrub_private_env_dir(dir: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect JIT env dir {}", dir.display()))
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!(
            "refusing JIT cleanup of {}: not a real directory",
            dir.display()
        );
    }
    remove_secret_file(&dir.join(JIT_ENV_FILE))?;
    std::fs::remove_dir(dir)
        .with_context(|| format!("remove private JIT env dir {}", dir.display()))?;
    Ok(())
}

fn remove_secret_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("scrub secret file {}", path.display())),
    }
}

impl WorkerIdentity {
    fn runner_name(&self) -> &str {
        self.ownership().runner_name()
    }
}

/// What [`ensure_runner`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerProvision {
    /// A live container with matching ownership labels was adopted.
    Adopted,
    /// A fresh runner container was created and started.
    Created,
}

/// Non-secret persisted command state for an existing runner container.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunnerContainerConfig {
    image: String,
    labels: BTreeMap<String, String>,
    network_mode: String,
    volumes_from: Option<Vec<String>>,
    entrypoint: Vec<String>,
    command: Option<Vec<String>>,
    /// Exact Docker lifecycle word. Unknown words remain `Some` here and are
    /// rejected by the lifecycle policy below; the projection never falls
    /// back to the lossy `.State.Running` boolean.
    status: String,
}

/// Parse the deliberately narrow `docker inspect --format` projection used
/// by [`ensure_runner`]. Never inspect `.Config.Env`: it can contain JIT data.
fn parse_runner_container_config(output: &str) -> Result<RunnerContainerConfig> {
    let mut fields = output.trim().split('\t');
    let image: String = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted image")?,
    )
    .context("parse runner container image")?;
    let labels: BTreeMap<String, String> = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted labels")?,
    )
    .context("parse runner container labels")?;
    let network_mode: String = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted network mode")?,
    )
    .context("parse runner container network mode")?;
    let volumes_from: Option<Vec<String>> = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted volume holder references")?,
    )
    .context("parse runner container volume holder references")?;
    let entrypoint: Option<Vec<String>> = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted entrypoint")?,
    )
    .context("parse runner container entrypoint")?;
    let command: Option<Vec<String>> = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted command")?,
    )
    .context("parse runner container command")?;
    let status: String = serde_json::from_str(
        fields
            .next()
            .context("runner container inspect omitted lifecycle status")?,
    )
    .context("parse runner container lifecycle status")?;
    if status.trim().is_empty() {
        anyhow::bail!("runner container inspect returned empty lifecycle status");
    }
    if fields.next().is_some() {
        anyhow::bail!("runner container inspect returned extra fields");
    }
    Ok(RunnerContainerConfig {
        image,
        labels,
        network_mode,
        volumes_from,
        entrypoint: entrypoint.unwrap_or_default(),
        command,
        status,
    })
}

fn runner_status_is_running(status: &str) -> bool {
    matches!(
        crate::docker::client::ContainerState::parse(status),
        Some(crate::docker::client::ContainerState::Running)
    )
}

fn runner_status_is_safe_to_recreate(status: &str) -> bool {
    matches!(
        crate::docker::client::ContainerState::parse(status),
        Some(
            crate::docker::client::ContainerState::Created
                | crate::docker::client::ContainerState::Exited
                | crate::docker::client::ContainerState::Dead
        )
    )
}

fn runner_status_error(status: &str) -> anyhow::Error {
    if crate::docker::client::ContainerState::parse(status).is_some() {
        anyhow::anyhow!("refusing runner container in unsafe lifecycle status {status:?}")
    } else {
        anyhow::anyhow!("refusing runner container with unknown lifecycle status {status:?}")
    }
}

fn runner_container_config_matches(config: &RunnerContainerConfig, image: &str) -> bool {
    config.image == image
        && config.entrypoint.is_empty()
        && config
            .command
            .as_deref()
            .is_some_and(|command| command == [RUNNER_START_COMMAND])
}

fn validate_runner_identity(
    object_name: &str,
    identity: &WorkerIdentity,
    labels: &BTreeMap<String, String>,
    network_mode: &str,
) -> Result<()> {
    let expected_labels = [
        (OWNERSHIP_LABEL, identity.ownership().as_str()),
        (RUNNER_LABEL, identity.ownership().runner_name().to_string()),
        (
            SCALE_SET_LABEL,
            identity.ownership().scale_set_id().to_string(),
        ),
        (WORKER_ROLE_LABEL, ROLE_RUNNER.to_string()),
    ];
    for (key, expected) in expected_labels {
        match labels.get(key) {
            Some(found) if found == &expected => {}
            Some(found) => {
                anyhow::bail!("{object_name} has {key}={found:?}, expected {expected:?}")
            }
            None => anyhow::bail!("{object_name} is missing label {key}"),
        }
    }

    let expected_network = format!("container:{}", identity.dind_container());
    if network_mode != expected_network {
        anyhow::bail!(
            "{object_name} has network mode {network_mode:?}, expected {expected_network:?}"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartRunnerProjection {
    image: String,
    labels: BTreeMap<String, String>,
    network_mode: String,
    volumes_from: Option<Vec<String>>,
    entrypoint: Option<Vec<String>>,
    command: Option<Vec<String>>,
    status: String,
}

fn parse_restart_runner_projection(output: &str) -> Result<RestartRunnerProjection> {
    let mut fields = output.trim().split('\t');
    let image = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted image")?,
    )
    .context("parse restart runner image")?;
    let labels = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted labels")?,
    )
    .context("parse restart runner labels")?;
    let network_mode = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted network mode")?,
    )
    .context("parse restart runner network mode")?;
    let volumes_from = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted volume holder references")?,
    )
    .context("parse restart runner volume holder references")?;
    let entrypoint = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted entrypoint")?,
    )
    .context("parse restart runner entrypoint")?;
    let command = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted command")?,
    )
    .context("parse restart runner command")?;
    let status = serde_json::from_str(
        fields
            .next()
            .context("restart runner inspect omitted lifecycle status")?,
    )
    .context("parse restart runner lifecycle status")?;
    if fields.next().is_some() {
        anyhow::bail!("restart runner inspect returned extra fields");
    }
    Ok(RestartRunnerProjection {
        image,
        labels,
        network_mode,
        volumes_from,
        entrypoint,
        command,
        status,
    })
}

/// Attest an existing runner during restart reconciliation.
///
/// This is deliberately inspect-only: it never consumes JIT credentials and
/// never creates, starts, or removes a container. Every field is a narrow
/// projection; in particular, `.Config.Env` is excluded because it can hold
/// the runner's JIT configuration.
pub(crate) fn attest_restart_runner(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
) -> Result<ContainerState> {
    let name = identity.runner_container();
    let inspected = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{json .Config.Image}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .HostConfig.NetworkMode}}{{"\t"}}{{json .HostConfig.VolumesFrom}}{{"\t"}}{{json .Config.Entrypoint}}{{"\t"}}{{json .Config.Cmd}}{{"\t"}}{{json .State.Status}}"#.to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("attest restart runner {name}"))?;
    if inspected.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspected.stderr) {
            return Err(super::RestartObjectMissing.into());
        }
        anyhow::bail!(
            "attest restart runner {name} exited {}: {}",
            inspected.code,
            inspected.stderr.trim()
        );
    }

    let projection = parse_restart_runner_projection(&inspected.stdout)
        .with_context(|| format!("attest restart runner {name}"))?;
    validate_runner_identity(
        &format!("restart runner {name}"),
        identity,
        &projection.labels,
        &projection.network_mode,
    )?;
    verify_volume_holder_reference(
        "restart runner",
        &name,
        projection.volumes_from.as_deref(),
        identity,
    )?;

    let expected_image = expected_image.reference();
    if projection.image != expected_image {
        anyhow::bail!(
            "restart runner {name} has image {:?}, expected {:?}",
            projection.image,
            expected_image
        );
    }

    if projection.entrypoint.unwrap_or_default() != Vec::<String>::new() {
        anyhow::bail!("restart runner {name} has a non-empty entrypoint");
    }
    if projection.command != Some(vec![RUNNER_START_COMMAND.to_string()]) {
        anyhow::bail!(
            "restart runner {name} has command {:?}, expected {:?}",
            projection.command,
            [RUNNER_START_COMMAND]
        );
    }

    match projection.status.as_str() {
        "running" => Ok(ContainerState::Running),
        "exited" => Ok(ContainerState::Exited),
        "dead" => Ok(ContainerState::Dead),
        status => {
            anyhow::bail!("restart runner {name} has unsafe or unknown lifecycle status {status:?}")
        }
    }
}

fn create_and_start_runner(
    runner: &mut dyn WorkerRunner,
    spec: &RunnerSpec,
    name: &str,
    before_start: &mut dyn FnMut() -> Result<()>,
) -> Result<RunnerProvision> {
    let env_file = spec.write_env_file()?;
    let created = runner.run("docker", &spec.create_args_with_env_file(&env_file));
    // Scrubbing is part of the create boundary. Never start/adopt the
    // runner if the secret file could not be removed.
    let scrubbed = spec.scrub_jit_env_files();
    let created = match (created, scrubbed) {
        (Ok(output), Ok(())) => output,
        (Err(create_error), Ok(())) => {
            return Err(create_error).with_context(|| format!("create runner container {name}"));
        }
        (Ok(_), Err(scrub_error)) => {
            anyhow::bail!("scrub JIT env file after create of runner {name}: {scrub_error:#}");
        }
        (Err(create_error), Err(scrub_error)) => {
            anyhow::bail!(
                "create runner container {name} failed: {create_error:#}; JIT env cleanup failed: {scrub_error:#}"
            );
        }
    };
    if created.code != 0 {
        anyhow::bail!(
            "create runner container {name} exited {}: {}",
            created.code,
            created.stderr.trim()
        );
    }
    before_start().context("persist runner startup deadline")?;
    let started = runner
        .run(
            "docker",
            &["start".to_string(), "--".to_string(), name.to_string()],
        )
        .with_context(|| format!("start runner container {name}"))?;
    if started.code != 0 {
        anyhow::bail!(
            "start runner container {name} exited {}: {}",
            started.code,
            started.stderr.trim()
        );
    }
    Ok(RunnerProvision::Created)
}

/// Ensure the runner container exists and is started, adopting on retry.
///
/// Same contract as [`super::dind::ensure_dind`]: missing → write the
/// `0600` JIT env file, create from
/// [`RunnerSpec::create_args_with_env_file`], delete the env file, then
/// start; present → complete identity labels, exact DinD network mode, and
/// persisted image/command must match before adoption or removal. A stopped
/// owned container is always removed and recreated so it receives a fresh
/// one-shot JIT environment. A running mismatch fails closed.
/// Call only after the DinD daemon is ready: the runner joins the
/// daemon's network namespace, so creating it first would fail against a
/// missing namespace.
pub(crate) fn ensure_runner(
    runner: &mut dyn WorkerRunner,
    spec: &RunnerSpec,
    before_start: &mut dyn FnMut() -> Result<()>,
) -> Result<RunnerProvision> {
    // A prior process may have died after writing the env file. Scrub both
    // the host-only location and the legacy mounted path before inspecting
    // or adopting any runner container.
    spec.scrub_jit_env_files()?;
    let name = spec.identity().runner_container();
    let inspect = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect runner container {name}"))?;
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return create_and_start_runner(runner, spec, &name, before_start);
        }
        anyhow::bail!(
            "inspect runner container {name} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }
    if inspect.stdout.trim().is_empty() {
        anyhow::bail!("inspect runner container {name} returned empty id");
    }

    super::dind::verify_container_ownership(runner, &name, &spec.identity().ownership().as_str())?;
    let config = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{json .Config.Image}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .HostConfig.NetworkMode}}{{"\t"}}{{json .HostConfig.VolumesFrom}}{{"\t"}}{{json .Config.Entrypoint}}{{"\t"}}{{json .Config.Cmd}}{{"\t"}}{{json .State.Status}}"#.to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect runner command {name}"))?;
    if config.code != 0 {
        anyhow::bail!(
            "inspect runner command {name} exited {}: {}",
            config.code,
            config.stderr.trim()
        );
    }
    let config = parse_runner_container_config(&config.stdout)
        .with_context(|| format!("inspect runner command {name}"))?;
    validate_runner_identity(
        &format!("runner container {name}"),
        spec.identity(),
        &config.labels,
        &config.network_mode,
    )?;
    verify_volume_holder_reference(
        "runner container",
        &name,
        config.volumes_from.as_deref(),
        spec.identity(),
    )?;
    let image = spec.image.reference();
    if runner_status_is_running(&config.status) {
        if !runner_container_config_matches(&config, &image) {
            anyhow::bail!(
                "refusing to replace running runner container {name} with mismatched image or command"
            );
        }
        // The container is already running. Do not call `docker start`: a
        // race between inspect and start could restart a one-shot JIT
        // container after it exits and reuse its persisted secret.
        before_start().context("persist runner startup deadline")?;
        return Ok(RunnerProvision::Adopted);
    }
    if runner_status_is_safe_to_recreate(&config.status) {
        let removed = runner
            .run(
                "docker",
                &crate::docker::client::container_remove_args(&name, false, false),
            )
            .with_context(|| format!("remove stopped runner container {name}"))?;
        if removed.code != 0 {
            anyhow::bail!(
                "remove stopped runner container {name} exited {}: {}",
                removed.code,
                removed.stderr.trim()
            );
        }
        return create_and_start_runner(runner, spec, &name, before_start);
    }
    Err(runner_status_error(&config.status))
}

/// Runner connectivity: what the supervisor observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerConnection {
    /// Container running and the JIT-connected marker observed.
    Connected,
    /// Container running but no connected marker yet.
    Starting,
    /// Container absent or stopped.
    Down,
}

/// Parse `docker logs` for the runner's connected marker.
///
/// The official runner prints `Connected to GitHub` once the JIT
/// registration completes; that line is the connected signal. (The exact
/// marker is re-verified by the live canary; absence reads as Starting,
/// never as Connected.)
#[must_use]
pub fn parse_connected_marker(logs: &str) -> bool {
    logs.lines()
        .any(|line| line.contains("Connected to GitHub"))
}

/// Classify runner connectivity: running state + connected marker.
///
/// * Not running (or missing) → [`RunnerConnection::Down`].
/// * Running without the marker → [`RunnerConnection::Starting`].
/// * Running with the marker → [`RunnerConnection::Connected`].
pub(crate) fn runner_connection(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<RunnerConnection> {
    let name = identity.runner_container();
    // Raw calls (not the `Docker` facade): connectivity needs inspect +
    // logs back-to-back on one runner borrow, and the facade owns its
    // borrow for its whole lifetime.
    let status = runner
        .run("docker", &crate::docker::client::status_args(&name))
        .with_context(|| format!("inspect runner container {name}"))?;
    if status.code != 0 {
        if crate::docker::client::daemon_reports_missing(&status.stderr) {
            return Ok(RunnerConnection::Down);
        }
        anyhow::bail!(
            "inspect runner container {name} exited {}: {}",
            status.code,
            status.stderr.trim()
        );
    }
    if !matches!(
        crate::docker::client::ContainerState::parse(status.stdout.trim()),
        Some(crate::docker::client::ContainerState::Running)
    ) {
        return Ok(RunnerConnection::Down);
    }
    let logs = runner
        .run(
            "docker",
            &[
                "logs".to_string(),
                "--tail".to_string(),
                "2000".to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("read runner container logs {name}"))?;
    if logs.code != 0 {
        anyhow::bail!(
            "read runner container logs {name} exited {}: {}",
            logs.code,
            logs.stderr.trim()
        );
    }
    if parse_connected_marker(&logs.stdout) || logs.stdout.contains("Running job:") {
        Ok(RunnerConnection::Connected)
    } else {
        Ok(RunnerConnection::Starting)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::super::ownership::OwnershipId;
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    const RUNNER_REF: &str = "ghcr.io/actions/actions-runner@sha256:e5496277be5d09bc968b3d64911b74e219ac4a3f2edce956a3ecf9271bea1ef4";
    const WRONG_RUNNER_REF: &str = "ghcr.io/actions/actions-runner@sha256:0000000000000000000000000000000000000000000000000000000000000000";

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
            }
        }

        fn ok(stdout: &str) -> WorkerOutput {
            WorkerOutput {
                code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            }
        }

        fn fail(code: i32, stderr: &str) -> WorkerOutput {
            WorkerOutput {
                code,
                stdout: String::new(),
                stderr: stderr.to_string(),
            }
        }
    }

    impl WorkerRunner for ScriptRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<WorkerOutput> {
            assert!(
                matches!(program, "docker" | "gh"),
                "unexpected program {program}"
            );
            self.seen.push(args.to_vec());
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at {program} {}", args.join(" ")))
        }
    }

    fn identity() -> WorkerIdentity {
        WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ))
    }

    fn temp_state(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("velnor-runner-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn container_config_parts(
        image: &str,
        entrypoint: Option<&[&str]>,
        command: Option<&[&str]>,
        status: &str,
    ) -> String {
        let mut labels = identity().labels();
        labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());
        let network_mode = format!("container:{}", identity().dind_container());
        container_config_parts_with(image, &labels, &network_mode, entrypoint, command, status)
    }

    fn container_config_parts_with(
        image: &str,
        labels: &BTreeMap<String, String>,
        network_mode: &str,
        entrypoint: Option<&[&str]>,
        command: Option<&[&str]>,
        status: &str,
    ) -> String {
        let volumes_from = Some(vec![identity().volume_holder_container()]);
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            serde_json::to_string(image).unwrap(),
            serde_json::to_string(labels).unwrap(),
            serde_json::to_string(network_mode).unwrap(),
            serde_json::to_string(&volumes_from).unwrap(),
            serde_json::to_string(&entrypoint).unwrap(),
            serde_json::to_string(&command).unwrap(),
            serde_json::to_string(status).unwrap(),
        )
    }

    fn container_config(image: &str, command: &str, status: &str) -> String {
        container_config_parts(image, None, Some(&[command]), status)
    }

    fn restart_runner_projection(labels: &BTreeMap<String, String>, status: &str) -> String {
        restart_runner_projection_with(
            labels,
            Some(vec![identity().volume_holder_container()]),
            status,
        )
    }

    fn restart_runner_projection_with(
        labels: &BTreeMap<String, String>,
        volumes_from: Option<Vec<String>>,
        status: &str,
    ) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            serde_json::to_string(RUNNER_REF).unwrap(),
            serde_json::to_string(labels).unwrap(),
            serde_json::to_string(&format!("container:{}", identity().dind_container())).unwrap(),
            serde_json::to_string(&volumes_from).unwrap(),
            serde_json::to_string(&Some(Vec::<String>::new())).unwrap(),
            serde_json::to_string(&Some(vec![RUNNER_START_COMMAND.to_string()])).unwrap(),
            serde_json::to_string(status).unwrap(),
        )
    }

    #[test]
    fn restart_runner_attestation_requires_scale_set_label() {
        let expected_image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut labels = identity().labels();
        labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());

        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(
            &restart_runner_projection(&labels, "running"),
        )]);
        assert_eq!(
            attest_restart_runner(&mut runner, &identity(), &expected_image).unwrap(),
            ContainerState::Running
        );

        labels.remove(SCALE_SET_LABEL);
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(
            &restart_runner_projection(&labels, "running"),
        )]);
        let error = attest_restart_runner(&mut runner, &identity(), &expected_image).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing label velnor.scaleset.set"),
            "{error:#}"
        );
    }

    #[test]
    fn restart_runner_attestation_requires_exact_volume_holder() {
        let expected_image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut labels = identity().labels();
        labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());
        let holder = identity().volume_holder_container();
        for volumes_from in [
            None,
            Some(Vec::new()),
            Some(vec!["legacy-named-holder".to_string()]),
            Some(vec![holder.clone(), "unexpected-second-holder".to_string()]),
        ] {
            let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(
                &restart_runner_projection_with(&labels, volumes_from, "running"),
            )]);
            let error =
                attest_restart_runner(&mut runner, &identity(), &expected_image).unwrap_err();
            assert!(
                error.to_string().contains("HostConfig.VolumesFrom"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn production_pins_parse() {
        assert_eq!(
            PinnedImage::parse(&format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}"))
                .unwrap()
                .reference(),
            format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}")
        );
        assert_eq!(
            PinnedImage::parse(&format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}"))
                .unwrap()
                .reference(),
            format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}")
        );
        // Per-arch digests are well-formed pins too.
        for digest in [RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64] {
            assert!(PinnedImage::parse(&format!("{RUNNER_REPOSITORY}@{digest}")).is_ok());
        }
        for digest in [DIND_DIGEST_AMD64, DIND_DIGEST_ARM64] {
            assert!(PinnedImage::parse(&format!("{DIND_REPOSITORY}@{digest}")).is_ok());
        }
    }

    #[test]
    fn tags_and_latest_are_rejected() {
        for raw in [
            "ghcr.io/actions/actions-runner:latest",
            "ghcr.io/actions/actions-runner:2.337.0",
            "docker:28-dind",
            "docker",
            "alpine:3.22",
            "registry.local:5000/team/app:latest",
        ] {
            let error = PinnedImage::parse(raw).unwrap_err();
            assert!(
                matches!(error, InvalidPinnedImage::Unpinned(_)),
                "{raw}: {error}"
            );
            assert!(
                error.to_string().contains("never tags, never latest"),
                "{error}"
            );
        }
    }

    #[test]
    fn malformed_references_are_rejected() {
        for raw in [
            "",
            "--privileged",
            "alpine@sha256:short",
            "alpine@sha512:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "UPPERCASE@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "with space@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(PinnedImage::parse(raw).is_err(), "{raw:?} must be rejected");
        }
    }

    #[test]
    fn profile_serves_provisioned_arches_only() {
        assert!(HomogeneousProfile::for_arch("x86_64").is_some());
        assert!(HomogeneousProfile::for_arch("aarch64").is_some());
        assert!(HomogeneousProfile::for_arch("riscv64").is_none());
        assert!(HomogeneousProfile::for_arch("").is_none());
        let profile = HomogeneousProfile::for_arch("x86_64").unwrap();
        assert_eq!(profile.versions(), (RUNNER_VERSION, DIND_VERSION));
        assert_eq!(profile.runner().digest(), RUNNER_INDEX_DIGEST);
        assert_eq!(profile.dind().digest(), DIND_INDEX_DIGEST);
    }

    #[test]
    fn runner_argv_joins_dind_netns_without_publishing() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state"),
            "jit-blob",
        );
        let env_file_path = spec.jit_env_file_path().unwrap();
        let args = spec.create_args_with_env_file(&env_file_path);
        assert!(args.contains(&"--network".to_string()));
        assert!(args
            .contains(&"container:velnor-scaleset-dind-s7-velnor-set-0007-2ad92676".to_string()));
        for forbidden in [
            "-p",
            "--publish",
            "--expose",
            "-P",
            "--publish-all",
            "--privileged",
        ] {
            assert!(
                !args.iter().any(|arg| arg == forbidden),
                "runner argv must not carry {forbidden}: {args:?}"
            );
        }
        // No host socket, no App keys, and no JIT blob bytes anywhere
        // in argv: the blob travels via --env-file only, so the host
        // process list never carries it.
        assert!(
            !args.iter().any(|arg| arg.contains("/var/run/docker.sock")),
            "{args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("GITHUB_APP") || arg.contains("ghs_")),
            "{args:?}"
        );
        assert!(!args.iter().any(|arg| arg.contains("jit-blob")), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.contains(JIT_CONFIG_ENV)),
            "{args:?}"
        );
        let env_file_arg = args
            .windows(2)
            .find(|pair| pair[0] == "--env-file")
            .map(|pair| pair[1].clone());
        assert_eq!(
            env_file_arg.as_deref(),
            Some(env_file_path.to_string_lossy().as_ref()),
            "{args:?}"
        );
        assert!(
            !env_file_path.starts_with(spec.state_dir()),
            "JIT file must not be runner-visible"
        );
        assert!(args
            .iter()
            .any(|arg| arg == &format!("{RUNNER_NAME_ENV}=velnor-set-0007")));
        assert!(args
            .iter()
            .any(|arg| arg == &format!("DOCKER_HOST=unix://{DIND_SOCKET}")));
    }

    #[test]
    fn runner_spec_debug_redacts_the_jit_blob() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state"),
            "live-jit-blob-bytes",
        );
        let rendered = format!("{spec:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("live-jit-blob-bytes"), "{rendered}");
    }

    #[test]
    fn jit_env_file_carries_the_image_input() {
        let state = temp_state("env-file");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "live-jit-blob-bytes",
        );
        let env_file = spec.write_env_file().unwrap();
        assert_eq!(env_file, spec.jit_env_file_path().unwrap());
        assert!(
            !env_file.starts_with(&state),
            "JIT file must be outside the runner bind mount"
        );
        assert_eq!(
            std::fs::read_to_string(&env_file).unwrap(),
            format!("{JIT_CONFIG_ENV}=live-jit-blob-bytes\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "JIT env file has mode {mode:o}");
        }
        RunnerSpec::scrub_jit_env_files_for_state(&state).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn jit_env_file_rejects_multiline_blobs() {
        let state = temp_state("env-file-multiline");
        for blob in ["one\ntwo", "one\rtwo"] {
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                &state,
                blob,
            );
            assert!(spec.write_env_file().is_err());
        }
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_argv_mounts_identical_absolute_paths() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state"),
            "jit-blob",
        );
        let args = spec
            .create_args_with_env_file(&spec.jit_env_file_path().unwrap())
            .join("\n");
        // The holder supplies the shared absolute workspace, tool-cache, and
        // DinD data mounts to both containers; runner argv must reference the
        // holder rather than a named volume source.
        for guest in [
            STATE_MOUNT,
            BUILDKIT_CACHE_DIR,
            "/home/runner/.cargo/registry",
            "/home/runner/.cargo/git",
        ] {
            assert!(args.contains(guest), "missing guest path {guest}:\n{args}");
        }
        assert!(args.contains(&format!(
            "--volumes-from\n{}",
            spec.identity().volume_holder_container()
        )));
        assert!(!args.contains(&format!(":{RUNNER_WORK_DIR}")));
        assert!(!args.contains(&format!(":{TOOL_CACHE_DIR}")));
    }

    #[test]
    fn runner_argv_explicitly_invokes_the_official_run_script() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state"),
            "jit-blob",
        );
        let args = spec
            .create_args_with_env_file(&spec.jit_env_file_path().unwrap())
            .join("\n");
        for forbidden in [
            "curl",
            "wget",
            "latest",
            "rustup",
            "cargo-nextest",
            "mise",
            "apt-get",
            "dpkg",
            "sudo",
        ] {
            assert!(
                !args.contains(forbidden),
                "worker create argv contains floating install path {forbidden}: {args}"
            );
        }
        let create_args = spec.create_args_with_env_file(&spec.jit_env_file_path().unwrap());
        assert_eq!(
            create_args.get(create_args.len() - 2),
            Some(&RUNNER_REF.to_string())
        );
        assert_eq!(
            create_args.last().map(String::as_str),
            Some(RUNNER_START_COMMAND)
        );
    }

    #[test]
    fn runner_container_config_parser_is_non_secret_and_exact() {
        let config = parse_runner_container_config(&container_config(
            RUNNER_REF,
            RUNNER_START_COMMAND,
            "exited",
        ))
        .unwrap();
        assert_eq!(config.image, RUNNER_REF);
        assert_eq!(
            config.volumes_from,
            Some(vec![identity().volume_holder_container()])
        );
        assert_eq!(config.entrypoint, Vec::<String>::new());
        assert_eq!(config.command, Some(vec![RUNNER_START_COMMAND.to_string()]));
        assert_eq!(config.status, "exited");
        assert!(runner_container_config_matches(&config, RUNNER_REF));
        assert!(!runner_container_config_matches(&config, WRONG_RUNNER_REF));
        assert!(parse_runner_container_config(
            "\"image\"\t{}\t\"container:dind\"\t[]\tnull\tnull\t\"created\"\n"
        )
        .unwrap()
        .command
        .is_none());
    }

    #[test]
    fn runner_container_config_parser_rejects_malformed_lifecycle_projection() {
        for projection in [
            "\"image\"\t{}\t\"bridge\"\tnull\t[]\n",
            "\"image\"\t{}\t\"bridge\"\tnull\t[]\ttrue\t\"running\"\n",
            "\"image\"\t{}\t\"bridge\"\tnull\t[]\t[]\t\"\"\n",
            "\"image\"\t{}\t\"bridge\"\tnull\t[]\t[]\t\"running\"\textra\n",
            "not-json\t{}\t\"bridge\"\tnull\t[]\t[]\t\"running\"\n",
        ] {
            assert!(
                parse_runner_container_config(projection).is_err(),
                "projection must fail closed: {projection:?}"
            );
        }
    }

    #[test]
    fn runner_provision_rejects_identity_or_network_mismatch_before_remove() {
        let mut missing_scale_set = identity().labels();
        missing_scale_set.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());
        missing_scale_set.remove(SCALE_SET_LABEL);
        let mut complete = identity().labels();
        complete.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());
        let cases = vec![
            (
                "missing-scale-set",
                missing_scale_set,
                format!("container:{}", identity().dind_container()),
                format!("missing label {SCALE_SET_LABEL}"),
            ),
            (
                "wrong-network",
                complete,
                "bridge".to_string(),
                "network mode".to_string(),
            ),
        ];

        for (case, labels, network_mode, expected_error) in cases {
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                Path::new("/tmp/velnor-test-runner-state-identity-boundary"),
                "redacted-jit",
            );
            let projection = container_config_parts_with(
                RUNNER_REF,
                &labels,
                &network_mode,
                None,
                Some(&[RUNNER_START_COMMAND][..]),
                "exited",
            );
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("cafe\n"),
                ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
                ScriptRunner::ok(&projection),
            ]);
            let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
            assert!(
                error.to_string().contains(&expected_error),
                "{case}: {error:#}"
            );
            assert_eq!(runner.seen.len(), 3, "{case}");
            assert!(
                !runner
                    .seen
                    .iter()
                    .any(|args| matches!(args.first().map(String::as_str), Some("rm" | "start"))),
                "{case}: mismatch must block mutation"
            );
        }
    }

    #[test]
    fn runner_provision_recreates_stopped_mismatched_container() {
        let state = temp_state("provision-recreate-mismatch");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "redacted-jit",
        );
        let name = spec.identity().runner_container();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
            ScriptRunner::ok(&container_config(RUNNER_REF, "/bin/bash", "exited")),
            ScriptRunner::ok(&format!("{name}\n")),
            ScriptRunner::ok("new-container\n"),
            ScriptRunner::ok(&format!("{name}\n")),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Created
        );
        assert_eq!(
            runner.seen[3],
            vec!["rm".to_string(), "--".to_string(), name.clone()]
        );
        let command_inspect = &runner.seen[2][2];
        assert!(command_inspect.contains(".Config.Labels"));
        assert!(command_inspect.contains(".HostConfig.NetworkMode"));
        assert!(command_inspect.contains(".Config.Entrypoint"));
        assert!(command_inspect.contains(".Config.Cmd"));
        assert!(command_inspect.contains(".State.Status"));
        assert!(!command_inspect.contains(".Config.Env"));
        assert_eq!(
            runner.seen[4].last().map(String::as_str),
            Some(RUNNER_START_COMMAND)
        );
        assert_eq!(
            runner.seen[5],
            vec!["start".to_string(), "--".to_string(), name.clone()]
        );
        assert!(!runner.seen[3].iter().any(|arg| arg == "--force"));
        assert!(!spec.jit_env_file_path().unwrap().exists());
        assert!(!state.join(JIT_ENV_FILE).exists());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_rejects_running_mismatched_container() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state-running-mismatch"),
            "redacted-jit",
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
            ScriptRunner::ok(&container_config(
                WRONG_RUNNER_REF,
                RUNNER_START_COMMAND,
                "running",
            )),
        ]);
        let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
        assert!(
            error.to_string().contains("running runner container"),
            "{error}"
        );
        assert_eq!(runner.seen.len(), 3);
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("rm")));
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("start")));
    }

    #[test]
    fn runner_provision_adopts_matching_persisted_command() {
        let state = temp_state("provision-adopt-matching-command");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "redacted-jit",
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
            ScriptRunner::ok(&container_config(
                RUNNER_REF,
                RUNNER_START_COMMAND,
                "running",
            )),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Adopted
        );
        assert_eq!(runner.seen.len(), 3);
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("start")));
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_recreates_stopped_wrong_image() {
        let state = temp_state("provision-recreate-wrong-image");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "redacted-jit",
        );
        let name = spec.identity().runner_container();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
            ScriptRunner::ok(&container_config(
                WRONG_RUNNER_REF,
                RUNNER_START_COMMAND,
                "exited",
            )),
            ScriptRunner::ok(""),
            ScriptRunner::ok("new-container\n"),
            ScriptRunner::ok(""),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Created
        );
        assert_eq!(
            runner.seen[3],
            vec!["rm".to_string(), "--".to_string(), name.clone()]
        );
        assert_eq!(
            runner.seen[5],
            vec!["start".to_string(), "--".to_string(), name]
        );
        assert!(!runner.seen[2][2].contains(".Config.Env"));
        assert!(runner.seen[4].contains(&RUNNER_REF.to_string()));
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_recreates_created_exited_and_dead_containers() {
        for status in ["created", "exited", "dead"] {
            let state = temp_state(&format!("provision-recreate-{status}"));
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                &state,
                "redacted-jit",
            );
            let name = spec.identity().runner_container();
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("cafe\n"),
                ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
                ScriptRunner::ok(&container_config(RUNNER_REF, RUNNER_START_COMMAND, status)),
                ScriptRunner::ok(""),
                ScriptRunner::ok("new-container\n"),
                ScriptRunner::ok(""),
            ]);
            assert_eq!(
                ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
                RunnerProvision::Created,
                "status {status}"
            );
            assert_eq!(
                runner.seen[3],
                vec!["rm".to_string(), "--".to_string(), name.clone()]
            );
            assert_eq!(
                runner.seen[5],
                vec!["start".to_string(), "--".to_string(), name]
            );
            assert!(!runner.seen[3].iter().any(|arg| arg == "--force"));
            std::fs::remove_dir_all(&state).unwrap();
        }
    }

    #[test]
    fn runner_provision_rejects_transitional_and_unknown_statuses() {
        for status in ["paused", "restarting", "removing", "mystery"] {
            let state = temp_state(&format!("provision-reject-{status}"));
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                &state,
                "redacted-jit",
            );
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("cafe\n"),
                ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
                ScriptRunner::ok(&container_config(RUNNER_REF, RUNNER_START_COMMAND, status)),
            ]);
            let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
            assert!(error.to_string().contains(status), "{error:#}");
            assert_eq!(runner.seen.len(), 3);
            assert!(!runner
                .seen
                .iter()
                .any(|args| matches!(args.first().map(String::as_str), Some("rm" | "start"))));
            std::fs::remove_dir_all(&state).unwrap();
        }
    }

    #[test]
    fn runner_provision_rejects_running_wrong_command_or_entrypoint() {
        for (entrypoint, command) in [
            (None, Some(&["/bin/bash"][..])),
            (Some(&["/bin/sh"][..]), Some(&[RUNNER_START_COMMAND][..])),
        ] {
            let state = temp_state("provision-running-contract-mismatch");
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                &state,
                "redacted-jit",
            );
            let projection = container_config_parts(RUNNER_REF, entrypoint, command, "running");
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("cafe\n"),
                ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
                ScriptRunner::ok(&projection),
            ]);
            let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
            assert!(error.to_string().contains("mismatched image or command"));
            assert_eq!(runner.seen.len(), 3);
            assert!(!runner
                .seen
                .iter()
                .any(|args| matches!(args.first().map(String::as_str), Some("rm" | "start"))));
            std::fs::remove_dir_all(&state).unwrap();
        }
    }

    #[test]
    fn hook_attests_matching_content() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok(&format!("[\"{RUNNER_REF}\"]\n")),
            ScriptRunner::ok(
                r#"{"org.opencontainers.image.source":"https://github.com/actions/runner","org.opencontainers.image.version":"24.04"}"#,
            ),
            ScriptRunner::ok("sha256:feedface\n"),
        ]);
        let hook = DockerToolContentHook;
        let attestation = hook
            .verify(&mut runner, &image, &ToolContentExpectation::runner())
            .unwrap();
        assert_eq!(attestation.reference, RUNNER_REF);
        assert_eq!(attestation.image_id, "sha256:feedface");
        assert_eq!(attestation.content_version, RUNNER_VERSION);
        assert_eq!(attestation.source.as_deref(), Some(RUNNER_SOURCE));
        assert_eq!(runner.seen.len(), 4);
        assert_eq!(runner.seen[0][0], "pull");
    }

    #[test]
    fn hook_rejects_digest_mismatch() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok("[\"ghcr.io/actions/actions-runner@sha256:0000000000000000000000000000000000000000000000000000000000000000\"]\n"),
        ]);
        let error = DockerToolContentHook
            .verify(&mut runner, &image, &ToolContentExpectation::runner())
            .unwrap_err();
        assert!(error.to_string().contains("digest disagrees"), "{error}");
    }

    #[test]
    fn hook_rejects_missing_configured_digest() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok("[]\n"),
        ]);
        let error = DockerToolContentHook
            .verify(&mut runner, &image, &ToolContentExpectation::runner())
            .unwrap_err();
        assert!(error.to_string().contains("digest disagrees"), "{error}");
    }

    fn verified_runner_provenance(image: &PinnedImage) -> String {
        let digest = image.digest().strip_prefix("sha256:").unwrap();
        serde_json::json!([{
            "verificationResult": {
                "signature": { "certificate": { "subjectAlternativeName": {
                    "value": "https://github.com/actions/runner/.github/workflows/release.yml@refs/heads/main"
                }}},
                "verifiedTimestamps": [{ "type": "Tlog" }],
                "statement": {
                    "subject": [{ "name": image.repository(), "digest": { "sha256": digest } }],
                    "predicateType": RUNNER_ATTESTATION_PREDICATE
                }
            }
        }])
        .to_string()
    }

    fn admission_script(image: &PinnedImage, final_result: WorkerOutput) -> ScriptRunner {
        ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok(&format!("[\"{}\"]\n", image.reference())),
            ScriptRunner::ok(
                r#"{"org.opencontainers.image.source":"https://github.com/actions/runner"}"#,
            ),
            ScriptRunner::ok("sha256:feedface\n"),
            ScriptRunner::ok("linux/amd64\n"),
            final_result,
        ])
    }

    #[test]
    fn admission_accepts_verified_official_runner_provenance() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = admission_script(
            &image,
            ScriptRunner::ok(&verified_runner_provenance(&image)),
        );
        let expected = ToolContentExpectation::runner();
        let attestation =
            admit_tool_content(&DockerToolContentHook, &mut runner, &image, &expected).unwrap();
        assert_eq!(attestation.reference, image.reference());
        let gh_args = runner
            .seen
            .iter()
            .find(|args| args.first().map(String::as_str) == Some("attestation"))
            .unwrap();
        assert!(gh_args.windows(2).any(|pair| {
            pair == [
                "--repo".to_string(),
                RUNNER_ATTESTATION_REPOSITORY.to_string(),
            ]
        }));
        assert!(gh_args.contains(&"--deny-self-hosted-runners".to_string()));
    }

    #[test]
    fn admission_rejects_runner_provenance_verifier_failure() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = admission_script(
            &image,
            ScriptRunner::fail(1, "no valid attestation matched the policy"),
        );
        let error = admit_tool_content(
            &DockerToolContentHook,
            &mut runner,
            &image,
            &ToolContentExpectation::runner(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("provenance verification rejected"),
            "{error}"
        );
    }

    #[test]
    fn admission_rejects_unknown_runner_provenance_result() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = admission_script(&image, ScriptRunner::ok(""));
        let error = admit_tool_content(
            &DockerToolContentHook,
            &mut runner,
            &image,
            &ToolContentExpectation::runner(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("provenance verification is unknown"),
            "{error}"
        );
    }

    #[test]
    fn hook_rejects_platform_mismatch() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let expected = ImagePlatform::parse("linux/amd64").unwrap();
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("linux/arm64\n")]);
        let error = DockerToolContentHook
            .verify_platform(&mut runner, &image, &expected)
            .unwrap_err();
        assert!(error.to_string().contains("platform disagrees"), "{error}");
    }

    struct MissingPlatformProofHook;

    impl ToolContentHook for MissingPlatformProofHook {
        fn verify(
            &self,
            _runner: &mut dyn WorkerRunner,
            image: &PinnedImage,
            expected: &ToolContentExpectation,
        ) -> Result<ToolContentAttestation> {
            Ok(ToolContentAttestation {
                reference: image.reference(),
                image_id: "sha256:verified".to_string(),
                content_version: expected.content_version.clone(),
                source: expected.source.clone(),
            })
        }
    }

    #[test]
    fn admission_rejects_missing_platform_proof_before_docker_state() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let expected =
            ToolContentExpectation::runner_for(&ImagePlatform::parse("linux/amd64").unwrap());
        let mut runner = ScriptRunner::scripted(vec![]);
        let error = admit_tool_content(&MissingPlatformProofHook, &mut runner, &image, &expected)
            .unwrap_err();
        assert!(
            error.to_string().contains("platform proof hook missing"),
            "{error}"
        );
        assert!(
            runner.seen.is_empty(),
            "proof failure must precede Docker calls"
        );
    }

    struct MissingSignatureProofHook;

    impl ToolContentHook for MissingSignatureProofHook {
        fn verify(
            &self,
            _runner: &mut dyn WorkerRunner,
            image: &PinnedImage,
            expected: &ToolContentExpectation,
        ) -> Result<ToolContentAttestation> {
            Ok(ToolContentAttestation {
                reference: image.reference(),
                image_id: "sha256:verified".to_string(),
                content_version: expected.content_version.clone(),
                source: expected.source.clone(),
            })
        }

        fn verify_platform(
            &self,
            _runner: &mut dyn WorkerRunner,
            _image: &PinnedImage,
            _expected: &ImagePlatform,
        ) -> Result<()> {
            Ok(())
        }

        fn verify_attestation(
            &self,
            _runner: &mut dyn WorkerRunner,
            _image: &PinnedImage,
            _expected: &ToolContentExpectation,
            _attestation: &ToolContentAttestation,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn admission_rejects_missing_signature_proof() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let expected =
            ToolContentExpectation::runner_for(&ImagePlatform::parse("linux/amd64").unwrap());
        let mut runner = ScriptRunner::scripted(vec![]);
        let error = admit_tool_content(&MissingSignatureProofHook, &mut runner, &image, &expected)
            .unwrap_err();
        assert!(
            error.to_string().contains("signature proof hook missing"),
            "{error}"
        );
    }

    #[test]
    fn hook_rejects_provenance_mismatch() {
        let image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok(&format!("[\"{RUNNER_REF}\"]\n")),
            ScriptRunner::ok(r#"{"org.opencontainers.image.source":"https://example.com/evil"}"#),
        ]);
        let error = DockerToolContentHook
            .verify(&mut runner, &image, &ToolContentExpectation::runner())
            .unwrap_err();
        assert!(
            error.to_string().contains("provenance disagrees"),
            "{error}"
        );
    }

    #[test]
    fn hook_accepts_dind_without_labels() {
        let dind = format!("{DIND_REPOSITORY}@{DIND_INDEX_DIGEST}");
        let image = PinnedImage::parse(&dind).unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("Status: Image is up to date\n"),
            ScriptRunner::ok(&format!("[\"docker.io/library/{dind}\"]\n")),
            ScriptRunner::ok("null\n"),
            ScriptRunner::ok("sha256:beef\n"),
        ]);
        let attestation = DockerToolContentHook
            .verify(&mut runner, &image, &ToolContentExpectation::dind())
            .unwrap();
        assert_eq!(attestation.content_version, DIND_VERSION);
        assert_eq!(attestation.source, None);
    }

    #[test]
    fn runner_spec_debug_redacts_jit_blob() {
        let profile = HomogeneousProfile::for_arch("x86_64").unwrap();
        let spec = RunnerSpec::new(
            WorkerIdentity::new(OwnershipId::bind(7, "velnor-7-4244")),
            profile.runner().clone(),
            std::path::Path::new("/tmp/velnor-jit-redact"),
            "live-jit-config-blob",
        );
        let rendered = format!("{spec:?}");
        assert!(
            !rendered.contains("live-jit-config-blob"),
            "RunnerSpec Debug leaked JIT: {rendered}"
        );
        assert!(rendered.contains("velnor-7-4244"));
    }

    #[test]
    fn connection_classifies_down_starting_connected() {
        // Missing container reads as Down.
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
        )]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Down
        );
        // Stopped container reads as Down.
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("exited\n")]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Down
        );
        // Transitional states are never treated as healthy, even when logs
        // might contain a stale connected marker.
        for status in ["paused", "restarting", "removing"] {
            let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(&format!("{status}\n"))]);
            assert_eq!(
                runner_connection(&mut runner, &identity()).unwrap(),
                RunnerConnection::Down,
                "status {status}"
            );
        }
        // Running without the marker reads as Starting.
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("running\n"),
            ScriptRunner::ok("Listening for Jobs\n"),
        ]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Starting
        );
        // Running with the marker reads as Connected.
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("running\n"),
            ScriptRunner::ok("Connected to GitHub\nListening for Jobs\n"),
        ]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Connected
        );
        // A daemon error that is NOT a missing answer propagates.
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "Error response from daemon: context deadline exceeded",
        )]);
        assert!(runner_connection(&mut runner, &identity()).is_err());
    }

    #[test]
    fn runner_provision_creates_then_adopts() {
        let state = temp_state("provision");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "jit-blob",
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(
                1,
                "Error: No such container: velnor-scaleset-runner-s7-velnor-set-0007-2ad92676",
            ),
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007-2ad92676\n"),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Created
        );
        // The create argv carries --env-file, never the blob (never App
        // keys either), and the env file is deleted right after create.
        let create = &runner.seen[1];
        assert!(
            !create.iter().any(|arg| arg.contains("jit-blob")),
            "{create:?}"
        );
        let env_file = spec.jit_env_file_path().unwrap();
        assert!(
            create
                .windows(2)
                .any(|pair| pair[0] == "--env-file" && pair[1] == env_file.display().to_string()),
            "{create:?}"
        );
        assert!(!env_file.starts_with(&state));
        assert!(!env_file.exists());
        assert!(!state.join("jit.env").exists());

        // Simulate a crash leaving both current and legacy files behind.
        std::fs::create_dir_all(env_file.parent().unwrap()).unwrap();
        std::fs::write(&env_file, "stale-secret").unwrap();
        std::fs::write(state.join("jit.env"), "legacy-secret").unwrap();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
            ScriptRunner::ok(&container_config(
                RUNNER_REF,
                RUNNER_START_COMMAND,
                "exited",
            )),
            ScriptRunner::ok(""),
            ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007-2ad92676\n"),
            ScriptRunner::ok(""),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Created
        );
        // A stopped container is never adopted: remove/create writes a fresh
        // JIT env file, then scrubs it before start.
        assert_eq!(
            runner.seen[3],
            vec![
                "rm".to_string(),
                "--".to_string(),
                spec.identity().runner_container()
            ]
        );
        assert!(runner.seen[4].contains(&"--env-file".to_string()));
        assert!(!env_file.exists());
        assert!(!state.join("jit.env").exists());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_rejects_non_missing_adoption_lookup_errors() {
        for (case, stderr) in [
            (
                "transport",
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
            ),
            (
                "permission",
                "permission denied while trying to connect to the Docker daemon",
            ),
            (
                "other",
                "Error response from daemon: context deadline exceeded",
            ),
        ] {
            let state = temp_state(&format!("provision-lookup-error-{case}"));
            let spec = RunnerSpec::new(
                identity(),
                PinnedImage::parse(RUNNER_REF).unwrap(),
                &state,
                "redacted-jit",
            );
            let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(1, stderr)]);
            let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
            assert!(error.to_string().contains(stderr), "{error:#}");
            assert_eq!(runner.seen.len(), 1, "lookup error must fail closed");
            std::fs::remove_dir_all(&state).unwrap();
        }
    }

    #[test]
    fn runner_provision_rejects_empty_successful_adoption_lookup() {
        let state = temp_state("provision-empty-successful-lookup");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "redacted-jit",
        );
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("")]);
        let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
        assert!(error.to_string().contains("returned empty id"), "{error:#}");
        assert_eq!(runner.seen.len(), 1, "empty success must fail closed");
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_fails_closed_when_jit_scrub_fails() {
        let state = temp_state("provision-scrub-fail");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "jit-blob",
        );
        let env_file = spec.jit_env_file_path().unwrap();
        std::fs::create_dir_all(env_file.parent().unwrap().join(JIT_ENV_FILE)).unwrap();
        let mut runner = ScriptRunner::scripted(vec![]);
        let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
        assert!(error.to_string().contains("scrub secret file"), "{error:#}");
        assert!(
            runner.seen.is_empty(),
            "Docker must not run after scrub failure"
        );
        std::fs::remove_dir_all(env_file.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_deletes_the_env_file_when_create_fails() {
        let state = temp_state("provision-fail");
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            &state,
            "jit-blob",
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok(""),
            ScriptRunner::fail(1, "Error response from daemon: conflict"),
        ]);
        assert!(ensure_runner(&mut runner, &spec, &mut || Ok(())).is_err());
        assert!(!spec.jit_env_file_path().unwrap().exists());
        assert!(!state.join("jit.env").exists());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn runner_provision_rejects_foreign_container() {
        let spec = RunnerSpec::new(
            identity(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
            Path::new("/tmp/velnor-test-runner-state"),
            "jit-blob",
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("cafe\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/stranger\n"),
        ]);
        let error = ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
    }

    #[test]
    fn connected_marker_parses() {
        assert!(parse_connected_marker("....\nConnected to GitHub\n....\n"));
        assert!(!parse_connected_marker("Listening for Jobs\n"));
        assert!(!parse_connected_marker(""));
    }

    #[test]
    #[ignore]
    fn live_tool_content_hook_proves_pinned_images() {
        use std::process::Command;
        let daemon_live = Command::new("docker")
            .arg("info")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !daemon_live {
            println!("live_tool_content_hook: SKIP, no live daemon");
            return;
        }
        struct HostRunner;
        impl WorkerRunner for HostRunner {
            fn run(&mut self, program: &str, args: &[String]) -> Result<WorkerOutput> {
                let output = Command::new(program).args(args).output()?;
                Ok(WorkerOutput {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
        }
        let profile = HomogeneousProfile::host().expect("host arch is provisioned");
        let mut runner = HostRunner;
        let hook = DockerToolContentHook;
        let runner_attestation = hook
            .verify(
                &mut runner,
                profile.runner(),
                &ToolContentExpectation::runner(),
            )
            .unwrap();
        assert_eq!(runner_attestation.content_version, RUNNER_VERSION);
        assert!(!runner_attestation.image_id.is_empty());
        let dind_attestation = hook
            .verify(&mut runner, profile.dind(), &ToolContentExpectation::dind())
            .unwrap();
        assert_eq!(dind_attestation.content_version, DIND_VERSION);
        assert!(!dind_attestation.image_id.is_empty());
    }
}

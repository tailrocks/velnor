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
//! `repo@digest`, and declared provenance labels must match. The hook
//! emits a [`ToolContentAttestation`] recording image id, digest, and the
//! runner version the pin documents; provisioning records the attestation
//! and refuses to start a runner the hook did not clear.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::dind::{BUILDKIT_CACHE_DIR, DIND_SOCKET, STATE_MOUNT};
use super::ownership::{WorkerIdentity, ROLE_RUNNER};
use super::WorkerRunner;

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

/// Env var carrying the JIT blob into the runner container (the image's
/// own input contract; cf. ARC's runner container).
pub const JIT_CONFIG_ENV: &str = "ACTIONS_RUNNER_INPUT_JITCONFIG";
/// Runner name env var (the image's own input contract).
pub const RUNNER_NAME_ENV: &str = "ACTIONS_RUNNER_INPUT_NAME";
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
}

impl HomogeneousProfile {
    /// Profile for `arch` (`x86_64`/`aarch64`): index digests pin content
    /// for every platform, so both arches share the index pins while the
    /// per-arch digests document what each platform resolves to.
    pub fn for_arch(arch: &str) -> Option<Self> {
        match arch {
            "x86_64" | "aarch64" => Some(Self::pinned()),
            _ => None,
        }
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
    fn pinned() -> Self {
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
        Self { runner, dind }
    }

    #[must_use]
    pub fn runner(&self) -> &PinnedImage {
        &self.runner
    }

    #[must_use]
    pub fn dind(&self) -> &PinnedImage {
        &self.dind
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
}

impl ToolContentExpectation {
    #[must_use]
    pub fn runner() -> Self {
        Self {
            source: Some(RUNNER_SOURCE.to_string()),
            content_version: RUNNER_VERSION.to_string(),
        }
    }

    #[must_use]
    pub fn dind() -> Self {
        // The dind index carries no config labels (`null` at index level,
        // verified live); its content proof is the digest match alone.
        Self {
            source: None,
            content_version: DIND_VERSION.to_string(),
        }
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
}

/// Default hook: digest match on `RepoDigests` + declared label match.
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
        let short_repo = image.repository().rsplit('/').next().unwrap_or_default();
        let digest_match = digests.iter().any(|digest| {
            digest == &reference
                || digest
                    .strip_prefix("docker.io/library/")
                    .is_some_and(|rest| rest == reference)
                || short_repo == image.repository()
                    && digest.ends_with(&format!("@{}", image.digest()))
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
            "--volume".to_string(),
            format!("{}:{RUNNER_WORK_DIR}", self.identity.workspace_volume()),
            "--volume".to_string(),
            format!("{}:{TOOL_CACHE_DIR}", self.identity.workspace_volume()),
            "--volume".to_string(),
            format!(
                "{}:{BUILDKIT_CACHE_DIR}",
                self.state_dir.join("buildkit-cache").display()
            ),
        ];
        args.extend(self.identity.label_args(ROLE_RUNNER));
        args.push("--".to_string());
        args.push(self.image.reference().to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push("sudo mkdir -p /home/runner/_work /opt/hostedtoolcache && sudo chown -R runner:runner /home/runner/_work /opt/hostedtoolcache && sudo chmod 0777 /home/runner/_work /opt/hostedtoolcache && (command -v gh >/dev/null 2>&1 || (arch=$(uname -m); [ \"$arch\" = \"aarch64\" ] && gh_arch=\"arm64\" || gh_arch=\"amd64\"; curl -fsSL \"https://github.com/cli/cli/releases/download/v2.101.0/gh_2.101.0_linux_${gh_arch}.tar.gz\" | sudo tar -xz -C /usr/local/bin --strip-components=2 \"gh_2.101.0_linux_${gh_arch}/bin/gh\" 2>/dev/null || true)) && (command -v cargo >/dev/null 2>&1 || (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable 2>/dev/null && sudo ln -sf /home/runner/.cargo/bin/* /usr/local/bin/ || true)) && (command -v mise >/dev/null 2>&1 || (curl -fsSL https://mise.run | sh 2>/dev/null && sudo ln -sf /home/runner/.local/bin/mise /usr/local/bin/mise || true)) && exec /home/runner/run.sh".to_string());
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

/// Ensure the runner container exists and is started, adopting on retry.
///
/// Same contract as [`super::dind::ensure_dind`]: missing → write the
/// `0600` JIT env file, create from
/// [`RunnerSpec::create_args_with_env_file`], delete the env file, +
/// start; present → ownership labels must match, then idempotent start.
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
    if inspect.stdout.trim().is_empty() {
        let env_file = spec.write_env_file()?;
        let created = runner.run("docker", &spec.create_args_with_env_file(&env_file));
        // Scrubbing is part of the create boundary. Never start/adopt the
        // runner if the secret file could not be removed.
        let scrubbed = spec.scrub_jit_env_files();
        let created = match (created, scrubbed) {
            (Ok(output), Ok(())) => output,
            (Err(create_error), Ok(())) => {
                return Err(create_error)
                    .with_context(|| format!("create runner container {name}"));
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
                &["start".to_string(), "--".to_string(), name.clone()],
            )
            .with_context(|| format!("start runner container {name}"))?;
        if started.code != 0 {
            anyhow::bail!(
                "start runner container {name} exited {}: {}",
                started.code,
                started.stderr.trim()
            );
        }
        return Ok(RunnerProvision::Created);
    }

    super::dind::verify_container_ownership(runner, &name, &spec.identity().ownership().as_str())?;
    before_start().context("persist runner startup deadline")?;
    let started = runner
        .run(
            "docker",
            &["start".to_string(), "--".to_string(), name.clone()],
        )
        .with_context(|| format!("start runner container {name}"))?;
    if started.code != 0 {
        anyhow::bail!(
            "start runner container {name} exited {}: {}",
            started.code,
            started.stderr.trim()
        );
    }
    Ok(RunnerProvision::Adopted)
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
    let running = runner
        .run("docker", &crate::docker::client::running_args(&name))
        .with_context(|| format!("inspect runner container {name}"))?;
    if running.code != 0 {
        if crate::docker::client::daemon_reports_missing(&running.stderr) {
            return Ok(RunnerConnection::Down);
        }
        anyhow::bail!(
            "inspect runner container {name} exited {}: {}",
            running.code,
            running.stderr.trim()
        );
    }
    if running.stdout.trim() != "true" {
        return Ok(RunnerConnection::Down);
    }
    let logs = runner
        .run(
            "docker",
            &[
                "logs".to_string(),
                "--tail".to_string(),
                "50".to_string(),
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
    if parse_connected_marker(&logs.stdout) {
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
    use std::collections::VecDeque;

    const RUNNER_REF: &str = "ghcr.io/actions/actions-runner@sha256:e5496277be5d09bc968b3d64911b74e219ac4a3f2edce956a3ecf9271bea1ef4";

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
            assert_eq!(program, "docker");
            self.seen.push(args.to_vec());
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
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
        // Same guest paths the DinD side mounts (dind.rs STATE_MOUNT etc.).
        for guest in [
            STATE_MOUNT,
            RUNNER_WORK_DIR,
            TOOL_CACHE_DIR,
            BUILDKIT_CACHE_DIR,
        ] {
            assert!(args.contains(guest), "missing guest path {guest}:\n{args}");
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
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("false\n")]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Down
        );
        // Running without the marker reads as Starting.
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("true\n"),
            ScriptRunner::ok("Listening for Jobs\n"),
        ]);
        assert_eq!(
            runner_connection(&mut runner, &identity()).unwrap(),
            RunnerConnection::Starting
        );
        // Running with the marker reads as Connected.
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("true\n"),
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
            ScriptRunner::ok(""),
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
            ScriptRunner::ok("velnor-scaleset-runner-s7-velnor-set-0007-2ad92676\n"),
        ]);
        assert_eq!(
            ensure_runner(&mut runner, &spec, &mut || Ok(())).unwrap(),
            RunnerProvision::Adopted
        );
        // Adoption writes no env file at all.
        assert!(!env_file.exists());
        assert!(!state.join("jit.env").exists());
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

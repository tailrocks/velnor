//! Real container workloads driven through the Docker CLI.
//!
//! This driver observes the container lifecycle and nothing else. It never
//! reports broker, acquisition, admission or capacity latency, because it never
//! talks to a broker: [`crate::record::BenchRecord::validate`] enforces that
//! separation on the record it produces.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context as _, Result};

use crate::{
    drivers::{isolated_docker::IsolatedDockerDaemon, Context, Workload},
    gittrace::GitEvidence,
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Invocation},
};

/// Shape of the container workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Image already on the host; measures pure container lifecycle.
    ExistingImage,
    /// Pulls an image through a disposable daemon with a private data root.
    ImagePull,
    /// Job-shaped container: workspace mount plus a user command.
    JobContainer,
    /// Job container plus a linked service container and a readiness wait.
    ServiceContainer,
    /// `docker build` with every layer cached.
    BuildCached,
    /// `docker build` with the cache defeated at the first instruction.
    BuildUncached,
    /// `docker buildx build` through the BuildKit driver.
    Buildx,
}

/// The command a job-shaped container runs as its first user step.
const USER_COMMAND: &str = "printf velnor-bench-first-user-command";

/// Service image used for the service-container scenario. Chosen because it is
/// tiny and has a deterministic readiness signal.
const SERVICE_IMAGE: &str = "docker.io/library/redis:7-alpine";

static NEXT_BUILD_TAG: AtomicU64 = AtomicU64::new(0);
static NEXT_DOCKER_OWNER_ID: AtomicU64 = AtomicU64::new(1);

fn is_build_kind(kind: Kind) -> bool {
    matches!(kind, Kind::BuildCached | Kind::BuildUncached | Kind::Buildx)
}

fn unique_build_tag() -> String {
    let serial = NEXT_BUILD_TAG.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "velnor-bench-cached-{}-{nonce:x}-{serial}:latest",
        std::process::id()
    )
}

fn with_cleanup(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup_error) => {
            primary.context(format!("workload cleanup also failed: {cleanup_error:#}"))
        }
    }
}

fn remove_scratch_directory(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn parse_docker_id(stdout: &str) -> Result<String> {
    let id = stdout.trim();
    if id.len() < 12
        || id.len() > 64
        || id.split_whitespace().count() != 1
        || !id.chars().all(|character| character.is_ascii_hexdigit())
    {
        bail!("Docker returned an invalid container ID: {id:?}");
    }
    Ok(id.to_owned())
}

fn parse_owned_image_inspect(output: &str, expected_owner: &str) -> Result<String> {
    let mut fields = output.split_whitespace();
    let raw_id = fields
        .next()
        .context("Docker image inspect returned no image ID")?;
    let owner = fields
        .next()
        .context("Docker image inspect returned no owner label")?;
    if owner != expected_owner {
        bail!("Docker image owner label mismatch: expected {expected_owner:?}, got {owner:?}");
    }
    if fields.next().is_some() {
        bail!("Docker image inspect returned extra fields");
    }
    let id = raw_id.strip_prefix("sha256:").unwrap_or(raw_id);
    let id = parse_docker_id(id)?;
    Ok(format!("sha256:{id}"))
}

fn verify_owned_image_identity(
    output: &str,
    expected_owner: &str,
    expected_id: &str,
) -> Result<String> {
    let id = parse_owned_image_inspect(output, expected_owner)?;
    if id != expected_id {
        bail!("Docker image identity mismatch: expected {expected_id:?}, got {id:?}");
    }
    Ok(id)
}

fn select_image_digest(repo_digests: &str, image_id: &str) -> Result<String> {
    if let Some(repo_digest) = repo_digests
        .split(',')
        .map(str::trim)
        .find(|digest| !digest.is_empty())
    {
        let (repository, raw_id) = repo_digest
            .split_once("@sha256:")
            .context("Docker image inspect returned an invalid repository digest")?;
        if repository.is_empty() {
            bail!("Docker image inspect returned an empty repository digest name");
        }
        let id = parse_docker_id(raw_id)
            .context("Docker image inspect returned an invalid repository digest ID")?;
        return Ok(format!("{repository}@sha256:{id}"));
    }

    let id = image_id.strip_prefix("sha256:").unwrap_or(image_id);
    Ok(format!(
        "sha256:{}",
        parse_docker_id(id).context("Docker image inspect returned an invalid image ID")?
    ))
}

fn parse_image_digest_inspect(output: &str) -> Result<String> {
    let (repo_digests, image_id) = output
        .trim_end()
        .split_once('\t')
        .context("Docker image inspect returned no image identity")?;
    select_image_digest(repo_digests, image_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    Container,
    Network,
}

impl ResourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Network => "network",
        }
    }
}

fn parse_owned_resource_listing(
    output: &str,
    expected_name: &str,
    kind: ResourceKind,
) -> Result<String> {
    let mut found = None;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut fields = line.split_whitespace();
        let raw_id = fields
            .next()
            .with_context(|| format!("Docker {} listing returned no ID", kind.as_str()))?;
        let name = fields
            .next()
            .with_context(|| format!("Docker {} listing returned no name", kind.as_str()))?;
        if fields.next().is_some() {
            bail!(
                "Docker {} listing returned extra fields for {name:?}",
                kind.as_str()
            );
        }
        if name != expected_name {
            bail!(
                "Docker {} listing did not prove exact name {expected_name:?}: got {name:?}",
                kind.as_str()
            );
        }
        if found.is_some() {
            bail!(
                "Docker {} listing returned multiple resources named {expected_name:?}",
                kind.as_str()
            );
        }
        found =
            Some(parse_docker_id(raw_id).with_context(|| {
                format!("Docker {} listing returned an invalid ID", kind.as_str())
            })?);
    }
    found.with_context(|| {
        format!(
            "Docker {} listing did not prove owned resource {expected_name:?}",
            kind.as_str()
        )
    })
}

fn parse_owned_resource_inspect(
    output: &str,
    expected_id: &str,
    expected_name: &str,
    expected_owner: &str,
    expected_role: &str,
    expected_image: Option<&str>,
    kind: ResourceKind,
) -> Result<String> {
    let fields: Vec<&str> = output.trim().split('\t').collect();
    let expected_fields = if kind == ResourceKind::Container {
        5
    } else {
        4
    };
    if fields.len() != expected_fields {
        bail!(
            "Docker {} inspect returned an incomplete ownership spec",
            kind.as_str()
        );
    }
    let id = parse_docker_id(fields[0])
        .with_context(|| format!("Docker {} inspect returned an invalid ID", kind.as_str()))?;
    if id != expected_id {
        bail!(
            "Docker {} inspect identity mismatch: expected {expected_id:?}, got {id:?}",
            kind.as_str()
        );
    }
    let inspected_name = match kind {
        ResourceKind::Container => fields[1].strip_prefix('/').unwrap_or(fields[1]),
        ResourceKind::Network => fields[1],
    };
    if inspected_name != expected_name {
        bail!(
            "Docker {} inspect name mismatch: expected {expected_name:?}, got {inspected_name:?}",
            kind.as_str()
        );
    }
    if fields[2] != expected_owner {
        bail!(
            "Docker {} inspect owner mismatch: expected {expected_owner:?}, got {:?}",
            kind.as_str(),
            fields[2]
        );
    }
    if fields[3] != expected_role {
        bail!(
            "Docker {} inspect role mismatch: expected {expected_role:?}, got {:?}",
            kind.as_str(),
            fields[3]
        );
    }
    if let Some(expected_image) = expected_image {
        if kind != ResourceKind::Container {
            bail!("Docker network ownership cannot carry an image expectation");
        }
        if fields[4] != expected_image {
            bail!(
                "Docker container image mismatch: expected {expected_image:?}, got {:?}",
                fields[4]
            );
        }
    }
    Ok(id)
}

fn recover_owned_resource_id(
    context: &mut Context,
    name: &str,
    role: &str,
    owner: &str,
    kind: ResourceKind,
    expected_image: Option<&str>,
) -> Result<String> {
    let owner_filter = format!("label=com.velnor.bench.owner={owner}");
    let role_filter = format!("label=com.velnor.bench.role={role}");
    let name_filter = format!("name=^{name}$");
    let listing_format = "{{.ID}}\t{{.Names}}";
    let inspect_format = match kind {
        ResourceKind::Container => {
            "{{.Id}}\t{{.Name}}\t{{index .Config.Labels \"com.velnor.bench.owner\"}}\t{{index .Config.Labels \"com.velnor.bench.role\"}}\t{{.Config.Image}}"
        }
        ResourceKind::Network => {
            "{{.Id}}\t{{.Name}}\t{{index .Labels \"com.velnor.bench.owner\"}}\t{{index .Labels \"com.velnor.bench.role\"}}"
        }
    };
    let (program, args) = match kind {
        ResourceKind::Container => (
            "docker",
            vec![
                "container".to_owned(),
                "ls".to_owned(),
                "--all".to_owned(),
                "--no-trunc".to_owned(),
                "--filter".to_owned(),
                owner_filter,
                "--filter".to_owned(),
                role_filter,
                "--filter".to_owned(),
                name_filter,
                "--format".to_owned(),
                listing_format.to_owned(),
            ],
        ),
        ResourceKind::Network => (
            "docker",
            vec![
                "network".to_owned(),
                "ls".to_owned(),
                "--no-trunc".to_owned(),
                "--filter".to_owned(),
                owner_filter,
                "--filter".to_owned(),
                role_filter,
                "--filter".to_owned(),
                name_filter,
                "--format".to_owned(),
                "{{.ID}}\t{{.Name}}".to_owned(),
            ],
        ),
    };
    let listing = context
        .runner
        .capture(program, &args)
        .map_err(anyhow::Error::msg)?;
    let id = parse_owned_resource_listing(&listing, name, kind)?;
    let inspect_args = match kind {
        ResourceKind::Container => vec![
            "inspect".to_owned(),
            "--format".to_owned(),
            inspect_format.to_owned(),
            id.clone(),
        ],
        ResourceKind::Network => vec![
            "network".to_owned(),
            "inspect".to_owned(),
            "--format".to_owned(),
            inspect_format.to_owned(),
            id.clone(),
        ],
    };
    let inspected = context
        .runner
        .capture("docker", &inspect_args)
        .map_err(anyhow::Error::msg)?;
    parse_owned_resource_inspect(&inspected, &id, name, owner, role, expected_image, kind)
}

fn remove_owned_image(
    context: &mut Context,
    tag: &str,
    expected_id: &str,
    expected_owner: &str,
) -> Result<()> {
    let inspected = context
        .runner
        .run(
            "docker",
            &[
                "image",
                "inspect",
                "--format",
                "{{.Id}}\t{{index .Config.Labels \"com.velnor.bench.owner\"}}",
                tag,
            ],
        )
        .context("docker owned image inspection")
        .cloned()?;
    if !inspected.ok() {
        if inspected
            .stderr
            .to_ascii_lowercase()
            .contains("no such image")
        {
            return Ok(());
        }
        bail!(
            "docker owned image inspection failed with exit code {}: {}",
            inspected.code,
            inspected.stderr.trim()
        );
    }
    verify_owned_image_identity(&inspected.stdout, expected_owner, expected_id)?;
    // Remove by the verified immutable ID, not by the mutable tag. If another
    // client retags `tag` after inspection, ID removal still targets only the
    // image whose owner label and identity were proved above.
    let invocation = context
        .runner
        .run("docker", &["image", "rm", expected_id])
        .context("docker owned image removal")?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such image")
    {
        return Ok(());
    }
    bail!(
        "docker owned image removal failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn resolve_image_digest(context: &mut Context, image: &str) -> Result<String> {
    let output = context
        .runner
        .capture(
            "docker",
            &[
                "image",
                "inspect",
                "--format",
                "{{join .RepoDigests \",\"}}\t{{.Id}}",
                image,
            ],
        )
        .map_err(anyhow::Error::msg)?;
    parse_image_digest_inspect(&output)
}

fn prepare_image(
    context: &mut Context,
    image: &str,
    pull_always: bool,
    operation: &str,
) -> Result<String> {
    if pull_always {
        let pulled = context
            .runner
            .run("docker", &["pull", image])
            .with_context(|| format!("pulling the {operation}"))?;
        require_success(pulled, &format!("docker pull {operation}"))?;
    } else {
        let present = context
            .runner
            .run("docker", &["image", "inspect", image])
            .map(|invocation| invocation.ok())
            .unwrap_or(false);
        if !present {
            let pulled = context
                .runner
                .run("docker", &["pull", image])
                .with_context(|| format!("pulling the {operation}"))?;
            require_success(pulled, &format!("docker pull {operation}"))?;
        }
    }
    resolve_image_digest(context, image)
}

fn force_remove_id(context: &mut Context, id: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["rm", "-f", id])
        .with_context(|| format!("removing Docker container {id}"))?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such container")
    {
        return Ok(());
    }
    bail!(
        "removing Docker container {id} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn remove_network_id(context: &mut Context, id: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["network", "rm", id])
        .with_context(|| format!("removing Docker network {id}"))?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such network")
    {
        return Ok(());
    }
    bail!(
        "removing Docker network {id} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

pub(super) fn build(scenario: &Scenario) -> Result<Box<dyn Workload>> {
    let kind = match scenario.id {
        "docker/existing-image" => Kind::ExistingImage,
        "docker/image-pull" => Kind::ImagePull,
        "docker/simple-job-container" => Kind::JobContainer,
        "docker/service-container" => Kind::ServiceContainer,
        "docker/build-cached" => Kind::BuildCached,
        "docker/build-uncached" => Kind::BuildUncached,
        "docker/buildx" => Kind::Buildx,
        // Rust scenarios fall back to running the same build inside the real
        // job image; that workload lives in the cargo driver, which knows how
        // to mutate a workspace. Routing them here would measure a container,
        // not a build.
        other => bail!(
            "no docker-direct workload is implemented for {other}; \
             it is declared in the matrix and reported as unrun"
        ),
    };
    Ok(Box::new(DockerWorkload {
        kind,
        build_tag: is_build_kind(kind).then(unique_build_tag),
        build_attempted: false,
        owned_images: Vec::new(),
        measured_job_image: None,
        measured_service_image: None,
        scratch: ScratchOwner::new(),
        owned_containers: Vec::new(),
        owned_networks: Vec::new(),
        iteration: 0,
        notes: Vec::new(),
    }))
}

#[derive(Debug, Default)]
struct ScratchOwner {
    id: u64,
    nonce: u128,
    root: Option<PathBuf>,
}

impl ScratchOwner {
    fn new() -> Self {
        Self {
            id: NEXT_DOCKER_OWNER_ID.fetch_add(1, Ordering::Relaxed),
            nonce: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
            ..Self::default()
        }
    }

    fn allocate(&mut self, work_root: &Path) -> Result<PathBuf> {
        let root = work_root.join(format!(
            "docker-{}-{}-{}",
            std::process::id(),
            self.nonce,
            self.id
        ));
        self.root = Some(root.clone());
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating Docker workload scratch {}", root.display()))?;
        Ok(root)
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(name))
            .context("Docker workload was not prepared")
    }
}

#[derive(Debug, Clone)]
struct OwnedContainer {
    name: String,
    role: String,
    id: Option<String>,
}

#[derive(Debug, Clone)]
struct OwnedNetwork {
    name: String,
    role: String,
    id: Option<String>,
}

#[derive(Debug, Clone)]
struct OwnedImage {
    tag: String,
    id: String,
}

struct DockerWorkload {
    kind: Kind,
    build_tag: Option<String>,
    build_attempted: bool,
    owned_images: Vec<OwnedImage>,
    measured_job_image: Option<String>,
    measured_service_image: Option<String>,
    scratch: ScratchOwner,
    owned_containers: Vec<OwnedContainer>,
    owned_networks: Vec<OwnedNetwork>,
    iteration: u64,
    notes: Vec<String>,
}

/// Wall time of one measured block, in milliseconds.
fn timed<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let value = body();
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    (value, elapsed)
}

fn require_success(invocation: &Invocation, operation: &str) -> Result<()> {
    if invocation.ok() {
        return Ok(());
    }
    bail!(
        "{operation} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    );
}

fn inspect_container_state(context: &mut Context, id: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["inspect", "--format", "{{.State.Status}}", id])?;
    require_success(invocation, "docker inspect completion")?;
    if invocation.stdout.trim().is_empty() {
        bail!("docker inspect completion returned no container state");
    }
    Ok(())
}

impl DockerWorkload {
    fn own_container(&mut self, name: &str, role: &str) {
        if !self.owned_containers.iter().any(|owned| owned.name == name) {
            self.owned_containers.push(OwnedContainer {
                name: name.to_owned(),
                role: role.to_owned(),
                id: None,
            });
        }
    }

    fn record_container_id(&mut self, name: &str, invocation: &Invocation) -> Result<String> {
        let id = parse_docker_id(&invocation.stdout).with_context(|| {
            format!("Docker create for container {name} returned no verified ID")
        })?;
        let owned = self
            .owned_containers
            .iter_mut()
            .find(|owned| owned.name == name)
            .with_context(|| format!("container {name} was not registered before creation"))?;
        owned.id = Some(id.clone());
        Ok(id)
    }

    fn own_network(&mut self, name: &str, role: &str) {
        if !self.owned_networks.iter().any(|owned| owned.name == name) {
            self.owned_networks.push(OwnedNetwork {
                name: name.to_owned(),
                role: role.to_owned(),
                id: None,
            });
        }
    }

    fn record_network_id(&mut self, name: &str, invocation: &Invocation) -> Result<String> {
        let id = parse_docker_id(&invocation.stdout)
            .with_context(|| format!("Docker network create for {name} returned no verified ID"))?;
        let owned = self
            .owned_networks
            .iter_mut()
            .find(|owned| owned.name == name)
            .with_context(|| format!("network {name} was not registered before creation"))?;
        owned.id = Some(id.clone());
        Ok(id)
    }

    fn owner_token(&self) -> String {
        format!("{:032x}-{}", self.scratch.nonce, self.scratch.id)
    }

    fn owner_label(&self) -> String {
        format!("com.velnor.bench.owner={}", self.owner_token())
    }

    fn role_label(role: &str) -> String {
        format!("com.velnor.bench.role={role}")
    }

    fn resource_labels(&self, role: &str) -> [String; 4] {
        [
            "--label".to_owned(),
            self.owner_label(),
            "--label".to_owned(),
            Self::role_label(role),
        ]
    }

    fn own_image(&mut self, tag: &str, id: String) -> Result<()> {
        if let Some(owned) = self.owned_images.iter().find(|owned| owned.tag == tag) {
            if owned.id != id {
                bail!(
                    "Docker image tag {tag:?} identity mismatch: expected {:?}, got {id:?}",
                    owned.id
                );
            }
            return Ok(());
        }
        self.owned_images.push(OwnedImage {
            tag: tag.to_owned(),
            id,
        });
        Ok(())
    }

    fn capture_owned_image(&mut self, context: &mut Context, tag: &str) -> Result<()> {
        let output = context
            .runner
            .capture(
                "docker",
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{.Id}} {{index .Config.Labels \"com.velnor.bench.owner\"}}",
                    tag,
                ],
            )
            .map_err(anyhow::Error::msg)?;
        let id = parse_owned_image_inspect(&output, &self.owner_token())?;
        self.own_image(tag, id)
    }

    fn adopt_owned_image(&mut self, context: &mut Context, tag: &str) -> Result<()> {
        let expected_id = self
            .owned_images
            .iter()
            .find(|owned| owned.tag == tag)
            .map(|owned| owned.id.as_str());
        let output = context
            .runner
            .run(
                "docker",
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{.Id}} {{index .Config.Labels \"com.velnor.bench.owner\"}}",
                    tag,
                ],
            )
            .context("docker owned image recovery inspection")
            .cloned()?;
        if !output.ok() {
            if output.stderr.to_ascii_lowercase().contains("no such image") {
                return Ok(());
            }
            bail!(
                "Docker image recovery inspection failed with exit code {}: {}",
                output.code,
                output.stderr.trim()
            );
        }
        let id = parse_owned_image_inspect(&output.stdout, &self.owner_token())?;
        if let Some(expected_id) = expected_id
            && id != expected_id
        {
            bail!(
                "Docker image tag {tag:?} identity mismatch: expected {expected_id:?}, got {id:?}"
            );
        }
        self.own_image(tag, id)
    }

    fn cleanup_owned_images(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();
        let mut remaining = Vec::new();
        for image in std::mem::take(&mut self.owned_images) {
            if let Err(error) =
                remove_owned_image(context, &image.tag, &image.id, &self.owner_token())
            {
                failures.push(format!("image {} ({}): {error:#}", image.tag, image.id));
                remaining.push(image);
            }
        }
        self.owned_images = remaining;
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("Docker image cleanup failed: {}", failures.join("; "))
        }
    }

    fn cleanup_owned_resources(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();
        let mut remaining_containers = Vec::new();
        for mut resource in std::mem::take(&mut self.owned_containers) {
            if resource.id.is_none() {
                let expected_image = match resource.role.as_str() {
                    "service" => self.measured_service_image.as_deref(),
                    "job" => self.measured_job_image.as_deref(),
                    _ => None,
                };
                match recover_owned_resource_id(
                    context,
                    &resource.name,
                    &resource.role,
                    &self.owner_token(),
                    ResourceKind::Container,
                    expected_image,
                ) {
                    Ok(id) => resource.id = Some(id),
                    Err(error) => {
                        failures.push(format!("container {}: {error:#}", resource.name));
                        remaining_containers.push(resource);
                        continue;
                    }
                }
            }
            let result = match resource.id.as_deref() {
                Some(id) => force_remove_id(context, id),
                None => Err(anyhow::anyhow!(
                    "container {} has no verified Docker ID after recovery",
                    resource.name
                )),
            };
            if let Err(error) = result {
                failures.push(format!("container {}: {error:#}", resource.name));
                remaining_containers.push(resource);
            }
        }
        self.owned_containers = remaining_containers;

        let mut remaining_networks = Vec::new();
        for mut resource in std::mem::take(&mut self.owned_networks) {
            if resource.id.is_none() {
                match recover_owned_resource_id(
                    context,
                    &resource.name,
                    &resource.role,
                    &self.owner_token(),
                    ResourceKind::Network,
                    None,
                ) {
                    Ok(id) => resource.id = Some(id),
                    Err(error) => {
                        failures.push(format!("network {}: {error:#}", resource.name));
                        remaining_networks.push(resource);
                        continue;
                    }
                }
            }
            let result = match resource.id.as_deref() {
                Some(id) => remove_network_id(context, id),
                None => Err(anyhow::anyhow!(
                    "network {} has no verified Docker ID after recovery",
                    resource.name
                )),
            };
            if let Err(error) = result {
                failures.push(format!("network {}: {error:#}", resource.name));
                remaining_networks.push(resource);
            }
        }
        self.owned_networks = remaining_networks;

        if failures.is_empty() {
            Ok(())
        } else {
            bail!("Docker resource cleanup failed: {}", failures.join("; "))
        }
    }

    fn container_name(&self, prefix: &str) -> String {
        format!(
            "velnor-bench-{prefix}-{}-{}-{}",
            std::process::id(),
            self.scratch.id,
            self.iteration
        )
    }
}

impl Workload for DockerWorkload {
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        if self.scratch.root.is_some() {
            bail!("Docker workload still owns scratch; teardown is required before prepare");
        }
        self.scratch.allocate(&context.work_root)?;
        if self.kind == Kind::ImagePull {
            self.notes.push(
                "image-pull uses a disposable isolated Docker daemon for every measured iteration"
                    .to_owned(),
            );
            return Ok(());
        }
        let job_image = context.job_image.clone();
        self.measured_job_image = Some(prepare_image(context, &job_image, false, "job image")?);
        if self.kind == Kind::ServiceContainer {
            let service_image = prepare_image(context, SERVICE_IMAGE, true, "service image")?;
            self.notes.push(format!(
                "service container image resolved to immutable digest {service_image}"
            ));
            self.measured_service_image = Some(service_image);
        }
        if matches!(
            self.kind,
            Kind::BuildCached | Kind::BuildUncached | Kind::Buildx
        ) {
            let dir = self.scratch.path("docker-build-context")?;
            std::fs::create_dir_all(&dir)?;
            let image = self
                .measured_job_image
                .as_deref()
                .context("job image digest was not resolved during preparation")?;
            write_build_context(&dir, image)?;
            if self.kind == Kind::BuildCached {
                // Warm the layer cache once, outside the measurement.
                let tag = self
                    .build_tag
                    .as_deref()
                    .expect("build workloads have an owned image tag")
                    .to_owned();
                self.build_attempted = true;
                let mut args = vec!["build".to_owned()];
                args.extend(self.resource_labels("image"));
                args.extend(["-t".to_owned(), tag.clone(), dir.display().to_string()]);
                let warmed = context.runner.run("docker", &args)?;
                require_success(warmed, "docker cached-build warmup")?;
                self.capture_owned_image(context, &tag)?;
            }
        }
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        context.runner.reset();
        let scratch_root = self.scratch.path("")?;
        let disk_before = tree_bytes(&scratch_root);
        let started = Instant::now();

        let mut stages = BTreeMap::new();
        let mut resources = Resources::default();

        match self.kind {
            Kind::ExistingImage | Kind::JobContainer => {
                self.run_container_lifecycle(context, &mut stages)?;
            }
            Kind::ImagePull => self.run_image_pull(context, &mut stages)?,
            Kind::ServiceContainer => {
                self.run_service_container(context, &mut stages)?;
            }
            Kind::BuildCached | Kind::BuildUncached | Kind::Buildx => {
                self.run_build(context, &mut stages, &mut resources)?;
            }
        }

        let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&scratch_root);

        resources.cpu_user_us = usage.user_us;
        resources.cpu_system_us = usage.system_us;
        resources.max_rss_bytes = usage.max_rss_bytes;
        resources.block_input_ops = usage.block_input_ops;
        resources.block_output_ops = usage.block_output_ops;
        resources.disk_bytes_delta =
            i64::try_from(disk_after).unwrap_or(i64::MAX) - i64::try_from(disk_before).unwrap_or(0);
        resources.process_count = context.runner.process_count() as u64;
        resources.docker_invocations = context.runner.count_of("docker") as u64;

        Ok(Observation {
            total_ms,
            stages_ms: stages,
            checkout_phases_ms: BTreeMap::new(),
            resources,
            git: GitEvidence::NotMeasured,
        })
    }

    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();

        if let Err(error) = self.cleanup_owned_resources(context) {
            failures.push(format!("owned resource cleanup failed: {error:#}"));
        }

        if self.build_attempted {
            if let Some(tag) = self.build_tag.clone()
                && !self.owned_images.iter().any(|image| image.tag == tag)
                && let Err(error) = self.adopt_owned_image(context, &tag)
            {
                failures.push(format!("owned image recovery failed: {error:#}"));
            }
            if let Err(error) = self.cleanup_owned_images(context) {
                failures.push(format!("owned image cleanup failed: {error:#}"));
            }
        }

        if self.owned_containers.is_empty()
            && self.owned_networks.is_empty()
            && self.owned_images.is_empty()
            && failures.is_empty()
            && let Some(root) = self.scratch.root.clone()
        {
            match remove_scratch_directory(&root) {
                Ok(()) => self.scratch.root = None,
                Err(error) => failures.push(format!(
                    "remove Docker workload scratch {} failed: {error:#}",
                    root.display()
                )),
            }
        }

        if !failures.is_empty() {
            bail!("Docker workload teardown failed: {}", failures.join("; "));
        }
        Ok(())
    }

    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }
}

impl DockerWorkload {
    fn run_image_pull(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let scratch_root = self.scratch.path("")?;
        let owner = self.owner_token();
        let (daemon, setup_ms) = timed(|| {
            IsolatedDockerDaemon::start(&scratch_root, &owner, self.iteration, &mut context.runner)
        });
        let mut daemon = daemon?;
        stages.insert(Stage::DockerSetup, setup_ms);

        let image = context.job_image.clone();
        let (pull_result, pull_ms) = timed(|| -> Result<()> {
            let pulled = daemon
                .run(&mut context.runner, &["pull", &image])
                .map_err(anyhow::Error::from)
                .context("running isolated Docker image pull")?
                .clone();
            require_success(&pulled, "docker image pull")
        });
        if let Err(error) = pull_result {
            return Err(with_cleanup(error, daemon.shutdown()));
        }
        stages.insert(Stage::FirstUserCommand, pull_ms);

        let (shutdown, teardown_ms) = timed(|| daemon.shutdown());
        shutdown?;
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_container_lifecycle(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let name = self.container_name("job");
        let image = self
            .measured_job_image
            .clone()
            .context("job image digest was not resolved during preparation")?;
        let mount = if self.kind == Kind::JobContainer {
            let workspace = self.scratch.path("workspace")?;
            std::fs::create_dir_all(&workspace)?;
            Some(format!("{}:/velnor/workspace", workspace.display()))
        } else {
            None
        };

        let (setup, setup_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", &image],
                )
                .map(|invocation| invocation.ok())
        });
        if !setup.unwrap_or(false) {
            bail!("job image {image} disappeared between preparation and measurement");
        }
        stages.insert(Stage::DockerSetup, setup_ms);

        let mut create_args = vec![
            "create".to_owned(),
            "--name".to_owned(),
            name.clone(),
            "--entrypoint".to_owned(),
            "/bin/sh".to_owned(),
        ];
        create_args.extend(self.resource_labels("job"));
        if let Some(mount) = &mount {
            create_args.push("-v".to_owned());
            create_args.push(mount.clone());
        }
        create_args.push(image.clone());
        create_args.push("-c".to_owned());
        // Keep the container alive so start and the first user command are
        // separately observable, exactly as the runner keeps a job container.
        create_args.push("sleep 30".to_owned());

        self.own_container(&name, "job");
        let (created, create_ms) = timed(|| context.runner.run("docker", &create_args).cloned());
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !created.ok() {
            let stderr = created.stderr.clone();
            let primary = anyhow::anyhow!("docker create failed: {}", stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        let id = match self.record_container_id(&name, &created) {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned_resources(context))),
        };
        stages.insert(Stage::ContainerCreate, create_ms);

        let (started, start_ms) = timed(|| context.runner.run("docker", &["start", &id]).cloned());
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !started.ok() {
            let stderr = started.stderr.clone();
            let primary = anyhow::anyhow!("docker start failed: {}", stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::ContainerStart, start_ms);

        let (executed, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &id, "/bin/sh", "-c", USER_COMMAND])
                .cloned()
        });
        let executed = match executed {
            Ok(executed) => executed,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !executed.ok() {
            let stderr = executed.stderr.clone();
            let primary = anyhow::anyhow!("first user command failed: {}", stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);

        // What a runner does after the last step and before teardown: read the
        // exit state back out of the daemon.
        let (completion, completion_ms) = timed(|| inspect_container_state(context, &id));
        if let Err(error) = completion {
            return Err(with_cleanup(error, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) =
            timed(|| context.runner.run("docker", &["rm", "-f", &id]).cloned());
        let removed = match removed {
            Ok(removed) => removed,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !removed.ok() {
            let stderr = removed.stderr.clone();
            let primary = anyhow::anyhow!("docker container teardown failed: {}", stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_service_container(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let network = self.container_name("net");
        let service = self.container_name("svc");
        let job = self.container_name("job");
        let image = self
            .measured_job_image
            .clone()
            .context("job image digest was not resolved during preparation")?;
        let service_image = self
            .measured_service_image
            .clone()
            .context("service image digest was not resolved during preparation")?;

        // Register before the daemon side effect. A missing ID remains
        // pending and can never authorize deletion by recyclable name.
        self.own_network(&network, "network");
        let mut network_args = vec!["network".to_owned(), "create".to_owned()];
        network_args.extend(self.resource_labels("network"));
        network_args.push(network.clone());
        let (network_created, network_ms) =
            timed(|| context.runner.run("docker", &network_args).cloned());
        let network_created = match network_created {
            Ok(network_created) => network_created,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !network_created.ok() {
            let primary = anyhow::anyhow!(
                "docker network create failed: {}",
                network_created.stderr.trim()
            );
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        let network_id = match self.record_network_id(&network, &network_created) {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned_resources(context))),
        };

        self.own_container(&service, "service");
        let mut service_args = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            service.clone(),
            "--network".to_owned(),
            network_id.clone(),
        ];
        service_args.extend(self.resource_labels("service"));
        service_args.push(service_image);
        let (service_started, service_ms) =
            timed(|| context.runner.run("docker", &service_args).cloned());
        let service_started = match service_started {
            Ok(service_started) => service_started,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !service_started.ok() {
            let primary = anyhow::anyhow!(
                "docker service start failed: {}",
                service_started.stderr.trim()
            );
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        let service_id = match self.record_container_id(&service, &service_started) {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned_resources(context))),
        };
        let setup_ms = network_ms.saturating_add(service_ms);
        stages.insert(Stage::DockerSetup, setup_ms);

        // Readiness wait is part of container start from a job's perspective.
        let (ready, start_ms) = timed(|| wait_for_health(context, &service_id));
        stages.insert(Stage::ContainerStart, start_ms);
        if !ready {
            let primary = anyhow::anyhow!("service container never became reachable");
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }

        self.own_container(&job, "job");
        let mut job_args = vec![
            "create".to_owned(),
            "--name".to_owned(),
            job.clone(),
            "--network".to_owned(),
            network_id,
            "--entrypoint".to_owned(),
            "/bin/sh".to_owned(),
        ];
        job_args.extend(self.resource_labels("job"));
        job_args.extend([image.clone(), "-c".to_owned(), "sleep 30".to_owned()]);
        let (created, create_ms) = timed(|| context.runner.run("docker", &job_args).cloned());
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !created.ok() {
            let primary = anyhow::anyhow!(
                "docker service job create failed: {}",
                created.stderr.trim()
            );
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        let job_id = match self.record_container_id(&job, &created) {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned_resources(context))),
        };
        stages.insert(Stage::ContainerCreate, create_ms);

        let (started, job_start_ms) =
            timed(|| context.runner.run("docker", &["start", &job_id]).cloned());
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !started.ok() {
            let primary =
                anyhow::anyhow!("docker service job start failed: {}", started.stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        let container_start_ms = stages
            .get(&Stage::ContainerStart)
            .copied()
            .unwrap_or_default();
        stages.insert(
            Stage::ContainerStart,
            container_start_ms.saturating_add(job_start_ms),
        );

        let (executed, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &job_id, "/bin/sh", "-c", USER_COMMAND])
                .cloned()
        });
        let executed = match executed {
            Ok(executed) => executed,
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    self.cleanup_owned_resources(context),
                ));
            }
        };
        if !executed.ok() {
            let primary = anyhow::anyhow!(
                "docker service first user command failed: {}",
                executed.stderr.trim()
            );
            return Err(with_cleanup(primary, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);
        let (completion, completion_ms) = timed(|| inspect_container_state(context, &job_id));
        if let Err(error) = completion {
            return Err(with_cleanup(error, self.cleanup_owned_resources(context)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) = timed(|| self.cleanup_owned_resources(context));
        removed?;
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_build(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
        resources: &mut Resources,
    ) -> Result<()> {
        let dir = self.scratch.path("docker-build-context")?;
        let (prepared, setup_ms) = timed(|| -> Result<()> {
            if self.kind == Kind::BuildUncached {
                // Defeat the cache at the first instruction, which is what an
                // uncached build actually means.
                std::fs::write(
                    dir.join("cache-buster"),
                    format!("{}-{}", std::process::id(), self.iteration),
                )?;
            }
            Ok(())
        });
        prepared?;
        stages.insert(Stage::DockerSetup, setup_ms);

        let path = dir.display().to_string();
        // Never retarget the warmup or a prior measured build tag. A fresh tag
        // keeps each immutable build identity independently recoverable.
        self.build_tag = Some(unique_build_tag());
        let tag = self
            .build_tag
            .clone()
            .expect("build workloads have an owned image tag");
        let mut args = if self.kind == Kind::Buildx {
            vec!["buildx".to_owned(), "build".to_owned(), "--load".to_owned()]
        } else {
            vec!["build".to_owned()]
        };
        args.extend(self.resource_labels("image"));
        args.extend(["-t".to_owned(), tag.clone(), path]);
        // A failed build may have created an image before it reported the
        // error. Teardown recovers only the exact owner-labeled IDs.
        self.build_attempted = true;
        let (built, build_ms) = timed(|| context.runner.run("docker", &args).cloned());
        let built = built?;
        if !built.ok() {
            bail!("docker build failed: {}", built.stderr.trim());
        }
        let (captured, inspect_ms) = timed(|| self.capture_owned_image(context, &tag));
        captured?;
        let (hits, misses) = count_layer_cache(&format!("{}\n{}", built.stdout, built.stderr));
        resources.cache_hits = hits;
        resources.cache_misses = misses;
        stages.insert(Stage::FirstUserCommand, build_ms);
        stages.insert(Stage::CompletionOverhead, inspect_ms);
        stages.insert(Stage::Teardown, 0);
        Ok(())
    }
}

/// Poll until the daemon reports the container running and its port answers.
fn wait_for_health(context: &mut Context, id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(status) = context
            .runner
            .capture("docker", &["inspect", "--format", "{{.State.Running}}", id])
            && status == "true"
            && let Ok(probe) = context
                .runner
                .run("docker", &["exec", id, "redis-cli", "ping"])
            && probe.ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// BuildKit prints `CACHED` for a reused step; every other step line is a miss.
fn finish_buildkit_step(
    steps: &mut BTreeMap<String, ()>,
    id: &str,
    cached: bool,
    hits: &mut u64,
    misses: &mut u64,
) {
    if steps.remove(id).is_some() {
        if cached {
            *hits += 1;
        } else {
            *misses += 1;
        }
    }
}

fn count_layer_cache(output: &str) -> (u64, u64) {
    let mut hits = 0_u64;
    let mut misses = 0_u64;
    let mut buildkit_steps = BTreeMap::<String, ()>::new();
    let mut classic_step_cached = None;

    for line in output.lines() {
        let trimmed = line.trim_start();
        // BuildKit progress lines identify real Dockerfile steps with a
        // numeric fraction (`[2/4]`). Internal/export steps also use brackets
        // but have no fraction and must not enter layer accounting.
        if let Some((id, detail)) = trimmed.split_once(' ')
            && id.starts_with('#')
            && detail
                .split_once(']')
                .is_some_and(|(header, _)| header.contains('[') && header.contains('/'))
        {
            buildkit_steps.insert(id.to_owned(), ());
        }
        if trimmed.starts_with('#') {
            let id = trimmed.split_whitespace().next().unwrap_or_default();
            if trimmed.contains("CACHED") {
                finish_buildkit_step(&mut buildkit_steps, id, true, &mut hits, &mut misses);
            } else if trimmed.contains("DONE") {
                finish_buildkit_step(&mut buildkit_steps, id, false, &mut hits, &mut misses);
            }
        }

        // Classic-builder output announces a step before printing either
        // `Using cache` or the completed layer. Finalize the previous step
        // only when the next step begins, then finalize the last one at EOF.
        if trimmed.starts_with("Step ") {
            if let Some(cached) = classic_step_cached.replace(false) {
                if cached {
                    hits += 1;
                } else {
                    misses += 1;
                }
            }
        } else if trimmed.contains("Using cache") && classic_step_cached.is_some() {
            classic_step_cached = Some(true);
        }
    }

    if let Some(cached) = classic_step_cached {
        if cached {
            hits += 1;
        } else {
            misses += 1;
        }
    }
    misses += buildkit_steps.len() as u64;
    (hits, misses)
}

/// Small, deterministic build context. It exists so the build scenarios measure
/// the builder rather than the size of some unrelated image.
fn write_build_context(dir: &Path, base_image: &str) -> Result<()> {
    std::fs::write(dir.join("cache-buster"), b"stable")?;
    std::fs::write(
        dir.join("Dockerfile"),
        format!(
            "FROM {base_image}\n\
             COPY cache-buster /velnor-bench/cache-buster\n\
             RUN printf layer-one > /velnor-bench/one\n\
             RUN printf layer-two > /velnor-bench/two\n"
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_docker_scenario_with_a_fallback_has_a_workload() {
        for scenario in crate::scenario::MATRIX {
            if scenario.family == crate::scenario::Family::Docker
                && scenario.fallback == Some(crate::scenario::Driver::DockerDirect)
            {
                assert!(build(scenario).is_ok(), "{} has no workload", scenario.id);
            }
        }
    }

    #[test]
    fn image_pull_builds_a_workload() {
        let scenario = crate::scenario::find("docker/image-pull").expect("scenario");
        assert!(build(scenario).is_ok());
    }

    #[test]
    fn a_scenario_without_a_docker_workload_is_refused_not_faked() {
        let scenario = crate::scenario::find("rust/cold").expect("scenario");
        let error = build(scenario).map(|_| ()).expect_err("must refuse");
        assert!(error.to_string().contains("reported as unrun"), "{error}");
    }

    #[test]
    fn cache_lines_are_counted_from_real_builder_output() {
        let buildkit = "#4 [1/3] FROM docker.io/library/alpine\n\
                        #5 [2/3] COPY cache-buster /velnor-bench/cache-buster\n\
                        #5 CACHED\n\
                        #6 [3/3] RUN printf layer-one > /velnor-bench/one\n";
        assert_eq!(count_layer_cache(buildkit), (1, 2));
        let classic = "Step 1/3 : FROM alpine\nStep 2/3 : COPY x /x\n ---> Using cache\n";
        assert_eq!(count_layer_cache(classic), (1, 1));
        let terminal = "#1 [1/2] FROM alpine\n#1 DONE 0.1s\n#2 [2/2] RUN printf ok\n#2 CACHED\n";
        assert_eq!(count_layer_cache(terminal), (1, 1));
        assert_eq!(count_layer_cache(""), (0, 0));
    }

    #[test]
    fn the_build_context_is_written_deterministically() {
        let dir = std::env::temp_dir().join(format!("velnor-bench-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create");
        write_build_context(&dir, "alpine:3").expect("write");
        let dockerfile = std::fs::read_to_string(dir.join("Dockerfile")).expect("read");
        assert!(dockerfile.starts_with("FROM alpine:3\n"));
        assert!(dockerfile.contains("COPY cache-buster"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_reports_a_duration() {
        let (value, elapsed) = timed(|| 7_u32);
        assert_eq!(value, 7);
        assert!(elapsed < 10_000);
    }

    #[test]
    fn failed_docker_invocations_are_not_accepted_as_measurements() {
        let invocation = Invocation {
            program: "docker".into(),
            args: vec!["exec".into()],
            code: 17,
            stdout: String::new(),
            stderr: "container failed".into(),
            wall: Duration::ZERO,
        };
        let error = require_success(&invocation, "docker exec").expect_err("must fail closed");
        assert!(error.to_string().contains("exit code 17"));
        assert!(error.to_string().contains("container failed"));
    }

    #[test]
    fn build_tags_are_unique_to_the_workload_owner() {
        let first = unique_build_tag();
        let second = unique_build_tag();
        assert_ne!(first, second);
        assert!(first.starts_with("velnor-bench-cached-"));
        assert!(first.ends_with(":latest"));
    }

    #[test]
    fn docker_ids_require_one_hex_token() {
        let valid = "a".repeat(64);
        assert_eq!(
            parse_docker_id(&format!("{valid}\n")).expect("valid ID"),
            valid
        );
        for invalid in ["", "not-an-id", "abc", "a a", "g".repeat(64).as_str()] {
            assert!(parse_docker_id(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn owned_image_inspect_requires_matching_owner_and_id() {
        let id = "d".repeat(64);
        assert_eq!(
            parse_owned_image_inspect(&format!("sha256:{id} owner-token"), "owner-token")
                .expect("valid owned image"),
            format!("sha256:{id}")
        );
        assert!(
            parse_owned_image_inspect(&format!("sha256:{id} other-owner"), "owner-token").is_err()
        );
        assert!(parse_owned_image_inspect("sha256:not-an-id owner-token", "owner-token").is_err());
        assert!(parse_owned_image_inspect(
            &format!("sha256:{id} owner-token trailing"),
            "owner-token"
        )
        .is_err());
    }

    #[test]
    fn image_digest_resolution_prefers_registry_digest_and_falls_back_to_id() {
        let id = "e".repeat(64);
        assert_eq!(
            parse_image_digest_inspect(&format!(
                "docker.io/library/ubuntu@sha256:{id}\tsha256:{id}\n"
            ))
            .expect("registry digest"),
            format!("docker.io/library/ubuntu@sha256:{id}")
        );
        assert_eq!(
            parse_image_digest_inspect(&format!("\tsha256:{id}\n")).expect("image ID"),
            format!("sha256:{id}")
        );
    }

    #[test]
    fn owned_image_identity_rejects_retargeted_tag() {
        let expected = format!("sha256:{}", "f".repeat(64));
        let actual = format!("sha256:{}", "a".repeat(64));
        let error =
            verify_owned_image_identity(&format!("{actual} owner-token"), "owner-token", &expected)
                .expect_err("retargeted tag must not be deleted");
        assert!(error.to_string().contains("identity mismatch"));
    }

    #[test]
    fn resource_recovery_requires_one_exact_name_and_id() {
        let id = "1".repeat(64);
        assert_eq!(
            parse_owned_resource_listing(
                &format!("{id}\texact-name\n"),
                "exact-name",
                ResourceKind::Container,
            )
            .expect("exact resource"),
            id
        );
        assert!(parse_owned_resource_listing(
            &format!("{id}\tother-name\n"),
            "exact-name",
            ResourceKind::Container,
        )
        .is_err());
        assert!(parse_owned_resource_listing(
            &format!("{id}\texact-name\n{id}\texact-name\n"),
            "exact-name",
            ResourceKind::Network,
        )
        .is_err());
    }

    #[test]
    fn resource_recovery_requires_exact_ownership_and_resource_shape() {
        let id = "2".repeat(64);
        let wrong_id = "3".repeat(64);
        let valid_container = format!("{id}\t/name\towner-token\tjob\tsha256:image");
        assert_eq!(
            parse_owned_resource_inspect(
                &valid_container,
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            )
            .expect("owned container"),
            id
        );
        let valid_network = format!("{id}\tnetwork-name\towner-token\tnetwork");
        assert_eq!(
            parse_owned_resource_inspect(
                &valid_network,
                &id,
                "network-name",
                "owner-token",
                "network",
                None,
                ResourceKind::Network,
            )
            .expect("owned network"),
            id
        );

        let cases = [
            (
                "wrong immutable ID",
                format!("{wrong_id}\t/name\towner-token\tjob\tsha256:image"),
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            ),
            (
                "wrong name",
                format!("{id}\t/other-name\towner-token\tjob\tsha256:image"),
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            ),
            (
                "wrong owner",
                format!("{id}\t/name\tother-owner\tjob\tsha256:image"),
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            ),
            (
                "wrong role",
                format!("{id}\t/name\towner-token\tservice\tsha256:image"),
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            ),
            (
                "wrong image",
                format!("{id}\t/name\towner-token\tjob\tsha256:other"),
                &id,
                "name",
                "owner-token",
                "job",
                Some("sha256:image"),
                ResourceKind::Container,
            ),
            (
                "network image expectation",
                valid_network.clone(),
                &id,
                "network-name",
                "owner-token",
                "network",
                Some("sha256:image"),
                ResourceKind::Network,
            ),
            (
                "container shape used as network",
                valid_container.clone(),
                &id,
                "name",
                "owner-token",
                "job",
                None,
                ResourceKind::Network,
            ),
            (
                "network shape used as container",
                valid_network,
                &id,
                "network-name",
                "owner-token",
                "network",
                None,
                ResourceKind::Container,
            ),
        ];
        for (
            case,
            output,
            expected_id,
            expected_name,
            expected_owner,
            expected_role,
            expected_image,
            kind,
        ) in cases
        {
            assert!(
                parse_owned_resource_inspect(
                    &output,
                    expected_id,
                    expected_name,
                    expected_owner,
                    expected_role,
                    expected_image,
                    kind,
                )
                .is_err(),
                "accepted invalid recovery case: {case}"
            );
        }
    }

    #[test]
    fn resource_labels_bind_owner_and_role() {
        let workload = DockerWorkload {
            kind: Kind::JobContainer,
            build_tag: None,
            build_attempted: false,
            owned_images: Vec::new(),
            measured_job_image: None,
            measured_service_image: None,
            scratch: ScratchOwner::new(),
            owned_containers: Vec::new(),
            owned_networks: Vec::new(),
            iteration: 0,
            notes: Vec::new(),
        };
        let labels = workload.resource_labels("job");
        assert_eq!(labels[0], "--label");
        assert!(labels[1].starts_with("com.velnor.bench.owner="));
        assert_eq!(labels[2], "--label");
        assert_eq!(labels[3], "com.velnor.bench.role=job");
    }

    #[test]
    fn successful_resource_outputs_are_recorded_as_immutable_ids() {
        let id = "b".repeat(64);
        let invocation = Invocation {
            program: "docker".into(),
            args: vec!["create".into()],
            code: 0,
            stdout: format!("{id}\n"),
            stderr: String::new(),
            wall: Duration::ZERO,
        };
        let mut workload = DockerWorkload {
            kind: Kind::JobContainer,
            build_tag: None,
            build_attempted: false,
            owned_images: Vec::new(),
            measured_job_image: None,
            measured_service_image: None,
            scratch: ScratchOwner::new(),
            owned_containers: Vec::new(),
            owned_networks: Vec::new(),
            iteration: 0,
            notes: Vec::new(),
        };
        workload.own_container("diagnostic-name", "job");
        assert_eq!(
            workload
                .record_container_id("diagnostic-name", &invocation)
                .expect("record container ID"),
            id
        );
        assert_eq!(
            workload.owned_containers[0].id.as_deref(),
            Some(id.as_str())
        );

        let network_id = "c".repeat(64);
        let network_invocation = Invocation {
            stdout: format!("{network_id}\n"),
            ..invocation
        };
        workload.own_network("network-name", "network");
        assert_eq!(
            workload
                .record_network_id("network-name", &network_invocation)
                .expect("record network ID"),
            network_id
        );
        assert_eq!(
            workload.owned_networks[0].id.as_deref(),
            Some(network_id.as_str())
        );
    }

    #[test]
    fn unresolved_container_cleanup_retains_ownership() {
        let mut workload = DockerWorkload {
            kind: Kind::JobContainer,
            build_tag: None,
            build_attempted: false,
            owned_images: Vec::new(),
            measured_job_image: None,
            measured_service_image: None,
            scratch: ScratchOwner::new(),
            owned_containers: vec![OwnedContainer {
                name: "possible-foreign-name".to_owned(),
                role: "job".to_owned(),
                id: None,
            }],
            owned_networks: Vec::new(),
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: crate::sys::Runner::new(),
        };

        let error = workload
            .cleanup_owned_resources(&mut context)
            .expect_err("unresolved ownership must fail closed");
        assert!(!error.to_string().is_empty());
        assert_eq!(
            context.runner.count_of("docker"),
            1,
            "unresolved ownership may be listed but never name-deleted"
        );
        assert_eq!(workload.owned_containers.len(), 1);
    }

    #[test]
    fn cleanup_error_keeps_the_primary_failure() {
        let error = with_cleanup(
            anyhow::anyhow!("primary workload failure"),
            Err(anyhow::anyhow!("container removal failed")),
        );
        let rendered = format!("{error:#}");
        assert!(rendered.contains("primary workload failure"));
        assert!(rendered.contains("container removal failed"));
    }

    #[test]
    fn scratch_owners_allocate_distinct_roots() {
        let work_root = std::env::temp_dir().join(format!(
            "velnor-bench-docker-scratch-{}",
            std::process::id()
        ));
        let mut first = ScratchOwner::new();
        let mut second = ScratchOwner::new();
        let first_root = first.allocate(&work_root).expect("allocate first root");
        let second_root = second.allocate(&work_root).expect("allocate second root");

        assert_ne!(first_root, second_root);
        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn teardown_removes_owned_scratch_root() {
        let work_root = std::env::temp_dir().join(format!(
            "velnor-bench-docker-teardown-{}",
            std::process::id()
        ));
        let mut scratch = ScratchOwner::new();
        let root = scratch.allocate(&work_root).expect("allocate scratch root");
        std::fs::create_dir_all(root.join("workspace")).expect("create workspace");
        std::fs::create_dir_all(root.join("docker-build-context")).expect("create build context");

        let mut workload = DockerWorkload {
            kind: Kind::JobContainer,
            build_tag: None,
            build_attempted: false,
            owned_images: Vec::new(),
            measured_job_image: None,
            measured_service_image: None,
            scratch,
            owned_containers: Vec::new(),
            owned_networks: Vec::new(),
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root,
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: crate::sys::Runner::new(),
        };

        workload.teardown(&mut context).expect("teardown");
        assert!(workload.scratch.root.is_none());
        assert!(!root.exists());
    }
}

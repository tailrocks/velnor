//! Job-scoped ownership of host Docker objects.
//!
//! Trusted jobs used to receive `/var/run/docker.sock` directly. Guest tools
//! (Testcontainers, compose, `docker run`) then created unlabeled host objects
//! that Velnor teardown never named, so they outlived the job on a persistent
//! host. The lease is the missing ownership boundary: the job talks to a
//! per-job proxy that injects `velnor.job-id` / `velnor.daemon-id` on every
//! create, and every terminal path deletes by that label.
//!
//! In-flight Engine requests (BuildKit `ContainerStart`) hold dockerd's
//! container lock until the HTTP client disconnects. A one-way host→guest
//! copy cannot see job cancel, so `docker rm` of Created BuildKit hung until
//! dockerd itself was killed. The proxy now splices both directions and Drop
//! shuts down every live Engine stream before reclaim.

use crate::docker::client as docker_client;
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
/// Every Docker object created for a job must stay below the package-owned
/// aggregate resource boundary, including containers created through the
/// per-job API proxy (BuildKit and Testcontainers).
pub const JOB_CGROUP_PARENT: &str = "velnor-jobs.slice";
pub const TESTCONTAINERS_LABEL: &str = "org.testcontainers.managed-by=testcontainers";
/// Job containers are owned by their Velnor name and labels. Do not use an
/// untagged `ancestor=` filter here: Docker resolves it as an image reference
/// on every scan and emits a lookup warning when only `:26.04` is tagged.
pub const JOB_CONTAINER_NAME_PREFIX: &str = "velnor-job-";
/// docker-container BuildKit daemon created by `docker buildx create --name velnor-builder-*`.
/// Job-end used a `name=-{scope}0$` filter; Docker's name filter is a match on the
/// container name, and `$` is not an end-anchor on every engine, so Created/removing
/// builders survived cancel/restart. Prefix match plus orphan-job reclaim is the
/// ownership path.
pub const BUILDKIT_CONTAINER_NAME_PREFIX: &str = "buildx_buildkit_velnor-builder-";
/// Host-approved image reference for Velnor's persistent docker-container
/// BuildKit daemon.  The lease also records the immutable local image ID
/// resolved by the host before Buildx is allowed to talk to Docker.
pub(crate) const PERSISTENT_BUILDKIT_IMAGE: &str = "moby/buildkit:buildx-stable-1";
/// The multi-platform manifest currently approved by the runner.  A mutable
/// tag is accepted only after the host image inspect proves this RepoDigest.
pub(crate) const PERSISTENT_BUILDKIT_REPO_DIGEST: &str =
    "moby/buildkit@sha256:cec9f139f45e93c5c69c60f8b07cfad9f43f4ef6b6a6cd917527fea5ff2e3dea";
const UNIX_SOCKET_PATH_LIMIT: usize = 100;

/// The immutable Docker handle retained after a successful create/inspect.
/// Names are discovery keys only; lifecycle mutations use this ID so a
/// same-name replacement cannot be stopped or removed by retry cleanup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DockerObjectIds {
    pub(crate) job_container: Option<String>,
    pub(crate) services: BTreeMap<String, String>,
    pub(crate) network: Option<String>,
}

/// Docker's inspect projection used by runner-owned cleanup preflight. It
/// deliberately excludes `.Config.Env`, which can contain workflow secrets.
pub(crate) const CONTAINER_IDENTITY_FORMAT: &str = r#"{{json .Id}}{{"\t"}}{{json .Name}}{{"\t"}}{{json .Config.Image}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .HostConfig.NetworkMode}}{{"\t"}}{{json .Path}}{{"\t"}}{{json .Args}}{{"\t"}}{{json .State.Status}}"#;
pub(crate) const NETWORK_IDENTITY_FORMAT: &str =
    r#"{{json .Id}}{{"\t"}}{{json .Name}}{{"\t"}}{{json .Driver}}{{"\t"}}{{json .Labels}}"#;
pub(crate) const VOLUME_IDENTITY_FORMAT: &str =
    r#"{{json .Name}}{{"\t"}}{{json .Driver}}{{"\t"}}{{json .Labels}}{{"\t"}}{{json .Options}}"#;

pub(crate) fn inspect_container_identity_args(target: &str) -> Vec<String> {
    vec![
        "inspect".into(),
        "--format".into(),
        CONTAINER_IDENTITY_FORMAT.into(),
        "--".into(),
        target.into(),
    ]
}

pub(crate) fn inspect_network_identity_args(target: &str) -> Vec<String> {
    vec![
        "network".into(),
        "inspect".into(),
        "--format".into(),
        NETWORK_IDENTITY_FORMAT.into(),
        "--".into(),
        target.into(),
    ]
}

pub(crate) fn inspect_volume_identity_args(target: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "inspect".into(),
        "--format".into(),
        VOLUME_IDENTITY_FORMAT.into(),
        "--".into(),
        target.into(),
    ]
}

/// Parse the single object ID printed by `docker create`, `docker run --detach`,
/// or `docker network create`. Empty output is retained as `None` for test
/// doubles and older Docker wrappers; production cleanup then performs the
/// attested name lookup before mutating anything.
pub(crate) fn parse_created_object_id(stdout: &str) -> Option<String> {
    let id = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    validate_owned_resource_id(id, "created Docker object").ok()
}

fn parse_identity_fields<'a>(output: &'a str, expected: usize, kind: &str) -> Result<Vec<&'a str>> {
    let fields = output.trim().split('\t').collect::<Vec<_>>();
    if fields.len() != expected || fields.iter().any(|field| field.trim().is_empty()) {
        bail!("Docker {kind} identity projection is malformed: expected {expected} fields");
    }
    Ok(fields)
}

fn parse_identity_json<T: serde::de::DeserializeOwned>(field: &str, kind: &str) -> Result<T> {
    serde_json::from_str(field).with_context(|| format!("parse Docker {kind} identity field"))
}

fn ids_match(expected: Option<&str>, observed: &str) -> bool {
    expected.is_none_or(|expected| expected == observed)
}

pub(crate) struct ContainerIdentityExpectation<'a> {
    pub(crate) expected_id: Option<&'a str>,
    pub(crate) expected_name: &'a str,
    pub(crate) expected_image: &'a str,
    pub(crate) expected_network: &'a str,
    pub(crate) expected_labels: Option<(&'a str, &'a str)>,
    pub(crate) expected_command: Option<&'a [&'a str]>,
}

/// Verify one runner-owned container's immutable image, labels, network mode,
/// command, and lifecycle projection, returning its full daemon ID.
pub(crate) fn attest_container_identity(
    output: &str,
    expected: &ContainerIdentityExpectation<'_>,
) -> Result<String> {
    attest_container_identity_impl(output, expected)
}

/// Attest a runner-started service container. Services are created by the
/// host runner's Docker CLI and historically do not carry the lease labels;
/// their immutable create ID, image, and network still bind cleanup safely.
pub(crate) fn attest_service_container_identity(
    output: &str,
    expected_id: Option<&str>,
    expected_name: &str,
    expected_image: &str,
    expected_network: &str,
) -> Result<String> {
    let expected = ContainerIdentityExpectation {
        expected_id,
        expected_name,
        expected_image,
        expected_network,
        expected_labels: None,
        expected_command: None,
    };
    attest_container_identity_impl(output, &expected)
}

fn attest_container_identity_impl(
    output: &str,
    expected: &ContainerIdentityExpectation<'_>,
) -> Result<String> {
    let fields = parse_identity_fields(output, 8, "container")?;
    let id: String = parse_identity_json(fields[0], "container id")?;
    let name: String = parse_identity_json(fields[1], "container name")?;
    let image: String = parse_identity_json(fields[2], "container image")?;
    let labels: Option<BTreeMap<String, String>> =
        parse_identity_json(fields[3], "container labels")?;
    let network: String = parse_identity_json(fields[4], "container network")?;
    let path: String = parse_identity_json(fields[5], "container command path")?;
    let args: Vec<String> = parse_identity_json(fields[6], "container command args")?;
    let state: String = parse_identity_json(fields[7], "container state")?;
    let labels = match (expected.expected_labels, labels) {
        (Some(_), None) => bail!("Docker container identity omitted ownership labels"),
        (_, Some(labels)) => labels,
        (None, None) => BTreeMap::new(),
    };
    if id.is_empty() || !ids_match(expected.expected_id, &id) {
        bail!("Docker container identity ID mismatch");
    }
    let observed_name = name.strip_prefix('/').unwrap_or(name.as_str());
    if observed_name != expected.expected_name {
        bail!(
            "Docker container name mismatch: expected {:?}, found {name:?}",
            expected.expected_name
        );
    }
    if image != expected.expected_image {
        bail!(
            "Docker container image mismatch: expected {:?}, found {image:?}",
            expected.expected_image
        );
    }
    if let Some((job_id, daemon_id)) = expected.expected_labels
        && (labels.get(JOB_ID_LABEL).map(String::as_str) != Some(job_id)
            || labels.get(DAEMON_ID_LABEL).map(String::as_str) != Some(daemon_id))
    {
        bail!("Docker container ownership labels do not match the recorded job");
    }
    if network != expected.expected_network {
        bail!(
            "Docker container network mismatch: expected {:?}, found {network:?}",
            expected.expected_network
        );
    }
    if let Some(expected_command) = expected.expected_command {
        let mut observed = Vec::with_capacity(1 + args.len());
        // Docker may render the image's shell as `sh` or its resolved
        // `/bin/sh`; a custom image entrypoint may prefix the requested
        // command, so require the runner supervisor command as the exact
        // final argv suffix while retaining every byte.
        observed.push(path.rsplit('/').next().unwrap_or(path.as_str()));
        observed.extend(args.iter().map(String::as_str));
        if observed != expected_command && !observed.ends_with(expected_command) {
            bail!("Docker container command mismatch");
        }
    }
    if crate::docker::client::ContainerState::parse(&state).is_none() {
        bail!("Docker container identity returned unknown lifecycle state {state:?}");
    }
    Ok(id)
}

/// Verify a runner-owned network's immutable driver and labels, returning its
/// full daemon ID.
pub(crate) fn attest_network_identity(
    output: &str,
    expected_id: Option<&str>,
    expected_name: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<String> {
    let fields = parse_identity_fields(output, 4, "network")?;
    let id: String = parse_identity_json(fields[0], "network id")?;
    let name: String = parse_identity_json(fields[1], "network name")?;
    let driver: String = parse_identity_json(fields[2], "network driver")?;
    let labels: Option<BTreeMap<String, String>> =
        parse_identity_json(fields[3], "network labels")?;
    let labels = labels.context("Docker network identity omitted ownership labels")?;
    if id.is_empty() || !ids_match(expected_id, &id) {
        bail!("Docker network identity ID mismatch for {expected_name}");
    }
    if name != expected_name {
        bail!("Docker network name mismatch: expected {expected_name:?}, found {name:?}");
    }
    if driver != "bridge" {
        bail!("Docker network {expected_name} has unexpected driver {driver:?}");
    }
    if labels.get(JOB_ID_LABEL).map(String::as_str) != Some(job_id)
        || labels.get(DAEMON_ID_LABEL).map(String::as_str) != Some(daemon_id)
    {
        bail!("Docker network {expected_name} ownership labels do not match the recorded job");
    }
    Ok(id)
}

/// Verify a named volume's immutable name, driver, and ownership labels. A
/// Docker volume has no separate daemon ID; its exact name is its identity.
pub(crate) fn attest_volume_identity(
    output: &str,
    expected_name: &str,
    job_id: &str,
    daemon_id: Option<&str>,
) -> Result<String> {
    let identity = parse_volume_identity(output)?;
    attest_volume_identity_fields(&identity, expected_name, job_id, daemon_id)
}

#[derive(Debug, Clone)]
struct VolumeIdentity {
    name: String,
    driver: String,
    labels: BTreeMap<String, String>,
    /// `None` means Docker omitted or returned null for Options. The local
    /// driver uses that representation for its empty default, so attestation
    /// treats it exactly like an empty map and rejects only actual options.
    options: Option<BTreeMap<String, String>>,
}

fn parse_volume_identity(output: &str) -> Result<VolumeIdentity> {
    let fields = output.trim().split('\t').collect::<Vec<_>>();
    if !matches!(fields.len(), 3 | 4) || fields.iter().any(|field| field.trim().is_empty()) {
        bail!("Docker volume identity projection is malformed: expected 3 or 4 fields");
    }
    let name: String = parse_identity_json(fields[0], "volume name")?;
    let driver: String = parse_identity_json(fields[1], "volume driver")?;
    let labels: Option<BTreeMap<String, String>> = parse_identity_json(fields[2], "volume labels")?;
    let options = fields
        .get(3)
        .map(|field| {
            parse_identity_json::<Option<BTreeMap<String, String>>>(field, "volume options")
        })
        .transpose()?
        .flatten();
    Ok(VolumeIdentity {
        name,
        driver,
        labels: labels.context("Docker volume identity omitted ownership labels")?,
        options,
    })
}

fn attest_volume_identity_fields(
    identity: &VolumeIdentity,
    expected_name: &str,
    job_id: &str,
    daemon_id: Option<&str>,
) -> Result<String> {
    if identity.name != expected_name {
        bail!(
            "Docker volume identity name mismatch: expected {expected_name:?}, found {:?}",
            identity.name
        );
    }
    if identity.driver != "local" {
        bail!(
            "Docker volume {expected_name} has unexpected driver {:?}",
            identity.driver
        );
    }
    if identity
        .options
        .as_ref()
        .is_some_and(|options| !options.is_empty())
    {
        bail!("Docker volume {expected_name} has unsafe local driver options");
    }
    if identity.labels.get(JOB_ID_LABEL).map(String::as_str) != Some(job_id) {
        bail!("Docker volume {expected_name} job ownership label mismatch");
    }
    if let Some(daemon_id) = daemon_id
        && identity.labels.get(DAEMON_ID_LABEL).map(String::as_str) != Some(daemon_id)
    {
        bail!("Docker volume {expected_name} daemon ownership label mismatch");
    }
    Ok(identity.name.clone())
}

/// Return the BuildKit builder scope encoded by a state volume name. The
/// suffix is a structural check only; ownership comes from the inspected
/// `velnor.job-id` label, never from a name-derived default job ID.
fn buildkit_volume_scope(name: &str) -> Option<&str> {
    if crate::buildkit::is_persistent_builder_object(name) {
        return None;
    }
    let scope = name
        .strip_prefix(BUILDKIT_CONTAINER_NAME_PREFIX)?
        .strip_suffix("_state")?
        .strip_suffix('0')?;
    if scope.is_empty() || matches!(scope, "." | "..") {
        return None;
    }
    scope
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        .then_some(scope)
}

/// Attest a generic orphan BuildKit volume. The job ID is taken from the
/// current inspect labels and validated again by callers immediately before
/// removal. This accepts legacy custom builder names as well as the default
/// name while excluding persistent shared state.
fn attest_orphan_buildkit_volume(
    output: &str,
    expected_name: &str,
    protected_jobs: &BTreeSet<String>,
    daemon_id: Option<&str>,
) -> Result<Option<String>> {
    let identity = parse_volume_identity(output)?;
    let Some(_scope) = buildkit_volume_scope(expected_name) else {
        bail!("Docker volume {expected_name} is not a removable legacy BuildKit state volume");
    };
    if identity.name != expected_name {
        bail!(
            "Docker volume identity name mismatch: expected {expected_name:?}, found {:?}",
            identity.name
        );
    }
    if identity.driver != "local" {
        bail!(
            "Docker volume {expected_name} has unexpected driver {:?}",
            identity.driver
        );
    }
    let Some(job_id) = identity
        .labels
        .get(JOB_ID_LABEL)
        .filter(|id| !id.is_empty())
    else {
        bail!("Docker volume {expected_name} omitted the Velnor job ownership label");
    };
    if protected_jobs.contains(job_id) {
        return Ok(None);
    }
    if let Some(daemon_id) = daemon_id
        && identity.labels.get(DAEMON_ID_LABEL).map(String::as_str) != Some(daemon_id)
    {
        bail!("Docker volume {expected_name} daemon ownership label mismatch");
    }
    Ok(Some(job_id.clone()))
}

const MAX_PROXY_BODY: usize = 32 * 1024 * 1024;
const MAX_PROXY_HEADER: usize = 64 * 1024;
const MAX_PROXY_LINE: usize = 8 * 1024;
const MAX_LEASE_CONNECTIONS: usize = 64;
const MAX_LEASE_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
const PROXY_COPY_BUFFER: usize = 64 * 1024;
const PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const PROXY_MAX_UPGRADE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const MAX_OWNED_DOCKER_RESOURCES: usize = 1024;
const MAX_OWNED_DOCKER_RESOURCE_ID: usize = 256;
const MAX_CREATE_RESPONSE_BODY: usize = 64 * 1024;
const VOLUME_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const VOLUME_LOCK_RETRY: Duration = Duration::from_millis(10);

/// The proxy is a capability boundary, not a transparent Docker socket.
/// Resource identifiers are added only after a successful create response and
/// are shared by all connections belonging to this one job lease.
#[derive(Clone)]
struct DockerLeasePolicy {
    resources: Arc<Mutex<OwnedDockerResources>>,
    volume_lock_root: Option<Arc<PathBuf>>,
}

#[derive(Default)]
struct OwnedDockerResources {
    containers: BTreeSet<String>,
    /// Names this job successfully created containers with (Docker's
    /// `?name=` create query), mapped to their immutable IDs. Docker clients
    /// routinely address containers by name (`buildx_buildkit_<builder><node>`)
    /// and by ID in the same session. Retaining the ID prevents a deleted
    /// name from authorizing a same-name replacement.
    container_names: BTreeMap<String, String>,
    networks: BTreeSet<String>,
    volumes: BTreeSet<String>,
    /// Shared BuildKit daemon names mapped to their attested immutable IDs.
    /// A reused builder is usable by this job but is not deletable by it.
    persistent_containers: BTreeMap<String, String>,
    /// IDs returned by an exact persistent-builder create while the host-side
    /// inspect is still running.  Candidates are already denied generic
    /// mutations; they become usable only after full attestation succeeds.
    persistent_container_candidates: BTreeSet<String>,
    /// State volume named by each attested persistent container. Recording the
    /// association does not attest the volume; a start must inspect it first.
    persistent_container_volumes: BTreeMap<String, String>,
    /// BuildKit state volumes accepted after a strict inspect. The structural
    /// name check is always coupled to a host-registered current-job builder.
    persistent_volumes: BTreeSet<String>,
    /// Exact persistent builders registered by the host runner for this job.
    /// Workflow-writable files and broad namespace prefixes are never used as
    /// a Docker capability allowlist.
    persistent_builders: BTreeSet<String>,
    /// Builders whose image and state volume are being host-attested. These
    /// entries are deliberately absent from the guest authorization set until
    /// setup completes successfully.
    persistent_builder_setups: BTreeSet<String>,
    /// Immutable local image ID approved by the host for each builder.  A
    /// Config.Image string is a mutable tag; container inspect must match this
    /// ID before the shared daemon is exposed to Buildx.
    persistent_builder_images: BTreeMap<String, String>,
    /// Per-name operation locks. Docker volumes have no immutable ID, so a
    /// name must stay bound from re-attestation through the forwarded
    /// operation and its response observer.
    volume_locks: BTreeMap<String, Arc<VolumeNameLock>>,
    execs: BTreeSet<String>,
    /// Exec IDs created through the attested persistent BuildKit path. The
    /// ID is the immutable binding used by the later hijack/start request.
    persistent_execs: BTreeSet<String>,
    /// Persistent exec ownership follows its attested builder so revoking one
    /// setup cannot leave a stale exec capability behind.
    persistent_exec_builders: BTreeMap<String, String>,
}

#[derive(Debug)]
struct VolumeNameLock {
    held: Mutex<bool>,
    changed: Condvar,
}

impl Default for VolumeNameLock {
    fn default() -> Self {
        Self {
            held: Mutex::new(false),
            changed: Condvar::new(),
        }
    }
}

#[derive(Debug)]
struct VolumeNameLockGuard {
    lock: Arc<VolumeNameLock>,
    file: Option<File>,
}

impl Drop for VolumeNameLockGuard {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Ok(mut held) = self.lock.held.lock() {
            *held = false;
            self.lock.changed.notify_one();
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct VolumeOperationLocks {
    _guards: Vec<VolumeNameLockGuard>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockerResourceKind {
    Container,
    Network,
    Volume,
    Exec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorizedDockerRoute {
    DaemonRead,
    Create(DockerResourceKind),
    Owned(DockerResourceKind),
    /// First inspect of a persistent BuildKit object. The response must
    /// attest its immutable identity before later lifecycle calls can use it.
    PersistentInspect(DockerResourceKind),
    /// Buildx needs ImageInspect while creating a docker-container builder,
    /// but the raw response contains image config/env data.
    PersistentImageInspect,
    /// Buildx pulls its configured image before creating a fresh daemon. The
    /// exact mutable Buildx reference is rewritten to the host-approved
    /// immutable digest before it reaches Docker.
    PersistentImagePull,
    /// Buildx copies its reviewed buildkitd.toml into an attested bootstrap
    /// container before starting it. The archive shape is validated below.
    PersistentArchive,
    /// An already attested shared BuildKit object. Shared builders have no
    /// DELETE capability through this route.
    Persistent(DockerResourceKind),
    /// Start a just-created exact BuildKit daemon after its host identity has
    /// been checked; this bootstrap route has no shared-container mutators.
    PersistentBootstrap,
    /// Buildx's `buildctl dial-stdio` exec path on an attested shared
    /// BuildKit container.
    PersistentExecCreate,
    PersistentExec,
    Hijack(DockerResourceKind),
    /// Daemon-scoped BuildKit tunnel (`/grpc`, `/session`). No object is
    /// addressed, so there is nothing to label or reclaim; dockerd's own
    /// BuildKit state for these streams is build-scoped and short-lived.
    DaemonTunnel,
}

/// An authorization denial that must reach the guest as a Docker-shaped HTTP
/// response instead of a torn-down connection. Docker clients branch on
/// status: buildx treats a 404 container inspect as "builder absent" and
/// falls back to the daemon's builtin BuildKit, while a transport EOF is a
/// hard failure for every driver.
#[derive(Debug)]
struct LeaseDeny {
    status: u16,
    message: String,
}

impl LeaseDeny {
    fn forbidden(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            status: 403,
            message: message.into(),
        })
    }

    fn not_found(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            status: 404,
            message: message.into(),
        })
    }
}

impl fmt::Display for LeaseDeny {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LeaseDeny {}

impl DockerLeasePolicy {
    #[cfg(test)]
    fn new(job_container: &str) -> Result<Self> {
        Self::new_with_volume_lock_root(job_container, None)
    }

    fn new_with_volume_lock_root(
        job_container: &str,
        volume_lock_root: Option<PathBuf>,
    ) -> Result<Self> {
        let job_container = validate_owned_resource_id(job_container, "job container")?;
        let mut containers = BTreeSet::new();
        containers.insert(job_container);
        Ok(Self {
            resources: Arc::new(Mutex::new(OwnedDockerResources {
                containers,
                container_names: BTreeMap::new(),
                networks: BTreeSet::new(),
                volumes: BTreeSet::new(),
                persistent_containers: BTreeMap::new(),
                persistent_container_candidates: BTreeSet::new(),
                persistent_container_volumes: BTreeMap::new(),
                persistent_volumes: BTreeSet::new(),
                persistent_builders: BTreeSet::new(),
                persistent_builder_setups: BTreeSet::new(),
                persistent_builder_images: BTreeMap::new(),
                volume_locks: BTreeMap::new(),
                execs: BTreeSet::new(),
                persistent_execs: BTreeSet::new(),
                persistent_exec_builders: BTreeMap::new(),
            })),
            volume_lock_root: volume_lock_root.map(Arc::new),
        })
    }

    /// Test helper for registering an already prepared builder directly.
    #[cfg(test)]
    fn allow_persistent_builder(&self, builder: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        if !crate::buildkit::is_persistent_builder_name(&builder) {
            bail!("persistent BuildKit builder is outside the Velnor namespace");
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.persistent_builders.insert(builder);
        Ok(())
    }

    /// Start host-side persistent-builder setup without exposing any guest
    /// route. Image and volume registration accepts this pending state so the
    /// final capability grant can happen only after both attestations pass.
    fn begin_persistent_builder_setup(&self, builder: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        if !crate::buildkit::is_persistent_builder_name(&builder) {
            bail!("persistent BuildKit builder is outside the Velnor namespace");
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.persistent_builders.remove(&builder);
        resources.persistent_builder_images.remove(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        resources.persistent_volumes.remove(&volume);
        resources
            .persistent_container_volumes
            .retain(|_, existing_volume| existing_volume != &volume);
        let removed_ids = resources
            .persistent_containers
            .iter()
            .filter(|(name, _)| persistent_buildkit_builder_name(name) == Some(builder.as_str()))
            .map(|(_, id)| id.clone())
            .collect::<BTreeSet<_>>();
        resources.persistent_containers.retain(|name, id| {
            persistent_buildkit_builder_name(name) != Some(builder.as_str())
                && !removed_ids.contains(id)
        });
        for id in &removed_ids {
            resources.persistent_container_candidates.remove(id);
        }
        resources
            .persistent_exec_builders
            .retain(|_, owner| owner != &builder);
        let retained_execs = resources
            .persistent_exec_builders
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        resources
            .persistent_execs
            .retain(|id| retained_execs.contains(id));
        resources.persistent_builder_setups.insert(builder);
        Ok(())
    }

    /// Complete host-side setup and expose the exact builder capability to
    /// the guest. Both the pinned image and state volume must already be
    /// registered by strict host inspection.
    fn complete_persistent_builder_setup(&self, builder: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !resources.persistent_builder_setups.contains(&builder) {
            bail!("persistent BuildKit builder setup was not started");
        }
        if !resources.persistent_builder_images.contains_key(&builder) {
            bail!("persistent BuildKit image was not host-attested");
        }
        if !resources.persistent_volumes.contains(&volume) {
            bail!("persistent BuildKit state volume was not host-attested");
        }
        resources.persistent_builder_setups.remove(&builder);
        resources.persistent_builders.insert(builder);
        Ok(())
    }

    fn register_persistent_builder_image(&self, builder: &str, image_id: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        let image_id = validate_owned_resource_id(image_id, "persistent BuildKit image ID")?;
        if !crate::buildkit::is_persistent_builder_name(&builder) {
            bail!("persistent BuildKit builder is outside the Velnor namespace");
        }
        if !image_id.starts_with("sha256:") {
            bail!("persistent BuildKit image ID is not an immutable digest");
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !resources.persistent_builders.contains(&builder)
            && !resources.persistent_builder_setups.contains(&builder)
        {
            bail!("persistent BuildKit image registered before its builder setup");
        }
        resources
            .persistent_builder_images
            .insert(builder, image_id);
        Ok(())
    }

    fn revoke_persistent_builder(&self, builder: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let volume = crate::buildkit::daemon_state_volume(&builder);
        resources.persistent_builders.remove(&builder);
        resources.persistent_builder_setups.remove(&builder);
        resources.persistent_builder_images.remove(&builder);
        resources.persistent_volumes.remove(&volume);
        let removed_ids = resources
            .persistent_containers
            .iter()
            .filter(|(name, _)| persistent_buildkit_builder_name(name) == Some(builder.as_str()))
            .map(|(_, id)| id.clone())
            .collect::<BTreeSet<_>>();
        resources.persistent_containers.retain(|name, id| {
            persistent_buildkit_builder_name(name) != Some(builder.as_str())
                && !removed_ids.contains(id)
        });
        for id in &removed_ids {
            resources.persistent_container_candidates.remove(id);
        }
        resources
            .persistent_container_volumes
            .retain(|_, state_volume| state_volume != &volume);
        let removed_execs = resources
            .persistent_exec_builders
            .iter()
            .filter(|(_, owner)| owner.as_str() == builder)
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        for id in removed_execs {
            resources.persistent_exec_builders.remove(&id);
            resources.persistent_execs.remove(&id);
            resources.execs.remove(&id);
        }
        Ok(())
    }

    fn persistent_builder_image(&self, builder: &str) -> Result<String> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources
            .persistent_builder_images
            .get(builder)
            .cloned()
            .context("persistent BuildKit image was not host-approved")
    }

    fn persistent_builder_images(&self) -> Result<BTreeMap<String, String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources.persistent_builder_images.clone())
    }

    fn persistent_builder_names(&self) -> Result<BTreeSet<String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources.persistent_builders.clone())
    }

    fn persistent_builder_names_for_attestation(&self) -> Result<BTreeSet<String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources
            .persistent_builders
            .union(&resources.persistent_builder_setups)
            .cloned()
            .collect())
    }

    fn is_allowed_persistent_buildkit_container_name(&self, container: &str) -> Result<bool> {
        let Some(builder) = persistent_buildkit_builder_name(container) else {
            return Ok(false);
        };
        Ok(self.persistent_builder_names()?.contains(builder))
    }

    fn is_allowed_persistent_buildkit_volume_name(&self, volume: &str) -> Result<bool> {
        let Some(builder) = persistent_buildkit_volume_builder_name(volume) else {
            return Ok(false);
        };
        Ok(self.persistent_builder_names()?.contains(builder))
    }

    fn is_persistent_container_target(&self, target: &str) -> Result<bool> {
        Ok(self.is_allowed_persistent_buildkit_container_name(target)?
            || self.is_attested_persistent_container(target)?
            || self.is_persistent_container_candidate(target)?)
    }

    fn is_persistent_container_candidate(&self, target: &str) -> Result<bool> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources.persistent_container_candidates.contains(&target)
            || resources
                .persistent_containers
                .values()
                .any(|id| id == &target))
    }

    fn is_persistent_exec(&self, target: &str) -> Result<bool> {
        let target = validate_owned_resource_id(target, "Docker exec")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources.persistent_execs.contains(&target))
    }

    fn validate_persistent_exec_create_request(&self, request: &[u8]) -> Result<()> {
        let body = docker_request_body(request)?;
        let value = parse_create_value(body).context("parse persistent BuildKit exec request")?;
        validate_persistent_exec_create_value(&value)
    }

    fn authorize(&self, request: &[u8]) -> Result<AuthorizedDockerRoute> {
        let (method, target) = docker_request_line(request)?;
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let method = method.to_ascii_uppercase();
        let upgrade = docker_upgrade_state(request)?;

        if matches!(segments.as_slice(), ["_ping"] | ["version"] | ["info"])
            && matches!(method.as_str(), "GET" | "HEAD")
        {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        // BuildKit's daemon tunnels: the docker driver solves builds over
        // `/grpc`, and the buildkit session (local context upload) attaches
        // over `/session`. Without them every `docker buildx build` on the
        // default builder dies at the tunnel, however permissive reads are.
        if matches!(segments.as_slice(), ["grpc"] | ["session"]) && method == "POST" {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonTunnel, upgrade);
        }
        if segments.as_slice() == ["build"] && method == "POST" {
            validate_build_request_target(target).map_err(create_capability_deny)?;
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["images", "create"] && method == "POST" {
            validate_persistent_image_pull_request(target).map_err(create_capability_deny)?;
            if self.persistent_builder_names()?.is_empty() {
                return Err(LeaseDeny::forbidden(
                    "Docker lease denies BuildKit image pulls without a claimed persistent builder",
                ));
            }
            return authorize_docker_route(AuthorizedDockerRoute::PersistentImagePull, upgrade);
        }
        // Image names may contain slashes (`moby/buildkit:tag`). Match
        // `/images/<ref>/json` by first/last segment, not a fixed length.
        if segments.first() == Some(&"images")
            && segments.last() == Some(&"json")
            && segments.len() >= 3
            && matches!(method.as_str(), "GET" | "HEAD")
        {
            if self.persistent_builder_names()?.is_empty() {
                return Err(LeaseDeny::forbidden(
                    "Docker lease denies BuildKit image inspect without a claimed builder",
                ));
            }
            let image = segments[1..segments.len() - 1].join("/");
            if !is_approved_persistent_image_reference(&image) {
                return Err(LeaseDeny::not_found(
                    "Docker lease denies inspect of an unapproved image",
                ));
            }
            return authorize_docker_route(AuthorizedDockerRoute::PersistentImageInspect, upgrade);
        }
        if segments.as_slice() == ["containers", "create"] && method == "POST" {
            let create_name =
                containers_create_query_name(request).map_err(create_capability_deny)?;
            if let Some(name) = create_name.as_deref()
                && self.is_allowed_persistent_buildkit_container_name(name)?
            {
                self.validate_persistent_container_create_request(request, name)
                    .map_err(create_capability_deny)?;
                return authorize_docker_route(AuthorizedDockerRoute::PersistentBootstrap, upgrade);
            }
            self.validate_container_create_request(request)
                .map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Container),
                upgrade,
            );
        }
        if segments.as_slice() == ["networks", "create"] && method == "POST" {
            validate_network_create_request(request).map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Network),
                upgrade,
            );
        }
        if segments.as_slice() == ["volumes", "create"] && method == "POST" {
            self.validate_volume_create_request(request)
                .map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Volume),
                upgrade,
            );
        }

        match segments.as_slice() {
            ["containers", id] => {
                if self.is_persistent_container_target(id)? {
                    return Err(LeaseDeny::forbidden(
                        "Docker lease denies direct mutation of a shared BuildKit container",
                    ));
                }
                self.require_owned(DockerResourceKind::Container, id)?;
                if method == "DELETE" {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                    );
                }
                if matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                    );
                }
            }
            ["containers", id, operation] => {
                if operation == &"json"
                    && method == "GET"
                    && (self.is_allowed_persistent_buildkit_container_name(id)?
                        || self.is_attested_persistent_container(id)?)
                {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container),
                        upgrade,
                    );
                }
                if self.is_attested_persistent_container(id)?
                    && matches!(
                        (method.as_str(), *operation),
                        ("GET", "json") | ("POST", "start")
                    )
                {
                    if method == "POST"
                        && operation == &"start"
                        && self.persistent_container_volume(id).is_err()
                    {
                        return Err(LeaseDeny::not_found(
                            "Docker lease persistent container has no attested state volume",
                        ));
                    }
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Persistent(DockerResourceKind::Container),
                        upgrade,
                    );
                }
                if operation == &"archive"
                    && method == "PUT"
                    && self.is_attested_persistent_container(id)?
                {
                    validate_persistent_archive_request(request, target)
                        .map_err(create_capability_deny)?;
                    return authorize_docker_route(
                        AuthorizedDockerRoute::PersistentArchive,
                        upgrade,
                    );
                }
                if operation == &"exec" && method == "POST" {
                    if self.is_attested_persistent_container(id)? {
                        self.validate_persistent_exec_create_request(request)?;
                        return authorize_docker_route(
                            AuthorizedDockerRoute::PersistentExecCreate,
                            upgrade,
                        );
                    }
                    if self.is_persistent_container_target(id)? {
                        return Err(LeaseDeny::forbidden(
                            "Docker lease denies exec on an unattested shared BuildKit container",
                        ));
                    }
                }
                if self.is_persistent_container_target(id)? {
                    return Err(LeaseDeny::forbidden(
                        "Docker lease denies unsupported operation on a shared BuildKit container",
                    ));
                }
                self.require_owned(DockerResourceKind::Container, id)?;
                if operation == &"exec" && method == "POST" {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Create(DockerResourceKind::Exec),
                        upgrade,
                    );
                }
                if matches!(
                    (method.as_str(), *operation),
                    (
                        "GET",
                        "archive" | "changes" | "json" | "logs" | "stats" | "top" | "wait"
                    ) | (
                        "POST",
                        "attach"
                            | "kill"
                            | "pause"
                            | "restart"
                            | "resize"
                            | "start"
                            | "stop"
                            | "unpause"
                            | "wait"
                    )
                ) {
                    let route = if operation == &"attach" {
                        AuthorizedDockerRoute::Hijack(DockerResourceKind::Container)
                    } else {
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container)
                    };
                    return authorize_docker_route(route, upgrade);
                }
            }
            ["exec", id, operation] => {
                if self.is_persistent_exec(id)? {
                    if operation == &"start" && method == "POST" {
                        if !upgrade {
                            return Err(LeaseDeny::forbidden(
                                "Docker lease requires an upgrade for persistent BuildKit exec",
                            ));
                        }
                        return authorize_docker_route(
                            AuthorizedDockerRoute::PersistentExec,
                            upgrade,
                        );
                    }
                    return Err(LeaseDeny::forbidden(
                        "Docker lease denies unsupported operation on a persistent BuildKit exec",
                    ));
                }
                self.require_owned(DockerResourceKind::Exec, id)?;
                if operation == &"start" && method == "POST" {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Hijack(DockerResourceKind::Exec),
                        upgrade,
                    );
                }
                if operation == &"json" && matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Exec),
                        upgrade,
                    );
                }
            }
            ["networks", id] => {
                self.require_owned(DockerResourceKind::Network, id)?;
                if method == "DELETE" || matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Network),
                        upgrade,
                    );
                }
            }
            ["networks", id, operation] => {
                self.require_owned(DockerResourceKind::Network, id)?;
                if matches!(*operation, "connect" | "disconnect") && method == "POST" {
                    self.require_owned_container_in_body(request)?;
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Network),
                        upgrade,
                    );
                }
            }
            ["volumes", id] => {
                if self.is_allowed_persistent_buildkit_volume_name(id)? {
                    return Err(LeaseDeny::forbidden(
                        "Docker lease denies guest access to a host-managed BuildKit state volume",
                    ));
                }
                self.require_owned(DockerResourceKind::Volume, id)?;
                if method == "DELETE" || matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Volume),
                        upgrade,
                    );
                }
            }
            _ => {}
        }

        Err(LeaseDeny::forbidden(format!(
            "Docker lease denied {method} {path}: route is not an owned capability"
        )))
    }

    fn require_owned(&self, kind: DockerResourceKind, id: &str) -> Result<()> {
        let id = validate_owned_resource_id(id, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let owned = match kind {
            // Buildx addresses builder containers by NAME for bootstrap
            // probes and by ID for lifecycle calls, both within one job; the
            // registry learns the name when the job creates the container.
            DockerResourceKind::Container => {
                resources.containers.contains(&id)
                    || resources
                        .container_names
                        .get(&id)
                        .is_some_and(|owner_id| resources.containers.contains(owner_id))
            }
            DockerResourceKind::Network => resources.networks.contains(&id),
            DockerResourceKind::Volume => resources.volumes.contains(&id),
            DockerResourceKind::Exec => resources.execs.contains(&id),
        };
        if owned {
            Ok(())
        } else {
            // A foreign object is invisible to this job, not forbidden:
            // answer docker's own "no such object" status so clients take
            // their absent-object branches instead of dying on a transport
            // error.
            Err(LeaseDeny::not_found(format!(
                "Docker lease denied foreign {kind:?} resource {id:?}"
            )))
        }
    }

    fn require_owned_container_in_body(&self, request: &[u8]) -> Result<()> {
        let body = docker_request_body(request)?;
        let value = parse_create_value(body).context("parse Docker network request")?;
        let id = value
            .get("Container")
            .and_then(Value::as_str)
            .context("Docker network request must name a container")?;
        self.require_owned(DockerResourceKind::Container, id)
    }

    fn volume_names(&self) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok((
            resources.volumes.clone(),
            resources.persistent_volumes.clone(),
        ))
    }

    /// Acquire all named-volume locks in lexical order. The guards are held
    /// by the request loop until Docker replies and ownership observation has
    /// completed, closing the inspect→operation replacement window.
    fn lock_volume_names(&self, names: &BTreeSet<String>) -> Result<VolumeOperationLocks> {
        if names.is_empty() {
            return Ok(VolumeOperationLocks::default());
        }
        let locks = {
            let mut resources = self
                .resources
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            names
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        resources
                            .volume_locks
                            .entry(name.clone())
                            .or_insert_with(|| Arc::new(VolumeNameLock::default()))
                            .clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut guards = Vec::with_capacity(locks.len());
        for (name, lock) in locks {
            let mut held = lock
                .held
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease volume lock is poisoned"))?;
            let deadline = Instant::now() + VOLUME_LOCK_TIMEOUT;
            while *held {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(anyhow::anyhow!(
                        "timed out acquiring in-process Docker volume lock {name:?}"
                    ));
                }
                let (next, timeout) = lock
                    .changed
                    .wait_timeout(held, remaining)
                    .map_err(|_| anyhow::anyhow!("Docker lease volume lock is poisoned"))?;
                held = next;
                if timeout.timed_out() && *held {
                    return Err(anyhow::anyhow!(
                        "timed out acquiring in-process Docker volume lock {name:?}"
                    ));
                }
            }
            *held = true;
            drop(held);
            let mut guard = VolumeNameLockGuard { lock, file: None };
            let file = self
                .volume_lock_root
                .as_deref()
                .map(|root| acquire_volume_file_lock(root, &name))
                .transpose();
            match file {
                Ok(file) => {
                    guard.file = file;
                    guards.push(guard);
                }
                Err(error) => {
                    drop(guard);
                    return Err(error);
                }
            }
        }
        Ok(VolumeOperationLocks { _guards: guards })
    }

    fn validate_container_create_request(&self, request: &[u8]) -> Result<()> {
        let (owned_volume_names, _) = self.volume_names()?;
        validate_container_create_request_with_volumes(request, &owned_volume_names)
    }

    fn validate_persistent_container_create_request(
        &self,
        request: &[u8],
        container: &str,
    ) -> Result<()> {
        let builder = persistent_buildkit_builder_name(container)
            .context("persistent BuildKit container name has no builder")?;
        let expected_image = self.persistent_builder_image(builder)?;
        let volume = crate::buildkit::daemon_state_volume(builder);
        let (_, persistent_volumes) = self.volume_names()?;
        if !persistent_volumes.contains(&volume) {
            bail!("persistent BuildKit state volume was not host-attested");
        }
        let body = docker_request_body(request)?;
        let value = parse_create_value(body)
            .context("parse persistent BuildKit container create request")?;
        validate_persistent_buildkit_container_value(&value, container, &volume, &expected_image)
    }

    fn validate_volume_create_request(&self, request: &[u8]) -> Result<()> {
        let body = docker_request_body(request)?;
        let value = parse_create_value(body).context("parse Docker volume create request")?;
        reject_unsafe_volume_create_value(&value)?;
        if let Some(name) = volume_create_request_name(&value)?
            && is_persistent_buildkit_volume_name(&name)
        {
            bail!("Docker persistent BuildKit state volumes are host-managed");
        }
        Ok(())
    }

    #[cfg(test)]
    fn rewrite_docker_api_request(
        &self,
        request: &[u8],
        job_id: &str,
        daemon_id: &str,
    ) -> Result<Vec<u8>> {
        let (owned_volume_names, _) = self.volume_names()?;
        rewrite_docker_api_request_with_volumes(
            request,
            job_id,
            daemon_id,
            &owned_volume_names,
            false,
            None,
        )
    }

    fn rewrite_docker_api_request_for_route(
        &self,
        request: &[u8],
        job_id: &str,
        daemon_id: &str,
        authorization: AuthorizedDockerRoute,
    ) -> Result<Vec<u8>> {
        let (owned_volume_names, _) = self.volume_names()?;
        if matches!(authorization, AuthorizedDockerRoute::PersistentImagePull) {
            return rewrite_persistent_image_pull_target(request);
        }
        let persistent_image_id =
            if matches!(authorization, AuthorizedDockerRoute::PersistentBootstrap) {
                let name = containers_create_query_name(request)?
                    .context("persistent BuildKit bootstrap omitted its container name")?;
                let builder = persistent_buildkit_builder_name(&name)
                    .context("persistent BuildKit bootstrap name has no builder")?;
                Some(self.persistent_builder_image(builder)?)
            } else {
                None
            };
        rewrite_docker_api_request_with_volumes(
            request,
            job_id,
            daemon_id,
            &owned_volume_names,
            matches!(authorization, AuthorizedDockerRoute::PersistentBootstrap),
            persistent_image_id.as_deref(),
        )
    }

    /// Replace a container alias with the immutable ID captured from its
    /// create/inspect response before forwarding an already-authorized route.
    /// If a concurrent delete revoked the alias after authorization, fail
    /// closed instead of forwarding the stale name to Docker, where it could
    /// resolve to a same-name replacement.
    fn rewrite_authorized_alias_target(
        &self,
        request: &[u8],
        authorization: AuthorizedDockerRoute,
    ) -> Result<Vec<u8>> {
        let container_route = matches!(
            authorization,
            AuthorizedDockerRoute::Owned(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Hijack(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Create(DockerResourceKind::Exec)
                | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::PersistentArchive
        );
        if !container_route {
            return Ok(request.to_vec());
        }
        let (_, target) = docker_request_line(request)?;
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let Some(["containers", target_id, ..]) = segments.get(..) else {
            return Ok(request.to_vec());
        };
        let target_id = *target_id;
        let replacement = {
            let resources = self
                .resources
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            match authorization {
                AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::PersistentArchive => {
                    if let Some(id) = resources.persistent_containers.get(target_id) {
                        Some(id.clone())
                    } else if resources
                        .persistent_containers
                        .values()
                        .any(|id| id == target_id)
                    {
                        None
                    } else {
                        return Err(LeaseDeny::not_found(format!(
                            "Docker lease persistent container alias {target_id:?} was revoked"
                        )));
                    }
                }
                _ => {
                    if let Some(id) = resources.container_names.get(target_id) {
                        Some(id.clone())
                    } else if resources.containers.contains(target_id) {
                        None
                    } else {
                        return Err(LeaseDeny::not_found(format!(
                            "Docker lease container alias {target_id:?} was revoked"
                        )));
                    }
                }
            }
        };
        let Some(replacement) = replacement else {
            return Ok(request.to_vec());
        };
        let mut rewritten_segments = path.split('/').collect::<Vec<_>>();
        let container_index = rewritten_segments
            .iter()
            .position(|segment| *segment == "containers")
            .context("authorized Docker container route omitted containers segment")?;
        let id_index = container_index
            .checked_add(1)
            .context("authorized Docker container route overflowed")?;
        if rewritten_segments.get(id_index).copied() != Some(target_id) {
            bail!("authorized Docker container target changed while rewriting");
        }
        rewritten_segments[id_index] = replacement.as_str();
        let rewritten_path = rewritten_segments.join("/");
        let query = target.split_once('?').map_or("", |(_, query)| query);
        let rewritten_target = if query.is_empty() {
            rewritten_path
        } else {
            format!("{rewritten_path}?{query}")
        };
        rewrite_http_request_target(request, &rewritten_target)
    }

    fn is_attested_persistent_container(&self, target: &str) -> Result<bool> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources.persistent_containers.contains_key(&target)
            || resources
                .persistent_containers
                .values()
                .any(|id| id == &target))
    }

    fn persistent_container_volume(&self, target: &str) -> Result<String> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let name = resources
            .persistent_containers
            .iter()
            .find_map(|(name, id)| (name == &target || id == &target).then_some(name))
            .context("persistent BuildKit container has no attested state-volume association")?;
        resources
            .persistent_container_volumes
            .get(name)
            .cloned()
            .context("persistent BuildKit container state volume was not recorded")
    }

    fn persistent_container_builder(&self, target: &str) -> Result<String> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let name = resources
            .persistent_containers
            .iter()
            .find_map(|(name, id)| (name == &target || id == &target).then_some(name))
            .context("persistent BuildKit container has no attested builder association")?;
        persistent_buildkit_builder_name(name)
            .map(str::to_owned)
            .context("persistent BuildKit container name has no builder")
    }

    fn forget_persistent_container(&self, target: &str) -> Result<()> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let removed_names = resources
            .persistent_containers
            .iter()
            .filter(|(name, id)| *name == &target || *id == &target)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let removed_ids = resources
            .persistent_containers
            .iter()
            .filter(|(name, id)| *name == &target || *id == &target)
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        resources
            .persistent_containers
            .retain(|name, id| name != &target && id != &target);
        resources.persistent_container_candidates.remove(&target);
        for id in removed_ids {
            resources.persistent_container_candidates.remove(&id);
        }
        for name in removed_names {
            resources.persistent_container_volumes.remove(&name);
        }
        Ok(())
    }

    fn note_persistent_container_candidate(&self, status: u16, body: &[u8]) -> Result<String> {
        if !(200..300).contains(&status) {
            return Err(LeaseDeny::forbidden(
                "persistent BuildKit container create did not succeed",
            ));
        }
        let value = parse_create_value(body)
            .context("parse persistent BuildKit container create response")?;
        let identifier = value
            .get("Id")
            .or_else(|| value.get("ID"))
            .and_then(Value::as_str)
            .context("persistent BuildKit container response omitted its identifier")?;
        let identifier = validate_owned_resource_id(identifier, "created Docker container")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources
            .persistent_container_candidates
            .insert(identifier.clone());
        Ok(identifier)
    }

    fn forget_volume(&self, target: &str) -> Result<()> {
        let target = validate_owned_resource_id(target, "Docker volume")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.volumes.remove(&target);
        resources.persistent_volumes.remove(&target);
        Ok(())
    }

    fn record_persistent_container_inspect(
        &self,
        target: &str,
        status: u16,
        body: &[u8],
        daemon_id: &str,
    ) -> Result<()> {
        if status == 404 {
            return self.forget_persistent_container(target);
        }
        if !(200..300).contains(&status) {
            // A non-404 response is an inconclusive identity result. Do not
            // treat it as an absent container and leave a stale capability in
            // the registry: fail closed until the next full attestation.
            self.forget_persistent_container(target)?;
            bail!(
                "persistent BuildKit container inspect returned inconclusive HTTP status {status}"
            );
        }
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        let (name, id, volume, _image_id) = match attest_persistent_buildkit_container(
            body,
            target,
            daemon_id,
            &allowed_builders,
            &self.persistent_builder_images()?,
        ) {
            Ok(attested) => attested,
            Err(error) => {
                self.forget_persistent_container(target)?;
                return Err(error);
            }
        };
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources
            .persistent_containers
            .retain(|existing_name, existing_id| existing_name != &name && existing_id != &id);
        resources.persistent_container_candidates.remove(&id);
        resources
            .persistent_container_volumes
            .retain(|existing_name, _| existing_name != &name);
        resources.persistent_containers.insert(name.clone(), id);
        resources.persistent_container_volumes.insert(name, volume);
        Ok(())
    }

    fn record_persistent_volume_inspect(
        &self,
        target: &str,
        status: u16,
        body: &[u8],
        daemon_id: &str,
    ) -> Result<()> {
        if status == 404 {
            return self.forget_volume(target);
        }
        if !(200..300).contains(&status) {
            self.forget_volume(target)?;
            bail!("persistent BuildKit volume inspect returned inconclusive HTTP status {status}");
        }
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        if let Err(error) =
            attest_persistent_buildkit_volume(body, target, daemon_id, &allowed_builders)
        {
            self.forget_volume(target)?;
            return Err(error);
        }
        self.remember_persistent_volume(target)
    }

    /// Register the TSV projection returned by the host runner's bounded
    /// `docker volume inspect --format` command. The guest-facing inspect
    /// route uses JSON, so it has a separate parser above.
    fn record_persistent_volume_projection(
        &self,
        target: &str,
        output: &[u8],
        daemon_id: &str,
    ) -> Result<()> {
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        let output = std::str::from_utf8(output)
            .context("Docker persistent BuildKit volume projection must be UTF-8")?;
        if let Err(error) = attest_persistent_buildkit_volume_projection(
            output,
            target,
            daemon_id,
            &allowed_builders,
        ) {
            self.forget_volume(target)?;
            return Err(error);
        }
        self.remember_persistent_volume(target)
    }

    fn remember_persistent_volume(&self, target: &str) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.persistent_volumes.insert(target.to_owned());
        Ok(())
    }

    fn record_owned_volume_inspect(
        &self,
        target: &str,
        status: u16,
        body: &[u8],
        job_id: &str,
        daemon_id: &str,
    ) -> Result<()> {
        if status == 404 {
            return self.forget_volume(target);
        }
        if !(200..300).contains(&status) {
            self.forget_volume(target)?;
            bail!("owned Docker volume inspect returned HTTP status {status}");
        }
        if let Err(error) = attest_created_volume_identity(body, target, job_id, daemon_id) {
            self.forget_volume(target)?;
            return Err(error);
        }
        Ok(())
    }

    /// Learn a created container's name once its create succeeded. Keep the
    /// immutable response ID beside the alias so a later same-name object
    /// cannot inherit this lease's capability.
    fn note_container_name(&self, name: &str, status: u16, body: &[u8]) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let name = validate_owned_resource_id(name, "Docker resource")?;
        let value = parse_create_value(body)
            .context("parse successful Docker create response for container alias")?;
        let identifier = value
            .get("Id")
            .or_else(|| value.get("ID"))
            .and_then(Value::as_str)
            .context("successful Docker create response omitted its container identifier")?;
        let identifier = validate_owned_resource_id(identifier, "created Docker container")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources.containers.contains(&identifier) {
            resources.container_names.insert(name, identifier);
        }
        Ok(())
    }

    fn note_persistent_exec(&self, status: u16, body: &[u8], builder: &str) -> Result<()> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let value =
            parse_create_value(body).context("parse persistent BuildKit exec create response")?;
        let identifier = value
            .get("Id")
            .or_else(|| value.get("ID"))
            .and_then(Value::as_str)
            .context("persistent BuildKit exec response omitted its identifier")?;
        let identifier = validate_owned_resource_id(identifier, "created Docker exec")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !resources.execs.contains(&identifier)
            && owned_resource_count(&resources) >= MAX_OWNED_DOCKER_RESOURCES
        {
            bail!("Docker lease ownership registry is full");
        }
        resources.execs.insert(identifier.clone());
        resources.persistent_execs.insert(identifier.clone());
        resources
            .persistent_exec_builders
            .insert(identifier, builder);
        Ok(())
    }

    #[cfg(test)]
    fn record_create_response(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        self.record_create_response_with_lease(kind, status, body, None, None, None)
    }

    fn record_create_response_with_lease(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
        expected_name: Option<&str>,
        job_id: Option<&str>,
        daemon_id: Option<&str>,
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        if kind == DockerResourceKind::Volume {
            let job_id = job_id.context("volume create response missing lease job identity")?;
            let daemon_id =
                daemon_id.context("volume create response missing lease daemon identity")?;
            let object = parse_api_object(body, "volume create")?;
            let returned_name = validate_owned_resource_id(
                api_object_string(&object, "Name")?,
                "Docker volume name",
            )?;
            if expected_name.is_some_and(|expected| expected != returned_name) {
                if let Some(expected_name) = expected_name {
                    self.forget_volume(expected_name)?;
                }
                bail!(
                    "Docker volume create response name mismatch: expected {:?}, found {:?}",
                    expected_name,
                    returned_name
                );
            }
            let allowed_builders = self.persistent_builder_names()?;
            if is_persistent_buildkit_volume_name(&returned_name) {
                if let Err(error) = attest_persistent_buildkit_volume(
                    body,
                    &returned_name,
                    daemon_id,
                    &allowed_builders,
                ) {
                    self.forget_volume(&returned_name)?;
                    return Err(error);
                }
                let mut resources = self
                    .resources
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
                resources.persistent_volumes.insert(returned_name);
                return Ok(());
            }
            let identifier =
                match attest_created_volume_identity(body, &returned_name, job_id, daemon_id) {
                    Ok(identifier) => identifier,
                    Err(error) => {
                        self.forget_volume(&returned_name)?;
                        return Err(error);
                    }
                };
            return self.record_owned_resource_identifier(kind, identifier);
        }
        let value = parse_create_value(body)
            .context("parse successful Docker create response for ownership")?;
        let identifier = match kind {
            DockerResourceKind::Container
            | DockerResourceKind::Network
            | DockerResourceKind::Exec => value
                .get("Id")
                .or_else(|| value.get("ID"))
                .and_then(Value::as_str),
            DockerResourceKind::Volume => None,
        }
        .context("successful Docker create response omitted its resource identifier")?;
        let identifier = validate_owned_resource_id(identifier, "created Docker resource")?;
        self.record_owned_resource_identifier(kind, identifier)
    }

    fn record_owned_resource_identifier(
        &self,
        kind: DockerResourceKind,
        identifier: String,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let already_owned = match kind {
            DockerResourceKind::Container => resources.containers.contains(&identifier),
            DockerResourceKind::Network => resources.networks.contains(&identifier),
            DockerResourceKind::Volume => resources.volumes.contains(&identifier),
            DockerResourceKind::Exec => resources.execs.contains(&identifier),
        };
        if !already_owned && owned_resource_count(&resources) >= MAX_OWNED_DOCKER_RESOURCES {
            bail!("Docker lease ownership registry is full");
        }
        match kind {
            DockerResourceKind::Container => {
                resources.containers.insert(identifier);
            }
            DockerResourceKind::Network => {
                resources.networks.insert(identifier);
            }
            DockerResourceKind::Volume => {
                resources.volumes.insert(identifier);
            }
            DockerResourceKind::Exec => {
                resources.execs.insert(identifier);
            }
        }
        Ok(())
    }

    fn record_delete_response(
        &self,
        kind: DockerResourceKind,
        target: &str,
        status: u16,
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        match kind {
            DockerResourceKind::Container => {
                let removed_id = resources.container_names.remove(&target);
                resources.containers.remove(&target);
                if let Some(removed_id) = removed_id {
                    resources.containers.remove(&removed_id);
                    resources
                        .container_names
                        .retain(|_, owner_id| owner_id != &removed_id);
                } else {
                    resources
                        .container_names
                        .retain(|_, owner_id| owner_id != &target);
                }
            }
            DockerResourceKind::Network => {
                resources.networks.remove(&target);
            }
            DockerResourceKind::Volume => {
                resources.volumes.remove(&target);
            }
            DockerResourceKind::Exec => {
                resources.execs.remove(&target);
                resources.persistent_execs.remove(&target);
                resources.persistent_exec_builders.remove(&target);
            }
        }
        Ok(())
    }
}

fn authorize_docker_route(
    route: AuthorizedDockerRoute,
    upgrade: bool,
) -> Result<AuthorizedDockerRoute> {
    if upgrade
        && !matches!(
            route,
            AuthorizedDockerRoute::Hijack(_)
                | AuthorizedDockerRoute::PersistentExec
                | AuthorizedDockerRoute::DaemonTunnel
        )
    {
        return Err(LeaseDeny::forbidden(
            "Docker lease denied unowned upgrade/tunnel route",
        ));
    }
    Ok(route)
}

/// Docker's `POST /containers/create?name=<name>` query. Parse the same
/// decoded first-value namespace Docker uses, but reject duplicate or
/// malformed `name` keys before route selection. Falling through to generic
/// create after a malformed persistent name would let Docker create an
/// unregistered alias.
fn containers_create_query_name(request: &[u8]) -> Result<Option<String>> {
    let (_, target) = docker_request_line(request)?;
    let Some(query) = target.split_once('?').map(|(_, query)| query) else {
        return Ok(None);
    };
    let mut name = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some(pair) => pair,
            None => {
                if percent_decode_query_component(pair)? == "name" {
                    bail!("Docker container create name query is missing its value");
                }
                continue;
            }
        };
        if percent_decode_query_component(key)? == "name" {
            if name.is_some() {
                bail!("Docker container create repeats name");
            }
            let value = percent_decode_query_component(value)?;
            if value.is_empty()
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'?' | b'#'))
            {
                bail!("Docker container create name is malformed");
            }
            name = Some(validate_owned_resource_id(&value, "Docker container name")?);
        }
    }
    Ok(name)
}

fn owned_resource_count(resources: &OwnedDockerResources) -> usize {
    resources.containers.len()
        + resources.networks.len()
        + resources.volumes.len()
        + resources.execs.len()
}

fn capture_response_bytes(captured: &mut Option<Vec<u8>>, bytes: &[u8]) -> Result<()> {
    let Some(captured) = captured else {
        return Ok(());
    };
    if bytes.len() > MAX_CREATE_RESPONSE_BODY.saturating_sub(captured.len()) {
        bail!("Docker create response exceeds ownership capture limit");
    }
    captured.extend_from_slice(bytes);
    Ok(())
}

fn validate_owned_resource_id(id: &str, kind: &str) -> Result<String> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > MAX_OWNED_DOCKER_RESOURCE_ID
        || id
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'/')
    {
        bail!("invalid {kind} identifier");
    }
    Ok(id.to_owned())
}

fn docker_request_line(request: &[u8]) -> Result<(&str, &str)> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("Docker API request is missing header terminator")?;
    let header = std::str::from_utf8(&request[..header_end])
        .context("Docker API request headers must be UTF-8")?;
    let line = header.split_once("\r\n").map_or(header, |(line, _)| line);
    let mut parts = line.split_ascii_whitespace();
    let method = parts.next().context("Docker API request has no method")?;
    let target = parts.next().context("Docker API request has no target")?;
    let version = parts.next().context("Docker API request has no version")?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        bail!("malformed Docker API request line");
    }
    Ok((method, target))
}

fn rewrite_http_request_target(request: &[u8], target: &str) -> Result<Vec<u8>> {
    let line_end = request
        .windows(2)
        .position(|window| window == b"\r\n")
        .context("Docker API request is missing request-line terminator")?;
    let line = std::str::from_utf8(&request[..line_end])
        .context("Docker API request line must be UTF-8")?;
    let mut parts = line.split_ascii_whitespace();
    let method = parts.next().context("Docker API request has no method")?;
    let _old_target = parts.next().context("Docker API request has no target")?;
    let version = parts.next().context("Docker API request has no version")?;
    if parts.next().is_some() {
        bail!("malformed Docker API request line");
    }
    let mut rewritten = Vec::with_capacity(request.len());
    rewritten.extend_from_slice(format!("{method} {target} {version}\r\n").as_bytes());
    rewritten.extend_from_slice(&request[line_end + 2..]);
    Ok(rewritten)
}

fn docker_request_body(request: &[u8]) -> Result<&[u8]> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    Ok(&request[header_end..])
}

fn docker_api_path_segments(path: &str) -> Result<Vec<&str>> {
    let mut segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty()) {
        bail!("Docker API route contains an empty path segment");
    }
    if segments.first().is_some_and(|segment| {
        segment.len() > 1 && segment.starts_with('v') && segment.as_bytes()[1].is_ascii_digit()
    }) {
        let version = segments.remove(0);
        let valid = version
            .strip_prefix('v')
            .and_then(|version| version.split_once('.'))
            .is_some_and(|(major, minor)| {
                !major.is_empty()
                    && !minor.is_empty()
                    && major.chars().all(|ch| ch.is_ascii_digit())
                    && minor.chars().all(|ch| ch.is_ascii_digit())
            });
        if !valid {
            bail!("Docker API route has an invalid version prefix");
        }
    }
    Ok(segments)
}

fn docker_resource_target(request: &[u8]) -> Option<String> {
    let (_, target) = docker_request_line(request).ok()?;
    let path = canonical_docker_path(target).ok()?;
    let segments = docker_api_path_segments(&path).ok()?;
    match segments.as_slice() {
        ["containers", id]
        | ["containers", id, _]
        | ["networks", id]
        | ["networks", id, _]
        | ["volumes", id] => Some((*id).to_owned()),
        _ => None,
    }
}

pub fn guest_docker_socket_host(job_id: &str, unique: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    hasher.update(unique.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut short = [0_u8; 8];
    short.copy_from_slice(&digest[..8]);
    // Keep the proxy socket below the job's shared work tree. The Docker
    // daemon may run in a VM (Docker Desktop/OrbStack), so a host-global
    // runtime path is not necessarily visible to it. `JobContainerSpec`
    // applies the same host-work-dir mapping to this path before mounting it;
    // the runner itself always binds the returned host-visible path.
    let name = format!("vdl-{:016x}.sock", u64::from_be_bytes(short));
    let preferred = unique.join("_velnor").join(&name);
    if preferred.to_string_lossy().len() < UNIX_SOCKET_PATH_LIMIT {
        return preferred;
    }

    // macOS's temporary roots can be longer than Linux's while still being
    // valid Docker bind sources. Walk upward only within the daemon-shared
    // work root when necessary. Never fall back to an unrelated global temp
    // directory: that would let the host listener and VM bind source diverge.
    let shared_root = unique
        .parent()
        .and_then(Path::parent)
        .map(|root| crate::container::daemon_shared_root(root.to_path_buf()));
    let mut ancestor = unique.parent();
    while let Some(path) = ancestor {
        let candidate = path.join(&name);
        if candidate.to_string_lossy().len() < UNIX_SOCKET_PATH_LIMIT {
            return candidate;
        }
        if shared_root.as_deref().is_some_and(|root| path == root) {
            break;
        }
        ancestor = path.parent();
    }
    // Keep the original tree identity when no shorter candidate exists. The
    // bind operation returns an actionable length error, and a configured VM
    // mapping can reject an escaping path before any container starts.
    preferred
}

pub fn list_owned_containers_args(job_id: &str) -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}".into(),
    ]
}

/// List job-owned containers with the ownership and lifecycle fields needed
/// for a fail-closed startup cleanup decision.
pub fn list_owned_containers_state_args(job_id: &str) -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.State}}".into(),
    ]
}

pub fn list_owned_networks_args(job_id: &str) -> Vec<String> {
    vec![
        "network".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
    ]
}

pub fn list_owned_volumes_args(job_id: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
    ]
}

pub fn list_owned_job_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}"),
        "--format".into(),
        "{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.State}}".into(),
    ]
}

/// List every container carrying a daemon ownership label, with the owning
/// daemon id in the third column so callers can scope reclamation to ONE
/// daemon (a host can run several daemons with different work roots).
pub fn list_daemon_owned_job_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}"),
        "--format".into(),
        "{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}\t{{.State}}"
            .into(),
    ]
}

pub fn list_testcontainers_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={TESTCONTAINERS_LABEL}"),
        "--format".into(),
        "{{.ID}}\t{{.Label \"velnor.job-id\"}}".into(),
    ]
}

pub fn list_job_image_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("name={JOB_CONTAINER_NAME_PREFIX}"),
        "--format".into(),
        "{{.ID}}\t{{.Label \"velnor.job-id\"}}".into(),
    ]
}

pub fn list_job_buildkit_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}\t{{.State}}"
            .into(),
    ]
}

pub fn list_job_buildkit_volume_args() -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
    ]
}

/// List BuildKit volumes with the ownership labels injected by the lease
/// proxy. Daemon-scoped startup reclaim must prove both daemon and job
/// ownership before deleting a volume; names alone are not an ownership
/// boundary when daemons share a Docker engine.
pub fn list_daemon_owned_job_buildkit_volume_format_args() -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}"),
        "--format".into(),
        "{{.Name}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}".into(),
    ]
}

/// Created `velnor-preflight-*` leftovers from prior job-image tags. After a
/// release retags `velnor/job-ubuntu:26.04`, `ancestor=` no longer matches
/// those older image IDs, so name prefix is the remaining ownership key.
pub fn list_preflight_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        "name=velnor-preflight-".into(),
        "--format".into(),
        "{{.ID}}\t{{.Label \"velnor.job-id\"}}".into(),
    ]
}

pub fn force_remove_container_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["rm".into(), "--force".into()];
    args.extend(ids.iter().cloned());
    args
}

/// Remove containers without force. Stale/orphan cleanup uses this form so a
/// container that becomes live after the final liveness snapshot is refused by
/// Docker instead of being killed.
pub fn remove_container_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["rm".into()];
    args.extend(ids.iter().cloned());
    args
}

/// One container id. BuildKit reclaim must never batch ids into one `docker rm`.
pub fn force_remove_one_container_args(id: &str) -> Vec<String> {
    vec!["rm".into(), "--force".into(), id.to_string()]
}

pub fn remove_one_container_args(id: &str) -> Vec<String> {
    remove_container_args(&[id.to_string()])
}

/// Docker Engine can issue concurrent DELETE requests when `docker rm` gets
/// multiple docker-container BuildKit IDs. Created/removing BuildKit daemons
/// then deadlock each other's removal and survive the bounded timeout. Keep
/// this narrow helper for BuildKit only; ordinary guest containers are safe to
/// remove in a batch.
pub fn force_remove_containers_serially(
    ids: &[String],
    mut docker: impl FnMut(&[String]) -> Result<()>,
) -> Result<()> {
    let mut first_error = None;
    for id in ids {
        if let Err(error) = docker(&force_remove_one_container_args(id))
            && first_error.is_none()
        {
            first_error = Some(error.context(format!("remove BuildKit container {id}")));
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Docker Engine can issue concurrent DELETE requests when `docker rm` gets
/// multiple docker-container BuildKit IDs. Keep stale/orphan cleanup
/// serialized while deliberately omitting `--force`, so Docker protects an
/// object that becomes live during the cleanup race.
pub fn remove_containers_serially(
    ids: &[String],
    mut docker: impl FnMut(&[String]) -> Result<()>,
) -> Result<()> {
    let mut first_error = None;
    for id in ids {
        if let Err(error) = docker(&remove_one_container_args(id))
            && first_error.is_none()
        {
            first_error = Some(error.context(format!("remove stale container {id}")));
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn force_remove_network_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["network".into(), "rm".into()];
    args.extend(ids.iter().cloned());
    args
}

/// Drop-guard for a job's `velnor-net-*` network.
///
/// The per-job network is created before any container exists and is normally
/// removed by terminal cleanup ([`list_job_owned`]/[`remove_job_owned`]).
/// Every path that skips
/// that cleanup — an early `?` between network creation and the step loop, a
/// panic unwinding out of the executor, cleanup returning an error after the
/// network removal itself failed — used to leak the network. Enough leaked
/// `velnor-net-*` networks exhaust Docker's address pool and then EVERY new
/// job fails ("all predefined address pools have been fully subnetted"). The
/// guard makes the executor own the network for its whole lifetime when its
/// immutable ID is known: dropping it while still armed removes that ID.
/// Unknown identities fail closed. Docker refuses to remove a network with
/// active endpoints, so a guard that fires while a job container is still
/// attached cannot break a live job — it fails best-effort and the periodic
/// empty-network sweep removes it once the job is gone.
pub struct JobNetworkGuard {
    network: String,
    network_id: Option<String>,
    armed: bool,
}

impl JobNetworkGuard {
    /// Arm the guard for `network`. Call [`JobNetworkGuard::defuse`] after
    /// terminal cleanup has removed the network itself.
    #[allow(dead_code)]
    pub fn arm(network: impl Into<String>) -> Self {
        Self::arm_with_id(network, None)
    }

    /// Arm the guard with the ID returned by `network create` or an attested
    /// inspect. The name remains for diagnostics only; Drop mutates by ID.
    pub fn arm_with_id(network: impl Into<String>, network_id: Option<String>) -> Self {
        Self {
            network: network.into(),
            network_id,
            armed: true,
        }
    }

    /// Mark the network as reclaimed by terminal cleanup; dropping the guard
    /// then becomes a no-op.
    pub fn defuse(mut self) {
        self.armed = false;
    }
}

impl Drop for JobNetworkGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let Some(network_id) = self.network_id.as_deref() else {
            eprintln!(
                "Warning: refusing job network drop cleanup for {}: immutable ID is unknown",
                self.network
            );
            return;
        };
        let args = force_remove_network_args(&[network_id.to_owned()]);
        if let Err(error) = docker_client::host_call(&args) {
            eprintln!(
                "Warning: job network drop-guard removal failed for {}: {error:#}",
                self.network
            );
        }
    }
}

pub fn force_remove_volume_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["volume".into(), "rm".into(), "--force".into()];
    args.extend(ids.iter().cloned());
    args
}

pub fn remove_volume_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["volume".into(), "rm".into()];
    args.extend(ids.iter().cloned());
    args
}

fn reclaim_orphan_job_buildkit(
    job_formatted: &str,
    daemon_id: Option<&str>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    reclaim_orphan_job_buildkit_with_live(
        &docker_client::live_job_ids(job_formatted),
        daemon_id,
        docker,
    )
}

fn refresh_live_buildkit_jobs(
    daemon_id: Option<&str>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<BTreeSet<String>> {
    let formatted = match daemon_id {
        Some(_) => docker(&list_daemon_owned_job_format_args())?,
        None => docker(&list_owned_job_format_args())?,
    };
    Ok(match daemon_id {
        Some(daemon_id) => docker_client::live_daemon_job_ids(&formatted, daemon_id),
        None => docker_client::live_job_ids(&formatted),
    })
}

fn reclaim_orphan_job_buildkit_with_live(
    live_jobs: &std::collections::BTreeSet<String>,
    daemon_id: Option<&str>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_job_buildkit_format_args())?;
    let initial_ids = docker_client::orphan_job_buildkit_ids(&formatted, live_jobs, daemon_id);
    let job_formatted = match daemon_id {
        Some(_) => docker(&list_daemon_owned_job_format_args())?,
        None => docker(&list_owned_job_format_args())?,
    };
    let protected_jobs = match daemon_id {
        Some(daemon_id) => docker_client::live_daemon_job_ids(&job_formatted, daemon_id),
        None => docker_client::live_job_ids(&job_formatted),
    };
    let ids = if initial_ids.is_empty() {
        Vec::new()
    } else {
        // Docker state can change between the orphan scan and DELETE. Re-list
        // job containers and BuildKit immediately before removal. An
        // unknown/malformed job state is protected, and deletion still needs
        // the same BuildKit id to be a stopped orphan in both scans.
        let revalidated = docker(&list_job_buildkit_format_args())?;
        let revalidated =
            docker_client::orphan_job_buildkit_ids(&revalidated, &protected_jobs, daemon_id);
        initial_ids
            .into_iter()
            .filter(|id| revalidated.binary_search(id).is_ok())
            .collect::<Vec<_>>()
    };
    if !ids.is_empty() {
        remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }
    let volume_ids = match daemon_id {
        Some(daemon_id) => {
            let listed = docker(&list_daemon_owned_job_buildkit_volume_format_args())?;
            let initial = daemon_owned_orphan_buildkit_volume_names(&listed, daemon_id);
            if initial.is_empty() {
                Vec::new()
            } else {
                let revalidated = docker(&list_daemon_owned_job_buildkit_volume_format_args())?;
                let revalidated =
                    daemon_owned_orphan_buildkit_volume_names(&revalidated, daemon_id);
                initial
                    .into_iter()
                    .filter(|name| revalidated.binary_search(name).is_ok())
                    .collect()
            }
        }
        None => {
            let listed = docker(&list_job_buildkit_volume_args())?;
            let initial = orphan_job_buildkit_volume_names(&listed);
            if initial.is_empty() {
                Vec::new()
            } else {
                // A name-only volume listing is a discovery hint. Re-list
                // immediately before any inspect/delete so a volume created
                // after the first scan cannot enter the mutation set.
                let revalidated = docker(&list_job_buildkit_volume_args())?;
                let revalidated = orphan_job_buildkit_volume_names(&revalidated);
                initial
                    .into_iter()
                    .filter(|name| revalidated.binary_search(name).is_ok())
                    .collect()
            }
        }
    };
    // Volumes have no immutable daemon ID. Before each name-based delete,
    // require the exact local driver, name, and Velnor job label. Persistent
    // BuildKit state is excluded by the candidate parser above. A replacement
    // or malformed projection is left untouched; a missing volume is already
    // clean.
    for volume in volume_ids {
        let inspected = match docker(&inspect_volume_identity_args(&volume)) {
            Ok(inspected) => inspected,
            Err(error) if docker_client::is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        let Ok(Some(job_id)) =
            attest_orphan_buildkit_volume(&inspected, &volume, &BTreeSet::new(), daemon_id)
        else {
            continue;
        };
        // A volume has no compare-and-delete handle. Re-attest the exact
        // name, local driver, scope, job label, and daemon label directly
        // before the one-name removal so a same-name replacement is left
        // untouched.
        let current = match docker(&inspect_volume_identity_args(&volume)) {
            Ok(current) => current,
            Err(error) if docker_client::is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        // Refresh liveness immediately before each state-volume deletion. A
        // job can start after the candidate scan; one snapshot for the whole
        // batch would let a newly live job lose its BuildKit state volume.
        let current_live_jobs = refresh_live_buildkit_jobs(daemon_id, docker)?;
        let Ok(Some(current_job_id)) =
            attest_orphan_buildkit_volume(&current, &volume, &current_live_jobs, daemon_id)
        else {
            continue;
        };
        if current_job_id != job_id {
            continue;
        }
        match docker(&remove_volume_args(std::slice::from_ref(&volume))) {
            Ok(_) => {}
            Err(error) if docker_client::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn orphan_job_buildkit_volume_names(formatted: &str) -> Vec<String> {
    let mut names = formatted
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| buildkit_volume_scope(name).is_some())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn daemon_owned_orphan_buildkit_volume_names(formatted: &str, daemon_id: &str) -> Vec<String> {
    let mut names =
        docker_client::daemon_owned_buildkit_volume_names(formatted, daemon_id, &BTreeSet::new())
            .into_iter()
            .filter(|name| buildkit_volume_scope(name).is_some())
            .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

#[allow(dead_code)]
pub fn is_docker_object_create(method: &str, target: &str) -> bool {
    if !method.eq_ignore_ascii_case("POST") {
        return false;
    }
    canonical_docker_path(target).is_ok_and(|path| is_docker_object_create_path(method, &path))
}

fn is_docker_object_create_path(method: &str, path: &str) -> bool {
    method.eq_ignore_ascii_case("POST")
        && (path.ends_with("/containers/create")
            || path.ends_with("/networks/create")
            || path.ends_with("/volumes/create"))
}

fn canonical_docker_path(target: &str) -> Result<String> {
    let raw_path = target.split_once('?').map_or(target, |(path, _)| path);
    if !raw_path.starts_with('/') {
        bail!("Docker API target path must be absolute");
    }
    let mut path = Vec::with_capacity(raw_path.len());
    let bytes = raw_path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes
                .get(index + 1)
                .and_then(|byte| (*byte as char).to_digit(16))
                .context("Docker API target contains an incomplete percent escape")?;
            let low = bytes
                .get(index + 2)
                .and_then(|byte| (*byte as char).to_digit(16))
                .context("Docker API target contains an invalid percent escape")?;
            let decoded = ((high << 4) | low) as u8;
            if decoded == b'/' || decoded == b'\\' {
                bail!("Docker API target contains an encoded path separator");
            }
            path.push(decoded);
            index += 3;
        } else {
            if bytes[index] == b'\\' {
                bail!("Docker API target contains a path separator alias");
            }
            path.push(bytes[index]);
            index += 1;
        }
    }
    let path = String::from_utf8(path).context("Docker API target path must be UTF-8")?;
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        bail!("Docker API target contains a dot path segment");
    }
    Ok(path)
}

/// Parse a Docker create body while rejecting duplicate object keys at every
/// depth. Building `Value` during the same traversal avoids a validation pass
/// followed by a second JSON parse/materialization pass.
fn parse_create_value(body: &[u8]) -> Result<Value> {
    if body.is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    struct Seed;

    impl<'de> serde::de::DeserializeSeed<'de> for Seed {
        type Value = Value;

        fn deserialize<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
        where
            D: serde::de::Deserializer<'de>,
        {
            struct Visitor;

            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Value;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a JSON value")
                }

                fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
                    Ok(Value::Bool(value))
                }

                fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
                    Ok(Value::Number(value.into()))
                }

                fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
                    Ok(Value::Number(value.into()))
                }

                fn visit_f64<E>(self, value: f64) -> std::result::Result<Value, E>
                where
                    E: serde::de::Error,
                {
                    serde_json::Number::from_f64(value)
                        .map(Value::Number)
                        .ok_or_else(|| E::custom("JSON number is not finite"))
                }

                fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E> {
                    Ok(Value::String(value.to_owned()))
                }

                fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Value, E> {
                    Ok(Value::String(value.to_owned()))
                }

                fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
                    Ok(Value::String(value))
                }

                fn visit_unit<E>(self) -> std::result::Result<Value, E> {
                    Ok(Value::Null)
                }

                fn visit_none<E>(self) -> std::result::Result<Value, E> {
                    Ok(Value::Null)
                }

                fn visit_some<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
                where
                    D: serde::de::Deserializer<'de>,
                {
                    serde::de::DeserializeSeed::deserialize(Seed, deserializer)
                }

                fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Value, A::Error>
                where
                    A: serde::de::SeqAccess<'de>,
                {
                    let mut values = Vec::new();
                    while let Some(value) = seq.next_element_seed(Seed)? {
                        values.push(value);
                    }
                    Ok(Value::Array(values))
                }

                fn visit_map<A>(self, mut map: A) -> std::result::Result<Value, A::Error>
                where
                    A: serde::de::MapAccess<'de>,
                {
                    let mut object = Map::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if object.contains_key(&key) {
                            return Err(serde::de::Error::custom(format!(
                                "duplicate JSON object key `{key}`"
                            )));
                        }
                        let value = map.next_value_seed(Seed)?;
                        object.insert(key, value);
                    }
                    Ok(Value::Object(object))
                }
            }

            deserializer.deserialize_any(Visitor)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let value = serde::de::DeserializeSeed::deserialize(Seed, &mut deserializer)
        .map_err(|error| anyhow::anyhow!("parse Docker create JSON: {error}"))?;
    deserializer
        .end()
        .context("parse trailing data after Docker create JSON")?;
    Ok(value)
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
pub fn inject_ownership_labels(body: &[u8], job_id: &str, daemon_id: &str) -> Result<Vec<u8>> {
    let mut value = parse_create_value(body)?;
    inject_ownership_labels_value(&mut value, job_id, daemon_id)?;
    serde_json::to_vec(&value).context("serialize labeled Docker create body")
}

fn inject_ownership_labels_value(value: &mut Value, job_id: &str, daemon_id: &str) -> Result<()> {
    let Some(object) = value.as_object_mut() else {
        bail!("Docker create body must be a JSON object");
    };
    let label_keys = object
        .keys()
        .filter(|key| key.eq_ignore_ascii_case("Labels"))
        .cloned()
        .collect::<Vec<_>>();
    if label_keys.len() > 1 {
        bail!("Docker create contains duplicate case-insensitive Labels keys");
    }
    let labels = label_keys
        .first()
        .and_then(|key| object.remove(key))
        .unwrap_or(Value::Object(Map::new()));
    let mut labels = match labels {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        other => {
            bail!("Docker create Labels must be an object, got {other}");
        }
    };
    labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.to_string()));
    labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.to_string()));
    object.insert("Labels".into(), Value::Object(labels));
    Ok(())
}

/// Rewrite a Docker Engine HTTP/1.1 request so object creates carry job labels.
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
pub fn rewrite_docker_api_request(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
) -> Result<Vec<u8>> {
    rewrite_docker_api_request_with_volumes(
        request,
        job_id,
        daemon_id,
        &BTreeSet::new(),
        false,
        None,
    )
}

fn rewrite_docker_api_request_with_volumes(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    owned_volume_names: &BTreeSet<String>,
    persistent_bootstrap: bool,
    persistent_image_id: Option<&str>,
) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header_bytes = &request[..header_end];
    let body = &request[header_end..];
    let header_text =
        std::str::from_utf8(header_bytes).context("Docker API headers must be UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let path = canonical_docker_path(target)?;
    let is_create = is_docker_object_create_path(method, &path);
    if !is_create {
        return Ok(request.to_vec());
    }
    let mut headers = Vec::new();
    let mut content_length_seen = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            headers.push(line);
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            if content_length_seen {
                bail!("refusing to rewrite Docker create request with duplicate Content-Length");
            }
            let declared = value
                .trim()
                .parse::<usize>()
                .context("parse Docker API Content-Length")?;
            if declared != body.len() {
                bail!(
                    "Docker API Content-Length {declared} does not match request body length {}",
                    body.len()
                );
            }
            content_length_seen = true;
            continue;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("refusing to rewrite Docker create request with Transfer-Encoding");
        }
        headers.push(line);
    }
    let mut value = parse_create_value(body)?;
    if path.ends_with("/containers/create") {
        if persistent_bootstrap {
            inject_persistent_bootstrap_value(
                &mut value,
                job_id,
                daemon_id,
                persistent_image_id.context("persistent bootstrap image was not attested")?,
            )?;
        } else {
            inject_job_cgroup_parent_value(&mut value, owned_volume_names)?;
            inject_ownership_labels_value(&mut value, job_id, daemon_id)?;
        }
    } else if path.ends_with("/networks/create") {
        validate_network_create_value(&value)?;
        inject_ownership_labels_value(&mut value, job_id, daemon_id)?;
    } else if path.ends_with("/volumes/create") {
        reject_unsafe_volume_create_value(&value)?;
        inject_ownership_labels_value(&mut value, job_id, daemon_id)?;
    }
    let labeled = serde_json::to_vec(&value).context("serialize rewritten Docker create body")?;
    let mut out = Vec::new();
    out.extend_from_slice(request_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for line in headers {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", labeled.len()).as_bytes());
    out.extend_from_slice(&labeled);
    Ok(out)
}

/// Keep the network-create capability to Docker's private, runner-compatible
/// bridge shape. Network creation is a host-affecting route: labels alone do
/// not constrain driver plugins, IPAM, or daemon network options.
fn validate_network_create_request(request: &[u8]) -> Result<()> {
    let body = docker_request_body(request)?;
    let value = parse_create_value(body).context("parse Docker network create request")?;
    validate_network_create_value(&value)
}

const APPROVED_BUILDKIT_CONFIG: &str = "[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]";

fn validate_persistent_archive_request(request: &[u8], target: &str) -> Result<()> {
    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .context("persistent BuildKit archive omitted its query")?;
    let mut path = None;
    let mut no_overwrite_dir_non_dir = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .context("persistent BuildKit archive query is malformed")?;
        let key = percent_decode_query_component(key)?;
        let value = percent_decode_query_component(value)?;
        match key.as_str() {
            "path" => {
                if path.replace(value).is_some() {
                    bail!("persistent BuildKit archive query repeats path");
                }
            }
            "noOverwriteDirNonDir" => {
                if no_overwrite_dir_non_dir
                    .replace(value.eq_ignore_ascii_case("true"))
                    .is_some()
                {
                    bail!("persistent BuildKit archive query repeats noOverwriteDirNonDir");
                }
                if !value.eq_ignore_ascii_case("true") {
                    bail!("persistent BuildKit archive requires noOverwriteDirNonDir=true");
                }
            }
            _ => bail!("persistent BuildKit archive query contains an unsupported field"),
        }
    }
    if path.as_deref() != Some("/etc") {
        bail!("persistent BuildKit archive destination is not /etc");
    }
    if no_overwrite_dir_non_dir != Some(true) {
        bail!("persistent BuildKit archive omitted noOverwriteDirNonDir=true");
    }
    let body = docker_request_body(request)?;
    if body.len() > 2 * 1024 * 1024 {
        bail!("persistent BuildKit archive exceeds the config size limit");
    }
    validate_persistent_buildkit_tar(body)
}

fn validate_persistent_buildkit_tar(body: &[u8]) -> Result<()> {
    if body.len() < 1024 || !body.len().is_multiple_of(512) {
        bail!("persistent BuildKit archive is not a padded tar stream");
    }
    let mut offset = 0;
    let mut files = 0;
    let mut directories = BTreeSet::new();
    let mut found_config = false;
    while offset + 512 <= body.len() {
        let header = &body[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            if body[offset..].iter().any(|byte| *byte != 0) {
                bail!("persistent BuildKit archive has nonzero data after its terminator");
            }
            break;
        }
        validate_tar_checksum(header)?;
        let name = tar_string(&header[..100])?;
        let prefix = tar_string(&header[345..500])?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.contains("..") || path.starts_with('/') {
            bail!("persistent BuildKit archive contains an unexpected path");
        }
        let kind = header[156];
        let size = tar_octal(&header[124..136])?;
        if size > 1024 * 1024 {
            bail!("persistent BuildKit config exceeds the size limit");
        }
        let data_start = offset + 512;
        let data_end = data_start
            .checked_add(size)
            .context("persistent BuildKit archive size overflow")?;
        let padded_end = data_start
            .checked_add(size.div_ceil(512) * 512)
            .context("persistent BuildKit archive padding overflow")?;
        if padded_end > body.len() {
            bail!("persistent BuildKit archive entry exceeds its body");
        }
        match kind {
            b'5' => {
                if path != "buildkit/" || size != 0 || tar_octal(&header[100..108])? != 0o755 {
                    bail!("persistent BuildKit archive contains an unexpected directory");
                }
                directories.insert(path);
            }
            0 | b'0' => {
                if path != "buildkit/buildkitd.toml"
                    || tar_octal(&header[100..108])? != 0o644
                    || !is_approved_buildkit_config(&body[data_start..data_end])
                {
                    bail!(
                        "persistent BuildKit archive permits only the approved buildkit/buildkitd.toml"
                    );
                }
                files += 1;
                found_config = true;
            }
            _ => bail!("persistent BuildKit archive contains a non-regular entry"),
        }
        offset = padded_end;
    }
    if files == 0 {
        if directories.is_empty() {
            return Ok(());
        }
        bail!("persistent BuildKit archive contains a directory without its config file");
    }
    if files != 1 || !found_config || directories != BTreeSet::from(["buildkit/".to_owned()]) {
        bail!("persistent BuildKit archive must contain exactly buildkit/buildkitd.toml");
    }
    Ok(())
}

/// Compare BuildKit config semantics rather than bytes. Buildx parses and
/// reserializes TOML while loading config files, so harmless whitespace and
/// quoting changes must not turn a safe mirror-only config into a denial.
pub(crate) fn is_approved_persistent_buildkit_config(text: &str) -> bool {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return false;
    };
    let Ok(expected) = toml::from_str::<toml::Value>(APPROVED_BUILDKIT_CONFIG) else {
        return false;
    };
    value == expected
}

fn is_approved_buildkit_config(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    is_approved_persistent_buildkit_config(text)
}

fn tar_string(bytes: &[u8]) -> Result<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .context("persistent BuildKit archive header is not UTF-8")
        .map(str::to_owned)
}

fn tar_octal(bytes: &[u8]) -> Result<usize> {
    let text = std::str::from_utf8(bytes)
        .context("persistent BuildKit archive size is not ASCII")?
        .trim_matches('\0')
        .trim();
    usize::from_str_radix(text, 8).context("persistent BuildKit archive size is not octal")
}

fn validate_tar_checksum(header: &[u8]) -> Result<()> {
    let expected = tar_octal(&header[148..156])?;
    let actual = header.iter().enumerate().fold(0_u64, |sum, (index, byte)| {
        sum + if (148..156).contains(&index) {
            b' ' as u64
        } else {
            *byte as u64
        }
    });
    if actual != expected as u64 {
        bail!("persistent BuildKit archive checksum mismatch");
    }
    Ok(())
}

fn validate_build_request_target(target: &str) -> Result<()> {
    let Some(query) = target.split_once('?').map(|(_, query)| query) else {
        return Ok(());
    };
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .context("Docker build query is malformed")?;
        let key = percent_decode_query_component(key)?;
        if matches!(key.as_str(), "t" | "tag" | "repository") {
            let value = percent_decode_query_component(value)?;
            if is_reserved_persistent_buildkit_reference(&value) {
                bail!("Docker build may not overwrite the host-approved BuildKit image");
            }
        }
    }
    Ok(())
}

/// Docker Hub has several equivalent registry spellings. Reserve the
/// complete `moby/buildkit` repository under each spelling, including an
/// untagged repository value, so `/build?t=...` cannot retag the image used by
/// the persistent docker-container builder through an alias.
fn is_reserved_persistent_buildkit_reference(reference: &str) -> bool {
    let reference = reference.trim().to_ascii_lowercase();
    let repository = ["docker.io/", "index.docker.io/", "registry-1.docker.io/"]
        .iter()
        .find_map(|prefix| reference.strip_prefix(prefix))
        .unwrap_or(&reference);
    let repository = repository
        .split_once('@')
        .map_or(repository, |(repository, _)| repository);
    let repository = repository
        .split_once(':')
        .map_or(repository, |(repository, _)| repository);
    repository == "moby/buildkit"
}

fn validate_persistent_image_pull_request(target: &str) -> Result<()> {
    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .context("Docker BuildKit image pull omitted its query")?;
    let mut from_image = None;
    let mut tag = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .context("Docker BuildKit image pull query is malformed")?;
        let key = percent_decode_query_component(key)?;
        let value = percent_decode_query_component(value)?;
        match key.as_str() {
            "fromImage" => {
                if from_image.replace(value).is_some() {
                    bail!("Docker BuildKit image pull repeats fromImage");
                }
            }
            "tag" => {
                if tag.replace(value).is_some() {
                    bail!("Docker BuildKit image pull repeats tag");
                }
            }
            _ => bail!("Docker BuildKit image pull query field {key:?} is not permitted"),
        }
    }
    let from_image = from_image.context("Docker BuildKit image pull omitted fromImage")?;
    let approved_digest_tag = PERSISTENT_BUILDKIT_REPO_DIGEST
        .split_once('@')
        .map(|(_, digest)| digest);
    let mutable_buildx_reference = (matches!(
        from_image.as_str(),
        "moby/buildkit" | "docker.io/moby/buildkit"
    ) && tag.as_deref() == Some("buildx-stable-1"))
        || (matches!(
            from_image.as_str(),
            PERSISTENT_BUILDKIT_IMAGE | "docker.io/moby/buildkit:buildx-stable-1"
        ) && tag.is_none());
    let canonical_approved_digest = matches!(
        from_image.as_str(),
        "moby/buildkit" | "docker.io/moby/buildkit"
    ) && tag.as_deref() == approved_digest_tag;
    // Buildx sends the mutable default reference. It is admitted only so the
    // proxy can rewrite it to the immutable digest before Docker sees it.
    let approved = mutable_buildx_reference
        || canonical_approved_digest
        || (is_approved_persistent_repo_digest(&from_image) && tag.is_none());
    if !approved {
        bail!("Docker image pull is restricted to the approved BuildKit reference");
    }
    Ok(())
}

fn rewrite_persistent_image_pull_target(request: &[u8]) -> Result<Vec<u8>> {
    let (_, target) = docker_request_line(request)?;
    let path = target
        .split_once('?')
        .map(|(path, _)| path)
        .context("Docker BuildKit image pull omitted its path")?;
    rewrite_http_request_target(
        request,
        &format!(
            "{path}?fromImage=moby/buildkit&tag={}",
            PERSISTENT_BUILDKIT_REPO_DIGEST
                .split_once('@')
                .map_or("", |(_, digest)| digest)
        ),
    )
}

fn validate_persistent_buildkit_container_value(
    value: &Value,
    expected_name: &str,
    expected_volume: &str,
    expected_image_id: &str,
) -> Result<()> {
    let object = value
        .as_object()
        .context("persistent BuildKit container body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "persistent BuildKit container")?;
    const ALLOWED_FIELDS: [&str; 27] = [
        "hostname",
        "domainname",
        "user",
        "attachstdin",
        "attachstdout",
        "attachstderr",
        "tty",
        "openstdin",
        "stdinonce",
        "env",
        "cmd",
        "healthcheck",
        "argsescaped",
        "image",
        "exposedports",
        "volumes",
        "workingdir",
        "entrypoint",
        "networkdisabled",
        "macaddress",
        "onbuild",
        "labels",
        "stopsignal",
        "stoptimeout",
        "shell",
        "hostconfig",
        "networkingconfig",
    ];
    for key in object.keys() {
        if !ALLOWED_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
            bail!("persistent BuildKit container field {key:?} is not permitted");
        }
    }
    for (key, value) in object {
        let safe = match key.to_ascii_lowercase().as_str() {
            "hostname" => value.is_null() || value.as_str().is_some_and(str::is_empty),
            "domainname" | "user" | "workingdir" | "macaddress" => {
                value.is_null() || value.as_str().is_some_and(str::is_empty)
            }
            "attachstdin" | "attachstdout" | "attachstderr" | "tty" | "openstdin" | "stdinonce"
            | "argsescaped" | "networkdisabled" => value.as_bool() == Some(false),
            "env" => {
                value.is_null()
                    || value
                        .as_array()
                        .is_some_and(|values| is_safe_buildkit_env(values))
            }
            "cmd" => is_safe_buildkit_cmd(value),
            "healthcheck" => value.is_null(),
            "image" => value.as_str().is_some(),
            "entrypoint" => is_safe_buildkit_entrypoint(value),
            "labels" => value.is_null() || value.as_object().is_some_and(Map::is_empty),
            "stopsignal" => {
                value.is_null()
                    || value.as_str().is_some_and(|signal| {
                        signal.is_empty() || signal.eq_ignore_ascii_case("SIGTERM")
                    })
            }
            "stoptimeout" => value.is_null() || is_zero_number(value),
            "shell" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
            "exposedports" | "volumes" | "onbuild" => is_empty_default(value),
            "hostconfig" => value.as_object().is_some(),
            "networkingconfig" => is_empty_default(value),
            _ => false,
        };
        if !safe {
            bail!("persistent BuildKit container field {key:?} is unsafe");
        }
    }
    let image = api_object_field(object, "Image")
        .and_then(Value::as_str)
        .context("persistent BuildKit container omitted Image")?;
    if !is_approved_persistent_image_reference(image) && image != expected_image_id {
        bail!("persistent BuildKit container image is not host-approved");
    }
    if let Some(env) = api_object_field(object, "Env")
        && !env.is_null()
        && !env
            .as_array()
            .is_some_and(|values| is_safe_buildkit_env(values))
    {
        bail!("persistent BuildKit container Env must be empty");
    }
    if let Some(entrypoint) = api_object_field(object, "Entrypoint")
        && !is_safe_buildkit_entrypoint(entrypoint)
    {
        bail!("persistent BuildKit container Entrypoint is not the approved entrypoint");
    }
    if let Some(cmd) = api_object_field(object, "Cmd")
        && !is_safe_buildkit_cmd(cmd)
    {
        bail!("persistent BuildKit container Cmd contains unsupported flags");
    }
    if let Some(labels) = api_object_field(object, "Labels")
        && !labels.is_null()
        && !labels.as_object().is_some_and(Map::is_empty)
    {
        bail!("persistent BuildKit container Labels must be empty");
    }
    let host_config = api_object_field(object, "HostConfig")
        .and_then(Value::as_object)
        .context("persistent BuildKit container omitted HostConfig")?;
    reject_case_insensitive_duplicate_keys(host_config, "persistent BuildKit HostConfig")?;
    const ALLOWED_HOST_FIELDS: &[&str] = &[
        "privileged",
        "restartpolicy",
        "mounts",
        "networkmode",
        "init",
        "cgroupparent",
        "binds",
        "containeridfile",
        "logconfig",
        "portbindings",
        "autoremove",
        "volumedriver",
        "volumesfrom",
        "consolesize",
        "annotations",
        "capadd",
        "capdrop",
        "devices",
        "devicecgrouprules",
        "securityopt",
        "dns",
        "dnsoptions",
        "dnssearch",
        "extrahosts",
        "groupadd",
        "links",
        "cgroup",
        "devicerequests",
        "ulimits",
        "oomscoreadj",
        "publishallports",
        "readonlyrootfs",
        "tmpfs",
        "sysctls",
        "storageopt",
        "maskedpaths",
        "readonlypaths",
        "runtime",
        "volumeoptions",
        "usernsmode",
        "pidmode",
        "ipcmode",
        "cgroupnsmode",
        "utsmode",
        "isolation",
        "shmsize",
        "umask",
        "cpushares",
        "nanocpus",
        "cpuperiod",
        "cpuquota",
        "cpurealtimeperiod",
        "cpurealtimeruntime",
        "cpucount",
        "cpupercent",
        "cpusetcpus",
        "cpusetmems",
        "memory",
        "memoryreservation",
        "memoryswap",
        "memoryswappiness",
        "oomkilldisable",
        "pidslimit",
        "blkioweight",
        "blkioweightdevice",
        "blkiodevicereadbps",
        "blkiodevicewritebps",
        "blkiodevicereadiops",
        "blkiodevicewriteiops",
        "iomaximumiops",
        "iomaximumbandwidth",
    ];
    for key in host_config.keys() {
        let normalized = key.to_ascii_lowercase();
        if !ALLOWED_HOST_FIELDS
            .iter()
            .any(|allowed| *allowed == normalized)
        {
            bail!("persistent BuildKit HostConfig field {key:?} is not permitted");
        }
    }
    for key in [
        "Binds",
        "ContainerIDFile",
        "VolumeDriver",
        "Annotations",
        "CapAdd",
        "CapDrop",
        "Devices",
        "DeviceCgroupRules",
        "SecurityOpt",
        "Runtime",
        "VolumesFrom",
        "VolumeOptions",
        "PidMode",
        "IpcMode",
        "CgroupnsMode",
        "UTSMode",
        "PortBindings",
        "DNS",
        "DNSOptions",
        "DNSSearch",
        "ExtraHosts",
        "GroupAdd",
        "Links",
        "Cgroup",
        "DeviceRequests",
        "Ulimits",
        "StorageOpt",
        "Tmpfs",
        "Sysctls",
        "MaskedPaths",
        "ReadonlyPaths",
        "Umask",
        "Isolation",
        "BlkioWeightDevice",
        "BlkioDeviceReadBps",
        "BlkioDeviceWriteBps",
        "BlkioDeviceReadIOps",
        "BlkioDeviceWriteIOps",
    ] {
        if let Some(value) = api_object_field(host_config, key)
            && is_strict_value_present(value)
        {
            bail!("persistent BuildKit HostConfig field {key} is not inert");
        }
    }
    if let Some(value) = api_object_field(host_config, "UsernsMode")
        && !value.as_str().is_some_and(|mode| {
            let mode = mode.trim();
            mode.is_empty() || mode.eq_ignore_ascii_case("default") || mode == "host"
        })
    {
        bail!("persistent BuildKit UsernsMode is not an approved default");
    }
    if let Some(value) = api_object_field(host_config, "LogConfig")
        && !is_safe_persistent_log_config(value)
    {
        bail!("persistent BuildKit LogConfig is not the Docker default");
    }
    if let Some(value) = api_object_field(host_config, "VolumeDriver")
        && !is_empty_default(value)
    {
        bail!("persistent BuildKit VolumeDriver must be empty");
    }
    if let Some(value) = api_object_field(host_config, "Annotations")
        && !is_empty_default(value)
    {
        bail!("persistent BuildKit Annotations must be empty");
    }
    for key in [
        "ContainerIDFile",
        "AutoRemove",
        "PublishAllPorts",
        "ReadonlyRootfs",
        "OomScoreAdj",
        "CpuShares",
        "NanoCpus",
        "CpuPeriod",
        "CpuQuota",
        "CpuRealtimePeriod",
        "CpuRealtimeRuntime",
        "CpuCount",
        "CpuPercent",
        "CpusetCpus",
        "CpusetMems",
        "Memory",
        "MemoryReservation",
        "MemorySwap",
        "MemorySwappiness",
        "OomKillDisable",
        "PidsLimit",
        "BlkioWeight",
        "IOMaximumIOps",
        "IOMaximumBandwidth",
    ] {
        if let Some(value) = api_object_field(host_config, key)
            && !is_empty_default(value)
        {
            bail!("persistent BuildKit HostConfig field {key} is not inert");
        }
    }
    if let Some(value) = api_object_field(host_config, "ShmSize")
        && !is_empty_default(value)
    {
        bail!("persistent BuildKit ShmSize must retain Docker's default");
    }
    if let Some(value) = api_object_field(host_config, "ConsoleSize")
        && !is_default_console_size(value)
    {
        bail!("persistent BuildKit ConsoleSize must retain Docker's default");
    }
    if api_object_field(host_config, "Privileged").and_then(Value::as_bool) != Some(true) {
        bail!("persistent BuildKit container must be privileged");
    }
    if api_object_field(host_config, "Init").and_then(Value::as_bool) != Some(true) {
        bail!("persistent BuildKit container must enable init");
    }
    if let Some(network) = api_object_field(host_config, "NetworkMode")
        && !network.as_str().is_some_and(|mode| {
            mode.is_empty() || mode.eq_ignore_ascii_case("default") || mode == "bridge"
        })
    {
        bail!("persistent BuildKit container uses an unsafe network mode");
    }
    let restart = api_object_field(host_config, "RestartPolicy")
        .context("persistent BuildKit container omitted RestartPolicy")?;
    if !is_named_restart_policy(restart, &["unless-stopped"])? {
        bail!("persistent BuildKit container must use unless-stopped restart policy");
    }
    let mounts = api_object_field(host_config, "Mounts")
        .and_then(Value::as_array)
        .context("persistent BuildKit container omitted Mounts")?;
    if mounts.len() != 1 || !is_exact_persistent_state_mount(&mounts[0], expected_volume) {
        bail!("persistent BuildKit container must mount only its approved state volume");
    }
    if let Some(cgroup_parent) = api_object_field(host_config, "CgroupParent")
        && !cgroup_parent.as_str().is_some_and(|parent| {
            parent.is_empty() || parent == JOB_CGROUP_PARENT || parent == "/docker/buildx"
        })
    {
        bail!("persistent BuildKit container requested an unsafe cgroup parent");
    }
    let _ = expected_name;
    Ok(())
}

fn is_approved_persistent_image_reference(image: &str) -> bool {
    image == PERSISTENT_BUILDKIT_IMAGE
        || image == format!("docker.io/{PERSISTENT_BUILDKIT_IMAGE}")
        || is_approved_persistent_repo_digest(image)
}

pub(crate) fn is_approved_persistent_repo_digest(digest: &str) -> bool {
    digest == PERSISTENT_BUILDKIT_REPO_DIGEST
        || digest == format!("docker.io/{PERSISTENT_BUILDKIT_REPO_DIGEST}")
}

fn is_safe_buildkit_entrypoint(value: &Value) -> bool {
    value.is_null()
        || value.as_array().is_some_and(|items| {
            items.len() == 1
                && matches!(
                    items[0].as_str(),
                    Some("/usr/bin/buildkitd-entrypoint") | Some("buildkitd")
                )
        })
}

fn is_safe_buildkit_cmd(value: &Value) -> bool {
    value.is_null()
        || value.as_array().is_some_and(|items| {
            items.is_empty()
                || (items.len() == 2
                    && items[0].as_str() == Some("--config")
                    && items[1].as_str() == Some("/etc/buildkit/buildkitd.toml"))
        })
}

fn is_exact_persistent_state_mount(value: &Value, expected_volume: &str) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.keys().any(|key| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "type"
                | "source"
                | "target"
                | "readonly"
                | "consistency"
                | "bindoptions"
                | "volumeoptions"
        )
    }) {
        return false;
    }
    let mount_type = api_object_field(object, "Type").and_then(Value::as_str);
    let source = api_object_field(object, "Source").and_then(Value::as_str);
    let target = api_object_field(object, "Target").and_then(Value::as_str);
    mount_type == Some("volume")
        && source == Some(expected_volume)
        && target == Some("/var/lib/buildkit")
        && api_object_field(object, "ReadOnly").is_none_or(|value| value.as_bool() == Some(false))
        && api_object_field(object, "Consistency")
            .is_none_or(|value| value.as_str().is_some_and(str::is_empty))
        && api_object_field(object, "BindOptions").is_none_or(is_empty_default)
        && api_object_field(object, "VolumeOptions").is_none_or(is_empty_default)
}

fn percent_decode_query_component(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = bytes
                    .get(index + 1)
                    .and_then(|byte| (*byte as char).to_digit(16))
                    .context("Docker query contains an incomplete percent escape")?;
                let low = bytes
                    .get(index + 2)
                    .and_then(|byte| (*byte as char).to_digit(16))
                    .context("Docker query contains an invalid percent escape")?;
                decoded.push(((high << 4) | low) as u8);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).context("Docker query component is not UTF-8")
}

/// Full container create body validation, including the exact
/// `HostConfig`/`CgroupParent` rules the request transform later enforces.
/// Running it here — instead of letting the transform discover the same
/// failure — means a rejected create is answered with a structured 403 deny
/// response; the guest never sees the connection drop mid-request.
fn validate_container_create_request_with_volumes(
    request: &[u8],
    owned_volume_names: &BTreeSet<String>,
) -> Result<()> {
    let body = docker_request_body(request)?;
    let mut value = parse_create_value(body).context("parse Docker container create request")?;
    inject_job_cgroup_parent_value(&mut value, owned_volume_names)
}

/// Buildx's docker-container driver creates exactly one attached exec for
/// `buildctl dial-stdio`. Keep that capability narrow: no arbitrary command,
/// environment, user, working directory, TTY, or detach configuration can
/// be injected into a shared persistent daemon.
fn validate_persistent_exec_create_value(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("Docker persistent BuildKit exec body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "Docker persistent BuildKit exec")?;
    const ALLOWED_FIELDS: [&str; 5] = ["attachstdin", "attachstdout", "attachstderr", "cmd", "tty"];
    for key in object.keys() {
        if !ALLOWED_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
            bail!("Docker persistent BuildKit exec field {key:?} is not permitted");
        }
    }
    for key in ["AttachStdin", "AttachStdout", "AttachStderr"] {
        if object
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| value.as_bool())
            != Some(true)
        {
            bail!("Docker persistent BuildKit exec requires {key}: true");
        }
    }
    if object
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Tty"))
        .and_then(|(_, value)| value.as_bool())
        .is_some_and(|tty| tty)
    {
        bail!("Docker persistent BuildKit exec does not permit a TTY");
    }
    let command = object
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Cmd"))
        .and_then(|(_, value)| value.as_array())
        .context("Docker persistent BuildKit exec must provide Cmd")?;
    if command.len() != 2
        || command[0].as_str() != Some("buildctl")
        || command[1].as_str() != Some("dial-stdio")
    {
        bail!("Docker persistent BuildKit exec command must be buildctl dial-stdio");
    }
    Ok(())
}

fn volume_create_request_name(value: &Value) -> Result<Option<String>> {
    let Some(name) = value.as_object().and_then(|object| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
            .map(|(_, value)| value)
    }) else {
        return Ok(None);
    };
    let name = name
        .as_str()
        .context("Docker volume create Name must be a string")?;
    Ok(Some(validate_owned_resource_id(
        name,
        "Docker volume name",
    )?))
}

/// Denials detected during create-request validation are capability
/// rejections, not proxy failures: report them as `LeaseDeny` so the serve
/// loop writes the JSON status response instead of closing the connection.
fn create_capability_deny(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<LeaseDeny>().is_some() {
        return error;
    }
    LeaseDeny::forbidden(format!("Docker lease denied create request: {error:#}"))
}

fn validate_network_create_value(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("Docker network create body must be a JSON object")?;
    let allowed = [
        "name",
        "driver",
        "checkduplicate",
        "internal",
        "attachable",
        "ingress",
        "enableipv6",
        "ipv6",
        "ipam",
        "options",
        "labels",
    ];
    let mut normalized = BTreeSet::new();
    for key in object.keys() {
        let lower = key.to_ascii_lowercase();
        if !normalized.insert(lower.clone()) {
            bail!("Docker network create contains duplicate case-insensitive field {key:?}");
        }
        if !allowed.contains(&lower.as_str()) {
            bail!("Docker network create field {key:?} is not permitted");
        }
    }

    let name = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
        .map(|(_, value)| value)
        .context("Docker network create must name the network")?
        .as_str()
        .context("Docker network create Name must be a string")?;
    if name.trim() != name {
        bail!("Docker network create Name must not have surrounding whitespace");
    }
    validate_owned_resource_id(name, "Docker network name")?;

    if let Some(driver) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Driver"))
        .map(|(_, value)| value)
        && driver.as_str() != Some("bridge")
    {
        bail!("Docker network create permits only the bridge driver");
    }
    if let Some(check_duplicate) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("CheckDuplicate"))
        .map(|(_, value)| value)
        && !check_duplicate.is_boolean()
    {
        bail!("Docker network create CheckDuplicate must be a boolean");
    }

    for field in ["Internal", "Attachable", "Ingress", "EnableIPv6", "IPv6"] {
        if let Some(value) = object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(field))
            .map(|(_, value)| value)
            && value.as_bool() != Some(false)
        {
            bail!("Docker network create {field} must be false");
        }
    }

    if let Some(options) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Options"))
        .map(|(_, value)| value)
        && !options.as_object().is_some_and(Map::is_empty)
    {
        bail!("Docker network create Options must be empty");
    }
    if let Some(ipam) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("IPAM"))
        .map(|(_, value)| value)
    {
        validate_empty_network_ipam(ipam)?;
    }
    if let Some(labels) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Labels"))
        .map(|(_, value)| value)
    {
        let labels = match labels {
            Value::Null => None,
            Value::Object(labels) => Some(labels),
            _ => bail!("Docker network create Labels must be an object"),
        };
        if let Some(labels) = labels {
            if labels.keys().any(|key| {
                key.eq_ignore_ascii_case(JOB_ID_LABEL) || key.eq_ignore_ascii_case(DAEMON_ID_LABEL)
            }) {
                bail!("Docker network create cannot supply runner ownership labels");
            }
            if labels.values().any(|value| !value.is_string()) {
                bail!("Docker network create label values must be strings");
            }
        }
    }
    Ok(())
}

fn validate_empty_network_ipam(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("Docker network create IPAM must be an object")?;
    let allowed = ["driver", "config", "options"];
    let mut normalized = BTreeSet::new();
    for key in object.keys() {
        let lower = key.to_ascii_lowercase();
        if !normalized.insert(lower.clone()) || !allowed.contains(&lower.as_str()) {
            bail!("Docker network create IPAM field {key:?} is not permitted");
        }
    }
    if let Some(driver) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Driver"))
        .map(|(_, value)| value)
        && !matches!(driver.as_str(), Some("") | Some("default"))
    {
        bail!("Docker network create IPAM driver must be default");
    }
    if let Some(config) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Config"))
        .map(|(_, value)| value)
        && !config.as_array().is_some_and(Vec::is_empty)
    {
        bail!("Docker network create IPAM Config must be empty");
    }
    if let Some(options) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Options"))
        .map(|(_, value)| value)
        && !options.as_object().is_some_and(Map::is_empty)
    {
        bail!("Docker network create IPAM Options must be empty");
    }
    Ok(())
}

/// HostConfig fields that would impose a CPU/RAM/PID ceiling on a
/// job-created container. Nested creates run unbounded like the outer job:
/// these are stripped (whatever their value — absent and zero are the same
/// to Docker), never passed through. `ShmSize` is deliberately absent:
/// shared-memory sizing is not a CPU/RAM ceiling.
const NESTED_QUOTA_HOST_CONFIG_KEYS: [&str; 16] = [
    "cpushares",
    "nanocpus",
    "cpuperiod",
    "cpuquota",
    "cpurealtimeperiod",
    "cpurealtimeruntime",
    "cpucount",
    "cpupercent",
    "cpusetcpus",
    "cpusetmems",
    "memory",
    "memoryreservation",
    "memoryswap",
    "memoryswappiness",
    "oomkilldisable",
    "pidslimit",
];

/// Remove every CPU/RAM/PID ceiling field from a nested container create's
/// `HostConfig`. Case-insensitive like the rest of the lease gate: Docker
/// sends canonical casing, but a guest that misspells the case must not
/// smuggle a ceiling past the strip.
fn strip_nested_quota_controls(host_config: &mut Map<String, Value>) {
    host_config.retain(|key, _| {
        !NESTED_QUOTA_HOST_CONFIG_KEYS.contains(&key.to_ascii_lowercase().as_str())
    });
}

/// Force every job-created Docker container into the runner-owned identity
/// cgroup and strip any CPU/RAM/PID ceiling it requested. The lease proxy
/// is the only Docker socket exposed to a job, so this also covers BuildKit
/// and nested Testcontainers creates. Runner policy wins over a
/// workflow-supplied `HostConfig.CgroupParent`, and unbounded wins over a
/// workflow-supplied ceiling.
fn inject_job_cgroup_parent_value(
    value: &mut Value,
    owned_volume_names: &BTreeSet<String>,
) -> Result<()> {
    let Some(object) = value.as_object_mut() else {
        bail!("Docker container create body must be a JSON object");
    };
    if let Some(alias) = object
        .keys()
        .find(|key| key.as_str() != "HostConfig" && key.eq_ignore_ascii_case("HostConfig"))
    {
        bail!("Docker container create contains ambiguous HostConfig key {alias:?}");
    }
    let host_config = object
        .remove("HostConfig")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut host_config = match host_config {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        other => {
            bail!("Docker container create HostConfig must be an object, got {other}");
        }
    };
    strip_nested_quota_controls(&mut host_config);
    reject_unsafe_nested_host_controls(&host_config, owned_volume_names)?;
    if let Some(alias) = host_config
        .keys()
        .find(|key| key.as_str() != "CgroupParent" && key.eq_ignore_ascii_case("CgroupParent"))
    {
        bail!("Docker container create contains ambiguous CgroupParent key {alias:?}");
    }
    host_config.insert(
        "CgroupParent".into(),
        Value::String(JOB_CGROUP_PARENT.to_owned()),
    );
    object.insert("HostConfig".into(), Value::Object(host_config));
    Ok(())
}

/// Apply only runner identity to an already validated persistent BuildKit
/// bootstrap request. Buildx requires privileged/unless-stopped here; the
/// persistent validator has already rejected every other host-control shape.
fn inject_persistent_bootstrap_value(
    value: &mut Value,
    job_id: &str,
    daemon_id: &str,
    image_id: &str,
) -> Result<()> {
    {
        let object = value
            .as_object_mut()
            .context("persistent BuildKit container body must be an object")?;
        let image_key = object
            .keys()
            .find(|key| key.eq_ignore_ascii_case("Image"))
            .cloned()
            .context("persistent BuildKit container omitted Image")?;
        object.insert(image_key, Value::String(image_id.to_owned()));
        let host_key = object
            .keys()
            .find(|key| key.eq_ignore_ascii_case("HostConfig"))
            .cloned()
            .context("persistent BuildKit container omitted HostConfig")?;
        let host_config = object
            .remove(&host_key)
            .context("persistent BuildKit container omitted HostConfig")?;
        let mut host_config = host_config
            .as_object()
            .cloned()
            .context("persistent BuildKit HostConfig must be an object")?;
        let cgroup_alias = host_config
            .keys()
            .find(|key| key.eq_ignore_ascii_case("CgroupParent"))
            .cloned();
        if let Some(cgroup_alias) = cgroup_alias {
            host_config.remove(&cgroup_alias);
        }
        host_config.insert(
            "CgroupParent".into(),
            Value::String(JOB_CGROUP_PARENT.to_owned()),
        );
        object.insert("HostConfig".into(), Value::Object(host_config));
    }
    inject_ownership_labels_value(value, job_id, daemon_id)
}

fn reject_unsafe_volume_create_value(value: &Value) -> Result<()> {
    let Some(object) = value.as_object() else {
        bail!("Docker volume create body must be a JSON object");
    };
    let mut normalized_keys = BTreeSet::new();
    for key in object.keys() {
        if !normalized_keys.insert(key.to_ascii_lowercase()) {
            bail!("Docker volume create contains duplicate case-insensitive key {key:?}");
        }
    }
    let _ = volume_create_request_name(value)?;
    if let Some((key, driver)) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("driver"))
    {
        let Some(driver) = driver.as_str() else {
            bail!("Docker volume create field {key:?} must be a string");
        };
        if !driver.trim().is_empty() && !driver.eq_ignore_ascii_case("local") {
            bail!("Docker volume create field {key:?} requests host control access");
        }
    }
    if let Some((key, options)) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("driveropts"))
        && is_strict_value_present(options)
    {
        bail!("Docker volume create field {key:?} requests host control access");
    }
    Ok(())
}

fn reject_unsafe_nested_host_controls(
    host_config: &Map<String, Value>,
    owned_volume_names: &BTreeSet<String>,
) -> Result<()> {
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    for (key, value) in host_config {
        let unsafe_control = match key.to_ascii_lowercase().as_str() {
            "networkmode" => !is_guest_network_mode(value),
            "pidmode" | "ipcmode" | "cgroupnsmode" | "usernsmode" | "utsmode" => {
                !is_default_mode(value)
            }
            // Live docker/build-push-action starts BuildKit with Privileged.
            // That is nested engine bootstrap, not a general guest escape.
            // Any other image stays denied.
            // Privileged is admitted only by the exact persistent BuildKit
            // bootstrap validator. Generic guest creates never receive this
            // host capability, regardless of image naming.
            "privileged" => !matches!(value, Value::Null | Value::Bool(false)),
            "capadd" | "devices" | "devicecgrouprules" | "securityopt" | "runtime" | "sysctls"
            | "volumedriver" | "volumesfrom" | "volumeoptions" | "containeridfile" => {
                is_strict_value_present(value)
            }
            // Live API 1.55: `{"Name":"no","MaximumRetryCount":0}` is the
            // default (no restart). BuildKit uses `unless-stopped`.
            // `always` / `on-failure` stay denied.
            "restartpolicy" => !is_guest_restart_policy(value)?,
            // BuildKit's GPU request is an exact, driverless shape. Other
            // device requests stay denied.
            "devicerequests" => !is_guest_device_requests(value)?,
            // Binds are always host paths. Named Type:volume mounts must be
            // in this lease's registry. Persistent BuildKit state is admitted
            // only after its exact current-job builder name has been
            // host-registered and the volume has passed immutable attestation.
            "binds" => !is_empty_mount_list(value),
            "mounts" => !is_guest_or_empty_mounts(value, owned_volume_names)?,
            // Docker's zero value means "unset" for this known field. Do
            // not generalize that exception to future numeric fields.
            "blkioweight" => !is_zero_number(value),
            // API 1.55 CLI sends `ConsoleSize: [0,0]` on every create
            // (live preview.63 probe/backend-parity). That is "no TTY
            // size", not host control. A nonzero size stays denied.
            "consolesize" => !is_default_console_size(value),
            // Quota fields (CpuShares/NanoCpus/CpuPeriod/CpuQuota/
            // CpuRealtime*/CpuCount/CpuPercent/Cpuset*/Memory*/
            // OomKillDisable/PidsLimit) are stripped before this gate runs;
            // any that reach it fall into the strict unknown-field default
            // below instead of passing through. ShmSize stays allowed:
            // shared-memory sizing is not a CPU/RAM ceiling.
            "autoremove" | "cgroupparent" | "readonlyrootfs" | "shmsize" | "init"
            | "stopsignal" | "stoptimeout" | "dns" | "dnsoptions" | "dnssearch" | "extrahosts"
            | "groupadd" | "ulimits" | "maskedpaths" | "readonlypaths" => false,
            // Testcontainers publishes ephemeral host ports (`-P` /
            // PortBindings) so the guest can reach Postgres/Redis/RabbitMQ.
            // The lease still labels every create and reclaims by
            // velnor.job-id. Untrusted jobs never receive this socket.
            // Host-path binds, privileged, and host-network stay denied.
            "portbindings" | "publishallports" => false,
            // Docker CLI serializes unused HostConfig fields as empty
            // defaults (live API 1.55: `BlkioDeviceReadBps: []`,
            // `BlkioWeight: 0`, `ConsoleSize: [0,0]`,
            // `IOMaximumBandwidth: 0`, `DeviceRequests: []`). Only this
            // unknown-field fallback accepts recursively empty values;
            // strict controls above must treat any nonempty container shape
            // as a capability request.
            _ => !is_empty_default(value),
        };
        if unsafe_control {
            bail!(
                "Docker container create HostConfig field {key:?} requests host control access: {value}"
            );
        }
    }
    Ok(())
}

fn is_default_mode(value: &Value) -> bool {
    value.as_str().is_some_and(|mode| {
        let mode = mode.trim();
        mode.is_empty() || mode.eq_ignore_ascii_case("default")
    })
}

/// Guest-isolated Docker network modes. `host` and `container:<id>` stay
/// host-control denies. Live backend-parity uses `--network none`.
/// Parse the exact single-node container name Buildx derives for a Velnor
/// persistent builder. Prefix matching alone would admit a host-managed
/// `buildx_buildkit_*` container, so the embedded builder name must pass the
/// persistent namespace parser too.
fn persistent_buildkit_builder_name(container: &str) -> Option<&str> {
    let builder = container.strip_prefix("buildx_buildkit_")?;
    let builder = builder.strip_suffix('0')?;
    crate::buildkit::is_persistent_builder_name(builder).then_some(builder)
}

fn is_persistent_buildkit_container_name(container: &str) -> bool {
    persistent_buildkit_builder_name(container).is_some()
}

/// A persistent BuildKit state volume is `<container>_state`. Keep this
/// matcher coupled to the exact builder namespace and suffix; a generic named
/// volume never enters the exception.
fn is_persistent_buildkit_volume_name(volume: &str) -> bool {
    persistent_buildkit_volume_builder_name(volume).is_some()
}

fn persistent_buildkit_volume_builder_name(volume: &str) -> Option<&str> {
    let container = volume.strip_suffix("_state")?;
    persistent_buildkit_builder_name(container)
}

fn api_object_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

fn api_object_string<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str> {
    api_object_field(object, name)
        .and_then(Value::as_str)
        .with_context(|| format!("Docker API response omitted string field {name}"))
}

fn parse_api_object(body: &[u8], kind: &str) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(body)
        .with_context(|| format!("parse Docker {kind} inspect response"))?;
    value
        .as_object()
        .cloned()
        .with_context(|| format!("Docker {kind} inspect response must be an object"))
}

/// Docker volume create is idempotent by name. A successful response is an
/// ownership claim only when Docker returned the exact requested name, the
/// local driver, and both labels injected by this lease. This prevents an
/// existing foreign or host-managed volume from entering the registry merely
/// because POST /volumes/create returned 201/200.
fn attest_created_volume_identity(
    body: &[u8],
    expected_name: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<String> {
    let object = parse_api_object(body, "volume create")?;
    let name = api_object_string(&object, "Name")?;
    if name != expected_name {
        bail!(
            "Docker volume create response name mismatch: expected {expected_name:?}, found {name:?}"
        );
    }
    let driver = api_object_string(&object, "Driver")?;
    if driver != "local" {
        bail!("Docker volume {expected_name} has unexpected driver {driver:?}");
    }
    if api_object_field(&object, "Options").is_some_and(|options| !is_empty_default(options)) {
        bail!("Docker volume {expected_name} has unsafe local driver options");
    }
    let labels = api_object_field(&object, "Labels")
        .and_then(Value::as_object)
        .context("Docker volume create response omitted ownership labels")?;
    if labels.get(JOB_ID_LABEL).and_then(Value::as_str) != Some(job_id)
        || labels.get(DAEMON_ID_LABEL).and_then(Value::as_str) != Some(daemon_id)
    {
        bail!("Docker volume {expected_name} ownership labels do not match the lease");
    }
    validate_owned_resource_id(name, "Docker volume name")
}

/// Attest the shared BuildKit state volume path exposed by /volumes/<name>.
/// The job label may belong to an earlier holder because this is a shared
/// persistent builder; the daemon label must still bind it to this Docker
/// endpoint, and the exact host-registered builder state name is checked.
fn attest_persistent_buildkit_volume(
    body: &[u8],
    expected_name: &str,
    daemon_id: &str,
    allowed_builders: &BTreeSet<String>,
) -> Result<()> {
    let object = parse_api_object(body, "volume")?;
    let identity = VolumeIdentity {
        name: api_object_string(&object, "Name")?.to_owned(),
        driver: api_object_string(&object, "Driver")?.to_owned(),
        labels: serde_json::from_value(
            api_object_field(&object, "Labels")
                .cloned()
                .context("Docker persistent BuildKit volume omitted ownership labels")?,
        )
        .context("Docker persistent BuildKit volume ownership labels must be strings")?,
        options: api_object_field(&object, "Options")
            .map(|options| {
                serde_json::from_value::<Option<BTreeMap<String, String>>>(options.clone())
                    .context("Docker persistent BuildKit volume options must be strings")
            })
            .transpose()?
            .flatten(),
    };
    attest_persistent_buildkit_volume_identity(
        &identity,
        expected_name,
        daemon_id,
        allowed_builders,
    )
}

/// Attest the same persistent BuildKit state volume projection used by the
/// host-side `docker volume inspect --format` command. Its four fields are
/// JSON values separated by tabs; keeping this parser separate from the
/// Docker HTTP object parser makes the command boundary explicit.
fn attest_persistent_buildkit_volume_projection(
    output: &str,
    expected_name: &str,
    daemon_id: &str,
    allowed_builders: &BTreeSet<String>,
) -> Result<()> {
    let identity = parse_volume_identity(output)?;
    attest_persistent_buildkit_volume_identity(
        &identity,
        expected_name,
        daemon_id,
        allowed_builders,
    )
}

fn attest_persistent_buildkit_volume_identity(
    identity: &VolumeIdentity,
    expected_name: &str,
    daemon_id: &str,
    allowed_builders: &BTreeSet<String>,
) -> Result<()> {
    let Some(builder) = persistent_buildkit_volume_builder_name(expected_name) else {
        bail!("Docker volume {expected_name} is outside the persistent BuildKit namespace");
    };
    if !allowed_builders.contains(builder) {
        bail!("Docker volume {expected_name} is not the current job's BuildKit state volume");
    }
    if identity.name != expected_name {
        bail!(
            "Docker volume inspect name mismatch: expected {expected_name:?}, found {:?}",
            identity.name
        );
    }
    if identity.driver != "local" {
        bail!("Docker volume {expected_name} is not local");
    }
    if identity
        .options
        .as_ref()
        .is_some_and(|options| !options.is_empty())
    {
        bail!("Docker persistent BuildKit volume has unsafe local driver options");
    }
    if identity
        .labels
        .get(JOB_ID_LABEL)
        .is_none_or(String::is_empty)
    {
        bail!("Docker persistent BuildKit volume omitted its job ownership label");
    }
    if identity.labels.len() != 2
        || identity
            .labels
            .keys()
            .any(|key| key != JOB_ID_LABEL && key != DAEMON_ID_LABEL)
    {
        bail!("Docker persistent BuildKit volume has unexpected ownership labels");
    }
    if identity.labels.get(DAEMON_ID_LABEL).map(String::as_str) != Some(daemon_id) {
        bail!("Docker persistent BuildKit volume daemon ownership label mismatch");
    }
    Ok(())
}

/// Attest a reused docker-container BuildKit daemon before granting its
/// start/wait operations. Its name, BuildKit image, lease labels, bridge
/// network, and exact expected state-volume mount all bind the object to
/// Velnor's persistent-builder contract. The volume itself is re-attested
/// separately before a start or guest mount.
fn attest_persistent_buildkit_container(
    body: &[u8],
    target: &str,
    daemon_id: &str,
    allowed_builders: &BTreeSet<String>,
    approved_images: &BTreeMap<String, String>,
) -> Result<(String, String, String, String)> {
    let object = parse_api_object(body, "container")?;
    let id = validate_owned_resource_id(api_object_string(&object, "Id")?, "Docker container ID")?;
    let raw_name = api_object_string(&object, "Name")?;
    let name = validate_owned_resource_id(
        raw_name.strip_prefix('/').unwrap_or(raw_name),
        "Docker container name",
    )?;
    let Some(builder) = persistent_buildkit_builder_name(&name) else {
        bail!("Docker container {name} is outside the persistent BuildKit namespace");
    };
    if !allowed_builders.contains(builder) {
        bail!("Docker container {name} is not the current job's BuildKit builder");
    }
    if is_persistent_buildkit_container_name(target) && name != target {
        bail!("Docker persistent BuildKit container name mismatch");
    }
    if !is_persistent_buildkit_container_name(target) && id != target {
        bail!("Docker persistent BuildKit container ID mismatch");
    }
    let config = api_object_field(&object, "Config")
        .and_then(Value::as_object)
        .context("Docker persistent BuildKit container omitted Config")?;
    validate_attested_persistent_container_config(config)?;
    let image = api_object_string(config, "Image")?;
    if !is_approved_persistent_image_reference(image)
        && approved_images
            .get(builder)
            .is_none_or(|approved| approved != image)
    {
        bail!("Docker persistent BuildKit container image is not host-approved");
    }
    let image_id = validate_owned_resource_id(
        api_object_string(&object, "Image")?,
        "Docker persistent BuildKit image ID",
    )?;
    if !image_id.starts_with("sha256:") {
        bail!("Docker persistent BuildKit container image ID is not immutable");
    }
    if approved_images.get(builder) != Some(&image_id) {
        bail!("Docker persistent BuildKit container image ID is not the host-approved image");
    }
    if let Some(env) = api_object_field(config, "Env")
        && !env
            .as_array()
            .is_some_and(|values| is_safe_buildkit_env(values))
    {
        bail!("Docker persistent BuildKit container environment is not approved");
    }
    if let Some(entrypoint) = api_object_field(config, "Entrypoint")
        && !is_safe_buildkit_entrypoint(entrypoint)
    {
        bail!("Docker persistent BuildKit container entrypoint is not approved");
    }
    if let Some(cmd) = api_object_field(config, "Cmd")
        && !is_safe_buildkit_cmd(cmd)
    {
        bail!("Docker persistent BuildKit container command is not approved");
    }
    let labels = api_object_field(config, "Labels")
        .and_then(Value::as_object)
        .context("Docker persistent BuildKit container omitted ownership labels")?;
    if labels.len() != 2
        || labels
            .keys()
            .any(|key| key != JOB_ID_LABEL && key != DAEMON_ID_LABEL)
    {
        bail!("Docker persistent BuildKit container has unexpected labels");
    }
    if labels
        .get(JOB_ID_LABEL)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || labels.get(DAEMON_ID_LABEL).and_then(Value::as_str) != Some(daemon_id)
    {
        bail!("Docker persistent BuildKit container ownership labels are invalid");
    }
    let host_config = api_object_field(&object, "HostConfig")
        .and_then(Value::as_object)
        .context("Docker persistent BuildKit container omitted HostConfig")?;
    validate_attested_persistent_host_config(host_config)?;
    let network = api_object_field(host_config, "NetworkMode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(network, "" | "default" | "bridge") {
        bail!("Docker persistent BuildKit container uses an unsafe network mode");
    }
    if api_object_field(host_config, "Privileged").and_then(Value::as_bool) != Some(true) {
        bail!("Docker persistent BuildKit container must be privileged");
    }
    if api_object_field(host_config, "Init").and_then(Value::as_bool) != Some(true) {
        bail!("Docker persistent BuildKit container must enable init");
    }
    if !is_named_restart_policy(
        api_object_field(host_config, "RestartPolicy")
            .context("Docker persistent BuildKit container omitted RestartPolicy")?,
        &["unless-stopped"],
    )? {
        bail!("Docker persistent BuildKit container restart policy is not approved");
    }
    let expected_volume = crate::buildkit::daemon_state_volume(builder);
    let mounts = api_object_field(&object, "Mounts")
        .and_then(Value::as_array)
        .context("Docker persistent BuildKit container omitted Mounts")?;
    if mounts.len() != 1 || !is_exact_persistent_inspect_mount(&mounts[0], &expected_volume) {
        bail!("Docker persistent BuildKit container has an unsafe state-volume mount set");
    }
    Ok((name, id, expected_volume, image_id))
}

fn is_safe_buildkit_env(values: &[Value]) -> bool {
    values.iter().all(|value| {
        value.as_str().is_some_and(|entry| {
            // These are the immutable defaults of the pinned BuildKit image.
            // A caller-controlled environment entry must never be accepted
            // merely because it happens to be attached to a BuildKit name.
            entry == "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                || entry == "BUILDKIT_SETUP_CGROUPV2_ROOT=1"
        })
    })
}

/// Docker returns the image's complete `Config` on inspect. A reused
/// container can therefore bypass the current create validator, so keep the
/// whole config schema closed here. Only the pinned image's inert defaults,
/// approved command/entrypoint/environment, and lease labels are accepted.
fn validate_attested_persistent_container_config(config: &Map<String, Value>) -> Result<()> {
    reject_case_insensitive_duplicate_keys(config, "persistent BuildKit Config")?;
    for (key, value) in config {
        let safe = match key.to_ascii_lowercase().as_str() {
            "hostname" => value.as_str().is_some(),
            "domainname" | "user" | "workingdir" | "macaddress" => {
                value.as_str().is_some_and(str::is_empty)
            }
            "attachstdin" | "attachstdout" | "attachstderr" | "tty" | "openstdin" | "stdinonce"
            | "networkdisabled" | "argsescaped" => value.as_bool() == Some(false),
            "env" => {
                value.is_null()
                    || value
                        .as_array()
                        .is_some_and(|values| is_safe_buildkit_env(values))
            }
            "cmd" => is_safe_buildkit_cmd(value),
            "entrypoint" => is_safe_buildkit_entrypoint(value),
            "image" => value.as_str().is_some(),
            "volumes" | "exposedports" | "onbuild" => is_empty_default(value),
            // A non-null Healthcheck can execute an attacker-controlled
            // command inside this privileged shared daemon. Docker's normal
            // no-healthcheck representation is JSON null.
            "healthcheck" => value.is_null(),
            "labels" => value.as_object().is_some(),
            "stopsignal" => value
                .as_str()
                .is_some_and(|signal| signal.is_empty() || signal.eq_ignore_ascii_case("SIGTERM")),
            "stoptimeout" => value.is_null() || is_zero_number(value),
            "shell" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
            _ => false,
        };
        if !safe {
            bail!("Docker persistent BuildKit Config field {key:?} is unsafe");
        }
    }
    Ok(())
}

/// Inspect responses are independently attested. A legacy or host-created
/// container can bypass the current create validator, so every HostConfig
/// field that can add host control is checked again here.
fn validate_attested_persistent_host_config(host_config: &Map<String, Value>) -> Result<()> {
    reject_case_insensitive_duplicate_keys(host_config, "persistent BuildKit HostConfig")?;
    for (key, value) in host_config {
        let normalized = key.to_ascii_lowercase();
        let safe = match normalized.as_str() {
            "networkmode" => value.as_str().is_some_and(|mode| {
                matches!(
                    mode.trim().to_ascii_lowercase().as_str(),
                    "" | "default" | "bridge"
                )
            }),
            "privileged" => value.as_bool() == Some(true),
            "init" => value.as_bool() == Some(true),
            "restartpolicy" => is_named_restart_policy(value, &["unless-stopped"])?,
            "mounts" => true,
            "cgroupparent" => value.as_str().is_some_and(|parent| {
                parent.is_empty() || parent == JOB_CGROUP_PARENT || parent == "/docker/buildx"
            }),
            "pidmode" | "ipcmode" | "utsmode" => is_default_mode(value),
            "usernsmode" => value.as_str().is_some_and(|mode| {
                let mode = mode.trim();
                mode.is_empty() || mode.eq_ignore_ascii_case("default") || mode == "host"
            }),
            "cgroupnsmode" => value.as_str().is_some_and(|mode| {
                matches!(
                    mode.trim().to_ascii_lowercase().as_str(),
                    "" | "default" | "private"
                )
            }),
            "runtime" => value
                .as_str()
                .is_some_and(|runtime| runtime.is_empty() || runtime.eq_ignore_ascii_case("runc")),
            "readonlyrootfs" | "autoremove" | "publishallports" => value.as_bool() == Some(false),
            "binds" | "capadd" | "capdrop" | "devices" | "devicecgrouprules" | "securityopt"
            | "volumesfrom" | "volumeoptions" | "portbindings" | "links" | "dns" | "dnsoptions"
            | "dnssearch" | "extrahosts" | "groupadd" | "sysctls" | "storageopt"
            | "maskedpaths" | "readonlypaths" => is_empty_default(value),
            "volumedriver" | "containeridfile" => is_empty_default(value),
            "logconfig" => is_safe_persistent_log_config(value),
            "isolation" => value
                .as_str()
                .is_some_and(|mode| mode.is_empty() || mode.eq_ignore_ascii_case("default")),
            // Resource ceilings are not needed by Buildx's persistent
            // daemon. They must remain at Docker's zero defaults.
            "cpushares" | "cpuquota" | "cpuperiod" | "cpurealtimeperiod" | "cpurealtimeruntime"
            | "cpucount" | "cpupercent" | "memory" | "memoryreservation" | "memoryswap"
            | "memoryswappiness" | "oomkilldisable" | "pidslimit" | "blkioweight"
            | "oomscoreadj" => is_empty_default(value),
            "shmsize" => matches!(value.as_i64(), Some(0) | Some(67_108_864)),
            _ => is_empty_default(value),
        };
        if !safe {
            bail!("Docker persistent BuildKit HostConfig field {key:?} is unsafe");
        }
    }
    Ok(())
}

fn is_safe_persistent_log_config(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return value.is_null();
    };
    let Some(log_type) = api_object_field(object, "Type").and_then(Value::as_str) else {
        return object.is_empty();
    };
    log_type.is_empty()
        || log_type.eq_ignore_ascii_case("json-file")
            && api_object_field(object, "Config").is_none_or(is_empty_default)
}

fn is_exact_persistent_inspect_mount(value: &Value, expected_volume: &str) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.keys().any(|key| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "type"
                | "name"
                | "destination"
                | "source"
                | "driver"
                | "mode"
                | "rw"
                | "propagation"
                | "nonrecursive"
                | "id"
        )
    }) {
        return false;
    }
    api_object_field(object, "Type").and_then(Value::as_str) == Some("volume")
        && api_object_field(object, "Name").and_then(Value::as_str) == Some(expected_volume)
        && api_object_field(object, "Destination").and_then(Value::as_str)
            == Some("/var/lib/buildkit")
        && api_object_field(object, "Driver")
            .is_none_or(|value| value.as_str().is_some_and(|driver| driver == "local"))
        && api_object_field(object, "RW").is_none_or(|value| value.as_bool() == Some(true))
        && api_object_field(object, "Mode")
            .is_none_or(|value| value.as_str().is_some_and(str::is_empty))
        && api_object_field(object, "Propagation").is_none_or(|value| {
            value
                .as_str()
                .is_some_and(|propagation| propagation.is_empty() || propagation == "rprivate")
        })
        && api_object_field(object, "NonRecursive")
            .is_none_or(|value| value.as_bool() == Some(false))
}

fn is_guest_network_mode(value: &Value) -> bool {
    value.as_str().is_some_and(|mode| {
        let mode = mode.trim();
        mode.is_empty()
            || mode.eq_ignore_ascii_case("default")
            || mode.eq_ignore_ascii_case("none")
            || mode.eq_ignore_ascii_case("bridge")
    })
}

fn is_zero_number(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.as_i64() == Some(0) || number.as_u64() == Some(0) || number.as_f64() == Some(0.0)
        }
        _ => false,
    }
}

fn is_empty_mount_list(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn is_guest_or_empty_mounts(value: &Value, owned_volume_names: &BTreeSet<String>) -> Result<bool> {
    let Value::Array(items) = value else {
        return Ok(value.is_null());
    };
    if items.is_empty() {
        return Ok(true);
    }
    for item in items {
        if !is_guest_named_volume_mount(item, owned_volume_names)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn volume_source_is_host_path(path: &str) -> bool {
    let path = path.trim();
    path.starts_with('/')
        || path == "."
        || path == ".."
        || path.starts_with("./")
        || path.starts_with("../")
        || path == "~"
        || path.starts_with("~/")
}

fn is_guest_named_volume_mount(
    value: &Value,
    owned_volume_names: &BTreeSet<String>,
) -> Result<bool> {
    let Value::Object(object) = value else {
        return Ok(false);
    };
    reject_case_insensitive_duplicate_keys(object, "Docker mount object")?;
    const ALLOWED_FIELDS: [&str; 5] = ["type", "source", "target", "readonly", "consistency"];
    for key in object.keys() {
        if !ALLOWED_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
            bail!("Docker volume mount field {key:?} is not permitted");
        }
    }
    let mount_type = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("type"))
        .and_then(|(_, value)| value.as_str());
    if mount_type != Some("volume") {
        return Ok(false);
    }
    let Some(source) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("source"))
        .and_then(|(_, value)| value.as_str())
    else {
        return Ok(false);
    };
    if source.is_empty() || volume_source_is_host_path(source) {
        return Ok(false);
    }
    let Some(target) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("target"))
        .and_then(|(_, value)| value.as_str())
    else {
        return Ok(false);
    };
    if !target.starts_with('/') || target.contains('\0') {
        return Ok(false);
    }
    let owned = owned_volume_names.contains(source);
    if !owned {
        return Ok(false);
    }
    if let Some(read_only) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("readonly"))
        .map(|(_, value)| value)
        && !read_only.is_boolean()
    {
        return Ok(false);
    }
    if let Some(consistency) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("consistency"))
        .map(|(_, value)| value)
        && !consistency.is_string()
    {
        return Ok(false);
    }
    Ok(true)
}

fn is_default_restart_policy(value: &Value) -> Result<bool> {
    is_named_restart_policy(value, &["", "no", "none"])
}

fn is_guest_restart_policy(value: &Value) -> Result<bool> {
    is_default_restart_policy(value)
}

fn is_named_restart_policy(value: &Value, allowed_names: &[&str]) -> Result<bool> {
    match value {
        Value::Null => Ok(allowed_names.iter().any(|name| name.is_empty())),
        Value::Object(object) => {
            reject_case_insensitive_duplicate_keys(object, "Docker RestartPolicy")?;
            const ALLOWED_FIELDS: [&str; 2] = ["name", "maximumretrycount"];
            for key in object.keys() {
                if !ALLOWED_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
                    bail!("Docker RestartPolicy field {key:?} is not permitted");
                }
            }
            let name = object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("name"))
                .map(|(_, value)| value);
            let name = match name {
                None => "",
                Some(value) => match value.as_str() {
                    Some(name) => name,
                    None => return Ok(false),
                },
            };
            let retries = object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("maximumretrycount"))
                .map(|(_, value)| value)
                .is_none_or(|value| value.as_i64() == Some(0) || value.as_u64() == Some(0));
            Ok(allowed_names
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
                && retries)
        }
        _ => Ok(false),
    }
}

fn is_guest_device_requests(value: &Value) -> Result<bool> {
    if value.is_null() {
        return Ok(true);
    }
    let Value::Array(items) = value else {
        return Ok(false);
    };
    for item in items {
        if !is_buildkit_device_request(item)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_buildkit_device_request(value: &Value) -> Result<bool> {
    let Value::Object(object) = value else {
        return Ok(false);
    };
    reject_case_insensitive_duplicate_keys(object, "Docker DeviceRequest")?;
    const ALLOWED_FIELDS: [&str; 5] = ["driver", "count", "deviceids", "capabilities", "options"];
    for key in object.keys() {
        if !ALLOWED_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
            bail!("Docker DeviceRequest field {key:?} is not permitted");
        }
    }
    let Some(driver) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("driver"))
        .and_then(|(_, value)| value.as_str())
    else {
        return Ok(false);
    };
    if !driver.is_empty() {
        return Ok(false);
    }
    let Some(count) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("count"))
        .and_then(|(_, value)| value.as_i64())
    else {
        return Ok(false);
    };
    let Some(ids) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("deviceids"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    let ids_empty =
        matches!(ids, Value::Null) || matches!(ids, Value::Array(values) if values.is_empty());
    if !ids_empty {
        return Ok(false);
    }
    let Some(capabilities) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("capabilities"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    let Some(options) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("options"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    if !options.as_object().is_some_and(Map::is_empty) {
        return Ok(false);
    }
    let empty_request =
        count == 0 && matches!(capabilities, Value::Array(values) if values.is_empty());
    let buildkit_gpu_request = count == -1 && is_buildkit_gpu_capabilities(capabilities);
    Ok(empty_request || buildkit_gpu_request)
}

fn is_buildkit_gpu_capabilities(value: &Value) -> bool {
    let Value::Array(groups) = value else {
        return false;
    };
    groups.len() == 1
        && matches!(&groups[0], Value::Array(names) if names.len() == 1
            && names[0].as_str() == Some("gpu"))
}

fn is_default_console_size(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) if values.is_empty() => true,
        Value::Array(values) if values.len() == 2 => values.iter().all(is_zero_number),
        _ => false,
    }
}

fn is_empty_default(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !*value,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.iter().all(is_empty_default),
        Value::Object(object) => object.values().all(is_empty_default),
        Value::Number(_) => is_zero_number(value),
    }
}

fn is_strict_value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(object) => !object.is_empty(),
        Value::Number(_) => true,
    }
}

fn reject_case_insensitive_duplicate_keys(
    object: &Map<String, Value>,
    description: &str,
) -> Result<()> {
    let mut normalized_keys = BTreeSet::new();
    for key in object.keys() {
        if !normalized_keys.insert(key.to_ascii_lowercase()) {
            bail!("{description} contains duplicate case-insensitive key {key:?}");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn without_expect_continue(request: &[u8]) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header =
        std::str::from_utf8(&request[..header_end]).context("Docker API headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut out = Vec::new();
    out.extend_from_slice(request_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line
            .split_once(':')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("expect"))
        {
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&request[header_end..]);
    Ok(out)
}

/// Terminal cleanup's list phase: every object carrying `velnor.job-id`.
/// Listed through the Engine-routed facade; the remove phase consumes it.
pub struct JobOwnedSnapshot {
    pub containers: Vec<docker_client::OwnedContainer>,
    pub networks: Vec<String>,
    pub volumes: Vec<String>,
}

/// Outcome of the two-snapshot startup reclaim gate. A live job is a safety
/// boundary, not a successful cleanup: callers must preserve its immutable
/// handles and stop before resetting the retry generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleJobReclaim {
    Reclaimed,
    ProtectedLive,
    Unknown,
}

/// List the three owned kinds through the Engine-routed facade: one API
/// call each on the fast path, the historical `ps`/`network ls`/`volume ls`
/// on fallback.
///
/// Terminal cleanup runs this before [`remove_job_owned`]: two phases where
/// the old closure version interleaved six calls. The borrow checker forces
/// the shape — one `&mut` runner cannot serve a live listing facade and a
/// removal closure at once — and terminal cleanup owns the job outright (its
/// containers are already gone; this path never revalidated between list and
/// rm), so batching changes no decision, only the call order.
pub fn list_job_owned(
    job_id: &str,
    docker: &mut docker_client::Docker<'_>,
) -> Result<JobOwnedSnapshot> {
    Ok(JobOwnedSnapshot {
        containers: docker.list_owned_containers(job_id)?,
        networks: docker.list_owned_networks(job_id)?,
        volumes: docker.list_owned_volumes(job_id)?,
    })
}

/// Terminal cleanup's remove phase: force-remove the snapshot's objects,
/// skipping empty kinds. Removals stay CLI — mutations are out of the
/// Engine migration's scope — through the same tolerant cleanup runner the
/// closure version used.
pub fn remove_job_owned(
    snapshot: &JobOwnedSnapshot,
    mut remove: impl FnMut(&[String]) -> Result<()>,
) -> Result<()> {
    let ids = docker_client::owned_container_ids_excluding_buildkit_rows(&snapshot.containers);
    if !ids.is_empty() {
        remove(&force_remove_container_args(&ids))?;
    }
    if !snapshot.networks.is_empty() {
        remove(&force_remove_network_args(&snapshot.networks))?;
    }
    // Persistent BuildKit state is shared across jobs. Claim release and the
    // dedicated BuildKit cleanup own those names; generic job teardown must
    // never remove them just because the creating job label is present.
    let volumes = snapshot
        .volumes
        .iter()
        .filter(|volume| !crate::buildkit::is_persistent_builder_object(volume))
        .cloned()
        .collect::<Vec<_>>();
    if !volumes.is_empty() {
        remove(&force_remove_volume_args(&volumes))?;
    }
    Ok(())
}

/// Reclaim resources left by a failed startup attempt without deleting a
/// container that may have become live. This path is deliberately distinct
/// from terminal cleanup: terminal cleanup owns the job and must remove its
/// running guest containers, while retry cleanup only accepts structurally
/// valid snapshots whose job container is absent or known non-live, with that
/// condition revalidated immediately before DELETE.
pub fn reclaim_stale_job_owned(
    job_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let list_args = list_owned_containers_state_args(job_id);
    let initial = docker(&list_args)?;
    let Some(initial) = docker_client::stale_job_owned_snapshot(job_id, &initial) else {
        return Ok(());
    };
    if !initial.job_container_absent_or_stopped {
        return Ok(());
    }
    let revalidated = docker(&list_args)?;
    let Some(revalidated) = docker_client::stale_job_owned_snapshot(job_id, &revalidated) else {
        return Ok(());
    };
    if !revalidated.job_container_absent_or_stopped {
        return Ok(());
    }
    let ids = initial
        .stopped_container_ids
        .into_iter()
        .filter(|id| revalidated.stopped_container_ids.binary_search(id).is_ok())
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }
    reclaim_listed(
        &list_owned_networks_args(job_id),
        &mut docker,
        force_remove_network_args,
    )?;
    reclaim_listed_non_persistent_volumes(job_id, &list_owned_volumes_args(job_id), &mut docker)?;
    Ok(())
}

/// Startup-retry variant of [`reclaim_stale_job_owned`] that removes only
/// stopped, revalidated containers. The caller owns the immutable network and
/// service handles and must attest those objects separately before deletion;
/// a label-only network/volume sweep here would erase an object whose create
/// response was lost during a partial start.
pub fn reclaim_stale_job_owned_containers(
    job_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<StaleJobReclaim> {
    let list_args = list_owned_containers_state_args(job_id);
    let initial = docker(&list_args)?;
    let Some(initial) = docker_client::stale_job_owned_snapshot(job_id, &initial) else {
        return Ok(StaleJobReclaim::Unknown);
    };
    if !initial.job_container_absent_or_stopped {
        return Ok(StaleJobReclaim::ProtectedLive);
    }
    let revalidated = docker(&list_args)?;
    let Some(revalidated) = docker_client::stale_job_owned_snapshot(job_id, &revalidated) else {
        return Ok(StaleJobReclaim::Unknown);
    };
    if !revalidated.job_container_absent_or_stopped {
        return Ok(StaleJobReclaim::ProtectedLive);
    }
    let ids = initial
        .stopped_container_ids
        .into_iter()
        .filter(|id| revalidated.stopped_container_ids.binary_search(id).is_ok())
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }
    Ok(StaleJobReclaim::Reclaimed)
}

/// Force-remove every container carrying `velnor.job-id=<job_id>`, running or
/// stopped. Stale in-flight recovery calls this before restoring the slot to
/// Ready: the worker is provably dead, so a still-running container can only
/// be a leak — the stopped-only reclaim above would leave it burning CPU
/// behind a Ready slot. Already-gone containers are success; only this job's
/// exact label is ever listed or removed, so other jobs' containers are
/// untouched. BuildKit daemon rows go one `rm` at a time (batched Engine
/// deletes of Created/removing BuildKit deadlock); persistent builders are
/// excluded by the parser because other jobs share them.
pub fn force_remove_job_owned_containers(
    job_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let listed = docker(&list_owned_containers_state_args(job_id))?;
    let Some(ids) = docker_client::job_owned_container_ids(job_id, &listed) else {
        bail!(
            "refusing to force-remove containers for job {job_id}: ownership listing failed closed"
        );
    };
    if !ids.containers.is_empty() {
        match docker(&force_remove_container_args(&ids.containers)) {
            Ok(_) => {}
            Err(error) if docker_client::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if !ids.buildkit.is_empty() {
        force_remove_containers_serially(&ids.buildkit, |args| match docker(args) {
            Ok(_) => Ok(()),
            Err(error) if docker_client::is_not_found(&error) => Ok(()),
            Err(error) => Err(error),
        })?;
    }
    Ok(())
}

pub fn reclaim_orphan_jobs(mut docker: impl FnMut(&[String]) -> Result<String>) -> Result<()> {
    #[cfg(unix)]
    let host_socket = crate::docker::engine::resolve_docker_endpoint()
        .context("resolve Docker endpoint for orphan-volume lock")?
        .socket;
    #[cfg(unix)]
    let mut volume_locks = BTreeMap::new();
    let mut call = |args: &[String]| {
        #[cfg(unix)]
        if let Some(volume) = host_volume_mutation_target(args)
            && !volume_locks.contains_key(volume)
        {
            volume_locks.insert(
                volume.to_owned(),
                lock_host_volume_name(&host_socket, volume)?,
            );
        }
        docker(args)
    };
    let formatted = call(&list_owned_job_format_args())?;
    for job_id in docker_client::orphan_job_ids(&formatted) {
        reclaim_stale_job_owned(&job_id, &mut call)?;
    }
    reclaim_orphan_job_buildkit(&formatted, None, &mut call)
}

/// Daemon-scoped variant of [`reclaim_orphan_jobs`] for daemon startup: only
/// containers whose `velnor.daemon-id` label belongs to THIS daemon are
/// considered, so co-located daemons never reclaim each other's jobs. Used at
/// boot to reclaim precreated job-environment containers (and their guest
/// siblings) orphaned by a drain/restart — previously only manual `doctor`
/// runs reclaimed them (tailrocks/velnor#311).
pub fn reclaim_daemon_orphan_jobs(
    daemon_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    #[cfg(unix)]
    let host_socket = crate::docker::engine::resolve_docker_endpoint()
        .context("resolve Docker endpoint for daemon-orphan volume lock")?
        .socket;
    #[cfg(unix)]
    let mut volume_locks = BTreeMap::new();
    let mut call = |args: &[String]| {
        #[cfg(unix)]
        if let Some(volume) = host_volume_mutation_target(args)
            && !volume_locks.contains_key(volume)
        {
            volume_locks.insert(
                volume.to_owned(),
                lock_host_volume_name(&host_socket, volume)?,
            );
        }
        docker(args)
    };
    let formatted = call(&list_daemon_owned_job_format_args())?;
    for job_id in docker_client::daemon_orphan_job_ids(&formatted, daemon_id) {
        reclaim_stale_job_owned(&job_id, &mut call)?;
    }
    let live = docker_client::live_daemon_job_ids(&formatted, daemon_id);
    reclaim_orphan_job_buildkit_with_live(&live, Some(daemon_id), &mut call)
}

pub fn reclaim_unlabeled_testcontainers(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_testcontainers_format_args())?;
    docker_client::validate_legacy_testcontainer_listing(&formatted).map_err(Into::into)
}

pub fn reclaim_unlabeled_job_image_siblings(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let mut ids = docker_client::unlabeled_job_image_ids(&docker(&list_job_image_format_args())?);
    ids.extend(docker_client::unlabeled_job_image_ids(&docker(
        &list_preflight_format_args(),
    )?));
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    docker(&force_remove_container_args(&ids)).map(|_| ())
}

fn reclaim_listed(
    list_args: &[String],
    docker: &mut impl FnMut(&[String]) -> Result<String>,
    remove_args: fn(&[String]) -> Vec<String>,
) -> Result<()> {
    let listed = docker(list_args)?;
    let ids = docker_client::parse_id_list(&listed);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&remove_args(&ids)).map(|_| ())
}

fn reclaim_listed_non_persistent_volumes(
    job_id: &str,
    list_args: &[String],
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let listed = docker(list_args)?;
    let names = docker_client::parse_id_list(&listed)
        .into_iter()
        .filter(|name| !crate::buildkit::is_persistent_builder_object(name))
        .collect::<Vec<_>>();
    for name in names {
        // The initial label-filtered list is discovery only. Re-list this
        // exact name immediately before each inspect so a replacement that
        // appeared after the scan cannot be treated as the old volume.
        let current = docker(list_args)?;
        if !docker_client::parse_id_list(&current)
            .iter()
            .any(|id| id == &name)
        {
            continue;
        }
        let inspected = match docker(&inspect_volume_identity_args(&name)) {
            Ok(inspected) => inspected,
            Err(error) if docker_client::is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        if attest_volume_identity(&inspected, &name, job_id, None).is_err() {
            continue;
        }
        // Re-list and re-inspect directly before removal. Docker volume rm
        // accepts only the name, so these two checks are the ownership guard
        // against a same-name replacement between inspect and rm.
        let current = docker(list_args)?;
        if !docker_client::parse_id_list(&current)
            .iter()
            .any(|id| id == &name)
        {
            continue;
        }
        let reattested = match docker(&inspect_volume_identity_args(&name)) {
            Ok(reattested) => reattested,
            Err(error) if docker_client::is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        if attest_volume_identity(&reattested, &name, job_id, None).is_err() {
            continue;
        }
        // Startup reclaim begins from a stopped snapshot, but the owner can
        // restart between that snapshot and this name-based delete. Refresh
        // the owner listing immediately before every legacy state-volume rm;
        // a live owner wins and all remaining volumes stay untouched.
        let owner_listing = docker(&list_owned_job_format_args())?;
        if docker_client::live_job_ids(&owner_listing).contains(job_id) {
            return Ok(());
        }
        match docker(&remove_volume_args(std::slice::from_ref(&name))) {
            Ok(_) => {}
            Err(error) if docker_client::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub struct DockerLeaseGuard {
    listen_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    policy: Arc<DockerLeasePolicy>,
    #[cfg(unix)]
    conns: Arc<LeaseConnSet>,
    #[cfg(unix)]
    shutdown_wake: Option<std::os::unix::net::UnixStream>,
}

/// Live guest/host unix streams for one job lease. Drop aborts them so an
/// in-flight Engine `POST /containers/{id}/start` cannot pin Created BuildKit
/// behind a lock that `docker rm --force` never wins.
#[cfg(unix)]
struct LeaseConnSet {
    shutdown: Arc<AtomicBool>,
    connection_count: std::sync::atomic::AtomicUsize,
    buffered_bytes: std::sync::atomic::AtomicUsize,
    next_id: Mutex<u64>,
    streams: Mutex<BTreeMap<u64, std::os::unix::net::UnixStream>>,
}

#[cfg(unix)]
struct WatchedStream {
    set: Arc<LeaseConnSet>,
    id: Option<u64>,
}

#[cfg(unix)]
impl LeaseConnSet {
    fn new(shutdown: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            shutdown,
            connection_count: std::sync::atomic::AtomicUsize::new(0),
            buffered_bytes: std::sync::atomic::AtomicUsize::new(0),
            next_id: Mutex::new(0),
            streams: Mutex::new(BTreeMap::new()),
        })
    }

    fn try_acquire_connection(self: &Arc<Self>) -> Option<LeaseConnectionPermit> {
        let mut current = self.connection_count.load(Ordering::Acquire);
        loop {
            if current >= MAX_LEASE_CONNECTIONS || self.is_shutdown() {
                return None;
            }
            match self.connection_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(LeaseConnectionPermit(Arc::clone(self))),
                Err(observed) => current = observed,
            }
        }
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn try_acquire_bytes(&self, bytes: usize) -> bool {
        let mut current = self.buffered_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > MAX_LEASE_BUFFERED_BYTES || self.is_shutdown() {
                return false;
            }
            match self.buffered_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release_bytes(&self, bytes: usize) {
        self.buffered_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    fn abort(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let mut streams = self.streams.lock().unwrap_or_else(|err| err.into_inner());
        let drained = std::mem::take(&mut *streams);
        for (_, stream) in drained {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn watch(self: &Arc<Self>, stream: &std::os::unix::net::UnixStream) -> WatchedStream {
        let id = stream.try_clone().ok().map(|clone| {
            let mut next = self.next_id.lock().unwrap_or_else(|err| err.into_inner());
            let id = *next;
            *next = next.saturating_add(1);
            self.streams
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .insert(id, clone);
            id
        });
        if self.is_shutdown() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        WatchedStream {
            set: Arc::clone(self),
            id,
        }
    }

    fn unregister(&self, id: u64) {
        self.streams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&id);
    }
}

#[cfg(unix)]
struct LeaseConnectionPermit(Arc<LeaseConnSet>);

#[cfg(unix)]
impl Drop for LeaseConnectionPermit {
    fn drop(&mut self) {
        self.0.connection_count.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(unix)]
struct RequestByteBudget {
    set: Option<Arc<LeaseConnSet>>,
    bytes: usize,
}

#[cfg(unix)]
impl RequestByteBudget {
    fn new(set: Option<&Arc<LeaseConnSet>>) -> Self {
        Self {
            set: set.cloned(),
            bytes: 0,
        }
    }

    fn reserve(&mut self, bytes: usize) -> Result<()> {
        if let Some(set) = &self.set
            && !set.try_acquire_bytes(bytes)
        {
            bail!("job Docker lease buffered-byte budget exceeded");
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("job Docker lease buffered-byte accounting overflow")?;
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_sub(bytes);
        if let Some(set) = &self.set {
            set.release_bytes(bytes);
        }
    }
}

#[cfg(unix)]
impl Drop for RequestByteBudget {
    fn drop(&mut self) {
        if let Some(set) = &self.set {
            set.release_bytes(self.bytes);
        }
    }
}

#[cfg(unix)]
impl Drop for WatchedStream {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.set.unregister(id);
        }
    }
}

impl DockerLeaseGuard {
    /// Bind a lease on the runner-visible filesystem and proxy it to the
    /// resolved local Docker daemon socket. A Docker VM path, when needed, is
    /// only used by the container bind mount; it must never be passed here as
    /// the listener path.
    pub fn bind(listen_path: PathBuf, job_id: String, daemon_id: String) -> Result<Self> {
        let host_socket = crate::docker::engine::resolve_docker_endpoint()
            .context("resolve Docker endpoint for job lease")?
            .socket;
        Self::bind_to(listen_path, host_socket, job_id, daemon_id)
    }

    pub fn bind_to(
        listen_path: PathBuf,
        host_socket: PathBuf,
        job_id: String,
        daemon_id: String,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (listen_path, host_socket, job_id, daemon_id);
            bail!("job Docker lease proxy requires unix");
        }
        #[cfg(unix)]
        {
            bind_unix_lease(listen_path, host_socket, job_id, daemon_id)
        }
    }

    pub(crate) fn begin_persistent_builder_setup(&self, builder: &str) -> Result<()> {
        self.policy.begin_persistent_builder_setup(builder)
    }

    pub(crate) fn complete_persistent_builder_setup(&self, builder: &str) -> Result<()> {
        self.policy.complete_persistent_builder_setup(builder)
    }

    pub(crate) fn revoke_persistent_builder(&self, builder: &str) -> Result<()> {
        self.policy.revoke_persistent_builder(builder)
    }

    /// Bind a persistent builder to the host-resolved immutable image ID
    /// before the guest can issue Buildx image/container calls.
    pub fn register_persistent_builder_image(&self, builder: &str, image_id: &str) -> Result<()> {
        self.policy
            .register_persistent_builder_image(builder, image_id)
    }

    /// Acquire the daemon-wide volume-name fence for a host-side lifecycle
    /// operation. The returned guard must remain alive through inspect and the
    /// mutation; failure to acquire the flock is fail-closed.
    #[cfg(unix)]
    pub(crate) fn lock_volume_name(&self, volume: &str) -> Result<VolumeOperationLocks> {
        self.policy
            .lock_volume_names(&BTreeSet::from([volume.to_owned()]))
    }

    /// Register a host-inspected persistent BuildKit state volume before the
    /// guest can create a container that mounts it. The host runner supplies
    /// the four-field TSV projection emitted by
    /// [`inspect_volume_identity_args`]; the policy still verifies the exact
    /// current builder, local driver, empty local options, daemon label, and
    /// nonempty job label.
    pub fn register_persistent_volume_inspect(
        &self,
        volume: &str,
        body: &[u8],
        daemon_id: &str,
    ) -> Result<()> {
        self.policy
            .record_persistent_volume_projection(volume, body, daemon_id)
    }
}

/// Host lifecycle code runs after the guest lease has been dropped, so it
/// cannot borrow that lease's in-process registry. It still uses the same
/// endpoint-keyed flock namespace to serialize inspect→delete by volume name
/// across runner processes.
#[cfg(unix)]
pub(crate) fn lock_host_volume_name(
    host_socket: &Path,
    volume: &str,
) -> Result<VolumeOperationLocks> {
    let root = docker_volume_lock_root(host_socket, "host-lifecycle")?;
    let policy =
        DockerLeasePolicy::new_with_volume_lock_root("velnor-host-volume-lock", Some(root))?;
    policy.lock_volume_names(&BTreeSet::from([volume.to_owned()]))
}

#[cfg(unix)]
fn host_volume_mutation_target(args: &[String]) -> Option<&str> {
    if args.first().map(String::as_str) != Some("volume") {
        return None;
    }
    matches!(args.get(1).map(String::as_str), Some("inspect" | "rm"))
        .then(|| args.last().map(String::as_str))
        .flatten()
        .filter(|target| !target.starts_with('-'))
}

impl Drop for DockerLeaseGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        {
            // Abort in-flight Engine HTTP first. Job cancel kills the guest
            // CLI, but a one-way host→client copy stays blocked on dockerd
            // `ContainerStart`; that lock is what made Created BuildKit
            // `docker rm --force` hang until dockerd was SIGKILL'd.
            self.conns.abort();
        }
        if let Some(thread) = self.accept_thread.take() {
            #[cfg(unix)]
            if let Some(mut wake) = self.shutdown_wake.take() {
                // Wake the poll set directly. Synthetic listener connects
                // raced shutdown and could still leave the accept thread
                // inside a blocking accept on busy hosts.
                let _ = wake.write_all(&[1]);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = thread.join();
                let _ = tx.send(());
            });
            if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
                eprintln!(
                    "Warning: job Docker lease accept thread did not stop within 2s; continuing teardown"
                );
            }
        }
        let _ = std::fs::remove_file(&self.listen_path);
        // dockerd auto-creates an empty DIRECTORY at a missing bind-mount
        // source; if this path ever became one, drop it too (remove_dir only
        // succeeds on empty dirs, so a real socket file tree is untouched).
        let _ = std::fs::remove_dir(&self.listen_path);
    }
}

#[cfg(unix)]
fn docker_volume_lock_root(host_socket: &Path, daemon_id: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let _ = daemon_id;
    // `daemon_id` is a per-slot work-dir label, not a Docker endpoint
    // identity.  A shared engine therefore needs one lock namespace even
    // when jobs run in different slots or storage roots.  Canonicalizing the
    // socket collapses symlink aliases while retaining a deterministic
    // fallback for a socket that is being created during startup.
    let engine_socket = std::fs::canonicalize(host_socket)
        .unwrap_or_else(|_| host_socket.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let base = std::env::temp_dir().join("velnor-docker-volume-locks");
    let root = base.join(volume_lock_key(&engine_socket));
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create Docker volume lock root {}", root.display()))?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict Docker volume lock root {}", root.display()))?;
    Ok(root)
}

fn volume_lock_key(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn acquire_volume_file_lock(root: &Path, volume: &str) -> Result<File> {
    let path = root.join(format!("{}.lock", volume_lock_key(volume)));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open Docker volume lock {}", path.display()))?;
    let deadline = Instant::now() + VOLUME_LOCK_TIMEOUT;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) => {
                if Instant::now() >= deadline {
                    bail!(
                        "timed out acquiring Docker volume lock {} after {:?}",
                        path.display(),
                        VOLUME_LOCK_TIMEOUT
                    );
                }
                std::thread::sleep(VOLUME_LOCK_RETRY);
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("lock Docker volume {}", path.display())));
            }
        }
    }
}

#[cfg(unix)]
fn bind_unix_lease(
    listen_path: PathBuf,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
) -> Result<DockerLeaseGuard> {
    use std::os::unix::{ffi::OsStrExt, net::UnixListener};

    let path_bytes = listen_path.as_os_str().as_bytes().len();
    if path_bytes >= UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "job Docker lease socket path {} is {} bytes, exceeding the safe Unix socket limit of {}; shorten --work-dir or choose a shorter daemon-visible work root",
            listen_path.display(),
            path_bytes,
            UNIX_SOCKET_PATH_LIMIT
        );
    }

    if let Some(parent) = listen_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create job Docker lease directory {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&listen_path);
    let listener = UnixListener::bind(&listen_path)
        .with_context(|| format!("bind job Docker lease socket {}", listen_path.display()))?;
    listener
        .set_nonblocking(true)
        .context("configure job Docker lease socket")?;
    let (wake_reader, wake_writer) =
        std::os::unix::net::UnixStream::pair().context("create job Docker lease shutdown wake")?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let conns = LeaseConnSet::new(Arc::clone(&shutdown));
    let volume_lock_root = docker_volume_lock_root(&host_socket, &daemon_id)?;
    let policy = Arc::new(DockerLeasePolicy::new_with_volume_lock_root(
        &job_id,
        Some(volume_lock_root),
    )?);
    let conns_thread = Arc::clone(&conns);
    let policy_thread = Arc::clone(&policy);
    let listen_path_thread = listen_path.clone();
    let accept_thread = std::thread::Builder::new()
        .name(format!("velnor-docker-lease-{}", job_id))
        .spawn(move || {
            accept_loop(
                listener,
                LeaseServeContext {
                    host_socket,
                    job_id,
                    daemon_id,
                    conns: conns_thread,
                    policy: policy_thread,
                    listen_path: listen_path_thread,
                },
                wake_reader,
            );
        })
        .context("start job Docker lease proxy thread")?;
    Ok(DockerLeaseGuard {
        listen_path,
        shutdown,
        accept_thread: Some(accept_thread),
        policy,
        conns,
        shutdown_wake: Some(wake_writer),
    })
}

#[cfg(unix)]
/// Everything `accept_loop` needs that identifies *which* lease it serves, as
/// opposed to the sockets it serves it on. Grouped so the identity travels as
/// one value: a caller cannot pass a job id from one lease with the policy of
/// another.
struct LeaseServeContext {
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    conns: Arc<LeaseConnSet>,
    policy: Arc<DockerLeasePolicy>,
    listen_path: PathBuf,
}

fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    context: LeaseServeContext,
    wake_reader: std::os::unix::net::UnixStream,
) {
    let LeaseServeContext {
        host_socket,
        job_id,
        daemon_id,
        conns,
        policy,
        listen_path,
    } = context;
    use std::os::fd::AsRawFd;

    let mut poll_fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    while !conns.is_shutdown() {
        poll_fds[0].revents = 0;
        poll_fds[1].revents = 0;
        let polled = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if poll_fds[1].revents != 0 {
            break;
        }
        if poll_fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            // Transient accept failures (a client aborts between connect and
            // accept, or the kernel refuses a peer) must not kill the lease
            // proxy mid-job: a dropped accept loop hangs every later Docker
            // call for this job. Keep the pre-poll behavior of tolerating
            // them and only exit on errors that leave the listener unusable.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    eprintln!(
                        "Warning: job Docker lease accept retry after transient error: {error}"
                    );
                }
                continue;
            }
            Err(_) => break,
        };
        if conns.is_shutdown() {
            break;
        }
        let Some(permit) = conns.try_acquire_connection() else {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            continue;
        };
        let host_socket = host_socket.clone();
        let job_id = job_id.clone();
        let daemon_id = daemon_id.clone();
        let conns = Arc::clone(&conns);
        let policy = Arc::clone(&policy);
        let _ = std::thread::Builder::new()
            .name("velnor-docker-lease-conn".into())
            .spawn(move || {
                let _permit = permit;
                if let Err(error) =
                    handle_client_with(stream, &host_socket, &job_id, &daemon_id, conns, policy)
                {
                    eprintln!("Warning: job Docker lease proxy: {error:#}");
                }
            });
    }
    let _ = std::fs::remove_file(listen_path);
}

#[cfg(all(test, unix))]
fn handle_client(
    client: std::os::unix::net::UnixStream,
    host_socket: &Path,
    job_id: &str,
    daemon_id: &str,
) -> Result<()> {
    handle_client_with(
        client,
        host_socket,
        job_id,
        daemon_id,
        LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
        Arc::new(DockerLeasePolicy::new(job_id)?),
    )
}

#[cfg(unix)]
fn handle_client_with(
    mut client: std::os::unix::net::UnixStream,
    host_socket: &Path,
    job_id: &str,
    daemon_id: &str,
    conns: Arc<LeaseConnSet>,
    policy: Arc<DockerLeasePolicy>,
) -> Result<()> {
    use std::os::unix::net::UnixStream;

    client
        .set_read_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure job Docker lease client idle timeout")?;
    client
        .set_write_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure job Docker lease client write timeout")?;
    let _client_watch = conns.watch(&client);
    if conns.is_shutdown() {
        return Ok(());
    }
    let mut client_prefix = Vec::new();
    let mut host_state: Option<(UnixStream, WatchedStream)> = None;
    let mut host_buffer = ResponseBuffer::default();
    loop {
        let request =
            match read_http_request_with_budget_from(&mut client, client_prefix, Some(&conns)) {
                Ok(request) => request,
                Err(_) if conns.is_shutdown() => return Ok(()),
                Err(error) => return Err(error),
            };
        let HttpRequest {
            bytes,
            remainder,
            mut budget,
        } = request;
        let authorization = match policy.authorize(&bytes) {
            Ok(authorization) => authorization,
            Err(error) => {
                if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                    write_deny_response(&mut client, deny.status, &deny.message)?;
                }
                return Err(error);
            }
        };
        let request_method = http_request_method(&bytes)?.to_owned();
        let request_wants_close = http_request_wants_close(&bytes);
        let upgrade = request_is_upgrade(&bytes);
        let resource_target = docker_resource_target(&bytes);
        let create_container_name = match authorization {
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
            | AuthorizedDockerRoute::PersistentBootstrap => containers_create_query_name(&bytes)?,
            _ => None,
        };
        let create_volume_name = match authorization {
            AuthorizedDockerRoute::Create(DockerResourceKind::Volume) => {
                let body = docker_request_body(&bytes)?;
                let value = parse_create_value(body)?;
                volume_create_request_name(&value)?
            }
            _ => None,
        };
        #[cfg(unix)]
        let _volume_locks = {
            let result = (|| -> Result<VolumeOperationLocks> {
                match authorization {
                    AuthorizedDockerRoute::Create(DockerResourceKind::Container)
                    | AuthorizedDockerRoute::PersistentBootstrap => {
                        preflight_container_mounts(&policy, host_socket, &bytes, job_id, daemon_id)
                    }
                    AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
                    | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
                        if matches!(request_method.as_str(), "GET" | "HEAD" | "DELETE") =>
                    {
                        let target = resource_target
                            .as_deref()
                            .context("volume route omitted its target")?;
                        let names = BTreeSet::from([target.to_owned()]);
                        let locks = policy.lock_volume_names(&names)?;
                        preflight_volume_identity(
                            &policy,
                            host_socket,
                            target,
                            authorization,
                            job_id,
                            daemon_id,
                        )?;
                        Ok(locks)
                    }
                    AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                        if request_method == "POST" && resource_target.is_some() =>
                    {
                        preflight_persistent_container_volume(
                            &policy,
                            host_socket,
                            resource_target
                                .as_deref()
                                .context("persistent container route omitted its target")?,
                            job_id,
                            daemon_id,
                        )
                    }
                    AuthorizedDockerRoute::Create(DockerResourceKind::Volume) => {
                        let Some(name) = create_volume_name.as_ref() else {
                            return Ok(VolumeOperationLocks::default());
                        };
                        policy.lock_volume_names(&BTreeSet::from([name.clone()]))
                    }
                    _ => Ok(VolumeOperationLocks::default()),
                }
            })();
            match result {
                Ok(locks) => locks,
                Err(error) => {
                    if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                        write_deny_response(&mut client, deny.status, &deny.message)?;
                    }
                    return Err(error);
                }
            }
        };
        let forwarded = transform_request_buffer(bytes, &mut budget, |request| {
            policy.rewrite_docker_api_request_for_route(request, job_id, daemon_id, authorization)
        })?;
        let forwarded = transform_request_buffer(forwarded, &mut budget, |request| {
            policy.rewrite_authorized_alias_target(request, authorization)
        })?;
        let forwarded = transform_request_buffer(forwarded, &mut budget, without_expect_continue)?;
        if conns.is_shutdown() {
            return Ok(());
        }
        if upgrade {
            let (mut host, _host_watch) = connect_lease_host(host_socket, &conns)?;
            host.write_all(&forwarded)
                .context("forward Docker API request through job lease")?;
            // Keep the same idle timeout on hijacked streams. Clearing it
            // would let an abandoned attach/build session hold one of the
            // bounded lease connections forever.
            if !remainder.is_empty() {
                host.write_all(&remainder)
                    .context("forward buffered Docker upgrade bytes")?;
            }
            return proxy_until_closed(host, client);
        }

        if host_state.is_none() {
            host_state = Some(connect_lease_host(host_socket, &conns)?);
        }
        let reusable = {
            // Proof: the branch above assigns `Some` or returns via `?`, so
            // the state is `Some` here.
            #[allow(clippy::expect_used, reason = "host state just initialized")]
            let (host, _) = host_state.as_mut().expect("host state initialized");
            if let Err(error) = host
                .write_all(&forwarded)
                .context("forward Docker API request through job lease")
            {
                eprintln!("T004 host write error: {error:#}");
                if conns.is_shutdown() {
                    return Ok(());
                }
                return Err(error);
            }
            let create_kind = match authorization {
                AuthorizedDockerRoute::Create(kind) => Some(kind),
                _ => None,
            };
            let capture_response = create_kind.is_some()
                || matches!(authorization, AuthorizedDockerRoute::PersistentBootstrap)
                || matches!(authorization, AuthorizedDockerRoute::PersistentImageInspect)
                || (matches!(
                    authorization,
                    AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
                ) && request_method == "GET")
                || matches!(authorization, AuthorizedDockerRoute::PersistentInspect(_));
            let redact_persistent_container_inspect = matches!(
                authorization,
                AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
            );
            let redact_persistent_image_inspect =
                matches!(authorization, AuthorizedDockerRoute::PersistentImageInspect);
            let response_options = ForwardResponseOptions {
                capture_body: capture_response,
                redact_persistent_container_inspect,
                redact_persistent_image_inspect,
                defer_response_until_observed: matches!(
                    authorization,
                    AuthorizedDockerRoute::PersistentBootstrap
                ),
                persistent_image_ids: if redact_persistent_image_inspect {
                    policy.persistent_builder_images()?.into_values().collect()
                } else {
                    BTreeSet::new()
                },
            };
            match forward_http_response_with_observer(
                host,
                &mut host_buffer,
                &mut client,
                &request_method,
                response_options,
                |status, body| {
                    if let Some(kind) = create_kind {
                        policy.record_create_response_with_lease(
                            kind,
                            status,
                            body,
                            create_volume_name.as_deref(),
                            Some(job_id),
                            Some(daemon_id),
                        )?;
                    }
                    if !matches!(authorization, AuthorizedDockerRoute::PersistentBootstrap)
                        && let Some(name) = create_container_name.as_deref()
                    {
                        policy.note_container_name(name, status, body)?;
                    }
                    if matches!(authorization, AuthorizedDockerRoute::PersistentBootstrap) {
                        let candidate = policy.note_persistent_container_candidate(status, body)?;
                        let target = create_container_name
                            .as_deref()
                            .context("persistent BuildKit create omitted its container name")?;
                        let (inspect_status, inspect_body) =
                            inspect_container_on_host(host_socket, &candidate)?;
                        if !(200..300).contains(&inspect_status) {
                            if inspect_status == 404 {
                                policy.forget_persistent_container(&candidate)?;
                            }
                            bail!(
                                "persistent BuildKit container {target} failed host attestation (HTTP {inspect_status})"
                            );
                        }
                        policy.record_persistent_container_inspect(
                            target,
                            inspect_status,
                            &inspect_body,
                            daemon_id,
                        )?;
                    }
                    if matches!(authorization, AuthorizedDockerRoute::PersistentExecCreate) {
                        let target = resource_target
                            .as_deref()
                            .context("persistent exec route omitted its container target")?;
                        let builder = policy.persistent_container_builder(target)?;
                        policy.note_persistent_exec(status, body, &builder)?;
                    }
                    match authorization {
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container) => {
                            policy.record_persistent_container_inspect(
                                resource_target
                                    .as_deref()
                                    .context("persistent container route omitted its target")?,
                                status,
                                body,
                                daemon_id,
                            )?
                        }
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume) => {
                            policy.record_persistent_volume_inspect(
                                resource_target
                                    .as_deref()
                                    .context("persistent volume route omitted its target")?,
                                status,
                                body,
                                daemon_id,
                            )?
                        }
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
                            if request_method == "GET" =>
                        {
                            policy.record_owned_volume_inspect(
                                resource_target
                                    .as_deref()
                                    .context("owned volume route omitted its target")?,
                                status,
                                body,
                                job_id,
                                daemon_id,
                            )?
                        }
                        AuthorizedDockerRoute::Owned(kind) if request_method == "DELETE" => policy
                            .record_delete_response(
                                kind,
                                resource_target
                                    .as_deref()
                                    .context("owned delete route omitted its target")?,
                                status,
                            )?,
                        _ => {}
                    }
                    Ok(())
                },
            ) {
                Ok(reusable) => reusable,
                Err(error) if error.downcast_ref::<GuestClosed>().is_some() => return Ok(()),
                Err(error) => return Err(error),
            }
        };
        drop(forwarded);
        drop(budget);
        if !reusable || request_wants_close {
            return Ok(());
        }
        client_prefix = remainder;
        if conns.is_shutdown() {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn write_deny_response(
    client: &mut std::os::unix::net::UnixStream,
    status: u16,
    message: &str,
) -> Result<()> {
    let reason = match status {
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let mut body = String::with_capacity(message.len() + 16);
    body.push_str("{\"message\":\"");
    for ch in message.chars() {
        match ch {
            '"' => body.push_str("\\\""),
            '\\' => body.push_str("\\\\"),
            '\n' => body.push_str("\\n"),
            '\r' => body.push_str("\\r"),
            '\t' => body.push_str("\\t"),
            ch if (ch as u32) < 0x20 => body.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => body.push(ch),
        }
    }
    body.push_str("\"}");
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body.len(),
        body
    );
    client
        .write_all(response.as_bytes())
        .context("write Docker lease denial response")
}

#[cfg(unix)]
#[derive(Debug)]
struct GuestClosed;

#[cfg(unix)]
impl fmt::Display for GuestClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("guest Docker client closed while awaiting response")
    }
}

#[cfg(unix)]
impl std::error::Error for GuestClosed {}

#[cfg(unix)]
fn connect_lease_host(
    host_socket: &Path,
    conns: &Arc<LeaseConnSet>,
) -> Result<(std::os::unix::net::UnixStream, WatchedStream)> {
    let host = std::os::unix::net::UnixStream::connect(host_socket).with_context(|| {
        format!(
            "connect job Docker lease to host engine {}",
            host_socket.display()
        )
    })?;
    host.set_read_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure host Docker lease idle timeout")?;
    host.set_write_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure host Docker lease write timeout")?;
    let watch = conns.watch(&host);
    if conns.is_shutdown() {
        return Ok((host, watch));
    }
    Ok((host, watch))
}

#[cfg(unix)]
fn preflight_volume_identity(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    target: &str,
    authorization: AuthorizedDockerRoute,
    job_id: &str,
    daemon_id: &str,
) -> Result<()> {
    let (status, body) = inspect_volume_on_host(host_socket, target)?;
    if status == 404 {
        policy.forget_volume(target)?;
        return Err(LeaseDeny::not_found(format!(
            "Docker lease volume {target:?} disappeared before use"
        )));
    }
    if !(200..300).contains(&status) {
        policy.forget_volume(target)?;
        return Err(LeaseDeny::not_found(format!(
            "Docker lease volume {target:?} could not be re-attested (HTTP {status})"
        )));
    }
    let attestation = match authorization {
        AuthorizedDockerRoute::Owned(DockerResourceKind::Volume) => {
            attest_created_volume_identity(&body, target, job_id, daemon_id)
        }
        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume) => policy
            .record_persistent_volume_inspect(target, status, &body, daemon_id)
            .map(|_| target.to_owned()),
        _ => return Ok(()),
    };
    if attestation.is_err()
        && matches!(
            authorization,
            AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
        )
    {
        policy.forget_volume(target)?;
    }
    attestation.map(|_| ()).map_err(|error| {
        LeaseDeny::not_found(format!(
            "Docker lease volume {target:?} failed immutable re-attestation: {error}"
        ))
    })
}

#[cfg(unix)]
fn preflight_persistent_container_volume(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    target: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<VolumeOperationLocks> {
    let volume = policy.persistent_container_volume(target).map_err(|_| {
        LeaseDeny::not_found("Docker lease persistent container has no attested state volume")
    })?;
    let names = BTreeSet::from([volume.clone()]);
    let locks = policy.lock_volume_names(&names)?;
    preflight_volume_identity(
        policy,
        host_socket,
        &volume,
        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume),
        job_id,
        daemon_id,
    )?;
    Ok(locks)
}

#[cfg(unix)]
fn preflight_container_mounts(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
) -> Result<VolumeOperationLocks> {
    let body = docker_request_body(request)?;
    let value = parse_create_value(body).context("parse Docker container mount preflight")?;
    let Some(host_config) = value
        .as_object()
        .and_then(|object| {
            object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
                .map(|(_, value)| value)
        })
        .and_then(Value::as_object)
    else {
        return Ok(VolumeOperationLocks::default());
    };
    let Some(mounts) = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Mounts"))
        .map(|(_, value)| value)
        .and_then(Value::as_array)
    else {
        return Ok(VolumeOperationLocks::default());
    };
    let (owned_volumes, persistent_volumes) = policy.volume_names()?;
    let mut names = BTreeSet::new();
    for mount in mounts {
        let Some(mount) = mount.as_object() else {
            continue;
        };
        if mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
            .and_then(|(_, value)| value.as_str())
            != Some("volume")
        {
            continue;
        }
        let Some(source) = mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Source"))
            .and_then(|(_, value)| value.as_str())
        else {
            continue;
        };
        if !owned_volumes.contains(source) && !persistent_volumes.contains(source) {
            continue;
        }
        names.insert(source.to_owned());
    }
    let locks = policy.lock_volume_names(&names)?;
    for source in names {
        let authorization = if owned_volumes.contains(&source) {
            AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
        } else {
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
        };
        preflight_volume_identity(
            policy,
            host_socket,
            &source,
            authorization,
            job_id,
            daemon_id,
        )?;
    }
    Ok(locks)
}

#[cfg(unix)]
fn inspect_volume_on_host(host_socket: &Path, target: &str) -> Result<(u16, Vec<u8>)> {
    inspect_object_on_host(host_socket, "volumes", target, "volume")
}

#[cfg(unix)]
fn inspect_container_on_host(host_socket: &Path, target: &str) -> Result<(u16, Vec<u8>)> {
    inspect_object_on_host(host_socket, "containers", target, "container")
}

#[cfg(unix)]
fn inspect_object_on_host(
    host_socket: &Path,
    resource_path: &str,
    target: &str,
    resource_kind: &str,
) -> Result<(u16, Vec<u8>)> {
    use std::os::unix::net::UnixStream;

    let target = validate_owned_resource_id(target, "Docker resource")?;
    let encoded_target = target.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
        encoded
    });
    let mut stream = UnixStream::connect(host_socket).with_context(|| {
        format!(
            "connect Docker host for {resource_kind} re-attestation {}",
            host_socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure Docker object re-attestation read timeout")?;
    stream
        .set_write_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure Docker object re-attestation write timeout")?;
    let request = format!(
        "GET /v1.43/{resource_path}/{encoded_target} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .context("request Docker object re-attestation")?;

    let mut response = Vec::new();
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    let header_end = loop {
        if let Some(index) = response.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if response.len() > MAX_PROXY_HEADER {
            bail!("Docker object re-attestation headers exceed lease proxy limit");
        }
        let read = stream
            .read(&mut scratch)
            .context("read Docker object re-attestation headers")?;
        if read == 0 {
            bail!("Docker host closed before object re-attestation headers finished");
        }
        response.extend_from_slice(&scratch[..read]);
    };
    let header = std::str::from_utf8(&response[..header_end])
        .context("Docker object re-attestation headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let status_line = lines
        .next()
        .context("Docker object re-attestation status line")?;
    let mut status_parts = status_line.split_ascii_whitespace();
    let version = status_parts
        .next()
        .context("Docker object re-attestation response version")?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        bail!("unsupported Docker object re-attestation response version {version:?}");
    }
    let status = status_parts
        .next()
        .context("Docker object re-attestation status code")?
        .parse::<u16>()
        .context("parse Docker object re-attestation status code")?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed Docker object re-attestation response header");
        };
        if name.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .context("parse Docker object re-attestation Content-Length")?;
            if content_length.replace(length).is_some() {
                bail!("duplicate Docker object re-attestation Content-Length");
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked |= value.trim().eq_ignore_ascii_case("chunked");
            if !chunked {
                bail!("unsupported Docker object re-attestation Transfer-Encoding");
            }
        }
    }
    if chunked {
        bail!("chunked Docker object re-attestation is unsupported");
    }
    let body_start = header_end;
    let body_length =
        content_length.context("Docker object re-attestation response has no bounded body")?;
    if body_length > MAX_CREATE_RESPONSE_BODY {
        bail!("Docker object re-attestation body exceeds capture limit");
    }
    while response.len() < body_start.saturating_add(body_length) {
        let read = stream
            .read(&mut scratch)
            .context("read Docker object re-attestation body")?;
        if read == 0 {
            bail!("Docker host closed before object re-attestation body finished");
        }
        response.extend_from_slice(&scratch[..read]);
        if response.len().saturating_sub(body_start) > MAX_CREATE_RESPONSE_BODY {
            bail!("Docker object re-attestation body exceeds capture limit");
        }
    }
    let body_end = body_start
        .checked_add(body_length)
        .context("Docker object re-attestation body length overflow")?;
    Ok((status, response[body_start..body_end].to_vec()))
}

#[cfg(unix)]
fn http_request_method(request: &[u8]) -> Result<&str> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("Docker API request is missing header terminator")?;
    let request_line = std::str::from_utf8(&request[..header_end])?
        .split_once("\r\n")
        .map_or_else(
            || std::str::from_utf8(&request[..header_end]),
            |(line, _)| Ok(line),
        )?;
    request_line
        .split_ascii_whitespace()
        .next()
        .context("Docker API request has no method")
}

#[cfg(unix)]
fn http_request_wants_close(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return true;
    };
    let Ok(header) = std::str::from_utf8(&request[..header_end]) else {
        return true;
    };
    let mut lines = header.split("\r\n");
    let version = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(2));
    let mut keep_alive = false;
    let mut close = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("connection") {
            continue;
        }
        for token in value.split(',').map(str::trim) {
            close |= token.eq_ignore_ascii_case("close");
            keep_alive |= token.eq_ignore_ascii_case("keep-alive");
        }
    }
    close || (version == Some("HTTP/1.0") && !keep_alive)
}

#[cfg(unix)]
fn request_is_upgrade(request: &[u8]) -> bool {
    docker_upgrade_state(request).unwrap_or(false)
}

fn docker_upgrade_state(request: &[u8]) -> Result<bool> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header = std::str::from_utf8(&request[..header_end])
        .context("Docker API request headers must be UTF-8")?;
    let mut connection_upgrade = false;
    let mut supported_upgrade = false;
    let mut connection_headers = 0;
    let mut upgrade_headers = 0;
    for line in header.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed Docker API request header");
        };
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade_headers += 1;
            if upgrade_headers > 1 {
                bail!("Docker API request has duplicate Upgrade headers");
            }
            let value = value.trim();
            if value.is_empty() {
                bail!("Docker API request has an empty Upgrade header");
            }
            supported_upgrade = value.trim().eq_ignore_ascii_case("tcp")
                || value.trim().eq_ignore_ascii_case("h2c");
        } else if name.eq_ignore_ascii_case("connection") {
            connection_headers += 1;
            if connection_headers > 1 {
                bail!("Docker API request has duplicate Connection headers");
            }
            connection_upgrade = value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
        }
    }
    let has_upgrade_marker = upgrade_headers != 0 || connection_upgrade;
    if !has_upgrade_marker {
        return Ok(false);
    }
    if connection_headers != 1 || upgrade_headers != 1 || !connection_upgrade || !supported_upgrade
    {
        bail!("Docker API request has an unsupported or malformed upgrade");
    }
    Ok(true)
}

/// Copy both directions, propagating half-closes instead of full teardown.
///
/// The Go docker client marks hijacked requests (`Connection: Upgrade`,
/// attach/exec/session) close-after-write, so it FINs its write side as soon
/// as the request is sent — long before the hijacked output stream arrives.
/// Treating that FIN as a guest disconnect and shutting both sockets down
/// killed dockerd's attach stream: `docker run` printed EMPTY stdout while
/// its exit code arrived on the separate `wait` connection (exit 0, work
/// done, logs dropped — tailrocks/velnor#348). A FIN is stdin-EOF semantics
/// here: forward it to the Engine write side only and keep pumping
/// Engine→guest until the Engine itself closes.
///
/// Non-upgrade used to `io::copy` host→guest only. Job cancel closed the
/// guest CLI, but the proxy kept the Engine `ContainerStart` HTTP request
/// open, so Created `buildx_buildkit_velnor-builder-*` could not be deleted.
/// The guest→host FIN still reaches the Engine (write shutdown = EOF on the
/// Engine's read side), so a cancelled client unblocks dockerd's in-flight
/// request; lease `Drop` aborts anything the Engine still holds open.
#[cfg(unix)]
fn proxy_until_closed(
    host: std::os::unix::net::UnixStream,
    client: std::os::unix::net::UnixStream,
) -> Result<()> {
    let lifetime_host = host
        .try_clone()
        .context("clone host Docker lease timer stream")?;
    let lifetime_client = client
        .try_clone()
        .context("clone job Docker lease timer stream")?;
    let (lifetime_cancel, lifetime_cancelled) = std::sync::mpsc::channel();
    let lifetime = std::thread::Builder::new()
        .name("velnor-docker-lease-lifetime".into())
        .spawn(move || {
            if lifetime_cancelled
                .recv_timeout(PROXY_MAX_UPGRADE_LIFETIME)
                .is_err()
            {
                let _ = lifetime_host.shutdown(std::net::Shutdown::Both);
                let _ = lifetime_client.shutdown(std::net::Shutdown::Both);
            }
        })
        .context("start job Docker lease lifetime timer")?;
    let mut host_read = host.try_clone().context("clone host Docker lease stream")?;
    let mut client_write = client
        .try_clone()
        .context("clone job Docker lease stream")?;
    let mut client_read = client
        .try_clone()
        .context("clone job Docker lease stream")?;
    let mut host_write = host.try_clone().context("clone host Docker lease stream")?;
    let client_half = client
        .try_clone()
        .context("clone job Docker lease stream")?;
    let up = std::thread::Builder::new()
        .name("velnor-docker-lease-io".into())
        .spawn(move || {
            let result = io::copy(&mut host_read, &mut client_write);
            // Engine finished: EOF the guest's read side, nothing else. The
            // guest closes at its leisure; the guest→host copy below then
            // returns on its own.
            let _ = client_half.shutdown(std::net::Shutdown::Write);
            result
        })
        .context("start job Docker lease copy thread")?;
    let _ = io::copy(&mut client_read, &mut host_write);
    // Guest FIN: stdin-EOF for the Engine, NOT a teardown of the hijacked
    // output stream still flowing in the copy thread above.
    let _ = host.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
    let _ = lifetime_cancel.send(());
    let _ = lifetime.join();
    Ok(())
}

/// Forward framed ordinary HTTP responses while keeping the guest and Engine
/// connections reusable. A response without HTTP framing remains a bounded
/// one-shot fallback because its end is defined by host EOF.
#[cfg(all(unix, test))]
fn forward_http_response(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
) -> Result<bool> {
    forward_http_response_with_observer(
        host,
        host_buffer,
        client,
        request_method,
        ForwardResponseOptions::default(),
        |_, _| Ok(()),
    )
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct ForwardResponseOptions {
    capture_body: bool,
    redact_persistent_container_inspect: bool,
    redact_persistent_image_inspect: bool,
    defer_response_until_observed: bool,
    persistent_image_ids: BTreeSet<String>,
}

#[cfg(unix)]
fn forward_http_response_with_observer(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    options: ForwardResponseOptions,
    mut observe: impl FnMut(u16, &[u8]) -> Result<()>,
) -> Result<bool> {
    loop {
        let head = read_http_response_head(host, host_buffer, client, request_method)?;
        if head.no_body {
            if options.defer_response_until_observed
                && let Err(error) = observe(head.status, &[])
            {
                write_observer_error_response(client, &error)?;
                return Err(error);
            }
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            if (100..200).contains(&head.status) && head.status != 101 {
                continue;
            }
            if !options.defer_response_until_observed {
                observe(head.status, &[])?;
            }
            return Ok(!head.close && head.status != 101);
        }
        if (options.redact_persistent_container_inspect || options.redact_persistent_image_inspect)
            && (200..300).contains(&head.status)
        {
            if head.chunked {
                bail!(
                    "persistent BuildKit container inspect response uses unsupported chunked framing"
                );
            }
            let content_length = head.content_length.context(
                "persistent BuildKit container inspect response has no bounded body framing",
            )?;
            if content_length > MAX_CREATE_RESPONSE_BODY {
                bail!(
                    "persistent BuildKit container inspect response exceeds ownership capture limit"
                );
            }
            let mut captured = Some(Vec::new());
            forward_exact_response_body_captured(
                host,
                host_buffer,
                client,
                content_length,
                &mut captured,
                false,
            )?;
            let raw_body = captured.as_deref().unwrap_or(&[]);
            // Attest the raw daemon response before any bytes reach the
            // guest. Buildx needs State/Mounts, but Config.Env may contain
            // workflow secrets injected into a reused BuildKit container.
            observe(head.status, raw_body)?;
            let redacted_body = if options.redact_persistent_container_inspect {
                project_persistent_container_inspect(raw_body)?
            } else {
                project_persistent_image_inspect(raw_body, &options.persistent_image_ids)?
            };
            debug_assert_eq!(redacted_body.len(), content_length);
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            client
                .write_all(&redacted_body)
                .context("forward redacted Docker API response body through job lease")?;
            return Ok(!head.close && head.status != 101);
        }
        if options.defer_response_until_observed {
            if head.chunked {
                bail!("persistent BuildKit bootstrap response uses unsupported chunked framing");
            }
            let content_length = head
                .content_length
                .context("persistent BuildKit bootstrap response has no bounded body framing")?;
            if content_length > MAX_CREATE_RESPONSE_BODY {
                bail!("persistent BuildKit bootstrap response exceeds capture limit");
            }
            let mut captured = Some(Vec::new());
            forward_exact_response_body_captured(
                host,
                host_buffer,
                client,
                content_length,
                &mut captured,
                false,
            )?;
            let body = captured.as_deref().unwrap_or(&[]);
            if let Err(error) = observe(head.status, body) {
                write_observer_error_response(client, &error)?;
                return Err(error);
            }
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            client
                .write_all(body)
                .context("forward Docker API response body through job lease")?;
            return Ok(!head.close && head.status != 101);
        }
        client
            .write_all(&head.bytes)
            .context("forward Docker API response headers through job lease")?;
        let mut captured = options.capture_body.then(Vec::new);
        if head.chunked {
            forward_chunked_response_captured(host, host_buffer, client, &mut captured)?;
        } else if let Some(content_length) = head.content_length {
            if options.capture_body && content_length > MAX_CREATE_RESPONSE_BODY {
                bail!("Docker create response exceeds ownership capture limit");
            }
            forward_exact_response_body_captured(
                host,
                host_buffer,
                client,
                content_length,
                &mut captured,
                true,
            )?;
        } else {
            if options.capture_body {
                bail!("Docker create response has no bounded body framing");
            }
            if !host_buffer.is_empty() {
                client
                    .write_all(host_buffer.as_slice())
                    .context("forward unframed Docker API response body")?;
                host_buffer.clear();
            }
            forward_unframed_response(host, client)?;
            observe(head.status, &[])?;
            return Ok(false);
        }
        if (100..200).contains(&head.status) && head.status != 101 {
            continue;
        }
        let body = captured.as_deref().unwrap_or(&[]);
        observe(head.status, body)?;
        return Ok(!head.close && head.status != 101);
    }
}

#[cfg(unix)]
fn write_observer_error_response(
    client: &mut std::os::unix::net::UnixStream,
    error: &anyhow::Error,
) -> Result<()> {
    let error_body = serde_json::to_vec(&serde_json::json!({"message": error.to_string()}))
        .unwrap_or_else(|_| b"{\"message\":\"attestation failed\"}".to_vec());
    let mut response = format!(
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        error_body.len()
    )
    .into_bytes();
    response.extend_from_slice(&error_body);
    client
        .write_all(&response)
        .context("write persistent BuildKit bootstrap attestation error")
}

/// Project a persistent BuildKit inspect into the tiny shape Buildx consumes.
/// The raw response is retained only inside the host-side observer for
/// attestation.  Keeping the response byte length unchanged preserves Docker's
/// framing header; JSON permits trailing whitespace.
#[cfg(unix)]
fn project_persistent_container_inspect(body: &[u8]) -> Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)
        .context("parse persistent BuildKit container inspect response for projection")?;
    let object = value
        .as_object()
        .context("persistent BuildKit container inspect response must be an object")?;
    let state = api_object_field(object, "State")
        .and_then(Value::as_object)
        .context("persistent BuildKit container inspect omitted State")?;
    let running = api_object_field(state, "Running")
        .and_then(Value::as_bool)
        .context("persistent BuildKit container inspect omitted State.Running")?;
    let started_at = api_object_string(state, "StartedAt")?;
    let mut projected = Map::new();
    projected.insert(
        "Id".into(),
        Value::String(api_object_string(object, "Id")?.to_owned()),
    );
    projected.insert(
        "Name".into(),
        Value::String(api_object_string(object, "Name")?.to_owned()),
    );
    projected.insert(
        "State".into(),
        serde_json::json!({"Running": running, "StartedAt": started_at}),
    );
    let original_len = body.len();
    let mut projected = serde_json::to_vec(&projected)
        .context("serialize projected persistent BuildKit container inspect response")?;
    if projected.len() > original_len {
        bail!(
            "projected persistent BuildKit container inspect response grew from {} to {} bytes",
            original_len,
            projected.len()
        );
    }
    projected.resize(original_len, b' ');
    Ok(projected)
}

/// Buildx's image inspect path needs only the immutable ID, repo digests, and
/// optional descriptor metadata. Never forward the raw image Config, which
/// can contain image environment values and labels.
#[cfg(unix)]
fn project_persistent_image_inspect(
    body: &[u8],
    approved_image_ids: &BTreeSet<String>,
) -> Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)
        .context("parse persistent BuildKit image inspect response for projection")?;
    let object = value
        .as_object()
        .context("persistent BuildKit image inspect response must be an object")?;
    let id = api_object_string(object, "Id")?;
    if !id.starts_with("sha256:") {
        bail!("persistent BuildKit image inspect omitted an immutable ID");
    }
    if !approved_image_ids.contains(id) {
        bail!("persistent BuildKit image inspect returned an unapproved image ID");
    }
    let digests = api_object_field(object, "RepoDigests")
        .and_then(Value::as_array)
        .context("persistent BuildKit image inspect omitted RepoDigests")?;
    let approved_digest = digests.iter().find(|digest| {
        digest
            .as_str()
            .is_some_and(is_approved_persistent_repo_digest)
    });
    let Some(approved_digest) = approved_digest else {
        bail!("persistent BuildKit image inspect is not pinned to the approved digest");
    };
    let mut projected = Map::new();
    projected.insert("Id".into(), Value::String(id.to_owned()));
    // Buildx only needs to know that the image is the approved immutable
    // repository digest. Do not expose other repo aliases or the raw
    // descriptor/annotations from the host image inspect response.
    projected.insert(
        "RepoDigests".into(),
        Value::Array(vec![approved_digest.clone()]),
    );
    let original_len = body.len();
    let mut projected = serde_json::to_vec(&projected)
        .context("serialize projected persistent BuildKit image inspect response")?;
    if projected.len() > original_len {
        bail!(
            "projected persistent BuildKit image inspect response grew from {} to {} bytes",
            projected.len(),
            original_len
        );
    }
    projected.resize(original_len, b' ');
    Ok(projected)
}

#[cfg(unix)]
#[derive(Default)]
struct ResponseBuffer {
    bytes: Vec<u8>,
    cursor: usize,
}

#[cfg(unix)]
impl ResponseBuffer {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[self.cursor..]
    }

    fn len(&self) -> usize {
        self.bytes.len() - self.cursor
    }

    fn is_empty(&self) -> bool {
        self.cursor == self.bytes.len()
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn consume(&mut self, length: usize) {
        debug_assert!(length <= self.len());
        self.cursor += length;
        self.compact_if_needed();
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.cursor = 0;
    }

    /// Move the live suffix only after enough prefix has accumulated. This
    /// makes repeated small chunk framing consumes amortized instead of
    /// shifting the tail on every consumed line.
    fn compact_if_needed(&mut self) {
        if self.cursor == self.bytes.len() {
            self.clear();
        } else if self.cursor >= PROXY_COPY_BUFFER && self.cursor >= self.bytes.len() / 2 {
            let remaining = self.bytes.len() - self.cursor;
            self.bytes.copy_within(self.cursor.., 0);
            self.bytes.truncate(remaining);
            self.cursor = 0;
        }
    }
}

#[cfg(unix)]
struct HttpResponseHead {
    bytes: Vec<u8>,
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
    close: bool,
    no_body: bool,
}

#[cfg(unix)]
fn read_http_response_head(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
) -> Result<HttpResponseHead> {
    let mut scan_from: usize = 0;
    let header_end = loop {
        let search_start = scan_from.saturating_sub(3);
        if let Some(relative) = buffered.as_slice()[search_start..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let end = search_start + relative + 4;
            if end > MAX_PROXY_HEADER {
                bail!("Docker API response headers exceed lease proxy limit");
            }
            break end;
        }
        if buffered.len() > MAX_PROXY_HEADER {
            bail!("Docker API response headers exceed lease proxy limit");
        }
        let previous_len = buffered.len();
        wait_for_host_response(host, client)?;
        let mut scratch = [0_u8; PROXY_COPY_BUFFER];
        let read = host
            .read(&mut scratch)
            .context("read Docker API response headers")?;
        if read == 0 {
            bail!("host Docker API closed before response headers finished");
        }
        scan_from = previous_len;
        buffered.extend_from_slice(&scratch[..read]);
    };
    let header_bytes = buffered.as_slice()[..header_end].to_vec();
    buffered.consume(header_end);
    let header_text =
        std::str::from_utf8(&header_bytes).context("Docker API response headers must be UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().context("Docker API response status line")?;
    let mut status_parts = status_line.split_ascii_whitespace();
    let version = status_parts.next().context("Docker API response version")?;
    if !version.eq_ignore_ascii_case("HTTP/1.0") && !version.eq_ignore_ascii_case("HTTP/1.1") {
        bail!("unsupported Docker API response version {version:?}");
    }
    let status: u16 = status_parts
        .next()
        .context("Docker API response status code")?
        .parse()
        .context("parse Docker API response status code")?;
    let mut content_length = None;
    let mut chunked = false;
    let mut close = version.eq_ignore_ascii_case("HTTP/1.0");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed Docker API response header");
        };
        if name.trim().is_empty() {
            bail!("Docker API response header has no field name");
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse()
                .context("parse Docker API response Content-Length")?;
            if content_length.replace(length).is_some() {
                bail!("refusing Docker API response with duplicate Content-Length");
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                bail!("refusing Docker API response with unsupported Transfer-Encoding");
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("connection") {
            close |= value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"));
        }
    }
    if chunked && content_length.is_some() {
        bail!("refusing Docker API response with both Content-Length and Transfer-Encoding");
    }
    let no_body = request_method.eq_ignore_ascii_case("HEAD")
        || (100..200).contains(&status)
        || matches!(status, 204 | 304);
    Ok(HttpResponseHead {
        bytes: header_bytes,
        status,
        content_length,
        chunked,
        close,
        no_body,
    })
}

#[cfg(unix)]
fn forward_exact_response_body_captured(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    mut remaining: usize,
    captured: &mut Option<Vec<u8>>,
    forward_to_client: bool,
) -> Result<()> {
    if !buffered.is_empty() && remaining != 0 {
        let take = remaining.min(buffered.len());
        capture_response_bytes(captured, &buffered.as_slice()[..take])?;
        if forward_to_client {
            client
                .write_all(&buffered.as_slice()[..take])
                .context("forward buffered Docker API response body")?;
        }
        buffered.consume(take);
        remaining -= take;
    }
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    while remaining != 0 {
        let read_len = remaining.min(scratch.len());
        wait_for_host_response(host, client)?;
        let read = host
            .read(&mut scratch[..read_len])
            .context("read Docker API response body")?;
        if read == 0 {
            bail!("host Docker API closed before response body finished");
        }
        capture_response_bytes(captured, &scratch[..read])?;
        if forward_to_client {
            client
                .write_all(&scratch[..read])
                .context("forward Docker API response body")?;
        }
        remaining -= read;
    }
    Ok(())
}

#[cfg(unix)]
fn read_response_line(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
) -> Result<Vec<u8>> {
    let mut scan_from: usize = 0;
    loop {
        let search_start = scan_from.saturating_sub(1);
        if let Some(relative) = buffered.as_slice()[search_start..]
            .windows(2)
            .position(|window| window == b"\r\n")
        {
            let end = search_start + relative + 2;
            if end > MAX_PROXY_LINE {
                bail!("Docker API response framing line exceeds lease proxy limit");
            }
            let line = buffered.as_slice()[..end].to_vec();
            buffered.consume(end);
            return Ok(line);
        }
        if buffered.len() > MAX_PROXY_LINE {
            bail!("Docker API response framing line exceeds lease proxy limit");
        }
        let previous_len = buffered.len();
        wait_for_host_response(host, client)?;
        let mut scratch = [0_u8; 8192];
        let read = host
            .read(&mut scratch)
            .context("read Docker API response framing")?;
        if read == 0 {
            bail!("host Docker API closed during chunked response framing");
        }
        scan_from = previous_len;
        buffered.extend_from_slice(&scratch[..read]);
    }
}

#[cfg(unix)]
fn validate_chunked_response_trailer(line: &[u8]) -> Result<()> {
    let field = line
        .strip_suffix(b"\r\n")
        .context("Docker API response trailer is missing its terminating CRLF")?;
    let Some(colon) = field.iter().position(|&byte| byte == b':') else {
        bail!("malformed Docker API response trailer");
    };
    let name = &field[..colon];
    let is_field_name_byte = |byte: &u8| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    };
    if name.is_empty() || !name.iter().all(is_field_name_byte) {
        bail!("malformed Docker API response trailer field name");
    }
    if field[colon + 1..]
        .iter()
        .any(|byte| byte.is_ascii_control() && *byte != b'\t')
    {
        bail!("malformed Docker API response trailer value");
    }
    Ok(())
}

#[cfg(all(unix, test))]
fn forward_chunked_response(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
) -> Result<()> {
    forward_chunked_response_captured(host, buffered, client, &mut None)
}

#[cfg(unix)]
fn forward_chunked_response_captured(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    captured: &mut Option<Vec<u8>>,
) -> Result<()> {
    loop {
        let line = read_response_line(host, buffered, client)?;
        client
            .write_all(&line)
            .context("forward Docker API chunk header")?;
        let line_text = std::str::from_utf8(&line[..line.len() - 2])
            .context("Docker API response chunk-size line must be UTF-8")?;
        let size_text = line_text
            .split_once(';')
            .map_or(line_text, |(size, _)| size)
            .trim();
        let size = usize::from_str_radix(size_text, 16)
            .context("parse Docker API response chunk-size line")?;
        forward_exact_response_body_captured(host, buffered, client, size, captured, true)?;
        if size == 0 {
            loop {
                let trailer = read_response_line(host, buffered, client)?;
                if trailer != b"\r\n" {
                    validate_chunked_response_trailer(&trailer)?;
                }
                client
                    .write_all(&trailer)
                    .context("forward Docker API response trailer")?;
                if trailer == b"\r\n" {
                    return Ok(());
                }
            }
        }
        let terminator = read_response_line(host, buffered, client)?;
        if terminator != b"\r\n" {
            bail!("Docker API response chunk is missing its terminating CRLF");
        }
        client
            .write_all(&terminator)
            .context("forward Docker API chunk terminator")?;
    }
}

#[cfg(unix)]
fn forward_unframed_response(
    host: &mut std::os::unix::net::UnixStream,
    client: &mut std::os::unix::net::UnixStream,
) -> Result<()> {
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    loop {
        wait_for_host_response(host, client)?;
        let read = host
            .read(&mut scratch)
            .context("read unframed Docker API response")?;
        if read == 0 {
            return Ok(());
        }
        client
            .write_all(&scratch[..read])
            .context("forward unframed Docker API response")?;
    }
}

#[cfg(unix)]
fn wait_for_host_response(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut poll_fds = [
        libc::pollfd {
            fd: host.as_raw_fd(),
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: client.as_raw_fd(),
            events: libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
    ];
    loop {
        let polled = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error()).context("poll Docker lease response streams");
        }
        if poll_fds[1].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            let _ = host.shutdown(std::net::Shutdown::Both);
            return Err(GuestClosed.into());
        }
        if poll_fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
            != 0
        {
            return Ok(());
        }
    }
}

#[cfg(unix)]
struct HttpRequest {
    bytes: Vec<u8>,
    remainder: Vec<u8>,
    budget: RequestByteBudget,
}

#[cfg(unix)]
fn transform_request_buffer(
    input: Vec<u8>,
    budget: &mut RequestByteBudget,
    transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let input_len = input.len();
    // Reserve one input-sized buffer before allocating the transformed copy.
    // This makes rewrite/header normalization participate in the same
    // lease-wide budget as socket reads instead of permitting copy
    // amplification above MAX_LEASE_BUFFERED_BYTES.
    budget.reserve(input_len)?;
    let output = transform(&input)?;
    if output.len() > input_len {
        budget.reserve(output.len() - input_len)?;
    }
    drop(input);
    budget.release(input_len);
    if output.len() < input_len {
        budget.release(input_len - output.len());
    }
    Ok(output)
}

#[cfg(all(test, unix))]
fn read_http_request(stream: &mut std::os::unix::net::UnixStream) -> Result<HttpRequest> {
    read_http_request_with_budget_from(stream, Vec::new(), None)
}

#[cfg(unix)]
fn read_http_request_with_budget_from(
    stream: &mut std::os::unix::net::UnixStream,
    prefix: Vec<u8>,
    set: Option<&Arc<LeaseConnSet>>,
) -> Result<HttpRequest> {
    let mut budget = RequestByteBudget::new(set);
    budget.reserve(prefix.len())?;
    let mut buf = prefix;
    let mut chunk = [0_u8; 8192];
    let mut scan_from: usize = 0;
    let header_end = loop {
        let search_start = scan_from.saturating_sub(3);
        if let Some(relative) = buf[search_start..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let end = search_start + relative + 4;
            if end > MAX_PROXY_HEADER {
                bail!("Docker API request headers exceed lease proxy limit");
            }
            break end;
        }
        if buf.len() > MAX_PROXY_HEADER {
            bail!("Docker API request headers exceed lease proxy limit");
        }
        let previous_len = buf.len();
        let read = stream
            .read(&mut chunk)
            .context("read Docker API request header")?;
        if read == 0 {
            bail!("client closed Docker API request before headers finished");
        }
        scan_from = previous_len;
        budget.reserve(read)?;
        buf.extend_from_slice(&chunk[..read]);
    };
    let header_text =
        std::str::from_utf8(&buf[..header_end]).context("Docker API headers must be UTF-8")?;
    let mut content_length = None;
    let mut transfer_encoding = None;
    let mut expect_continue = false;
    for line in header_text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer_encoding.is_some() {
                bail!("refusing Docker API request with duplicate Transfer-Encoding");
            }
            if !value.trim().eq_ignore_ascii_case("chunked") {
                bail!("refusing Docker API request with unsupported Transfer-Encoding");
            }
            transfer_encoding = Some(());
            continue;
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                bail!("refusing Docker API request with duplicate Content-Length");
            }
            content_length = Some(
                value
                    .trim()
                    .parse()
                    .context("parse Docker API Content-Length")?,
            );
            continue;
        }
        if name.eq_ignore_ascii_case("expect") {
            if expect_continue {
                bail!("refusing Docker API request with duplicate Expect");
            }
            if !value.trim().eq_ignore_ascii_case("100-continue") {
                bail!("refusing Docker API request with unsupported Expect");
            }
            expect_continue = true;
        }
    }
    if expect_continue {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .context("acknowledge Docker API Expect: 100-continue")?;
    }
    if transfer_encoding.is_some() {
        if content_length.is_some() {
            bail!("refusing Docker API request with both Content-Length and Transfer-Encoding");
        }
        return read_chunked_http_request(stream, buf, header_end, &mut chunk, &mut budget);
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_PROXY_BODY {
        bail!("Docker API request body exceeds lease proxy limit");
    }
    let request_len = header_end + content_length;
    while buf.len() < request_len {
        read_request_bytes(
            stream,
            &mut buf,
            &mut chunk,
            &mut budget,
            MAX_PROXY_BODY.saturating_add(MAX_PROXY_HEADER),
        )?;
    }
    let remainder = buf.split_off(request_len);
    Ok(HttpRequest {
        bytes: buf,
        remainder,
        budget,
    })
}

#[cfg(unix)]
fn read_chunked_http_request(
    stream: &mut std::os::unix::net::UnixStream,
    mut buf: Vec<u8>,
    header_end: usize,
    scratch: &mut [u8],
    budget: &mut RequestByteBudget,
) -> Result<HttpRequest> {
    let max_raw = MAX_PROXY_BODY.saturating_add(MAX_PROXY_HEADER);
    let mut cursor = header_end;
    let mut write_cursor = header_end;
    loop {
        let line_end = loop {
            if let Some(relative) = buf[cursor..]
                .windows(2)
                .position(|window| window == b"\r\n")
            {
                let line_end = cursor + relative;
                if line_end.saturating_sub(cursor).saturating_add(2) > MAX_PROXY_LINE {
                    bail!("Docker API chunk-size line exceeds lease proxy limit");
                }
                break line_end;
            }
            if buf.len().saturating_sub(cursor) > MAX_PROXY_LINE {
                bail!("Docker API chunk-size line exceeds lease proxy limit");
            }
            read_request_bytes(stream, &mut buf, scratch, budget, max_raw)?;
        };
        let line = std::str::from_utf8(&buf[cursor..line_end])
            .context("Docker chunk-size line must be UTF-8")?;
        let size_text = line.split_once(';').map_or(line, |(size, _)| size).trim();
        if size_text.is_empty() {
            bail!("Docker API chunk-size line is empty");
        }
        let size =
            usize::from_str_radix(size_text, 16).context("parse Docker API chunk-size line")?;
        let data_start = line_end + 2;
        let data_end = data_start
            .checked_add(size)
            .context("Docker API chunk size overflows usize")?;
        let decoded_len = write_cursor.saturating_sub(header_end);
        if decoded_len
            .checked_add(size)
            .is_none_or(|length| length > MAX_PROXY_BODY)
        {
            bail!("Docker API request body exceeds lease proxy limit");
        }
        if size == 0 {
            cursor = data_start;
            loop {
                let trailer_end = loop {
                    if let Some(relative) = buf[cursor..]
                        .windows(2)
                        .position(|window| window == b"\r\n")
                    {
                        let trailer_end = cursor + relative;
                        if trailer_end.saturating_sub(cursor).saturating_add(2) > MAX_PROXY_LINE {
                            bail!("Docker API chunk trailer exceeds lease proxy limit");
                        }
                        break trailer_end;
                    }
                    if buf.len().saturating_sub(cursor) > MAX_PROXY_LINE {
                        bail!("Docker API chunk trailer exceeds lease proxy limit");
                    }
                    read_request_bytes(stream, &mut buf, scratch, budget, max_raw)?;
                };
                if trailer_end == cursor {
                    cursor += 2;
                    break;
                }
                let trailer = std::str::from_utf8(&buf[cursor..trailer_end])
                    .context("Docker API chunk trailer must be UTF-8")?;
                let Some((name, _)) = trailer.split_once(':') else {
                    bail!("Docker API chunk trailer is malformed");
                };
                if name.trim().is_empty() {
                    bail!("Docker API chunk trailer has no field name");
                }
                cursor = trailer_end + 2;
            }
            break;
        }
        let framing_end = data_end
            .checked_add(2)
            .context("Docker API chunk framing overflows usize")?;
        while buf.len() < framing_end {
            read_request_bytes(stream, &mut buf, scratch, budget, max_raw)?;
        }
        if &buf[data_end..framing_end] != b"\r\n" {
            bail!("Docker API chunk is missing its terminating CRLF");
        }
        buf.copy_within(data_start..data_end, write_cursor);
        write_cursor = write_cursor
            .checked_add(size)
            .context("decoded Docker API chunk body overflows usize")?;
        cursor = framing_end;
    }
    let decoded_len = write_cursor.saturating_sub(header_end);
    let mut normalized = normalize_chunked_request_header(&buf[..header_end], decoded_len)?;
    let normalized_body_len = normalized
        .len()
        .checked_add(decoded_len)
        .context("normalized Docker API request size overflows usize")?;
    budget.reserve(normalized_body_len)?;
    normalized.extend_from_slice(&buf[header_end..write_cursor]);
    let remainder = buf.split_off(cursor);
    let raw_request_bytes = buf.len();
    drop(buf);
    budget.release(raw_request_bytes);
    Ok(HttpRequest {
        bytes: normalized,
        remainder,
        budget: std::mem::replace(budget, RequestByteBudget::new(None)),
    })
}

#[cfg(unix)]
fn read_request_bytes(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
    scratch: &mut [u8],
    budget: &mut RequestByteBudget,
    max_len: usize,
) -> Result<()> {
    let read = stream
        .read(scratch)
        .context("read Docker API chunked request")?;
    if read == 0 {
        bail!("client closed Docker API request before chunked body finished");
    }
    budget.reserve(read)?;
    buf.extend_from_slice(&scratch[..read]);
    if buf.len() > max_len {
        bail!("Docker API chunked request exceeds lease proxy limit");
    }
    Ok(())
}

#[cfg(unix)]
fn normalize_chunked_request_header(header: &[u8], body_len: usize) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(header).context("Docker API headers must be UTF-8")?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut normalized = Vec::new();
    normalized.extend_from_slice(request_line.as_bytes());
    normalized.extend_from_slice(b"\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            normalized.extend_from_slice(line.as_bytes());
            normalized.extend_from_slice(b"\r\n");
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        normalized.extend_from_slice(line.as_bytes());
        normalized.extend_from_slice(b"\r\n");
    }
    normalized.extend_from_slice(format!("Content-Length: {body_len}\r\n\r\n").as_bytes());
    Ok(normalized)
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
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn job_network_guard_defused_drop_is_noop() {
        // No panic, no docker invocation: the guard type runs `docker` only
        // when armed, so a defused drop must be silent even on a real host.
        let guard = JobNetworkGuard::arm("velnor-net-defused");
        guard.defuse();
    }

    #[test]
    fn job_network_guard_without_id_refuses_name_fallback() {
        // A partial create has no safe mutation handle. Drop must leave the
        // deterministic name untouched rather than resolving a replacement.
        drop(JobNetworkGuard::arm_with_id("velnor-net-unknown", None));
    }

    #[test]
    fn job_network_guard_armed_drop_attempts_forced_removal() {
        // Armed drop shells out to the host docker CLI (no injectable runner),
        // so only assert the removable-args contract it relies on: a forced
        // `network rm` of exactly the captured immutable network ID.
        let args = force_remove_network_args(&["network-id".to_string()]);
        assert_eq!(args, vec!["network", "rm", "network-id"]);
        let guard =
            JobNetworkGuard::arm_with_id("velnor-net-guarded", Some("network-id".to_owned()));
        drop(guard);
    }

    fn docker_rm_ids(args: &[String]) -> Vec<&str> {
        if args.first().map(String::as_str) != Some("rm") {
            return Vec::new();
        }
        args.iter()
            .skip(1)
            .filter(|arg| !arg.starts_with('-'))
            .map(String::as_str)
            .collect()
    }

    fn api_request(method: &str, target: &str, body: &[u8]) -> Vec<u8> {
        format!(
            "{method} {target} HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        )
        .into_bytes()
    }

    #[test]
    fn immutable_identity_attestation_binds_id_labels_image_network_and_command() {
        let labels = BTreeMap::from([
            (JOB_ID_LABEL.to_owned(), "job".to_owned()),
            (DAEMON_ID_LABEL.to_owned(), "daemon".to_owned()),
        ]);
        let output = [
            serde_json::to_string("container-full-id").unwrap(),
            serde_json::to_string("/velnor-job-job").unwrap(),
            serde_json::to_string("ubuntu@sha256:image").unwrap(),
            serde_json::to_string(&labels).unwrap(),
            serde_json::to_string("velnor-net").unwrap(),
            serde_json::to_string("/bin/sh").unwrap(),
            serde_json::to_string(&vec!["-c", "runner-command"]).unwrap(),
            serde_json::to_string("exited").unwrap(),
        ]
        .join("\t");
        let id = attest_container_identity(
            &output,
            &ContainerIdentityExpectation {
                expected_id: Some("container-full-id"),
                expected_name: "velnor-job-job",
                expected_image: "ubuntu@sha256:image",
                expected_network: "velnor-net",
                expected_labels: Some(("job", "daemon")),
                expected_command: Some(&["sh", "-c", "runner-command"]),
            },
        )
        .unwrap();
        assert_eq!(id, "container-full-id");
        let custom_entrypoint = [
            serde_json::to_string("container-full-id").unwrap(),
            serde_json::to_string("/velnor-job-job").unwrap(),
            serde_json::to_string("ubuntu@sha256:image").unwrap(),
            serde_json::to_string(&labels).unwrap(),
            serde_json::to_string("velnor-net").unwrap(),
            serde_json::to_string("/usr/local/bin/entrypoint").unwrap(),
            serde_json::to_string(&vec!["sh", "-c", "runner-command"]).unwrap(),
            serde_json::to_string("exited").unwrap(),
        ]
        .join("\t");
        assert_eq!(
            attest_container_identity(
                &custom_entrypoint,
                &ContainerIdentityExpectation {
                    expected_id: Some("container-full-id"),
                    expected_name: "velnor-job-job",
                    expected_image: "ubuntu@sha256:image",
                    expected_network: "velnor-net",
                    expected_labels: Some(("job", "daemon")),
                    expected_command: Some(&["sh", "-c", "runner-command"]),
                },
            )
            .unwrap(),
            "container-full-id"
        );
        let error = attest_container_identity(
            &output,
            &ContainerIdentityExpectation {
                expected_id: Some("replacement-id"),
                expected_name: "velnor-job-job",
                expected_image: "ubuntu@sha256:image",
                expected_network: "velnor-net",
                expected_labels: Some(("job", "daemon")),
                expected_command: Some(&["sh", "-c", "runner-command"]),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("identity ID mismatch"));
    }

    #[test]
    fn immutable_identity_attestation_rejects_foreign_network_and_volume() {
        let labels = BTreeMap::from([
            (JOB_ID_LABEL.to_owned(), "job".to_owned()),
            (DAEMON_ID_LABEL.to_owned(), "daemon".to_owned()),
        ]);
        let network = [
            serde_json::to_string("network-full-id").unwrap(),
            serde_json::to_string("velnor-net").unwrap(),
            serde_json::to_string("bridge").unwrap(),
            serde_json::to_string(&labels).unwrap(),
        ]
        .join("\t");
        assert_eq!(
            attest_network_identity(
                &network,
                Some("network-full-id"),
                "velnor-net",
                "job",
                "daemon"
            )
            .unwrap(),
            "network-full-id"
        );
        let mut foreign = labels.clone();
        foreign.insert(JOB_ID_LABEL.to_owned(), "other-job".to_owned());
        let foreign_network = [
            serde_json::to_string("network-full-id").unwrap(),
            serde_json::to_string("velnor-net").unwrap(),
            serde_json::to_string("bridge").unwrap(),
            serde_json::to_string(&foreign).unwrap(),
        ]
        .join("\t");
        assert!(
            attest_network_identity(&foreign_network, None, "velnor-net", "job", "daemon").is_err()
        );

        let volume = [
            serde_json::to_string("volume-job").unwrap(),
            serde_json::to_string("local").unwrap(),
            serde_json::to_string(&labels).unwrap(),
        ]
        .join("\t");
        assert_eq!(
            attest_volume_identity(&volume, "volume-job", "job", Some("daemon")).unwrap(),
            "volume-job"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lease_policy_denies_foreign_resources_and_unsafe_routes() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let requests = [
            api_request("GET", "/v1.43/containers/foreign/json", b""),
            api_request("POST", "/v1.43/containers/foreign/kill", b""),
            api_request("POST", "/v1.43/containers/velnor-job-owned/rename", b""),
            api_request("POST", "/v1.43/containers/velnor-job-owned/update", b"{}"),
            api_request("GET", "/v1.43/exec/foreign/json", b""),
            api_request("GET", "/v1.43/system/df", b""),
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let result = policy.authorize(&request);
            assert!(
                result.is_err(),
                "foreign or unsafe Docker route {index} must be denied: {}",
                String::from_utf8_lossy(&request)
            );
            let error = result.expect_err("checked above");
            assert!(
                error.to_string().contains("Docker lease denied"),
                "{error:#}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn lease_policy_allows_owned_routes_and_rejects_generic_upgrade() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        assert_eq!(
            policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/velnor-job-owned/attach",
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::Hijack(DockerResourceKind::Container)
        );
        let mut upgrade = api_request("POST", "/v1.43/build", b"");
        let header_end = upgrade
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        upgrade.splice(
            header_end + 2..header_end + 2,
            b"Connection: Upgrade\r\nUpgrade: h2c\r\n".iter().copied(),
        );
        let error = policy
            .authorize(&upgrade)
            .expect_err("generic Docker build upgrade must be denied");
        assert!(error.to_string().contains("upgrade/tunnel"), "{error:#}");
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/_ping", b""))
            .is_ok());

        let mut owned_upgrade = api_request("GET", "/v1.43/containers/velnor-job-owned/json", b"");
        let header_end = owned_upgrade
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        owned_upgrade.splice(
            header_end + 2..header_end + 2,
            b"Connection: Upgrade\r\nUpgrade: h2c\r\n".iter().copied(),
        );
        let error = policy
            .authorize(&owned_upgrade)
            .expect_err("inspection must not become a generic upgrade tunnel");
        assert!(error.to_string().contains("upgrade/tunnel"), "{error:#}");
    }

    #[test]
    fn lease_policy_registers_only_successful_create_response_ids() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let create = api_request("POST", "/v1.43/containers/create", b"{}");
        assert_eq!(
            policy.authorize(&create).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
        );
        policy
            .record_create_response(DockerResourceKind::Container, 500, br#"{"Id":"bad"}"#)
            .unwrap();
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/containers/bad/json", b""))
            .is_err());
        policy
            .record_create_response(
                DockerResourceKind::Container,
                201,
                br#"{"Id":"created-container"}"#,
            )
            .unwrap();
        assert!(policy
            .authorize(&api_request(
                "GET",
                "/v1.43/containers/created-container/json",
                b""
            ))
            .is_ok());
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/containers/foreign/json", b""))
            .is_err());
    }

    #[test]
    fn lease_policy_allows_delete_only_for_registered_resources() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        assert_eq!(
            policy
                .authorize(&api_request(
                    "DELETE",
                    "/v1.43/containers/velnor-job-owned",
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::Owned(DockerResourceKind::Container)
        );
        policy
            .record_create_response(DockerResourceKind::Network, 201, br#"{"Id":"net-owned"}"#)
            .unwrap();
        policy
            .record_create_response(DockerResourceKind::Volume, 201, br#"{"Name":"vol-owned"}"#)
            .expect_err("volume ownership requires an attested Docker response");
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#,
                Some("vol-owned"),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .unwrap();
        assert!(policy
            .authorize(&api_request("DELETE", "/v1.43/networks/net-owned", b""))
            .is_ok());
        assert!(policy
            .authorize(&api_request("DELETE", "/v1.43/volumes/vol-owned", b""))
            .is_ok());
        assert!(policy
            .authorize(&api_request("DELETE", "/v1.43/volumes/foreign", b""))
            .is_err());

        policy
            .record_owned_volume_inspect(
                "vol-owned",
                200,
                br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-other","velnor.daemon-id":"daemon-a"}}"#,
                "velnor-job-owned",
                "daemon-a",
            )
            .expect_err("a same-name replacement must revoke the stale volume capability");
        assert!(policy
            .authorize(&api_request("DELETE", "/v1.43/volumes/vol-owned", b""))
            .is_err());
    }

    #[test]
    fn lease_policy_delete_by_id_revokes_container_name_alias() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let response = br#"{"Id":"container-owned"}"#;
        policy
            .record_create_response(DockerResourceKind::Container, 201, response)
            .unwrap();
        policy
            .note_container_name("container-alias", 201, response)
            .unwrap();
        assert!(policy
            .authorize(&api_request(
                "GET",
                "/v1.43/containers/container-alias/json",
                b""
            ))
            .is_ok());
        policy
            .record_delete_response(DockerResourceKind::Container, "container-owned", 204)
            .unwrap();
        assert!(policy
            .authorize(&api_request(
                "GET",
                "/v1.43/containers/container-alias/json",
                b""
            ))
            .is_err());
    }

    #[test]
    fn lease_policy_rewrites_authorized_container_alias_to_immutable_id() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let response = br#"{"Id":"container-owned"}"#;
        policy
            .record_create_response(DockerResourceKind::Container, 201, response)
            .unwrap();
        policy
            .note_container_name("container-alias", 201, response)
            .unwrap();
        let request = api_request(
            "DELETE",
            "/v1.43/containers/container-alias?force=true",
            b"",
        );
        let authorization = policy.authorize(&request).unwrap();
        let rewritten = policy
            .rewrite_authorized_alias_target(&request, authorization)
            .unwrap();
        let rewritten = String::from_utf8(rewritten).unwrap();
        assert!(rewritten.contains("DELETE /v1.43/containers/container-owned?force=true"));
        assert!(!rewritten.contains("container-alias"));

        policy
            .record_delete_response(DockerResourceKind::Container, "container-owned", 204)
            .unwrap();
        let authorization = policy
            .authorize(&request)
            .expect_err("revoked alias must not authorize a replacement");
        assert!(authorization.to_string().contains("foreign"));
    }

    #[test]
    fn lease_policy_attests_persistent_buildkit_before_lifecycle_use() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let container = "buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0";
        policy
            .allow_persistent_builder(persistent_buildkit_builder_name(container).unwrap())
            .unwrap();
        policy
            .register_persistent_builder_image(
                persistent_buildkit_builder_name(container).unwrap(),
                "sha256:persistent-image",
            )
            .unwrap();
        let volume = format!("{container}_state");
        let inspect = format!(
            r#"{{"Id":"persistent-id","Image":"sha256:persistent-image","Name":"/{container}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"velnor-job-old","velnor.daemon-id":"daemon-a"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}"#
        );
        let unsafe_config = inspect.replace(
            "\"Labels\":",
            "\"Healthcheck\":{\"Test\":[\"CMD-SHELL\",\"touch /tmp/unsafe\"]},\"Labels\":",
        );
        policy
            .record_persistent_container_inspect(
                container,
                200,
                unsafe_config.as_bytes(),
                "daemon-a",
            )
            .expect_err("reused BuildKit containers cannot carry a healthcheck command");
        assert_eq!(
            policy
                .authorize(&api_request(
                    "GET",
                    &format!("/v1.43/containers/{container}/json"),
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        );
        policy
            .record_persistent_container_inspect(container, 200, inspect.as_bytes(), "daemon-a")
            .expect("persistent BuildKit inspect should attest");
        let mount_request = api_request(
            "POST",
            "/v1.43/containers/create?name=buildkit-node",
            format!(
                r#"{{"Image":"moby/buildkit:buildx-stable-1","HostConfig":{{"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}}}"#
            )
            .as_bytes(),
        );
        assert!(
            policy.authorize(&mount_request).is_err(),
            "container inspect alone must not register the persistent state volume"
        );
        assert_eq!(
            policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/persistent-id/start",
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
        );
        assert!(policy
            .authorize(&api_request(
                "DELETE",
                &format!("/v1.43/containers/{container}"),
                b""
            ))
            .is_err());

        let volume_inspect = format!(
            r#"{{"Name":"{volume}","Driver":"local","Options":{{}},"Labels":{{"velnor.job-id":"velnor-job-old","velnor.daemon-id":"daemon-a"}}}}"#
        );
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                volume_inspect.as_bytes(),
                Some(&volume),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .expect("persistent volume reuse must attest before registration");
        assert!(policy
            .authorize(&api_request(
                "GET",
                &format!("/v1.43/volumes/{volume}"),
                b""
            ))
            .is_err());
        policy
            .record_persistent_volume_inspect(&volume, 200, volume_inspect.as_bytes(), "daemon-a")
            .expect("persistent state volume inspect should attest");

        let foreign = inspect.replace("daemon-a", "host-daemon");
        policy
            .record_persistent_container_inspect(container, 200, foreign.as_bytes(), "daemon-a")
            .expect_err("host-managed BuildKit-shaped container must fail attestation");
        assert!(policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/persistent-id/start",
                b""
            ))
            .is_err());
    }

    #[test]
    fn persistent_buildkit_access_is_exactly_current_job_builder_scoped() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let current = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        let foreign = "velnor-builder-shared-unbounded-v1-trusted-release-o_r";
        policy.allow_persistent_builder(current).unwrap();
        let current_container = format!("buildx_buildkit_{current}0");
        let foreign_container = format!("buildx_buildkit_{foreign}0");
        assert!(policy
            .authorize(&api_request(
                "GET",
                &format!("/v1.43/containers/{current_container}/json"),
                b"",
            ))
            .is_ok());
        assert!(policy
            .authorize(&api_request(
                "GET",
                &format!("/v1.43/containers/{foreign_container}/json"),
                b"",
            ))
            .is_err());

        let foreign_volume = format!("{foreign_container}_state");
        let foreign_inspect = format!(
            r#"{{"Name":"{foreign_volume}","Driver":"local","Labels":{{"velnor.job-id":"other-job","velnor.daemon-id":"daemon-a"}}}}"#
        );
        policy
            .record_persistent_volume_inspect(
                &foreign_volume,
                200,
                foreign_inspect.as_bytes(),
                "daemon-a",
            )
            .expect_err("foreign builder state must fail exact allowlist attestation");
    }

    #[test]
    fn host_persistent_volume_projection_registers_before_buildx_mount() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        policy.allow_persistent_builder(builder).unwrap();
        let volume = format!("buildx_buildkit_{builder}0_state");
        // This is the exact four-field TSV shape emitted by
        // inspect_volume_identity_args, including JSON-encoded fields.
        let projection = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.daemon-id\":\"daemon-a\"}}\t{{}}\n"
        );
        policy
            .record_persistent_volume_projection(&volume, projection.as_bytes(), "daemon-a")
            .expect("host inspect projection should register the attested state volume");
        let omitted_options = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.daemon-id\":\"daemon-a\"}}\n"
        );
        policy
            .record_persistent_volume_projection(&volume, omitted_options.as_bytes(), "daemon-a")
            .expect("Docker's omitted Options field is the empty local default");
        let null_options = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.daemon-id\":\"daemon-a\"}}\tnull\n"
        );
        policy
            .record_persistent_volume_projection(&volume, null_options.as_bytes(), "daemon-a")
            .expect("Docker's null Options field is the empty local default");

        assert!(policy
            .authorize(&api_request(
                "GET",
                &format!("/v1.43/volumes/{volume}"),
                b""
            ))
            .is_err());
    }

    #[test]
    fn persistent_builder_capability_waits_for_image_and_volume_setup() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        let volume = format!("buildx_buildkit_{builder}0_state");
        policy.begin_persistent_builder_setup(builder).unwrap();
        policy
            .register_persistent_builder_image(builder, "sha256:buildkit-image")
            .unwrap();
        policy
            .record_persistent_volume_projection(
                &volume,
                format!(
                    "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"old-job\",\"velnor.daemon-id\":\"daemon-a\"}}\t{{}}\n"
                )
                .as_bytes(),
                "daemon-a",
            )
            .unwrap();
        let image_pull = api_request(
            "POST",
            "/v1.43/images/create?fromImage=moby%2Fbuildkit&tag=buildx-stable-1",
            b"",
        );
        assert!(policy.authorize(&image_pull).is_err());
        policy.complete_persistent_builder_setup(builder).unwrap();
        assert_eq!(
            policy.authorize(&image_pull).unwrap(),
            AuthorizedDockerRoute::PersistentImagePull
        );
    }

    #[test]
    fn persistent_exec_response_failure_cannot_fall_through_owned() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request("POST", "/v1.43/exec/exec-id/start", b"");
        assert!(policy.authorize(&request).is_err());
        assert!(policy
            .note_persistent_exec(
                201,
                br#"{"unexpected":true}"#,
                "velnor-builder-shared-unbounded-v1-trusted-branch-o_r"
            )
            .is_err());
        assert!(policy.authorize(&request).is_err());
        policy
            .note_persistent_exec(
                201,
                br#"{"Id":"exec-id"}"#,
                "velnor-builder-shared-unbounded-v1-trusted-branch-o_r",
            )
            .unwrap();
        let mut upgrade_request = request.clone();
        let header_end = upgrade_request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        upgrade_request.splice(
            header_end + 2..header_end + 2,
            b"Connection: Upgrade\r\nUpgrade: tcp\r\n".iter().copied(),
        );
        assert_eq!(
            policy.authorize(&upgrade_request).unwrap(),
            AuthorizedDockerRoute::PersistentExec
        );
    }

    #[test]
    fn persistent_volume_attestation_rejects_local_bind_options() {
        let builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        let volume = format!("buildx_buildkit_{builder}0_state");
        let allowed = BTreeSet::from([builder.to_owned()]);
        let hostile = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"old-job\",\"velnor.daemon-id\":\"daemon-a\"}}\t{{\"type\":\"none\",\"o\":\"bind\",\"device\":\"/\"}}\n"
        );
        assert!(attest_persistent_buildkit_volume_projection(
            &hostile, &volume, "daemon-a", &allowed,
        )
        .is_err());
    }

    #[test]
    fn persistent_buildkit_archive_matches_buildx_copy_to_container_shape() {
        use std::io::Cursor;

        let mut builder = tar::Builder::new(Vec::new());
        let mut directory = tar::Header::new_gnu();
        directory.set_path("buildkit/").unwrap();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o755);
        directory.set_size(0);
        directory.set_cksum();
        builder
            .append(&directory, Cursor::new(Vec::<u8>::new()))
            .unwrap();

        let config = b"# formatting is intentionally different\n[registry.\"docker.io\"]\nmirrors = [\"mirror.gcr.io\"]\n";
        let mut file = tar::Header::new_gnu();
        file.set_path("buildkit/buildkitd.toml").unwrap();
        file.set_entry_type(tar::EntryType::Regular);
        file.set_mode(0o644);
        file.set_size(config.len() as u64);
        file.set_cksum();
        builder.append(&file, Cursor::new(config.to_vec())).unwrap();
        let archive = builder.into_inner().unwrap();
        validate_persistent_buildkit_tar(&archive).unwrap();

        let mut request = format!(
            "PUT /v1.43/containers/builder/archive?path=%2Fetc&noOverwriteDirNonDir=true HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            archive.len()
        )
        .into_bytes();
        request.extend_from_slice(&archive);
        validate_persistent_archive_request(
            &request,
            "/v1.43/containers/builder/archive?path=%2Fetc&noOverwriteDirNonDir=true",
        )
        .unwrap();

        let empty = vec![0_u8; 1024];
        validate_persistent_buildkit_tar(&empty).unwrap();
        assert!(validate_persistent_archive_request(
            &{
                let mut request = format!(
                    "PUT /v1.43/containers/builder/archive?path=%2Fetc&noOverwriteDirNonDir=true HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                    empty.len()
                )
                .into_bytes();
                request.extend_from_slice(&empty);
                request
            },
            "/v1.43/containers/builder/archive?path=%2Fetc&noOverwriteDirNonDir=true",
        )
        .is_ok());
        assert!(validate_persistent_archive_request(
            &request,
            "/v1.43/containers/builder/archive?path=%2Fetc%2Fbuildkit&noOverwriteDirNonDir=true",
        )
        .is_err());
    }

    #[test]
    fn lease_policy_denies_global_container_list() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let error = policy
            .authorize(&api_request("GET", "/v1.43/containers/json?all=1", b""))
            .expect_err("global container listing would expose other leases");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("global container listing must return a Docker denial");
        assert_eq!(deny.status, 404);
    }

    #[test]
    fn lease_policy_allows_namespaced_image_inspect() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        policy.allow_persistent_builder(builder).unwrap();
        policy
            .register_persistent_builder_image(builder, "sha256:buildkit-image")
            .unwrap();
        assert_eq!(
            policy
                .authorize(&api_request(
                    "GET",
                    "/v1.55/images/moby/buildkit:buildx-stable-1/json",
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::PersistentImageInspect
        );
    }

    #[test]
    fn persistent_image_pull_requires_the_immutable_approved_digest() {
        let digest_target = format!(
            "/v1.55/images/create?fromImage={}",
            PERSISTENT_BUILDKIT_REPO_DIGEST.replace('/', "%2F")
        );
        validate_persistent_image_pull_request(&digest_target).unwrap();
        let digest = PERSISTENT_BUILDKIT_REPO_DIGEST
            .split_once('@')
            .map_or("", |(_, digest)| digest);
        validate_persistent_image_pull_request(&format!(
            "/v1.55/images/create?fromImage=moby/buildkit&tag={digest}"
        ))
        .unwrap();
        validate_persistent_image_pull_request(
            "/v1.55/images/create?fromImage=moby/buildkit&tag=buildx-stable-1",
        )
        .unwrap();
        assert!(validate_persistent_image_pull_request(
            "/v1.55/images/create?fromImage=attacker.example/buildkit&tag=latest"
        )
        .is_err());

        let request = api_request(
            "POST",
            "/v1.55/images/create?fromImage=moby/buildkit&tag=buildx-stable-1",
            b"",
        );
        let rewritten = rewrite_persistent_image_pull_target(&request).unwrap();
        let rewritten = String::from_utf8(rewritten).unwrap();
        assert!(rewritten.contains("fromImage=moby/buildkit&tag=sha256:"));
        assert!(!rewritten.contains("tag=buildx-stable-1"));
    }

    #[test]
    fn build_target_reserves_all_docker_hub_buildkit_aliases() {
        for reference in [
            "moby/buildkit:buildx-stable-1",
            "docker.io/moby/buildkit:buildx-stable-1",
            "index.docker.io/moby/buildkit:buildx-stable-1",
            "registry-1.docker.io/moby/buildkit@sha256:deadbeef",
            "INDEX.DOCKER.IO/MOBY/BUILDKIT",
        ] {
            let target = format!("/v1.55/build?t={reference}");
            assert!(
                validate_build_request_target(&target).is_err(),
                "BuildKit alias was not reserved: {reference}"
            );
        }
        validate_build_request_target("/v1.55/build?t=example/buildkit:latest").unwrap();
    }

    #[test]
    fn container_create_name_parser_rejects_ambiguous_or_malformed_aliases() {
        let duplicate = api_request(
            "POST",
            "/v1.43/containers/create?name=approved&name=foreign",
            b"{}",
        );
        assert!(containers_create_query_name(&duplicate).is_err());
        let missing_value =
            api_request("POST", "/v1.43/containers/create?name=approved&name", b"{}");
        assert!(containers_create_query_name(&missing_value).is_err());
        let encoded = api_request(
            "POST",
            "/v1.43/containers/create?name=buildx%5Fbuilder",
            b"{}",
        );
        assert_eq!(
            containers_create_query_name(&encoded).unwrap().as_deref(),
            Some("buildx_builder")
        );
        let encoded_separator = api_request(
            "POST",
            "/v1.43/containers/create?name=buildx%2Fbuilder",
            b"{}",
        );
        assert!(containers_create_query_name(&encoded_separator).is_err());
    }

    #[test]
    fn lease_policy_binds_exec_to_an_owned_container() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let body = br#"{"AttachStdout":true}"#;
        let create = api_request("POST", "/v1.43/containers/velnor-job-owned/exec", body);
        assert_eq!(
            policy.authorize(&create).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Exec)
        );
        policy
            .record_create_response(DockerResourceKind::Exec, 201, br#"{"Id":"exec-owned"}"#)
            .unwrap();
        assert!(policy
            .authorize(&api_request("POST", "/v1.43/exec/exec-owned/start", b"{}"))
            .is_ok());
        assert!(policy
            .authorize(&api_request("POST", "/v1.43/exec/foreign/start", b"{}"))
            .is_err());
    }

    #[test]
    fn lease_policy_requires_owned_container_for_network_connect() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response(DockerResourceKind::Network, 201, br#"{"Id":"net-owned"}"#)
            .unwrap();
        let foreign = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"foreign"}"#,
        );
        assert!(policy.authorize(&foreign).is_err());
        let owned = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"velnor-job-owned"}"#,
        );
        assert!(policy.authorize(&owned).is_ok());
        let duplicate = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"velnor-job-owned","Container":"foreign"}"#,
        );
        assert!(policy.authorize(&duplicate).is_err());
    }

    #[test]
    fn lease_policy_rejects_encoded_route_separators() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request("GET", "/v1.43/containers/%2fetc/json", b"");
        let error = policy
            .authorize(&request)
            .expect_err("encoded separators must not reach Docker");
        assert!(error.to_string().contains("encoded path separator"));
    }

    #[test]
    fn container_create_tolerates_empty_unknown_hostconfig_defaults() {
        // Live API 1.55 CLI default serialization: unknown HostConfig fields
        // arrive as empty defaults and grant no host control. `BlkioWeight: 0`
        // is the one numeric default explicitly admitted below.
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"BlkioDeviceReadBps":[],"BlkioDeviceWriteBps":[],"BlkioWeightDevice":[],"BlkioWeight":0,"ConsoleSize":[0,0],"IOMaximumBandwidth":0,"DeviceRequests":[],"NetworkMode":"none","AutoRemove":true}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");
    }

    #[test]
    fn container_create_allows_testcontainers_published_ports() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        for body in [
            br#"{"Image":"postgres:18-alpine","HostConfig":{"AutoRemove":true,"NetworkMode":"bridge","PublishAllPorts":true}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"PortBindings":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"0"}]},"NetworkMode":"bridge"}}"#
                .as_slice(),
        ] {
            let request = api_request("POST", "/v1.43/containers/create?name=tc-pg", body);
            let result = policy.authorize(&request);
            assert!(
                result.is_ok(),
                "testcontainers port publish must be a labeled lease create, not a host-control deny: {result:#?}"
            );
        }
    }

    #[test]
    fn container_create_empty_device_requests_are_absent() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"DeviceRequests":[{"Driver":"","Count":0,"DeviceIDs":[],"Capabilities":[],"Options":{}}]}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"DeviceRequests":[{"Driver":"nvidia","Count":1}]}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("populated DeviceRequests is host control");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("DeviceRequests denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("DeviceRequests"));
        assert!(deny.message.contains("nvidia"));
    }

    #[test]
    fn container_create_gpu_requests_require_exact_buildkit_shape() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        for (request, expected_fragment) in [
            (
                br#"{"Driver":"","Count":1,"DeviceIDs":[],"Capabilities":[["gpu"]],"Options":{}}"#
                    .as_slice(),
                "DeviceRequests",
            ),
            (
                br#"{"Driver":"nvidia","Count":-1,"DeviceIDs":[],"Capabilities":[["gpu"]],"Options":{}}"#
                    .as_slice(),
                "DeviceRequests",
            ),
            (
                br#"{"Driver":"","Count":-1,"DeviceIDs":["GPU-1"],"Capabilities":[["gpu"]],"Options":{}}"#
                    .as_slice(),
                "DeviceRequests",
            ),
            (
                br#"{"Driver":"","Count":-1,"DeviceIDs":[],"Capabilities":[["gpu"]],"Options":{"foo":"bar"}}"#
                    .as_slice(),
                "DeviceRequests",
            ),
            (
                br#"{"Driver":"","Count":-1,"DeviceIDs":[],"Capabilities":[["gpu","nvidia"]],"Options":{}}"#
                    .as_slice(),
                "DeviceRequests",
            ),
            (
                br#"{"Driver":"","Count":-1,"DeviceIDs":[],"Capabilities":[["gpu"]],"Options":{},"FutureField":{}}"#
                    .as_slice(),
                "FutureField",
            ),
        ] {
            let body = format!(
                r#"{{"Image":"busybox:1.36","HostConfig":{{"DeviceRequests":[{}]}}}}"#,
                std::str::from_utf8(request).unwrap()
            );
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                    body.as_bytes(),
                ))
                .expect_err("non-BuildKit GPU request must fail closed");
            assert!(error.to_string().contains(expected_fragment), "{error:#}");
        }
    }

    #[test]
    fn container_create_live_cli_restart_policy_no_is_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"MaximumRetryCount":0,"Name":"no"}}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"always"}}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("RestartPolicy always is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"on-failure","MaximumRetryCount":3}}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("RestartPolicy on-failure is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"no","MaximumRetryCount":0,"FutureField":0}}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("unknown RestartPolicy fields must fail closed");
        assert!(error.to_string().contains("FutureField"), "{error:#}");
    }

    #[test]
    fn container_create_live_buildx_gpu_and_volume_mounts_are_guest() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder =
            "velnor-builder-shared-unbounded-v1-trusted-branch-tailrocks_velnor-actions-fixture";
        let volume = format!("buildx_buildkit_{builder}0_state");
        policy.allow_persistent_builder(builder).unwrap();
        policy
            .record_persistent_volume_inspect(
                &volume,
                200,
                format!(
                    r#"{{"Name":"{volume}","Driver":"local","Options":{{}},"Labels":{{"velnor.job-id":"velnor-job-old","velnor.daemon-id":"daemon-a"}}}}"#
                )
                .as_bytes(),
                "daemon-a",
            )
            .unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            format!(r#"{{"Image":"moby/buildkit:buildx-stable-1","HostConfig":{{"Privileged":true,"RestartPolicy":{{"MaximumRetryCount":0,"Name":"unless-stopped"}},"DeviceRequests":[{{"Capabilities":[["gpu"]],"Count":-1,"DeviceIDs":null,"Driver":"","Options":{{}}}}],"Mounts":[{{"Source":"{volume}","Target":"/var/lib/buildkit","Type":"volume"}}]}}}}"#).as_bytes(),
        );
        let result = policy.authorize(&request);
        assert!(
            result.is_err(),
            "generic creates may not mount the shared persistent BuildKit state volume"
        );

        policy
            .register_persistent_builder_image(builder, "sha256:buildkit-image")
            .unwrap();
        let bootstrap = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}0"),
            format!(
                r#"{{"Image":"moby/buildkit:buildx-stable-1","Env":null,"Entrypoint":null,"Cmd":[],"Labels":null,"HostConfig":{{"Privileged":true,"RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}},"Init":true,"NetworkMode":"bridge","Mounts":[{{"Source":"{volume}","Target":"/var/lib/buildkit","Type":"volume"}}]}}}}"#
            )
            .as_bytes(),
        );
        assert_eq!(
            policy.authorize(&bootstrap).unwrap(),
            AuthorizedDockerRoute::PersistentBootstrap
        );
        let wire_defaults = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}0"),
            format!(
                r#"{{"Hostname":"","Domainname":"","User":"","AttachStdin":false,"AttachStdout":false,"AttachStderr":false,"Tty":false,"OpenStdin":false,"StdinOnce":false,"Env":null,"Cmd":null,"Healthcheck":null,"ArgsEscaped":false,"Image":"moby/buildkit:buildx-stable-1","Volumes":null,"WorkingDir":"","Entrypoint":null,"NetworkDisabled":false,"MacAddress":"","OnBuild":null,"Labels":null,"StopSignal":"SIGTERM","StopTimeout":null,"Shell":null,"HostConfig":{{"Privileged":true,"RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}},"Mounts":[{{"Source":"{volume}","Target":"/var/lib/buildkit","Type":"volume"}}],"Init":true,"NetworkMode":"bridge","CgroupParent":"/docker/buildx","UsernsMode":"host"}},"NetworkingConfig":null}}"#
            )
            .as_bytes(),
        );
        assert_eq!(
            policy.authorize(&wire_defaults).unwrap(),
            AuthorizedDockerRoute::PersistentBootstrap
        );
        let rewritten = policy
            .rewrite_docker_api_request_for_route(
                &bootstrap,
                "velnor-job-owned",
                "daemon-a",
                AuthorizedDockerRoute::PersistentBootstrap,
            )
            .unwrap();
        let rewritten = String::from_utf8(rewritten).unwrap();
        assert!(rewritten.contains("\"Privileged\":true"));
        assert!(rewritten.contains(JOB_ID_LABEL));
        assert!(rewritten.contains("\"Image\":\"sha256:buildkit-image\""));

        let untrusted_image = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            format!(r#"{{"Image":"moby/buildkit-malicious:latest","HostConfig":{{"Mounts":[{{"Source":"{volume}","Target":"/var/lib/buildkit","Type":"volume"}}]}}}}"#).as_bytes(),
        );
        assert!(
            policy.authorize(&untrusted_image).is_err(),
            "persistent state volume exception must stay bound to the BuildKit image"
        );
    }

    #[test]
    fn container_create_privileged_stays_denied_except_buildkit() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=job-container",
                br#"{"Image":"busybox:1.36","HostConfig":{"Privileged":true}}"#,
            ))
            .expect_err("privileged busybox is host control");
        assert!(error.to_string().contains("Privileged"), "{error:#}");

        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=job-container",
                br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"MaximumRetryCount":0,"Name":"unless-stopped"}}}"#,
            ))
            .expect_err("unless-stopped busybox is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");
    }

    #[test]
    fn container_create_named_volumes_require_exact_mount_shape() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"runner-owned-volume","Driver":"local","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#,
                Some("runner-owned-volume"),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .expect("attested volume create should register the lease-owned volume");

        let valid = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"Mounts":[{"Source":"runner-owned-volume","Target":"/data","Type":"volume","ReadOnly":true,"Consistency":"consistent"}]}}"#,
        );
        assert!(
            policy.authorize(&valid).is_ok(),
            "named volume with exact guest shape should be admitted"
        );
        policy
            .rewrite_docker_api_request(&valid, "job-a", "daemon-a")
            .expect("production rewrite must admit the same named volume");
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"foreign-volume","Driver":"local","Labels":{"velnor.job-id":"velnor-job-other","velnor.daemon-id":"daemon-a"}}"#,
                Some("foreign-volume"),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .expect_err("an existing other-job volume must not be claimed");
        for response in [
            br#"{"Name":"host-volume","Driver":"local","Labels":{}}"#.as_slice(),
            br#"{"Name":"host-volume","Driver":"nfs","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#.as_slice(),
        ] {
            policy
                .record_create_response_with_lease(
                    DockerResourceKind::Volume,
                    201,
                    response,
                    Some("host-volume"),
                    Some("velnor-job-owned"),
                    Some("daemon-a"),
                )
                .expect_err("host-managed volume identity must not be claimed");
        }

        for source in ["other-job-volume", "host-managed-volume"] {
            let body = format!(
                r#"{{"Image":"busybox:1.36","HostConfig":{{"Mounts":[{{"Source":"{source}","Target":"/data","Type":"volume"}}]}}}}"#,
            );
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                    body.as_bytes(),
                ))
                .expect_err("foreign or host-managed named volume must be denied");
            assert!(error.to_string().contains("Mounts"), "{error:#}");
        }

        for (field, value) in [
            ("VolumeOptions", r#"{"NoCopy":false}"#),
            ("DriverConfig", r#"{"Name":"local"}"#),
            ("FutureField", "0"),
        ] {
            let body = format!(
                r#"{{"Image":"busybox:1.36","HostConfig":{{"Mounts":[{{"Source":"runner-owned-volume","Target":"/data","Type":"volume","{field}":{value}}}]}}}}"#,
            );
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                    body.as_bytes(),
                ))
                .expect_err("unsafe or unknown volume mount fields must fail closed");
            assert!(error.to_string().contains(field), "{error:#}");
        }

        let host_path = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"Mounts":[{"Source":"/etc","Target":"/data","Type":"volume"}]}}"#,
        );
        let error = policy
            .authorize(&host_path)
            .expect_err("host-path volume source stays host control");
        assert!(error.to_string().contains("Mounts"), "{error:#}");
    }

    #[test]
    fn unnamed_volume_create_attests_and_registers_docker_returned_name() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/volumes/create",
            br#"{"Driver":"local","Labels":{}}"#,
        );
        assert_eq!(
            policy.authorize(&request).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Volume)
        );
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"docker-generated-volume","Driver":"local","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#,
                None,
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .expect("returned Docker volume identity should be registered");
        let mount = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"Mounts":[{"Source":"docker-generated-volume","Target":"/data","Type":"volume"}]}}"#,
        );
        assert!(policy.authorize(&mount).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn volume_preflight_rejects_same_name_replacement_before_delete() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#,
                Some("vol-owned"),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .unwrap();

        let dir = unique_unix_dir("velnor-lease-volume-preflight");
        let socket_path = dir.join("engine.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let host_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&chunk[..read]);
            }
            assert!(String::from_utf8_lossy(&request).contains("/volumes/vol-owned"));
            let body = br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-other","velnor.daemon-id":"daemon-a"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            )
            .unwrap();
        });

        let error = preflight_volume_identity(
            &policy,
            &socket_path,
            "vol-owned",
            AuthorizedDockerRoute::Owned(DockerResourceKind::Volume),
            "velnor-job-owned",
            "daemon-a",
        )
        .expect_err("same-name replacement must fail immutable volume re-attestation");
        assert_eq!(error.downcast_ref::<LeaseDeny>().unwrap().status, 404);
        assert!(policy
            .authorize(&api_request("DELETE", "/v1.43/volumes/vol-owned", b""))
            .is_err());
        host_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn container_mount_preflight_rejects_replaced_owned_volume() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response_with_lease(
                DockerResourceKind::Volume,
                201,
                br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-owned","velnor.daemon-id":"daemon-a"}}"#,
                Some("vol-owned"),
                Some("velnor-job-owned"),
                Some("daemon-a"),
            )
            .unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"Mounts":[{"Type":"volume","Source":"vol-owned","Target":"/data"}]}}"#,
        );

        let dir = unique_unix_dir("velnor-lease-mount-preflight");
        let socket_path = dir.join("engine.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let host_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&chunk[..read]);
            }
            let body = br#"{"Name":"vol-owned","Driver":"local","Labels":{"velnor.job-id":"velnor-job-other","velnor.daemon-id":"daemon-a"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            )
            .unwrap();
        });

        let error = preflight_container_mounts(
            &policy,
            &socket_path,
            &request,
            "velnor-job-owned",
            "daemon-a",
        )
        .expect_err("container mount must re-attest its volume before forwarding");
        assert_eq!(error.downcast_ref::<LeaseDeny>().unwrap().status, 404);
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/volumes/vol-owned", b""))
            .is_err());
        host_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn container_create_unknown_numeric_zero_is_empty_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"IOMaximumBandwidth":0,"FutureHostControl":0}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");
    }

    #[test]
    fn container_create_unknown_recursively_empty_default_is_allowed() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"FutureHostControl":[{}]}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");
    }

    #[test]
    fn container_create_strict_controls_reject_recursively_empty_values() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        for (field, value) in [
            ("Binds", "[{}]"),
            ("Mounts", "[{}]"),
            ("Sysctls", "{\"net.ipv4.ip_forward\":0}"),
        ] {
            let body = format!(r#"{{"Image":"busybox:1.36","HostConfig":{{"{field}":{value}}}}}"#);
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                    body.as_bytes(),
                ))
                .expect_err("strict control must not inherit recursive empty semantics");
            assert!(error.to_string().contains(field), "{error:#}");
        }
    }

    #[test]
    fn container_create_rejects_unknown_numeric_nonzero_hostconfig_field() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"FutureHostControl":1}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("unknown nonzero HostConfig fields must fail closed");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("unknown numeric field denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("FutureHostControl"));
    }

    #[test]
    fn container_create_allows_only_zero_console_size_as_known_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"ConsoleSize":[0,0]}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"ConsoleSize":[80,24]}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("nonzero ConsoleSize is a capability request");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("nonzero ConsoleSize denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("ConsoleSize"));
    }

    #[test]
    fn container_create_allows_only_zero_blkio_weight_as_known_numeric_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"BlkioWeight":0}}"#,
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"BlkioWeight":100}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("nonzero BlkioWeight is host IO control");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("nonzero BlkioWeight denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("BlkioWeight"));
    }

    #[test]
    fn container_create_host_network_mode_stays_lease_deny() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"host"}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("NetworkMode host remains host control");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("create denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("NetworkMode"));
    }

    #[test]
    fn container_create_with_populated_unknown_hostconfig_field_is_lease_deny() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            br#"{"Image":"busybox:1.36","HostConfig":{"BlkioDeviceReadBps":[{"Path":"/dev/sda","Rate":1024}]}}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("populated unknown HostConfig field is a capability request");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("create denial must answer as LeaseDeny, not a dropped connection");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("BlkioDeviceReadBps"));
    }

    #[test]
    fn invalid_network_create_body_is_lease_deny() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/networks/create",
            br#"{"Name":"job-net","Driver":"host"}"#,
        );
        let error = policy
            .authorize(&request)
            .expect_err("host-driver network create must be denied");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("network create denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
    }

    fn assert_container_rms_are_singleton(calls: &[Vec<String>]) {
        for call in calls {
            let ids = docker_rm_ids(call);
            assert!(
                ids.len() <= 1,
                "docker rm batched {} ids (Engine 29 concurrent DELETE deadlocks BuildKit): {call:?}",
                ids.len()
            );
        }
    }

    #[test]
    fn force_remove_containers_serially_issues_one_id_per_rm() {
        let mut calls = Vec::new();
        force_remove_containers_serially(&["id-a".into(), "id-b".into(), "id-c".into()], |args| {
            calls.push(args.to_vec());
            assert_eq!(docker_rm_ids(args).len(), 1, "batched docker rm {args:?}");
            Ok(())
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![
                force_remove_one_container_args("id-a"),
                force_remove_one_container_args("id-b"),
                force_remove_one_container_args("id-c"),
            ]
        );
        assert_container_rms_are_singleton(&calls);
    }

    #[test]
    fn remove_containers_serially_issues_non_force_one_id_per_rm() {
        let mut calls = Vec::new();
        remove_containers_serially(&["id-a".into(), "id-b".into(), "id-c".into()], |args| {
            calls.push(args.to_vec());
            assert_eq!(docker_rm_ids(args).len(), 1, "batched docker rm {args:?}");
            assert!(!args.iter().any(|arg| arg == "--force"));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![
                remove_one_container_args("id-a"),
                remove_one_container_args("id-b"),
                remove_one_container_args("id-c"),
            ]
        );
        assert_container_rms_are_singleton(&calls);
    }

    #[test]
    fn claimed_container_rm_args_preserve_force_mode() {
        let non_force = remove_container_args(&["id-a".into(), "id-b".into()]);
        assert_eq!(
            docker_client::container_rm_args_with_claimed_ids(
                docker_client::NonEmptyDockerArgs::new(&non_force).unwrap(),
                &["id-a".into()],
            ),
            remove_container_args(&["id-a".into()])
        );
        let force = force_remove_container_args(&["id-a".into(), "id-b".into()]);
        assert_eq!(
            docker_client::container_rm_args_with_claimed_ids(
                docker_client::NonEmptyDockerArgs::new(&force).unwrap(),
                &["id-b".into()],
            ),
            force_remove_container_args(&["id-b".into()])
        );
    }

    #[test]
    fn guest_socket_path_fits_unix_sun_len() {
        let path = guest_docker_socket_host(
            "velnor-job-fa461ac2-8b9f-5ef8-9754-a1ffe47774f1",
            Path::new("/var/lib/velnor-jackin-project/work/slot-1/job/temp"),
        );
        let rendered = path.to_string_lossy();
        assert!(
            rendered.len() < 100,
            "unix socket path must fit sockaddr_un, got {rendered}"
        );
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("vdl-") && name.ends_with(".sock"),
            "got {rendered}"
        );
        let parent = path.parent().unwrap().to_string_lossy();
        assert!(
            parent.ends_with("/temp/_velnor") || parent.ends_with("\\temp\\_velnor"),
            "lease socket must live below the job temp tree visible to Docker, got {rendered}"
        );
        assert!(
            rendered.contains("/temp/_velnor/vdl-") || rendered.contains("\\temp\\_velnor\\vdl-")
        );
    }

    #[test]
    fn long_socket_path_never_falls_back_outside_the_shared_work_tree() {
        let unique = Path::new(
            "/private/tmp/velnor-host-with-a-deliberately-long-name/runner/work/slot-1/job/temp",
        );
        let path = guest_docker_socket_host("job", unique);
        let shared = crate::container::daemon_shared_root(PathBuf::from(
            "/private/tmp/velnor-host-with-a-deliberately-long-name/runner/work/slot-1",
        ));
        assert!(
            path.starts_with(&shared),
            "lease path escaped shared root: {} not below {}",
            path.display(),
            shared.display()
        );
        assert!(
            !path.starts_with(std::env::temp_dir().join("vdl-")),
            "lease path must not use an unrelated global temp fallback: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn bind_rejects_a_host_socket_path_that_exceeds_unix_limit() {
        let path = PathBuf::from("/tmp").join("x".repeat(UNIX_SOCKET_PATH_LIMIT));
        let error = DockerLeaseGuard::bind_to(
            path,
            PathBuf::from("/nonexistent-host-docker.sock"),
            "job".into(),
            "daemon".into(),
        )
        .err()
        .expect("overlong Unix socket path must fail before bind");
        assert!(error.to_string().contains("safe Unix socket limit"));
        assert!(error.to_string().contains("shorten --work-dir"));
    }

    #[test]
    fn injects_job_and_daemon_labels_into_container_create() {
        let body = br#"{"Image":"postgres:18-alpine","Labels":{"org.testcontainers.managed-by":"testcontainers"}}"#;
        let labeled =
            inject_ownership_labels(body, "velnor-job-1", "/var/lib/velnor/work/slot-1").unwrap();
        let value: Value = serde_json::from_slice(&labeled).unwrap();
        let labels = value["Labels"].as_object().unwrap();
        assert_eq!(labels["org.testcontainers.managed-by"], "testcontainers");
        assert_eq!(labels[JOB_ID_LABEL], "velnor-job-1");
        assert_eq!(labels[DAEMON_ID_LABEL], "/var/lib/velnor/work/slot-1");
    }

    #[test]
    fn injects_labels_when_create_body_omits_them() {
        let labeled = inject_ownership_labels(br#"{"Name":"net"}"#, "job-a", "daemon-a").unwrap();
        let value: Value = serde_json::from_slice(&labeled).unwrap();
        assert_eq!(value["Labels"][JOB_ID_LABEL], "job-a");
        assert_eq!(value["Labels"][DAEMON_ID_LABEL], "daemon-a");
    }

    #[test]
    fn network_create_allows_only_runner_safe_bridge_shape() {
        let body = br#"{
            "Name":"velnor-net",
            "Driver":"bridge",
            "CheckDuplicate":true,
            "Internal":false,
            "Attachable":false,
            "Ingress":false,
            "EnableIPv6":false,
            "IPAM":{"Driver":"default","Config":[],"Options":{}},
            "Options":{},
            "Labels":{"purpose":"test"}
        }"#;
        let request = api_request("POST", "/v1.43/networks/create", body);
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        assert!(matches!(
            policy.authorize(&request),
            Ok(AuthorizedDockerRoute::Create(DockerResourceKind::Network))
        ));
        let rewritten = rewrite_docker_api_request(&request, "job-a", "daemon-a").unwrap();
        let rewritten_body = docker_request_body(&rewritten).unwrap();
        let value: Value = serde_json::from_slice(rewritten_body).unwrap();
        assert_eq!(value["Driver"], "bridge");
        assert_eq!(value["Labels"][JOB_ID_LABEL], "job-a");
        assert_eq!(value["Labels"][DAEMON_ID_LABEL], "daemon-a");
    }

    #[test]
    fn network_create_rejects_host_affecting_payloads_before_forwarding() {
        let bodies = [
            br#"{"Name":"velnor-net","Driver":"host"}"#.as_slice(),
            br#"{"Name":"velnor-net","Options":{"com.docker.network.bridge.name":"docker0"}}"#
                .as_slice(),
            br#"{"Name":"velnor-net","IPAM":{"Config":[{"Subnet":"10.0.0.0/8"}]}}"#.as_slice(),
            br#"{"Name":"velnor-net","Internal":true}"#.as_slice(),
            br#"{"Name":"velnor-net","Unknown":true}"#.as_slice(),
            br#"{"Name":"velnor-net","Labels":{"velnor.job-id":"other-job"}}"#.as_slice(),
            br#"{"Name":"velnor-net","Name":"other-net"}"#.as_slice(),
        ];
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        for body in bodies {
            let request = api_request("POST", "/v1.43/networks/create", body);
            assert!(
                policy.authorize(&request).is_err(),
                "unsafe network body was authorized: {}",
                String::from_utf8_lossy(body)
            );
            assert!(
                rewrite_docker_api_request(&request, "job-a", "daemon-a").is_err(),
                "unsafe network body was rewritten: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn rejects_duplicate_json_object_keys_before_policy_rewrite() {
        let error = inject_ownership_labels(
            br#"{"Image":"postgres:18-alpine","Labels":{},"Labels":{}}"#,
            "job-a",
            "daemon-a",
        )
        .expect_err("duplicate JSON keys must fail closed");
        assert!(error.to_string().contains("duplicate JSON object key"));
    }

    #[test]
    fn materializes_nested_create_json_while_applying_policy() {
        let body = br#"{
            "Image":"postgres:18-alpine",
            "Env":["POSTGRES_DB=app",{"nested":true}],
            "Memory":12.5,
            "Nullable":null
        }"#;
        let labeled = inject_ownership_labels(body, "job-a", "daemon-a").unwrap();
        let value: Value = serde_json::from_slice(&labeled).unwrap();
        assert_eq!(value["Env"][0], "POSTGRES_DB=app");
        assert_eq!(value["Env"][1]["nested"], true);
        assert_eq!(value["Memory"], 12.5);
        assert!(value["Nullable"].is_null());
        assert_eq!(value["Labels"][JOB_ID_LABEL], "job-a");
    }

    #[test]
    fn rejects_duplicate_json_object_keys_at_nested_depths() {
        for body in [
            br#"{"HostConfig":{"Memory":1,"Memory":2}}"#.as_slice(),
            br#"{"Env":[{"name":"A","name":"B"}]}"#.as_slice(),
            br#"{"Labels":{"cache":true,"\u0063ache":false}}"#.as_slice(),
        ] {
            let error = inject_ownership_labels(body, "job-a", "daemon-a")
                .expect_err("duplicate JSON keys at any depth must fail closed");
            assert!(error.to_string().contains("duplicate JSON object key"));
        }
    }

    #[test]
    fn rewrite_injects_cgroup_parent_and_strips_nested_ceiling_policy() {
        let body = br#"{
            "Image":"postgres:18-alpine",
            "HostConfig":{
                "CgroupParent":"untrusted.slice",
                "Memory":123,
                "NanoCpus":500000000,
                "CpuQuota":50000,
                "CpusetCpus":"0-1",
                "MemorySwap":456,
                "PidsLimit":512,
                "OomKillDisable":true,
                "ShmSize":268435456
            }
        }"#;
        let request = format!(
            "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let rewritten =
            rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a").unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap();
        let value: Value = serde_json::from_str(body).unwrap();
        assert_eq!(value["HostConfig"]["CgroupParent"], JOB_CGROUP_PARENT);
        // Nested creates run unbounded: every ceiling field is stripped.
        for key in [
            "Memory",
            "NanoCpus",
            "CpuQuota",
            "CpusetCpus",
            "MemorySwap",
            "PidsLimit",
            "OomKillDisable",
        ] {
            assert!(
                value["HostConfig"].get(key).is_none(),
                "nested HostConfig must not carry {key}: {value}"
            );
        }
        // ShmSize is shared-memory sizing, not a ceiling: it survives.
        assert_eq!(value["HostConfig"]["ShmSize"], 268435456);
    }

    #[test]
    fn rewrite_rejects_malformed_nested_container_policy() {
        let body = br#"{"Image":"postgres:18-alpine","HostConfig":[]}"#;
        let request = format!(
            "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
            .expect_err("malformed HostConfig must fail closed");
        assert!(error.to_string().contains("HostConfig must be an object"));
    }

    #[test]
    fn rewrite_rejects_nested_host_control_access() {
        for body in [
            br#"{"Image":"alpine:3.20","HostConfig":{"Binds":["/var/run/docker.sock:/host.sock"]}}"#
                .as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Mounts":[{"Type":"bind","Source":"/run/docker.sock","Target":"/host.sock"}]}}"#
                .as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"NetworkMode":"host"}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"CgroupnsMode":"host"}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Privileged":true}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"CapAdd":["SYS_ADMIN"]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Devices":[{"PathOnHost":"/dev/kvm"}]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"SecurityOpt":["seccomp=unconfined"]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Binds":["/etc:/host-etc"]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Binds":["./relative:/host"]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"Mounts":[{"Type":"bind","Source":"../host","Target":"/host"}]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"VolumeDriver":"local"}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"VolumesFrom":["other"]}}"#.as_slice(),
            br#"{"Image":"alpine:3.20","HostConfig":{"UtsMode":"host"}}"#.as_slice(),
        ] {
            let request = format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("nested Docker host control access must fail closed");
            assert!(error.to_string().contains("host control access"));
        }
    }

    #[test]
    fn rewrite_allows_testcontainers_published_ports() {
        for body in [
            br#"{"Image":"postgres:18-alpine","HostConfig":{"AutoRemove":true,"NetworkMode":"bridge","PublishAllPorts":true}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"PortBindings":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"0"}]},"NetworkMode":"bridge"}}"#
                .as_slice(),
        ] {
            let request = format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect("testcontainers port publish must rewrite, not fail closed");
        }
    }

    #[test]
    fn rewrite_rejects_case_insensitive_cgroup_policy_aliases() {
        for body in [
            br#"{"Image":"postgres:18-alpine","hostconfig":{"CgroupParent":"untrusted.slice"}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"cgroupParent":"untrusted.slice"}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","Labels":{},"labels":{}}"#.as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"Mounts":[],"mounts":[]}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"Binds":[],"binds":[]}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"Mounts":[{"Type":"volume","type":"bind","Source":"/run/docker.sock"}]}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"Mounts":[{"Type":"bind","Source":"/safe","source":"/run/docker.sock"}]}}"#
                .as_slice(),
        ] {
            let request = format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("case-insensitive Docker policy aliases must fail closed");
            let message = error.to_string();
            assert!(message.contains("ambiguous") || message.contains("duplicate"));
        }
    }

    #[test]
    fn rewrite_rejects_ambiguous_or_malformed_http_framing() {
        let body = br#"{"Image":"postgres:18-alpine"}"#;
        let requests = [
            format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body.len(),
                std::str::from_utf8(body).unwrap()
            ),
            format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: nope\r\n\r\n{}",
                std::str::from_utf8(body).unwrap()
            ),
            format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\nTransfer-Encoding: chunked\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            ),
        ];
        for request in requests {
            rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("ambiguous Docker request framing must fail closed");
        }
    }

    #[test]
    fn rewrite_labels_container_create_and_passes_other_methods_through() {
        let body = br#"{"Image":"redis:5.0"}"#;
        let request = format!(
            "POST /v1.43/containers/create?name=goofy HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let rewritten =
            rewrite_docker_api_request(request.as_bytes(), "velnor-job-9", "daemon-9").unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("\"velnor.job-id\":\"velnor-job-9\""));
        assert!(text.contains("Content-Length:"));

        let ping = b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n";
        assert_eq!(
            rewrite_docker_api_request(ping, "job", "daemon").unwrap(),
            ping
        );
    }

    #[test]
    fn rewrite_canonicalizes_encoded_route_once_and_rejects_encoded_separators() {
        let body = br#"{"Image":"alpine:3.20"}"#;
        let request = format!(
            "POST /v1.43/%63ontainers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let rewritten =
            rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a").unwrap();
        assert!(String::from_utf8(rewritten)
            .unwrap()
            .contains("velnor.job-id"));
        assert!(!is_docker_object_create(
            "POST",
            "/v1.43/%2fcontainers/create"
        ));
        let encoded_separator = request.replace("%63ontainers", "%2fcontainers");
        rewrite_docker_api_request(encoded_separator.as_bytes(), "job-a", "daemon-a")
            .expect_err("encoded path separators must not bypass the policy route");
    }

    #[test]
    fn rewrite_rejects_host_volume_driver_controls() {
        for body in [
            br#"{"Name":"escape","Driver":"local","DriverOpts":{"type":"none","o":"bind","device":"/"}}"#.as_slice(),
            br#"{"Name":"escape","Driver":"local","DriverOpts":{"device":"/etc"}}"#.as_slice(),
            br#"{"Name":"escape","Driver":"host-plugin"}"#.as_slice(),
            br#"{"Name":"escape","DriverOpts":{},"driveropts":{"device":"/"}}"#.as_slice(),
        ] {
            let request = format!(
                "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("host-backed volume creation must fail closed");
            let message = error.to_string();
            assert!(
                message.contains("host control access") || message.contains("duplicate"),
                "{message}"
            );
        }
    }

    #[test]
    fn volume_create_rejects_driveropts_with_empty_nested_values() {
        let body = br#"{"Name":"cache","Driver":"local","DriverOpts":{"device":""}}"#;
        let request = format!(
            "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
            .expect_err("nonempty DriverOpts must stay a host-control request");
        assert!(error.to_string().contains("DriverOpts"), "{error:#}");
    }

    #[test]
    fn rewrite_allows_plain_local_volume_creation() {
        let body = br#"{"Name":"cache","Driver":"local","Labels":{}}"#;
        let request = format!(
            "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let rewritten = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
            .expect("plain local volume creation is safe");
        assert!(String::from_utf8(rewritten)
            .unwrap()
            .contains("velnor.job-id"));
    }

    #[test]
    fn request_keepalive_policy_honors_http_versions_and_close_tokens() {
        assert!(!http_request_wants_close(
            b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: keep-alive\r\n\r\n"
        ));
        assert!(http_request_wants_close(
            b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n"
        ));
        assert!(http_request_wants_close(
            b"GET /_ping HTTP/1.0\r\nHost: docker\r\n\r\n"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn request_is_upgrade_requires_supported_docker_hijack_headers() {
        assert!(request_is_upgrade(
            b"POST /v1.43/containers/abc/attach HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n"
        ));
        assert!(request_is_upgrade(
            b"POST /v1.43/build HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: h2c\r\n\r\n"
        ));
        assert!(!request_is_upgrade(
            b"POST /v1.43/containers/abc/attach HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: bogus\r\n\r\n"
        ));
        assert!(!request_is_upgrade(
            b"POST /v1.43/containers/abc/attach HTTP/1.1\r\nUpgrade: tcp\r\n\r\n"
        ));
        let mut binary_body =
            b"POST /v1.43/containers/abc/attach HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Length: 2\r\n\r\n"
                .to_vec();
        binary_body.extend_from_slice(&[0xff, 0xfe]);
        assert!(request_is_upgrade(&binary_body));
    }

    #[cfg(unix)]
    #[test]
    fn read_http_request_batches_reads_and_retains_pipeline_bytes() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let first = b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n";
        let second = b"POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: 2\r\n\r\n{}";
        writer
            .write_all(&[first.as_slice(), second.as_slice()].concat())
            .unwrap();

        let request = read_http_request(&mut reader).unwrap();
        assert_eq!(request.bytes, first);
        assert_eq!(request.remainder, second);
    }

    #[cfg(unix)]
    #[test]
    fn read_http_request_decodes_chunked_body_and_retains_pipeline_bytes() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let body = br#"{"Name":"cache"}"#;
        let split = 5;
        let second = b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n";
        let wire = format!(
            "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n{}\r\n{:X}\r\n{}\r\n0\r\nX-Ignored: trailer\r\n\r\n{}",
            split,
            std::str::from_utf8(&body[..split]).unwrap(),
            body.len() - split,
            std::str::from_utf8(&body[split..]).unwrap(),
            std::str::from_utf8(second).unwrap(),
        );
        writer.write_all(wire.as_bytes()).unwrap();

        let request = read_http_request(&mut reader).unwrap();
        let expected = format!(
            "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap(),
        );
        assert_eq!(request.bytes, expected.as_bytes());
        assert_eq!(request.remainder, second);
    }

    #[cfg(unix)]
    #[test]
    fn read_http_request_acknowledges_expect_continue_before_body_completion() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let body = br#"{"Name":"cache"}"#;
        let request = format!(
            "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        writer.write_all(request.as_bytes()).unwrap();

        let parsed = read_http_request(&mut reader).unwrap();
        let mut acknowledgement = vec![0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
        writer.read_exact(&mut acknowledgement).unwrap();
        assert_eq!(acknowledgement, b"HTTP/1.1 100 Continue\r\n\r\n");
        assert_eq!(
            without_expect_continue(&parsed.bytes).unwrap(),
            format!(
                "POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            )
            .as_bytes()
        );
    }

    /// Scripted [`CommandRunner`](crate::executor::CommandRunner) for the
    /// list phase: the Engine route defaults off in tests, so the facade
    /// runs its historical CLI legs through here.
    struct ScriptRunner {
        outputs: std::collections::VecDeque<String>,
        calls: Vec<Vec<String>>,
    }

    impl crate::executor::CommandRunner for ScriptRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::executor::CommandResult> {
            assert_eq!(program, "docker");
            self.calls.push(args.to_vec());
            let stdout = self.outputs.pop_front().expect("script exhausted");
            Ok(crate::executor::CommandResult {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn job_owned_reclaim_lists_then_removes_each_kind() {
        let mut runner = ScriptRunner {
            outputs: [
                "aaa\tguest-postgres\nbbb\tbuildx_buildkit_velnor-builder-dead0\n",
                "guest-net\n",
                "guest-vol\n",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            calls: Vec::new(),
        };
        let mut removals = Vec::new();
        {
            let mut docker = docker_client::Docker::job(&mut runner);
            let snapshot = list_job_owned("velnor-job-1", &mut docker).unwrap();
            remove_job_owned(&snapshot, |args| {
                removals.push(args.to_vec());
                Ok(())
            })
            .unwrap();
        }
        // All three listings first (the facade's historical CLI legs), then
        // the removals: phases, not interleaved calls.
        assert_eq!(
            runner.calls,
            vec![
                list_owned_containers_args("velnor-job-1"),
                list_owned_networks_args("velnor-job-1"),
                list_owned_volumes_args("velnor-job-1"),
            ]
        );
        // Same removal decisions as the closure version: the guest goes, the
        // BuildKit daemon stays for its own reclaim, network and volume go.
        assert_eq!(
            removals,
            vec![
                force_remove_container_args(&["aaa".into()]),
                force_remove_network_args(&["guest-net".into()]),
                force_remove_volume_args(&["guest-vol".into()]),
            ]
        );
    }

    #[test]
    fn remove_job_owned_skips_empty_kinds() {
        let snapshot = JobOwnedSnapshot {
            containers: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
        };
        let mut removals = Vec::new();
        remove_job_owned(&snapshot, |args| {
            removals.push(args.to_vec());
            Ok(())
        })
        .unwrap();
        assert!(removals.is_empty());
    }

    #[test]
    fn remove_job_owned_preserves_persistent_buildkit_volumes() {
        let snapshot = JobOwnedSnapshot {
            containers: Vec::new(),
            networks: Vec::new(),
            volumes: vec![
                "job-cache".into(),
                "buildx_buildkit_velnor-builder-shared-repo_state".into(),
            ],
        };
        let mut removals = Vec::new();
        remove_job_owned(&snapshot, |args| {
            removals.push(args.to_vec());
            Ok(())
        })
        .unwrap();

        assert_eq!(
            removals,
            vec![force_remove_volume_args(&["job-cache".into()])]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_skips_all_deletes_for_unknown_state() {
        let job_id = "velnor-job-unknown";
        let mut calls = Vec::new();
        let mut outputs = vec![format!("container\t{job_id}\t{job_id}\tpaused-by-engine\n")];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls, vec![list_owned_containers_state_args(job_id)]);
    }

    #[test]
    fn reclaim_stale_job_owned_skips_all_deletes_for_malformed_snapshot() {
        let job_id = "velnor-job-malformed";
        let mut calls = Vec::new();
        let mut outputs = vec![format!("container\t{job_id}\t{job_id}\n")];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls, vec![list_owned_containers_state_args(job_id)]);
    }

    #[test]
    fn reclaim_stale_job_owned_skips_network_and_volume_when_job_restarts() {
        let job_id = "velnor-job-race";
        let mut calls = Vec::new();
        let mut outputs = vec![
            format!("job-container\t{job_id}\t{job_id}\texited\n"),
            format!("job-container\t{job_id}\t{job_id}\trunning\n"),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![
                list_owned_containers_state_args(job_id),
                list_owned_containers_state_args(job_id),
            ]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_refreshes_liveness_before_volume_remove() {
        let job_id = "velnor-job-volume-live-race";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let volume = "job-cache";
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            format!("{volume}\n"),
            format!("{volume}\n"),
            format!("{volume:?}\t\"local\"\t{{\"velnor.job-id\":\"{job_id}\"}}\n"),
            format!("{volume}\n"),
            format!("{volume:?}\t\"local\"\t{{\"velnor.job-id\":\"{job_id}\"}}\n"),
            format!("{job_id}\t{job_id}\trunning\n"),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls.last().unwrap(), &list_owned_job_format_args());
        assert!(!calls
            .iter()
            .any(|call| call == &remove_volume_args(&[volume.to_owned()])));
    }

    #[test]
    fn reclaim_stale_job_owned_removes_without_force() {
        let job_id = "velnor-job-stale";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            "guest-net\n".to_string(),
            String::new(),
            "guest-vol\n".to_string(),
            "guest-vol\n".to_string(),
            "\"guest-vol\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-stale\"}\n".to_string(),
            "guest-vol\n".to_string(),
            "\"guest-vol\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-stale\"}\n".to_string(),
            "velnor-job-stale\tvelnor-job-stale\texited\n".to_string(),
            String::new(),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert!(calls
            .iter()
            .any(|call| call == &remove_container_args(&["guest-id".into()])));
        assert!(calls
            .iter()
            .any(|call| call == &force_remove_network_args(&["guest-net".into()])));
        assert!(calls
            .iter()
            .any(|call| call == &remove_volume_args(&["guest-vol".into()])));
        assert!(calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg == "--force")));
    }

    #[test]
    fn reclaim_stale_job_owned_preserves_persistent_buildkit_volumes() {
        let job_id = "velnor-job-stale-buildkit";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            "job-cache\nbuildx_buildkit_velnor-builder-shared-repo_state\n".to_string(),
            "job-cache\n".to_string(),
            "\"job-cache\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-stale-buildkit\"}\n"
                .to_string(),
            "job-cache\n".to_string(),
            "\"job-cache\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-stale-buildkit\"}\n"
                .to_string(),
            "velnor-job-stale-buildkit\tvelnor-job-stale-buildkit\texited\n".to_string(),
            String::new(),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls.last().unwrap(),
            &remove_volume_args(&["job-cache".into()])
        );
    }

    #[test]
    fn reclaim_stale_job_owned_containers_leaves_network_for_attested_cleanup() {
        let job_id = "velnor-job-partial";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![snapshot.clone(), snapshot];
        reclaim_stale_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            if args == remove_container_args(&["guest-id".into()]) {
                return Ok(String::new());
            }
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], list_owned_containers_state_args(job_id));
        assert_eq!(calls[1], list_owned_containers_state_args(job_id));
        assert_eq!(calls[2], remove_container_args(&["guest-id".into()]));
        assert!(calls
            .iter()
            .all(|call| { !call.iter().any(|arg| arg == "network" || arg == "volume") }));
    }

    #[test]
    fn reclaim_stale_job_owned_containers_reports_live_job_without_cleanup() {
        let job_id = "velnor-job-live-partial";
        let snapshot = format!("job-id\t{job_id}\t{job_id}\trunning\n");
        let mut calls = Vec::new();
        let mut outputs = vec![snapshot];
        let outcome = reclaim_stale_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(outcome, StaleJobReclaim::ProtectedLive);
        assert_eq!(calls, vec![list_owned_containers_state_args(job_id)]);
    }

    #[test]
    fn force_remove_job_owned_containers_batches_guest_rm_and_serializes_buildkit() {
        let job_id = "velnor-job-orphan";
        let listing = format!(
            "job-id\t{job_id}\t{job_id}\trunning\n\
             guest-id\tguest-container\t{job_id}\texited\n\
             bk-id\t{BUILDKIT_CONTAINER_NAME_PREFIX}deadbeef\t{job_id}\trunning\n"
        );
        let mut calls = Vec::new();
        let mut outputs = vec![listing, String::new(), String::new()];
        force_remove_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_owned_containers_state_args(job_id));
        assert_eq!(
            calls[1],
            force_remove_container_args(&["guest-id".into(), "job-id".into()])
        );
        assert_eq!(calls[2], force_remove_one_container_args("bk-id"));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn force_remove_job_owned_containers_treats_missing_as_success() {
        let job_id = "velnor-job-gone";
        let listing = format!("job-id\t{job_id}\t{job_id}\texited\n");
        let mut calls = Vec::new();
        force_remove_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            if args.first().is_some_and(|command| command == "ps") {
                return Ok(listing.clone());
            }
            Err(anyhow::Error::new(docker_client::NotFound {
                object: "job-id".to_string(),
            }))
        })
        .unwrap();
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn force_remove_job_owned_containers_removes_nothing_when_listing_fails_closed() {
        let job_id = "velnor-job-foreign";
        let mut calls = Vec::new();
        let result = force_remove_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            Ok("other-id\tother\tvelnor-job-other\trunning\n".to_string())
        });
        assert!(result.is_err());
        assert_eq!(calls, vec![list_owned_containers_state_args(job_id)]);
    }

    #[test]
    fn reclaim_stale_job_owned_live_race_fails_without_force() {
        let job_id = "velnor-job-live-race";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![snapshot.clone(), snapshot];
        let result = reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            if args == remove_container_args(&["guest-id".into()]) {
                return Err(anyhow!("container is running"));
            }
            Ok(outputs.remove(0))
        });

        assert!(result.is_err());
        assert_eq!(calls[2], remove_container_args(&["guest-id".into()]));
        assert!(!calls[2].iter().any(|arg| arg == "--force"));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn reclaim_stale_job_owned_attached_volume_race_fails_without_force() {
        let job_id = "velnor-job-volume-race";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            "attached-volume\n".to_string(),
            "attached-volume\n".to_string(),
            format!("\"attached-volume\"\t\"local\"\t{{\"velnor.job-id\":\"{job_id}\"}}\n"),
            "attached-volume\n".to_string(),
            format!("\"attached-volume\"\t\"local\"\t{{\"velnor.job-id\":\"{job_id}\"}}\n"),
            format!("{job_id}\t{job_id}\texited\n"),
        ];
        let result = reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            if args == remove_volume_args(&["attached-volume".into()]) {
                assert!(!args.iter().any(|arg| arg == "--force"));
                return Err(anyhow!("volume is attached; Docker refused non-force rm"));
            }
            Ok(outputs.remove(0))
        });

        assert!(result.is_err());
        assert!(calls
            .iter()
            .any(|call| call == &remove_volume_args(&["attached-volume".into()])));
        assert!(!calls
            .iter()
            .any(|call| { call.starts_with(&["volume".into(), "rm".into(), "--force".into()]) }));
    }

    #[test]
    fn reclaim_stale_job_owned_skips_same_name_volume_replacement() {
        let job_id = "velnor-job-volume-replaced";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let volume = "job-cache";
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            format!("{volume}\n"),
            format!("{volume}\n"),
            format!("{volume:?}\t\"local\"\t{{\"velnor.job-id\":\"{job_id}\"}}\n"),
            format!("{volume}\n"),
            format!("{volume:?}\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-foreign\"}}\n"),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert!(!calls
            .iter()
            .any(|call| call == &remove_volume_args(&[volume.to_owned()])));
    }

    #[test]
    fn orphan_job_ids_keep_live_jobs_and_reclaim_finished_guest_siblings() {
        let formatted = "\
velnor-job-live\tvelnor-job-live\trunning
guest-pg\tvelnor-job-live\trunning
guest-old\tvelnor-job-dead\trunning
velnor-job-dead\tvelnor-job-dead\texited
";
        assert_eq!(
            docker_client::orphan_job_ids(formatted),
            vec!["velnor-job-dead".to_string()]
        );
    }

    #[test]
    fn reclaim_orphan_jobs_deletes_finished_job_objects_and_keeps_live_jobs() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\trunning\nguest-old\tvelnor-job-dead\trunning\n"
                .to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\texited\n".to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\texited\n".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ];
        reclaim_orphan_jobs(|args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_owned_job_format_args());
        assert_eq!(
            calls[1],
            list_owned_containers_state_args("velnor-job-dead")
        );
        assert!(calls
            .iter()
            .any(|call| call == &remove_one_container_args("guest-old")));
        assert!(!calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "rm") && call.iter().any(|arg| arg == "--force")
        }));
        assert!(calls
            .iter()
            .any(|call| call == &list_job_buildkit_format_args()));
        assert!(calls
            .iter()
            .any(|call| call == &list_job_buildkit_volume_args()));
    }

    #[test]
    fn owned_container_ids_excluding_buildkit_keep_guests_only() {
        let formatted = "\
aaa\tguest-postgres
bbb\tbuildx_buildkit_velnor-builder-dead0
ccc\tvelnor-docker-action-velnor-job-dead
";
        assert_eq!(
            docker_client::owned_container_ids_excluding_buildkit_rows(
                &docker_client::parse_owned_container_rows(formatted)
            ),
            vec!["aaa".to_string(), "ccc".to_string()]
        );
    }

    #[test]
    fn every_maintenance_command_is_bounded_below_the_step_default() {
        let step_default = Duration::from_secs(6 * 3600);
        for args in [
            force_remove_container_args(&["id".into()]),
            vec!["volume".into(), "rm".into(), "--force".into(), "v".into()],
            vec!["ps".into(), "--all".into()],
            list_daemon_owned_job_format_args(),
        ] {
            let (op, deadline) =
                crate::docker::deadline_for(&args, docker_client::MAINTENANCE_PAYLOAD_DEADLINE);
            assert!(op.is_control_plane(), "{args:?} classified as {op}");
            assert!(deadline < step_default, "{args:?} ({op}) is unbounded");
        }
        assert_eq!(
            crate::docker::deadline_for(
                &force_remove_container_args(&["id".into()]),
                docker_client::MAINTENANCE_PAYLOAD_DEADLINE
            )
            .1,
            Duration::from_secs(20)
        );
    }

    #[test]
    fn claim_docker_container_rm_single_flights_same_id() {
        let args = force_remove_container_args(&["same-id".into()]);
        let first = docker_client::claim_docker_container_rm(&args).unwrap();
        assert_eq!(first.ids, vec!["same-id".to_string()]);
        let second = docker_client::claim_docker_container_rm(&args).unwrap();
        assert!(second.ids.is_empty());
        drop(first);
        let third = docker_client::claim_docker_container_rm(&args).unwrap();
        assert_eq!(third.ids, vec!["same-id".to_string()]);
    }

    #[test]
    fn orphan_job_buildkit_ids_keep_live_created_and_reclaim_ended_created_removing() {
        let live = docker_client::live_job_ids(
            "velnor-job-live\tvelnor-job-live\trunning\n\
             velnor-job-dead\tvelnor-job-dead\texited\n",
        );
        let formatted = "\
id-live-created\tbuildx_buildkit_velnor-builder-live0\tvelnor-job-live\t/var/lib/velnor/work/slot-1\tcreated
id-dead-created\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\tcreated
id-dead-removing\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\tremoving
id-unlabeled\tbuildx_buildkit_velnor-builder-orphan0\t\t\tcreated
id-other\tpostgres\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\trunning
";
        assert_eq!(
            docker_client::orphan_job_buildkit_ids(formatted, &live, None),
            vec![
                "id-dead-created".to_string(),
                "id-dead-removing".to_string(),
                "id-unlabeled".to_string(),
            ]
        );
    }

    #[test]
    fn daemon_scoped_buildkit_reclaim_requires_daemon_ownership() {
        let daemon = "/var/lib/velnor-fleet/work";
        let formatted = "\
owned\tbuildx_buildkit_velnor-builder-owned0\tvelnor-job-old\t/var/lib/velnor-fleet/work/slot-1\tcreated
foreign\tbuildx_buildkit_velnor-builder-foreign0\tvelnor-job-foreign\t/var/lib/velnor-other/work\tcreated
unlabeled\tbuildx_buildkit_velnor-builder-unlabeled0\tvelnor-job-unlabeled\t\tcreated
";
        assert_eq!(
            docker_client::orphan_job_buildkit_ids(formatted, &BTreeSet::new(), Some(daemon)),
            vec!["owned".to_string()]
        );
    }

    #[test]
    fn daemonless_buildkit_volume_candidates_accept_custom_names_and_skip_persistent() {
        let listed = "buildx_buildkit_velnor-builder-dead0_state\n\
            buildx_buildkit_velnor-builder-live0_state\n\
            buildx_buildkit_velnor-builder-shared-trusted-repo_state\n\
            buildx_buildkit_velnor-builder-dead-shadow0_state\n\
            buildx_buildkit_velnor-builder-requested-name-slot-3_0_state\n";
        assert_eq!(
            orphan_job_buildkit_volume_names(listed),
            vec![
                "buildx_buildkit_velnor-builder-dead-shadow0_state".to_string(),
                "buildx_buildkit_velnor-builder-dead0_state".to_string(),
                "buildx_buildkit_velnor-builder-live0_state".to_string(),
                "buildx_buildkit_velnor-builder-requested-name-slot-3_0_state".to_string(),
            ]
        );
    }

    #[test]
    fn daemonless_buildkit_volume_reclaim_rechecks_identity_before_delete() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            String::new(),
            String::new(),
            "buildx_buildkit_velnor-builder-race0_state\n\
             buildx_buildkit_velnor-builder-shared-trusted-repo_state\n"
                .to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n\
             buildx_buildkit_velnor-builder-shared-trusted-repo_state\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-dead\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-foreign\"}\n"
                .to_string(),
            String::new(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert!(calls.iter().any(|call| call
            == &inspect_volume_identity_args("buildx_buildkit_velnor-builder-race0_state")));
        assert!(!calls.iter().any(|call| {
            call == &remove_volume_args(&["buildx_buildkit_velnor-builder-race0_state".into()])
        }));
        assert!(!calls.iter().any(|call| {
            call.iter()
                .any(|arg| arg == "buildx_buildkit_velnor-builder-shared-trusted-repo_state")
                && call.first().is_some_and(|arg| arg == "volume")
                && call.get(1).is_some_and(|arg| arg == "rm")
        }));
    }

    #[test]
    fn daemonless_buildkit_volume_reclaim_uses_custom_name_label_identity() {
        let volume = "buildx_buildkit_velnor-builder-requested-name-slot-3_0_state";
        let identity =
            format!("{volume:?}\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-custom\"}}\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            String::new(),
            String::new(),
            format!("{volume}\n"),
            format!("{volume}\n"),
            identity.clone(),
            identity,
            String::new(),
            String::new(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert!(calls
            .iter()
            .any(|call| call == &remove_volume_args(&[volume.to_owned()])));
    }

    #[test]
    fn reclaim_orphan_job_buildkit_removes_created_removing_of_ended_jobs_without_force() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "id-live\tbuildx_buildkit_velnor-builder-live0\tvelnor-job-live\t\tcreated\n\
             id-created\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tcreated\n\
             id-removing\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tremoving\n"
                .to_string(),
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
            "id-live\tbuildx_buildkit_velnor-builder-live0\tvelnor-job-live\t\tcreated\n\
             id-created\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tcreated\n\
             id-removing\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tremoving\n"
                .to_string(),
            String::new(),
            String::new(),
            "buildx_buildkit_velnor-builder-dead0_state\nbuildx_buildkit_velnor-builder-live0_state\n\
             buildx_buildkit_velnor-builder-shared-trusted-repo_state\n"
                .to_string(),
            "buildx_buildkit_velnor-builder-dead0_state\nbuildx_buildkit_velnor-builder-live0_state\n\
             buildx_buildkit_velnor-builder-shared-trusted-repo_state\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-dead0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-dead\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-dead0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-dead\"}\n"
                .to_string(),
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
            String::new(),
            "\"buildx_buildkit_velnor-builder-live0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-live\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-live0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-live\"}\n"
                .to_string(),
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
        ];
        reclaim_orphan_jobs(|args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_owned_job_format_args());
        assert!(calls
            .iter()
            .any(|call| call == &list_job_buildkit_format_args()));
        assert_container_rms_are_singleton(&calls);
        assert!(calls
            .iter()
            .any(|call| call == &remove_one_container_args("id-created")));
        assert!(calls
            .iter()
            .any(|call| call == &remove_one_container_args("id-removing")));
        assert!(!calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "rm") && call.iter().any(|arg| arg == "--force")
        }));
        assert!(calls
            .iter()
            .any(|call| call == &list_job_buildkit_volume_args()));
        assert!(calls.iter().any(|call| {
            call == &remove_volume_args(&["buildx_buildkit_velnor-builder-dead0_state".into()])
                && call.contains(&"buildx_buildkit_velnor-builder-dead0_state".to_string())
                && !call.contains(&"buildx_buildkit_velnor-builder-live0_state".to_string())
        }));
        assert!(!calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "volume")
                && call.get(1).is_some_and(|arg| arg == "rm")
                && call
                    .iter()
                    .any(|arg| arg.contains("shared-trusted-repo_state"))
        }));
        assert!(!calls.iter().any(|call| {
            call.get(2) == Some(&"id-live".to_string()) && call.first().is_some_and(|a| a == "rm")
        }));
    }

    #[test]
    fn reclaim_orphan_job_buildkit_revalidates_live_jobs_before_delete() {
        let mut calls = Vec::new();
        let buildkit =
            "id-race\tbuildx_buildkit_velnor-builder-race0\tvelnor-job-race\t\tcreated\n";
        let mut outputs = vec![
            buildkit.to_string(),
            "velnor-job-race\tvelnor-job-race\trunning\n".to_string(),
            buildkit.to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            "velnor-job-race\tvelnor-job-race\trunning\n".to_string(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_job_buildkit_format_args());
        assert_eq!(calls[1], list_owned_job_format_args());
        assert_eq!(calls[2], list_job_buildkit_format_args());
        assert_eq!(calls[3], list_job_buildkit_volume_args());
        assert_eq!(calls[4], list_job_buildkit_volume_args());
        assert_eq!(calls[7], list_owned_job_format_args());
        assert!(!calls
            .iter()
            .any(|call| call.first().is_some_and(|a| a == "rm")));
        assert!(!calls
            .iter()
            .any(|call| { call.starts_with(&["volume".into(), "rm".into(), "--force".into()]) }));
    }

    #[test]
    fn reclaim_orphan_job_buildkit_surfaces_attached_volume_race_without_force() {
        let buildkit =
            "id-race\tbuildx_buildkit_velnor-builder-race0\tvelnor-job-race\t\tcreated\n";
        let mut calls = Vec::new();
        let mut outputs = vec![
            buildkit.to_string(),
            String::new(),
            buildkit.to_string(),
            String::new(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            String::new(),
        ];
        let result = reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            if args == remove_volume_args(&["buildx_buildkit_velnor-builder-race0_state".into()]) {
                return Err(anyhow!("volume is attached; Docker refused non-force rm"));
            }
            Ok(outputs.remove(0))
        });

        assert!(result.is_err());
        assert_eq!(calls[3], remove_one_container_args("id-race"));
        assert!(calls.iter().any(|call| {
            call == &remove_volume_args(&["buildx_buildkit_velnor-builder-race0_state".into()])
        }));
        assert!(calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg == "--force")));
    }

    #[test]
    fn reclaim_orphan_job_buildkit_refreshes_live_jobs_before_volume_delete() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            String::new(),
            String::new(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            "\"buildx_buildkit_velnor-builder-race0_state\"\t\"local\"\t{\"velnor.job-id\":\"velnor-job-race\"}\n"
                .to_string(),
            "velnor-job-race\tvelnor-job-race\trunning\n".to_string(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_job_buildkit_format_args());
        assert_eq!(calls[1], list_owned_job_format_args());
        assert_eq!(calls[2], list_job_buildkit_volume_args());
        assert_eq!(calls[3], list_job_buildkit_volume_args());
        assert_eq!(calls[6], list_owned_job_format_args());
        assert!(!calls
            .iter()
            .any(|call| { call.starts_with(&["volume".into(), "rm".into(), "--force".into()]) }));
    }

    #[test]
    fn unlabeled_testcontainer_ids_keep_only_pre_lease_orphans() {
        let formatted = "aaa\t\nbbb\tvelnor-job-live\nccc\t\n";
        assert_eq!(
            docker_client::unlabeled_testcontainer_ids(formatted),
            vec!["aaa".to_string(), "ccc".to_string()]
        );
    }

    #[test]
    fn reclaim_unlabeled_testcontainers_refuses_unlabeled_rows_without_removal() {
        let mut calls = Vec::new();
        let result = reclaim_unlabeled_testcontainers(|args| {
            calls.push(args.to_vec());
            Ok("dead1\t\nlive1\tvelnor-job-now\ndead2\t\n".to_string())
        });
        let error = result.expect_err("unlabeled rows must refuse legacy reclaim");
        assert!(matches!(
            error.downcast_ref::<docker_client::LegacyTestcontainerReclaimError>(),
            Some(docker_client::LegacyTestcontainerReclaimError::Unlabeled { ids })
                if ids == &vec!["dead1".to_string(), "dead2".to_string()]
        ));
        assert_eq!(calls[0], list_testcontainers_format_args());
        assert_eq!(calls.len(), 1, "refusal must make zero delete calls");
    }

    #[test]
    fn reclaim_unlabeled_testcontainers_refuses_malformed_rows_without_removal() {
        let mut calls = Vec::new();
        let result = reclaim_unlabeled_testcontainers(|args| {
            calls.push(args.to_vec());
            Ok("malformed-row-without-tab\n".to_string())
        });
        let error = result.expect_err("malformed rows must refuse legacy reclaim");
        assert!(matches!(
            error.downcast_ref::<docker_client::LegacyTestcontainerReclaimError>(),
            Some(docker_client::LegacyTestcontainerReclaimError::MalformedRow { line: 1, row })
                if row == "malformed-row-without-tab"
        ));
        assert_eq!(calls, vec![list_testcontainers_format_args()]);
    }

    #[test]
    fn reclaim_unlabeled_testcontainers_ignores_labeled_rows_without_removal() {
        let mut calls = Vec::new();
        reclaim_unlabeled_testcontainers(|args| {
            calls.push(args.to_vec());
            Ok("live1\tvelnor-job-now\n".to_string())
        })
        .unwrap();
        assert_eq!(calls, vec![list_testcontainers_format_args()]);
    }

    #[test]
    fn reclaim_unlabeled_job_image_siblings_force_removes_orphans_only() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "gagarin\t\nvelnor-job-live\tvelnor-job-live\nride\t\n".to_string(),
            "preflight1\t\nlive-pre\tvelnor-job-live\n".to_string(),
            String::new(),
        ];
        reclaim_unlabeled_job_image_siblings(|args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_job_image_format_args());
        assert_eq!(calls[1], list_preflight_format_args());
        assert_eq!(
            calls[2],
            force_remove_container_args(&["gagarin".into(), "preflight1".into(), "ride".into()])
        );
    }

    #[test]
    fn job_image_reclaim_scans_names_without_resolving_an_image_reference() {
        assert_eq!(
            list_job_image_format_args(),
            vec![
                "ps",
                "--all",
                "--filter",
                "name=velnor-job-",
                "--format",
                "{{.ID}}\t{{.Label \"velnor.job-id\"}}",
            ]
        );
    }

    #[test]
    fn daemon_owns_label_accepts_root_and_direct_slot_children_only() {
        let daemon = "/var/lib/velnor-fleet/work";
        assert!(docker_client::daemon_owns_label(daemon, daemon));
        assert!(docker_client::daemon_owns_label(
            "/var/lib/velnor-fleet/work/slot-3",
            daemon
        ));
        assert!(!docker_client::daemon_owns_label(
            "/var/lib/velnor-other/work",
            daemon
        ));
        assert!(!docker_client::daemon_owns_label(
            "/var/lib/velnor-fleet/work/slot-3/nested",
            daemon
        ));
        assert!(!docker_client::daemon_owns_label(
            "/var/lib/velnor-fleet/work/slots",
            daemon
        ));
        assert!(!docker_client::daemon_owns_label("", daemon));
    }

    #[test]
    fn daemon_orphan_job_ids_scopes_to_owning_daemon() {
        let daemon = "/var/lib/velnor-fleet/work";
        let formatted = "\
velnor-job-live\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning
guest-pg\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning
guest-old\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-2\trunning
velnor-job-dead\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-2\texited
other-dead\tother-dead\t/var/lib/velnor-other/work\texited
";
        assert_eq!(
            docker_client::daemon_orphan_job_ids(formatted, daemon),
            vec!["velnor-job-dead".to_string()]
        );
    }

    #[test]
    fn daemon_scoped_buildkit_volume_reclaim_requires_labeled_owner() {
        let daemon = "/var/lib/velnor-fleet/work";
        let protected = BTreeSet::from(["velnor-job-live".to_string()]);
        let formatted = "\
buildx_buildkit_velnor-builder-dead0_state\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1
buildx_buildkit_velnor-builder-live0_state\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1
buildx_buildkit_velnor-builder-foreign0_state\tvelnor-job-foreign\t/var/lib/velnor-other/work
buildx_buildkit_velnor-builder-unlabeled0_state\tvelnor-job-unlabeled\t
";
        assert_eq!(
            docker_client::daemon_owned_buildkit_volume_names(formatted, daemon, &protected),
            vec!["buildx_buildkit_velnor-builder-dead0_state".to_string()]
        );
    }

    #[test]
    fn reclaim_daemon_orphan_jobs_reclaims_only_this_daemons_orphans() {
        let daemon = "/var/lib/velnor-fleet/work";
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning\nguest-old\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1\trunning\nother\tother\t/var/lib/velnor-other/work\texited\n"
                .to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\texited\n".to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\texited\n".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ];
        reclaim_daemon_orphan_jobs(daemon, |args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_daemon_owned_job_format_args());
        assert_eq!(
            calls[1],
            list_owned_containers_state_args("velnor-job-dead")
        );
        assert_eq!(calls[3], remove_one_container_args("guest-old"));
        assert!(!calls[3].iter().any(|arg| arg == "--force"));
        assert!(calls
            .iter()
            .any(|call| call == &list_daemon_owned_job_buildkit_volume_format_args()));
        // The other daemon's exited job is never looked up or reclaimed.
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "other")),
            "foreign daemon job leaked into reclaim calls: {calls:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn response_buffer_compacts_consumed_prefix_amortized() {
        let mut buffered = ResponseBuffer::default();
        buffered.extend_from_slice(&vec![b'x'; PROXY_COPY_BUFFER]);
        buffered.consume(PROXY_COPY_BUFFER - 1);
        buffered.extend_from_slice(b"tail");
        buffered.consume(1);

        assert_eq!(buffered.cursor, 0);
        assert_eq!(buffered.as_slice(), b"tail");
    }

    #[cfg(unix)]
    #[test]
    fn forwards_fragmented_chunked_response_with_extensions_and_trailers() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let wire = b"5\r\nhello\r\n6;source=test\r\n world\r\n0\r\nX-Complete: yes\r\n\r\n";
        for byte in wire {
            source.write_all(&[*byte]).unwrap();
        }
        drop(source);

        let mut buffered = ResponseBuffer::default();
        forward_chunked_response(&mut host, &mut buffered, &mut sink).unwrap();
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        assert_eq!(forwarded, wire);
        assert!(buffered.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn forwards_pipelined_chunked_and_following_keepalive_responses() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let first =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let second = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
        source.write_all(first).unwrap();
        source.write_all(second).unwrap();
        drop(source);

        let mut buffered = ResponseBuffer::default();
        assert!(forward_http_response(&mut host, &mut buffered, &mut sink, "GET").unwrap());
        assert!(!forward_http_response(&mut host, &mut buffered, &mut sink, "GET").unwrap());
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        assert_eq!(forwarded, [first.as_slice(), second.as_slice()].concat());
        assert!(buffered.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn persistent_container_inspect_redacts_env_before_guest_forward() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let body = br#"{"Id":"persistent-id","Name":"/buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0","State":{"Running":false,"StartedAt":"2026-01-01T00:00:00Z"},"Mounts":[{"Type":"volume","Name":"buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0_state","Destination":"/var/lib/buildkit"}],"Config":{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILD_SECRET=sentinel-secret"]}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(response.as_bytes()).unwrap();
        drop(source);
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let mut observed = Vec::new();
        forward_http_response_with_observer(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut sink,
            "GET",
            ForwardResponseOptions {
                capture_body: true,
                redact_persistent_container_inspect: true,
                ..ForwardResponseOptions::default()
            },
            |status, raw| {
                assert_eq!(status, 200);
                observed.extend_from_slice(raw);
                assert!(raw
                    .windows(b"sentinel-secret".len())
                    .any(|window| { window == b"sentinel-secret" }));
                Ok(())
            },
        )
        .unwrap();
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        let forwarded = String::from_utf8(forwarded).unwrap();
        assert!(!forwarded.contains("sentinel-secret"));
        assert!(!forwarded.contains("\"Env\""));
        assert!(forwarded.contains("\"State\""));
        assert!(!forwarded.contains("\"Mounts\""));
        assert!(forwarded.contains("\"StartedAt\""));
        assert!(observed
            .windows(b"sentinel-secret".len())
            .any(|window| { window == b"sentinel-secret" }));
    }

    #[cfg(unix)]
    #[test]
    fn persistent_bootstrap_does_not_forward_empty_success_before_attestation() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        source
            .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        drop(source);
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let error = forward_http_response_with_observer(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut sink,
            "POST",
            ForwardResponseOptions {
                defer_response_until_observed: true,
                ..ForwardResponseOptions::default()
            },
            |_, _| Err(anyhow!("host attestation failed")),
        )
        .expect_err("bootstrap response must fail before forwarding success");
        assert!(error.to_string().contains("host attestation failed"));
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        let forwarded = String::from_utf8(forwarded).unwrap();
        assert!(forwarded.starts_with("HTTP/1.1 502 Bad Gateway"));
        assert!(!forwarded.contains("201 Created"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_chunked_response_with_missing_chunk_terminator() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(b"3\r\nabcX\r\n").unwrap();
        drop(source);
        let (client, mut sink) = UnixStream::pair().unwrap();
        let error = forward_chunked_response(&mut host, &mut ResponseBuffer::default(), &mut sink)
            .expect_err("chunk data without CRLF must fail closed");
        assert!(error.to_string().contains("terminating CRLF"));
        let _ = client.shutdown(std::net::Shutdown::Both);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_chunked_response_with_invalid_size_line() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(b"not-hex\r\n").unwrap();
        drop(source);
        let (client, mut sink) = UnixStream::pair().unwrap();
        let error = forward_chunked_response(&mut host, &mut ResponseBuffer::default(), &mut sink)
            .expect_err("invalid chunk size must fail closed");
        assert!(error.to_string().contains("chunk-size"));
        let _ = client.shutdown(std::net::Shutdown::Both);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_chunked_response_with_malformed_trailer() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(b"0\r\nnot-a-trailer\r\n\r\n").unwrap();
        drop(source);
        let (client, mut sink) = UnixStream::pair().unwrap();
        let error = forward_chunked_response(&mut host, &mut ResponseBuffer::default(), &mut sink)
            .expect_err("malformed chunk trailer must fail closed");
        assert!(error
            .to_string()
            .contains("malformed Docker API response trailer"));
        let _ = client.shutdown(std::net::Shutdown::Both);
    }

    #[cfg(unix)]
    #[test]
    fn handle_client_reuses_keepalive_connection_for_sequential_requests() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!(
            "velnor-lease-ka-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let engine_path = dir.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        fn read_response(stream: &mut UnixStream) -> Vec<u8> {
            let mut response = Vec::new();
            let mut chunk = [0_u8; 256];
            while !response.ends_with(b"\r\n\r\nOK") {
                let read = stream.read(&mut chunk).unwrap();
                assert_ne!(read, 0, "response stream closed before the body arrived");
                response.extend_from_slice(&chunk[..read]);
            }
            response
        }
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let first = read_http_request(&mut sock).unwrap();
            assert!(String::from_utf8_lossy(&first.bytes).contains("GET /_ping"));
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK",
            )
            .unwrap();
            let second = read_http_request(&mut sock).unwrap();
            assert!(String::from_utf8_lossy(&second.bytes).contains("GET /version"));
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .unwrap();
        });

        let (mut client, proxy_client) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        let engine_for_proxy = engine_path.clone();
        std::thread::spawn(move || {
            let result = handle_client(proxy_client, &engine_for_proxy, "job", "daemon");
            let _ = tx.send(result);
        });

        client
            .write_all(b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: keep-alive\r\n\r\n")
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let first_response = read_response(&mut client);
        assert!(
            std::str::from_utf8(&first_response)
                .unwrap()
                .contains("200 OK"),
            "guest should receive the ping response, got {}",
            String::from_utf8_lossy(&first_response)
        );
        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n")
            .unwrap();
        let second_response = read_response(&mut client);
        assert!(
            std::str::from_utf8(&second_response)
                .unwrap()
                .contains("200 OK"),
            "guest should receive the version response, got {}",
            String::from_utf8_lossy(&second_response)
        );
        drop(client);
        rx.recv_timeout(Duration::from_secs(2))
            .expect("keepalive proxy must finish after the client requests close")
            .unwrap();
        engine_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn handle_client_forwards_pipelined_requests_with_rewrite() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = unique_unix_dir("velnor-lease-pipeline");
        let engine_path = dir.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let (seen_tx, seen_rx) = mpsc::channel::<Vec<Vec<u8>>>();
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let first = read_http_request(&mut sock).unwrap();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK",
            )
            .unwrap();
            let second = read_http_request(&mut sock).unwrap();
            seen_tx.send(vec![first.bytes, second.bytes]).unwrap();
            sock.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"Id\":\"created\"}",
            )
                .unwrap();
        });

        let (mut client, proxy_client) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let engine_for_proxy = engine_path.clone();
        std::thread::spawn(move || {
            let result = handle_client(proxy_client, &engine_for_proxy, "job", "daemon");
            let _ = done_tx.send(result);
        });

        client
            .write_all(
                b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: keep-alive\r\n\r\n\
                  POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: 2\r\n\r\n{}",
            )
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut responses = Vec::new();
        client.read_to_end(&mut responses).unwrap();

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ordinary keepalive proxy must finish after response close")
            .unwrap();
        let seen = seen_rx.recv().unwrap();
        assert!(String::from_utf8_lossy(&seen[0]).contains("GET /_ping"));
        let second = String::from_utf8_lossy(&seen[1]);
        assert!(second.contains("/containers/create"));
        assert!(second.contains("velnor.job-id"));
        assert_eq!(
            String::from_utf8_lossy(&responses)
                .matches("200 OK")
                .count(),
            1
        );
        assert_eq!(
            String::from_utf8_lossy(&responses)
                .matches("201 Created")
                .count(),
            1
        );
        engine_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn unique_unix_dir(prefix: &str) -> PathBuf {
        let dir = PathBuf::from("/tmp").join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn handle_client_closes_host_when_guest_disconnects_during_hanging_engine() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = unique_unix_dir("velnor-lease-guest-eof");
        let engine_path = dir.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = sock.read(&mut buf);
            let _ = accepted_tx.send(());
            sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let n = sock.read(&mut buf);
            let _ = closed_tx.send(n.map(|bytes| bytes == 0).unwrap_or(true));
        });

        let (mut client, proxy_client) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let engine_for_proxy = engine_path.clone();
        std::thread::spawn(move || {
            let result = handle_client(proxy_client, &engine_for_proxy, "job", "daemon");
            let _ = done_tx.send(result);
        });

        client
            .write_all(b"POST /v1.43/containers/job/start HTTP/1.1\r\nHost: docker\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("engine should accept the forwarded start");
        drop(client);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("handle_client must return when the guest disconnects; one-way host copy leaves Engine Start held")
            .unwrap();
        let engine_saw_close = closed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("engine should see the host stream close");
        assert!(
            engine_saw_close,
            "guest EOF must shut down the Engine request, not leave ContainerStart in flight"
        );
        engine_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn hijacked_stream_survives_client_half_close() {
        use std::io::Read as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        use std::time::Duration;

        // Regression for tailrocks/velnor#348: the Go docker client FINs its
        // write side right after sending a hijacked attach request
        // (`Connection: Upgrade` implies close-after-write in net/http). The
        // proxy must treat that FIN as stdin-EOF for the Engine, not as a
        // guest disconnect that tears down the hijacked output stream —
        // `docker run` printed EMPTY stdout with exit code 0 otherwise.
        let dir = unique_unix_dir("velnor-lease-hijack-halfclose");
        let engine_path = dir.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let (sent_tx, sent_rx) = mpsc::channel();
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = sock.read(&mut buf);
            sock.write_all(
                b"HTTP/1.1 101 UPGRADED\r\n\
                   Content-Type: application/vnd.docker.raw-stream\r\n\
                   Connection: Upgrade\r\n\
                   Upgrade: tcp\r\n\r\n\
                   package=app-a\n",
            )
            .unwrap();
            let _ = sent_tx.send(());
            // Keep the hijacked stream open past the guest FIN, like dockerd
            // streaming a container that has not exited yet, then close.
            std::thread::sleep(Duration::from_millis(300));
            drop(sock);
        });

        let (mut client, proxy_client) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let engine_for_proxy = engine_path.clone();
        std::thread::spawn(move || {
            let result = handle_client(proxy_client, &engine_for_proxy, "job", "daemon");
            let _ = done_tx.send(result);
        });

        client
            .write_all(
                b"POST /v1.54/containers/job/attach?stderr=1&stdout=1&stream=1 HTTP/1.1\r\n\
                   Host: docker\r\n\
                   Connection: Upgrade\r\n\
                   Upgrade: tcp\r\n\
                   Content-Length: 0\r\n\r\n",
            )
            .unwrap();
        // Go net/http closes the write side of an upgraded request as soon
        // as it is sent, before any hijacked output exists.
        client.shutdown(std::net::Shutdown::Write).unwrap();
        sent_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("engine should send the hijacked stream");

        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            match client.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => received.extend_from_slice(&buf[..n]),
            }
        }
        let text = String::from_utf8_lossy(&received);
        assert!(
            text.contains("package=app-a"),
            "hijacked output must survive the guest half-close, got: {text}"
        );
        drop(client);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("handle_client must return once the Engine closes the hijacked stream")
            .unwrap();
        engine_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn drop_aborts_in_flight_host_engine_request() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = unique_unix_dir("velnor-lease-drop-abort");
        let listen_path = dir.join("lease.sock");
        let engine_path = dir.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = sock.read(&mut buf);
            let _ = accepted_tx.send(());
            sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let n = sock.read(&mut buf);
            let _ = closed_tx.send(n.map(|bytes| bytes == 0).unwrap_or(true));
        });

        let guard = DockerLeaseGuard::bind_to(
            listen_path.clone(),
            engine_path,
            "job".into(),
            "daemon".into(),
        )
        .unwrap();
        let mut client = UnixStream::connect(&listen_path).unwrap();
        client
            .write_all(b"POST /v1.43/containers/job/start HTTP/1.1\r\nHost: docker\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("engine should accept the forwarded start");
        drop(guard);
        let _keep_guest = client;
        let engine_saw_close = closed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("Drop must abort the in-flight Engine stream");
        assert!(
            engine_saw_close,
            "lease Drop must shut down host Engine HTTP, not leave ContainerStart held after the job ends"
        );
        engine_thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn drop_stops_idle_accept_thread_promptly() {
        let dir = unique_unix_dir("velnor-lease-drop-idle");
        let listen_path = dir.join("lease.sock");
        let guard = DockerLeaseGuard::bind_to(
            listen_path.clone(),
            dir.join("missing-engine.sock"),
            "job".into(),
            "daemon".into(),
        )
        .unwrap();

        let started = std::time::Instant::now();
        drop(guard);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "idle lease accept thread shutdown took {:?}",
            started.elapsed()
        );
        assert!(!listen_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

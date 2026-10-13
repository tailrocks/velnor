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
//! shuts down ordinary Engine streams before reclaim. A dispatched persistent
//! BuildKit `ContainerCreate` is durably fenced and its exact reply is drained
//! after guest cancellation because Moby can finish a cancelled create later
//! (moby/moby#24858).

use crate::docker::client as docker_client;
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
pub const BUILDKIT_DOMAIN_LABEL: &str = "velnor.buildkit-domain";
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
/// Docker's `name=` filter is a substring discovery query, not an ownership
/// boundary. Callers structurally classify rows and re-attest immutable
/// container/volume identity before any lifecycle mutation.
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
pub(crate) const PERSISTENT_BUILDKIT_VOLUME_PRESSURE_IDENTITY_FORMAT: &str = r#"{{json .Name}}{{"\t"}}{{json .Driver}}{{"\t"}}{{json .Labels}}{{"\t"}}{{json .Options}}{{"\t"}}{{json .Mountpoint}}"#;
pub(crate) const PERSISTENT_BUILDKIT_CONTAINER_IDENTITY_FORMAT: &str =
    r#"{{json .Id}}{{"\t"}}{{json .Name}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .Mounts}}"#;

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

/// Host inspection projection used before mutating a persistent BuildKit
/// container. `.Config.Env` is deliberately omitted because it can contain
/// workflow secrets.
pub(crate) fn inspect_persistent_buildkit_container_args(target: &str) -> Vec<String> {
    vec![
        "inspect".into(),
        "--format".into(),
        PERSISTENT_BUILDKIT_CONTAINER_IDENTITY_FORMAT.into(),
        "--".into(),
        target.into(),
    ]
}

/// Host inspection args for a persistent BuildKit state volume.
pub(crate) fn inspect_persistent_buildkit_volume_args(target: &str) -> Vec<String> {
    inspect_volume_identity_args(target)
}

pub(crate) fn inspect_persistent_buildkit_volume_pressure_args(target: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "inspect".into(),
        "--format".into(),
        PERSISTENT_BUILDKIT_VOLUME_PRESSURE_IDENTITY_FORMAT.into(),
        "--".into(),
        target.into(),
    ]
}

/// Strictly attest the exact domain volume and return the same inspect
/// response's host mountpoint. Pressure pruning requires this projection so
/// driver/options/labels and path cannot come from separate volume versions.
pub(crate) fn attest_persistent_buildkit_volume_mountpoint(
    output: &str,
    expected_name: &str,
    domain_token: &str,
) -> Result<String> {
    let fields = parse_identity_fields(output, 5, "persistent BuildKit volume")?;
    let base = fields[..4].join("\t");
    attest_persistent_buildkit_volume_identity(&base, expected_name, domain_token)?;
    let mountpoint: String = parse_identity_json(fields[4], "persistent BuildKit mountpoint")?;
    if !Path::new(&mountpoint).is_absolute() {
        bail!("Docker persistent BuildKit volume mountpoint is not absolute");
    }
    Ok(mountpoint)
}

/// Attest the host CLI projection for a persistent BuildKit daemon container.
/// Name and labels are bound to the v2 builder's domain, and its only mount
/// must be that builder's named state volume at BuildKit's data path.
pub(crate) fn attest_persistent_buildkit_container_identity(
    output: &str,
    builder: &str,
    expected_volume: &str,
    domain_token: &str,
) -> Result<String> {
    let expected_domain_token = persistent_buildkit_domain_token(builder)
        .context("persistent BuildKit container identity requires a v2 builder name")?;
    if expected_domain_token != domain_token {
        bail!("persistent BuildKit container domain does not match its builder name");
    }
    let expected_name = crate::buildkit::daemon_container_name(builder);
    if crate::buildkit::daemon_state_volume(builder).as_str() != expected_volume {
        bail!("persistent BuildKit container expected state volume does not match its builder");
    }
    let fields = parse_identity_fields(output, 4, "persistent BuildKit container")?;
    let id: String = parse_identity_json(fields[0], "persistent BuildKit container ID")?;
    let raw_name: String = parse_identity_json(fields[1], "persistent BuildKit container name")?;
    let labels: Option<BTreeMap<String, String>> =
        parse_identity_json(fields[2], "persistent BuildKit container labels")?;
    let mounts: Vec<Value> =
        parse_identity_json(fields[3], "persistent BuildKit container mounts")?;
    let id = validate_owned_resource_id(&id, "persistent BuildKit container ID")?;
    if raw_name.strip_prefix('/').unwrap_or(&raw_name) != expected_name.as_str() {
        bail!("persistent BuildKit container name does not match its builder");
    }
    let labels = labels.context("persistent BuildKit container omitted ownership labels")?;
    if labels.len() != 2
        || labels
            .keys()
            .any(|key| key != JOB_ID_LABEL && key != BUILDKIT_DOMAIN_LABEL)
        || labels.get(JOB_ID_LABEL).is_none_or(String::is_empty)
        || labels.get(BUILDKIT_DOMAIN_LABEL).map(String::as_str) != Some(expected_domain_token)
    {
        bail!("persistent BuildKit container ownership labels do not match its domain");
    }
    if mounts.len() != 1 {
        bail!("persistent BuildKit container has an unexpected mount set");
    }
    let mount = mounts[0]
        .as_object()
        .context("persistent BuildKit container mount is malformed")?;
    if api_object_field(mount, "Type").and_then(Value::as_str) != Some("volume")
        || api_object_field(mount, "Name").and_then(Value::as_str) != Some(expected_volume)
        || api_object_field(mount, "Destination").and_then(Value::as_str)
            != Some("/var/lib/buildkit")
    {
        bail!("persistent BuildKit container does not mount its expected state volume");
    }
    Ok(id)
}

/// Attest the host CLI projection for a persistent BuildKit state volume.
pub(crate) fn attest_persistent_buildkit_volume_identity(
    output: &str,
    expected_name: &str,
    domain_token: &str,
) -> Result<String> {
    let builder = persistent_buildkit_volume_builder_name(expected_name)
        .context("persistent BuildKit volume identity requires a v2 state volume name")?;
    let expected_domain_token = persistent_buildkit_domain_token(builder)
        .context("persistent BuildKit volume identity requires a v2 builder name")?;
    if expected_domain_token != domain_token {
        bail!("persistent BuildKit volume domain does not match its name");
    }
    let allowed_builders = BTreeSet::from([builder.to_owned()]);
    attest_persistent_buildkit_volume_projection(
        output,
        expected_name,
        domain_token,
        &allowed_builders,
    )?;
    Ok(expected_name.to_owned())
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
    let builder = crate::buildkit::buildkit_daemon_builder_name(name)?;
    let scope = builder.strip_prefix("velnor-builder-")?;
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
pub(crate) struct DockerLeasePolicy {
    resources: Arc<Mutex<OwnedDockerResources>>,
    persistent_builder_requests_changed: Arc<Condvar>,
    #[cfg(unix)]
    volume_lock_root: Option<Arc<crate::fs_copy::NoFollowDestinationDir>>,
    #[cfg(not(unix))]
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
    /// Setup config mode and monotonically increasing capability generation.
    /// Every response observer is fenced against the generation captured when
    /// its request was authorized.
    persistent_builder_config_fingerprints: BTreeMap<String, String>,
    /// The process-shared create/archive/start transaction for each builder.
    /// Keeping the RAII lock here spans the separate Docker API requests that
    /// make up Buildx bootstrap.
    persistent_builder_creator_leases:
        BTreeMap<String, crate::buildkit::PersistentBuildKitCreatorLease>,
    persistent_builder_generations: BTreeMap<String, u64>,
    /// Persistent API requests admitted under a builder generation. Setup and
    /// revoke close admission and drain these requests before changing the
    /// generation or stopping the daemon.
    persistent_builder_requests_closing: BTreeSet<String>,
    /// Builder -> unique closer invocation. A contender that wakes from a
    /// poisoned condvar must never clear another invocation's ownership.
    persistent_builder_requests_closer_active: BTreeMap<String, u64>,
    next_persistent_builder_closer: u64,
    persistent_builder_requests_in_flight: BTreeMap<String, usize>,
    /// Concurrent Buildx daemon-create requests per builder generation.
    /// A 409 may wait on an unbound creator lease only while a distinct
    /// create request can still win and bind the immutable container ID.
    persistent_builder_create_requests_in_flight: BTreeMap<String, usize>,
    /// Bootstrap requests admitted locally but not yet able to acquire the
    /// process-shared creator flock. They cannot dispatch Docker work and
    /// therefore do not block the current 409 recovery election.
    persistent_builder_creator_lock_waiters: BTreeMap<String, usize>,
    persistent_builder_conflict_waiters: BTreeMap<String, usize>,
    persistent_builder_recovery_generations: BTreeMap<String, u64>,
    #[cfg(unix)]
    persistent_builder_tunnels: BTreeMap<u64, PersistentBuilderTunnelState>,
    #[cfg(unix)]
    next_persistent_builder_tunnel: u64,
    /// Fresh daemon IDs may start only after their exact config archive was
    /// accepted. Reused IDs need a durable host readiness proof instead.
    persistent_container_fresh_ids: BTreeSet<String>,
    persistent_container_config_archives: BTreeMap<String, String>,
    persistent_builder_readiness_epochs: BTreeMap<String, u64>,
    /// Durable proof cache keyed by immutable container ID. The builder's
    /// builder generation is implicit because begin/revoke drain admissions;
    /// each proof also carries the durable per-container start epoch.
    persistent_container_ready_fingerprints: BTreeMap<String, String>,
    persistent_container_readiness_epochs: BTreeMap<String, u64>,
    /// Per-name operation locks. Docker volumes have no immutable ID, so a
    /// name must stay bound from re-attestation through the forwarded
    /// operation and its response observer.
    volume_locks: BTreeMap<String, Arc<VolumeNameLock>>,
    execs: BTreeSet<String>,
    /// In-flight create requests reserve from the same hard resource cap
    /// before any Engine mutation is dispatched.
    reserved_resource_slots: usize,
    /// Exec IDs created through the attested persistent BuildKit path. The
    /// ID is the immutable binding used by the later hijack/start request.
    persistent_execs: BTreeSet<String>,
    /// Persistent exec ownership follows its attested builder so revoking one
    /// setup cannot leave a stale exec capability behind.
    persistent_exec_builders: BTreeMap<String, String>,
    persistent_exec_containers: BTreeMap<String, String>,
    persistent_exec_generations: BTreeMap<String, u64>,
    persistent_exec_readiness_epochs: BTreeMap<String, u64>,
}

#[cfg(unix)]
struct PersistentBuilderTunnelState {
    builder: String,
    generation: u64,
    host: std::os::unix::net::UnixStream,
    client: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
struct PersistentBuilderTunnel {
    resources: Arc<Mutex<OwnedDockerResources>>,
    changed: Arc<Condvar>,
    id: u64,
}

struct OwnedResourceReservation {
    resources: Arc<Mutex<OwnedDockerResources>>,
    state: OwnedResourceReservationState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnedResourceReservationState {
    Reserved,
    Dispatched,
    Pinned,
    Finished,
}

impl OwnedResourceReservation {
    fn mark_dispatched(&mut self) -> Result<()> {
        match self.state {
            OwnedResourceReservationState::Reserved => {
                self.state = OwnedResourceReservationState::Dispatched;
                Ok(())
            }
            OwnedResourceReservationState::Dispatched => Ok(()),
            OwnedResourceReservationState::Pinned | OwnedResourceReservationState::Finished => {
                bail!("Docker lease resource reservation is no longer dispatchable")
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        if self.state == OwnedResourceReservationState::Finished {
            return Ok(());
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.reserved_resource_slots = resources
            .reserved_resource_slots
            .checked_sub(1)
            .context("Docker lease resource reservation underflow")?;
        self.state = OwnedResourceReservationState::Finished;
        Ok(())
    }

    /// Keep the slot occupied when the Engine may have created an object but
    /// its response could not be attested or fully observed. Releasing that
    /// reservation would let later concurrent creates exceed the cap.
    fn pin(&mut self) {
        if self.state != OwnedResourceReservationState::Finished {
            self.state = OwnedResourceReservationState::Pinned;
        }
    }
}

impl Drop for OwnedResourceReservation {
    fn drop(&mut self) {
        if self.state != OwnedResourceReservationState::Reserved {
            return;
        }
        let Ok(mut resources) = self.resources.lock() else {
            return;
        };
        resources.reserved_resource_slots = resources.reserved_resource_slots.saturating_sub(1);
    }
}

fn finish_resource_reservation_after_observation(
    reservation: &mut Option<OwnedResourceReservation>,
    status: u16,
    result: Result<()>,
) -> Result<()> {
    let Some(reservation) = reservation.as_mut() else {
        return result;
    };
    match result {
        Ok(()) => reservation.finish(),
        Err(error) => {
            if (200..300).contains(&status) {
                reservation.pin();
            } else {
                reservation.finish()?;
            }
            Err(error)
        }
    }
}

fn pin_resource_reservation_after_uncertain_dispatch(
    reservation: &mut Option<OwnedResourceReservation>,
) {
    if let Some(reservation) = reservation.as_mut() {
        reservation.pin();
    }
}

#[cfg(unix)]
fn shutdown_persistent_builder_tunnels(
    resources: &mut OwnedDockerResources,
    changed: &Condvar,
    builder: &str,
    generation: u64,
    closer_id: u64,
    mut shutdown: impl FnMut(&std::os::unix::net::UnixStream) -> io::Result<()>,
) -> Result<()> {
    let mut first_error = None;
    for tunnel in resources
        .persistent_builder_tunnels
        .values()
        .filter(|tunnel| tunnel.builder == builder && tunnel.generation <= generation)
    {
        for (stream, endpoint) in [(&tunnel.host, "host"), (&tunnel.client, "guest")] {
            if let Err(error) = shutdown(stream)
                .with_context(|| format!("close BuildKit {endpoint} tunnel for {builder}"))
            {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        // Keep admission closed, but release the single-closer marker so a
        // later setup/revoke call can retry shutdown instead of waiting forever.
        clear_persistent_builder_closer(resources, builder, closer_id);
        changed.notify_all();
        return Err(error);
    }
    Ok(())
}

fn clear_persistent_builder_closer(
    resources: &mut OwnedDockerResources,
    builder: &str,
    closer_id: u64,
) {
    if resources
        .persistent_builder_requests_closer_active
        .get(builder)
        == Some(&closer_id)
    {
        resources
            .persistent_builder_requests_closer_active
            .remove(builder);
    }
}

#[cfg(unix)]
impl Drop for PersistentBuilderTunnel {
    fn drop(&mut self) {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            // A poisoned registry cannot grant new authority, but this tunnel
            // must still retire so a closer waiting on the condition variable
            // can wake and fail closed instead of hanging indefinitely.
            Err(poisoned) => poisoned.into_inner(),
        };
        resources.persistent_builder_tunnels.remove(&self.id);
        self.changed.notify_all();
    }
}

fn ensure_persistent_builder_generation_locked(
    resources: &OwnedDockerResources,
    builder: &str,
    generation: u64,
) -> Result<()> {
    if resources.persistent_builder_generations.get(builder) != Some(&generation)
        || !resources.persistent_builders.contains(builder)
    {
        bail!("persistent BuildKit capability was revoked or replaced");
    }
    Ok(())
}

fn require_persistent_container_ready_locked(
    resources: &OwnedDockerResources,
    builder: &str,
    container_id: &str,
) -> Result<()> {
    let current_epoch = resources
        .persistent_builder_readiness_epochs
        .get(builder)
        .copied()
        .context("persistent BuildKit has no current readiness epoch")?;
    let expected = resources
        .persistent_builder_config_fingerprints
        .get(builder)
        .context("persistent BuildKit config mode was not registered")?;
    if resources
        .persistent_container_ready_fingerprints
        .get(container_id)
        != Some(expected)
        || resources
            .persistent_container_readiness_epochs
            .get(container_id)
            != Some(&current_epoch)
    {
        bail!("persistent BuildKit exec requires exact durable readiness proof");
    }
    Ok(())
}

#[cfg(unix)]
fn accept_reused_persistent_container_readiness(
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    generation: u64,
    config_fingerprint: &str,
) -> Result<()> {
    let result = (|| {
        note_current_persistent_readiness(
            policy,
            domain,
            builder,
            generation,
            container_id,
            config_fingerprint,
        )
    })();
    if let Err(error) = result {
        // The inspect response was strict, but its durable readiness proof
        // was missing, unreadable, or stale. Remove this immutable binding
        // and any exec IDs derived from it before returning the inspect
        // failure to the guest.
        policy
            .forget_persistent_container_fenced(container_id, Some((builder, generation)))
            .context("forget unready persistent BuildKit container binding")?;
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn note_current_persistent_readiness(
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    container_id: &str,
    config_fingerprint: &str,
) -> Result<u64> {
    let readiness_epoch = crate::buildkit::builder_readiness_epoch(domain, builder)?;
    if !crate::buildkit::builder_readiness_matches_epoch(
        domain,
        builder,
        container_id,
        config_fingerprint,
        readiness_epoch,
    )? {
        bail!("persistent BuildKit container has no matching durable readiness proof");
    }
    policy.note_persistent_ready_container(
        builder,
        generation,
        container_id,
        config_fingerprint,
        readiness_epoch,
    )?;
    Ok(readiness_epoch)
}

fn remove_persistent_execs_for_builder(resources: &mut OwnedDockerResources, builder: &str) {
    let removed = resources
        .persistent_exec_builders
        .iter()
        .filter(|(_, owner)| owner.as_str() == builder)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    for id in removed {
        resources.execs.remove(&id);
        resources.persistent_execs.remove(&id);
        resources.persistent_exec_builders.remove(&id);
        resources.persistent_exec_containers.remove(&id);
        resources.persistent_exec_generations.remove(&id);
        resources.persistent_exec_readiness_epochs.remove(&id);
    }
}

fn remove_persistent_execs_for_container(resources: &mut OwnedDockerResources, container_id: &str) {
    let removed = resources
        .persistent_exec_containers
        .iter()
        .filter(|(_, owner)| owner.as_str() == container_id)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    for id in removed {
        resources.execs.remove(&id);
        resources.persistent_execs.remove(&id);
        resources.persistent_exec_builders.remove(&id);
        resources.persistent_exec_containers.remove(&id);
        resources.persistent_exec_generations.remove(&id);
        resources.persistent_exec_readiness_epochs.remove(&id);
    }
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

/// Durable journal handle for one persistent BuildKit ContainerCreate. Moby
/// may complete create after a client context/socket is cancelled and can
/// report 404 before the late-created object appears. Drop never clears the
/// transaction; only exact archive/start/readiness proof can settle it.
#[cfg(unix)]
struct PersistentBuildKitCreateFence {
    domain: crate::buildkit::PersistentBuildKitDomain,
    record: crate::buildkit::PendingBuildKitCreateTransaction,
}

#[cfg(unix)]
impl PersistentBuildKitCreateFence {
    fn begin(
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        generation: u64,
        volume: &str,
        container_name: &str,
        config_fingerprint: &str,
        expected_image_id: &str,
        expects_config: bool,
        request: &[u8],
    ) -> Result<Self> {
        let request_sha256 = Sha256::digest(request)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let body = docker_request_body(request)?;
        let expected_create_shape = normalized_persistent_create_shape(
            body,
            container_name,
            volume,
            &domain.token,
            expected_image_id,
        )?;
        let shape = Sha256::digest(serde_json::to_vec(&expected_create_shape)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let record = crate::buildkit::PendingBuildKitCreateTransaction {
            version: 1,
            transaction_id: uuid::Uuid::new_v4().to_string(),
            engine_id: domain.engine_id.clone(),
            domain_token: domain.token.clone(),
            builder: builder.to_owned(),
            generation,
            config_fingerprint: config_fingerprint.to_owned(),
            state_volume: volume.to_owned(),
            container_name: container_name.to_owned(),
            request_sha256,
            normalized_shape_sha256: shape,
            expected_create_shape,
            expected_image_id: expected_image_id.to_owned(),
            expects_config,
            phase: crate::buildkit::PendingBuildKitCreatePhase::Dispatched,
            container_id: None,
            attested_shape_sha256: None,
            archived_config_fingerprint: None,
        };
        crate::buildkit::begin_pending_buildkit_create_transaction(domain, record.clone())?;
        Ok(Self {
            domain: domain.clone(),
            record,
        })
    }

    fn bind_container(&mut self, container_id: &str, shape_sha256: &str) -> Result<()> {
        self.record = crate::buildkit::bind_pending_buildkit_create_container(
            &self.domain,
            &self.record.builder,
            &self.record.transaction_id,
            container_id,
            shape_sha256,
        )?;
        Ok(())
    }

    fn record_archive(&mut self, container_id: &str, fingerprint: &str) -> Result<()> {
        self.record = crate::buildkit::record_pending_buildkit_create_archive(
            &self.domain,
            &self.record.builder,
            &self.record.transaction_id,
            container_id,
            fingerprint,
        )?;
        Ok(())
    }

    fn mark_started(&mut self, container_id: &str) -> Result<()> {
        self.record = crate::buildkit::mark_pending_buildkit_create_started(
            &self.domain,
            &self.record.builder,
            &self.record.transaction_id,
            container_id,
        )?;
        Ok(())
    }
}

#[cfg(unix)]
fn normalized_persistent_create_shape(
    body: &[u8],
    container_name: &str,
    volume: &str,
    domain_token: &str,
    expected_image_id: &str,
) -> Result<Value> {
    let mut value = parse_create_value(body)?;
    let object = value
        .as_object_mut()
        .context("persistent BuildKit create body must be an object")?;
    let labels_key = object
        .keys()
        .find(|key| key.eq_ignore_ascii_case("Labels"))
        .cloned()
        .context("persistent BuildKit create omitted ownership labels")?;
    let labels = object
        .get_mut(&labels_key)
        .and_then(Value::as_object_mut)
        .context("persistent BuildKit create labels must be an object")?;
    labels.insert(
        JOB_ID_LABEL.to_owned(),
        Value::String("<creator-job>".into()),
    );
    labels.insert(
        BUILDKIT_DOMAIN_LABEL.to_owned(),
        Value::String(domain_token.to_owned()),
    );
    let normalized = serde_json::json!({
        "name": container_name,
        "volume": volume,
        "image_id": expected_image_id,
        "create": value,
    });
    Ok(normalized)
}

#[cfg(unix)]
fn persistent_inspected_create_shape_sha256(body: &[u8]) -> Result<String> {
    let object = parse_api_object(body, "container")?;
    let name = api_object_string(&object, "Name")?;
    let image_id = api_object_string(&object, "Image")?;
    let config = api_object_field(&object, "Config")
        .context("persistent container inspect omitted Config")?;
    let host_config = api_object_field(&object, "HostConfig")
        .context("persistent container inspect omitted HostConfig")?;
    let mounts = api_object_field(&object, "Mounts")
        .context("persistent container inspect omitted Mounts")?;
    let normalized = serde_json::json!({
        "name": name,
        "image_id": image_id,
        "config": config,
        "host_config": host_config,
        "mounts": mounts,
    });
    let bytes = serde_json::to_vec(&normalized)
        .context("serialize attested persistent BuildKit create shape")?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(unix)]
fn json_matches_expected_projection(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected.iter().all(|(key, value)| {
            actual
                .iter()
                .find(|(actual_key, _)| actual_key.eq_ignore_ascii_case(key))
                .is_some_and(|(_, actual_value)| {
                    json_matches_expected_projection(value, actual_value)
                })
        }),
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| json_matches_expected_projection(expected, actual))
        }
        _ => expected == actual,
    }
}

#[cfg(unix)]
fn normalize_buildkit_job_label(value: &mut Value) -> Result<()> {
    let labels = value
        .as_object_mut()
        .context("BuildKit create shape labels must be an object")?;
    let job_label = labels
        .keys()
        .find(|key| key.eq_ignore_ascii_case(JOB_ID_LABEL))
        .cloned()
        .context("BuildKit create shape omitted its job label")?;
    labels.insert(job_label, Value::String("<creator-job>".to_owned()));
    Ok(())
}

/// Match every explicitly supplied, validated create field against the
/// corresponding immutable inspect field. Docker may add default fields, so
/// compare the complete request projection as a case-insensitive subset;
/// unsafe or unexpected inspected fields are rejected separately by the
/// strict persistent-container attestor.
#[cfg(unix)]
fn pending_create_shape_matches_inspect(
    expected_shape: &Value,
    inspect_object: &Map<String, Value>,
) -> Result<bool> {
    let expected_create = expected_shape
        .get("create")
        .and_then(Value::as_object)
        .context("pending BuildKit transaction omitted normalized create shape")?;
    let actual_config = api_object_field(inspect_object, "Config")
        .and_then(Value::as_object)
        .context("inspected BuildKit container omitted Config")?;
    let actual_host_config = api_object_field(inspect_object, "HostConfig")
        .and_then(Value::as_object)
        .context("inspected BuildKit container omitted HostConfig")?;

    for (key, expected) in expected_create {
        match key.to_ascii_lowercase().as_str() {
            "hostconfig" => {
                if !json_matches_expected_projection(
                    expected,
                    &Value::Object(actual_host_config.clone()),
                ) {
                    return Ok(false);
                }
            }
            // The API create's NetworkingConfig is required empty by the
            // validator. Docker's inspect representation is NetworkSettings,
            // which is runtime state and is checked through HostConfig and
            // the exact bridge-mode policy instead.
            "networkingconfig" => {}
            "labels" => {
                let mut expected_labels = expected.clone();
                let Some(actual_labels) = api_object_field(actual_config, key) else {
                    return Ok(false);
                };
                let mut actual_labels = actual_labels.clone();
                normalize_buildkit_job_label(&mut expected_labels)?;
                normalize_buildkit_job_label(&mut actual_labels)?;
                if !json_matches_expected_projection(&expected_labels, &actual_labels) {
                    return Ok(false);
                }
            }
            _ => {
                let Some(actual) = api_object_field(actual_config, key) else {
                    return Ok(false);
                };
                if !json_matches_expected_projection(expected, actual) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn bind_persistent_create_fence_to_inspect(
    fence: &mut PersistentBuildKitCreateFence,
    body: &[u8],
) -> Result<()> {
    let (id, shape, _) = attest_pending_buildkit_create_inspect(body, &fence.record)?;
    fence.bind_container(&id, &shape)
}

#[cfg(unix)]
pub(crate) fn attest_pending_buildkit_create_inspect(
    body: &[u8],
    transaction: &crate::buildkit::PendingBuildKitCreateTransaction,
) -> Result<(String, String, String)> {
    let allowed = BTreeSet::from([transaction.builder.clone()]);
    let images = BTreeMap::from([(
        transaction.builder.clone(),
        transaction.expected_image_id.clone(),
    )]);
    let (name, id, volume, image_id, has_config) =
        attest_persistent_buildkit_container(body, &transaction.container_name, &allowed, &images)?;
    if name != transaction.container_name
        || volume != transaction.state_volume
        || image_id != transaction.expected_image_id
        || has_config != transaction.expects_config
    {
        bail!("inspected BuildKit create shape does not match durable transaction intent");
    }
    if transaction
        .container_id
        .as_deref()
        .is_some_and(|expected| expected != id)
    {
        bail!("inspected BuildKit create immutable ID changed from transaction");
    }
    let object = parse_api_object(body, "container")?;
    let state = api_object_field(&object, "State")
        .and_then(Value::as_object)
        .and_then(|state| api_object_field(state, "Status"))
        .and_then(Value::as_str)
        .context("inspected BuildKit create omitted state")?
        .to_owned();
    if !matches!(state.as_str(), "created" | "running") {
        bail!("BuildKit create is in non-recoverable state {state:?}");
    }
    let shape = persistent_inspected_create_shape_sha256(body)?;
    if !pending_create_shape_matches_inspect(&transaction.expected_create_shape, &object)? {
        bail!("inspected BuildKit container differs from durable normalized create intent");
    }
    if transaction
        .attested_shape_sha256
        .as_deref()
        .is_some_and(|expected| expected != shape)
    {
        bail!("inspected BuildKit create shape changed from durable transaction");
    }
    Ok((id, shape, state))
}

#[cfg(unix)]
fn pending_buildkit_create_marker_name(volume: &str) -> String {
    format!("pending-buildkit-create-{}.json", volume_lock_key(volume))
}

#[cfg(unix)]
fn ensure_no_pending_buildkit_create(
    root: &crate::fs_copy::NoFollowDestinationDir,
    volume: &str,
) -> Result<()> {
    let marker_name = pending_buildkit_create_marker_name(volume);
    if root
        .open_relative_file_if_exists(Path::new(&marker_name))
        .with_context(|| format!("inspect persistent BuildKit create fence {marker_name}"))?
        .is_some()
    {
        bail!("persistent BuildKit create remains unresolved for volume {volume:?}");
    }
    Ok(())
}

#[cfg(unix)]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPendingBuildKitCreateMarker {
    version: u32,
    engine_id: String,
    builder: String,
    generation: u64,
    volume: String,
    container_name: String,
    request_sha256: String,
}

#[cfg(unix)]
fn migrate_legacy_pending_buildkit_create(
    runtime_root: &crate::fs_copy::NoFollowDestinationDir,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    volume: &str,
) -> Result<()> {
    let marker_name = pending_buildkit_create_marker_name(volume);
    let Some(mut marker) = runtime_root
        .open_relative_file_if_exists(Path::new(&marker_name))
        .with_context(|| format!("open legacy BuildKit create marker {marker_name} safely"))?
    else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    Read::take(&mut marker, 4097)
        .read_to_end(&mut bytes)
        .context("read legacy BuildKit create marker")?;
    if bytes.len() > 4096 {
        bail!("legacy BuildKit create marker exceeds the migration limit");
    }
    let legacy: LegacyPendingBuildKitCreateMarker = serde_json::from_slice(&bytes)
        .context("parse legacy BuildKit create marker; keep it fenced for operator repair")?;
    if legacy.version != 1
        || legacy.engine_id != domain.engine_id
        || legacy.volume != volume
        || crate::buildkit::persistent_builder_domain_token(&legacy.builder)
            != Some(domain.token.as_str())
        || crate::buildkit::daemon_state_volume(&legacy.builder) != volume
        || crate::buildkit::daemon_container_name(&legacy.builder) != legacy.container_name
        || legacy.generation == 0
        || legacy.request_sha256.len() != 64
        || !legacy
            .request_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("legacy BuildKit create marker identity is uncertain; keep it fenced");
    }
    crate::buildkit::quarantine_legacy_pending_buildkit_create(domain, volume, &bytes)?;
    runtime_root
        .remove_tree_entry(OsStr::new(&marker_name))
        .context("remove durably quarantined legacy BuildKit create marker")?;
    runtime_root
        .sync_directory()
        .context("sync removal of legacy BuildKit create marker")
}

#[cfg(unix)]
fn ensure_domain_pending_buildkit_create_clear(
    domain: &crate::buildkit::PersistentBuildKitDomain,
    volume: &str,
    access: Option<&crate::buildkit::PendingBuildKitCreateAccess>,
) -> Result<()> {
    let Some(builder) = persistent_buildkit_volume_builder_name(volume) else {
        return Ok(());
    };
    if persistent_buildkit_domain_token(builder) != Some(domain.token.as_str())
        || crate::buildkit::daemon_state_volume(builder) != volume
    {
        return Ok(());
    }
    if crate::buildkit::legacy_pending_buildkit_create_is_quarantined(domain, volume)? {
        bail!("legacy persistent BuildKit create remains quarantined for operator repair");
    }
    let Some(transaction) = crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
    else {
        return Ok(());
    };
    if access.is_some_and(|access| {
        access.builder == builder
            && access.generation == transaction.generation
            && access.transaction_id == transaction.transaction_id
    }) {
        return Ok(());
    }
    bail!("persistent BuildKit create transaction remains unresolved for {volume:?}");
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

struct PersistentBuilderAdmission {
    resources: Arc<Mutex<OwnedDockerResources>>,
    changed: Arc<Condvar>,
    builder: String,
    generation: u64,
    container_id: Option<String>,
    readiness_epoch: Option<u64>,
    is_bootstrap_create: bool,
    is_bootstrap_conflict_waiter: bool,
}

impl fmt::Debug for PersistentBuilderAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentBuilderAdmission")
            .field("builder", &self.builder)
            .field("generation", &self.generation)
            .field("container_id", &self.container_id)
            .field("readiness_epoch", &self.readiness_epoch)
            .field("is_bootstrap_create", &self.is_bootstrap_create)
            .field(
                "is_bootstrap_conflict_waiter",
                &self.is_bootstrap_conflict_waiter,
            )
            .finish_non_exhaustive()
    }
}

struct PersistentBuilderRecoveryAdmission {
    resources: Arc<Mutex<OwnedDockerResources>>,
    changed: Arc<Condvar>,
    builder: String,
    generation: u64,
    counted_request: bool,
}

impl Drop for PersistentBuilderAdmission {
    fn drop(&mut self) {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            // Retire the in-flight ticket even after registry poisoning. The
            // capability remains fail-closed because the mutex stays poisoned.
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(in_flight) = resources
            .persistent_builder_requests_in_flight
            .get_mut(&self.builder)
        {
            *in_flight = in_flight.saturating_sub(1);
            if *in_flight == 0 {
                resources
                    .persistent_builder_requests_in_flight
                    .remove(&self.builder);
            }
        }
        if self.is_bootstrap_create {
            decrement_bootstrap_create_count(&mut resources, &self.builder);
        }
        if self.is_bootstrap_conflict_waiter {
            decrement_builder_count(
                &mut resources.persistent_builder_conflict_waiters,
                &self.builder,
            );
        }
        self.changed.notify_all();
    }
}

impl Drop for PersistentBuilderRecoveryAdmission {
    fn drop(&mut self) {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            Err(poisoned) => poisoned.into_inner(),
        };
        if resources
            .persistent_builder_recovery_generations
            .get(&self.builder)
            == Some(&self.generation)
        {
            resources
                .persistent_builder_recovery_generations
                .remove(&self.builder);
        }
        if self.counted_request
            && let Some(in_flight) = resources
                .persistent_builder_requests_in_flight
                .get_mut(&self.builder)
        {
            *in_flight = in_flight.saturating_sub(1);
            if *in_flight == 0 {
                resources
                    .persistent_builder_requests_in_flight
                    .remove(&self.builder);
            }
        }
        self.changed.notify_all();
    }
}

fn decrement_builder_count(counts: &mut BTreeMap<String, usize>, builder: &str) -> bool {
    let Some(count) = counts.get(builder).copied() else {
        return false;
    };
    if count <= 1 {
        counts.remove(builder);
    } else {
        counts.insert(builder.to_owned(), count - 1);
    }
    true
}

fn decrement_bootstrap_create_count(resources: &mut OwnedDockerResources, builder: &str) -> bool {
    let Some(count) = resources
        .persistent_builder_create_requests_in_flight
        .get(builder)
        .copied()
    else {
        return false;
    };
    if count <= 1 {
        resources
            .persistent_builder_create_requests_in_flight
            .remove(builder);
    } else {
        resources
            .persistent_builder_create_requests_in_flight
            .insert(builder.to_owned(), count - 1);
    }
    true
}

impl PersistentBuilderAdmission {
    fn readiness_epoch(&self) -> Option<u64> {
        self.readiness_epoch
    }

    fn set_readiness_epoch(&mut self, readiness_epoch: u64) {
        self.readiness_epoch = Some(readiness_epoch);
    }

    fn validate_before_dispatch(&self, route: AuthorizedDockerRoute) -> Result<()> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, &self.builder, self.generation)?;
        if let Some(container_id) = self.container_id.as_deref() {
            let current_epoch = resources
                .persistent_builder_readiness_epochs
                .get(&self.builder)
                .copied()
                .unwrap_or_default();
            if self.readiness_epoch != Some(current_epoch) {
                bail!("persistent BuildKit request was authorized in a stale readiness epoch");
            }
            if matches!(
                route,
                AuthorizedDockerRoute::PersistentExecCreate | AuthorizedDockerRoute::PersistentExec
            ) {
                require_persistent_container_ready_locked(&resources, &self.builder, container_id)?;
            }
        }
        Ok(())
    }

    fn has_other_bootstrap_create(&self) -> Result<bool> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, &self.builder, self.generation)?;
        let count = resources
            .persistent_builder_create_requests_in_flight
            .get(&self.builder)
            .copied()
            .unwrap_or_default();
        let own_count = if self.is_bootstrap_create { 1 } else { 0 };
        Ok(count > own_count)
    }

    fn retire_bootstrap_create_dispatch(&mut self) -> Result<()> {
        if !self.is_bootstrap_create {
            return Ok(());
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, &self.builder, self.generation)?;
        if !decrement_bootstrap_create_count(&mut resources, &self.builder) {
            bail!("persistent BuildKit create admission count is missing");
        }
        self.is_bootstrap_create = false;
        self.changed.notify_all();
        Ok(())
    }

    fn mark_bootstrap_create_dispatchable(
        &mut self,
        resources: &mut OwnedDockerResources,
    ) -> Result<()> {
        ensure_persistent_builder_generation_locked(resources, &self.builder, self.generation)?;
        if self.is_bootstrap_create {
            bail!("persistent BuildKit create admission was registered twice");
        }
        let creates = resources
            .persistent_builder_create_requests_in_flight
            .get(&self.builder)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .context("persistent BuildKit in-flight create count overflow")?;
        resources
            .persistent_builder_create_requests_in_flight
            .insert(self.builder.clone(), creates);
        self.is_bootstrap_create = true;
        Ok(())
    }

    fn retire_bootstrap_create_as_conflict_waiter(&mut self) -> Result<()> {
        if !self.is_bootstrap_create || self.is_bootstrap_conflict_waiter {
            bail!("persistent BuildKit conflict does not own a create admission");
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, &self.builder, self.generation)?;
        if !decrement_bootstrap_create_count(&mut resources, &self.builder) {
            bail!("persistent BuildKit create admission count is missing");
        }
        let waiters = resources
            .persistent_builder_conflict_waiters
            .get(&self.builder)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .context("persistent BuildKit conflict waiter count overflow")?;
        resources
            .persistent_builder_conflict_waiters
            .insert(self.builder.clone(), waiters);
        self.is_bootstrap_create = false;
        self.is_bootstrap_conflict_waiter = true;
        self.changed.notify_all();
        Ok(())
    }
}

#[cfg(unix)]
impl PersistentBuilderAdmission {
    fn register_tunnel(
        &self,
        host: &std::os::unix::net::UnixStream,
        client: &std::os::unix::net::UnixStream,
    ) -> Result<PersistentBuilderTunnel> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, &self.builder, self.generation)?;
        if resources
            .persistent_builder_requests_closing
            .contains(&self.builder)
        {
            bail!("persistent BuildKit capability is closing before tunnel dispatch");
        }
        let id = resources
            .next_persistent_builder_tunnel
            .checked_add(1)
            .context("persistent BuildKit tunnel ID overflow")?;
        let state = PersistentBuilderTunnelState {
            builder: self.builder.clone(),
            generation: self.generation,
            host: host
                .try_clone()
                .context("clone BuildKit host stream for revocation")?,
            client: client
                .try_clone()
                .context("clone BuildKit guest stream for revocation")?,
        };
        resources.next_persistent_builder_tunnel = id;
        resources.persistent_builder_tunnels.insert(id, state);
        Ok(PersistentBuilderTunnel {
            resources: Arc::clone(&self.resources),
            changed: Arc::clone(&self.changed),
            id,
        })
    }
}

#[derive(Debug)]
struct AuthorizedDockerRequest {
    route: AuthorizedDockerRoute,
    /// Holding this permit through host dispatch and response observation
    /// linearizes the request against builder setup/revocation.
    _persistent_builder: Option<PersistentBuilderAdmission>,
    /// Immutable owned container ID resolved atomically with route auth.
    /// This also fences ordinary job-container aliases against replacement.
    owned_container_id: Option<String>,
}

impl AuthorizedDockerRequest {
    fn plain(route: AuthorizedDockerRoute) -> Self {
        Self {
            route,
            _persistent_builder: None,
            owned_container_id: None,
        }
    }

    fn fence(&self) -> Option<(&str, u64)> {
        self._persistent_builder
            .as_ref()
            .map(|admission| (admission.builder.as_str(), admission.generation))
    }

    fn container_id(&self) -> Option<&str> {
        self.owned_container_id.as_deref().or_else(|| {
            self._persistent_builder
                .as_ref()
                .and_then(|admission| admission.container_id.as_deref())
        })
    }

    fn readiness_epoch(&self) -> Option<u64> {
        self._persistent_builder
            .as_ref()
            .and_then(PersistentBuilderAdmission::readiness_epoch)
    }

    fn set_readiness_epoch(&mut self, readiness_epoch: u64) -> Result<()> {
        let admission = self
            ._persistent_builder
            .as_mut()
            .context("persistent BuildKit route has no capability admission")?;
        admission.set_readiness_epoch(readiness_epoch);
        Ok(())
    }

    fn validate_persistent_dispatch(&self, route: AuthorizedDockerRoute) -> Result<()> {
        if let Some(admission) = self._persistent_builder.as_ref() {
            admission.validate_before_dispatch(route)?;
        }
        Ok(())
    }

    fn has_other_persistent_bootstrap_create(&self) -> Result<bool> {
        self._persistent_builder
            .as_ref()
            .context("persistent BuildKit bootstrap has no builder admission")?
            .has_other_bootstrap_create()
    }

    fn retire_persistent_bootstrap_create(&mut self) -> Result<()> {
        self._persistent_builder
            .as_mut()
            .context("persistent BuildKit bootstrap has no builder admission")?
            .retire_bootstrap_create_dispatch()
    }

    fn retire_persistent_bootstrap_conflict(&mut self) -> Result<()> {
        self._persistent_builder
            .as_mut()
            .context("persistent BuildKit bootstrap has no builder admission")?
            .retire_bootstrap_create_as_conflict_waiter()
    }

    #[cfg(unix)]
    fn register_persistent_tunnel(
        &self,
        host: &std::os::unix::net::UnixStream,
        client: &std::os::unix::net::UnixStream,
    ) -> Result<PersistentBuilderTunnel> {
        self._persistent_builder
            .as_ref()
            .context("persistent Docker upgrade has no builder admission")?
            .register_tunnel(host, client)
    }
}

/// Validate a persistent request at its dispatch boundary. The caller must
/// hold the Engine/state-volume flock for the bound immutable container.
#[cfg(unix)]
fn validate_persistent_route_under_volume_lock(
    authorization: &mut AuthorizedDockerRequest,
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    route: AuthorizedDockerRoute,
) -> Result<()> {
    authorization.validate_persistent_dispatch(route)?;
    let Some((builder, generation)) = authorization
        .fence()
        .map(|(builder, generation)| (builder.to_owned(), generation))
    else {
        return Ok(());
    };
    let expected_epoch = authorization
        .readiness_epoch()
        .context("persistent BuildKit route has no readiness epoch")?;
    let durable_epoch = crate::buildkit::builder_readiness_epoch(domain, &builder)?;
    if durable_epoch != expected_epoch {
        if route != AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container) {
            bail!("persistent BuildKit request was admitted in a stale durable readiness epoch");
        }
        let container_id = authorization
            .container_id()
            .context("persistent container inspect has no immutable container ID")?;
        let config_fingerprint = policy.persistent_builder_config_fingerprint(&builder)?;
        if !crate::buildkit::builder_readiness_matches_epoch(
            domain,
            &builder,
            container_id,
            &config_fingerprint,
            durable_epoch,
        )? {
            bail!("persistent container inspect cannot refresh an unready durable epoch");
        }
        policy.note_persistent_ready_container(
            &builder,
            generation,
            container_id,
            &config_fingerprint,
            durable_epoch,
        )?;
        authorization.set_readiness_epoch(durable_epoch)?;
    }
    match route {
        AuthorizedDockerRoute::PersistentExecCreate | AuthorizedDockerRoute::PersistentExec => {
            let container_id = authorization
                .container_id()
                .context("persistent BuildKit exec route has no immutable container ID")?;
            let config_fingerprint = policy.persistent_builder_config_fingerprint(&builder)?;
            if !crate::buildkit::builder_readiness_matches_epoch(
                domain,
                &builder,
                container_id,
                &config_fingerprint,
                expected_epoch,
            )? {
                bail!("persistent BuildKit exec lacks current durable readiness proof");
            }
        }
        AuthorizedDockerRoute::Persistent(DockerResourceKind::Container) => {
            let container_id = authorization
                .container_id()
                .context("persistent BuildKit start has no immutable container ID")?;
            let config_fingerprint = policy.persistent_builder_config_fingerprint(&builder)?;
            if !crate::buildkit::builder_starting_readiness_matches_epoch(
                domain,
                &builder,
                container_id,
                &config_fingerprint,
                expected_epoch,
            )? {
                bail!("persistent BuildKit start lost its durable start epoch");
            }
        }
        AuthorizedDockerRoute::PersistentBootstrap
        | AuthorizedDockerRoute::PersistentArchive
        | AuthorizedDockerRoute::PersistentImagePull
        | AuthorizedDockerRoute::PersistentImageInspect
        | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container) => {}
        _ => {}
    }
    Ok(())
}

#[cfg(unix)]
fn advance_persistent_start_epoch_under_volume_lock(
    authorization: &mut AuthorizedDockerRequest,
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
) -> Result<u64> {
    let route = authorization.route;
    authorization.validate_persistent_dispatch(route)?;
    let (builder, generation) = authorization
        .fence()
        .context("persistent BuildKit start has no capability generation")?;
    let container_id = authorization
        .container_id()
        .context("persistent BuildKit start has no immutable container ID")?;
    let config_fingerprint = policy.persistent_builder_config_fingerprint(builder)?;
    let expected_epoch = authorization
        .readiness_epoch()
        .context("persistent BuildKit start has no readiness epoch")?;
    let new_epoch = crate::buildkit::invalidate_builder_readiness_before_start(
        domain,
        builder,
        container_id,
        &config_fingerprint,
        expected_epoch,
    )?;
    policy.note_persistent_start_epoch(
        builder,
        generation,
        container_id,
        expected_epoch,
        new_epoch,
    )?;
    authorization.set_readiness_epoch(new_epoch)?;
    Ok(new_epoch)
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

    pub(crate) fn new_with_volume_lock_root(
        job_container: &str,
        volume_lock_root: Option<PathBuf>,
    ) -> Result<Self> {
        let job_container = validate_owned_resource_id(job_container, "job container")?;
        let mut containers = BTreeSet::new();
        containers.insert(job_container);
        #[cfg(unix)]
        let volume_lock_root = volume_lock_root
            .map(|root| {
                crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&root)
                    .with_context(|| format!("secure Docker volume lock root {}", root.display()))
            })
            .transpose()?
            .map(Arc::new);
        #[cfg(not(unix))]
        let volume_lock_root = volume_lock_root.map(Arc::new);
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
                persistent_builder_config_fingerprints: BTreeMap::new(),
                persistent_builder_creator_leases: BTreeMap::new(),
                persistent_builder_generations: BTreeMap::new(),
                persistent_builder_requests_closing: BTreeSet::new(),
                persistent_builder_requests_closer_active: BTreeMap::new(),
                next_persistent_builder_closer: 0,
                persistent_builder_requests_in_flight: BTreeMap::new(),
                persistent_builder_create_requests_in_flight: BTreeMap::new(),
                persistent_builder_creator_lock_waiters: BTreeMap::new(),
                persistent_builder_conflict_waiters: BTreeMap::new(),
                persistent_builder_recovery_generations: BTreeMap::new(),
                #[cfg(unix)]
                persistent_builder_tunnels: BTreeMap::new(),
                #[cfg(unix)]
                next_persistent_builder_tunnel: 0,
                persistent_container_fresh_ids: BTreeSet::new(),
                persistent_container_config_archives: BTreeMap::new(),
                persistent_builder_readiness_epochs: BTreeMap::new(),
                persistent_container_ready_fingerprints: BTreeMap::new(),
                persistent_container_readiness_epochs: BTreeMap::new(),
                volume_locks: BTreeMap::new(),
                execs: BTreeSet::new(),
                persistent_execs: BTreeSet::new(),
                persistent_exec_builders: BTreeMap::new(),
                persistent_exec_containers: BTreeMap::new(),
                persistent_exec_generations: BTreeMap::new(),
                persistent_exec_readiness_epochs: BTreeMap::new(),
                reserved_resource_slots: 0,
            })),
            persistent_builder_requests_changed: Arc::new(Condvar::new()),
            volume_lock_root,
        })
    }

    fn reserve_owned_resource_slot(&self) -> Result<OwnedResourceReservation> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if owned_resource_count(&resources).saturating_add(resources.reserved_resource_slots)
            >= MAX_OWNED_DOCKER_RESOURCES
        {
            bail!("Docker lease ownership registry is full");
        }
        resources.reserved_resource_slots += 1;
        Ok(OwnedResourceReservation {
            resources: Arc::clone(&self.resources),
            state: OwnedResourceReservationState::Reserved,
        })
    }

    fn admit_persistent_builder_locked(
        &self,
        resources: &mut OwnedDockerResources,
        builder: &str,
    ) -> Result<PersistentBuilderAdmission> {
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
        {
            bail!("persistent BuildKit capability is being revoked or replaced");
        }
        if resources
            .persistent_builder_recovery_generations
            .contains_key(builder)
        {
            bail!("persistent BuildKit capability is in host recovery");
        }
        if !resources.persistent_builders.contains(builder) {
            bail!("persistent BuildKit builder is not active in this lease");
        }
        let generation = resources
            .persistent_builder_generations
            .get(builder)
            .copied()
            .context("persistent BuildKit builder has no capability generation")?;
        let readiness_epoch = resources
            .persistent_builder_readiness_epochs
            .get(builder)
            .copied()
            .unwrap_or_default();
        let in_flight = resources
            .persistent_builder_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .context("persistent BuildKit in-flight request count overflow")?;
        resources
            .persistent_builder_requests_in_flight
            .insert(builder.to_owned(), in_flight);
        Ok(PersistentBuilderAdmission {
            resources: Arc::clone(&self.resources),
            changed: Arc::clone(&self.persistent_builder_requests_changed),
            builder: builder.to_owned(),
            generation,
            container_id: None,
            readiness_epoch: Some(readiness_epoch),
            is_bootstrap_create: false,
            is_bootstrap_conflict_waiter: false,
        })
    }

    fn begin_persistent_builder_recovery(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        generation: u64,
        config_fingerprint: &str,
    ) -> Result<Option<PersistentBuilderRecoveryAdmission>> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources
            .persistent_builder_creator_leases
            .get(builder)
            .is_some_and(|lease| lease.matches(domain, builder, config_fingerprint, generation))
        {
            bail!("persistent BuildKit recovery lacks this generation's live creator lease");
        }
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
            || resources
                .persistent_builder_recovery_generations
                .contains_key(builder)
        {
            return Ok(None);
        }
        let requests = resources
            .persistent_builder_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default();
        let conflict_waiters = resources
            .persistent_builder_conflict_waiters
            .get(builder)
            .copied()
            .unwrap_or_default();
        let creates = resources
            .persistent_builder_create_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default();
        let creator_lock_waiters = resources
            .persistent_builder_creator_lock_waiters
            .get(builder)
            .copied()
            .unwrap_or_default();
        // A stale Created recovery may run only after every current request
        // has received 409 and retired its create dispatch. This prevents an
        // observer from racing the actual winner or a guest request.
        if requests.saturating_sub(creator_lock_waiters) != conflict_waiters
            || conflict_waiters == 0
            || creates != 0
        {
            return Ok(None);
        }
        resources
            .persistent_builder_recovery_generations
            .insert(builder.to_owned(), generation);
        Ok(Some(PersistentBuilderRecoveryAdmission {
            resources: Arc::clone(&self.resources),
            changed: Arc::clone(&self.persistent_builder_requests_changed),
            builder: builder.to_owned(),
            generation,
            counted_request: false,
        }))
    }

    fn begin_pending_create_recovery(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        generation: u64,
        config_fingerprint: &str,
    ) -> Result<Option<PersistentBuilderRecoveryAdmission>> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources
            .persistent_builder_creator_leases
            .get(builder)
            .is_some_and(|lease| lease.matches(domain, builder, config_fingerprint, generation))
        {
            bail!("pending-create recovery lacks this generation's creator lease");
        }
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
            || resources
                .persistent_builder_recovery_generations
                .contains_key(builder)
        {
            return Ok(None);
        }
        // The caller itself accounts for one active request. Recovery must be
        // the only request using this builder so no guest start/archive or
        // competing create can observe a half-recovered daemon.
        if resources
            .persistent_builder_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default()
            != 1
            || resources
                .persistent_builder_create_requests_in_flight
                .get(builder)
                .copied()
                .unwrap_or_default()
                > 1
        {
            return Ok(None);
        }
        resources
            .persistent_builder_recovery_generations
            .insert(builder.to_owned(), generation);
        Ok(Some(PersistentBuilderRecoveryAdmission {
            resources: Arc::clone(&self.resources),
            changed: Arc::clone(&self.persistent_builder_requests_changed),
            builder: builder.to_owned(),
            generation,
            counted_request: false,
        }))
    }

    fn begin_pending_create_setup_recovery(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        generation: u64,
        config_fingerprint: &str,
    ) -> Result<Option<PersistentBuilderRecoveryAdmission>> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources.persistent_builder_generations.get(builder) != Some(&generation)
            || !resources.persistent_builder_setups.contains(builder)
            || resources.persistent_builders.contains(builder)
            || resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .map(String::as_str)
                != Some(config_fingerprint)
        {
            bail!("pending-create recovery is outside the current host setup generation");
        }
        if !resources
            .persistent_builder_creator_leases
            .get(builder)
            .is_some_and(|lease| lease.matches(domain, builder, config_fingerprint, generation))
        {
            bail!("pending-create setup recovery lacks its exact creator lease");
        }
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
            || resources
                .persistent_builder_recovery_generations
                .contains_key(builder)
        {
            return Ok(None);
        }
        if resources
            .persistent_builder_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default()
            != 0
            || resources
                .persistent_builder_create_requests_in_flight
                .get(builder)
                .copied()
                .unwrap_or_default()
                != 0
        {
            bail!("guest Docker requests are active during host BuildKit setup recovery");
        }
        let in_flight = resources
            .persistent_builder_requests_in_flight
            .get(builder)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .context("persistent BuildKit setup recovery count overflow")?;
        resources
            .persistent_builder_requests_in_flight
            .insert(builder.to_owned(), in_flight);
        resources
            .persistent_builder_recovery_generations
            .insert(builder.to_owned(), generation);
        Ok(Some(PersistentBuilderRecoveryAdmission {
            resources: Arc::clone(&self.resources),
            changed: Arc::clone(&self.persistent_builder_requests_changed),
            builder: builder.to_owned(),
            generation,
            counted_request: true,
        }))
    }

    fn ensure_pending_create_creator_lease(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        config_fingerprint: &str,
        generation: u64,
    ) -> Result<()> {
        {
            let resources = self
                .resources
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            if resources.persistent_builder_generations.get(builder) != Some(&generation)
                || (!resources.persistent_builders.contains(builder)
                    && !resources.persistent_builder_setups.contains(builder))
            {
                bail!("persistent BuildKit capability was revoked or replaced");
            }
            if resources
                .persistent_builder_creator_leases
                .get(builder)
                .is_some_and(|lease| lease.matches(domain, builder, config_fingerprint, generation))
            {
                return Ok(());
            }
            if resources
                .persistent_builder_creator_leases
                .contains_key(builder)
            {
                bail!("pending-create recovery found a different live creator lease");
            }
        }

        let lease = crate::buildkit::begin_persistent_builder_creator_lease(
            domain,
            builder,
            config_fingerprint,
            generation,
        )?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
            || resources.persistent_builder_generations.get(builder) != Some(&generation)
            || (!resources.persistent_builders.contains(builder)
                && !resources.persistent_builder_setups.contains(builder))
            || resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .map(String::as_str)
                != Some(config_fingerprint)
            || resources
                .persistent_builder_creator_leases
                .contains_key(builder)
        {
            bail!("pending-create recovery admission changed while acquiring creator lease");
        }
        resources
            .persistent_builder_creator_leases
            .insert(builder.to_owned(), lease);
        Ok(())
    }

    fn with_persistent_builder_admission_closed<T>(
        &self,
        builder: &str,
        operation: impl FnOnce(&mut OwnedDockerResources) -> Result<T>,
    ) -> Result<T> {
        self.with_persistent_builder_admission_closed_and_wait_hook(builder, operation, || {})
    }

    fn with_persistent_builder_admission_closed_and_wait_hook<T>(
        &self,
        builder: &str,
        operation: impl FnOnce(&mut OwnedDockerResources) -> Result<T>,
        mut before_wait: impl FnMut(),
    ) -> Result<T> {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            Err(poisoned) => {
                let mut resources = poisoned.into_inner();
                // Poison means the registry cannot safely authorize or mutate
                // anything again. This invocation owns no closer token yet,
                // so it must not clear a marker owned by another closer.
                resources
                    .persistent_builder_requests_closing
                    .insert(builder.to_owned());
                self.persistent_builder_requests_changed.notify_all();
                return Err(anyhow::anyhow!(
                    "Docker lease ownership registry is poisoned"
                ));
            }
        };
        while resources
            .persistent_builder_requests_closer_active
            .contains_key(builder)
        {
            before_wait();
            resources = match self.persistent_builder_requests_changed.wait(resources) {
                Ok(resources) => resources,
                Err(poisoned) => {
                    let mut resources = poisoned.into_inner();
                    // This contender never acquired the active closer token.
                    // Preserve the current owner's marker while keeping
                    // admission closed after registry poisoning.
                    resources
                        .persistent_builder_requests_closing
                        .insert(builder.to_owned());
                    self.persistent_builder_requests_changed.notify_all();
                    return Err(anyhow::anyhow!(
                        "Docker lease ownership registry is poisoned"
                    ));
                }
            };
        }
        let closer_id = resources
            .next_persistent_builder_closer
            .checked_add(1)
            .context("persistent BuildKit closer ID overflow")?;
        resources.next_persistent_builder_closer = closer_id;
        resources
            .persistent_builder_requests_closing
            .insert(builder.to_owned());
        resources
            .persistent_builder_requests_closer_active
            .insert(builder.to_owned(), closer_id);
        #[cfg(unix)]
        {
            let generation = resources
                .persistent_builder_generations
                .get(builder)
                .copied()
                .unwrap_or(u64::MAX);
            let shutdown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                shutdown_persistent_builder_tunnels(
                    &mut resources,
                    &self.persistent_builder_requests_changed,
                    builder,
                    generation,
                    closer_id,
                    |stream| stream.shutdown(std::net::Shutdown::Both),
                )
            }));
            match shutdown {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    // Failed shutdown leaves admission closed, but releases
                    // the active closer so a later caller can retry.
                    clear_persistent_builder_closer(&mut resources, builder, closer_id);
                    self.persistent_builder_requests_changed.notify_all();
                    return Err(error);
                }
                Err(payload) => {
                    // Do not unwind while holding the registry mutex: that
                    // would poison it and strand other closers. Authority
                    // stays closed until a separate explicit recovery.
                    clear_persistent_builder_closer(&mut resources, builder, closer_id);
                    resources
                        .persistent_builder_requests_closing
                        .insert(builder.to_owned());
                    self.persistent_builder_requests_changed.notify_all();
                    drop(resources);
                    std::panic::resume_unwind(payload);
                }
            }
        }
        loop {
            let requests_in_flight = resources
                .persistent_builder_requests_in_flight
                .get(builder)
                .copied()
                .unwrap_or_default();
            #[cfg(unix)]
            let tunnels_in_flight = resources
                .persistent_builder_tunnels
                .values()
                .any(|tunnel| tunnel.builder == builder);
            #[cfg(not(unix))]
            let tunnels_in_flight = false;
            if requests_in_flight == 0 && !tunnels_in_flight {
                break;
            }
            before_wait();
            resources = match self.persistent_builder_requests_changed.wait(resources) {
                Ok(resources) => resources,
                Err(poisoned) => {
                    let mut resources = poisoned.into_inner();
                    clear_persistent_builder_closer(&mut resources, builder, closer_id);
                    resources
                        .persistent_builder_requests_closing
                        .insert(builder.to_owned());
                    self.persistent_builder_requests_changed.notify_all();
                    return Err(anyhow::anyhow!(
                        "Docker lease ownership registry is poisoned"
                    ));
                }
            };
        }
        let operation =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut resources)));
        clear_persistent_builder_closer(&mut resources, builder, closer_id);
        if matches!(&operation, Ok(Ok(_))) {
            resources
                .persistent_builder_requests_closing
                .remove(builder);
        } else {
            resources
                .persistent_builder_requests_closing
                .insert(builder.to_owned());
        }
        self.persistent_builder_requests_changed.notify_all();
        match operation {
            Ok(result) => result,
            Err(payload) => {
                drop(resources);
                std::panic::resume_unwind(payload);
            }
        }
    }

    fn authorize_builder_route(
        &self,
        route: AuthorizedDockerRoute,
        upgrade: bool,
        builder: &str,
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(route, upgrade)?;
        let creator_domain = if matches!(route, AuthorizedDockerRoute::PersistentBootstrap) {
            Some(
                crate::buildkit::PersistentBuildKitDomain::resolve()
                    .context("resolve persistent BuildKit creator domain")?,
            )
        } else {
            None
        };
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let admission = self.admit_persistent_builder_locked(&mut resources, builder)?;
        let generation = admission.generation;
        authorization._persistent_builder = Some(admission);
        if matches!(route, AuthorizedDockerRoute::PersistentBootstrap) {
            let domain = creator_domain
                .as_ref()
                .context("persistent BuildKit creator domain was not resolved")?;
            if crate::buildkit::persistent_builder_domain_token(builder)
                != Some(domain.token.as_str())
            {
                bail!("persistent BuildKit creator belongs to another domain");
            }
            let config_fingerprint = resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .cloned()
                .context("persistent BuildKit config mode was not registered")?;
            if resources
                .persistent_builder_creator_leases
                .get(builder)
                .is_some_and(|lease| {
                    lease.matches(&domain, builder, &config_fingerprint, generation)
                })
            {
                authorization
                    ._persistent_builder
                    .as_mut()
                    .context("persistent bootstrap admission was not retained")?
                    .mark_bootstrap_create_dispatchable(&mut resources)?;
                return Ok(authorization);
            }

            // Do not wait for the cross-process creator flock while holding
            // the resource registry. The admitted request pins this builder
            // generation while another lease completes its bootstrap.
            let waiting = resources
                .persistent_builder_creator_lock_waiters
                .get(builder)
                .copied()
                .unwrap_or_default()
                .checked_add(1)
                .context("persistent BuildKit creator-lock waiter count overflow")?;
            resources
                .persistent_builder_creator_lock_waiters
                .insert(builder.to_owned(), waiting);
            drop(resources);
            let creator_result = crate::buildkit::begin_persistent_builder_creator_lease(
                domain,
                builder,
                &config_fingerprint,
                generation,
            );
            self.finish_persistent_builder_creator_admission(
                domain,
                builder,
                &config_fingerprint,
                generation,
                creator_result,
                authorization
                    ._persistent_builder
                    .as_mut()
                    .context("persistent bootstrap admission was not retained")?,
            )?;
        }
        Ok(authorization)
    }

    fn finish_persistent_builder_creator_admission(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        config_fingerprint: &str,
        generation: u64,
        creator_result: Result<crate::buildkit::PersistentBuildKitCreatorLease>,
        admission: &mut PersistentBuilderAdmission,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !decrement_builder_count(
            &mut resources.persistent_builder_creator_lock_waiters,
            builder,
        ) {
            bail!("persistent BuildKit creator-lock waiter count is missing");
        }
        self.persistent_builder_requests_changed.notify_all();
        let creator = creator_result?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        // A concurrent 409 may be recovering the exact Created daemon while
        // this request was blocked on its creator flock. Do not install a
        // second creator record or dispatch Create until that recovery has
        // published readiness and dropped its gate.
        while resources
            .persistent_builder_recovery_generations
            .contains_key(builder)
        {
            resources = self
                .persistent_builder_requests_changed
                .wait(resources)
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        }
        if resources
            .persistent_builder_requests_closing
            .contains(builder)
            || !resources.persistent_builders.contains(builder)
            || resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .map(String::as_str)
                != Some(config_fingerprint)
        {
            bail!("persistent BuildKit creator admission changed while acquiring its lease");
        }
        if let Some(existing) = resources.persistent_builder_creator_leases.get(builder) {
            if !existing.matches(domain, builder, config_fingerprint, generation) {
                bail!("persistent BuildKit creator lease changed during admission");
            }
            bail!("persistent BuildKit creator admission raced another setup");
        }
        resources
            .persistent_builder_creator_leases
            .insert(builder.to_owned(), creator);
        admission.mark_bootstrap_create_dispatchable(&mut resources)
    }

    fn authorize_active_builder_route(
        &self,
        route: AuthorizedDockerRoute,
        upgrade: bool,
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(route, upgrade)?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let builder = resources
            .persistent_builders
            .iter()
            .find(|builder| {
                !resources
                    .persistent_builder_requests_closing
                    .contains(builder.as_str())
            })
            .cloned()
            .context("Docker lease has no active persistent BuildKit builder")?;
        let admission = self.admit_persistent_builder_locked(&mut resources, &builder)?;
        authorization._persistent_builder = Some(admission);
        Ok(authorization)
    }

    fn authorize_owned_container_route(
        &self,
        route: AuthorizedDockerRoute,
        upgrade: bool,
        target: &str,
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(route, upgrade)?;
        let target = validate_owned_resource_id(target, "Docker container")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let id = resources
            .container_names
            .get(&target)
            .filter(|id| resources.containers.contains(*id))
            .cloned()
            .or_else(|| {
                resources
                    .containers
                    .contains(&target)
                    .then_some(target.clone())
            })
            .ok_or_else(|| {
                LeaseDeny::not_found(format!(
                    "Docker lease denied foreign container resource {target:?}"
                ))
            })?;
        authorization.owned_container_id = Some(id);
        Ok(authorization)
    }

    fn authorize_network_container_route(
        &self,
        upgrade: bool,
        network: &str,
        request: &[u8],
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(
            AuthorizedDockerRoute::Owned(DockerResourceKind::Network),
            upgrade,
        )?;
        let network = validate_owned_resource_id(network, "Docker network")?;
        let body = docker_request_body(request)?;
        let object = parse_create_value(body)
            .context("parse Docker network container request")?
            .as_object()
            .cloned()
            .context("Docker network container request must be an object")?;
        reject_case_insensitive_duplicate_keys(&object, "Docker network container request")?;
        let key = object
            .keys()
            .find(|key| key.eq_ignore_ascii_case("Container"))
            .cloned()
            .context("Docker network request must name a container")?;
        let target = object
            .get(&key)
            .and_then(Value::as_str)
            .context("Docker network container reference must be a string")?;
        let target = validate_owned_resource_id(target, "Docker container")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !resources.networks.contains(&network) {
            return Err(LeaseDeny::not_found(format!(
                "Docker lease denied foreign network resource {network:?}"
            ))
            .into());
        }
        let id = resources
            .container_names
            .get(&target)
            .filter(|id| resources.containers.contains(*id))
            .cloned()
            .or_else(|| {
                resources
                    .containers
                    .contains(&target)
                    .then_some(target.clone())
            })
            .ok_or_else(|| {
                LeaseDeny::not_found(format!(
                    "Docker lease denied foreign container resource {target:?}"
                ))
            })?;
        authorization.owned_container_id = Some(id);
        Ok(authorization)
    }

    fn authorize_persistent_container_route(
        &self,
        route: AuthorizedDockerRoute,
        upgrade: bool,
        target: &str,
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(route, upgrade)?;
        let route = authorization.route;
        let target = validate_owned_resource_id(target, "persistent Docker container")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let builder = if let Some(builder) = persistent_buildkit_builder_name(&target) {
            builder.to_owned()
        } else {
            resources
                .persistent_containers
                .iter()
                .find_map(|(name, id)| {
                    (name.as_str() == target.as_str() || id.as_str() == target.as_str())
                        .then_some(name)
                })
                .and_then(|name| persistent_buildkit_builder_name(name))
                .map(str::to_owned)
                .context("persistent container is not bound to an active builder")?
        };
        if !matches!(
            route,
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        ) && !resources.persistent_containers.iter().any(|(name, id)| {
            (name.as_str() == target.as_str() || id.as_str() == target.as_str())
                && persistent_buildkit_builder_name(name) == Some(builder.as_str())
        }) {
            bail!("persistent container route has no attested immutable container binding");
        }
        if matches!(
            route,
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        ) {
            // A fresh inspect must revalidate durable readiness before it can
            // leave BuildKit exec authority enabled. Retire cached readiness
            // and derived exec IDs under the same registry lock before the
            // request is dispatched; the observer restores readiness only
            // after the immutable ID and durable record both pass.
            if let Some(id) = resources
                .persistent_containers
                .iter()
                .find(|(name, id)| {
                    name.as_str() == target.as_str() || id.as_str() == target.as_str()
                })
                .map(|(_, id)| id.clone())
            {
                resources
                    .persistent_container_ready_fingerprints
                    .remove(&id);
                resources.persistent_container_readiness_epochs.remove(&id);
                remove_persistent_execs_for_container(&mut resources, &id);
            }
        }
        if matches!(
            route,
            AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
        ) {
            let (_, id) = resources
                .persistent_containers
                .iter()
                .find(|(name, id)| {
                    (name.as_str() == target.as_str() || id.as_str() == target.as_str())
                        && persistent_buildkit_builder_name(name) == Some(builder.as_str())
                })
                .map(|(name, id)| (name.clone(), id.clone()))
                .context("persistent start route lost its container binding")?;
            let expected = resources
                .persistent_builder_config_fingerprints
                .get(&builder)
                .context("persistent BuildKit config mode was not registered")?;
            let current_epoch = resources
                .persistent_builder_readiness_epochs
                .get(&builder)
                .copied()
                .unwrap_or_default();
            let ready = resources.persistent_container_ready_fingerprints.get(&id)
                == Some(expected)
                && resources.persistent_container_readiness_epochs.get(&id) == Some(&current_epoch);
            let fresh = resources.persistent_container_fresh_ids.contains(&id)
                && resources.persistent_container_config_archives.get(&id) == Some(expected);
            if !ready && !fresh {
                bail!("persistent BuildKit start lacks exact config and readiness proof");
            }
        } else if matches!(route, AuthorizedDockerRoute::PersistentExecCreate) {
            let (_, id) = resources
                .persistent_containers
                .iter()
                .find(|(name, id)| {
                    (name.as_str() == target.as_str() || id.as_str() == target.as_str())
                        && persistent_buildkit_builder_name(name) == Some(builder.as_str())
                })
                .context("persistent exec-create route lost its container binding")?;
            require_persistent_container_ready_locked(&resources, &builder, id)?;
        }
        let mut admission = self.admit_persistent_builder_locked(&mut resources, &builder)?;
        admission.container_id = resources
            .persistent_containers
            .iter()
            .find_map(|(name, id)| {
                (name == &target || id == &target)
                    .then_some(id.clone())
                    .filter(|_| persistent_buildkit_builder_name(name) == Some(builder.as_str()))
            });
        authorization._persistent_builder = Some(admission);
        Ok(authorization)
    }

    fn authorize_persistent_exec_route(
        &self,
        route: AuthorizedDockerRoute,
        upgrade: bool,
        exec_id: &str,
    ) -> Result<AuthorizedDockerRequest> {
        let mut authorization = authorize_docker_route(route, upgrade)?;
        let exec_id = validate_owned_resource_id(exec_id, "Docker exec")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if !resources.persistent_execs.contains(&exec_id) {
            bail!("persistent BuildKit exec capability is not registered");
        }
        let builder = resources
            .persistent_exec_builders
            .get(&exec_id)
            .cloned()
            .context("persistent BuildKit exec has no builder binding")?;
        let container_id = resources
            .persistent_exec_containers
            .get(&exec_id)
            .cloned()
            .context("persistent BuildKit exec has no immutable container binding")?;
        let generation = resources
            .persistent_exec_generations
            .get(&exec_id)
            .copied()
            .context("persistent BuildKit exec has no capability generation")?;
        ensure_persistent_builder_generation_locked(&resources, &builder, generation)?;
        if !resources.persistent_containers.iter().any(|(name, id)| {
            id == &container_id && persistent_buildkit_builder_name(name) == Some(builder.as_str())
        }) {
            bail!("persistent BuildKit exec container binding is stale");
        }
        let readiness_epoch = resources
            .persistent_exec_readiness_epochs
            .get(&exec_id)
            .copied()
            .context("persistent BuildKit exec has no readiness epoch")?;
        if resources.persistent_builder_readiness_epochs.get(&builder) != Some(&readiness_epoch) {
            bail!("persistent BuildKit exec belongs to a stale readiness epoch");
        }
        require_persistent_container_ready_locked(&resources, &builder, &container_id)?;
        let mut admission = self.admit_persistent_builder_locked(&mut resources, &builder)?;
        debug_assert_eq!(admission.generation, generation);
        admission.container_id = Some(container_id);
        admission.readiness_epoch = Some(readiness_epoch);
        authorization._persistent_builder = Some(admission);
        Ok(authorization)
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
        resources
            .persistent_builder_generations
            .entry(builder.clone())
            .or_insert(1);
        resources
            .persistent_builder_readiness_epochs
            .entry(builder.clone())
            .or_insert(0);
        resources
            .persistent_builder_config_fingerprints
            .entry(builder.clone())
            .or_insert_with(|| "no-config-v1".to_owned());
        resources.persistent_builders.insert(builder);
        Ok(())
    }

    /// Start host-side persistent-builder setup without exposing any guest
    /// route. Image and volume registration accepts this pending state so the
    /// final capability grant can happen only after both attestations pass.
    fn begin_persistent_builder_setup(
        &self,
        builder: &str,
        config_fingerprint: &str,
    ) -> Result<u64> {
        let builder = validate_owned_resource_id(builder, "persistent BuildKit builder")?;
        if !crate::buildkit::is_persistent_builder_name(&builder) {
            bail!("persistent BuildKit builder is outside the Velnor namespace");
        }
        self.with_persistent_builder_admission_closed(&builder, |resources| {
            resources.persistent_builder_creator_leases.remove(&builder);
            resources.persistent_builders.remove(&builder);
            resources.persistent_builder_images.remove(&builder);
            let generation = resources
                .persistent_builder_generations
                .get(&builder)
                .copied()
                .unwrap_or_default()
                .checked_add(1)
                .context("persistent BuildKit capability generation overflow")?;
            resources
                .persistent_builder_generations
                .insert(builder.clone(), generation);
            resources
                .persistent_builder_config_fingerprints
                .insert(builder.clone(), config_fingerprint.to_owned());
            resources
                .persistent_builder_readiness_epochs
                .insert(builder.clone(), 0);
            let volume = crate::buildkit::daemon_state_volume(&builder);
            resources.persistent_volumes.remove(&volume);
            resources
                .persistent_container_volumes
                .retain(|_, existing_volume| existing_volume != &volume);
            let removed_ids = resources
                .persistent_containers
                .iter()
                .filter(|(name, _)| {
                    persistent_buildkit_builder_name(name) == Some(builder.as_str())
                })
                .map(|(_, id)| id.clone())
                .collect::<BTreeSet<_>>();
            resources.persistent_containers.retain(|name, id| {
                persistent_buildkit_builder_name(name) != Some(builder.as_str())
                    && !removed_ids.contains(id)
            });
            for id in &removed_ids {
                resources.persistent_container_candidates.remove(id);
                resources.persistent_container_fresh_ids.remove(id);
                resources.persistent_container_config_archives.remove(id);
                resources.persistent_container_ready_fingerprints.remove(id);
                resources.persistent_container_readiness_epochs.remove(id);
            }
            remove_persistent_execs_for_builder(resources, &builder);
            resources.persistent_builder_setups.insert(builder.clone());
            Ok(generation)
        })
    }

    /// Complete host-side setup and expose the exact builder capability to
    /// the guest. Both the pinned image and state volume must already be
    /// registered by strict host inspection.
    fn complete_persistent_builder_setup(&self, builder: &str, readiness_epoch: u64) -> Result<()> {
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
        if !resources
            .persistent_builder_config_fingerprints
            .contains_key(&builder)
        {
            bail!("persistent BuildKit config mode was not registered");
        }
        let current_epoch = resources
            .persistent_builder_readiness_epochs
            .get(&builder)
            .copied()
            .unwrap_or_default();
        if current_epoch > readiness_epoch {
            bail!("BuildKit setup readiness epoch moved backwards");
        }
        resources
            .persistent_builder_readiness_epochs
            .insert(builder.clone(), readiness_epoch);
        resources.persistent_builder_setups.remove(&builder);
        resources.persistent_builders.insert(builder);
        Ok(())
    }

    fn persistent_builder_config_fingerprint(&self, builder: &str) -> Result<String> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources
            .persistent_builder_config_fingerprints
            .get(builder)
            .cloned()
            .context("persistent BuildKit config mode was not registered")
    }

    fn allow_ready_persistent_container(
        &self,
        builder: &str,
        container_id: &str,
        config_fingerprint: &str,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let expected = resources
            .persistent_builder_config_fingerprints
            .get(builder)
            .context("persistent BuildKit config mode was not registered")?;
        if expected != config_fingerprint {
            bail!("persistent BuildKit readiness proof has a different config mode");
        }
        if !resources.persistent_builders.contains(builder) {
            bail!("persistent BuildKit builder is not active in this lease");
        }
        let known_id = resources.persistent_containers.iter().any(|(name, id)| {
            id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
        });
        if !known_id {
            bail!("persistent BuildKit readiness proof ID was not attested in this lease");
        }
        resources
            .persistent_container_ready_fingerprints
            .insert(container_id.to_owned(), config_fingerprint.to_owned());
        let readiness_epoch = resources
            .persistent_builder_readiness_epochs
            .get(builder)
            .copied()
            .unwrap_or_default();
        resources
            .persistent_container_readiness_epochs
            .insert(container_id.to_owned(), readiness_epoch);
        resources
            .persistent_container_fresh_ids
            .remove(container_id);
        resources
            .persistent_container_config_archives
            .remove(container_id);
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
        self.with_persistent_builder_admission_closed(&builder, |resources| {
            let volume = crate::buildkit::daemon_state_volume(&builder);
            resources.persistent_builder_creator_leases.remove(&builder);
            resources.persistent_builders.remove(&builder);
            resources.persistent_builder_setups.remove(&builder);
            resources.persistent_builder_images.remove(&builder);
            resources
                .persistent_builder_config_fingerprints
                .remove(&builder);
            let generation = resources
                .persistent_builder_generations
                .get(&builder)
                .copied()
                .unwrap_or_default()
                .checked_add(1)
                .context("persistent BuildKit capability generation overflow")?;
            resources
                .persistent_builder_generations
                .insert(builder.clone(), generation);
            resources.persistent_volumes.remove(&volume);
            let removed_ids = resources
                .persistent_containers
                .iter()
                .filter(|(name, _)| {
                    persistent_buildkit_builder_name(name) == Some(builder.as_str())
                })
                .map(|(_, id)| id.clone())
                .collect::<BTreeSet<_>>();
            resources.persistent_containers.retain(|name, id| {
                persistent_buildkit_builder_name(name) != Some(builder.as_str())
                    && !removed_ids.contains(id)
            });
            for id in &removed_ids {
                resources.persistent_container_candidates.remove(id);
                resources.persistent_container_fresh_ids.remove(id);
                resources.persistent_container_config_archives.remove(id);
                resources.persistent_container_ready_fingerprints.remove(id);
                resources.persistent_container_readiness_epochs.remove(id);
            }
            resources
                .persistent_container_volumes
                .retain(|_, state_volume| state_volume != &volume);
            remove_persistent_execs_for_builder(resources, &builder);
            Ok(())
        })
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

    fn persistent_container_can_start(&self, target: &str) -> Result<bool> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let Some((name, id)) = resources
            .persistent_containers
            .iter()
            .find(|(name, id)| *name == &target || *id == &target)
        else {
            return Ok(false);
        };
        let Some(builder) = persistent_buildkit_builder_name(name) else {
            return Ok(false);
        };
        let Some(expected) = resources
            .persistent_builder_config_fingerprints
            .get(builder)
        else {
            return Ok(false);
        };
        let current_epoch = resources
            .persistent_builder_readiness_epochs
            .get(builder)
            .copied()
            .unwrap_or_default();
        let ready = resources.persistent_container_ready_fingerprints.get(id) == Some(expected)
            && resources.persistent_container_readiness_epochs.get(id) == Some(&current_epoch);
        let fresh = resources.persistent_container_fresh_ids.contains(id)
            && resources.persistent_container_config_archives.get(id) == Some(expected);
        Ok(resources.persistent_builders.contains(builder) && (ready || fresh))
    }

    fn is_fresh_persistent_container(&self, container_id: &str) -> Result<bool> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources
            .persistent_container_fresh_ids
            .contains(container_id))
    }

    fn persistent_exec_is_current(&self, exec_id: &str) -> Result<bool> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let Some(builder) = resources.persistent_exec_builders.get(exec_id) else {
            return Ok(false);
        };
        let Some(container_id) = resources.persistent_exec_containers.get(exec_id) else {
            return Ok(false);
        };
        let Some(generation) = resources.persistent_exec_generations.get(exec_id) else {
            return Ok(false);
        };
        let Some(readiness_epoch) = resources.persistent_exec_readiness_epochs.get(exec_id) else {
            return Ok(false);
        };
        if resources.persistent_builder_generations.get(builder) != Some(generation)
            || !resources.persistent_builders.contains(builder)
            || resources.persistent_builder_readiness_epochs.get(builder) != Some(readiness_epoch)
        {
            return Ok(false);
        }
        let container_is_current = resources.persistent_containers.iter().any(|(name, id)| {
            id == container_id && persistent_buildkit_builder_name(name) == Some(builder.as_str())
        });
        Ok(container_is_current
            && require_persistent_container_ready_locked(&resources, builder, container_id).is_ok())
    }

    fn persistent_exec_binding(&self, exec_id: &str) -> Result<(String, String, u64)> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let builder = resources
            .persistent_exec_builders
            .get(exec_id)
            .cloned()
            .context("persistent BuildKit exec has no builder binding")?;
        let container_id = resources
            .persistent_exec_containers
            .get(exec_id)
            .cloned()
            .context("persistent BuildKit exec has no immutable container binding")?;
        let generation = resources
            .persistent_exec_generations
            .get(exec_id)
            .copied()
            .context("persistent BuildKit exec has no capability generation")?;
        Ok((builder, container_id, generation))
    }

    fn note_persistent_config_archive(
        &self,
        builder: &str,
        container_id: &str,
        generation: u64,
        status: u16,
        fingerprint: &str,
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
            .context("resolve persistent BuildKit archive domain")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources.persistent_builders.contains(builder) {
            bail!("persistent BuildKit archive arrived outside active builder setup");
        }
        let expected = resources
            .persistent_builder_config_fingerprints
            .get(builder)
            .context("persistent BuildKit config mode was not registered")?;
        if expected != fingerprint {
            bail!("persistent BuildKit config archive did not match runner input bytes");
        }
        if !resources
            .persistent_container_fresh_ids
            .contains(container_id)
        {
            bail!("persistent BuildKit config archive did not target a newly attested container");
        }
        if crate::buildkit::persistent_builder_domain_token(builder) != Some(domain.token.as_str())
            || !resources
                .persistent_builder_creator_leases
                .get(builder)
                .is_some_and(|lease| lease.matches(&domain, builder, expected, generation))
        {
            bail!("persistent BuildKit archive has no matching live creator lease");
        }
        drop(resources);
        crate::buildkit::record_persistent_builder_creator_archive(
            &domain,
            builder,
            generation,
            fingerprint,
            container_id,
            fingerprint,
        )?;
        let access = crate::buildkit::pending_buildkit_create_access(
            &domain,
            builder,
            fingerprint,
            generation,
        )?
        .context("persistent BuildKit archive has no durable pending-create transaction")?;
        crate::buildkit::record_pending_buildkit_create_archive(
            &domain,
            builder,
            &access.transaction_id,
            container_id,
            fingerprint,
        )?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources.persistent_builders.contains(builder)
            || resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .map(String::as_str)
                != Some(fingerprint)
            || !resources
                .persistent_container_fresh_ids
                .contains(container_id)
        {
            bail!("persistent BuildKit archive binding changed while being recorded");
        }
        resources
            .persistent_container_config_archives
            .insert(container_id.to_owned(), fingerprint.to_owned());
        Ok(())
    }

    /// Reject a config write before Docker applies it to the shared state
    /// volume. The response observer still records success only after Docker
    /// returns a framed 2xx, but a post-write check cannot undo a wrong-ID or
    /// wrong-mode extraction.
    fn authorize_persistent_config_archive(
        &self,
        builder: &str,
        container_id: &str,
        generation: u64,
        fingerprint: &str,
    ) -> Result<()> {
        let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
            .context("resolve persistent BuildKit archive domain")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources.persistent_builders.contains(builder) {
            bail!("persistent BuildKit archive is outside active builder setup");
        }
        let expected = resources
            .persistent_builder_config_fingerprints
            .get(builder)
            .context("persistent BuildKit config mode was not registered")?;
        if expected != fingerprint {
            bail!("persistent BuildKit archive does not match expected config mode and bytes");
        }
        if !resources
            .persistent_container_fresh_ids
            .contains(container_id)
        {
            bail!("persistent BuildKit archive target is not a fresh attested container");
        }
        if !resources.persistent_containers.iter().any(|(name, id)| {
            id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
        }) {
            bail!("persistent BuildKit archive target is not bound to this builder");
        }
        if crate::buildkit::persistent_builder_domain_token(builder) != Some(domain.token.as_str())
            || !resources
                .persistent_builder_creator_leases
                .get(builder)
                .is_some_and(|lease| lease.matches(&domain, builder, expected, generation))
        {
            bail!("persistent BuildKit archive has no matching live creator lease");
        }
        Ok(())
    }

    fn note_fresh_persistent_container(
        &self,
        builder: &str,
        generation: u64,
        container_id: &str,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        if !resources.persistent_builders.contains(builder)
            || !resources.persistent_containers.iter().any(|(name, id)| {
                id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
            })
        {
            bail!("fresh persistent BuildKit container is not bound to its active builder");
        }
        resources
            .persistent_container_fresh_ids
            .insert(container_id.to_owned());
        Ok(())
    }

    fn note_persistent_ready_container(
        &self,
        builder: &str,
        generation: u64,
        container_id: &str,
        config_fingerprint: &str,
        readiness_epoch: u64,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        let expected = resources
            .persistent_builder_config_fingerprints
            .get(builder)
            .context("persistent BuildKit config mode was not registered")?;
        if expected != config_fingerprint
            || !resources.persistent_builders.contains(builder)
            || resources
                .persistent_builder_readiness_epochs
                .get(builder)
                .is_some_and(|current| *current > readiness_epoch)
            || !resources.persistent_containers.iter().any(|(name, id)| {
                id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
            })
        {
            bail!("persistent BuildKit readiness does not match the active builder");
        }
        resources
            .persistent_container_ready_fingerprints
            .insert(container_id.to_owned(), config_fingerprint.to_owned());
        resources
            .persistent_container_readiness_epochs
            .insert(container_id.to_owned(), readiness_epoch);
        resources
            .persistent_builder_readiness_epochs
            .insert(builder.to_owned(), readiness_epoch);
        resources
            .persistent_container_fresh_ids
            .remove(container_id);
        resources
            .persistent_container_config_archives
            .remove(container_id);
        // Durable readiness has already been recorded by the start/recovery
        // path. Release the process-shared creator lock only after that proof
        // exists, allowing another lease to reuse the same builder.
        resources.persistent_builder_creator_leases.remove(builder);
        Ok(())
    }

    fn note_persistent_ready_container_during_setup(
        &self,
        builder: &str,
        generation: u64,
        container_id: &str,
        config_fingerprint: &str,
        readiness_epoch: u64,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources.persistent_builder_generations.get(builder) != Some(&generation)
            || !resources.persistent_builder_setups.contains(builder)
            || resources
                .persistent_builder_config_fingerprints
                .get(builder)
                .map(String::as_str)
                != Some(config_fingerprint)
            || resources
                .persistent_builder_readiness_epochs
                .get(builder)
                .is_some_and(|current| *current > readiness_epoch)
            || !resources.persistent_containers.iter().any(|(name, id)| {
                id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
            })
        {
            bail!("recovered BuildKit readiness changed before setup registration");
        }
        resources
            .persistent_container_ready_fingerprints
            .insert(container_id.to_owned(), config_fingerprint.to_owned());
        resources
            .persistent_container_readiness_epochs
            .insert(container_id.to_owned(), readiness_epoch);
        resources
            .persistent_builder_readiness_epochs
            .insert(builder.to_owned(), readiness_epoch);
        resources
            .persistent_container_fresh_ids
            .remove(container_id);
        resources
            .persistent_container_config_archives
            .remove(container_id);
        Ok(())
    }

    fn note_persistent_start_epoch(
        &self,
        builder: &str,
        generation: u64,
        container_id: &str,
        expected_epoch: u64,
        new_epoch: u64,
    ) -> Result<()> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        let current = resources
            .persistent_builder_readiness_epochs
            .get(builder)
            .copied()
            .unwrap_or_default();
        if current != expected_epoch
            || expected_epoch.checked_add(1) != Some(new_epoch)
            || !resources.persistent_containers.iter().any(|(name, id)| {
                id == container_id && persistent_buildkit_builder_name(name) == Some(builder)
            })
        {
            bail!("persistent BuildKit start epoch changed before capability invalidation");
        }
        resources
            .persistent_builder_readiness_epochs
            .insert(builder.to_owned(), new_epoch);
        resources
            .persistent_container_ready_fingerprints
            .remove(container_id);
        resources
            .persistent_container_readiness_epochs
            .remove(container_id);
        remove_persistent_execs_for_container(&mut resources, container_id);
        Ok(())
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
        Ok(self.authorize_admitted(request)?.route)
    }

    fn authorize_admitted(&self, request: &[u8]) -> Result<AuthorizedDockerRequest> {
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
            return self.authorize_active_builder_route(
                AuthorizedDockerRoute::PersistentImagePull,
                upgrade,
            );
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
            return self.authorize_active_builder_route(
                AuthorizedDockerRoute::PersistentImageInspect,
                upgrade,
            );
        }
        if segments.as_slice() == ["containers", "create"] && method == "POST" {
            let create_name =
                containers_create_query_name(request).map_err(create_capability_deny)?;
            if let Some(name) = create_name.as_deref()
                && self.is_allowed_persistent_buildkit_container_name(name)?
            {
                self.validate_persistent_container_create_request(request, name)
                    .map_err(create_capability_deny)?;
                let builder = persistent_buildkit_builder_name(name)
                    .context("persistent BuildKit container name has no builder")?;
                return self.authorize_builder_route(
                    AuthorizedDockerRoute::PersistentBootstrap,
                    upgrade,
                    builder,
                );
            }
            if create_name
                .as_deref()
                .is_some_and(is_reserved_persistent_buildkit_container_name)
            {
                return Err(LeaseDeny::forbidden(
                    "Docker lease denies creation of an unclaimed persistent BuildKit container",
                ));
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
                if method == "DELETE" {
                    return self.authorize_owned_container_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                        id,
                    );
                }
                if matches!(method.as_str(), "GET" | "HEAD") {
                    return self.authorize_owned_container_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                        id,
                    );
                }
            }
            ["containers", id, operation] => {
                if operation == &"json"
                    && method == "GET"
                    && (self.is_allowed_persistent_buildkit_container_name(id)?
                        || self.is_attested_persistent_container(id)?)
                {
                    return self.authorize_persistent_container_route(
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container),
                        upgrade,
                        id,
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
                    if method == "POST"
                        && operation == &"start"
                        && !self.persistent_container_can_start(id)?
                    {
                        return Err(LeaseDeny::forbidden(
                            "Docker lease denies persistent BuildKit start before exact config archive and readiness proof",
                        ));
                    }
                    return self.authorize_persistent_container_route(
                        AuthorizedDockerRoute::Persistent(DockerResourceKind::Container),
                        upgrade,
                        id,
                    );
                }
                if operation == &"archive"
                    && method == "PUT"
                    && self.is_attested_persistent_container(id)?
                {
                    validate_persistent_archive_request(request, target)
                        .map_err(create_capability_deny)?;
                    return self.authorize_persistent_container_route(
                        AuthorizedDockerRoute::PersistentArchive,
                        upgrade,
                        id,
                    );
                }
                if operation == &"exec" && method == "POST" {
                    if self.is_attested_persistent_container(id)? {
                        self.validate_persistent_exec_create_request(request)?;
                        return self.authorize_persistent_container_route(
                            AuthorizedDockerRoute::PersistentExecCreate,
                            upgrade,
                            id,
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
                if operation == &"exec" && method == "POST" {
                    return self.authorize_owned_container_route(
                        AuthorizedDockerRoute::Create(DockerResourceKind::Exec),
                        upgrade,
                        id,
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
                    return self.authorize_owned_container_route(route, upgrade, id);
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
                        return self.authorize_persistent_exec_route(
                            AuthorizedDockerRoute::PersistentExec,
                            upgrade,
                            id,
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
                if matches!(*operation, "connect" | "disconnect") && method == "POST" {
                    return self.authorize_network_container_route(upgrade, id, request);
                }
                self.require_owned(DockerResourceKind::Network, id)?;
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

    /// Acquire ordinary job-volume locks in lexical order. Persistent
    /// BuildKit state volumes must use the domain-aware API below. The guards
    /// are held through Docker's reply and ownership observation, closing the
    /// inspect→operation replacement window.
    pub(crate) fn lock_volume_names(
        &self,
        names: &BTreeSet<String>,
    ) -> Result<VolumeOperationLocks> {
        self.lock_volume_names_with_create_access(names, None, None)
    }

    /// Acquire volume locks while checking the selected BuildKit domain's
    /// durable pending-create journal. Persistent state-volume names require
    /// a domain; only exact access for that journal may bypass its fence.
    pub(crate) fn lock_volume_names_with_create_access(
        &self,
        names: &BTreeSet<String>,
        domain: Option<&crate::buildkit::PersistentBuildKitDomain>,
        access: Option<&crate::buildkit::PendingBuildKitCreateAccess>,
    ) -> Result<VolumeOperationLocks> {
        if domain.is_none()
            && names
                .iter()
                .any(|name| is_persistent_buildkit_volume_object(name))
        {
            bail!("persistent BuildKit volume locks require a resolved domain");
        }
        if let Some(domain) = domain {
            for name in names
                .iter()
                .filter(|name| is_persistent_buildkit_volume_object(name))
            {
                let builder = persistent_buildkit_volume_builder_name(name)
                    .context("persistent BuildKit volume is not an exact node-zero state volume")?;
                if persistent_buildkit_domain_token(builder) != Some(domain.token.as_str()) {
                    bail!(
                        "persistent BuildKit volume {name:?} does not belong to the resolved domain"
                    );
                }
            }
        }
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
            #[cfg(unix)]
            let file = self
                .volume_lock_root
                .as_deref()
                .map(|root| acquire_volume_file_lock(root, &name))
                .transpose();
            #[cfg(not(unix))]
            let file: Result<Option<File>> = Ok(None);
            match file {
                Ok(file) => {
                    guard.file = file;
                    #[cfg(unix)]
                    if let Some(root) = self.volume_lock_root.as_deref() {
                        let marker_result = if let Some(domain) = domain {
                            migrate_legacy_pending_buildkit_create(root, domain, &name).and_then(
                                |_| {
                                    ensure_domain_pending_buildkit_create_clear(
                                        domain, &name, access,
                                    )
                                },
                            )
                        } else {
                            ensure_no_pending_buildkit_create(root, &name)
                        };
                        if let Err(error) = marker_result {
                            drop(guard);
                            return Err(error);
                        }
                    }
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
            && is_persistent_buildkit_volume_object(&name)
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
        authorized_container_id: Option<&str>,
    ) -> Result<Vec<u8>> {
        let (_, target) = docker_request_line(request)?;
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        if matches!(
            authorization,
            AuthorizedDockerRoute::Owned(DockerResourceKind::Network)
        ) && matches!(
            segments.as_slice(),
            ["networks", _, "connect" | "disconnect"]
        ) {
            if let Some(container_id) = authorized_container_id {
                return rewrite_network_container_reference(request, container_id);
            }
        }
        let container_route = matches!(
            authorization,
            AuthorizedDockerRoute::Owned(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Hijack(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Create(DockerResourceKind::Exec)
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::PersistentArchive
        );
        if !container_route {
            return Ok(request.to_vec());
        }
        let Some(["containers", target_id, ..]) = segments.get(..) else {
            return Ok(request.to_vec());
        };
        let target_id = *target_id;
        let replacement = if let Some(id) = authorized_container_id {
            Some(id.to_owned())
        } else {
            self.persistent_alias_replacement(authorization, target_id)?
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

    fn persistent_alias_replacement(
        &self,
        authorization: AuthorizedDockerRoute,
        target_id: &str,
    ) -> Result<Option<String>> {
        let replacement = {
            let resources = self
                .resources
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            match authorization {
                AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::PersistentArchive => {
                    if persistent_buildkit_builder_name(target_id).is_some()
                        && !resources.persistent_containers.contains_key(target_id)
                    {
                        None
                    } else if let Some(id) = resources.persistent_containers.get(target_id) {
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
        Ok(replacement)
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

    fn persistent_container_id(&self, target: &str) -> Result<String> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources
            .persistent_containers
            .iter()
            .find_map(|(name, id)| (name == &target || id == &target).then_some(id.clone()))
            .context("persistent BuildKit container has no attested immutable ID")
    }

    fn forget_persistent_container(&self, target: &str) -> Result<()> {
        self.forget_persistent_container_fenced(target, None)
    }

    fn forget_persistent_container_fenced(
        &self,
        target: &str,
        request_fence: Option<(&str, u64)>,
    ) -> Result<()> {
        let target = validate_owned_resource_id(target, "Docker resource")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if let Some((builder, generation)) = request_fence {
            ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
        }
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
            resources.persistent_container_fresh_ids.remove(&id);
            resources.persistent_container_config_archives.remove(&id);
            resources
                .persistent_container_ready_fingerprints
                .remove(&id);
            resources.persistent_container_readiness_epochs.remove(&id);
            remove_persistent_execs_for_container(&mut resources, &id);
        }
        for name in removed_names {
            resources.persistent_container_volumes.remove(&name);
        }
        Ok(())
    }

    fn note_persistent_container_candidate(
        &self,
        status: u16,
        body: &[u8],
        request_fence: Option<(&str, u64)>,
    ) -> Result<String> {
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
        if let Some((builder, generation)) = request_fence {
            ensure_persistent_builder_generation_locked(&resources, builder, generation)?;
            if !resources.persistent_builders.contains(builder) {
                bail!("persistent BuildKit create response arrived outside active setup");
            }
        }
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
    ) -> Result<()> {
        self.record_persistent_container_inspect_fenced(target, status, body, None)
    }

    fn record_persistent_container_inspect_fenced(
        &self,
        target: &str,
        status: u16,
        body: &[u8],
        request_fence: Option<(&str, u64)>,
    ) -> Result<()> {
        if status == 404 {
            return self.forget_persistent_container_fenced(target, request_fence);
        }
        if !(200..300).contains(&status) {
            // A non-404 response is an inconclusive identity result. Do not
            // treat it as an absent container and leave a stale capability in
            // the registry: fail closed until the next full attestation.
            self.forget_persistent_container_fenced(target, request_fence)?;
            bail!(
                "persistent BuildKit container inspect returned inconclusive HTTP status {status}"
            );
        }
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        let (name, id, volume, _image_id, has_config_flag) =
            match attest_persistent_buildkit_container(
                body,
                target,
                &allowed_builders,
                &self.persistent_builder_images()?,
            ) {
                Ok(attested) => attested,
                Err(error) => {
                    self.forget_persistent_container_fenced(target, request_fence)?;
                    return Err(error);
                }
            };
        let builder = persistent_buildkit_builder_name(&name)
            .context("attested persistent BuildKit container has no builder")?;
        let expected_config = self.persistent_builder_config_fingerprint(builder)?;
        if (expected_config == "no-config-v1") == has_config_flag {
            self.forget_persistent_container_fenced(target, request_fence)?;
            bail!("persistent BuildKit container config mode does not match runner setup");
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if let Some((expected_builder, generation)) = request_fence {
            if expected_builder != builder {
                bail!("persistent container response belongs to another builder");
            }
            let setup_recovery = resources
                .persistent_builder_setups
                .contains(expected_builder)
                && resources
                    .persistent_builder_recovery_generations
                    .get(expected_builder)
                    == Some(&generation);
            if resources
                .persistent_builder_generations
                .get(expected_builder)
                != Some(&generation)
                || (!resources.persistent_builders.contains(expected_builder) && !setup_recovery)
            {
                bail!(
                    "persistent BuildKit container response lost its setup or request generation"
                );
            }
        }
        let replaced_ids = resources
            .persistent_containers
            .iter()
            .filter(|(existing_name, existing_id)| existing_name == &&name && existing_id != &&id)
            .map(|(_, existing_id)| existing_id.clone())
            .collect::<BTreeSet<_>>();
        for old_id in replaced_ids {
            resources.persistent_container_candidates.remove(&old_id);
            resources.persistent_container_fresh_ids.remove(&old_id);
            resources
                .persistent_container_config_archives
                .remove(&old_id);
            resources
                .persistent_container_ready_fingerprints
                .remove(&old_id);
            resources
                .persistent_container_readiness_epochs
                .remove(&old_id);
            remove_persistent_execs_for_container(&mut resources, &old_id);
        }
        // Re-inspection is a new readiness decision even when Docker returns
        // the same immutable ID. Invalidate cached authority before replacing
        // the binding; the host response observer restores it only after the
        // durable readiness record passes again.
        resources
            .persistent_container_ready_fingerprints
            .remove(&id);
        resources.persistent_container_readiness_epochs.remove(&id);
        remove_persistent_execs_for_container(&mut resources, &id);
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
    ) -> Result<()> {
        if status == 404 {
            return self.forget_volume(target);
        }
        if !(200..300).contains(&status) {
            self.forget_volume(target)?;
            bail!("persistent BuildKit volume inspect returned inconclusive HTTP status {status}");
        }
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        let Some(domain_token) = persistent_buildkit_domain_token_for_volume(target) else {
            self.forget_volume(target)?;
            bail!("persistent BuildKit state volume has no current domain token");
        };
        if let Err(error) =
            attest_persistent_buildkit_volume(body, target, domain_token, &allowed_builders)
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
        domain_token: &str,
    ) -> Result<()> {
        let allowed_builders = self.persistent_builder_names_for_attestation()?;
        let output = std::str::from_utf8(output)
            .context("Docker persistent BuildKit volume projection must be UTF-8")?;
        if let Err(error) = attest_persistent_buildkit_volume_projection(
            output,
            target,
            domain_token,
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

    fn note_persistent_exec(
        &self,
        status: u16,
        body: &[u8],
        builder: &str,
        container_id: &str,
        generation: u64,
        readiness_epoch: u64,
    ) -> Result<()> {
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
        ensure_persistent_builder_generation_locked(&resources, &builder, generation)?;
        if resources.persistent_builder_readiness_epochs.get(&builder) != Some(&readiness_epoch) {
            bail!("persistent BuildKit exec response belongs to a stale readiness epoch");
        }
        if !resources.persistent_builders.contains(&builder)
            || !resources.persistent_containers.iter().any(|(name, id)| {
                id == container_id
                    && persistent_buildkit_builder_name(name) == Some(builder.as_str())
            })
        {
            bail!("persistent BuildKit exec response arrived after its container was revoked");
        }
        require_persistent_container_ready_locked(&resources, &builder, container_id)?;
        if !resources.execs.contains(&identifier)
            && owned_resource_count(&resources) >= MAX_OWNED_DOCKER_RESOURCES
        {
            bail!("Docker lease ownership registry is full");
        }
        resources.execs.insert(identifier.clone());
        resources.persistent_execs.insert(identifier.clone());
        resources
            .persistent_exec_builders
            .insert(identifier.clone(), builder);
        resources
            .persistent_exec_containers
            .insert(identifier.clone(), container_id.to_owned());
        resources
            .persistent_exec_generations
            .insert(identifier.clone(), generation);
        resources
            .persistent_exec_readiness_epochs
            .insert(identifier, readiness_epoch);
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
                let domain_token = persistent_buildkit_domain_token_for_volume(&returned_name)
                    .context("persistent BuildKit volume has no current domain token")?;
                if let Err(error) = attest_persistent_buildkit_volume(
                    body,
                    &returned_name,
                    domain_token,
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
        self.record_delete_response_fenced(kind, target, status, None)
    }

    fn record_delete_response_fenced(
        &self,
        kind: DockerResourceKind,
        target: &str,
        status: u16,
        expected_container_id: Option<&str>,
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
                if let Some(expected_id) = expected_container_id {
                    let expected_id =
                        validate_owned_resource_id(expected_id, "attested Docker container")?;
                    if resources.container_names.get(&target) == Some(&expected_id) {
                        resources.container_names.remove(&target);
                    }
                    resources.containers.remove(&expected_id);
                    resources
                        .container_names
                        .retain(|_, owner_id| owner_id != &expected_id);
                } else {
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
) -> Result<AuthorizedDockerRequest> {
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
    Ok(AuthorizedDockerRequest::plain(route))
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

fn rewrite_network_container_reference(request: &[u8], container_id: &str) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker network request is missing header terminator")?;
    let header_text = std::str::from_utf8(&request[..header_end])
        .context("Docker network request headers must be UTF-8")?;
    let body = &request[header_end..];
    let mut object = parse_create_value(body)
        .context("parse Docker network container request")?
        .as_object()
        .cloned()
        .context("Docker network container request must be an object")?;
    reject_case_insensitive_duplicate_keys(&object, "Docker network container request")?;
    let key = object
        .keys()
        .find(|key| key.eq_ignore_ascii_case("Container"))
        .cloned()
        .context("Docker network request must name a container")?;
    object.insert(key, Value::String(container_id.to_owned()));
    let body = serde_json::to_vec(&object).context("serialize Docker network container request")?;

    let mut normalized = Vec::with_capacity(header_text.len() + body.len());
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().context("Docker network request line")?;
    normalized.extend_from_slice(request_line.as_bytes());
    normalized.extend_from_slice(b"\r\n");
    let mut content_length_seen = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed Docker network request header");
        };
        if name.eq_ignore_ascii_case("content-length") {
            if content_length_seen {
                bail!("Docker network request repeats Content-Length");
            }
            let declared = value
                .trim()
                .parse::<usize>()
                .context("parse Docker network request Content-Length")?;
            if declared != body.len() {
                bail!("Docker network request Content-Length does not match body");
            }
            content_length_seen = true;
            continue;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("Docker network request uses unsupported Transfer-Encoding");
        }
        normalized.extend_from_slice(line.as_bytes());
        normalized.extend_from_slice(b"\r\n");
    }
    if !content_length_seen {
        bail!("Docker network request omitted bounded Content-Length");
    }
    normalized.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    normalized.extend_from_slice(&body);
    Ok(normalized)
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

fn inject_persistent_buildkit_labels_value(
    value: &mut Value,
    job_id: &str,
    domain_token: &str,
) -> Result<()> {
    if job_id.trim().is_empty() {
        bail!("persistent BuildKit container requires a nonempty creator job ID");
    }
    if domain_token.trim().is_empty() {
        bail!("persistent BuildKit container requires a nonempty domain token");
    }
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
    if let Some(key) = label_keys.first() {
        object.remove(key);
    }
    object.insert(
        "Labels".into(),
        Value::Object(Map::from_iter([
            (JOB_ID_LABEL.into(), Value::String(job_id.to_owned())),
            (
                BUILDKIT_DOMAIN_LABEL.into(),
                Value::String(domain_token.to_owned()),
            ),
        ])),
    );
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
                &persistent_bootstrap_domain_token(request)?,
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
// Buildx v0.36.1 uses Pelletier TOML v2.3.1 to normalize this exact approved
// config before archiving it. Readiness fingerprints bind these archived
// payload bytes, not the user's equivalent TOML spelling.
const APPROVED_BUILDKIT_CONFIG_ARCHIVE: &[u8] =
    b"[registry]\n[registry.'docker.io']\nmirrors = ['mirror.gcr.io']\n";

#[cfg(unix)]
fn approved_buildkit_recovery_archive(config_fingerprint: &str) -> Result<Vec<u8>> {
    if config_fingerprint == "no-config-v1" {
        return Ok(vec![0; 1024]);
    }
    let digest = Sha256::digest(APPROVED_BUILDKIT_CONFIG_ARCHIVE);
    let mut expected = String::with_capacity(71);
    expected.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut expected, "{byte:02x}")?;
    }
    if config_fingerprint != expected {
        bail!("refuse recovery archive for an unapproved BuildKit config fingerprint");
    }

    let mut archive = tar::Builder::new(Vec::new());
    let mut directory = tar::Header::new_gnu();
    directory.set_path("buildkit/")?;
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_mode(0o755);
    directory.set_uid(0);
    directory.set_gid(0);
    directory.set_mtime(0);
    directory.set_size(0);
    directory.set_cksum();
    archive.append(&directory, std::io::Cursor::new([]))?;

    let mut file = tar::Header::new_gnu();
    file.set_path("buildkit/buildkitd.toml")?;
    file.set_entry_type(tar::EntryType::Regular);
    file.set_mode(0o644);
    file.set_uid(0);
    file.set_gid(0);
    file.set_mtime(0);
    file.set_size(APPROVED_BUILDKIT_CONFIG_ARCHIVE.len() as u64);
    file.set_cksum();
    archive.append(
        &file,
        std::io::Cursor::new(APPROVED_BUILDKIT_CONFIG_ARCHIVE),
    )?;
    let bytes = archive.into_inner()?;
    let archived_fingerprint = validate_persistent_buildkit_tar(&bytes)?;
    if archived_fingerprint != config_fingerprint {
        bail!("generated BuildKit recovery archive fingerprint changed");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn upload_approved_buildkit_archive_on_host(
    host_socket: &Path,
    container_id: &str,
    config_fingerprint: &str,
) -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let container_id = validate_owned_resource_id(container_id, "BuildKit container ID")?;
    let archive = approved_buildkit_recovery_archive(config_fingerprint)?;
    let encoded_id = container_id
        .bytes()
        .fold(String::new(), |mut encoded, byte| {
            if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
            encoded
        });
    let mut stream = UnixStream::connect(host_socket).with_context(|| {
        format!(
            "connect Docker for BuildKit recovery archive {}",
            host_socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure BuildKit recovery archive read timeout")?;
    stream
        .set_write_timeout(Some(PROXY_IDLE_TIMEOUT))
        .context("configure BuildKit recovery archive write timeout")?;
    let header = format!(
        "PUT /v1.43/containers/{encoded_id}/archive?path=%2Fetc&noOverwriteDirNonDir=true HTTP/1.1\r\nHost: docker\r\nContent-Type: application/x-tar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        archive.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(&archive))
        .context("send approved BuildKit recovery archive")?;

    let mut response = Vec::new();
    let mut scratch = [0_u8; 4096];
    let header_end = loop {
        if let Some(index) = response.windows(4).position(|window| window == b"\r\n\r\n") {
            let end = index + 4;
            if end > MAX_PROXY_HEADER {
                bail!("BuildKit recovery archive response headers exceed the limit");
            }
            break end;
        }
        if response.len() > MAX_PROXY_HEADER {
            bail!("BuildKit recovery archive response headers exceed the limit");
        }
        let read = stream
            .read(&mut scratch)
            .context("read BuildKit recovery archive response")?;
        if read == 0 {
            bail!("Docker closed before BuildKit recovery archive response was framed");
        }
        response.extend_from_slice(&scratch[..read]);
    };
    let header_text = std::str::from_utf8(&response[..header_end])
        .context("BuildKit recovery archive response headers are not UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .context("BuildKit recovery archive response omitted status")?;
    let mut status_parts = status_line.split_ascii_whitespace();
    let version = status_parts.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        bail!("unsupported BuildKit recovery archive response version");
    }
    let status = status_parts
        .next()
        .context("BuildKit recovery archive response omitted status code")?
        .parse::<u16>()
        .context("parse BuildKit recovery archive response status")?;
    let mut content_length = None;
    let mut connection_close = version == "HTTP/1.0";
    let mut connection_keep_alive = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .context("malformed BuildKit recovery archive response header")?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("chunked BuildKit recovery archive responses are unsupported");
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .context("parse BuildKit recovery archive response length")?;
            if content_length.replace(length).is_some() {
                bail!("duplicate BuildKit recovery archive response length");
            }
        }
        if name.eq_ignore_ascii_case("connection") {
            for token in value.split(',').map(str::trim) {
                if token.eq_ignore_ascii_case("close") {
                    connection_close = true;
                } else if token.eq_ignore_ascii_case("keep-alive") {
                    connection_keep_alive = true;
                }
            }
        }
    }
    if !(200..300).contains(&status) {
        bail!("Docker rejected BuildKit recovery archive with HTTP {status}");
    }
    match content_length {
        Some(0) if response.len() == header_end => {}
        None if connection_close && !connection_keep_alive => {
            // Some supported Engine versions close-delimit their empty PUT
            // response. The request asks for Connection: close; accept that
            // frame only after EOF and only when no response body arrived.
            loop {
                let read = stream
                    .read(&mut scratch)
                    .context("finish close-delimited BuildKit recovery response")?;
                if read == 0 {
                    break;
                }
                response.extend_from_slice(&scratch[..read]);
                if response.len() > MAX_PROXY_HEADER || response.len() != header_end {
                    bail!("BuildKit recovery archive returned an unexpected response body");
                }
            }
        }
        _ => bail!("BuildKit recovery archive did not receive a framed empty 2xx response"),
    }
    Ok(())
}

#[cfg(unix)]
fn recover_unbound_created_builder_for_request(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    generation: u64,
    config_fingerprint: &str,
) -> Result<bool> {
    let _recovery = match policy.begin_persistent_builder_recovery(
        domain,
        builder,
        generation,
        config_fingerprint,
    ) {
        Ok(Some(recovery)) => recovery,
        Ok(None) => {
            return crate::buildkit::builder_readiness_matches(
                domain,
                builder,
                container_id,
                config_fingerprint,
            );
        }
        Err(error) => {
            if crate::buildkit::builder_readiness_matches(
                domain,
                builder,
                container_id,
                config_fingerprint,
            )? {
                return Ok(true);
            }
            return Err(error);
        }
    };
    let recovered = recover_pending_buildkit_create_under_gate(
        policy,
        host_socket,
        domain,
        builder,
        generation,
        config_fingerprint,
        _recovery,
    )?;
    let Some(recovered_id) = recovered else {
        return Ok(crate::buildkit::builder_readiness_matches(
            domain,
            builder,
            container_id,
            config_fingerprint,
        )?);
    };
    if recovered_id != container_id {
        bail!("pending BuildKit recovery changed immutable container ID");
    }
    note_current_persistent_readiness(
        policy,
        domain,
        builder,
        generation,
        &recovered_id,
        config_fingerprint,
    )?;
    Ok(true)
}

#[cfg(unix)]
fn recover_pending_buildkit_create_for_request(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
) -> Result<Option<String>> {
    if !has_recoverable_pending_buildkit_create(domain, builder)? {
        return Ok(None);
    }
    policy.ensure_pending_create_creator_lease(domain, builder, config_fingerprint, generation)?;
    let Some(recovery) =
        policy.begin_pending_create_recovery(domain, builder, generation, config_fingerprint)?
    else {
        bail!("another active Docker request prevents pending BuildKit recovery");
    };
    recover_pending_buildkit_create_under_gate(
        policy,
        host_socket,
        domain,
        builder,
        generation,
        config_fingerprint,
        recovery,
    )
}

#[cfg(unix)]
fn recover_pending_buildkit_create_for_setup_with_volume_lock(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    engine_id: &str,
    lock_volume: impl FnMut(
        &crate::buildkit::PersistentBuildKitDomain,
        &str,
        &crate::buildkit::PendingBuildKitCreateAccess,
    ) -> Result<VolumeOperationLocks>,
) -> Result<Option<String>> {
    let mut transport = HostPendingBuildKitRecoveryTransport { host_socket };
    recover_pending_buildkit_create_for_setup_with_volume_lock_and_transport(
        policy,
        domain,
        builder,
        generation,
        config_fingerprint,
        engine_id,
        lock_volume,
        &mut transport,
    )
}

#[cfg(unix)]
fn recover_pending_buildkit_create_for_setup_with_volume_lock_and_transport(
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    engine_id: &str,
    lock_volume: impl FnMut(
        &crate::buildkit::PersistentBuildKitDomain,
        &str,
        &crate::buildkit::PendingBuildKitCreateAccess,
    ) -> Result<VolumeOperationLocks>,
    transport: &mut impl PendingBuildKitRecoveryTransport,
) -> Result<Option<String>> {
    if !has_recoverable_pending_buildkit_create(domain, builder)? {
        return Ok(None);
    }
    if engine_id != domain.engine_id {
        bail!("pending BuildKit setup recovery resolved another Docker Engine");
    }
    policy.ensure_pending_create_creator_lease(domain, builder, config_fingerprint, generation)?;
    let Some(recovery) = policy.begin_pending_create_setup_recovery(
        domain,
        builder,
        generation,
        config_fingerprint,
    )?
    else {
        bail!("another lifecycle operation blocks pending BuildKit setup recovery");
    };
    let container_id = recover_pending_buildkit_create_under_gate_with_volume_lock(
        policy,
        domain,
        builder,
        generation,
        config_fingerprint,
        recovery,
        lock_volume,
        transport,
    )?
    .context("pending BuildKit setup recovery did not settle its transaction")?;
    let readiness_epoch = crate::buildkit::builder_readiness_epoch(domain, builder)?;
    if !crate::buildkit::builder_readiness_matches_epoch(
        domain,
        builder,
        &container_id,
        config_fingerprint,
        readiness_epoch,
    )? {
        bail!("pending BuildKit setup recovery lacks a Ready proof for its immutable ID");
    }
    policy.note_persistent_ready_container_during_setup(
        builder,
        generation,
        &container_id,
        config_fingerprint,
        readiness_epoch,
    )?;
    Ok(Some(container_id))
}

fn has_recoverable_pending_buildkit_create(
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
) -> Result<bool> {
    if crate::buildkit::pending_buildkit_create_transaction(domain, builder)?.is_some() {
        return Ok(true);
    }
    let volume = crate::buildkit::daemon_state_volume(builder);
    if crate::buildkit::legacy_pending_buildkit_create_is_quarantined(domain, &volume)? {
        bail!("legacy persistent BuildKit create remains quarantined for operator repair");
    }
    Ok(false)
}

#[cfg(unix)]
fn recover_pending_buildkit_create_under_gate(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    recovery: PersistentBuilderRecoveryAdmission,
) -> Result<Option<String>> {
    let mut transport = HostPendingBuildKitRecoveryTransport { host_socket };
    recover_pending_buildkit_create_under_gate_with_volume_lock(
        policy,
        domain,
        builder,
        generation,
        config_fingerprint,
        recovery,
        |domain, volume, access| lock_host_volume_name_for_pending_create(domain, volume, access),
        &mut transport,
    )
}

#[cfg(unix)]
trait PendingBuildKitRecoveryTransport {
    fn inspect_volume(&mut self, target: &str) -> Result<(u16, Vec<u8>)>;
    fn inspect_container(&mut self, target: &str) -> Result<(u16, Vec<u8>)>;
    fn upload_archive(&mut self, container_id: &str, config_fingerprint: &str) -> Result<()>;
    fn start_container(&mut self, container_id: &str) -> Result<()>;
    fn publish_readiness(
        &mut self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        container_id: &str,
        config_fingerprint: &str,
        readiness_epoch: u64,
    ) -> Result<()>;
}

#[cfg(unix)]
struct HostPendingBuildKitRecoveryTransport<'a> {
    host_socket: &'a Path,
}

#[cfg(unix)]
impl PendingBuildKitRecoveryTransport for HostPendingBuildKitRecoveryTransport<'_> {
    fn inspect_volume(&mut self, target: &str) -> Result<(u16, Vec<u8>)> {
        inspect_volume_on_host(self.host_socket, target)
    }

    fn inspect_container(&mut self, target: &str) -> Result<(u16, Vec<u8>)> {
        inspect_container_on_host(self.host_socket, target)
    }

    fn upload_archive(&mut self, container_id: &str, config_fingerprint: &str) -> Result<()> {
        upload_approved_buildkit_archive_on_host(self.host_socket, container_id, config_fingerprint)
    }

    fn start_container(&mut self, container_id: &str) -> Result<()> {
        crate::docker::Docker::host()
            .container_start(container_id)
            .map(|_| ())
    }

    fn publish_readiness(
        &mut self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        container_id: &str,
        config_fingerprint: &str,
        readiness_epoch: u64,
    ) -> Result<()> {
        crate::buildkit::persist_builder_readiness_after_start(
            domain,
            builder,
            container_id,
            config_fingerprint,
            readiness_epoch,
        )
    }
}

#[cfg(unix)]
fn recover_pending_buildkit_create_under_gate_with_volume_lock(
    policy: &DockerLeasePolicy,
    domain: &crate::buildkit::PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    _recovery: PersistentBuilderRecoveryAdmission,
    mut lock_volume: impl FnMut(
        &crate::buildkit::PersistentBuildKitDomain,
        &str,
        &crate::buildkit::PendingBuildKitCreateAccess,
    ) -> Result<VolumeOperationLocks>,
    transport: &mut impl PendingBuildKitRecoveryTransport,
) -> Result<Option<String>> {
    let Some(mut transaction) =
        crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
    else {
        return Ok(None);
    };
    if transaction.config_fingerprint != config_fingerprint
        || transaction.state_volume != crate::buildkit::daemon_state_volume(builder)
        || transaction.container_name != crate::buildkit::daemon_container_name(builder)
        || crate::buildkit::persistent_builder_domain_token(builder) != Some(domain.token.as_str())
    {
        bail!("pending BuildKit create does not match this builder domain or config");
    }
    // The transaction may have completed while this request waited on the
    // process-shared creator flock. Re-read it only after taking the local
    // recovery gate, then require the exact same durable transaction identity.
    let Some(current) = crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
    else {
        return Ok(None);
    };
    if current.transaction_id != transaction.transaction_id {
        bail!("pending BuildKit create changed while entering recovery");
    }
    transaction = current;
    let access = crate::buildkit::pending_buildkit_create_access(
        domain,
        builder,
        config_fingerprint,
        generation,
    )?
    .context("pending BuildKit recovery lacks its exact lock bypass")?;
    let volume_lock = lock_volume(domain, &transaction.state_volume, &access)?;
    let (volume_status, volume_body) = transport
        .inspect_volume(&transaction.state_volume)
        .context("inspect pending BuildKit state volume before recovery")?;
    if !(200..300).contains(&volume_status) {
        bail!("pending BuildKit state volume inspect returned HTTP {volume_status}");
    }
    policy.record_persistent_volume_inspect(
        &transaction.state_volume,
        volume_status,
        &volume_body,
    )?;

    let initial_target = transaction
        .container_id
        .as_deref()
        .unwrap_or(&transaction.container_name);
    let (status, body) = transport
        .inspect_container(initial_target)
        .context("inspect pending BuildKit container before recovery")?;
    if !(200..300).contains(&status) {
        bail!(
            "pending BuildKit container inspect returned HTTP {status}; transaction remains fenced"
        );
    }
    let (container_id, shape, state) = attest_pending_buildkit_create_inspect(&body, &transaction)?;
    if let Some(expected_shape) = transaction.attested_shape_sha256.as_deref()
        && expected_shape != shape
    {
        bail!("pending BuildKit container full shape differs from its durable attestation");
    }
    crate::buildkit::bind_pending_buildkit_create_container(
        domain,
        builder,
        &transaction.transaction_id,
        &container_id,
        &shape,
    )?;
    crate::buildkit::bind_persistent_builder_creator_container(
        domain,
        builder,
        generation,
        config_fingerprint,
        &container_id,
    )?;
    policy.record_persistent_container_inspect_fenced(
        &transaction.container_name,
        status,
        &body,
        Some((builder, generation)),
    )?;
    transaction = crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
        .context("pending BuildKit transaction disappeared after container binding")?;

    if crate::buildkit::builder_readiness_matches(
        domain,
        builder,
        &container_id,
        config_fingerprint,
    )? {
        if state != "running" {
            bail!("durable BuildKit readiness names a non-running container");
        }
        crate::buildkit::mark_pending_buildkit_create_existing_ready(
            domain,
            builder,
            &transaction.transaction_id,
            &container_id,
            &state,
        )?;
        crate::buildkit::finish_pending_buildkit_create_transaction(
            domain,
            builder,
            &transaction.transaction_id,
            &container_id,
            config_fingerprint,
        )?;
        drop(volume_lock);
        return Ok(Some(container_id));
    }

    if state == "running"
        && transaction.archived_config_fingerprint.as_deref() != Some(config_fingerprint)
    {
        bail!("running pending BuildKit container has no durable matching archive proof");
    }
    let readiness_epoch = if state == "created" {
        // Reapply the exact approved payload after every recovery cut. The
        // journal's prior archive response may have reached durable storage
        // while the Engine archive was incomplete or its container remained
        // Created; extraction of this one fixed file is idempotent.
        transport
            .upload_archive(&container_id, config_fingerprint)
            .context("reapply the pending BuildKit transaction's approved config archive")?;
        crate::buildkit::record_persistent_builder_creator_archive(
            domain,
            builder,
            generation,
            config_fingerprint,
            &container_id,
            config_fingerprint,
        )?;
        crate::buildkit::record_pending_buildkit_create_archive(
            domain,
            builder,
            &transaction.transaction_id,
            &container_id,
            config_fingerprint,
        )?;
        transaction = crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
            .context("pending BuildKit transaction disappeared after archive proof")?;

        let (status, body) = transport
            .inspect_container(&container_id)
            .context("re-inspect pending BuildKit container after archive")?;
        if !(200..300).contains(&status) {
            bail!("pending BuildKit container re-inspect returned HTTP {status}");
        }
        let (rechecked_id, rechecked_shape, rechecked_state) =
            attest_pending_buildkit_create_inspect(&body, &transaction)?;
        if rechecked_id != container_id || rechecked_shape != shape || rechecked_state != "created"
        {
            bail!("pending BuildKit container changed after config archive");
        }
        let current_epoch = crate::buildkit::builder_readiness_epoch(domain, builder)?;
        let epoch = crate::buildkit::invalidate_builder_readiness_before_start(
            domain,
            builder,
            &container_id,
            config_fingerprint,
            current_epoch,
        )?;
        transport
            .start_container(&container_id)
            .context("start the attested pending BuildKit container")?;
        crate::buildkit::mark_pending_buildkit_create_started(
            domain,
            builder,
            &transaction.transaction_id,
            &container_id,
        )?;
        epoch
    } else if state == "running" {
        let current_epoch = crate::buildkit::builder_readiness_epoch(domain, builder)?;
        crate::buildkit::invalidate_builder_readiness_before_start(
            domain,
            builder,
            &container_id,
            config_fingerprint,
            current_epoch,
        )?
    } else {
        bail!("pending BuildKit container is {state}; recovery remains fenced");
    };

    drop(volume_lock);
    transport
        .publish_readiness(
            domain,
            builder,
            &container_id,
            config_fingerprint,
            readiness_epoch,
        )
        .context("publish readiness after pending BuildKit create recovery")?;
    Ok(Some(container_id))
}

fn validate_persistent_archive_request(request: &[u8], target: &str) -> Result<String> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("persistent BuildKit archive request omitted its header terminator")?;
    for line in request[..header_end].split(|byte| *byte == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        let (name, value) = line.split_at(separator);
        let value = &value[1..];
        if name.eq_ignore_ascii_case(b"content-encoding")
            && !value.iter().all(u8::is_ascii_whitespace)
        {
            bail!("persistent BuildKit archive rejects Content-Encoding");
        }
    }
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

fn validate_persistent_buildkit_tar(body: &[u8]) -> Result<String> {
    if body.len() < 1024 || !body.len().is_multiple_of(512) {
        bail!("persistent BuildKit archive is not a padded tar stream");
    }
    if body.len() == 1024 && body.iter().all(|byte| *byte == 0) {
        return Ok("no-config-v1".to_owned());
    }
    if body.iter().all(|byte| *byte == 0) {
        bail!("persistent BuildKit no-config archive must contain exactly two zero blocks");
    }
    let mut offset = 0;
    let mut files = 0;
    let mut directories = BTreeSet::new();
    let mut config_fingerprint = None;
    let mut found_terminator = false;
    while offset + 512 <= body.len() {
        let header = &body[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            if offset + 1024 > body.len()
                || body[offset..offset + 1024].iter().any(|byte| *byte != 0)
            {
                bail!("persistent BuildKit archive has a truncated end-of-archive marker");
            }
            if body[offset..].iter().any(|byte| *byte != 0) {
                bail!("persistent BuildKit archive has nonzero data after its terminator");
            }
            found_terminator = true;
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
                if !directories.insert(path) {
                    bail!("persistent BuildKit archive repeats its config directory");
                }
            }
            0 | b'0' => {
                if path != "buildkit/buildkitd.toml"
                    || tar_octal(&header[100..108])? != 0o644
                    || size == 0
                    || &body[data_start..data_end] != APPROVED_BUILDKIT_CONFIG_ARCHIVE
                    || !is_approved_buildkit_config(&body[data_start..data_end])
                {
                    bail!(
                        "persistent BuildKit archive permits only the approved buildkit/buildkitd.toml"
                    );
                }
                files += 1;
                let digest = Sha256::digest(&body[data_start..data_end]);
                let mut fingerprint = String::with_capacity(71);
                fingerprint.push_str("sha256:");
                for byte in digest {
                    use std::fmt::Write as _;
                    write!(&mut fingerprint, "{byte:02x}")?;
                }
                config_fingerprint = Some(fingerprint);
            }
            _ => bail!("persistent BuildKit archive contains a non-regular entry"),
        }
        offset = padded_end;
    }
    if !found_terminator {
        bail!("persistent BuildKit archive omitted its end-of-archive marker");
    }
    if files != 1 || directories != BTreeSet::from(["buildkit/".to_owned()]) {
        bail!("persistent BuildKit archive must contain exactly buildkit/buildkitd.toml");
    }
    config_fingerprint.context("persistent BuildKit archive omitted its config file")
}

/// Compare supplied BuildKit config semantics rather than its source bytes.
/// The archive path separately requires Buildx's pinned canonical payload.
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

fn buildkit_command_has_approved_config(value: &Value) -> Result<bool> {
    if !is_safe_buildkit_cmd(value) {
        bail!("Docker persistent BuildKit Cmd is outside the approved config modes");
    }
    let Some(items) = value.as_array() else {
        return Ok(false);
    };
    match items.as_slice() {
        [] => Ok(false),
        [flag, path]
            if flag.as_str() == Some("--config")
                && path.as_str() == Some("/etc/buildkit/buildkitd.toml") =>
        {
            Ok(true)
        }
        _ => bail!("Docker persistent BuildKit Cmd has an unapproved config argument"),
    }
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
    domain_token: &str,
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
    inject_persistent_buildkit_labels_value(value, job_id, domain_token)
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

fn persistent_buildkit_domain_token(builder: &str) -> Option<&str> {
    crate::buildkit::persistent_builder_domain_token(builder)
}

fn persistent_buildkit_domain_token_for_container(container: &str) -> Option<&str> {
    persistent_buildkit_builder_name(container).and_then(persistent_buildkit_domain_token)
}

fn persistent_buildkit_domain_token_for_volume(volume: &str) -> Option<&str> {
    persistent_buildkit_volume_builder_name(volume).and_then(persistent_buildkit_domain_token)
}

fn persistent_bootstrap_domain_token(request: &[u8]) -> Result<String> {
    let name = containers_create_query_name(request)?
        .context("persistent BuildKit bootstrap omitted its container name")?;
    persistent_buildkit_domain_token_for_container(&name)
        .map(str::to_owned)
        .context("persistent BuildKit bootstrap name has no current domain token")
}

fn is_persistent_buildkit_container_name(container: &str) -> bool {
    persistent_buildkit_builder_name(container).is_some()
}

/// Buildx's generated node names append a decimal index, but `--node` can
/// supply a custom name. Velnor owns generated node 0 only, so reject every
/// other reserved numeric node here. Custom names fall through to generic
/// create validation, which denies Buildx's required `Privileged: true`.
fn is_reserved_persistent_buildkit_container_name(container: &str) -> bool {
    let Some(rest) = container.strip_prefix("buildx_buildkit_") else {
        return false;
    };
    let first_node_digit = rest
        .char_indices()
        .rev()
        .take_while(|(_, char)| char.is_ascii_digit())
        .last()
        .map(|(index, _)| index);
    first_node_digit.is_some_and(|index| {
        let builder = &rest[..index];
        !builder.is_empty() && crate::buildkit::is_persistent_builder_name(builder)
    })
}

/// A persistent BuildKit state volume is `<container>_state`. Keep this
/// matcher coupled to the exact builder namespace and suffix; a generic named
/// volume never enters the exception.
pub(crate) fn is_persistent_buildkit_volume_name(volume: &str) -> bool {
    persistent_buildkit_volume_builder_name(volume).is_some()
}

/// Cleanup quarantine for every structurally named Velnor Buildx node volume,
/// including retired and appended numeric nodes. Authorization and attestation
/// still use the exact node-zero matcher above; these children have no durable
/// domain proof and must not be deleted by generic job-volume cleanup.
pub(crate) fn is_persistent_buildkit_volume_object(volume: &str) -> bool {
    crate::buildkit::is_persistent_builder_object(volume)
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
/// persistent builder; the domain label binds it to the stable v2 builder
/// name, and the exact host-registered builder state name is checked.
fn attest_persistent_buildkit_volume(
    body: &[u8],
    expected_name: &str,
    domain_token: &str,
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
    attest_persistent_buildkit_volume_fields(
        &identity,
        expected_name,
        domain_token,
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
    domain_token: &str,
    allowed_builders: &BTreeSet<String>,
) -> Result<()> {
    let identity = parse_volume_identity(output)?;
    attest_persistent_buildkit_volume_fields(
        &identity,
        expected_name,
        domain_token,
        allowed_builders,
    )
}

fn attest_persistent_buildkit_volume_fields(
    identity: &VolumeIdentity,
    expected_name: &str,
    domain_token: &str,
    allowed_builders: &BTreeSet<String>,
) -> Result<()> {
    let Some(builder) = persistent_buildkit_volume_builder_name(expected_name) else {
        bail!("Docker volume {expected_name} is outside the persistent BuildKit namespace");
    };
    if !allowed_builders.contains(builder) {
        bail!("Docker volume {expected_name} is not the current job's BuildKit state volume");
    }
    let expected_domain_token = persistent_buildkit_domain_token(builder)
        .context("Docker persistent BuildKit volume name has no current domain token")?;
    if expected_domain_token != domain_token {
        bail!("Docker persistent BuildKit volume domain does not match the requested domain");
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
            .any(|key| key != JOB_ID_LABEL && key != BUILDKIT_DOMAIN_LABEL)
    {
        bail!("Docker persistent BuildKit volume has unexpected ownership labels");
    }
    if identity
        .labels
        .get(BUILDKIT_DOMAIN_LABEL)
        .map(String::as_str)
        != Some(expected_domain_token)
    {
        bail!("Docker persistent BuildKit volume domain ownership label mismatch");
    }
    Ok(())
}

/// Attest a reused docker-container BuildKit daemon before granting its
/// start/wait operations. Its name, BuildKit image, lease labels, bridge
/// network, and exact expected state-volume mount all bind the object to
/// Velnor's persistent-builder contract. The volume itself is re-attested
/// separately before a start or guest mount.
pub(crate) fn attest_persistent_buildkit_container(
    body: &[u8],
    target: &str,
    allowed_builders: &BTreeSet<String>,
    approved_images: &BTreeMap<String, String>,
) -> Result<(String, String, String, String, bool)> {
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
    let domain_token = persistent_buildkit_domain_token(builder)
        .context("Docker persistent BuildKit container name has no current domain token")?;
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
    let cmd = api_object_field(config, "Cmd")
        .context("Docker persistent BuildKit container omitted Cmd")?;
    if !is_safe_buildkit_cmd(cmd) {
        bail!("Docker persistent BuildKit container command is not approved");
    }
    let has_config_flag = buildkit_command_has_approved_config(cmd)?;
    let labels = api_object_field(config, "Labels")
        .and_then(Value::as_object)
        .context("Docker persistent BuildKit container omitted ownership labels")?;
    if labels.len() != 2
        || labels
            .keys()
            .any(|key| key != JOB_ID_LABEL && key != BUILDKIT_DOMAIN_LABEL)
    {
        bail!("Docker persistent BuildKit container has unexpected labels");
    }
    if labels
        .get(JOB_ID_LABEL)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || labels.get(BUILDKIT_DOMAIN_LABEL).and_then(Value::as_str) != Some(domain_token)
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
    Ok((name, id, expected_volume, image_id, has_config_flag))
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
    // Persistent BuildKit state is shared across jobs. Registered node-0
    // builders are removed by domain lifecycle cleanup. Unregistered appended
    // numeric nodes remain quarantined for explicit operator cleanup; generic
    // teardown must never remove any persistent marker by creator job label.
    let volumes = snapshot
        .volumes
        .iter()
        .filter(|volume| !is_persistent_buildkit_volume_object(volume))
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
        .filter(|name| !is_persistent_buildkit_volume_object(name))
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
    host_socket: PathBuf,
    #[cfg(unix)]
    conns: Arc<LeaseConnSet>,
    #[cfg(unix)]
    shutdown_wake: Option<std::os::unix::net::UnixStream>,
}

/// Live guest/host unix streams for one job lease. Drop aborts ordinary
/// requests so an in-flight Engine `POST /containers/{id}/start` cannot pin
/// Created BuildKit behind a lock that `docker rm --force` never wins. A
/// fenced ContainerCreate's host stream stays open until its final reply.
#[cfg(unix)]
struct LeaseConnSet {
    shutdown: Arc<AtomicBool>,
    connection_count: std::sync::atomic::AtomicUsize,
    buffered_bytes: std::sync::atomic::AtomicUsize,
    next_id: Mutex<u64>,
    streams: Mutex<BTreeMap<u64, WatchedLeaseStream>>,
}

#[cfg(unix)]
struct WatchedLeaseStream {
    stream: std::os::unix::net::UnixStream,
    abort_protected: bool,
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
        streams.retain(|_, watched| {
            if watched.abort_protected {
                true
            } else {
                let _ = watched.stream.shutdown(std::net::Shutdown::Both);
                false
            }
        });
    }

    fn set_abort_protected(&self, id: u64, protected: bool) -> bool {
        let mut streams = self.streams.lock().unwrap_or_else(|err| err.into_inner());
        if protected && self.is_shutdown() {
            return false;
        }
        let Some(watched) = streams.get_mut(&id) else {
            return false;
        };
        watched.abort_protected = protected;
        true
    }

    fn watch(self: &Arc<Self>, stream: &std::os::unix::net::UnixStream) -> WatchedStream {
        let id = stream.try_clone().ok().map(|clone| {
            let mut next = self.next_id.lock().unwrap_or_else(|err| err.into_inner());
            let id = *next;
            *next = next.saturating_add(1);
            self.streams
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .insert(
                    id,
                    WatchedLeaseStream {
                        stream: clone,
                        abort_protected: false,
                    },
                );
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

#[cfg(unix)]
impl WatchedStream {
    fn set_abort_protected(&self, protected: bool) -> bool {
        self.id
            .is_some_and(|id| self.set.set_abort_protected(id, protected))
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

    #[cfg(test)]
    pub(crate) fn bind_to_with_test_volume_lock_root(
        listen_path: PathBuf,
        host_socket: PathBuf,
        job_id: String,
        daemon_id: String,
        volume_lock_root: PathBuf,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (
                listen_path,
                host_socket,
                job_id,
                daemon_id,
                volume_lock_root,
            );
            bail!("job Docker lease proxy requires unix");
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(&volume_lock_root).with_context(|| {
                format!(
                    "create test Docker volume lock root {}",
                    volume_lock_root.display()
                )
            })?;
            let volume_lock_root = std::fs::canonicalize(&volume_lock_root)
                .context("canonicalize test Docker volume lock root")?;
            bind_unix_lease_with_volume_lock_root(
                listen_path,
                host_socket,
                job_id,
                daemon_id,
                volume_lock_root,
            )
        }
    }

    pub(crate) fn begin_persistent_builder_setup(
        &self,
        builder: &str,
        config_fingerprint: &str,
    ) -> Result<u64> {
        self.policy
            .begin_persistent_builder_setup(builder, config_fingerprint)
    }

    #[cfg(unix)]
    pub(crate) fn recover_pending_buildkit_create_before_volume_setup(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
        generation: u64,
        config_fingerprint: &str,
    ) -> Result<Option<String>> {
        if !has_recoverable_pending_buildkit_create(domain, builder)? {
            return Ok(None);
        }
        let engine_id = require_volume_lock_engine_id(
            crate::docker::engine::daemon_identity_blocking(&self.host_socket)
                .map(|identity| identity.id),
        )?;
        recover_pending_buildkit_create_for_setup_with_volume_lock(
            &self.policy,
            &self.host_socket,
            domain,
            builder,
            generation,
            config_fingerprint,
            &engine_id,
            lock_host_volume_name_for_pending_create,
        )
    }

    #[cfg(not(unix))]
    pub(crate) fn recover_pending_buildkit_create_before_volume_setup(
        &self,
        _domain: &crate::buildkit::PersistentBuildKitDomain,
        _builder: &str,
        _generation: u64,
        _config_fingerprint: &str,
    ) -> Result<Option<String>> {
        bail!("pending BuildKit setup recovery requires a local Unix Docker Engine")
    }

    #[cfg(unix)]
    pub(crate) fn complete_persistent_builder_setup(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        builder: &str,
    ) -> Result<()> {
        let config_fingerprint = self.policy.persistent_builder_config_fingerprint(builder)?;
        let approved_images = self.policy.persistent_builder_images()?;
        let allowed_builders = BTreeSet::from([builder.to_owned()]);
        crate::buildkit::promote_builder_readiness_v1_with(
            domain,
            builder,
            &config_fingerprint,
            |volume| lock_host_volume_name_for_domain(domain, volume),
            || {
                let endpoint = crate::docker::engine::resolve_docker_endpoint()
                    .context("resolve Docker Engine for readiness migration")?;
                if endpoint.socket != self.host_socket {
                    bail!("Docker endpoint changed during BuildKit readiness migration");
                }
                let engine_id = crate::docker::engine::daemon_identity_blocking(&self.host_socket)
                    .map(|identity| identity.id)
                    .filter(|identity| !identity.trim().is_empty())
                    .context("Docker Engine ID is unavailable during readiness migration")?;
                if engine_id != domain.engine_id {
                    bail!("Docker Engine changed during BuildKit readiness migration");
                }
                Ok(())
            },
            |builder, volume, container_id| {
                let volume_output = crate::docker::client::host_call(
                    &inspect_persistent_buildkit_volume_args(volume),
                )
                .with_context(|| format!("inspect legacy BuildKit state volume {volume}"))?;
                attest_persistent_buildkit_volume_identity(&volume_output, volume, &domain.token)?;
                let (status, body) = inspect_container_on_host(&self.host_socket, container_id)?;
                if status != 200 {
                    bail!("legacy BuildKit container inspect returned HTTP {status}");
                }
                let (name, inspected_id, inspected_volume, _, has_config_flag) =
                    attest_persistent_buildkit_container(
                        &body,
                        container_id,
                        &allowed_builders,
                        &approved_images,
                    )?;
                if name != crate::buildkit::daemon_container_name(builder)
                    || inspected_id != container_id
                    || inspected_volume != volume
                    || !crate::buildkit::config_mode_matches_command(
                        &config_fingerprint,
                        has_config_flag,
                    )
                {
                    bail!("legacy BuildKit container does not match its exact domain/config");
                }
                let state = crate::docker::Docker::host()
                    .inspect_exit(container_id)
                    .context("inspect legacy BuildKit container state")?;
                if state.status != Some(crate::docker::client::ContainerState::Running) {
                    bail!("legacy BuildKit container is not running during readiness migration");
                }
                Ok(())
            },
            crate::buildkit::wait_for_attested_buildkit_ready,
        )?;
        crate::buildkit::recover_starting_builder_in_domain(domain, builder, &config_fingerprint)?;
        let volume = crate::buildkit::daemon_state_volume(builder);
        let _volume_lock = lock_host_volume_name_for_domain(domain, &volume)?;
        let readiness_epoch = crate::buildkit::builder_readiness_epoch_for_setup(
            domain,
            builder,
            &config_fingerprint,
        )?;
        self.policy
            .complete_persistent_builder_setup(builder, readiness_epoch)
    }

    #[cfg(not(unix))]
    pub(crate) fn complete_persistent_builder_setup(
        &self,
        _domain: &crate::buildkit::PersistentBuildKitDomain,
        _builder: &str,
    ) -> Result<()> {
        bail!("persistent BuildKit setup requires a local Unix Docker Engine")
    }

    pub(crate) fn revoke_persistent_builder(&self, builder: &str) -> Result<()> {
        self.policy.revoke_persistent_builder(builder)
    }

    pub(crate) fn revoke_all_persistent_builders(&self) -> Result<()> {
        let builders = self.policy.persistent_builder_names_for_attestation()?;
        for builder in builders {
            self.policy.revoke_persistent_builder(&builder)?;
        }
        Ok(())
    }

    /// Bind a persistent builder to the host-resolved immutable image ID
    /// before the guest can issue Buildx image/container calls.
    pub fn register_persistent_builder_image(&self, builder: &str, image_id: &str) -> Result<()> {
        self.policy
            .register_persistent_builder_image(builder, image_id)
    }

    /// Acquire the lease's local and cross-process volume lock while also
    /// migrating any pre-domain BuildKit create marker into durable domain
    /// quarantine. Persistent volume creation must use this path so a reboot
    /// cannot erase its only pending-create fence.
    #[cfg(unix)]
    pub(crate) fn lock_volume_name_for_domain(
        &self,
        domain: &crate::buildkit::PersistentBuildKitDomain,
        volume: &str,
    ) -> Result<VolumeOperationLocks> {
        self.policy.lock_volume_names_with_create_access(
            &BTreeSet::from([volume.to_owned()]),
            Some(domain),
            None,
        )
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
        domain_token: &str,
    ) -> Result<()> {
        self.policy
            .record_persistent_volume_projection(volume, body, domain_token)
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
    let root = docker_volume_lock_root(host_socket)?;
    let policy =
        DockerLeasePolicy::new_with_volume_lock_root("velnor-host-volume-lock", Some(root))?;
    if persistent_buildkit_volume_builder_name(volume).is_some() {
        let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
            .context("resolve domain before locking a persistent BuildKit volume")?;
        let engine_id = require_volume_lock_engine_id(
            crate::docker::engine::daemon_identity_blocking(host_socket)
                .map(|identity| identity.id),
        )?;
        if domain.engine_id != engine_id {
            bail!("persistent BuildKit volume lock resolved a different Docker Engine");
        }
        policy.lock_volume_names_with_create_access(
            &BTreeSet::from([volume.to_owned()]),
            Some(&domain),
            None,
        )
    } else {
        policy.lock_volume_names(&BTreeSet::from([volume.to_owned()]))
    }
}

/// Acquire an engine-wide volume lock when the caller already resolved the
/// durable storage identity root and Engine `/info.ID`. BuildKit lifecycle
/// cleanup uses this path so it shares the live lease's lock inode even when
/// the runner runs under a different temporary directory.
#[cfg(unix)]
pub(crate) fn lock_host_volume_name_for_domain(
    domain: &crate::buildkit::PersistentBuildKitDomain,
    volume: &str,
) -> Result<VolumeOperationLocks> {
    let root = docker_volume_lock_root_for_domain(&domain.identity_root, &domain.engine_id)?;
    let policy =
        DockerLeasePolicy::new_with_volume_lock_root("velnor-host-volume-lock", Some(root))?;
    policy.lock_volume_names_with_create_access(
        &BTreeSet::from([volume.to_owned()]),
        Some(domain),
        None,
    )
}

#[cfg(unix)]
pub(crate) fn lock_host_volume_name_for_pending_create(
    domain: &crate::buildkit::PersistentBuildKitDomain,
    volume: &str,
    access: &crate::buildkit::PendingBuildKitCreateAccess,
) -> Result<VolumeOperationLocks> {
    let root = docker_volume_lock_root_for_domain(&domain.identity_root, &domain.engine_id)?;
    let policy = DockerLeasePolicy::new_with_volume_lock_root(
        "velnor-buildkit-create-recovery",
        Some(root),
    )?;
    policy.lock_volume_names_with_create_access(
        &BTreeSet::from([volume.to_owned()]),
        Some(domain),
        Some(access),
    )
}

#[cfg(all(test, unix))]
pub(crate) fn try_lock_volume_name_at_for_test(
    volume_lock_root: &Path,
    volume: &str,
) -> Result<Option<File>> {
    let root = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(volume_lock_root)?;
    let file_name = volume_lock_file_name(volume);
    let file = root.open_or_create_lock_file(OsStr::new(&file_name))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try Docker volume lock")),
    }
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
            // `docker rm --force` hang until dockerd was SIGKILL'd. A
            // dispatched persistent create is the exception: its durable
            // fence requires draining the same socket's final reply.
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
fn docker_volume_lock_root(host_socket: &Path) -> Result<PathBuf> {
    let layout = require_volume_lock_storage_layout(
        crate::storage::selected_layout().or_else(crate::storage::StorageLayout::resolve),
    )?;
    let identity_root = layout.buildkit_identity_root();
    let engine_id = require_volume_lock_engine_id(
        crate::docker::engine::daemon_identity_blocking(host_socket).map(|identity| identity.id),
    )?;
    docker_volume_lock_root_for_domain(&identity_root, &engine_id)
}

fn require_volume_lock_storage_layout(
    layout: Option<crate::storage::StorageLayout>,
) -> Result<crate::storage::StorageLayout> {
    layout.context("Docker volume locking requires the selected Velnor storage layout")
}

fn require_volume_lock_engine_id(engine_id: Option<String>) -> Result<String> {
    let engine_id = engine_id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
        .context("Docker Engine /info.ID is unavailable; Docker volume locking is disabled")?;
    Ok(engine_id)
}

#[cfg(unix)]
fn docker_volume_lock_root_for_domain(identity_root: &Path, engine_id: &str) -> Result<PathBuf> {
    let _layout = require_volume_lock_storage_layout(
        crate::storage::selected_layout().or_else(crate::storage::StorageLayout::resolve),
    )?;
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    docker_volume_lock_root_for_layout(identity_root, engine_id, xdg_runtime_dir.as_deref())
}

fn docker_volume_lock_root_for_layout(
    identity_root: &Path,
    engine_id: &str,
    xdg_runtime_dir: Option<&Path>,
) -> Result<PathBuf> {
    if engine_id.trim().is_empty() || engine_id.chars().any(char::is_control) {
        bail!("Docker Engine /info.ID is empty or malformed");
    }
    crate::storage::ensure_buildkit_storage_identity(identity_root)
        .context("validate durable Velnor storage identity for Docker volume locking")?;
    let lock_namespace = shared_host_volume_lock_namespace(identity_root, xdg_runtime_dir)?;
    docker_volume_lock_root_under(&lock_namespace, engine_id)
}

fn shared_host_volume_lock_namespace(
    _identity_root: &Path,
    xdg_runtime_dir: Option<&Path>,
) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let _ = xdg_runtime_dir;
        return Ok(PathBuf::from("/run/velnor/docker-volume-locks"));
    }

    #[cfg(target_os = "macos")]
    {
        let _ = xdg_runtime_dir;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .filter(|path| {
                !path
                    .components()
                    .any(|component| component == std::path::Component::ParentDir)
            })
            .context("Docker volume locking requires a stable absolute HOME on macOS")?;
        return Ok(home.join("Library/Caches/velnor/docker-volume-locks"));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = xdg_runtime_dir;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .filter(|path| {
                !path
                    .components()
                    .any(|component| component == std::path::Component::ParentDir)
            })
            .context("Docker volume locking requires a stable absolute HOME")?;
        Ok(home.join(".cache/velnor/docker-volume-locks"))
    }
}

fn docker_volume_lock_root_under(namespace_root: &Path, engine_id: &str) -> Result<PathBuf> {
    if engine_id.trim().is_empty() || engine_id.chars().any(char::is_control) {
        bail!("Docker Engine /info.ID is empty or malformed");
    }
    let engine_segment = volume_lock_key(engine_id.trim());
    let root = namespace_root.join(engine_segment);
    crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(&root)
        .with_context(|| format!("secure Docker volume lock root {}", root.display()))?;
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

#[cfg(unix)]
fn acquire_volume_file_lock(
    root: &crate::fs_copy::NoFollowDestinationDir,
    volume: &str,
) -> Result<File> {
    let file_name = volume_lock_file_name(volume);
    let file = root
        .open_or_create_lock_file(OsStr::new(&file_name))
        .with_context(|| format!("open Docker volume lock {file_name}"))?;
    let deadline = Instant::now() + VOLUME_LOCK_TIMEOUT;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) => {
                if Instant::now() >= deadline {
                    bail!(
                        "timed out acquiring Docker volume lock {file_name} after {:?}",
                        VOLUME_LOCK_TIMEOUT
                    );
                }
                std::thread::sleep(VOLUME_LOCK_RETRY);
            }
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context(format!("lock Docker volume {file_name}"))
                );
            }
        }
    }
}

#[cfg(unix)]
fn release_persistent_conflict_volume_lock(status: u16, locks: &mut Option<VolumeOperationLocks>) {
    if status == 409 {
        drop(locks.take());
    }
}

#[cfg(unix)]
fn volume_lock_file_name(volume: &str) -> String {
    format!("{}.lock", volume_lock_key(volume))
}

#[cfg(unix)]
fn bind_unix_lease(
    listen_path: PathBuf,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
) -> Result<DockerLeaseGuard> {
    validate_unix_lease_path(&listen_path)?;
    let volume_lock_root = docker_volume_lock_root(&host_socket)?;
    bind_unix_lease_with_volume_lock_root(
        listen_path,
        host_socket,
        job_id,
        daemon_id,
        volume_lock_root,
    )
}

#[cfg(unix)]
fn bind_unix_lease_with_volume_lock_root(
    listen_path: PathBuf,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    volume_lock_root: PathBuf,
) -> Result<DockerLeaseGuard> {
    use std::os::unix::net::UnixListener;

    validate_unix_lease_path(&listen_path)?;

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
    let policy = Arc::new(DockerLeasePolicy::new_with_volume_lock_root(
        &job_id,
        Some(volume_lock_root),
    )?);
    let conns_thread = Arc::clone(&conns);
    let policy_thread = Arc::clone(&policy);
    let listen_path_thread = listen_path.clone();
    let guard_host_socket = host_socket.clone();
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
        #[cfg(unix)]
        host_socket: guard_host_socket,
        conns,
        shutdown_wake: Some(wake_writer),
    })
}

#[cfg(unix)]
fn validate_unix_lease_path(listen_path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path_bytes = listen_path.as_os_str().as_bytes().len();
    if path_bytes >= UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "job Docker lease socket path {} is {} bytes, exceeding the safe Unix socket limit of {}; shorten --work-dir or choose a shorter daemon-visible work root",
            listen_path.display(),
            path_bytes,
            UNIX_SOCKET_PATH_LIMIT
        );
    }
    Ok(())
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
        let mut authorization = match policy.authorize_admitted(&bytes) {
            Ok(authorization) => authorization,
            Err(error) => {
                if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                    write_deny_response(&mut client, deny.status, &deny.message)?;
                }
                return Err(error);
            }
        };
        let route = authorization.route;
        let mut request_readiness_epoch = authorization.readiness_epoch();
        // Snapshot the immutable authorization fields into owned values so
        // the response observer can retire this request's admission without
        // borrowing the same `authorization` object immutably across dispatch.
        let request_fence_owner = authorization
            .fence()
            .map(|(builder, generation)| (builder.to_owned(), generation));
        let request_fence = request_fence_owner
            .as_ref()
            .map(|(builder, generation)| (builder.as_str(), *generation));
        let authorized_container_id_owner = authorization.container_id().map(str::to_owned);
        let authorized_container_id = authorized_container_id_owner.as_deref();
        if matches!(
            route,
            AuthorizedDockerRoute::PersistentBootstrap
                | AuthorizedDockerRoute::PersistentArchive
                | AuthorizedDockerRoute::PersistentImagePull
                | AuthorizedDockerRoute::PersistentImageInspect
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::PersistentExec
                | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        ) && request_fence.is_none()
        {
            bail!("persistent BuildKit route has no atomic capability generation");
        }
        let request_method = http_request_method(&bytes)?.to_owned();
        let request_wants_close = http_request_wants_close(&bytes);
        let upgrade = request_is_upgrade(&bytes);
        let resource_target = docker_resource_target(&bytes);
        let persistent_archive_fingerprint =
            if matches!(route, AuthorizedDockerRoute::PersistentArchive) {
                Some(validate_persistent_archive_request(
                    &bytes,
                    docker_request_line(&bytes)?.1,
                )?)
            } else {
                None
            };
        if let Some(fingerprint) = persistent_archive_fingerprint.as_deref() {
            let (builder, generation) = request_fence
                .context("persistent BuildKit archive omitted capability generation")?;
            let container_id = authorized_container_id
                .context("persistent BuildKit archive omitted immutable container ID")?;
            policy.authorize_persistent_config_archive(
                builder,
                container_id,
                generation,
                fingerprint,
            )?;
        }
        let create_container_name = match route {
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
            | AuthorizedDockerRoute::PersistentBootstrap => containers_create_query_name(&bytes)?,
            _ => None,
        };
        let create_volume_name = match route {
            AuthorizedDockerRoute::Create(DockerResourceKind::Volume) => {
                let body = docker_request_body(&bytes)?;
                let value = parse_create_value(body)?;
                volume_create_request_name(&value)?
            }
            _ => None,
        };
        #[cfg(unix)]
        let persistent_domain = if matches!(
            route,
            AuthorizedDockerRoute::PersistentBootstrap
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
        ) {
            Some(
                crate::buildkit::PersistentBuildKitDomain::resolve()
                    .context("resolve domain before persistent BuildKit volume access")?,
            )
        } else {
            None
        };
        #[cfg(unix)]
        if matches!(
            route,
            AuthorizedDockerRoute::PersistentBootstrap
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        ) && let Some((builder, generation)) = request_fence
        {
            let domain = persistent_domain
                .as_ref()
                .context("persistent BuildKit request omitted its resolved domain")?;
            let config_fingerprint = policy.persistent_builder_config_fingerprint(builder)?;
            if let Some(container_id) = recover_pending_buildkit_create_for_request(
                &policy,
                host_socket,
                domain,
                builder,
                generation,
                &config_fingerprint,
            )? {
                let readiness_epoch = note_current_persistent_readiness(
                    &policy,
                    domain,
                    builder,
                    generation,
                    &container_id,
                    &config_fingerprint,
                )?;
                authorization.set_readiness_epoch(readiness_epoch)?;
                request_readiness_epoch = Some(readiness_epoch);
            }
        }
        let mut resource_reservation = if matches!(route, AuthorizedDockerRoute::Create(_))
            || matches!(route, AuthorizedDockerRoute::PersistentExecCreate)
        {
            Some(policy.reserve_owned_resource_slot()?)
        } else {
            None
        };
        #[cfg(unix)]
        let mut volume_locks = Some({
            let result = (|| -> Result<VolumeOperationLocks> {
                match route {
                    AuthorizedDockerRoute::Create(DockerResourceKind::Container)
                    | AuthorizedDockerRoute::PersistentBootstrap => preflight_container_mounts(
                        &policy,
                        host_socket,
                        &bytes,
                        job_id,
                        daemon_id,
                        persistent_domain.as_ref(),
                    ),
                    AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
                    | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
                        if matches!(request_method.as_str(), "GET" | "HEAD" | "DELETE") =>
                    {
                        let target = resource_target
                            .as_deref()
                            .context("volume route omitted its target")?;
                        let names = BTreeSet::from([target.to_owned()]);
                        let locks = if matches!(
                            route,
                            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
                        ) {
                            let domain = persistent_domain
                                .as_ref()
                                .context("persistent volume route omitted its resolved domain")?;
                            policy.lock_volume_names_with_create_access(
                                &names,
                                Some(domain),
                                None,
                            )?
                        } else {
                            policy.lock_volume_names(&names)?
                        };
                        preflight_volume_identity(
                            &policy,
                            host_socket,
                            target,
                            route,
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
                            authorized_container_id
                                .or(resource_target.as_deref())
                                .context(
                                    "persistent container route omitted its immutable target",
                                )?,
                            job_id,
                            daemon_id,
                            request_fence,
                        )
                    }
                    AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                        if resource_target.is_some() =>
                    {
                        preflight_persistent_container_volume(
                            &policy,
                            host_socket,
                            authorized_container_id
                                .or(resource_target.as_deref())
                                .context("persistent inspect omitted its immutable target")?,
                            job_id,
                            daemon_id,
                            request_fence,
                        )
                    }
                    AuthorizedDockerRoute::PersistentArchive
                    | AuthorizedDockerRoute::PersistentExecCreate
                        if resource_target.is_some() =>
                    {
                        preflight_persistent_container_volume(
                            &policy,
                            host_socket,
                            authorized_container_id
                                .or(resource_target.as_deref())
                                .context(
                                    "persistent container route omitted its immutable target",
                                )?,
                            job_id,
                            daemon_id,
                            request_fence,
                        )
                    }
                    AuthorizedDockerRoute::PersistentExec => preflight_persistent_container_volume(
                        &policy,
                        host_socket,
                        authorization
                            .container_id()
                            .context("persistent exec route omitted its immutable container ID")?,
                        job_id,
                        daemon_id,
                        request_fence,
                    ),
                    AuthorizedDockerRoute::PersistentImagePull
                    | AuthorizedDockerRoute::PersistentImageInspect => {
                        let (builder, _) = authorization
                            .fence()
                            .context("persistent image route omitted its builder generation")?;
                        let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                            .context("resolve BuildKit domain before image dispatch")?;
                        let engine_id = require_volume_lock_engine_id(
                            crate::docker::engine::daemon_identity_blocking(host_socket)
                                .map(|identity| identity.id),
                        )?;
                        if engine_id != domain.engine_id {
                            bail!("persistent image route resolved another Docker Engine");
                        }
                        lock_host_volume_name_for_domain(
                            &domain,
                            &crate::buildkit::daemon_state_volume(builder),
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
        });
        let forwarded = transform_request_buffer(bytes, &mut budget, |request| {
            policy.rewrite_docker_api_request_for_route(request, job_id, daemon_id, route)
        })?;
        let forwarded = transform_request_buffer(forwarded, &mut budget, |request| {
            policy.rewrite_authorized_alias_target(request, route, authorized_container_id)
        })?;
        let forwarded = transform_request_buffer(forwarded, &mut budget, without_expect_continue)?;
        if conns.is_shutdown() {
            return Ok(());
        }
        if upgrade {
            let (mut host, _host_watch) = connect_lease_host(host_socket, &conns)?;
            // Register the live socket pair before dispatch. A Docker Engine
            // that never answers the upgrade must still be interruptible by
            // revoke/setup, which closes admission and shuts down every
            // registered request socket before draining.
            let persistent_tunnel = if matches!(route, AuthorizedDockerRoute::PersistentExec) {
                Some(authorization.register_persistent_tunnel(&host, &client)?)
            } else {
                None
            };
            if matches!(route, AuthorizedDockerRoute::PersistentExec) {
                let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                    .context("resolve BuildKit domain before persistent exec dispatch")?;
                validate_persistent_route_under_volume_lock(
                    &mut authorization,
                    &policy,
                    &domain,
                    route,
                )?;
            }
            host.write_all(&forwarded)
                .context("forward Docker API request through job lease")?;
            // Keep the same idle timeout on hijacked streams. Clearing it
            // would let an abandoned attach/build session hold one of the
            // bounded lease connections forever.
            if !remainder.is_empty() {
                host.write_all(&remainder)
                    .context("forward buffered Docker upgrade bytes")?;
            }
            if matches!(route, AuthorizedDockerRoute::PersistentExec) {
                let response = read_upgrade_response(&mut host, &mut client)?;
                if response.status == 101 {
                    let tunnel = persistent_tunnel
                        .context("persistent Docker upgrade lost its registered tunnel")?;
                    client
                        .write_all(&response.bytes[..response.header_end])
                        .context("forward Docker exec upgrade response headers")?;
                    // Keep the admission through dispatch and the complete
                    // 101 response headers, but not for the long-lived
                    // buildctl stream.
                    authorization._persistent_builder.take();
                    volume_locks.take();
                    let result =
                        proxy_until_closed(host, client, &response.bytes[response.header_end..]);
                    drop(tunnel);
                    return result;
                }

                // Docker reports denied or stale exec starts with an ordinary
                // framed response. Forward it as HTTP and keep the admission
                // until its body is complete; never reinterpret it as a
                // successful hijack.
                let mut buffered = ResponseBuffer::default();
                buffered.extend_from_slice(&response.bytes);
                let _ = forward_http_response_with_observer(
                    &mut host,
                    &mut buffered,
                    &mut client,
                    &request_method,
                    ForwardResponseOptions::default(),
                    |_, _| Ok(()),
                )?;
                return Ok(());
            }
            return proxy_until_closed(host, client, &[]);
        }

        if host_state.is_none() {
            host_state = Some(connect_lease_host(host_socket, &conns)?);
        }
        if matches!(
            route,
            AuthorizedDockerRoute::PersistentBootstrap
                | AuthorizedDockerRoute::PersistentArchive
                | AuthorizedDockerRoute::PersistentImagePull
                | AuthorizedDockerRoute::PersistentImageInspect
                | AuthorizedDockerRoute::PersistentExecCreate
                | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
        ) && !(matches!(
            route,
            AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
        ) && request_method == "POST")
        {
            let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                .context("resolve domain before persistent BuildKit preflight")?;
            validate_persistent_route_under_volume_lock(
                &mut authorization,
                &policy,
                &domain,
                route,
            )?;
        }
        let mut create_fence = if matches!(route, AuthorizedDockerRoute::PersistentBootstrap) {
            let (builder, generation) = request_fence
                .context("persistent BuildKit create omitted capability generation")?;
            let container_name = create_container_name
                .as_deref()
                .context("persistent BuildKit create omitted its container name")?;
            let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                .context("resolve Engine identity before persistent BuildKit create")?;
            let engine_id = require_volume_lock_engine_id(
                crate::docker::engine::daemon_identity_blocking(host_socket)
                    .map(|identity| identity.id),
            )?;
            if engine_id != domain.engine_id {
                bail!("persistent BuildKit lease endpoint changed Engine identity");
            }
            let volume = crate::buildkit::daemon_state_volume(builder);
            let config_fingerprint = policy.persistent_builder_config_fingerprint(builder)?;
            let expected_image_id = policy.persistent_builder_image(builder)?;
            let create_value = parse_create_value(docker_request_body(&forwarded)?)?;
            let expects_config = api_object_field(
                create_value
                    .as_object()
                    .context("persistent BuildKit create body must be an object")?,
                "Cmd",
            )
            .map(buildkit_command_has_approved_config)
            .transpose()?
            .unwrap_or(false);
            let root = policy
                .volume_lock_root
                .as_ref()
                .context("persistent BuildKit create has no Engine-volume lock root")?
                .clone();
            let expected_lock_root =
                docker_volume_lock_root_for_domain(&domain.identity_root, &engine_id)?;
            let expected_lock_root =
                crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(
                    &expected_lock_root,
                )?;
            if expected_lock_root.physical_identity()? != root.physical_identity()? {
                bail!("persistent BuildKit create lock root no longer matches its Engine identity");
            }
            let host_watch = &host_state
                .as_ref()
                .context("persistent BuildKit create has no Engine connection")?
                .1;
            if !host_watch.set_abort_protected(true) {
                return Ok(());
            }
            Some(PersistentBuildKitCreateFence::begin(
                &domain,
                builder,
                generation,
                &volume,
                container_name,
                &config_fingerprint,
                &expected_image_id,
                expects_config,
                &forwarded,
            )?)
        } else {
            None
        };
        let reusable = {
            // Proof: the branch above assigns `Some` or returns via `?`, so
            // the state is `Some` here.
            #[allow(clippy::expect_used, reason = "host state just initialized")]
            let (host, _) = host_state.as_mut().expect("host state initialized");
            if matches!(
                route,
                AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
            ) && request_method == "POST"
            {
                let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                    .context("resolve domain before persistent BuildKit start")?;
                let epoch = advance_persistent_start_epoch_under_volume_lock(
                    &mut authorization,
                    &policy,
                    &domain,
                )?;
                request_readiness_epoch = Some(epoch);
            }
            if matches!(
                route,
                AuthorizedDockerRoute::PersistentBootstrap
                    | AuthorizedDockerRoute::PersistentArchive
                    | AuthorizedDockerRoute::PersistentImagePull
                    | AuthorizedDockerRoute::PersistentImageInspect
                    | AuthorizedDockerRoute::PersistentExecCreate
                    | AuthorizedDockerRoute::PersistentExec
                    | AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                    | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
                    | AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume)
            ) {
                let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                    .context("resolve domain before persistent BuildKit dispatch")?;
                validate_persistent_route_under_volume_lock(
                    &mut authorization,
                    &policy,
                    &domain,
                    route,
                )?;
            }
            if let Some(reservation) = resource_reservation.as_mut() {
                reservation.mark_dispatched()?;
            }
            if let Err(error) = host
                .write_all(&forwarded)
                .context("forward Docker API request through job lease")
            {
                pin_resource_reservation_after_uncertain_dispatch(&mut resource_reservation);
                eprintln!("T004 host write error: {error:#}");
                if conns.is_shutdown() {
                    return Ok(());
                }
                return Err(error);
            }
            let create_kind = match route {
                AuthorizedDockerRoute::Create(kind) => Some(kind),
                _ => None,
            };
            let capture_response = create_kind.is_some()
                || matches!(route, AuthorizedDockerRoute::PersistentBootstrap)
                || matches!(route, AuthorizedDockerRoute::PersistentExecCreate)
                || matches!(route, AuthorizedDockerRoute::PersistentImageInspect)
                || (matches!(
                    route,
                    AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
                ) && request_method == "GET")
                || matches!(route, AuthorizedDockerRoute::PersistentInspect(_));
            let redact_persistent_container_inspect = matches!(
                route,
                AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
            );
            let redact_persistent_image_inspect =
                matches!(route, AuthorizedDockerRoute::PersistentImageInspect);
            let response_options = ForwardResponseOptions {
                capture_body: capture_response,
                redact_persistent_container_inspect,
                redact_persistent_image_inspect,
                defer_response_until_observed: create_kind.is_some()
                    || matches!(route, AuthorizedDockerRoute::PersistentBootstrap)
                    || matches!(route, AuthorizedDockerRoute::PersistentExecCreate)
                    || matches!(route, AuthorizedDockerRoute::PersistentArchive)
                    || (matches!(
                        route,
                        AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                    ) && request_method == "POST"),
                detach_client_on_hup: matches!(route, AuthorizedDockerRoute::PersistentBootstrap),
                // BuildKit's 2xx create response must not reach the guest
                // until the exact immutable host object has been inspected
                // and durably bound to the pending-create transaction.
                observe_after_delivery_on_success: false,
                detach_signal: matches!(route, AuthorizedDockerRoute::PersistentBootstrap)
                    .then(|| Arc::clone(&conns.shutdown)),
                persistent_image_ids: if redact_persistent_image_inspect {
                    policy.persistent_builder_images()?.into_values().collect()
                } else {
                    BTreeSet::new()
                },
            };
            let forwarded = forward_http_response_with_delivery(
                host,
                &mut host_buffer,
                &mut client,
                &request_method,
                response_options,
                |status, body, _client_connected| {
                    if matches!(route, AuthorizedDockerRoute::PersistentBootstrap)
                        && (status == 409 || !(200..300).contains(&status))
                    {
                        // A conflict or failed create cannot win bootstrap.
                        // Retire this request from the dispatchable-create
                        // count before conflict recovery waits, so concurrent
                        // 409 responses can observe that no winner remains.
                        if status == 409 {
                            authorization.retire_persistent_bootstrap_conflict()?;
                        } else {
                            authorization.retire_persistent_bootstrap_create()?;
                        }
                    }
                    if let Some(kind) = create_kind {
                        let result = policy.record_create_response_with_lease(
                            kind,
                            status,
                            body,
                            create_volume_name.as_deref(),
                            Some(job_id),
                            Some(daemon_id),
                        );
                        finish_resource_reservation_after_observation(
                            &mut resource_reservation,
                            status,
                            result,
                        )?;
                    }
                    if !matches!(route, AuthorizedDockerRoute::PersistentBootstrap)
                        && let Some(name) = create_container_name.as_deref()
                    {
                        policy.note_container_name(name, status, body)?;
                    }
                    if matches!(route, AuthorizedDockerRoute::PersistentBootstrap) {
                        // The 409 reuse path reacquires this same Engine-wide
                        // state-volume lock after the first host inspection,
                        // then re-attests the immutable container ID under
                        // that lock. Drop our preflight guard before entering
                        // it so separate flock descriptors cannot self-block.
                        release_persistent_conflict_volume_lock(status, &mut volume_locks);
                        let target = create_container_name
                            .as_deref()
                            .context("persistent BuildKit create omitted its container name")?;
                        observe_persistent_bootstrap_response_fenced_with_binding(
                            policy.as_ref(),
                            target,
                            status,
                            body,
                            request_fence,
                            |candidate| inspect_container_on_host(host_socket, candidate),
                            |target, inspect_body| {
                                let fence = create_fence.as_mut().context(
                                    "persistent BuildKit create lost its durable transaction",
                                )?;
                                bind_persistent_create_fence_to_inspect(fence, inspect_body)
                                    .with_context(|| {
                                        format!(
                                            "bind inspected persistent BuildKit create {target}"
                                        )
                                    })
                            },
                            |policy, target, fence| {
                                let builder = policy.persistent_container_builder(target)?;
                                let container_id = policy.persistent_container_id(target)?;
                                let generation = fence.map(|(_, generation)| generation).context(
                                    "persistent BuildKit conflict omitted its generation",
                                )?;
                                let config_fingerprint =
                                    policy.persistent_builder_config_fingerprint(&builder)?;
                                let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
                                    .context(
                                    "resolve domain before reusing conflicting BuildKit container",
                                )?;
                                if crate::buildkit::persistent_builder_domain_token(&builder)
                                    != Some(domain.token.as_str())
                                {
                                    bail!("conflicting BuildKit container belongs to another storage or Engine domain");
                                }
                                if !crate::buildkit::ensure_conflicting_builder_ready_in_domain(
                                    &domain,
                                    &builder,
                                    &container_id,
                                    &config_fingerprint,
                                    || authorization.has_other_persistent_bootstrap_create(),
                                    |id| {
                                        recover_unbound_created_builder_for_request(
                                            policy,
                                            host_socket,
                                            &domain,
                                            &builder,
                                            id,
                                            generation,
                                            &config_fingerprint,
                                        )
                                    },
                                )? {
                                    bail!("conflicting persistent BuildKit container disappeared before it became ready");
                                }
                                if let Some(transaction) =
                                    crate::buildkit::pending_buildkit_create_transaction(
                                        &domain, &builder,
                                    )?
                                {
                                    if transaction.container_id.as_deref()
                                        != Some(container_id.as_str())
                                    {
                                        bail!(
                                            "conflict readiness does not match pending create ID"
                                        );
                                    }
                                    let (inspect_status, inspect_body) = inspect_container_on_host(
                                        host_socket,
                                        &container_id,
                                    )
                                    .context(
                                        "re-attest BuildKit conflict state before settling create",
                                    )?;
                                    if !(200..300).contains(&inspect_status) {
                                        bail!("BuildKit conflict re-attestation returned HTTP {inspect_status}");
                                    }
                                    let (inspected_id, inspected_shape, inspected_state) =
                                        attest_pending_buildkit_create_inspect(
                                            &inspect_body,
                                            &transaction,
                                        )?;
                                    if inspected_id != container_id
                                        || inspected_state != "running"
                                        || transaction.attested_shape_sha256.as_deref()
                                            != Some(inspected_shape.as_str())
                                    {
                                        bail!("BuildKit conflict is not the same attested running daemon");
                                    }
                                    crate::buildkit::mark_pending_buildkit_create_existing_ready(
                                        &domain,
                                        &builder,
                                        &transaction.transaction_id,
                                        &container_id,
                                        &inspected_state,
                                    )?;
                                    crate::buildkit::finish_pending_buildkit_create_transaction(
                                        &domain,
                                        &builder,
                                        &transaction.transaction_id,
                                        &container_id,
                                        &config_fingerprint,
                                    )?;
                                }
                                note_current_persistent_readiness(
                                    policy,
                                    &domain,
                                    &builder,
                                    generation,
                                    &container_id,
                                    &config_fingerprint,
                                )?;
                                Ok(())
                            },
                        )?;
                    }
                    if matches!(route, AuthorizedDockerRoute::PersistentArchive) {
                        let (builder, generation) = request_fence
                            .context("persistent BuildKit archive omitted capability generation")?;
                        let container_id = authorized_container_id.context(
                            "persistent BuildKit archive omitted immutable container ID",
                        )?;
                        policy.note_persistent_config_archive(
                            builder,
                            container_id,
                            generation,
                            status,
                            persistent_archive_fingerprint.as_deref().context(
                                "validated persistent config archive fingerprint missing",
                            )?,
                        )?;
                    }
                    if matches!(route, AuthorizedDockerRoute::PersistentExecCreate) {
                        let (builder, generation) = request_fence
                            .context("persistent exec create omitted capability generation")?;
                        let container_id = authorized_container_id
                            .context("persistent exec omitted immutable container ID")?;
                        let result = policy.note_persistent_exec(
                            status,
                            body,
                            builder,
                            container_id,
                            generation,
                            request_readiness_epoch
                                .context("persistent exec create omitted its readiness epoch")?,
                        );
                        finish_resource_reservation_after_observation(
                            &mut resource_reservation,
                            status,
                            result,
                        )?;
                    }
                    match route {
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container) => {
                            let target = authorized_container_id
                                .or(resource_target.as_deref())
                                .context("persistent container route omitted its target")?;
                            policy.record_persistent_container_inspect_fenced(
                                target,
                                status,
                                body,
                                request_fence,
                            )?;
                            if status == 404 {
                                return Ok(());
                            }
                            let id = if let Some(id) = authorized_container_id {
                                id.to_owned()
                            } else {
                                policy.persistent_container_id(target)?
                            };
                            if !policy.is_fresh_persistent_container(&id)? {
                                let readiness = (|| {
                                    let builder = policy.persistent_container_builder(target)?;
                                    let generation =
                                        request_fence.map(|(_, generation)| generation).context(
                                            "persistent inspect omitted capability generation",
                                        )?;
                                    let config_fingerprint =
                                        policy.persistent_builder_config_fingerprint(&builder)?;
                                    let domain =
                                        crate::buildkit::PersistentBuildKitDomain::resolve()
                                            .context(
                                                "resolve domain before reusing BuildKit container",
                                            )?;
                                    accept_reused_persistent_container_readiness(
                                        &policy,
                                        &domain,
                                        &builder,
                                        &id,
                                        generation,
                                        &config_fingerprint,
                                    )
                                })();
                                if let Err(error) = readiness {
                                    policy
                                        .forget_persistent_container_fenced(target, request_fence)
                                        .context(
                                            "forget persistent BuildKit binding after readiness failure",
                                        )?;
                                    return Err(error);
                                }
                            }
                        }
                        AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
                            if request_method == "POST" =>
                        {
                            let (builder, generation) = request_fence
                                .context("persistent start omitted capability generation")?;
                            let id = authorized_container_id
                                .context("persistent start omitted immutable container ID")?;
                            let config_fingerprint =
                                policy.persistent_builder_config_fingerprint(builder)?;
                            if (200..300).contains(&status) || status == 304 {
                                let domain = crate::buildkit::PersistentBuildKitDomain::resolve()?;
                                if let Some(access) =
                                    crate::buildkit::pending_buildkit_create_access(
                                        &domain,
                                        builder,
                                        &config_fingerprint,
                                        generation,
                                    )?
                                {
                                    crate::buildkit::mark_pending_buildkit_create_started(
                                        &domain,
                                        builder,
                                        &access.transaction_id,
                                        id,
                                    )?;
                                }
                                // This helper takes the same Engine/volume
                                // flock to re-attest and persist readiness.
                                // Release the request's preflight descriptor
                                // first, then let the helper reacquire it and
                                // verify the immutable ID under that lock.
                                volume_locks.take();
                                let readiness_epoch = request_readiness_epoch.context(
                                    "persistent start response omitted its readiness epoch",
                                )?;
                                crate::buildkit::persist_builder_readiness_after_start(
                                    &domain,
                                    builder,
                                    id,
                                    &config_fingerprint,
                                    readiness_epoch,
                                )?;
                                note_current_persistent_readiness(
                                    policy.as_ref(),
                                    &domain,
                                    builder,
                                    generation,
                                    id,
                                    &config_fingerprint,
                                )?;
                            }
                        }
                        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume) => {
                            policy.record_persistent_volume_inspect(
                                resource_target
                                    .as_deref()
                                    .context("persistent volume route omitted its target")?,
                                status,
                                body,
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
                            .record_delete_response_fenced(
                                kind,
                                resource_target
                                    .as_deref()
                                    .context("owned delete route omitted its target")?,
                                status,
                                authorized_container_id,
                            )?,
                        _ => {}
                    }
                    Ok(())
                },
            );
            if create_fence.is_some()
                && let Some((_, host_watch)) = host_state.as_ref()
            {
                let _ = host_watch.set_abort_protected(false);
            }
            let reusable = match forwarded {
                Ok(reusable) => reusable,
                Err(error) => {
                    // The request reached the Engine, so an incomplete or
                    // malformed response cannot prove that no object was
                    // created. Keep the slot occupied fail-closed.
                    pin_resource_reservation_after_uncertain_dispatch(&mut resource_reservation);
                    if error.downcast_ref::<GuestClosed>().is_some() {
                        return Ok(());
                    }
                    return Err(error);
                }
            };
            reusable
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
            .record_persistent_volume_inspect(target, status, &body)
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
    request_fence: Option<(&str, u64)>,
) -> Result<VolumeOperationLocks> {
    let builder = persistent_buildkit_builder_name(target)
        .map(str::to_owned)
        .or_else(|| policy.persistent_container_builder(target).ok())
        .context("persistent container has no active builder association")?;
    let volume = crate::buildkit::daemon_state_volume(&builder);
    if let Ok(recorded) = policy.persistent_container_volume(target)
        && recorded != volume
    {
        bail!("persistent container state volume changed after authorization");
    }
    let expected_id = policy.persistent_container_id(target).ok();
    let names = BTreeSet::from([volume.clone()]);
    let (fenced_builder, generation) =
        request_fence.context("persistent container preflight omitted capability generation")?;
    if fenced_builder != builder {
        bail!("persistent container preflight builder differs from capability owner");
    }
    let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
        .context("resolve selected BuildKit domain for container preflight")?;
    if persistent_buildkit_domain_token(&builder) != Some(domain.token.as_str()) {
        bail!("persistent container preflight resolved another BuildKit domain");
    }
    let config_fingerprint = policy.persistent_builder_config_fingerprint(&builder)?;
    let create_access = crate::buildkit::pending_buildkit_create_access(
        &domain,
        &builder,
        &config_fingerprint,
        generation,
    )?;
    let locks = policy.lock_volume_names_with_create_access(
        &names,
        Some(&domain),
        create_access.as_ref(),
    )?;
    preflight_volume_identity(
        policy,
        host_socket,
        &volume,
        AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Volume),
        job_id,
        daemon_id,
    )?;
    let inspect_target = expected_id.as_deref().unwrap_or(target);
    let (status, body) = inspect_container_on_host(host_socket, inspect_target)?;
    if status == 404 && expected_id.is_none() {
        return Ok(locks);
    }
    if !(200..300).contains(&status) {
        bail!("persistent BuildKit container re-attestation returned HTTP {status}");
    }
    let allowed = policy.persistent_builder_names_for_attestation()?;
    let (name, id, attested_volume, _, _) = attest_persistent_buildkit_container(
        &body,
        inspect_target,
        &allowed,
        &policy.persistent_builder_images()?,
    )?;
    if persistent_buildkit_builder_name(&name) != Some(builder.as_str())
        || attested_volume != volume
        || expected_id
            .as_deref()
            .is_some_and(|expected| expected != id)
    {
        bail!("persistent BuildKit container changed after authorization");
    }
    Ok(locks)
}

#[cfg(unix)]
fn preflight_container_mounts(
    policy: &DockerLeasePolicy,
    host_socket: &Path,
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    persistence_domain: Option<&crate::buildkit::PersistentBuildKitDomain>,
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
    let has_persistent_volume = names
        .iter()
        .any(|name| is_persistent_buildkit_volume_object(name));
    let locks = if has_persistent_volume {
        let domain = persistence_domain
            .context("persistent BuildKit mount preflight omitted its resolved domain")?;
        policy.lock_volume_names_with_create_access(&names, Some(domain), None)?
    } else {
        policy.lock_volume_names(&names)?
    };
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
pub(crate) fn inspect_container_on_host(
    host_socket: &Path,
    target: &str,
) -> Result<(u16, Vec<u8>)> {
    inspect_object_on_host(host_socket, "containers", target, "container")
}

#[cfg(unix)]
fn observe_persistent_bootstrap_response_with(
    policy: &DockerLeasePolicy,
    target: &str,
    status: u16,
    body: &[u8],
    inspect: impl FnMut(&str) -> Result<(u16, Vec<u8>)>,
    mut start_conflict: impl FnMut(&DockerLeasePolicy, &str) -> Result<()>,
) -> Result<()> {
    observe_persistent_bootstrap_response_fenced(
        policy,
        target,
        status,
        body,
        None,
        inspect,
        |policy, target, _| start_conflict(policy, target),
    )
}

#[cfg(unix)]
fn observe_persistent_bootstrap_response_fenced(
    policy: &DockerLeasePolicy,
    target: &str,
    status: u16,
    body: &[u8],
    request_fence: Option<(&str, u64)>,
    inspect: impl FnMut(&str) -> Result<(u16, Vec<u8>)>,
    mut start_conflict: impl FnMut(&DockerLeasePolicy, &str, Option<(&str, u64)>) -> Result<()>,
) -> Result<()> {
    observe_persistent_bootstrap_response_fenced_with_binding(
        policy,
        target,
        status,
        body,
        request_fence,
        inspect,
        |_, _| Ok(()),
        |policy, target, fence| start_conflict(policy, target, fence),
    )
}

#[cfg(unix)]
fn observe_persistent_bootstrap_response_fenced_with_binding(
    policy: &DockerLeasePolicy,
    target: &str,
    status: u16,
    body: &[u8],
    request_fence: Option<(&str, u64)>,
    mut inspect: impl FnMut(&str) -> Result<(u16, Vec<u8>)>,
    mut bind_transaction: impl FnMut(&str, &[u8]) -> Result<()>,
    mut start_conflict: impl FnMut(&DockerLeasePolicy, &str, Option<(&str, u64)>) -> Result<()>,
) -> Result<()> {
    if status == 409 {
        let (inspect_status, inspect_body) = match inspect(target) {
            Ok(result) => result,
            Err(error) => {
                policy.forget_persistent_container_fenced(target, request_fence)?;
                return Err(error).with_context(|| {
                    format!("inspect conflicting persistent BuildKit container {target}")
                });
            }
        };
        if !(200..300).contains(&inspect_status) {
            policy.record_persistent_container_inspect_fenced(
                target,
                inspect_status,
                &inspect_body,
                request_fence,
            )?;
            bail!(
                "conflicting persistent BuildKit container {target} failed host attestation (HTTP {inspect_status})"
            );
        }
        policy.record_persistent_container_inspect_fenced(
            target,
            inspect_status,
            &inspect_body,
            request_fence,
        )?;
        bind_transaction(target, &inspect_body)?;
        return start_conflict(policy, target, request_fence);
    }

    let candidate = policy.note_persistent_container_candidate(status, body, request_fence)?;
    let (inspect_status, inspect_body) = match inspect(&candidate) {
        Ok(result) => result,
        Err(error) => {
            policy.forget_persistent_container_fenced(&candidate, request_fence)?;
            return Err(error).with_context(|| {
                format!("inspect created persistent BuildKit container {target}")
            });
        }
    };
    if !(200..300).contains(&inspect_status) {
        policy.forget_persistent_container_fenced(&candidate, request_fence)?;
        policy.record_persistent_container_inspect_fenced(
            target,
            inspect_status,
            &inspect_body,
            request_fence,
        )?;
        bail!(
            "created persistent BuildKit container {target} failed host attestation (HTTP {inspect_status})"
        );
    }
    if let Err(error) = policy.record_persistent_container_inspect_fenced(
        target,
        inspect_status,
        &inspect_body,
        request_fence,
    ) {
        policy.forget_persistent_container_fenced(&candidate, request_fence)?;
        return Err(error);
    }
    let id = policy.persistent_container_id(target)?;
    bind_transaction(&id, &inspect_body)?;
    if let Some((builder, generation)) = request_fence {
        policy.note_fresh_persistent_container(builder, generation, &id)?;
        let domain = crate::buildkit::PersistentBuildKitDomain::resolve()
            .context("resolve persistent BuildKit creator domain after inspect")?;
        let config_fingerprint = policy.persistent_builder_config_fingerprint(builder)?;
        crate::buildkit::bind_persistent_builder_creator_container(
            &domain,
            builder,
            generation,
            &config_fingerprint,
            &id,
        )?;
    }
    Ok(())
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
    let mut header_start = 0;
    let (status, header_start, header_end) = loop {
        if let Some(index) = response[header_start..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let header_end = header_start + index + 4;
            if header_end > MAX_PROXY_HEADER {
                bail!("Docker object re-attestation headers exceed lease proxy limit");
            }
            let header = std::str::from_utf8(&response[header_start..header_end])
                .context("Docker object re-attestation headers must be UTF-8")?;
            let status_line = header
                .split("\r\n")
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
            if !(100..=599).contains(&status) {
                bail!("Docker object re-attestation has invalid HTTP status {status}");
            }
            if (100..200).contains(&status) && status != 101 {
                header_start = header_end;
                continue;
            }
            if status == 101 {
                bail!("Docker object re-attestation unexpectedly switched protocols");
            }
            break (status, header_start, header_end);
        }
        if response.len() >= MAX_PROXY_HEADER {
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
    let header = std::str::from_utf8(&response[header_start..header_end])
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
    let parsed_status = status_parts
        .next()
        .context("Docker object re-attestation status code")?
        .parse::<u16>()
        .context("parse Docker object re-attestation status code")?;
    if parsed_status != status {
        bail!("Docker object re-attestation status changed while parsing headers");
    }
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
    mut client: std::os::unix::net::UnixStream,
    host_preface: &[u8],
) -> Result<()> {
    if !host_preface.is_empty() {
        client
            .write_all(host_preface)
            .context("forward buffered Docker upgrade response")?;
    }
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

/// Docker upgrade response already read through its header terminator. Bytes
/// after the terminator may be the beginning of the hijacked stream.
#[cfg(unix)]
struct BufferedUpgradeResponse {
    status: u16,
    bytes: Vec<u8>,
    header_end: usize,
}

/// Read the complete Docker upgrade response headers. The caller must only
/// treat status 101 as a hijacked stream; other statuses stay framed HTTP.
#[cfg(unix)]
fn read_upgrade_response(
    host: &mut std::os::unix::net::UnixStream,
    client: &mut std::os::unix::net::UnixStream,
) -> Result<BufferedUpgradeResponse> {
    read_upgrade_response_with_timeout(host, client, Duration::from_secs(30))
}

#[cfg(unix)]
fn read_upgrade_response_with_timeout(
    host: &mut std::os::unix::net::UnixStream,
    client: &mut std::os::unix::net::UnixStream,
    timeout: Duration,
) -> Result<BufferedUpgradeResponse> {
    let deadline = Instant::now() + timeout;
    let mut response = Vec::new();
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    let mut header_start = 0;
    loop {
        if let Some(index) = response[header_start..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let header_end = header_start + index + 4;
            if header_end > MAX_PROXY_HEADER {
                bail!("Docker upgrade response headers exceed lease proxy limit");
            }
            let header_text = std::str::from_utf8(&response[header_start..header_end])
                .context("Docker upgrade response headers must be UTF-8")?;
            let status_line = header_text
                .split("\r\n")
                .next()
                .context("Docker upgrade response omitted status line")?;
            let mut status_parts = status_line.split_ascii_whitespace();
            let version = status_parts
                .next()
                .context("Docker upgrade response omitted HTTP version")?;
            if !version.eq_ignore_ascii_case("HTTP/1.0")
                && !version.eq_ignore_ascii_case("HTTP/1.1")
            {
                bail!("Docker upgrade response has an unsupported status line");
            }
            let status = status_parts
                .next()
                .context("Docker upgrade response omitted status code")?
                .parse::<u16>()
                .context("parse Docker upgrade response status code")?;
            if !(100..=599).contains(&status) {
                bail!("Docker upgrade response has an invalid status code");
            }
            if (100..200).contains(&status) && status != 101 {
                header_start = header_end;
                continue;
            }
            return Ok(BufferedUpgradeResponse {
                status,
                bytes: response,
                header_end,
            });
        }
        if response.len() > MAX_PROXY_HEADER {
            bail!("Docker upgrade response headers exceed lease proxy limit");
        }
        wait_for_host_response_until(host, client, Some(deadline))?;
        let read = host
            .read(&mut scratch)
            .context("read Docker upgrade response headers")?;
        if read == 0 {
            bail!("Docker host closed before upgrade response headers finished");
        }
        response.extend_from_slice(&scratch[..read]);
    }
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
    detach_client_on_hup: bool,
    observe_after_delivery_on_success: bool,
    detach_signal: Option<Arc<AtomicBool>>,
    persistent_image_ids: BTreeSet<String>,
}

#[cfg(unix)]
#[derive(Debug)]
struct ResponseDelivery {
    client_connected: bool,
    detach_client_on_hup: bool,
    detach_signal: Option<Arc<AtomicBool>>,
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
    forward_http_response_with_delivery(
        host,
        host_buffer,
        client,
        request_method,
        options,
        |status, body, _client_connected| observe(status, body),
    )
}

#[cfg(unix)]
fn forward_http_response_with_delivery(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    options: ForwardResponseOptions,
    mut observe: impl FnMut(u16, &[u8], bool) -> Result<()>,
) -> Result<bool> {
    let mut delivery = ResponseDelivery {
        client_connected: true,
        detach_client_on_hup: options.detach_client_on_hup,
        detach_signal: options.detach_signal,
    };
    loop {
        let head =
            read_http_response_head(host, host_buffer, client, request_method, &mut delivery)?;
        if head.no_body {
            if (100..200).contains(&head.status) && head.status != 101 {
                // Informational responses are not the operation result. They
                // must reach the guest so it can continue waiting for the
                // final response, but must never run a deferred observer.
                if delivery.client_connected {
                    if let Err(error) = client.write_all(&head.bytes) {
                        if delivery.detach_client_on_hup {
                            delivery.client_connected = false;
                        } else {
                            return Err(error).context(
                                "forward Docker API informational response through job lease",
                            );
                        }
                    }
                }
                continue;
            }
            if options.defer_response_until_observed {
                refresh_response_delivery(client, &mut delivery)?;
                let observe_after_delivery =
                    options.observe_after_delivery_on_success && (200..300).contains(&head.status);
                if observe_after_delivery {
                    if delivery.client_connected
                        && let Err(error) = client.write_all(&head.bytes)
                    {
                        if delivery.detach_client_on_hup {
                            delivery.client_connected = false;
                        } else {
                            return Err(error)
                                .context("forward Docker API response headers through job lease");
                        }
                    }
                    refresh_response_delivery(client, &mut delivery)?;
                    observe(head.status, &[], delivery.client_connected)?;
                } else {
                    if let Err(error) = observe(head.status, &[], delivery.client_connected) {
                        if delivery.client_connected {
                            write_observer_error_response(client, &error)?;
                        }
                        return Err(error);
                    }
                    // Deferred operations publish no success status until its
                    // observer has authorized the completed Engine response.
                    if delivery.client_connected {
                        client
                            .write_all(&head.bytes)
                            .context("forward Docker API response headers through job lease")?;
                    }
                }
            } else {
                client
                    .write_all(&head.bytes)
                    .context("forward Docker API response headers through job lease")?;
                observe(head.status, &[], delivery.client_connected)?;
            }
            return Ok(delivery.client_connected && !head.close && head.status != 101);
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
            observe(head.status, raw_body, delivery.client_connected)?;
            let redacted_body = if options.redact_persistent_container_inspect {
                project_persistent_container_inspect(raw_body)?
            } else {
                project_persistent_image_inspect(raw_body, &options.persistent_image_ids)?
            };
            debug_assert_eq!(redacted_body.len(), content_length);
            if delivery.client_connected {
                client
                    .write_all(&head.bytes)
                    .context("forward Docker API response headers through job lease")?;
                client
                    .write_all(&redacted_body)
                    .context("forward redacted Docker API response body through job lease")?;
            }
            return Ok(!head.close && head.status != 101);
        }
        if options.defer_response_until_observed {
            let mut captured = Some(Vec::new());
            let framed_body = if head.chunked {
                Some(forward_chunked_response_deferred(
                    host,
                    host_buffer,
                    client,
                    &mut captured,
                    &mut delivery,
                )?)
            } else {
                let content_length = head.content_length.context(
                    "persistent BuildKit bootstrap response has no bounded body framing",
                )?;
                if content_length > MAX_CREATE_RESPONSE_BODY {
                    bail!("persistent BuildKit bootstrap response exceeds capture limit");
                }
                forward_exact_response_body_captured_with_delivery(
                    host,
                    host_buffer,
                    client,
                    content_length,
                    &mut captured,
                    false,
                    &mut delivery,
                )?;
                None
            };
            refresh_response_delivery(client, &mut delivery)?;
            let body = captured.as_deref().unwrap_or(&[]);
            let framed_body = framed_body.as_deref().unwrap_or(body);
            let observe_after_delivery =
                options.observe_after_delivery_on_success && (200..300).contains(&head.status);
            if observe_after_delivery {
                if delivery.client_connected {
                    let response_write = client
                        .write_all(&head.bytes)
                        .and_then(|()| client.write_all(framed_body));
                    if let Err(error) = response_write {
                        if delivery.detach_client_on_hup {
                            delivery.client_connected = false;
                        } else {
                            return Err(error).context("forward Docker API response to job lease");
                        }
                    }
                }
                refresh_response_delivery(client, &mut delivery)?;
                observe(head.status, body, delivery.client_connected)?;
            } else {
                if let Err(error) = observe(head.status, body, delivery.client_connected) {
                    if delivery.client_connected {
                        write_observer_error_response(client, &error)?;
                    }
                    return Err(error);
                }
                if delivery.client_connected {
                    client
                        .write_all(&head.bytes)
                        .context("forward Docker API response headers through job lease")?;
                    client
                        .write_all(framed_body)
                        .context("forward Docker API response body through job lease")?;
                }
            }
            return Ok(delivery.client_connected && !head.close && head.status != 101);
        }
        if delivery.client_connected {
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
        }
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
            observe(head.status, &[], delivery.client_connected)?;
            return Ok(false);
        }
        if (100..200).contains(&head.status) && head.status != 101 {
            continue;
        }
        let body = captured.as_deref().unwrap_or(&[]);
        observe(head.status, body, delivery.client_connected)?;
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
    delivery: &mut ResponseDelivery,
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
        wait_for_host_response_delivery(host, client, delivery)?;
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
    remaining: usize,
    captured: &mut Option<Vec<u8>>,
    forward_to_client: bool,
) -> Result<()> {
    forward_exact_response_body_captured_inner(
        host,
        buffered,
        client,
        remaining,
        captured,
        forward_to_client,
        None,
    )
}

#[cfg(unix)]
fn forward_exact_response_body_captured_with_delivery(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    remaining: usize,
    captured: &mut Option<Vec<u8>>,
    forward_to_client: bool,
    delivery: &mut ResponseDelivery,
) -> Result<()> {
    forward_exact_response_body_captured_inner(
        host,
        buffered,
        client,
        remaining,
        captured,
        forward_to_client,
        Some(delivery),
    )
}

#[cfg(unix)]
fn forward_exact_response_body_captured_inner(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    mut remaining: usize,
    captured: &mut Option<Vec<u8>>,
    forward_to_client: bool,
    mut delivery: Option<&mut ResponseDelivery>,
) -> Result<()> {
    if !buffered.is_empty() && remaining != 0 {
        let take = remaining.min(buffered.len());
        capture_response_bytes(captured, &buffered.as_slice()[..take])?;
        if forward_to_client
            && delivery
                .as_deref()
                .is_none_or(|state| state.client_connected)
        {
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
        if let Some(delivery) = delivery.as_deref_mut() {
            wait_for_host_response_delivery(host, client, delivery)?;
        } else {
            wait_for_host_response(host, client)?;
        }
        let read = host
            .read(&mut scratch[..read_len])
            .context("read Docker API response body")?;
        if read == 0 {
            bail!("host Docker API closed before response body finished");
        }
        capture_response_bytes(captured, &scratch[..read])?;
        if forward_to_client
            && delivery
                .as_deref()
                .is_none_or(|state| state.client_connected)
        {
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
fn read_response_line_with_delivery(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    delivery: &mut ResponseDelivery,
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
        wait_for_host_response_delivery(host, client, delivery)?;
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
fn append_deferred_chunk_wire(wire: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    const MAX_CHUNKED_CREATE_WIRE: usize = MAX_CREATE_RESPONSE_BODY * 6 + MAX_PROXY_HEADER;
    if bytes.len() > MAX_CHUNKED_CREATE_WIRE.saturating_sub(wire.len()) {
        bail!("chunked Docker create response exceeds capture limit");
    }
    wire.extend_from_slice(bytes);
    Ok(())
}

#[cfg(unix)]
fn forward_chunked_response_deferred(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    decoded: &mut Option<Vec<u8>>,
    delivery: &mut ResponseDelivery,
) -> Result<Vec<u8>> {
    let mut wire = Vec::new();
    loop {
        let line = read_response_line_with_delivery(host, buffered, client, delivery)?;
        append_deferred_chunk_wire(&mut wire, &line)?;
        let line_text = std::str::from_utf8(&line[..line.len() - 2])
            .context("Docker API response chunk-size line must be UTF-8")?;
        let size_text = line_text
            .split_once(';')
            .map_or(line_text, |(size, _)| size)
            .trim();
        let size = usize::from_str_radix(size_text, 16)
            .context("parse Docker API response chunk-size line")?;
        if size == 0 {
            loop {
                let trailer = read_response_line_with_delivery(host, buffered, client, delivery)?;
                append_deferred_chunk_wire(&mut wire, &trailer)?;
                if trailer != b"\r\n" {
                    validate_chunked_response_trailer(&trailer)?;
                }
                if trailer == b"\r\n" {
                    return Ok(wire);
                }
            }
        }

        let previous_len = decoded.as_ref().map_or(0, Vec::len);
        forward_exact_response_body_captured_with_delivery(
            host, buffered, client, size, decoded, false, delivery,
        )?;
        let decoded = decoded
            .as_deref()
            .context("capture chunked Docker create response body")?;
        append_deferred_chunk_wire(&mut wire, &decoded[previous_len..])?;
        let terminator = read_response_line_with_delivery(host, buffered, client, delivery)?;
        append_deferred_chunk_wire(&mut wire, &terminator)?;
        if terminator != b"\r\n" {
            bail!("Docker API response chunk is missing its terminating CRLF");
        }
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
    wait_for_host_response_until(host, client, None)
}

#[cfg(unix)]
fn wait_for_host_response_until(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
    deadline: Option<Instant>,
) -> Result<()> {
    let mut delivery = ResponseDelivery {
        client_connected: true,
        detach_client_on_hup: false,
        detach_signal: None,
    };
    wait_for_host_response_until_with_delivery(host, client, deadline, &mut delivery)
}

#[cfg(unix)]
fn wait_for_host_response_delivery(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
    delivery: &mut ResponseDelivery,
) -> Result<()> {
    wait_for_host_response_until_with_delivery(host, client, None, delivery)
}

#[cfg(unix)]
fn wait_for_host_response_until_with_delivery(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
    deadline: Option<Instant>,
    delivery: &mut ResponseDelivery,
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
        if delivery.detach_client_on_hup
            && delivery
                .detach_signal
                .as_ref()
                .is_some_and(|signal| signal.load(Ordering::SeqCst))
        {
            delivery.client_connected = false;
        }
        if !delivery.client_connected {
            poll_fds[1].fd = -1;
            poll_fds[1].events = 0;
        }
        let timeout_ms = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("timed out waiting for Docker lease response");
                }
                // poll(2) accepts millisecond precision. Round the final
                // partial millisecond up, then recheck the absolute deadline.
                remaining
                    .as_millis()
                    .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
                    .clamp(1, i32::MAX as u128) as i32
            }
            None => -1,
        };
        let polled = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, timeout_ms) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error()).context("poll Docker lease response streams");
        }
        if polled == 0 {
            bail!("timed out waiting for Docker lease response");
        }
        if poll_fds[1].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            if delivery.detach_client_on_hup {
                delivery.client_connected = false;
                poll_fds[1].fd = -1;
                poll_fds[1].events = 0;
                if poll_fds[0].revents
                    & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
                    != 0
                {
                    return Ok(());
                }
                continue;
            }
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
fn refresh_response_delivery(
    client: &std::os::unix::net::UnixStream,
    delivery: &mut ResponseDelivery,
) -> Result<()> {
    if !delivery.client_connected || !delivery.detach_client_on_hup {
        return Ok(());
    }
    if delivery
        .detach_signal
        .as_ref()
        .is_some_and(|signal| signal.load(Ordering::SeqCst))
    {
        delivery.client_connected = false;
        return Ok(());
    }
    use std::os::fd::AsRawFd;
    let mut poll_fd = libc::pollfd {
        fd: client.as_raw_fd(),
        events: libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    let polled = unsafe { libc::poll(&mut poll_fd, 1, 0) };
    if polled < 0 {
        if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return Ok(());
        }
        return Err(io::Error::last_os_error()).context("poll Docker lease client delivery");
    }
    if poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        delivery.client_connected = false;
    }
    Ok(())
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

    fn test_persistent_builder(tier: &str) -> String {
        crate::buildkit::persistent_builder_name("", "scope", tier, Some("org/repo"))
    }

    fn register_test_persistent_volume_projection(
        policy: &DockerLeasePolicy,
        volume: &str,
        domain_token: &str,
    ) {
        let projection = [
            serde_json::to_string(volume).unwrap(),
            serde_json::to_string("local").unwrap(),
            serde_json::to_string(&BTreeMap::from([
                (JOB_ID_LABEL.to_owned(), "previous-job".to_owned()),
                (BUILDKIT_DOMAIN_LABEL.to_owned(), domain_token.to_owned()),
            ]))
            .unwrap(),
            serde_json::to_string(&BTreeMap::<String, String>::new()).unwrap(),
        ]
        .join("\t");
        policy
            .record_persistent_volume_projection(volume, projection.as_bytes(), domain_token)
            .unwrap();
    }

    fn test_storage_root(prefix: &str) -> PathBuf {
        let canonical_temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = canonical_temp.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn test_pending_buildkit_create_fence(
        root: &Path,
    ) -> (
        crate::buildkit::PersistentBuildKitDomain,
        crate::buildkit::PersistentBuildKitCreatorLease,
        PersistentBuildKitCreateFence,
        String,
        String,
    ) {
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            root,
            "pending-create-test-storage",
            "pending-create-test-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "test",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let container_name = crate::buildkit::daemon_container_name(&builder);
        let generation = 7;
        let config_fingerprint = "no-config-v1";
        let expected_image_id = format!("sha256:{}", "a".repeat(64));
        let creator = crate::buildkit::begin_persistent_builder_creator_lease(
            &domain,
            &builder,
            config_fingerprint,
            generation,
        )
        .unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "Image": expected_image_id,
            "Cmd": ["buildkitd"],
            "HostConfig": {
                "Privileged": true,
                "Init": true,
                "RestartPolicy": {"Name": "unless-stopped"},
                "Mounts": [{
                    "Type": "volume",
                    "Source": volume,
                    "Target": "/var/lib/buildkit"
                }]
            },
            "Labels": {
                JOB_ID_LABEL: "test-job",
                BUILDKIT_DOMAIN_LABEL: domain.token
            }
        }))
        .unwrap();
        let request = api_request(
            "POST",
            &format!("/containers/create?name={container_name}"),
            &body,
        );
        let fence = PersistentBuildKitCreateFence::begin(
            &domain,
            &builder,
            generation,
            &volume,
            &container_name,
            config_fingerprint,
            &expected_image_id,
            false,
            &request,
        )
        .unwrap();
        (domain, creator, fence, builder, volume)
    }

    #[cfg(unix)]
    struct FakePendingBuildKitRecoveryTransport {
        volume_response: (u16, Vec<u8>),
        container_responses: std::collections::VecDeque<(u16, Vec<u8>)>,
        archived: Vec<String>,
        started: Vec<String>,
        published: Vec<(String, u64)>,
        volume_lock_probe: Option<(PathBuf, String)>,
    }

    #[cfg(unix)]
    impl FakePendingBuildKitRecoveryTransport {
        fn assert_volume_lock_held(&self) {
            if let Some((root, volume)) = &self.volume_lock_probe {
                assert!(
                    try_lock_volume_name_at_for_test(root, volume)
                        .unwrap()
                        .is_none(),
                    "pending-create recovery must hold the cross-process volume lock"
                );
            }
        }

        fn assert_volume_lock_released(&self) {
            if let Some((root, volume)) = &self.volume_lock_probe {
                assert!(
                    try_lock_volume_name_at_for_test(root, volume)
                        .unwrap()
                        .is_some(),
                    "BuildKit worker readiness must be probed without the volume lock"
                );
            }
        }
    }

    #[cfg(unix)]
    impl PendingBuildKitRecoveryTransport for FakePendingBuildKitRecoveryTransport {
        fn inspect_volume(&mut self, target: &str) -> Result<(u16, Vec<u8>)> {
            self.assert_volume_lock_held();
            let value: Value = serde_json::from_slice(&self.volume_response.1)?;
            assert_eq!(value.get("Name").and_then(Value::as_str), Some(target));
            Ok(self.volume_response.clone())
        }

        fn inspect_container(&mut self, target: &str) -> Result<(u16, Vec<u8>)> {
            self.assert_volume_lock_held();
            let response = self
                .container_responses
                .pop_front()
                .context("test recovery has no scripted container inspection")?;
            if response.0 == 200 {
                let value: Value = serde_json::from_slice(&response.1)?;
                let name = value
                    .get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = value.get("Id").and_then(Value::as_str).unwrap_or_default();
                assert!(
                    target == id || target == name.trim_start_matches('/'),
                    "recovery inspected unexpected target {target:?}"
                );
            }
            Ok(response)
        }

        fn upload_archive(&mut self, container_id: &str, config_fingerprint: &str) -> Result<()> {
            self.assert_volume_lock_held();
            assert!(!container_id.is_empty());
            assert!(!config_fingerprint.is_empty());
            self.archived.push(container_id.to_owned());
            Ok(())
        }

        fn start_container(&mut self, container_id: &str) -> Result<()> {
            self.assert_volume_lock_held();
            self.started.push(container_id.to_owned());
            Ok(())
        }

        fn publish_readiness(
            &mut self,
            domain: &crate::buildkit::PersistentBuildKitDomain,
            builder: &str,
            container_id: &str,
            config_fingerprint: &str,
            readiness_epoch: u64,
        ) -> Result<()> {
            self.assert_volume_lock_released();
            crate::buildkit::publish_builder_readiness_for_epoch(
                domain,
                builder,
                container_id,
                config_fingerprint,
                readiness_epoch,
            )?;
            let transaction =
                crate::buildkit::pending_buildkit_create_transaction(domain, builder)?
                    .context("test readiness publication lost pending transaction")?;
            crate::buildkit::finish_pending_buildkit_create_transaction(
                domain,
                builder,
                &transaction.transaction_id,
                container_id,
                config_fingerprint,
            )?;
            self.published
                .push((container_id.to_owned(), readiness_epoch));
            Ok(())
        }
    }

    #[cfg(unix)]
    fn pending_recovery_volume_inspect(
        domain: &crate::buildkit::PersistentBuildKitDomain,
        volume: &str,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "Name": volume,
            "Driver": "local",
            "Labels": {
                JOB_ID_LABEL: "previous-job",
                BUILDKIT_DOMAIN_LABEL: domain.token.as_str(),
            },
            "Options": null,
        }))
        .unwrap()
    }

    #[cfg(unix)]
    fn pending_recovery_container_inspect(
        transaction: &crate::buildkit::PendingBuildKitCreateTransaction,
        container_id: &str,
        state: &str,
        mismatch: bool,
    ) -> Vec<u8> {
        let token = if mismatch {
            "wrong-buildkit-domain"
        } else {
            transaction.domain_token.as_str()
        };
        serde_json::to_vec(&serde_json::json!({
            "Id": container_id,
            "Name": format!("/{}", transaction.container_name),
            "Image": transaction.expected_image_id.as_str(),
            "Config": {
                "Image": transaction.expected_image_id.as_str(),
                "Cmd": ["buildkitd"],
                "Labels": {
                    JOB_ID_LABEL: "previous-job",
                    BUILDKIT_DOMAIN_LABEL: token,
                },
            },
            "HostConfig": {
                "Privileged": true,
                "Init": true,
                "RestartPolicy": {"Name": "unless-stopped", "MaximumRetryCount": 0},
                "Mounts": [{
                    "Type": "volume",
                    "Source": transaction.state_volume.as_str(),
                    "Target": "/var/lib/buildkit",
                }],
                "NetworkMode": "default",
            },
            "Mounts": [{
                "Type": "volume",
                "Name": transaction.state_volume.as_str(),
                "Destination": "/var/lib/buildkit",
                "Driver": "local",
                "Mode": "",
                "RW": true,
                "Propagation": "rprivate",
                "NonRecursive": false,
            }],
            "State": {"Status": state},
        }))
        .unwrap()
    }

    #[cfg(unix)]
    fn pending_recovery_setup(
        prefix: &str,
    ) -> (
        PathBuf,
        crate::buildkit::PersistentBuildKitDomain,
        DockerLeasePolicy,
        u64,
        String,
        String,
        crate::buildkit::PendingBuildKitCreateTransaction,
    ) {
        let root = test_storage_root(prefix);
        let (domain, creator, fence, builder, volume) = test_pending_buildkit_create_fence(&root);
        drop((creator, fence));
        let transaction = crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(
            transaction.phase,
            crate::buildkit::PendingBuildKitCreatePhase::Dispatched
        );
        assert!(transaction.container_id.is_none());

        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "pending-create-recovery-test-job",
            Some(root.clone()),
        )
        .unwrap();
        let generation = policy
            .begin_persistent_builder_setup(&builder, &transaction.config_fingerprint)
            .unwrap();
        policy
            .register_persistent_builder_image(&builder, &transaction.expected_image_id)
            .unwrap();
        (
            root,
            domain,
            policy,
            generation,
            builder,
            volume,
            transaction,
        )
    }

    #[cfg(unix)]
    fn recover_pending_with_test_transport(
        domain: &crate::buildkit::PersistentBuildKitDomain,
        policy: &DockerLeasePolicy,
        generation: u64,
        builder: &str,
        transaction: &crate::buildkit::PendingBuildKitCreateTransaction,
        transport: &mut FakePendingBuildKitRecoveryTransport,
    ) -> Result<Option<String>> {
        recover_pending_buildkit_create_for_setup_with_volume_lock_and_transport(
            policy,
            domain,
            builder,
            generation,
            &transaction.config_fingerprint,
            &domain.engine_id,
            |domain, volume, access| {
                assert_eq!(access.builder, builder);
                assert_eq!(access.transaction_id, transaction.transaction_id);
                policy.lock_volume_names_with_create_access(
                    &BTreeSet::from([volume.to_owned()]),
                    Some(domain),
                    Some(access),
                )
            },
            transport,
        )
    }

    #[cfg(unix)]
    #[test]
    fn docker_lease_drains_late_create_response_after_guard_abort() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let root = test_storage_root("pending-create-late-response");
        let policy = Arc::new(
            DockerLeasePolicy::new_with_volume_lock_root("create-job", Some(root.clone())).unwrap(),
        );
        let (domain, creator, fence, builder, volume) = test_pending_buildkit_create_fence(&root);
        // The durable create record must outlive this process-local handle.
        drop(fence);
        let create_access =
            crate::buildkit::pending_buildkit_create_access(&domain, &builder, "no-config-v1", 7)
                .unwrap()
                .unwrap();
        let volume_locks = policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                Some(&create_access),
            )
            .unwrap();
        let (mut engine, mut host) = UnixStream::pair().unwrap();
        let (mut sink, guest) = UnixStream::pair().unwrap();
        let conns = LeaseConnSet::new(Arc::new(AtomicBool::new(false)));
        let host_watch = conns.watch(&host);
        assert!(host_watch.set_abort_protected(true));
        let client_watch = conns.watch(&sink);
        let detach_signal = Some(Arc::clone(&conns.shutdown));
        conns.abort();

        assert!(try_lock_volume_name_at_for_test(&root, &volume)
            .unwrap()
            .is_none());

        let response = b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1a\r\n{\"Id\":\"late-container-id\"}\r\n0\r\n\r\n";
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let engine_thread = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            engine.write_all(response).unwrap();
        });
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let proxy_thread = std::thread::spawn(move || {
            let result = forward_http_response_with_delivery(
                &mut host,
                &mut ResponseBuffer::default(),
                &mut sink,
                "POST",
                ForwardResponseOptions {
                    defer_response_until_observed: true,
                    detach_client_on_hup: true,
                    observe_after_delivery_on_success: false,
                    detach_signal,
                    ..ForwardResponseOptions::default()
                },
                |status, body, client_connected| {
                    assert_eq!(status, 201);
                    assert_eq!(body, br#"{"Id":"late-container-id"}"#);
                    assert!(!client_connected, "aborted guest must stay detached");
                    Ok(())
                },
            );
            drop(volume_locks);
            drop((host_watch, client_watch));
            finished_tx
                .send(result.map_err(|error| format!("{error:#}")))
                .unwrap();
        });
        drop(guest);
        release_tx.send(()).unwrap();
        let result = finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .is_some()
        );
        assert!(policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                None,
            )
            .is_err());
        assert!(try_lock_volume_name_at_for_test(&root, &volume)
            .unwrap()
            .is_some());
        engine_thread.join().unwrap();
        proxy_thread.join().unwrap();
        drop(creator);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn truncated_create_response_leaves_durable_fence_and_blocks_volume_lock() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let root = test_storage_root("pending-create-truncated-response");
        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "truncated-create-job",
            Some(root.clone()),
        )
        .unwrap();
        let (domain, _creator, _fence, builder, volume) = test_pending_buildkit_create_fence(&root);
        let create_access =
            crate::buildkit::pending_buildkit_create_access(&domain, &builder, "no-config-v1", 7)
                .unwrap()
                .unwrap();
        let locks = policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                Some(&create_access),
            )
            .unwrap();
        let (mut engine, mut host) = UnixStream::pair().unwrap();
        let (_guest, mut sink) = UnixStream::pair().unwrap();
        engine
            .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 40\r\nConnection: close\r\n\r\n{\"Id\":\"short\"}")
            .unwrap();
        drop(engine);
        let result = forward_http_response_with_delivery(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut sink,
            "POST",
            ForwardResponseOptions {
                defer_response_until_observed: true,
                detach_client_on_hup: true,
                observe_after_delivery_on_success: true,
                ..ForwardResponseOptions::default()
            },
            |_, _, _| panic!("truncated response is not a settled create"),
        );
        assert!(result.is_err());
        drop(locks);
        assert!(policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.to_owned()]),
                Some(&domain),
                None,
            )
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn persistent_bootstrap_mount_preflight_checks_create_fence_before_inspect() {
        let root = test_storage_root("pending-create-mount-preflight");
        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "pending-create-mount-preflight-job",
            Some(root.clone()),
        )
        .unwrap();
        let (domain, _creator, _fence, _builder, volume) =
            test_pending_buildkit_create_fence(&root);
        register_test_persistent_volume_projection(&policy, &volume, &domain.token);
        let body = serde_json::json!({
            "Image": "moby/buildkit:buildx-stable-1",
            "HostConfig": {
                "Mounts": [{
                    "Type": "volume",
                    "Source": volume,
                    "Target": "/var/lib/buildkit",
                }],
            },
        })
        .to_string();
        let request = api_request(
            "POST",
            "/containers/create?name=buildx_buildkit_test0",
            body.as_bytes(),
        );

        let error = preflight_container_mounts(
            &policy,
            &root.join("engine.sock"),
            &request,
            "pending-create-mount-preflight-job",
            "daemon-a",
            Some(&domain),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("create transaction remains unresolved"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unresolved_create_fence_fails_closed_without_response_evidence() {
        let root = test_storage_root("pending-create-fail-closed");
        let (domain, creator, fence, builder, volume) = test_pending_buildkit_create_fence(&root);
        let transaction = crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
            .unwrap()
            .unwrap();
        let journal = serde_json::to_value(&transaction).unwrap();
        assert_eq!(journal["engine_id"], domain.engine_id);
        assert_eq!(journal["builder"], builder);
        assert_eq!(journal["generation"], 7);
        assert_eq!(journal["state_volume"], volume);
        assert_eq!(
            journal["container_name"],
            crate::buildkit::daemon_container_name(&builder)
        );
        assert_eq!(journal["phase"], "dispatched");
        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "fail-closed-create-job",
            Some(root.clone()),
        )
        .unwrap();
        let error = policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("remains unresolved"));
        drop((fence, creator));
        assert!(
            policy
                .lock_volume_names_with_create_access(
                    &BTreeSet::from([volume]),
                    Some(&domain),
                    None,
                )
                .is_err(),
            "dropping creator and response leases must retain the transaction"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_create_recovers_exact_created_container_without_saved_id() {
        let (root, domain, policy, generation, builder, volume, transaction) =
            pending_recovery_setup("pending-create-recover-created");
        let container_id = "a".repeat(64);
        let inspect =
            pending_recovery_container_inspect(&transaction, &container_id, "created", false);
        let mut transport = FakePendingBuildKitRecoveryTransport {
            volume_response: (200, pending_recovery_volume_inspect(&domain, &volume)),
            container_responses: std::collections::VecDeque::from([
                (200, inspect.clone()),
                (200, inspect),
            ]),
            archived: Vec::new(),
            started: Vec::new(),
            published: Vec::new(),
            volume_lock_probe: Some((root.clone(), volume.clone())),
        };

        let recovered = recover_pending_with_test_transport(
            &domain,
            &policy,
            generation,
            &builder,
            &transaction,
            &mut transport,
        )
        .unwrap()
        .unwrap();

        assert_eq!(recovered, container_id);
        assert_eq!(transport.archived, vec![container_id.clone()]);
        assert_eq!(transport.started, vec![container_id.clone()]);
        assert_eq!(transport.published.len(), 1);
        assert_eq!(transport.published[0].0, container_id);
        assert!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .is_none()
        );
        assert!(crate::buildkit::builder_readiness_matches(
            &domain,
            &builder,
            &recovered,
            &transaction.config_fingerprint,
        )
        .unwrap());
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_create_404_retains_fence_then_recovers_when_container_appears() {
        let (root, domain, policy, generation, builder, volume, transaction) =
            pending_recovery_setup("pending-create-404-later-appears");
        let original = transaction.clone();
        let container_id = "b".repeat(64);
        let mut absent_transport = FakePendingBuildKitRecoveryTransport {
            volume_response: (200, pending_recovery_volume_inspect(&domain, &volume)),
            container_responses: std::collections::VecDeque::from([(404, Vec::new())]),
            archived: Vec::new(),
            started: Vec::new(),
            published: Vec::new(),
            volume_lock_probe: Some((root.clone(), volume.clone())),
        };

        let first = recover_pending_with_test_transport(
            &domain,
            &policy,
            generation,
            &builder,
            &transaction,
            &mut absent_transport,
        );
        assert!(first.is_err());
        assert!(absent_transport.archived.is_empty());
        assert!(absent_transport.started.is_empty());
        assert!(absent_transport.published.is_empty());
        assert_eq!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .unwrap(),
            original,
            "404 is not proof that an ambiguous ContainerCreate cannot appear later"
        );
        assert!(policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                None,
            )
            .is_err());

        let inspect =
            pending_recovery_container_inspect(&transaction, &container_id, "created", false);
        let mut appeared_transport = FakePendingBuildKitRecoveryTransport {
            volume_response: (200, pending_recovery_volume_inspect(&domain, &volume)),
            container_responses: std::collections::VecDeque::from([
                (200, inspect.clone()),
                (200, inspect),
            ]),
            archived: Vec::new(),
            started: Vec::new(),
            published: Vec::new(),
            volume_lock_probe: Some((root.clone(), volume.clone())),
        };
        let recovered = recover_pending_with_test_transport(
            &domain,
            &policy,
            generation,
            &builder,
            &transaction,
            &mut appeared_transport,
        )
        .unwrap()
        .unwrap();
        assert_eq!(recovered, container_id);
        assert_eq!(appeared_transport.archived, vec![container_id.clone()]);
        assert_eq!(appeared_transport.started, vec![container_id.clone()]);
        assert!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .is_none()
        );
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_create_config_mismatch_retains_fence_without_inspection() {
        let (root, domain, policy, generation, builder, volume, transaction) =
            pending_recovery_setup("pending-create-config-mismatch");
        let original = transaction.clone();
        let mut transport = FakePendingBuildKitRecoveryTransport {
            volume_response: (200, pending_recovery_volume_inspect(&domain, &volume)),
            container_responses: std::collections::VecDeque::new(),
            archived: Vec::new(),
            started: Vec::new(),
            published: Vec::new(),
            volume_lock_probe: Some((root.clone(), volume.clone())),
        };
        let mut lock_acquired = false;

        let result = recover_pending_buildkit_create_for_setup_with_volume_lock_and_transport(
            &policy,
            &domain,
            &builder,
            generation,
            "no-config-v2",
            &domain.engine_id,
            |_, _, _| {
                lock_acquired = true;
                Ok(VolumeOperationLocks::default())
            },
            &mut transport,
        );

        assert!(result.is_err(), "different config intent must fail closed");
        assert!(
            !lock_acquired,
            "mismatched config cannot acquire bypass lock"
        );
        assert!(transport.container_responses.is_empty());
        assert!(transport.archived.is_empty());
        assert!(transport.started.is_empty());
        assert!(transport.published.is_empty());
        assert_eq!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .unwrap(),
            original,
            "config mismatch cannot rewrite or clear the original transaction"
        );
        assert!(policy
            .lock_volume_names_with_create_access(&BTreeSet::from([volume]), Some(&domain), None,)
            .is_err());
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_create_identity_mismatch_retains_fence_without_mutation() {
        let (root, domain, policy, generation, builder, volume, transaction) =
            pending_recovery_setup("pending-create-identity-mismatch");
        let original = transaction.clone();
        let container_id = "c".repeat(64);
        let inspect =
            pending_recovery_container_inspect(&transaction, &container_id, "created", true);
        let mut transport = FakePendingBuildKitRecoveryTransport {
            volume_response: (200, pending_recovery_volume_inspect(&domain, &volume)),
            container_responses: std::collections::VecDeque::from([(200, inspect)]),
            archived: Vec::new(),
            started: Vec::new(),
            published: Vec::new(),
            volume_lock_probe: Some((root.clone(), volume.clone())),
        };

        let result = recover_pending_with_test_transport(
            &domain,
            &policy,
            generation,
            &builder,
            &transaction,
            &mut transport,
        );

        assert!(result.is_err());
        assert!(transport.archived.is_empty());
        assert!(transport.started.is_empty());
        assert!(transport.published.is_empty());
        assert_eq!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .unwrap(),
            original,
            "identity mismatch cannot bind or clear the durable transaction"
        );
        assert!(policy
            .lock_volume_names_with_create_access(&BTreeSet::from([volume]), Some(&domain), None,)
            .is_err());
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn setup_recovery_missing_volume_fails_before_volume_creation_and_keeps_transaction() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixListener;

        let root = test_storage_root("pending-create-setup-missing-volume");
        let (domain, creator, fence, builder, volume) = test_pending_buildkit_create_fence(&root);
        let original = crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
            .unwrap()
            .unwrap();
        drop((creator, fence));

        let policy =
            DockerLeasePolicy::new_with_volume_lock_root("setup-recovery-job", Some(root.clone()))
                .unwrap();
        let config_fingerprint = "no-config-v1";
        let generation = policy
            .begin_persistent_builder_setup(&builder, config_fingerprint)
            .unwrap();
        let socket = root.join("fake-engine.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected_path = format!("/v1.43/volumes/{volume}");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut scratch = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut scratch).unwrap();
                assert_ne!(read, 0, "host closed before volume inspect request arrived");
                request.extend_from_slice(&scratch[..read]);
            }
            assert!(String::from_utf8_lossy(&request)
                .starts_with(&format!("GET {expected_path} HTTP/1.1\r\n")));
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let mut volume_create_called = false;
        let result = recover_pending_buildkit_create_for_setup_with_volume_lock(
            &policy,
            &socket,
            &domain,
            &builder,
            generation,
            config_fingerprint,
            &domain.engine_id,
            |_, requested_volume, access| {
                assert_eq!(requested_volume, volume.as_str());
                assert_eq!(access.builder, builder);
                assert_eq!(access.transaction_id, original.transaction_id);
                Ok(VolumeOperationLocks::default())
            },
        )
        .map(|_| {
            volume_create_called = true;
        });
        server.join().unwrap();

        assert!(
            result.is_err(),
            "missing transaction volume must block setup"
        );
        assert!(
            !volume_create_called,
            "volume creation must follow recovery"
        );
        assert_eq!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .unwrap(),
            original,
            "uncertain recovery must retain the exact durable transaction"
        );
        assert!(
            policy
                .lock_volume_names_with_create_access(
                    &BTreeSet::from([volume]),
                    Some(&domain),
                    None,
                )
                .is_err(),
            "ordinary setup locks stay fenced after failed recovery"
        );
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn setup_without_pending_transaction_still_respects_legacy_create_quarantine() {
        let root = test_storage_root("pending-create-legacy-quarantine");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "legacy-quarantine-test-storage",
            "legacy-quarantine-test-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "test",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let volume = crate::buildkit::daemon_state_volume(&builder);
        crate::buildkit::quarantine_legacy_pending_buildkit_create(
            &domain,
            &volume,
            br#"{"legacy":"marker-without-recovery-proof"}"#,
        )
        .unwrap();

        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "legacy-quarantine-setup-job",
            Some(root.clone()),
        )
        .unwrap();
        let generation = policy
            .begin_persistent_builder_setup(&builder, "no-config-v1")
            .unwrap();
        let recovery = recover_pending_buildkit_create_for_setup_with_volume_lock(
            &policy,
            Path::new("/unused-docker.sock"),
            &domain,
            &builder,
            generation,
            "no-config-v1",
            &domain.engine_id,
            |_, _, _| panic!("quarantined setup must not acquire a volume lock"),
        );
        let mut volume_create_called = false;
        let result =
            crate::executor::ensure_persistent_buildkit_volume_after_recovery(recovery, || {
                volume_create_called = true;
                Ok(())
            });
        let error = result.unwrap_err();

        assert!(error.to_string().contains("quarantined"));
        assert!(
            !volume_create_called,
            "durable-only quarantine must stop executor before Docker volume create"
        );
        assert!(
            crate::buildkit::pending_buildkit_create_transaction(&domain, &builder)
                .unwrap()
                .is_none()
        );
        assert!(
            crate::buildkit::legacy_pending_buildkit_create_is_quarantined(&domain, &volume)
                .unwrap()
        );
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn setup_migrates_runtime_create_marker_before_volume_create_and_survives_runtime_loss() {
        let root = test_storage_root("pending-create-runtime-marker-migration");
        let identity_root = root.join("storage");
        let runtime_root = root.join("run");
        std::fs::create_dir_all(&identity_root).unwrap();
        std::fs::create_dir_all(&runtime_root).unwrap();
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &identity_root,
            "runtime-marker-storage",
            "runtime-marker-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "test",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let marker_name = pending_buildkit_create_marker_name(&volume);
        let marker_bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "engine_id": domain.engine_id.clone(),
            "builder": builder.clone(),
            "generation": 1,
            "volume": volume.clone(),
            "container_name": crate::buildkit::daemon_container_name(&builder),
            "request_sha256": "a".repeat(64),
        }))
        .unwrap();
        std::fs::write(runtime_root.join(&marker_name), &marker_bytes).unwrap();

        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "runtime-marker-migration-job",
            Some(runtime_root.clone()),
        )
        .unwrap();
        let generation = policy
            .begin_persistent_builder_setup(&builder, "no-config-v1")
            .unwrap();
        // Match executor ordering: recovery returns None when there is no
        // durable journal/quarantine, then the domain-aware volume lock must
        // migrate the old runtime marker before the Docker create closure.
        let recovery = recover_pending_buildkit_create_for_setup_with_volume_lock(
            &policy,
            Path::new("/unused-docker.sock"),
            &domain,
            &builder,
            generation,
            "no-config-v1",
            &domain.engine_id,
            |_, _, _| panic!("no durable transaction means no recovery lock"),
        );
        assert!(recovery.as_ref().unwrap().is_none());
        let mut volume_create_called = false;
        let result =
            crate::executor::ensure_persistent_buildkit_volume_after_recovery(recovery, || {
                let volume_lock = policy.lock_volume_names_with_create_access(
                    &BTreeSet::from([volume.clone()]),
                    Some(&domain),
                    None,
                );
                crate::executor::run_persistent_buildkit_volume_operation(volume_lock, || {
                    volume_create_called = true;
                    Ok(())
                })
            });
        assert!(result.unwrap_err().to_string().contains("quarantined"));
        assert!(
            !volume_create_called,
            "migrated runtime marker must block executor volume creation"
        );
        assert!(!runtime_root.join(&marker_name).exists());
        assert!(
            crate::buildkit::legacy_pending_buildkit_create_is_quarantined(&domain, &volume)
                .unwrap()
        );

        // `/run` can disappear at reboot; the selected durable storage root
        // retains the quarantine and blocks the next setup without the old
        // marker file.
        drop(policy);
        std::fs::remove_dir_all(&runtime_root).unwrap();
        std::fs::create_dir_all(&runtime_root).unwrap();
        let restarted_policy = DockerLeasePolicy::new_with_volume_lock_root(
            "runtime-marker-migration-restarted-job",
            Some(runtime_root.clone()),
        )
        .unwrap();
        let volume_lock = restarted_policy.lock_volume_names_with_create_access(
            &BTreeSet::from([volume.clone()]),
            Some(&domain),
            None,
        );
        let result = crate::executor::run_persistent_buildkit_volume_operation(volume_lock, || {
            volume_create_called = true;
            Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("quarantined"));
        assert!(
            !volume_create_called,
            "durable quarantine must survive runtime-root loss"
        );
        drop(restarted_policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn mismatched_runtime_create_marker_stays_unmodified_and_unquarantined() {
        for mismatch in ["engine", "domain", "volume", "builder", "container"] {
            let suffix = format!("pending-create-runtime-marker-wrong-{mismatch}");
            let root = test_storage_root(&suffix);
            let identity_root = root.join("storage");
            let runtime_root = root.join("run");
            std::fs::create_dir_all(&identity_root).unwrap();
            std::fs::create_dir_all(&runtime_root).unwrap();
            let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
                &identity_root,
                "runtime-marker-storage",
                "runtime-marker-engine",
            )
            .unwrap();
            let requested_builder = crate::buildkit::persistent_builder_name_for_domain(
                &domain.token,
                "test",
                "scope",
                crate::buildkit::TRUST_TIER_BRANCH,
                Some("org/repo"),
            );
            let volume = crate::buildkit::daemon_state_volume(&requested_builder);
            let marker_builder = match mismatch {
                "domain" => {
                    let foreign_root = root.join("foreign-storage");
                    std::fs::create_dir_all(&foreign_root).unwrap();
                    let foreign = crate::buildkit::PersistentBuildKitDomain::from_identities(
                        &foreign_root,
                        "other-storage",
                        domain.engine_id.as_str(),
                    )
                    .unwrap();
                    crate::buildkit::persistent_builder_name_for_domain(
                        &foreign.token,
                        "test",
                        "scope",
                        crate::buildkit::TRUST_TIER_BRANCH,
                        Some("org/repo"),
                    )
                }
                "builder" => crate::buildkit::persistent_builder_name_for_domain(
                    &domain.token,
                    "other-builder",
                    "scope",
                    crate::buildkit::TRUST_TIER_BRANCH,
                    Some("org/repo"),
                ),
                _ => requested_builder.clone(),
            };
            let marker_name = pending_buildkit_create_marker_name(&volume);
            let marker_container = if mismatch == "container" {
                "buildx_buildkit_wrong-container0".to_owned()
            } else {
                crate::buildkit::daemon_container_name(&marker_builder)
            };
            let marker_engine = if mismatch == "engine" {
                "different-engine"
            } else {
                domain.engine_id.as_str()
            };
            let marker_volume = if mismatch == "volume" {
                "buildx_buildkit_other-builder0_state"
            } else {
                volume.as_str()
            };
            let marker_bytes = serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "engine_id": marker_engine,
                "builder": marker_builder.clone(),
                "generation": 1,
                "volume": marker_volume,
                "container_name": marker_container,
                "request_sha256": "a".repeat(64),
            }))
            .unwrap();
            let marker_path = runtime_root.join(&marker_name);
            std::fs::write(&marker_path, &marker_bytes).unwrap();

            let policy = DockerLeasePolicy::new_with_volume_lock_root(
                "runtime-marker-mismatch-job",
                Some(runtime_root),
            )
            .unwrap();
            let mut volume_create_called = false;
            let volume_lock = policy.lock_volume_names_with_create_access(
                &BTreeSet::from([volume.clone()]),
                Some(&domain),
                None,
            );
            let result =
                crate::executor::run_persistent_buildkit_volume_operation(volume_lock, || {
                    volume_create_called = true;
                    Ok(())
                });
            assert!(result.is_err());
            assert!(!volume_create_called);
            assert_eq!(std::fs::read(marker_path).unwrap(), marker_bytes);
            assert!(
                !crate::buildkit::legacy_pending_buildkit_create_is_quarantined(&domain, &volume)
                    .unwrap(),
                "a mismatched marker cannot be attributed to this domain/volume"
            );
            drop(policy);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn stale_quarantine_cannot_authorize_removing_a_different_runtime_marker() {
        let root = test_storage_root("pending-create-stale-marker-quarantine");
        let identity_root = root.join("storage");
        let runtime_root = root.join("run");
        std::fs::create_dir_all(&identity_root).unwrap();
        std::fs::create_dir_all(&runtime_root).unwrap();
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &identity_root,
            "runtime-marker-storage",
            "runtime-marker-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "test",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let marker_bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "engine_id": domain.engine_id.clone(),
            "builder": builder.clone(),
            "generation": 1,
            "volume": volume.clone(),
            "container_name": crate::buildkit::daemon_container_name(&builder),
            "request_sha256": "a".repeat(64),
        }))
        .unwrap();
        let marker_path = runtime_root.join(pending_buildkit_create_marker_name(&volume));
        std::fs::write(&marker_path, &marker_bytes).unwrap();
        crate::buildkit::quarantine_legacy_pending_buildkit_create(
            &domain,
            &volume,
            b"different older marker bytes",
        )
        .unwrap();

        let policy = DockerLeasePolicy::new_with_volume_lock_root(
            "stale-quarantine-marker-job",
            Some(runtime_root),
        )
        .unwrap();
        let mut volume_create_called = false;
        let volume_lock = policy.lock_volume_names_with_create_access(
            &BTreeSet::from([volume.clone()]),
            Some(&domain),
            None,
        );
        let result = crate::executor::run_persistent_buildkit_volume_operation(volume_lock, || {
            volume_create_called = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!volume_create_called);
        assert_eq!(std::fs::read(marker_path).unwrap(), marker_bytes);
        drop(policy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pending_create_recovery_matches_inspect_to_normalized_create_intent() {
        let expected = serde_json::json!({
            "create": {
                "Image": "sha256:pinned-image",
                "Cmd": ["buildkitd", "--debug"],
                "Labels": {
                    JOB_ID_LABEL: "<creator-job>",
                    BUILDKIT_DOMAIN_LABEL: "stable-domain"
                },
                "HostConfig": {
                    "Privileged": true,
                    "Init": true,
                    "Mounts": [{"Type": "volume", "Source": "builder_state", "Target": "/var/lib/buildkit"}]
                },
                "NetworkingConfig": {}
            }
        });
        let actual = serde_json::json!({
            "Config": {
                "Image": "sha256:pinned-image",
                "Cmd": ["buildkitd", "--debug"],
                "Labels": {
                    JOB_ID_LABEL: "current-job",
                    BUILDKIT_DOMAIN_LABEL: "stable-domain"
                }
            },
            "HostConfig": {
                "Privileged": true,
                "Init": true,
                "Mounts": [{"Type": "volume", "Source": "builder_state", "Target": "/var/lib/buildkit", "ReadOnly": false}],
                "NetworkMode": "default"
            }
        });
        let actual = actual.as_object().unwrap();
        assert!(pending_create_shape_matches_inspect(&expected, actual).unwrap());

        let mut changed = actual.clone();
        changed
            .get_mut("Config")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "Cmd".into(),
                serde_json::json!(["buildkitd", "--oci-worker-no-process-sandbox"]),
            );
        assert!(!pending_create_shape_matches_inspect(&expected, &changed).unwrap());

        let mut changed = actual.clone();
        changed
            .get_mut("HostConfig")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("Mounts")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]["Source"] = serde_json::json!("other_state");
        assert!(!pending_create_shape_matches_inspect(&expected, &changed).unwrap());
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

    #[cfg(unix)]
    #[test]
    fn volume_lock_namespace_is_tmpdir_independent_and_engine_scoped() {
        use std::sync::mpsc;
        use std::time::Duration;

        let shared_host_root = test_storage_root("velnor-shared-docker-volume-locks");
        let storage_root_a = test_storage_root("velnor-volume-lock-storage-a");
        let storage_root_b = test_storage_root("velnor-volume-lock-storage-b");
        let tmpdir_a = storage_root_a.join("tmp-a");
        let tmpdir_b = storage_root_b.join("tmp-b");
        std::fs::create_dir_all(&tmpdir_a).unwrap();
        std::fs::create_dir_all(&tmpdir_b).unwrap();
        crate::storage::ensure_buildkit_storage_identity(&storage_root_a).unwrap();
        crate::storage::ensure_buildkit_storage_identity(&storage_root_b).unwrap();

        // Distinct storage domains on one Engine still share one host-wide
        // Engine/name lock inode. Different TMPDIRs model systemd PrivateTmp.
        let canonical_lock_namespace =
            shared_host_root.join("canonical-runtime/velnor/docker-volume-locks");
        let root_a =
            docker_volume_lock_root_under(&canonical_lock_namespace, "engine-stable").unwrap();
        let root_b =
            docker_volume_lock_root_under(&canonical_lock_namespace, "engine-stable").unwrap();
        let other_engine_root =
            docker_volume_lock_root_under(&canonical_lock_namespace, "engine-other").unwrap();
        assert_eq!(root_a, root_b);
        assert_ne!(root_a, other_engine_root);
        assert!(!root_a.starts_with(&tmpdir_a));
        assert!(!root_b.starts_with(&tmpdir_b));

        let first =
            DockerLeasePolicy::new_with_volume_lock_root("job-a", Some(root_a.clone())).unwrap();
        let second = DockerLeasePolicy::new_with_volume_lock_root("job-b", Some(root_b)).unwrap();
        let other =
            DockerLeasePolicy::new_with_volume_lock_root("job-other-volume", Some(root_a.clone()))
                .unwrap();
        let first_guard = first
            .lock_volume_names(&BTreeSet::from(["shared-volume".to_owned()]))
            .unwrap();
        let distinct_volume = other
            .lock_volume_names(&BTreeSet::from(["different-volume".to_owned()]))
            .unwrap();
        drop(distinct_volume);
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let guard = second
                .lock_volume_names(&BTreeSet::from(["shared-volume".to_owned()]))
                .unwrap();
            acquired_tx.send(()).unwrap();
            drop(guard);
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "same Engine ID and volume must contend across storage roots and policies"
        );
        drop(first_guard);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();

        std::fs::remove_dir_all(shared_host_root).unwrap();
        std::fs::remove_dir_all(storage_root_a).unwrap();
        std::fs::remove_dir_all(storage_root_b).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unscoped_volume_lock_rejects_persistent_state_volume_names() {
        let root = test_storage_root("velnor-unscoped-persistent-volume-lock");
        let policy =
            DockerLeasePolicy::new_with_volume_lock_root("unscoped-lock-test", Some(root.clone()))
                .unwrap();
        let volume = crate::buildkit::daemon_state_volume(&test_persistent_builder("branch"));

        let error = policy
            .lock_volume_names(&BTreeSet::from([volume.clone()]))
            .unwrap_err();
        assert!(error.to_string().contains("require a resolved domain"));

        let wrong_domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "wrong-storage-domain",
            "wrong-engine-domain",
        )
        .unwrap();
        let error = policy
            .lock_volume_names_with_create_access(
                &BTreeSet::from([volume]),
                Some(&wrong_domain),
                None,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not belong to the resolved domain"));

        let ordinary = policy
            .lock_volume_names(&BTreeSet::from(["ordinary-owned-volume".to_owned()]))
            .unwrap();
        drop(ordinary);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn persistent_conflict_releases_preflight_volume_flock_before_reacquire() {
        let storage_root = test_storage_root("velnor-conflict-volume-lock-release");
        let (domain, creator, _fence, builder, volume) =
            test_pending_buildkit_create_fence(&storage_root);
        let lock_root = docker_volume_lock_root_under(&storage_root, &domain.engine_id).unwrap();
        let policy =
            DockerLeasePolicy::new_with_volume_lock_root("conflict-job", Some(lock_root.clone()))
                .unwrap();
        let create_access =
            crate::buildkit::pending_buildkit_create_access(&domain, &builder, "no-config-v1", 7)
                .unwrap()
                .unwrap();
        let mut locks = Some(
            policy
                .lock_volume_names_with_create_access(
                    &BTreeSet::from([volume.clone()]),
                    Some(&domain),
                    Some(&create_access),
                )
                .unwrap(),
        );

        release_persistent_conflict_volume_lock(201, &mut locks);
        assert!(locks.is_some(), "ordinary create keeps its preflight lock");
        release_persistent_conflict_volume_lock(409, &mut locks);
        assert!(
            locks.is_none(),
            "409 path releases before domain-lock reacquire"
        );
        assert!(!*policy
            .resources
            .lock()
            .unwrap()
            .volume_locks
            .get(&volume)
            .unwrap()
            .held
            .lock()
            .unwrap());

        let directory =
            crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&lock_root).unwrap();
        let file_name = volume_lock_file_name(&volume);
        let file = directory
            .open_or_create_lock_file(OsStr::new(&file_name))
            .unwrap();
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .expect("the volume lock must be available to the re-attesting start helper");
        drop(file);
        drop(creator);
        std::fs::remove_dir_all(storage_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn host_volume_lock_namespace_uses_shared_runtime_root() {
        #[cfg(target_os = "linux")]
        {
            let package_runtime = Path::new("/run/velnor");
            let explicit_runtime = Path::new("/run/user/1000");
            assert_eq!(
                shared_host_volume_lock_namespace(Path::new("/var/lib/velnor"), None).unwrap(),
                Path::new("/run/velnor/docker-volume-locks")
            );
            assert_eq!(
                shared_host_volume_lock_namespace(
                    Path::new("/explicit/daemon-config"),
                    Some(explicit_runtime),
                )
                .unwrap(),
                shared_host_volume_lock_namespace(
                    Path::new("/var/lib/velnor"),
                    Some(package_runtime),
                )
                .unwrap(),
                "package and explicit layouts must share one Engine-volume lock namespace"
            );
            assert_eq!(
                shared_host_volume_lock_namespace(
                    Path::new("/one/lib"),
                    Some(Path::new("/run/user/1000")),
                )
                .unwrap(),
                shared_host_volume_lock_namespace(
                    Path::new("/two/lib"),
                    Some(Path::new("/private/tmp/velnor")),
                )
                .unwrap(),
                "PrivateTmp/XDG differences cannot split one Engine lock namespace"
            );
        }

        #[cfg(target_os = "macos")]
        {
            let home = PathBuf::from(std::env::var_os("HOME").expect("test HOME is set"));
            let package_runtime = Path::new("/private/var/run/velnor");
            let explicit_runtime = Path::new("/private/tmp/velnor");
            assert_eq!(
                shared_host_volume_lock_namespace(Path::new("/one/lib"), Some(package_runtime))
                    .unwrap(),
                home.join("Library/Caches/velnor/docker-volume-locks")
            );
            assert_eq!(
                shared_host_volume_lock_namespace(Path::new("/two/lib"), Some(explicit_runtime))
                    .unwrap(),
                shared_host_volume_lock_namespace(Path::new("/one/lib"), None).unwrap(),
                "XDG/private temporary roots cannot split one Docker Desktop Engine lock namespace"
            );
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let home = PathBuf::from(std::env::var_os("HOME").expect("test HOME is set"));
            assert_eq!(
                shared_host_volume_lock_namespace(
                    Path::new("/one/lib"),
                    Some(Path::new("/run/user/1000"))
                )
                .unwrap(),
                home.join(".cache/velnor/docker-volume-locks")
            );
            assert_eq!(
                shared_host_volume_lock_namespace(
                    Path::new("/two/lib"),
                    Some(Path::new("/private/tmp/velnor"))
                )
                .unwrap(),
                shared_host_volume_lock_namespace(Path::new("/one/lib"), None).unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn volume_lock_namespace_fails_closed_without_storage_or_engine_identity() {
        assert!(require_volume_lock_storage_layout(None).is_err());
        assert!(require_volume_lock_engine_id(None).is_err());
        assert!(require_volume_lock_engine_id(Some(" \n".to_owned())).is_err());
        assert!(docker_volume_lock_root_for_domain(Path::new("/tmp"), " ").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn volume_lock_file_rejects_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let storage_root = test_storage_root("velnor-volume-lock-symlink");
        let root = docker_volume_lock_root_under(&storage_root, "engine-stable").unwrap();
        let file_name = format!("{}.lock", volume_lock_key("shared-volume"));
        let external = storage_root.join("outside");
        std::fs::write(&external, "unmodified").unwrap();
        symlink(&external, root.join(&file_name)).unwrap();

        let policy = DockerLeasePolicy::new_with_volume_lock_root("job", Some(root)).unwrap();
        assert!(policy
            .lock_volume_names(&BTreeSet::from(["shared-volume".to_owned()]))
            .is_err());
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "unmodified");
        std::fs::remove_dir_all(storage_root).unwrap();
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
    fn create_capacity_reservation_precedes_mutation_and_pins_uncertain_success() {
        fn fill_to_one_slot(policy: &DockerLeasePolicy) {
            let mut resources = policy.resources.lock().unwrap();
            resources.networks.extend(
                (0..MAX_OWNED_DOCKER_RESOURCES - 2).map(|index| format!("network-{index}")),
            );
            assert_eq!(
                owned_resource_count(&resources),
                MAX_OWNED_DOCKER_RESOURCES - 1
            );
        }

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        fill_to_one_slot(&policy);
        let mut reservation = policy.reserve_owned_resource_slot().unwrap();
        assert!(policy.reserve_owned_resource_slot().is_err());
        reservation.finish().unwrap();
        policy
            .record_owned_resource_identifier(DockerResourceKind::Network, "last-network".into())
            .unwrap();
        assert_eq!(
            owned_resource_count(&policy.resources.lock().unwrap()),
            MAX_OWNED_DOCKER_RESOURCES
        );
        assert!(policy.reserve_owned_resource_slot().is_err());

        let uncertain = DockerLeasePolicy::new("velnor-job-uncertain").unwrap();
        fill_to_one_slot(&uncertain);
        let mut reservation = uncertain.reserve_owned_resource_slot().unwrap();
        reservation.pin();
        assert!(uncertain.reserve_owned_resource_slot().is_err());
        assert_eq!(
            uncertain.resources.lock().unwrap().reserved_resource_slots,
            1,
            "uncertain 2xx outcome must retain its capacity reservation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_create_capacity_stays_reserved_when_response_framing_fails() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let policy = DockerLeasePolicy::new("velnor-job-truncated-create-capacity").unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.networks.extend(
                (0..MAX_OWNED_DOCKER_RESOURCES - 1).map(|index| format!("network-{index}")),
            );
            assert_eq!(
                owned_resource_count(&resources),
                MAX_OWNED_DOCKER_RESOURCES - 1
            );
        }

        let mut reservation = Some(policy.reserve_owned_resource_slot().unwrap());
        reservation.as_mut().unwrap().mark_dispatched().unwrap();

        let (mut engine, mut host) = UnixStream::pair().unwrap();
        let (_guest, mut client) = UnixStream::pair().unwrap();
        engine
            .write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 32\r\nConnection: close\r\n\r\n{\"Id\":\"truncated\"}",
            )
            .unwrap();
        drop(engine);
        let result = forward_http_response_with_delivery(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "POST",
            ForwardResponseOptions {
                defer_response_until_observed: true,
                capture_body: true,
                ..ForwardResponseOptions::default()
            },
            |_, _, _| panic!("incomplete response must not commit resource ownership"),
        );
        assert!(result.is_err());
        pin_resource_reservation_after_uncertain_dispatch(&mut reservation);
        assert_eq!(
            reservation.as_ref().unwrap().state,
            OwnedResourceReservationState::Pinned
        );
        drop(reservation);

        assert_eq!(
            policy.resources.lock().unwrap().reserved_resource_slots,
            1,
            "a dispatched mutation with an incomplete response remains pinned"
        );
        assert!(
            policy.reserve_owned_resource_slot().is_err(),
            "uncertain create must not let later requests exceed the resource cap"
        );
    }

    #[test]
    fn undispatched_create_capacity_is_released_on_drop() {
        let policy = DockerLeasePolicy::new("velnor-job-undispatched-create-capacity").unwrap();
        let reservation = policy.reserve_owned_resource_slot().unwrap();
        assert_eq!(policy.resources.lock().unwrap().reserved_resource_slots, 1);
        drop(reservation);
        assert_eq!(policy.resources.lock().unwrap().reserved_resource_slots, 0);
        assert!(policy.reserve_owned_resource_slot().is_ok());
    }

    #[test]
    fn concurrent_create_capacity_reservations_admit_only_free_slots() {
        use std::sync::Barrier;

        const ATTEMPTS: usize = 8;
        let policy = DockerLeasePolicy::new("velnor-job-concurrent-capacity").unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.networks.extend(
                (0..MAX_OWNED_DOCKER_RESOURCES - 3).map(|index| format!("network-{index}")),
            );
            assert_eq!(
                owned_resource_count(&resources),
                MAX_OWNED_DOCKER_RESOURCES - 2
            );
        }

        let barrier = Arc::new(Barrier::new(ATTEMPTS + 1));
        let (results, received) = std::sync::mpsc::channel();
        let workers = (0..ATTEMPTS)
            .map(|index| {
                let policy = policy.clone();
                let barrier = Arc::clone(&barrier);
                let results = results.clone();
                std::thread::spawn(move || {
                    let mut reservation = policy.reserve_owned_resource_slot().ok();
                    let admitted = reservation.is_some();
                    results.send((index, admitted)).unwrap();
                    barrier.wait();
                    if let Some(reservation) = reservation.as_mut() {
                        policy
                            .record_owned_resource_identifier(
                                DockerResourceKind::Network,
                                format!("concurrent-network-{index}"),
                            )
                            .unwrap();
                        reservation.finish().unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(results);

        let admitted = received
            .into_iter()
            .take(ATTEMPTS)
            .filter(|(_, admitted)| *admitted)
            .count();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(admitted, 2);
        let resources = policy.resources.lock().unwrap();
        assert_eq!(resources.reserved_resource_slots, 0);
        assert_eq!(owned_resource_count(&resources), MAX_OWNED_DOCKER_RESOURCES);
    }

    #[test]
    fn persistent_authorization_captures_generation_and_drains_before_revoke() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("trusted");
        policy.allow_persistent_builder(&builder).unwrap();
        let request = api_request("GET", &format!("/v1.43/containers/{builder}/json"), b"");
        let authorization = policy.authorize_admitted(&request).unwrap();
        assert_eq!(
            authorization.route,
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        );
        let (captured_builder, captured_generation) = authorization.fence().unwrap();
        assert_eq!(captured_builder, builder);
        assert_eq!(captured_generation, 1);

        let revoke_policy = policy.clone();
        let revoke_builder = builder.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let revoker = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            revoke_policy
                .revoke_persistent_builder(&revoke_builder)
                .unwrap();
            finished_tx.send(()).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(25)).is_err());

        drop(authorization);
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        revoker.join().unwrap();
        policy.allow_persistent_builder(&builder).unwrap();
        let next = policy.authorize_admitted(&request).unwrap();
        let (_, next_generation) = next.fence().unwrap();
        assert_eq!(next_generation, captured_generation + 1);
        assert_ne!(next_generation, captured_generation);
    }

    #[test]
    fn bootstrap_conflict_distinguishes_a_second_create_request() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let root = test_storage_root("bootstrap-create-attempt-lock");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "storage-a",
            "engine-a",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        policy.allow_persistent_builder(&builder).unwrap();

        let first_creator = crate::buildkit::begin_persistent_builder_creator_lease(
            &domain,
            &builder,
            "no-config-v1",
            1,
        )
        .unwrap();
        assert!(first_creator.matches(&domain, &builder, "no-config-v1", 1));
        assert!(!first_creator.matches(&domain, &builder, "no-config-v1", 2));
        let mut first = {
            let mut resources = policy.resources.lock().unwrap();
            policy
                .admit_persistent_builder_locked(&mut resources, &builder)
                .unwrap()
        };
        {
            let mut resources = policy.resources.lock().unwrap();
            first
                .mark_bootstrap_create_dispatchable(&mut resources)
                .unwrap();
        }
        assert!(!first.has_other_bootstrap_create().unwrap());

        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let (dispatched_tx, dispatched_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let waiting_policy = policy.clone();
        let waiting_domain = domain.clone();
        let waiting_builder = builder.clone();
        let second = std::thread::spawn(move || {
            let mut admission = {
                let mut resources = waiting_policy.resources.lock().unwrap();
                waiting_policy
                    .admit_persistent_builder_locked(&mut resources, &waiting_builder)
                    .unwrap()
            };
            waiting_tx.send(()).unwrap();
            let creator = crate::buildkit::begin_persistent_builder_creator_lease(
                &waiting_domain,
                &waiting_builder,
                "no-config-v1",
                1,
            )
            .unwrap();
            {
                let mut resources = waiting_policy.resources.lock().unwrap();
                admission
                    .mark_bootstrap_create_dispatchable(&mut resources)
                    .unwrap();
            }
            dispatched_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            drop(creator);
        });
        waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            !first.has_other_bootstrap_create().unwrap(),
            "a request blocked acquiring the process-shared creator flock is not dispatchable"
        );
        drop(first_creator);
        dispatched_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(first.has_other_bootstrap_create().unwrap());
        continue_tx.send(()).unwrap();
        second.join().unwrap();
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_bootstrap_conflicts_elect_one_recovery_and_close_admission() {
        let policy = DockerLeasePolicy::new("velnor-job-conflict-recovery").unwrap();
        let root = test_storage_root("bootstrap-conflict-recovery-gate");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "storage-recovery",
            "engine-recovery",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        policy.allow_persistent_builder(&builder).unwrap();
        let creator = crate::buildkit::begin_persistent_builder_creator_lease(
            &domain,
            &builder,
            "no-config-v1",
            1,
        )
        .unwrap();
        policy
            .resources
            .lock()
            .unwrap()
            .persistent_builder_creator_leases
            .insert(builder.clone(), creator);

        let (mut first, mut second) = {
            let mut resources = policy.resources.lock().unwrap();
            let first = policy
                .admit_persistent_builder_locked(&mut resources, &builder)
                .unwrap();
            let second = policy
                .admit_persistent_builder_locked(&mut resources, &builder)
                .unwrap();
            (first, second)
        };
        {
            let mut resources = policy.resources.lock().unwrap();
            first
                .mark_bootstrap_create_dispatchable(&mut resources)
                .unwrap();
            second
                .mark_bootstrap_create_dispatchable(&mut resources)
                .unwrap();
        }
        first.retire_bootstrap_create_as_conflict_waiter().unwrap();
        second.retire_bootstrap_create_as_conflict_waiter().unwrap();
        assert!(!first.has_other_bootstrap_create().unwrap());
        assert!(!second.has_other_bootstrap_create().unwrap());

        let recovery = policy
            .begin_persistent_builder_recovery(&domain, &builder, 1, "no-config-v1")
            .unwrap()
            .expect("one 409 observer should own stale Created recovery");
        assert!(policy
            .begin_persistent_builder_recovery(&domain, &builder, 1, "no-config-v1")
            .unwrap()
            .is_none());
        {
            let mut resources = policy.resources.lock().unwrap();
            assert!(
                policy
                    .admit_persistent_builder_locked(&mut resources, &builder)
                    .is_err(),
                "guest/bootstrap admissions stay closed during recovery"
            );
        }
        drop(recovery);

        let ordinary = {
            let mut resources = policy.resources.lock().unwrap();
            policy
                .admit_persistent_builder_locked(&mut resources, &builder)
                .unwrap()
        };
        assert!(
            policy
                .begin_persistent_builder_recovery(&domain, &builder, 1, "no-config-v1")
                .unwrap()
                .is_none(),
            "non-conflict request blocks recovery admission"
        );
        drop(ordinary);
        assert!(policy
            .begin_persistent_builder_recovery(&domain, &builder, 1, "no-config-v1")
            .unwrap()
            .is_some());
        drop(first);
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_creator_flock_waiter_cannot_dispatch_during_conflict_recovery() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let policy = DockerLeasePolicy::new("velnor-job-conflict-queued-creator").unwrap();
        let root = test_storage_root("bootstrap-conflict-queued-creator");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "storage-queued",
            "engine-queued",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        policy.allow_persistent_builder(&builder).unwrap();
        let creator = crate::buildkit::begin_persistent_builder_creator_lease(
            &domain,
            &builder,
            "no-config-v1",
            1,
        )
        .unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (flock_acquired_tx, flock_acquired_rx) = mpsc::channel();
        let (dispatched_tx, dispatched_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let waiting_policy = policy.clone();
        let waiting_domain = domain.clone();
        let waiting_builder = builder.clone();
        let waiter = std::thread::spawn(move || {
            let mut admission = {
                let mut resources = waiting_policy.resources.lock().unwrap();
                let admission = waiting_policy
                    .admit_persistent_builder_locked(&mut resources, &waiting_builder)
                    .unwrap();
                let waiters = resources
                    .persistent_builder_creator_lock_waiters
                    .get(&waiting_builder)
                    .copied()
                    .unwrap_or_default()
                    .checked_add(1)
                    .unwrap();
                resources
                    .persistent_builder_creator_lock_waiters
                    .insert(waiting_builder.clone(), waiters);
                admission
            };
            waiting_tx.send(()).unwrap();
            let creator = crate::buildkit::begin_persistent_builder_creator_lease(
                &waiting_domain,
                &waiting_builder,
                "no-config-v1",
                1,
            )
            .unwrap();
            flock_acquired_tx.send(()).unwrap();
            waiting_policy
                .finish_persistent_builder_creator_admission(
                    &waiting_domain,
                    &waiting_builder,
                    "no-config-v1",
                    1,
                    Ok(creator),
                    &mut admission,
                )
                .unwrap();
            dispatched_tx.send(()).unwrap();
            continue_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let creator = waiting_policy
                .resources
                .lock()
                .unwrap()
                .persistent_builder_creator_leases
                .remove(&waiting_builder)
                .unwrap();
            drop(creator);
        });
        waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Model the small authorization gap after the first request acquires
        // the process-shared flock but before its lease is published in the
        // in-process registry. The second request has already registered as a
        // non-dispatchable lock waiter before the first publishes; its flock
        // call below must wait for the first lease to release.
        policy
            .resources
            .lock()
            .unwrap()
            .persistent_builder_creator_leases
            .insert(builder.clone(), creator);
        let mut conflict = {
            let mut resources = policy.resources.lock().unwrap();
            let mut conflict = policy
                .admit_persistent_builder_locked(&mut resources, &builder)
                .unwrap();
            conflict
                .mark_bootstrap_create_dispatchable(&mut resources)
                .unwrap();
            conflict
        };
        conflict
            .retire_bootstrap_create_as_conflict_waiter()
            .unwrap();

        let recovery = policy
            .begin_persistent_builder_recovery(&domain, &builder, 1, "no-config-v1")
            .unwrap()
            .expect("the active 409 owner can recover while another request waits on flock");
        assert!(
            flock_acquired_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "the queued request must remain behind the process-shared creator flock"
        );
        {
            let mut resources = policy.resources.lock().unwrap();
            assert!(
                policy
                    .admit_persistent_builder_locked(&mut resources, &builder)
                    .is_err(),
                "recovery closes admission even with a queued flock waiter"
            );
        }

        // Publishing readiness releases the first creator's process-shared
        // flock. The waiter can acquire it, but remains non-dispatchable until
        // recovery drops its generation gate.
        let creator = policy
            .resources
            .lock()
            .unwrap()
            .persistent_builder_creator_leases
            .remove(&builder)
            .unwrap();
        drop(creator);
        flock_acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let resources = policy.resources.lock().unwrap();
            let waiting_for_creator = resources
                .persistent_builder_creator_lock_waiters
                .get(&builder)
                .copied()
                .unwrap_or_default();
            if waiting_for_creator == 0 {
                assert!(
                    resources
                        .persistent_builder_create_requests_in_flight
                        .get(&builder)
                        .copied()
                        .unwrap_or_default()
                        == 0,
                    "gate-blocked request cannot become dispatchable"
                );
                assert!(dispatched_rx.try_recv().is_err());
                break;
            }
            assert!(Instant::now() < deadline, "waiter did not enter gate wait");
            drop(resources);
            std::thread::yield_now();
        }
        drop(recovery);
        dispatched_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        continue_tx.send(()).unwrap();
        waiter.join().unwrap();
        drop(conflict);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn host_recovery_archive_matches_explicit_config_mode_and_fingerprint() {
        let empty = approved_buildkit_recovery_archive("no-config-v1").unwrap();
        assert_eq!(empty.len(), 1024);
        assert!(empty.iter().all(|byte| *byte == 0));
        assert_eq!(
            validate_persistent_buildkit_tar(&empty).unwrap(),
            "no-config-v1"
        );

        let fingerprint =
            crate::buildkit::persistent_buildkit_config_fingerprint(Some(APPROVED_BUILDKIT_CONFIG))
                .unwrap();
        let archive = approved_buildkit_recovery_archive(&fingerprint).unwrap();
        assert_eq!(
            validate_persistent_buildkit_tar(&archive).unwrap(),
            fingerprint
        );
        assert!(approved_buildkit_recovery_archive("sha256:wrong").is_err());
        assert!(validate_persistent_buildkit_tar(&[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_recovery_archive_upload_requires_framed_success_for_exact_id() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixListener;

        for (response, accepted) in [
            (
                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".as_slice(),
                true,
            ),
            (
                b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\n\r\n".as_slice(),
                false,
            ),
            (b"HTTP/1.1 201 Created\r\n\r\n".as_slice(), false),
            (
                b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".as_slice(),
                true,
            ),
            (
                b"HTTP/1.1 201 Created\r\nContent-Length: 1\r\n\r\nX".as_slice(),
                false,
            ),
            (
                b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".as_slice(),
                false,
            ),
        ] {
            let response_label = String::from_utf8_lossy(response).into_owned();
            let response = response.to_vec();
            let dir = unique_unix_dir("velnor-buildkit-recovery");
            let socket = dir.join("engine.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut scratch = [0_u8; 2048];
                let header_end = loop {
                    let read = stream.read(&mut scratch).unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&scratch[..read]);
                    if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break index + 4;
                    }
                };
                let header = std::str::from_utf8(&request[..header_end]).unwrap();
                assert!(header.starts_with(
                    "PUT /v1.43/containers/immutable-id/archive?path=%2Fetc&noOverwriteDirNonDir=true HTTP/1.1\r\n"
                ));
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                while request.len() < header_end + length {
                    let read = stream.read(&mut scratch).unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&scratch[..read]);
                }
                assert_eq!(request.len(), header_end + length);
                assert_eq!(
                    validate_persistent_buildkit_tar(&request[header_end..]).unwrap(),
                    "no-config-v1"
                );
                stream.write_all(&response).unwrap();
            });
            let result =
                upload_approved_buildkit_archive_on_host(&socket, "immutable-id", "no-config-v1");
            server.join().unwrap();
            let _ = std::fs::remove_dir_all(dir);
            assert_eq!(result.is_ok(), accepted, "response {response_label:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn persistent_builder_revoke_closes_admitted_upgrade_tunnel() {
        use std::io::Read as _;
        use std::os::unix::net::UnixStream;

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("trusted");
        policy.allow_persistent_builder(&builder).unwrap();
        let request = api_request("GET", &format!("/v1.43/containers/{builder}/json"), b"");
        let authorization = policy.authorize_admitted(&request).unwrap();
        let (host, mut engine_peer) = UnixStream::pair().unwrap();
        let (client, mut client_peer) = UnixStream::pair().unwrap();
        let tunnel = authorization
            .register_persistent_tunnel(&host, &client)
            .unwrap();
        drop(authorization);

        let revoke_policy = policy.clone();
        let revoker = std::thread::spawn(move || {
            revoke_policy.revoke_persistent_builder(&builder).unwrap();
        });
        engine_peer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client_peer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(engine_peer.read(&mut byte).unwrap(), 0);
        assert_eq!(client_peer.read(&mut byte).unwrap(), 0);
        drop(tunnel);
        revoker.join().unwrap();
        assert!(policy.persistent_builder_names().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn persistent_builder_shutdown_error_releases_closer_for_retry() {
        use std::os::unix::net::UnixStream;

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("trusted");
        policy.allow_persistent_builder(&builder).unwrap();
        let request = api_request("GET", &format!("/v1.43/containers/{builder}/json"), b"");
        let authorization = policy.authorize_admitted(&request).unwrap();
        let (host, _host_peer) = UnixStream::pair().unwrap();
        let (client, _client_peer) = UnixStream::pair().unwrap();
        let tunnel = authorization
            .register_persistent_tunnel(&host, &client)
            .unwrap();
        let (host_two, _host_peer_two) = UnixStream::pair().unwrap();
        let (client_two, _client_peer_two) = UnixStream::pair().unwrap();
        let tunnel_two = authorization
            .register_persistent_tunnel(&host_two, &client_two)
            .unwrap();
        drop(authorization);

        {
            let mut resources = policy.resources.lock().unwrap();
            let generation = resources.persistent_builder_generations[&builder];
            resources
                .persistent_builder_requests_closing
                .insert(builder.clone());
            let closer_id = 91;
            resources
                .persistent_builder_requests_closer_active
                .insert(builder.clone(), closer_id);
            let mut attempts = 0;
            let error = shutdown_persistent_builder_tunnels(
                &mut resources,
                &policy.persistent_builder_requests_changed,
                &builder,
                generation,
                closer_id,
                |_| {
                    attempts += 1;
                    if attempts == 1 {
                        Err(io::Error::other("injected tunnel shutdown failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("injected tunnel shutdown failure"));
            assert_eq!(attempts, 4, "shutdown must visit every tunnel endpoint");
            assert!(!resources
                .persistent_builder_requests_closer_active
                .contains_key(&builder));
            assert!(resources
                .persistent_builder_requests_closing
                .contains(&builder));
        }

        // Retire the failed tunnel, then prove a later closer can take over
        // and reopen admission after completing its lifecycle operation.
        drop(tunnel);
        drop(tunnel_two);
        policy
            .with_persistent_builder_admission_closed(&builder, |_| Ok(()))
            .unwrap();
        let resources = policy.resources.lock().unwrap();
        assert!(!resources
            .persistent_builder_requests_closer_active
            .contains_key(&builder));
        assert!(!resources
            .persistent_builder_requests_closing
            .contains(&builder));
    }

    #[test]
    fn poisoned_persistent_builder_condvar_wait_clears_closer_and_fails_closed() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("trusted");
        policy.allow_persistent_builder(&builder).unwrap();
        let request = api_request("GET", &format!("/v1.43/containers/{builder}/json"), b"");
        let admission = policy.authorize_admitted(&request).unwrap();
        let operation_called = Arc::new(AtomicBool::new(false));
        let waiter_policy = policy.clone();
        let waiter_builder = builder.clone();
        let poisoned_builder = builder.clone();
        let waiter_called = Arc::clone(&operation_called);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result =
                waiter_policy.with_persistent_builder_admission_closed(&waiter_builder, |_| {
                    waiter_called.store(true, Ordering::SeqCst);
                    Ok(())
                });
            result_tx.send(result.is_err()).unwrap();
        });

        // Acquiring the mutex after the closer marks itself active proves it
        // has entered Condvar::wait with the live admission still outstanding.
        let marker_deadline = std::time::Instant::now() + Duration::from_secs(1);
        let resources = loop {
            let resources = policy.resources.lock().unwrap();
            if resources
                .persistent_builder_requests_closer_active
                .contains_key(&builder)
            {
                break resources;
            }
            drop(resources);
            assert!(
                std::time::Instant::now() < marker_deadline,
                "closer did not publish its active marker before the deadline"
            );
            std::thread::yield_now();
        };
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let resources = resources;
            resources
                .persistent_builder_requests_in_flight
                .get(&poisoned_builder)
                .is_some_and(|count| *count > 0)
                .then_some(())
                .expect("admission remains live while closer waits");
            panic!("inject mutex poison while closer waits");
        }));
        assert!(poisoned.is_err());
        policy.persistent_builder_requests_changed.notify_all();

        assert!(result_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        drop(admission);
        waiter.join().unwrap();
        let resources = match policy.resources.lock() {
            Ok(_) => panic!("injected condvar panic did not poison the registry"),
            Err(poisoned) => poisoned.into_inner(),
        };
        assert!(!resources
            .persistent_builder_requests_closer_active
            .contains_key(&builder));
        assert!(resources
            .persistent_builder_requests_closing
            .contains(&builder));
        assert!(!operation_called.load(Ordering::SeqCst));
        drop(resources);
        let request = api_request("GET", &format!("/v1.43/containers/{builder}/json"), b"");
        assert!(policy.authorize_admitted(&request).is_err());
    }

    #[test]
    fn poisoned_contender_wait_preserves_active_closer_identity() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("trusted");
        let owner_id = 41;
        {
            let mut resources = policy.resources.lock().unwrap();
            resources
                .persistent_builder_requests_closing
                .insert(builder.clone());
            resources
                .persistent_builder_requests_closer_active
                .insert(builder.clone(), owner_id);
        }

        let operation_called = Arc::new(AtomicBool::new(false));
        let waiter_operation_called = Arc::clone(&operation_called);
        let waiter_policy = policy.clone();
        let waiter_builder = builder.clone();
        let poisoned_builder = builder.clone();
        let (at_wait_tx, at_wait_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let wait_hook_used = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter_wait_hook_used = std::sync::Arc::clone(&wait_hook_used);
        let waiter = std::thread::spawn(move || {
            let result = waiter_policy.with_persistent_builder_admission_closed_and_wait_hook(
                &waiter_builder,
                |_| {
                    waiter_operation_called.store(true, Ordering::SeqCst);
                    Ok(())
                },
                || {
                    if !waiter_wait_hook_used.swap(true, Ordering::SeqCst) {
                        at_wait_tx.send(()).unwrap();
                        resume_rx
                            .recv_timeout(Duration::from_secs(1))
                            .expect("test did not release contender wait hook before deadline");
                    }
                },
            );
            result_tx.send(result.is_err()).unwrap();
        });

        at_wait_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The hook holds the registry mutex. After releasing it, success from
        // try_lock proves the contender atomically entered Condvar::wait.
        resume_tx.send(()).unwrap();
        let wait_deadline = std::time::Instant::now() + Duration::from_secs(1);
        let resources = loop {
            match policy.resources.try_lock() {
                Ok(resources) => break resources,
                Err(std::sync::TryLockError::WouldBlock) => {
                    assert!(
                        std::time::Instant::now() < wait_deadline,
                        "contender did not enter its condition-variable wait before the deadline"
                    );
                    std::thread::yield_now();
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("registry was poisoned before this contender waited")
                }
            }
        };
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let resources = resources;
            assert_eq!(
                resources
                    .persistent_builder_requests_closer_active
                    .get(&poisoned_builder),
                Some(&owner_id),
                "the other closer still owns its marker"
            );
            panic!("inject registry poison while contender waits");
        }));
        assert!(poisoned.is_err());
        policy.persistent_builder_requests_changed.notify_all();

        assert!(result_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        waiter.join().unwrap();
        let resources = match policy.resources.lock() {
            Ok(_) => panic!("injected contender panic did not poison the registry"),
            Err(poisoned) => poisoned.into_inner(),
        };
        assert_eq!(
            resources
                .persistent_builder_requests_closer_active
                .get(&builder),
            Some(&owner_id),
            "a contender must not clear another closer's ownership"
        );
        assert!(resources
            .persistent_builder_requests_closing
            .contains(&builder));
        assert!(!operation_called.load(Ordering::SeqCst));
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
            .rewrite_authorized_alias_target(&request, authorization, None)
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
    fn generic_container_alias_delete_cannot_target_or_revoke_replacement() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let mut resources = policy.resources.lock().unwrap();
        resources.containers.insert("old-container-id".to_owned());
        resources.containers.insert("new-container-id".to_owned());
        resources
            .container_names
            .insert("container-alias".to_owned(), "old-container-id".to_owned());
        drop(resources);

        let request = api_request(
            "DELETE",
            "/v1.43/containers/container-alias?force=true",
            b"",
        );
        let authorization = policy.authorize_admitted(&request).unwrap();
        assert_eq!(authorization.container_id(), Some("old-container-id"));

        // Replace the alias after authorization to model another in-flight
        // lease request completing before this delete reaches Docker.
        policy
            .resources
            .lock()
            .unwrap()
            .container_names
            .insert("container-alias".to_owned(), "new-container-id".to_owned());

        let rewritten = policy
            .rewrite_authorized_alias_target(
                &request,
                authorization.route,
                authorization.container_id(),
            )
            .unwrap();
        assert!(String::from_utf8(rewritten)
            .unwrap()
            .contains("DELETE /v1.43/containers/old-container-id?force=true"));
        policy
            .record_delete_response_fenced(
                DockerResourceKind::Container,
                "container-alias",
                204,
                authorization.container_id(),
            )
            .unwrap();

        let resources = policy.resources.lock().unwrap();
        assert_eq!(
            resources
                .container_names
                .get("container-alias")
                .map(String::as_str),
            Some("new-container-id")
        );
        assert!(resources.containers.contains("new-container-id"));
        assert!(!resources.containers.contains("old-container-id"));
    }

    #[test]
    fn persistent_config_archive_is_rejected_before_dispatch_without_exact_fresh_id_proof() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        let container_name = format!("buildx_buildkit_{builder}0");
        let expected = "sha256:approved-buildkit-config";
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.persistent_builders.insert(builder.clone());
            resources
                .persistent_builder_generations
                .insert(builder.clone(), 4);
            resources
                .persistent_builder_config_fingerprints
                .insert(builder.clone(), expected.to_owned());
            resources
                .persistent_containers
                .insert(container_name, "fresh-container-id".to_owned());
            resources
                .persistent_container_fresh_ids
                .insert("fresh-container-id".to_owned());
        }

        policy
            .authorize_persistent_config_archive(&builder, "fresh-container-id", 4, expected)
            .expect("exact active generation, ID, and config fingerprint authorize dispatch");
        assert!(policy
            .authorize_persistent_config_archive(
                &builder,
                "fresh-container-id",
                4,
                "sha256:other-approved-mode",
            )
            .is_err());
        assert!(policy
            .authorize_persistent_config_archive(&builder, "replacement-id", 4, expected)
            .is_err());
        assert!(policy
            .authorize_persistent_config_archive(&builder, "fresh-container-id", 3, expected)
            .is_err());
    }

    #[test]
    fn lease_policy_attests_persistent_buildkit_before_lifecycle_use() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let container = format!("buildx_buildkit_{builder}0");
        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        let volume = format!("{container}_state");
        let inspect = format!(
            r#"{{"Id":"persistent-id","Image":"sha256:persistent-image","Name":"/{container}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"velnor-job-old","velnor.buildkit-domain":"{domain_token}"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}"#
        );
        let unsafe_config = inspect.replace(
            "\"Labels\":",
            "\"Healthcheck\":{\"Test\":[\"CMD-SHELL\",\"touch /tmp/unsafe\"]},\"Labels\":",
        );
        policy
            .record_persistent_container_inspect(&container, 200, unsafe_config.as_bytes())
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
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
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
        {
            let mut resources = policy.resources.lock().unwrap();
            resources
                .persistent_container_fresh_ids
                .insert("persistent-id".to_owned());
            resources
                .persistent_container_config_archives
                .insert("persistent-id".to_owned(), "no-config-v1".to_owned());
        }
        let exec_create = api_request(
            "POST",
            &format!("/v1.43/containers/{container}/exec"),
            br#"{"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Cmd":["buildctl","dial-stdio"]}"#,
        );
        assert!(
            policy.authorize(&exec_create).is_err(),
            "fresh archive proof authorizes start, not BuildKit exec before readiness"
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
        policy
            .allow_ready_persistent_container(&builder, "persistent-id", "no-config-v1")
            .unwrap();
        assert!(policy
            .authorize(&api_request(
                "DELETE",
                &format!("/v1.43/containers/{container}"),
                b""
            ))
            .is_err());

        let volume_inspect = format!(
            r#"{{"Name":"{volume}","Driver":"local","Options":{{}},"Labels":{{"velnor.job-id":"velnor-job-old","velnor.buildkit-domain":"{domain_token}"}}}}"#
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
            .record_persistent_volume_inspect(&volume, 200, volume_inspect.as_bytes())
            .expect("persistent state volume inspect should attest");

        let foreign = inspect.replace(domain_token, "f0123456789abcdef0123456789abcdef");
        policy
            .record_persistent_container_inspect(&container, 200, foreign.as_bytes())
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
    fn host_persistent_buildkit_identity_projections_attest_domain_and_state_mount() {
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let container = crate::buildkit::daemon_container_name(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let container_projection = |domain: &str, mount_name: &str| {
            [
                serde_json::to_string("container-id").unwrap(),
                serde_json::to_string(&format!("/{container}")).unwrap(),
                serde_json::to_string(&BTreeMap::from([
                    (JOB_ID_LABEL.to_owned(), "creator-job".to_owned()),
                    (BUILDKIT_DOMAIN_LABEL.to_owned(), domain.to_owned()),
                ]))
                .unwrap(),
                serde_json::to_string(&vec![serde_json::json!({
                    "Type": "volume",
                    "Name": mount_name,
                    "Destination": "/var/lib/buildkit",
                })])
                .unwrap(),
            ]
            .join("\t")
        };

        assert_eq!(
            attest_persistent_buildkit_container_identity(
                &container_projection(domain_token, &volume),
                &builder,
                &volume,
                domain_token,
            )
            .unwrap(),
            "container-id"
        );
        assert!(attest_persistent_buildkit_container_identity(
            &container_projection("f0123456789abcdef0123456789abcdef", &volume),
            &builder,
            &volume,
            domain_token,
        )
        .is_err());
        assert!(attest_persistent_buildkit_container_identity(
            &container_projection(domain_token, "foreign-state-volume"),
            &builder,
            &volume,
            domain_token,
        )
        .is_err());

        let volume_projection = format!(
            "{volume:?}\t\"local\"\t{{\"{JOB_ID_LABEL}\":\"creator-job\",\"{BUILDKIT_DOMAIN_LABEL}\":\"{domain_token}\"}}\t{{}}"
        );
        assert_eq!(
            attest_persistent_buildkit_volume_identity(&volume_projection, &volume, domain_token)
                .unwrap(),
            volume
        );
        let wrong_volume_domain =
            volume_projection.replace(domain_token, "f0123456789abcdef0123456789abcdef");
        assert!(attest_persistent_buildkit_volume_identity(
            &wrong_volume_domain,
            &volume,
            domain_token,
        )
        .is_err());
    }

    #[test]
    fn persistent_buildkit_access_is_exactly_current_job_builder_scoped() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let current = test_persistent_builder("branch");
        let foreign = test_persistent_builder("release");
        let foreign_domain = persistent_buildkit_domain_token(&foreign).unwrap();
        policy.allow_persistent_builder(&current).unwrap();
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
            r#"{{"Name":"{foreign_volume}","Driver":"local","Labels":{{"velnor.job-id":"other-job","velnor.buildkit-domain":"{foreign_domain}"}}}}"#
        );
        policy
            .record_persistent_volume_inspect(&foreign_volume, 200, foreign_inspect.as_bytes())
            .expect_err("foreign builder state must fail exact allowlist attestation");
    }

    #[test]
    fn host_persistent_volume_projection_registers_before_buildx_mount() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        policy.allow_persistent_builder(&builder).unwrap();
        let volume = format!("buildx_buildkit_{builder}0_state");
        // This is the exact four-field TSV shape emitted by
        // inspect_volume_identity_args, including JSON-encoded fields.
        let projection = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.buildkit-domain\":\"{domain_token}\"}}\t{{}}\n"
        );
        policy
            .record_persistent_volume_projection(&volume, projection.as_bytes(), domain_token)
            .expect("host inspect projection should register the attested state volume");
        let wrong_domain = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.buildkit-domain\":\"f0123456789abcdef0123456789abcdef\"}}\t{{}}\n"
        );
        policy
            .record_persistent_volume_projection(&volume, wrong_domain.as_bytes(), domain_token)
            .expect_err("volume with a different domain token must fail attestation");
        let omitted_options = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.buildkit-domain\":\"{domain_token}\"}}\n"
        );
        policy
            .record_persistent_volume_projection(&volume, omitted_options.as_bytes(), domain_token)
            .expect("Docker's omitted Options field is the empty local default");
        let null_options = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"velnor-job-old\",\"velnor.buildkit-domain\":\"{domain_token}\"}}\tnull\n"
        );
        policy
            .record_persistent_volume_projection(&volume, null_options.as_bytes(), domain_token)
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
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let volume = format!("buildx_buildkit_{builder}0_state");
        policy
            .begin_persistent_builder_setup(&builder, "no-config-v1")
            .unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:buildkit-image")
            .unwrap();
        policy
            .record_persistent_volume_projection(
                &volume,
                format!(
                    "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"old-job\",\"velnor.buildkit-domain\":\"{domain_token}\"}}\t{{}}\n"
                )
                .as_bytes(),
                domain_token,
            )
            .unwrap();
        let image_pull = api_request(
            "POST",
            "/v1.43/images/create?fromImage=moby%2Fbuildkit&tag=buildx-stable-1",
            b"",
        );
        assert!(policy.authorize(&image_pull).is_err());
        policy
            .complete_persistent_builder_setup(&builder, 0)
            .unwrap();
        assert_eq!(
            policy.authorize(&image_pull).unwrap(),
            AuthorizedDockerRoute::PersistentImagePull
        );
    }

    #[cfg(unix)]
    #[test]
    fn persistent_image_routes_revalidate_epoch_under_state_volume_lock() {
        let root = test_storage_root("persistent-image-readiness-epoch");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "image-epoch-storage",
            "image-epoch-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "requested",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("repo-key-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let policy = DockerLeasePolicy::new("velnor-job-image-epoch").unwrap();
        policy.allow_persistent_builder(&builder).unwrap();

        let requests = [
            (
                api_request(
                    "POST",
                    "/v1.43/images/create?fromImage=moby%2Fbuildkit&tag=buildx-stable-1",
                    b"",
                ),
                AuthorizedDockerRoute::PersistentImagePull,
            ),
            (
                api_request(
                    "GET",
                    "/v1.43/images/moby/buildkit:buildx-stable-1/json",
                    b"",
                ),
                AuthorizedDockerRoute::PersistentImageInspect,
            ),
        ];
        let mut admitted = requests
            .into_iter()
            .map(|(request, route)| {
                let authorization = policy.authorize_admitted(&request).unwrap();
                assert_eq!(authorization.route, route);
                authorization
            })
            .collect::<Vec<_>>();

        let state_volume = crate::buildkit::daemon_state_volume(&builder);
        let _volume_lock = lock_host_volume_name_for_domain(&domain, &state_volume).unwrap();
        for authorization in &mut admitted {
            validate_persistent_route_under_volume_lock(
                authorization,
                &policy,
                &domain,
                authorization.route,
            )
            .unwrap();
        }

        // Another proxy starts the same daemon after these image requests
        // were admitted. Their admission generation remains locally valid,
        // but the durable epoch check under the shared lock rejects dispatch.
        crate::buildkit::invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            "image-epoch-container",
            "no-config-v1",
            0,
        )
        .unwrap();
        for authorization in &mut admitted {
            assert!(validate_persistent_route_under_volume_lock(
                authorization,
                &policy,
                &domain,
                authorization.route,
            )
            .is_err());
        }
        drop(_volume_lock);
        admitted.clear();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_exec_response_failure_cannot_fall_through_owned() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let container = crate::buildkit::daemon_container_name(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        register_test_persistent_volume_projection(&policy, &volume, domain_token);
        let container_inspect = format!(
            r#"{{"Id":"persistent-container-id","Image":"sha256:persistent-image","Name":"/{container}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"creator-job","velnor.buildkit-domain":"{domain_token}"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}}}"#
        );
        policy
            .record_persistent_container_inspect(&container, 200, container_inspect.as_bytes())
            .unwrap();
        let owned_container_id = policy.persistent_container_id(&container).unwrap();
        let generation = policy
            .resources
            .lock()
            .unwrap()
            .persistent_builder_generations[&builder];
        let create = api_request(
            "POST",
            &format!("/v1.43/containers/{container}/exec"),
            br#"{"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Cmd":["buildctl","dial-stdio"]}"#,
        );
        assert!(policy.authorize(&create).is_err());
        policy
            .allow_ready_persistent_container(&builder, &owned_container_id, "no-config-v1")
            .unwrap();
        assert_eq!(
            policy.authorize(&create).unwrap(),
            AuthorizedDockerRoute::PersistentExecCreate
        );
        let request = api_request("POST", "/v1.43/exec/exec-id/start", b"");
        assert!(policy.authorize(&request).is_err());
        assert!(policy
            .note_persistent_exec(
                201,
                br#"{"unexpected":true}"#,
                &builder,
                &owned_container_id,
                generation,
                0,
            )
            .is_err());
        assert!(policy.authorize(&request).is_err());
        policy
            .note_persistent_exec(
                201,
                br#"{"Id":"exec-id"}"#,
                &builder,
                &owned_container_id,
                generation,
                0,
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
        policy
            .resources
            .lock()
            .unwrap()
            .persistent_container_ready_fingerprints
            .remove(&owned_container_id);
        assert!(policy.authorize(&create).is_err());
        assert!(policy.authorize(&upgrade_request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn failed_persistent_readiness_proof_forgets_binding_and_denies_exec() {
        let root = test_storage_root("persistent-exec-readiness-failure");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "readiness-storage",
            "readiness-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "requested",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let domain_token = domain.token.as_str();
        let container = crate::buildkit::daemon_container_name(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let container_id = "readiness-container-id";
        let config_fingerprint = "no-config-v1";
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        register_test_persistent_volume_projection(&policy, &volume, domain_token);
        let inspect = format!(
            r#"{{"Id":"{container_id}","Image":"sha256:persistent-image","Name":"/{container}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"creator-job","velnor.buildkit-domain":"{domain_token}"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}"#
        );
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        let generation = policy
            .resources
            .lock()
            .unwrap()
            .persistent_builder_generations[&builder];
        assert!(!crate::buildkit::builder_readiness_matches(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap());
        assert!(accept_reused_persistent_container_readiness(
            &policy,
            &domain,
            &builder,
            container_id,
            generation,
            config_fingerprint,
        )
        .is_err());
        assert!(policy.persistent_container_id(&container).is_err());

        let exec_create = api_request(
            "POST",
            &format!("/v1.43/containers/{container}/exec"),
            br#"{"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Cmd":["buildctl","dial-stdio"]}"#,
        );
        assert!(policy.authorize(&exec_create).is_err());
        let exec_start = api_request("POST", "/v1.43/exec/stale-exec-id/start", b"");
        assert!(policy.authorize(&exec_start).is_err());

        // Once the exact durable record exists, inspect promotes this same
        // immutable ID and the intended BuildKit exec is admitted.
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        crate::buildkit::write_test_builder_readiness(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap();
        assert!(crate::buildkit::builder_readiness_matches(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap());
        accept_reused_persistent_container_readiness(
            &policy,
            &domain,
            &builder,
            container_id,
            generation,
            config_fingerprint,
        )
        .unwrap();
        assert_eq!(
            policy.authorize(&exec_create).unwrap(),
            AuthorizedDockerRoute::PersistentExecCreate
        );
        policy
            .note_persistent_exec(
                201,
                br#"{"Id":"ready-exec-id"}"#,
                &builder,
                container_id,
                generation,
                1,
            )
            .unwrap();
        let mut ready_exec_start = api_request("POST", "/v1.43/exec/ready-exec-id/start", b"");
        let header_end = ready_exec_start
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        ready_exec_start.splice(
            header_end + 2..header_end + 2,
            b"Connection: Upgrade\r\nUpgrade: tcp\r\n".iter().copied(),
        );
        assert_eq!(
            policy.authorize(&ready_exec_start).unwrap(),
            AuthorizedDockerRoute::PersistentExec
        );

        // A restart must retire both cached and durable old readiness before
        // Docker receives `/start`; a failed worker probe cannot reuse the
        // pre-restart proof or its exec IDs.
        let start_request =
            api_request("POST", &format!("/v1.43/containers/{container}/start"), b"");
        assert_eq!(
            policy.authorize(&start_request).unwrap(),
            AuthorizedDockerRoute::Persistent(DockerResourceKind::Container)
        );
        let mut stale_exec_create = policy.authorize_admitted(&exec_create).unwrap();
        let mut stale_exec_start = policy.authorize_admitted(&ready_exec_start).unwrap();
        let _volume_lock = lock_host_volume_name_for_domain(
            &domain,
            &crate::buildkit::daemon_state_volume(&builder),
        )
        .unwrap();
        // Simulate another proxy advancing the shared durable epoch. Local
        // authorization still sees epoch 1, so only the durable check under
        // this Engine-volume lock can reject these preauthorized requests.
        let external_epoch = crate::buildkit::invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
            1,
        )
        .unwrap();
        assert_eq!(external_epoch, 2);
        assert!(!crate::buildkit::builder_readiness_matches(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap());
        policy
            .note_persistent_start_epoch(&builder, generation, container_id, 1, external_epoch)
            .unwrap();
        let (lock_acquired_tx, lock_acquired_rx) = std::sync::mpsc::sync_channel(1);
        let (dispatch_denied_tx, dispatch_denied_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _exec_lock = lock_host_volume_name_for_domain(
                    &domain,
                    &crate::buildkit::daemon_state_volume(&builder),
                )
                .unwrap();
                lock_acquired_tx.send(()).unwrap();
                let denied = validate_persistent_route_under_volume_lock(
                    &mut stale_exec_create,
                    &policy,
                    &domain,
                    AuthorizedDockerRoute::PersistentExecCreate,
                )
                .is_err();
                dispatch_denied_tx.send(denied).unwrap();
            });
            assert!(lock_acquired_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err());
            drop(_volume_lock);
            lock_acquired_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("exec request should acquire the shared lock after start releases it");
            assert!(dispatch_denied_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("exec request should finish dispatch validation"));
        });
        assert!(validate_persistent_route_under_volume_lock(
            &mut stale_exec_start,
            &policy,
            &domain,
            AuthorizedDockerRoute::PersistentExec,
        )
        .is_err());
        assert!(policy.authorize(&exec_create).is_err());
        assert!(policy.authorize(&ready_exec_start).is_err());
        drop(stale_exec_create);
        drop(stale_exec_start);

        // A successful subsequent readiness proof restores only the current
        // builder's intended exec capability.
        crate::buildkit::write_test_builder_readiness(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap();
        policy
            .note_persistent_ready_container(
                &builder,
                generation,
                container_id,
                config_fingerprint,
                external_epoch,
            )
            .unwrap();

        let mut start_authorization = policy.authorize_admitted(&start_request).unwrap();
        let _start_lock = lock_host_volume_name_for_domain(
            &domain,
            &crate::buildkit::daemon_state_volume(&builder),
        )
        .unwrap();
        let restarted_epoch = advance_persistent_start_epoch_under_volume_lock(
            &mut start_authorization,
            &policy,
            &domain,
        )
        .unwrap();
        drop(_start_lock);
        drop(start_authorization);
        crate::buildkit::write_test_builder_readiness(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap();
        policy
            .note_persistent_ready_container(
                &builder,
                generation,
                container_id,
                config_fingerprint,
                restarted_epoch,
            )
            .unwrap();
        assert_eq!(
            policy.authorize(&exec_create).unwrap(),
            AuthorizedDockerRoute::PersistentExecCreate
        );
        policy
            .note_persistent_exec(
                201,
                br#"{"Id":"ready-exec-id"}"#,
                &builder,
                container_id,
                generation,
                restarted_epoch,
            )
            .unwrap();
        assert_eq!(
            policy.authorize(&ready_exec_start).unwrap(),
            AuthorizedDockerRoute::PersistentExec
        );

        // Reinspection of the same immutable ID must revoke the old cached
        // proof before checking disk. A stale/corrupt durable record must not
        // leave the previously registered exec ID usable during the check.
        crate::buildkit::write_test_builder_readiness(
            &domain,
            &builder,
            container_id,
            "sha256:stale-config",
        )
        .unwrap();
        let inspect_request =
            api_request("GET", &format!("/v1.43/containers/{container}/json"), b"");
        assert_eq!(
            policy.authorize(&inspect_request).unwrap(),
            AuthorizedDockerRoute::PersistentInspect(DockerResourceKind::Container)
        );
        assert!(policy.authorize(&exec_create).is_err());
        assert!(policy.authorize(&ready_exec_start).is_err());
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        assert!(accept_reused_persistent_container_readiness(
            &policy,
            &domain,
            &builder,
            container_id,
            generation,
            config_fingerprint,
        )
        .is_err());
        assert!(policy.persistent_container_id(&container).is_err());
        assert!(policy.authorize(&exec_create).is_err());
        assert!(policy.authorize(&ready_exec_start).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stopped_builder_revokes_pre_authorized_exec_until_new_readiness_probe() {
        let root = test_storage_root("persistent-exec-stopped-readiness");
        let domain = crate::buildkit::PersistentBuildKitDomain::from_identities(
            &root,
            "stopped-storage",
            "stopped-engine",
        )
        .unwrap();
        let builder = crate::buildkit::persistent_builder_name_for_domain(
            &domain.token,
            "requested",
            "scope",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("https://github.com/123"),
        );
        let domain_token = domain.token.as_str();
        let container = crate::buildkit::daemon_container_name(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let container_id = "stopped-builder-container-id";
        let config_fingerprint = "no-config-v1";
        let policy = DockerLeasePolicy::new("velnor-job-stopped").unwrap();
        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        register_test_persistent_volume_projection(&policy, &volume, domain_token);
        let inspect = format!(
            r#"{{"Id":"{container_id}","Image":"sha256:persistent-image","Name":"/{container}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"creator-job","velnor.buildkit-domain":"{domain_token}"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}]}}"#
        );
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        crate::buildkit::write_test_builder_readiness(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
        )
        .unwrap();
        policy
            .note_persistent_ready_container(&builder, 1, container_id, config_fingerprint, 1)
            .unwrap();

        let exec_create = api_request(
            "POST",
            &format!("/v1.43/containers/{container}/exec"),
            br#"{"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Cmd":["buildctl","dial-stdio"]}"#,
        );
        let mut exec_start = api_request("POST", "/v1.43/exec/stopped-exec/start", b"");
        let header_end = exec_start
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        exec_start.splice(
            header_end + 2..header_end + 2,
            b"Connection: Upgrade\r\nUpgrade: tcp\r\n".iter().copied(),
        );
        policy
            .note_persistent_exec(
                201,
                br#"{"Id":"stopped-exec"}"#,
                &builder,
                container_id,
                1,
                1,
            )
            .unwrap();
        let mut admitted_create = policy.authorize_admitted(&exec_create).unwrap();
        let mut admitted_start = policy.authorize_admitted(&exec_start).unwrap();

        let _volume_lock = lock_host_volume_name_for_domain(&domain, &volume).unwrap();
        let stopping_epoch = crate::buildkit::invalidate_builder_readiness_before_stop(
            &domain,
            &builder,
            container_id,
        )
        .unwrap()
        .unwrap()
        .1;
        assert_eq!(stopping_epoch, 2);
        crate::buildkit::publish_builder_stopped_after_stop(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
            stopping_epoch,
        )
        .unwrap();
        assert!(validate_persistent_route_under_volume_lock(
            &mut admitted_create,
            &policy,
            &domain,
            AuthorizedDockerRoute::PersistentExecCreate,
        )
        .is_err());
        assert!(validate_persistent_route_under_volume_lock(
            &mut admitted_start,
            &policy,
            &domain,
            AuthorizedDockerRoute::PersistentExec,
        )
        .is_err());

        // A fresh Engine inspect of the same immutable ID must not resurrect
        // the stopped epoch as reusable readiness.
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        assert!(accept_reused_persistent_container_readiness(
            &policy,
            &domain,
            &builder,
            container_id,
            1,
            config_fingerprint,
        )
        .is_err());
        assert!(
            policy.authorize(&exec_create).is_err(),
            "an inspect after stop must not restore exec authority from the old epoch"
        );
        drop(_volume_lock);

        let starting_epoch = crate::buildkit::invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
            stopping_epoch,
        )
        .unwrap();
        crate::buildkit::probe_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
            starting_epoch,
            || Ok(()),
        )
        .unwrap();
        crate::buildkit::publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config_fingerprint,
            starting_epoch,
        )
        .unwrap();
        policy
            .record_persistent_container_inspect(&container, 200, inspect.as_bytes())
            .unwrap();
        policy
            .note_persistent_ready_container(
                &builder,
                1,
                container_id,
                config_fingerprint,
                starting_epoch,
            )
            .unwrap();
        assert_eq!(starting_epoch, 3);
        assert_eq!(
            policy.authorize(&exec_create).unwrap(),
            AuthorizedDockerRoute::PersistentExecCreate,
            "fresh worker proof restores only new exec creation"
        );
        assert!(
            policy.authorize(&exec_start).is_err(),
            "old exec ID stays stale"
        );

        drop(admitted_create);
        drop(admitted_start);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_volume_attestation_rejects_local_bind_options() {
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let volume = format!("buildx_buildkit_{builder}0_state");
        let allowed = BTreeSet::from([builder.to_owned()]);
        let hostile = format!(
            "\"{volume}\"\t\"local\"\t{{\"velnor.job-id\":\"old-job\",\"velnor.buildkit-domain\":\"{domain_token}\"}}\t{{\"type\":\"none\",\"o\":\"bind\",\"device\":\"/\"}}\n"
        );
        assert!(attest_persistent_buildkit_volume_projection(
            &hostile,
            &volume,
            domain_token,
            &allowed,
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

        let config = APPROVED_BUILDKIT_CONFIG_ARCHIVE;
        let mut file = tar::Header::new_gnu();
        file.set_path("buildkit/buildkitd.toml").unwrap();
        file.set_entry_type(tar::EntryType::Regular);
        file.set_mode(0o644);
        file.set_size(config.len() as u64);
        file.set_cksum();
        builder.append(&file, Cursor::new(config.to_vec())).unwrap();
        let archive = builder.into_inner().unwrap();
        let fingerprint = validate_persistent_buildkit_tar(&archive).unwrap();
        assert_eq!(
            fingerprint,
            "sha256:333c40f4fee6f473bee299aed751bb40967b9e8315af90378a5de0b5dc69a76b"
        );

        // Semantically equivalent source TOML is acceptable as input, but
        // Buildx must have normalized it before the archive readiness proof.
        let mut noncanonical = tar::Builder::new(Vec::new());
        let mut directory = tar::Header::new_gnu();
        directory.set_path("buildkit/").unwrap();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o755);
        directory.set_size(0);
        directory.set_cksum();
        noncanonical
            .append(&directory, Cursor::new(Vec::<u8>::new()))
            .unwrap();
        let source_spelling =
            b"# comment\n[registry.\"docker.io\"]\nmirrors = [\"mirror.gcr.io\"]\n";
        let mut file = tar::Header::new_gnu();
        file.set_path("buildkit/buildkitd.toml").unwrap();
        file.set_entry_type(tar::EntryType::Regular);
        file.set_mode(0o644);
        file.set_size(source_spelling.len() as u64);
        file.set_cksum();
        noncanonical
            .append(&file, Cursor::new(source_spelling.to_vec()))
            .unwrap();
        assert!(validate_persistent_buildkit_tar(&noncanonical.into_inner().unwrap()).is_err());

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
        let builder = test_persistent_builder("branch");
        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .register_persistent_builder_image(&builder, "sha256:buildkit-image")
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
    fn network_connect_uses_container_id_captured_during_authorization() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let container_response = br#"{"Id":"old-container-id"}"#;
        policy
            .record_create_response(DockerResourceKind::Container, 201, container_response)
            .unwrap();
        policy
            .note_container_name("container-alias", 201, container_response)
            .unwrap();
        policy
            .record_create_response(DockerResourceKind::Network, 201, br#"{"Id":"net-owned"}"#)
            .unwrap();
        let request = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"container-alias","EndpointConfig":{}}"#,
        );
        let authorization = policy.authorize_admitted(&request).unwrap();
        assert_eq!(authorization.container_id(), Some("old-container-id"));

        policy
            .record_create_response(
                DockerResourceKind::Container,
                201,
                br#"{"Id":"new-container-id"}"#,
            )
            .unwrap();
        policy
            .resources
            .lock()
            .unwrap()
            .container_names
            .insert("container-alias".to_owned(), "new-container-id".to_owned());

        let rewritten = policy
            .rewrite_authorized_alias_target(
                &request,
                authorization.route,
                authorization.container_id(),
            )
            .unwrap();
        let body = docker_request_body(&rewritten).unwrap();
        let value: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["Container"], "old-container-id");
        let (_, target) = docker_request_line(&rewritten).unwrap();
        assert_eq!(target, "/v1.43/networks/net-owned/connect");
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
    fn volume_create_denies_all_persistent_buildkit_node_state_names() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        for node in [1, 10] {
            let name = format!("buildx_buildkit_{builder}{node}_state");
            let request = api_request(
                "POST",
                "/v1.43/volumes/create",
                format!(r#"{{"Name":"{name}"}}"#).as_bytes(),
            );
            let error = policy
                .authorize(&request)
                .expect_err("Buildx appended-node state volumes are outside Velnor's lifecycle");
            assert!(error.to_string().contains("host-managed"), "{error:#}");
        }

        let generic_marker = format!("guest-buildx_buildkit_{builder}1_state");
        let request = api_request(
            "POST",
            "/v1.43/volumes/create",
            format!(r#"{{"Name":"{generic_marker}"}}"#).as_bytes(),
        );
        assert!(
            policy.authorize(&request).is_ok(),
            "generic names containing the Buildx marker remain ordinary guest volumes"
        );
    }

    #[test]
    fn container_create_live_buildx_gpu_and_volume_mounts_are_guest() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let volume = format!("buildx_buildkit_{builder}0_state");

        let unclaimed_current = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}0"),
            b"{}",
        );
        assert!(
            policy.authorize(&unclaimed_current).is_err(),
            "an unclaimed current-domain daemon name must not fall through to generic create"
        );
        let retired_builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        let retired_daemon = crate::buildkit::daemon_container_name(retired_builder);
        let retired_generation = api_request(
            "POST",
            &format!("/v1.43/containers/create?name={retired_daemon}"),
            b"{}",
        );
        assert!(
            policy.authorize(&retired_generation).is_err(),
            "a retired persistent daemon name must not fall through to generic create"
        );
        let appended_node = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}1"),
            b"{}",
        );
        assert!(
            policy.authorize(&appended_node).is_err(),
            "Buildx append node 1 is outside Velnor's single-node lifecycle"
        );
        let appended_node_ten = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}10"),
            b"{}",
        );
        assert!(
            policy.authorize(&appended_node_ten).is_err(),
            "Buildx append node 10 is outside Velnor's single-node lifecycle"
        );
        let custom_node = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=buildx_buildkit_{builder}custom-node"),
            br#"{"Image":"moby/buildkit:buildx-stable-1","HostConfig":{"Privileged":true,"RestartPolicy":{"Name":"unless-stopped","MaximumRetryCount":0}}}"#,
        );
        assert!(
            policy.authorize(&custom_node).is_err(),
            "a custom Buildx --node name must still hit generic privileged-create denial"
        );

        policy.allow_persistent_builder(&builder).unwrap();
        policy
            .record_persistent_volume_inspect(
                &volume,
                200,
                format!(
                    r#"{{"Name":"{volume}","Driver":"local","Options":{{}},"Labels":{{"velnor.job-id":"velnor-job-old","velnor.buildkit-domain":"{domain_token}"}}}}"#
                )
                .as_bytes(),
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
            .register_persistent_builder_image(&builder, "sha256:buildkit-image")
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
            None,
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
    fn persistent_buildkit_bootstrap_injects_domain_without_daemon_label() {
        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let mut body = serde_json::json!({
            "Image": "moby/buildkit:buildx-stable-1",
            "HostConfig": {},
        });
        inject_persistent_bootstrap_value(
            &mut body,
            "creator-job",
            domain_token,
            "sha256:approved-image",
        )
        .unwrap();
        let labels = body["Labels"].as_object().unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[JOB_ID_LABEL], "creator-job");
        assert_eq!(labels[BUILDKIT_DOMAIN_LABEL], domain_token);
        assert!(!labels.contains_key(DAEMON_ID_LABEL));
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
        let current = crate::buildkit::daemon_state_volume(&test_persistent_builder("branch"));
        let node_one = format!(
            "buildx_buildkit_{}1_state",
            test_persistent_builder("branch")
        );
        let retired = crate::buildkit::daemon_state_volume(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r",
        );
        let generic_marker = "buildx_buildkit_guest-velnor-builder-shared-user-cache0_state";
        let embedded_marker = "guest-buildx_buildkit_velnor-builder-shared-user-cache0_state";
        let snapshot = JobOwnedSnapshot {
            containers: Vec::new(),
            networks: Vec::new(),
            volumes: vec![
                "job-cache".into(),
                generic_marker.into(),
                embedded_marker.into(),
                current.clone(),
                node_one.clone(),
                retired.clone(),
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
            vec![force_remove_volume_args(&[
                "job-cache".into(),
                generic_marker.into(),
                embedded_marker.into(),
            ])]
        );
        assert!(is_persistent_buildkit_volume_name(&current));
        assert!(!is_persistent_buildkit_volume_name(&node_one));
        assert!(is_persistent_buildkit_volume_object(&node_one));
        assert!(is_persistent_buildkit_volume_name(&retired));
        assert!(!is_persistent_buildkit_volume_name(generic_marker));
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
        let retired = crate::buildkit::daemon_state_volume(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r",
        );
        let snapshot = format!("guest-id\tguest-container\t{job_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            format!("job-cache\n{retired}\n"),
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
ddd\tguest-buildx_buildkit_velnor-builder-marker0
";
        assert_eq!(
            docker_client::owned_container_ids_excluding_buildkit_rows(
                &docker_client::parse_owned_container_rows(formatted)
            ),
            vec!["aaa".to_string(), "ccc".to_string(), "ddd".to_string()]
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
id-embedded-marker\tguest-buildx_buildkit_velnor-builder-orphan0\t\t\tcreated
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
        let retired = crate::buildkit::daemon_state_volume(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r",
        );
        let current = crate::buildkit::persistent_builder_name(
            "velnor-builder",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let current_node_ten = format!("buildx_buildkit_{current}10_state");
        let listed = format!(
            "buildx_buildkit_velnor-builder-dead0_state\n\
             buildx_buildkit_velnor-builder-live0_state\n\
             {retired}\n\
             {current_node_ten}\n\
             buildx_buildkit_guest-velnor-builder-shared-user-cache0_state\n\
             guest-buildx_buildkit_velnor-builder-embedded0_state\n\
             buildx_buildkit_velnor-builder-dead-shadow0_state\n\
             buildx_buildkit_velnor-builder-requested-name-slot-3_0_state\n"
        );
        assert_eq!(
            orphan_job_buildkit_volume_names(&listed),
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
        let retired = crate::buildkit::daemon_state_volume(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r",
        );
        let mut outputs = vec![
            String::new(),
            String::new(),
            format!("buildx_buildkit_velnor-builder-race0_state\n{retired}\n"),
            format!("buildx_buildkit_velnor-builder-race0_state\n{retired}\n"),
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
            call.iter().any(|arg| arg == &retired)
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
    fn deferred_no_body_observer_error_never_forwards_success_headers() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        source
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
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
            |status, body| {
                assert_eq!(status, 204);
                assert!(body.is_empty());
                Err(anyhow!("readiness proof missing"))
            },
        )
        .expect_err("failed readiness observer must reject 204 before forwarding it");
        assert!(error.to_string().contains("readiness proof missing"));
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        let forwarded = String::from_utf8(forwarded).unwrap();
        assert!(forwarded.starts_with("HTTP/1.1 502 Bad Gateway"));
        assert!(!forwarded.contains("204 No Content"));
    }

    #[cfg(unix)]
    #[test]
    fn deferred_observer_skips_103_until_final_create_response() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let body = br#"{"Id":"immutable-created-id"}"#;
        let response = format!(
            "HTTP/1.1 103 Early Hints\r\nLink: </buildkit>\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(response.as_bytes()).unwrap();
        drop(source);
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let observed = std::cell::RefCell::new(Vec::new());

        forward_http_response_with_observer(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut sink,
            "POST",
            ForwardResponseOptions {
                defer_response_until_observed: true,
                ..ForwardResponseOptions::default()
            },
            |status, raw| {
                observed.borrow_mut().push((status, raw.to_vec()));
                Ok(())
            },
        )
        .unwrap();
        drop(sink);

        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        assert_eq!(
            *observed.borrow(),
            vec![(201, body.to_vec())],
            "deferred ownership observer must receive only the final response"
        );
        assert_eq!(forwarded, response.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn persistent_bootstrap_starts_stopped_attested_container_before_raw_409_forwarding() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let builder = test_persistent_builder("branch");
        let domain_token = persistent_buildkit_domain_token(&builder).unwrap();
        let target = crate::buildkit::daemon_container_name(&builder);
        let volume = crate::buildkit::daemon_state_volume(&builder);
        let container_inspect = format!(
            r#"{{"Id":"container-from-first-lease","Image":"sha256:persistent-image","Name":"/{target}","Config":{{"Image":"moby/buildkit:buildx-stable-1","Env":["BUILDKIT_SETUP_CGROUPV2_ROOT=1"],"Entrypoint":["/usr/bin/buildkitd-entrypoint"],"Cmd":[],"Labels":{{"velnor.job-id":"first-job","velnor.buildkit-domain":"{domain_token}"}}}},"HostConfig":{{"NetworkMode":"bridge","Privileged":true,"Init":true,"CgroupParent":"/docker/buildx","RestartPolicy":{{"Name":"unless-stopped","MaximumRetryCount":0}}}},"Mounts":[{{"Type":"volume","Name":"{volume}","Destination":"/var/lib/buildkit"}}],"State":{{"Status":"running","Running":true}}}}"#
        )
        .into_bytes();
        let stopped_container_inspect = String::from_utf8(container_inspect.clone())
            .unwrap()
            .replace(
                r#""Status":"running","Running":true"#,
                r#""Status":"exited","Running":false"#,
            )
            .into_bytes();

        let first_lease = DockerLeasePolicy::new("first-job").unwrap();
        first_lease.allow_persistent_builder(&builder).unwrap();
        register_test_persistent_volume_projection(&first_lease, &volume, domain_token);
        first_lease
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        observe_persistent_bootstrap_response_with(
            &first_lease,
            &target,
            201,
            br#"{"Id":"container-from-first-lease"}"#,
            |candidate| {
                assert_eq!(candidate, "container-from-first-lease");
                Ok((200, container_inspect.clone()))
            },
            |_, _| unreachable!("created persistent builder must not take conflict start path"),
        )
        .unwrap();
        assert!(first_lease
            .is_attested_persistent_container(&target)
            .unwrap());

        // A second job has an independent lease policy. Docker reports a
        // name conflict; the observer must inspect the requested name, bind
        // the prior container into this policy only after full attestation,
        // then let the response forwarder return these exact 409 bytes.
        let second_lease = DockerLeasePolicy::new("second-job").unwrap();
        second_lease.allow_persistent_builder(&builder).unwrap();
        register_test_persistent_volume_projection(&second_lease, &volume, domain_token);
        second_lease
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        let conflict_body = br#"{"message":"Conflict. The container name is already in use."}"#;
        let raw_response = format!(
            "HTTP/1.1 409 Conflict\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            conflict_body.len(),
            std::str::from_utf8(conflict_body).unwrap()
        );
        let started = std::cell::Cell::new(false);
        let (mut source, mut host) = UnixStream::pair().unwrap();
        source.write_all(raw_response.as_bytes()).unwrap();
        drop(source);
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        forward_http_response_with_observer(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut sink,
            "POST",
            ForwardResponseOptions {
                defer_response_until_observed: true,
                ..ForwardResponseOptions::default()
            },
            |status, body| {
                assert_eq!(status, 409);
                assert_eq!(body, conflict_body);
                observe_persistent_bootstrap_response_with(
                    &second_lease,
                    &target,
                    status,
                    body,
                    |candidate| {
                        assert_eq!(candidate, target);
                        Ok((200, stopped_container_inspect.clone()))
                    },
                    |policy, candidate| {
                        assert_eq!(candidate, target);
                        assert!(std::str::from_utf8(&stopped_container_inspect)
                            .unwrap()
                            .contains(r#""Status":"exited""#));
                        assert_eq!(
                            policy.persistent_container_id(candidate)?,
                            "container-from-first-lease",
                            "the conflict path must bind restart to the attested immutable ID"
                        );
                        started.set(true);
                        Ok(())
                    },
                )
            },
        )
        .unwrap();
        drop(sink);
        let mut forwarded = Vec::new();
        client.read_to_end(&mut forwarded).unwrap();
        assert_eq!(forwarded, raw_response.as_bytes());
        assert!(
            started.get(),
            "a stopped daemon must start before 409 forwarding"
        );
        assert!(second_lease
            .is_attested_persistent_container(&target)
            .unwrap());

        let running_lease = DockerLeasePolicy::new("third-job-running-reuse").unwrap();
        running_lease.allow_persistent_builder(&builder).unwrap();
        register_test_persistent_volume_projection(&running_lease, &volume, domain_token);
        running_lease
            .register_persistent_builder_image(&builder, "sha256:persistent-image")
            .unwrap();
        let running_ready = std::cell::Cell::new(false);
        observe_persistent_bootstrap_response_with(
            &running_lease,
            &target,
            409,
            conflict_body,
            |candidate| {
                assert_eq!(candidate, target);
                Ok((200, container_inspect.clone()))
            },
            |policy, candidate| {
                assert_eq!(
                    policy.persistent_container_id(candidate)?,
                    "container-from-first-lease"
                );
                running_ready.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(
            running_ready.get(),
            "a later lease must attach to a running, attested daemon after 409"
        );

        let labels_marker = format!("\"velnor.buildkit-domain\":\"{domain_token}\"");
        let wrong_labels_marker =
            "\"velnor.buildkit-domain\":\"f0123456789abcdef0123456789abcdef\"";
        let wrong_domain = String::from_utf8(container_inspect.clone())
            .unwrap()
            .replacen(&labels_marker, wrong_labels_marker, 1)
            .into_bytes();
        let wrong_mount = String::from_utf8(container_inspect.clone())
            .unwrap()
            .replace(&volume, "foreign-state-volume")
            .into_bytes();
        for invalid in [wrong_domain, wrong_mount] {
            let lease = DockerLeasePolicy::new("third-job").unwrap();
            lease.allow_persistent_builder(&builder).unwrap();
            register_test_persistent_volume_projection(&lease, &volume, domain_token);
            lease
                .register_persistent_builder_image(&builder, "sha256:persistent-image")
                .unwrap();
            assert!(observe_persistent_bootstrap_response_with(
                &lease,
                &target,
                409,
                conflict_body,
                |_| Ok((200, invalid.clone())),
                |_, _| unreachable!("unattested conflict must not start"),
            )
            .is_err());
            assert!(!lease.is_attested_persistent_container(&target).unwrap());
        }
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
    fn host_object_inspect_skips_interim_http_response() {
        use std::os::unix::net::UnixListener;

        let dir = unique_unix_dir("velnor-lease-inspect-interim");
        let socket_path = dir.join("engine.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let engine = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut scratch = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut scratch).unwrap();
                assert_ne!(read, 0, "proxy must send a complete inspect request");
                request.extend_from_slice(&scratch[..read]);
            }
            assert!(request.starts_with(b"GET /v1.43/containers/container-id "));
            socket
                .write_all(
                    b"HTTP/1.1 103 Early Hints\r\nLink: </docs>; rel=preload\r\n\r\n\
                      HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}",
                )
                .unwrap();
        });

        let (status, body) =
            inspect_object_on_host(&socket_path, "containers", "container-id", "container")
                .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"{}");
        engine.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn upgrade_response_status_is_checked_before_switching_to_hijack() {
        use std::os::unix::net::UnixStream;

        for (response, expected_status, expected_headers, expected_tail) in [
            (
                b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\n\r\nstream".as_slice(),
                101,
                b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\n\r\n".as_slice(),
                b"stream".as_slice(),
            ),
            (
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\n\r\nstream".as_slice(),
                101,
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\n\r\n".as_slice(),
                b"stream".as_slice(),
            ),
            (
                b"HTTP/1.1 409 Conflict\r\nContent-Length: 3\r\n\r\nno!".as_slice(),
                409,
                b"HTTP/1.1 409 Conflict\r\nContent-Length: 3\r\n\r\n".as_slice(),
                b"no!".as_slice(),
            ),
            (
                b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 409 Conflict\r\nContent-Length: 3\r\n\r\nno!".as_slice(),
                409,
                b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 409 Conflict\r\nContent-Length: 3\r\n\r\n".as_slice(),
                b"no!".as_slice(),
            ),
        ] {
            let (mut host, mut engine) = UnixStream::pair().unwrap();
            let (mut client, _client_peer) = UnixStream::pair().unwrap();
            let wire = response.to_vec();
            engine.write_all(&wire).unwrap();
            engine.shutdown(std::net::Shutdown::Write).unwrap();

            let response = read_upgrade_response(&mut host, &mut client).unwrap();
            assert_eq!(response.status, expected_status);
            assert_eq!(&response.bytes[..response.header_end], expected_headers);
            assert_eq!(&response.bytes[response.header_end..], expected_tail);

            if expected_status == 409 {
                let (mut source, mut host) = UnixStream::pair().unwrap();
                source.write_all(&wire).unwrap();
                drop(source);
                let (mut client, mut sink) = UnixStream::pair().unwrap();
                let mut forwarded_buffer = ResponseBuffer::default();
                let observed = std::cell::RefCell::new(Vec::new());
                forward_http_response_with_observer(
                    &mut host,
                    &mut forwarded_buffer,
                    &mut sink,
                    "POST",
                    ForwardResponseOptions::default(),
                    |status, body| {
                        observed.borrow_mut().push((status, body.to_vec()));
                        Ok(())
                    },
                )
                .unwrap();
                drop(sink);
                let mut forwarded = Vec::new();
                client.read_to_end(&mut forwarded).unwrap();
                assert_eq!(forwarded, wire);
                assert_eq!(*observed.borrow(), vec![(409, b"no!".to_vec())]);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn persistent_upgrade_response_wait_has_a_bounded_deadline() {
        use std::os::unix::net::UnixStream;

        let (mut host, _engine) = UnixStream::pair().unwrap();
        let (mut client, _client_peer) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let result =
            read_upgrade_response_with_timeout(&mut host, &mut client, Duration::from_millis(20));
        let error = match result {
            Ok(_) => panic!("silent Engine response must hit its deadline"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(1));
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

        let guard = DockerLeaseGuard::bind_to_with_test_volume_lock_root(
            listen_path.clone(),
            engine_path,
            "job".into(),
            "daemon".into(),
            dir.join("volume-locks"),
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
        let guard = DockerLeaseGuard::bind_to_with_test_volume_lock_root(
            listen_path.clone(),
            dir.join("missing-engine.sock"),
            "job".into(),
            "daemon".into(),
            dir.join("volume-locks"),
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

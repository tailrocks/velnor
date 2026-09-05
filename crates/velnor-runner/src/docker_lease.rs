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

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
/// Every Docker object created for a job must stay below the package-owned
/// aggregate resource boundary, including containers created through the
/// per-job API proxy (BuildKit and Testcontainers).
pub const JOB_CGROUP_PARENT: &str = "velnor-jobs.slice";
pub const TESTCONTAINERS_LABEL: &str = "org.testcontainers.managed-by=testcontainers";
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";
const HOST_DOCKER_ENDPOINT: &str = "unix:///var/run/docker.sock";
/// Host-visible runtime dir. systemd `PrivateTmp=yes` remaps daemon `/tmp`, so
/// a lease socket there is invisible to host dockerd and the guest bind-mount
/// of `/tmp/vdl-*.sock` is not the proxy.
pub const LEASE_SOCKET_DIR: &str = "/run/velnor";
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

/// The proxy is a capability boundary, not a transparent Docker socket.
/// Resource identifiers are added only after a successful create response and
/// are shared by all connections belonging to this one job lease.
#[derive(Clone)]
struct DockerLeasePolicy {
    resources: Arc<Mutex<OwnedDockerResources>>,
}

#[derive(Default)]
struct OwnedDockerResources {
    containers: BTreeSet<String>,
    networks: BTreeSet<String>,
    volumes: BTreeSet<String>,
    execs: BTreeSet<String>,
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
    Hijack(DockerResourceKind),
}

impl DockerLeasePolicy {
    fn new(job_container: &str) -> Result<Self> {
        let job_container = validate_owned_resource_id(job_container, "job container")?;
        let mut containers = BTreeSet::new();
        containers.insert(job_container);
        Ok(Self {
            resources: Arc::new(Mutex::new(OwnedDockerResources {
                containers,
                ..OwnedDockerResources::default()
            })),
        })
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
        if segments.as_slice() == ["build"] && method == "POST" {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["images", "create"] && method == "POST" {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["images", "json"] && matches!(method.as_str(), "GET" | "HEAD") {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.len() == 3
            && segments[0] == "images"
            && segments[2] == "json"
            && matches!(method.as_str(), "GET" | "HEAD")
        {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["containers", "create"] && method == "POST" {
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Container),
                upgrade,
            );
        }
        if segments.as_slice() == ["networks", "create"] && method == "POST" {
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Network),
                upgrade,
            );
        }
        if segments.as_slice() == ["volumes", "create"] && method == "POST" {
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Volume),
                upgrade,
            );
        }

        match segments.as_slice() {
            ["containers", id] => {
                self.require_owned(DockerResourceKind::Container, id)?;
                if matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                    );
                }
            }
            ["containers", id, operation] => {
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
                if matches!(method.as_str(), "GET" | "HEAD") {
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
                self.require_owned(DockerResourceKind::Volume, id)?;
                if matches!(method.as_str(), "GET" | "HEAD") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Volume),
                        upgrade,
                    );
                }
            }
            _ => {}
        }

        bail!("Docker lease denied {method} {path}: route is not an owned capability")
    }

    fn require_owned(&self, kind: DockerResourceKind, id: &str) -> Result<()> {
        let id = validate_owned_resource_id(id, "Docker resource")?;
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let owned = match kind {
            DockerResourceKind::Container => resources.containers.contains(&id),
            DockerResourceKind::Network => resources.networks.contains(&id),
            DockerResourceKind::Volume => resources.volumes.contains(&id),
            DockerResourceKind::Exec => resources.execs.contains(&id),
        };
        if owned {
            Ok(())
        } else {
            bail!("Docker lease denied foreign {kind:?} resource {id:?}")
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

    fn record_create_response(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
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
            DockerResourceKind::Volume => value.get("Name").and_then(Value::as_str),
        }
        .context("successful Docker create response omitted its resource identifier")?;
        let identifier = validate_owned_resource_id(identifier, "created Docker resource")?;
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
}

fn authorize_docker_route(
    route: AuthorizedDockerRoute,
    upgrade: bool,
) -> Result<AuthorizedDockerRoute> {
    if upgrade && !matches!(route, AuthorizedDockerRoute::Hijack(_)) {
        bail!("Docker lease denied unowned upgrade/tunnel route");
    }
    Ok(route)
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

pub fn guest_docker_socket_host(job_id: &str, unique: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    hasher.update(unique.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut short = [0_u8; 8];
    short.copy_from_slice(&digest[..8]);
    lease_socket_dir().join(format!("vdl-{:016x}.sock", u64::from_be_bytes(short)))
}

/// Prefer systemd `RuntimeDirectory` (`/run/velnor`): host-visible, not
/// remapped by `PrivateTmp`. Tests and unprivileged checkouts cannot create
/// that dir; they fall back to `$TMPDIR/velnor-lease`, still outside the
/// daemon's private `/tmp/vdl-*` path that dockerd never sees.
fn lease_socket_dir() -> PathBuf {
    let runtime = PathBuf::from(LEASE_SOCKET_DIR);
    if std::fs::create_dir_all(&runtime).is_ok() {
        return runtime;
    }
    let fallback = std::env::temp_dir().join("velnor-lease");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
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
/// removed by terminal cleanup (`reclaim_job_owned`). Every path that skips
/// that cleanup — an early `?` between network creation and the step loop, a
/// panic unwinding out of the executor, cleanup returning an error after the
/// network removal itself failed — used to leak the network. Enough leaked
/// `velnor-net-*` networks exhaust Docker's address pool and then EVERY new
/// job fails ("all predefined address pools have been fully subnetted"). The
/// guard makes the executor own the network for its whole lifetime: dropping
/// it while still armed removes the network. Docker refuses to remove a
/// network with active endpoints, so a guard that fires while a job container
/// is still attached cannot break a live job — it fails best-effort and the
/// periodic empty-network sweep removes it once the job is gone.
pub struct JobNetworkGuard {
    network: String,
    armed: bool,
}

impl JobNetworkGuard {
    /// Arm the guard for `network`. Call [`JobNetworkGuard::defuse`] after
    /// terminal cleanup has removed the network itself.
    pub fn arm(network: impl Into<String>) -> Self {
        Self {
            network: network.into(),
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
        let args = force_remove_network_args(std::slice::from_ref(&self.network));
        if let Err(error) = run_host_docker(&args) {
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

pub fn parse_docker_id_list(stdout: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockerContainerState {
    Created,
    Running,
    Restarting,
    Removing,
    Paused,
    Exited,
    Dead,
}

impl DockerContainerState {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "created" => Some(Self::Created),
            "running" => Some(Self::Running),
            "restarting" => Some(Self::Restarting),
            "removing" => Some(Self::Removing),
            "paused" => Some(Self::Paused),
            "exited" => Some(Self::Exited),
            "dead" => Some(Self::Dead),
            _ => None,
        }
    }

    fn safe_to_reclaim(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Removing | Self::Exited | Self::Dead
        )
    }
}

struct StaleJobOwnedSnapshot {
    stopped_container_ids: Vec<String>,
    job_container_absent_or_stopped: bool,
}

/// Parse a reclaim snapshot only when it is structurally valid. The explicit
/// job-container proof is checked on both the initial and immediate snapshots
/// before any owned network or volume can be removed.
fn stale_job_owned_snapshot(job_id: &str, formatted: &str) -> Option<StaleJobOwnedSnapshot> {
    let mut ids = Vec::new();
    let mut job_container_present = false;
    let mut job_container_stopped = true;
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return None;
        }
        let id = fields[0].trim();
        let name = fields[1].trim();
        let label = fields[2].trim();
        let state = DockerContainerState::parse(fields[3])?;
        if id.is_empty() || name.is_empty() || label != job_id {
            return None;
        }
        if name == job_id {
            job_container_present = true;
            job_container_stopped &= state.safe_to_reclaim();
        }
        if !name.contains(BUILDKIT_CONTAINER_NAME_PREFIX) && state.safe_to_reclaim() {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    Some(StaleJobOwnedSnapshot {
        stopped_container_ids: ids,
        job_container_absent_or_stopped: !job_container_present || job_container_stopped,
    })
}

/// Labeled job containers minus docker-container BuildKit daemons.
///
/// BuildKit carries `velnor.job-id`, so the generic owned-container reclaim
/// used to `docker rm --force` it with the 6h step timeout while job-end
/// and doctor also rm'd the same id. Concurrent Engine deletes of a Created
/// `buildx_buildkit_velnor-builder-*` deadlock; the leftover stays. BuildKit
/// has its own prefix reclaim with a 20s bound.
pub fn owned_container_ids_excluding_buildkit(formatted: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (id, names) = line.split_once('\t').unwrap_or((line, line));
        let id = id.trim();
        let names = names.trim();
        if id.is_empty() || names.contains(BUILDKIT_CONTAINER_NAME_PREFIX) {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Host maintenance runs no job step, so a payload-classified command reaching
/// [`run_host_docker`] has no step deadline to inherit. Thirty minutes is the
/// registry-transfer bound: long enough for any maintenance pull, finite.
const MAINTENANCE_PAYLOAD_DEADLINE: Duration = Duration::from_secs(1800);

fn container_ids_from_rm_args(args: &[String]) -> Vec<String> {
    if args.first().map(String::as_str) != Some("rm") {
        return Vec::new();
    }
    args.iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

pub(crate) fn container_rm_args_with_claimed_ids(args: &[String], ids: &[String]) -> Vec<String> {
    let mut claimed = vec![args[0].clone()];
    claimed.extend(
        args.iter()
            .skip(1)
            .filter(|arg| arg.starts_with('-'))
            .cloned(),
    );
    claimed.extend(ids.iter().cloned());
    claimed
}

static IN_FLIGHT_CONTAINER_RM: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Claim container ids for a `docker rm` so job-end and doctor never start a
/// second Engine delete of the same Created BuildKit (that deadlock is the
/// leftover class). Empty `ids` means every id is already in flight: skip.
pub struct DockerContainerRmClaim {
    pub ids: Vec<String>,
}

impl Drop for DockerContainerRmClaim {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let mut held = IN_FLIGHT_CONTAINER_RM
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        for id in &self.ids {
            held.remove(id);
        }
    }
}

pub fn claim_docker_container_rm(args: &[String]) -> Option<DockerContainerRmClaim> {
    let ids = container_ids_from_rm_args(args);
    if ids.is_empty() {
        return None;
    }
    let mut held = IN_FLIGHT_CONTAINER_RM
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let claimed = ids
        .into_iter()
        .filter(|id| held.insert(id.clone()))
        .collect::<Vec<_>>();
    Some(DockerContainerRmClaim { ids: claimed })
}

/// Job ids whose job container is not running. Guest objects for those jobs are orphans.
pub fn orphan_job_ids(formatted: &str) -> Vec<String> {
    let mut seen_jobs = std::collections::BTreeSet::new();
    let mut protected_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            continue;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        if job_id.is_empty() {
            continue;
        }
        let Some(state) = DockerContainerState::parse(fields[2]) else {
            protected_jobs.insert(job_id.to_string());
            continue;
        };
        seen_jobs.insert(job_id.to_string());
        if name == job_id && !state.safe_to_reclaim() {
            protected_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !protected_jobs.contains(job_id))
        .collect()
}

/// True when a `velnor.daemon-id` label value belongs to `daemon_id`: either
/// the shared work root itself or one of its direct `slot-N` children (job
/// containers are labelled with the slot work directory, while daemon
/// startup knows only the shared root).
pub fn daemon_owns_label(owner: &str, daemon_id: &str) -> bool {
    if owner == daemon_id {
        return true;
    }
    Path::new(owner).parent() == Some(Path::new(daemon_id))
        && Path::new(owner)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("slot-").is_some_and(|slot| {
                    !slot.is_empty() && slot.chars().all(|c| c.is_ascii_digit())
                })
            })
}

/// Orphan job ids restricted to containers owned by `daemon_id` (see
/// [`daemon_owns_label`]). Input is the
/// `name \t job-id \t daemon-id \t state` row format from
/// [`list_daemon_owned_job_format_args`].
pub fn daemon_orphan_job_ids(formatted: &str, daemon_id: &str) -> Vec<String> {
    let mut seen_jobs = std::collections::BTreeSet::new();
    let mut protected_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            continue;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        let owner = fields[2].trim();
        if job_id.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        let Some(state) = DockerContainerState::parse(fields[3]) else {
            protected_jobs.insert(job_id.to_string());
            continue;
        };
        seen_jobs.insert(job_id.to_string());
        if name == job_id && !state.safe_to_reclaim() {
            protected_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !protected_jobs.contains(job_id))
        .collect()
}

/// IDs of `velnor/job-ubuntu` siblings with no job label (docker-generated names).
pub fn unlabeled_job_image_ids(formatted: &str) -> Vec<String> {
    unlabeled_testcontainer_ids(formatted)
}

pub fn live_job_ids(formatted: &str) -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let Some(job_id) = fields.get(1).map(|field| field.trim()) else {
            continue;
        };
        if job_id.is_empty() {
            continue;
        }
        if fields.len() != 3 {
            live.insert(job_id.to_string());
            continue;
        }
        let name = fields[0].trim();
        let Some(state) = DockerContainerState::parse(fields[2]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

pub fn live_daemon_job_ids(formatted: &str, daemon_id: &str) -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let Some(job_id) = fields.get(1).map(|field| field.trim()) else {
            continue;
        };
        if job_id.is_empty() {
            continue;
        }
        if fields.len() != 4 {
            live.insert(job_id.to_string());
            continue;
        }
        let name = fields[0].trim();
        let owner = fields[2].trim();
        if owner.is_empty() {
            live.insert(job_id.to_string());
            continue;
        }
        if !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        let Some(state) = DockerContainerState::parse(fields[3]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

/// Rows from [`list_job_buildkit_format_args`]: id, names, job-id, daemon-id, state.
/// A builder is leftover when its job is not running (Created/removing/exited of a
/// finished/cancelled job) or never carried a job label. Live jobs keep their
/// bootstrapping Created builders.
pub fn orphan_job_buildkit_ids(
    formatted: &str,
    live_jobs: &std::collections::BTreeSet<String>,
    daemon_id: Option<&str>,
) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        let names = fields[1].trim();
        let job_id = fields[2].trim();
        let owner = fields[3].trim();
        let Some(state) = DockerContainerState::parse(fields[4]) else {
            continue;
        };
        if id.is_empty()
            || !names.contains(BUILDKIT_CONTAINER_NAME_PREFIX)
            || !state.safe_to_reclaim()
        {
            continue;
        }
        // Startup reclaim is daemon-scoped. An absent ownership label is
        // not proof that this daemon owns the builder; fail closed so a
        // co-located daemon cannot reclaim an unlabeled live resource.
        if let Some(daemon_id) = daemon_id
            && (owner.is_empty() || !daemon_owns_label(owner, daemon_id))
        {
            continue;
        }
        let job_live = if job_id.is_empty() {
            live_jobs
                .iter()
                .any(|live| names.contains(live.trim_start_matches("velnor-job-")))
        } else {
            live_jobs.contains(job_id)
        };
        if !job_live {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Current-job builders including Created/removing. Job-end must delete them
/// even while the job container is still running (cleanup happens before rm).
pub fn job_buildkit_ids_for_job(formatted: &str, job_id: &str, scope: &str) -> Vec<String> {
    let needle = format!("{BUILDKIT_CONTAINER_NAME_PREFIX}{scope}");
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let id = parts.next().unwrap_or("").trim();
        let names = parts.next().unwrap_or("").trim();
        let labeled_job = parts.next().unwrap_or("").trim();
        if id.is_empty() {
            continue;
        }
        if labeled_job == job_id || names.contains(&needle) {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn reclaim_orphan_job_buildkit(
    job_formatted: &str,
    daemon_id: Option<&str>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    reclaim_orphan_job_buildkit_with_live(&live_job_ids(job_formatted), daemon_id, docker)
}

fn reclaim_orphan_job_buildkit_with_live(
    live_jobs: &std::collections::BTreeSet<String>,
    daemon_id: Option<&str>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_job_buildkit_format_args())?;
    let initial_ids = orphan_job_buildkit_ids(&formatted, live_jobs, daemon_id);
    let job_formatted = match daemon_id {
        Some(_) => docker(&list_daemon_owned_job_format_args())?,
        None => docker(&list_owned_job_format_args())?,
    };
    let protected_jobs = match daemon_id {
        Some(daemon_id) => live_daemon_job_ids(&job_formatted, daemon_id),
        None => live_job_ids(&job_formatted),
    };
    let ids = if initial_ids.is_empty() {
        Vec::new()
    } else {
        // Docker state can change between the orphan scan and DELETE. Re-list
        // job containers and BuildKit immediately before removal. An
        // unknown/malformed job state is protected, and deletion still needs
        // the same BuildKit id to be a stopped orphan in both scans.
        let revalidated = docker(&list_job_buildkit_format_args())?;
        let revalidated = orphan_job_buildkit_ids(&revalidated, &protected_jobs, daemon_id);
        initial_ids
            .into_iter()
            .filter(|id| revalidated.binary_search(id).is_ok())
            .collect::<Vec<_>>()
    };
    if !ids.is_empty() {
        remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }
    // Re-list jobs immediately before volume deletion. A job can become live
    // after the container revalidation above, and a not-yet-attached BuildKit
    // volume is otherwise removable even though the job now owns it.
    let volume_protected_jobs = match daemon_id {
        Some(_) => docker(&list_daemon_owned_job_format_args())?,
        None => docker(&list_owned_job_format_args())?,
    };
    let volume_protected_jobs = match daemon_id {
        Some(daemon_id) => live_daemon_job_ids(&volume_protected_jobs, daemon_id),
        None => live_job_ids(&volume_protected_jobs),
    };
    let volume_ids = match daemon_id {
        Some(daemon_id) => {
            let volumes = docker(&list_daemon_owned_job_buildkit_volume_format_args())?;
            daemon_owned_buildkit_volume_names(&volumes, daemon_id, &volume_protected_jobs)
        }
        None => {
            let volumes = docker(&list_job_buildkit_volume_args())?;
            volumes
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty() && name.contains(BUILDKIT_CONTAINER_NAME_PREFIX))
                .filter(|name| {
                    let scope = name
                        .strip_prefix(BUILDKIT_CONTAINER_NAME_PREFIX)
                        .unwrap_or(name);
                    let scope = scope.strip_suffix("_state").unwrap_or(scope);
                    let scope = scope.strip_suffix('0').unwrap_or(scope);
                    let job = format!("velnor-job-{scope}");
                    !volume_protected_jobs.contains(&job)
                        && !volume_protected_jobs
                            .iter()
                            .any(|live| name.contains(live.trim_start_matches("velnor-job-")))
                })
                .map(ToOwned::to_owned)
                .collect()
        }
    };
    if !volume_ids.is_empty() {
        docker(&remove_volume_args(&volume_ids)).map(|_| ())?;
    }
    Ok(())
}

fn daemon_owned_buildkit_volume_names(
    formatted: &str,
    daemon_id: &str,
    protected_jobs: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut names = formatted
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 {
                return None;
            }
            let name = fields[0].trim();
            let job_id = fields[1].trim();
            let owner = fields[2].trim();
            if name.is_empty()
                || job_id.is_empty()
                || !name.contains(BUILDKIT_CONTAINER_NAME_PREFIX)
                || !daemon_owns_label(owner, daemon_id)
                || protected_jobs.contains(job_id)
                || protected_jobs
                    .iter()
                    .any(|live| name.contains(live.trim_start_matches("velnor-job-")))
            {
                return None;
            }
            Some(name.to_string())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// IDs of testcontainers that were created before the lease proxy (no job label).
///
/// This remains a best-effort inspection helper for compatibility. Automatic
/// cleanup must use [`validate_legacy_testcontainer_listing`], which refuses
/// both malformed rows and every unlabeled container before any delete call.
pub fn unlabeled_testcontainer_ids(formatted: &str) -> Vec<String> {
    let mut ids = formatted
        .lines()
        .filter_map(|line| {
            let (id, job_id) = line.split_once('\t')?;
            let id = id.trim();
            if id.is_empty() || !job_id.trim().is_empty() {
                return None;
            }
            Some(id.to_string())
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// A legacy Testcontainers listing is not an ownership proof. Keep its
/// failure modes typed so callers cannot accidentally turn an inspection
/// result into a destructive cleanup decision.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LegacyTestcontainerReclaimError {
    #[error("refusing legacy Testcontainers reclaim: malformed docker ps row {line}: {row:?}")]
    MalformedRow { line: usize, row: String },
    #[error(
        "refusing legacy Testcontainers reclaim: unlabeled containers have no Velnor ownership proof: {ids:?}"
    )]
    Unlabeled { ids: Vec<String> },
}

/// Validate the legacy Testcontainers listing without producing deleteable
/// IDs. Rows carrying a Velnor job label are intentionally ignored here;
/// their cleanup belongs to [`reclaim_job_owned`] or
/// [`reclaim_stale_job_owned`]. An unlabeled row is a hard refusal because the
/// `org.testcontainers.managed-by` label proves only Testcontainers origin,
/// not Velnor ownership.
pub fn validate_legacy_testcontainer_listing(
    formatted: &str,
) -> std::result::Result<(), LegacyTestcontainerReclaimError> {
    let mut unlabeled = Vec::new();
    for (line_number, row) in formatted.lines().enumerate() {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() != 2 || fields[0].trim().is_empty() {
            return Err(LegacyTestcontainerReclaimError::MalformedRow {
                line: line_number + 1,
                row: row.to_string(),
            });
        }
        if fields[1].trim().is_empty() {
            unlabeled.push(fields[0].trim().to_string());
        }
    }

    if unlabeled.is_empty() {
        return Ok(());
    }
    unlabeled.sort();
    unlabeled.dedup();
    Err(LegacyTestcontainerReclaimError::Unlabeled { ids: unlabeled })
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
pub fn rewrite_docker_api_request(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
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
    inject_ownership_labels_value(&mut value, job_id, daemon_id)?;
    if path.ends_with("/containers/create") {
        inject_job_cgroup_parent_value(&mut value)?;
    } else if path.ends_with("/volumes/create") {
        reject_unsafe_volume_create_value(&value)?;
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

/// Force every job-created Docker container into the runner-owned aggregate
/// cgroup. The lease proxy is the only Docker socket exposed to a job, so this
/// also covers BuildKit and nested Testcontainers creates. Runner policy wins
/// over a workflow-supplied `HostConfig.CgroupParent`.
fn inject_job_cgroup_parent_value(value: &mut Value) -> Result<()> {
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
    reject_unsafe_nested_host_controls(&host_config)?;
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
        && value_is_present(options)
    {
        bail!("Docker volume create field {key:?} requests host control access");
    }
    Ok(())
}

fn reject_unsafe_nested_host_controls(host_config: &Map<String, Value>) -> Result<()> {
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    for (key, value) in host_config {
        let unsafe_control = match key.to_ascii_lowercase().as_str() {
            "pidmode" | "ipcmode" | "networkmode" | "cgroupnsmode" | "usernsmode" | "utsmode" => {
                !is_default_mode(value)
            }
            "privileged" => match value {
                Value::Null | Value::Bool(false) => false,
                Value::Bool(true) => true,
                _ => true,
            },
            "capadd" | "devices" | "devicecgrouprules" | "devicerequests" | "securityopt"
            | "runtime" | "sysctls" | "volumedriver" | "volumesfrom" | "volumeoptions"
            | "portbindings" | "publishallports" | "containeridfile" | "restartpolicy" => {
                value_is_present(value)
            }
            "binds" | "mounts" => contains_host_bind(value)? || value_is_present(value),
            "autoremove" | "cgroupparent" | "cpucount" | "cpupercent" | "cpushares"
            | "cpuquota" | "cpuperiod" | "cpurealtimeperiod" | "cpurealtimeruntime"
            | "cpusetcpus" | "cpusetmems" | "memory" | "memoryreservation" | "memoryswap"
            | "memoryswappiness" | "nanocpus" | "oomkilldisable" | "pidslimit"
            | "readonlyrootfs" | "shmsize" | "init" | "stopsignal" | "stoptimeout" | "dns"
            | "dnsoptions" | "dnssearch" | "extrahosts" | "groupadd" | "ulimits"
            | "maskedpaths" | "readonlypaths" => false,
            _ => true,
        };
        if unsafe_control {
            bail!("Docker container create HostConfig field {key:?} requests host control access");
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

fn value_is_present(value: &Value) -> bool {
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

fn contains_host_bind(value: &Value) -> Result<bool> {
    match value {
        Value::Array(values) => {
            for value in values {
                if contains_host_bind(value)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Object(object) => {
            reject_case_insensitive_duplicate_keys(object, "Docker mount object")?;
            let mount_type = object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("type"))
                .and_then(|(_, value)| value.as_str());
            let source = object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("source"))
                .and_then(|(_, value)| value.as_str());
            Ok(
                mount_type.is_none_or(|kind| !kind.eq_ignore_ascii_case("volume"))
                    && source.is_some_and(is_host_bind_source),
            )
        }
        Value::String(bind) => Ok(bind.split_once(':').map_or_else(
            || is_host_bind_source(bind),
            |(source, _)| is_host_bind_source(source),
        )),
        _ => Ok(false),
    }
}

fn is_host_bind_source(path: &str) -> bool {
    let path = path.trim();
    path.starts_with('/')
        || path == "."
        || path == ".."
        || path.starts_with("./")
        || path.starts_with("../")
        || path == "~"
        || path.starts_with("~/")
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

pub fn reclaim_job_owned(
    job_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let listed = docker(&list_owned_containers_args(job_id))?;
    let ids = owned_container_ids_excluding_buildkit(&listed);
    if !ids.is_empty() {
        docker(&force_remove_container_args(&ids)).map(|_| ())?;
    }
    reclaim_listed(
        &list_owned_networks_args(job_id),
        &mut docker,
        force_remove_network_args,
    )?;
    reclaim_listed(
        &list_owned_volumes_args(job_id),
        &mut docker,
        force_remove_volume_args,
    )?;
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
    let Some(initial) = stale_job_owned_snapshot(job_id, &initial) else {
        return Ok(());
    };
    if !initial.job_container_absent_or_stopped {
        return Ok(());
    }
    let revalidated = docker(&list_args)?;
    let Some(revalidated) = stale_job_owned_snapshot(job_id, &revalidated) else {
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
    reclaim_listed(
        &list_owned_volumes_args(job_id),
        &mut docker,
        remove_volume_args,
    )?;
    Ok(())
}

pub fn reclaim_orphan_jobs(mut docker: impl FnMut(&[String]) -> Result<String>) -> Result<()> {
    let formatted = docker(&list_owned_job_format_args())?;
    for job_id in orphan_job_ids(&formatted) {
        reclaim_stale_job_owned(&job_id, &mut docker)?;
    }
    reclaim_orphan_job_buildkit(&formatted, None, &mut docker)
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
    let formatted = docker(&list_daemon_owned_job_format_args())?;
    for job_id in daemon_orphan_job_ids(&formatted, daemon_id) {
        reclaim_stale_job_owned(&job_id, &mut docker)?;
    }
    let live = live_daemon_job_ids(&formatted, daemon_id);
    reclaim_orphan_job_buildkit_with_live(&live, Some(daemon_id), &mut docker)
}

pub fn reclaim_unlabeled_testcontainers(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_testcontainers_format_args())?;
    validate_legacy_testcontainer_listing(&formatted).map_err(Into::into)
}

pub fn reclaim_unlabeled_job_image_siblings(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let mut ids = unlabeled_job_image_ids(&docker(&list_job_image_format_args())?);
    ids.extend(unlabeled_job_image_ids(&docker(
        &list_preflight_format_args(),
    )?));
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    docker(&force_remove_container_args(&ids)).map(|_| ())
}

/// Run one host `docker` command under the deadline its operation class earns.
///
/// Every path is bounded. Before the deadline policy existed only the `rm`
/// family was, and every other maintenance call — `ps`, `inspect`, reclaim
/// listings — waited on a wedged daemon forever.
pub fn run_host_docker(args: &[String]) -> Result<String> {
    let (_, deadline) = crate::docker::deadline_for(args, MAINTENANCE_PAYLOAD_DEADLINE);
    run_host_docker_bounded(args, deadline)
}

/// Run one host `docker` command under an explicit deadline.
///
/// Expiry is a failure. The process is SIGKILLed and the caller gets a typed
/// [`crate::docker::DockerTimeout`] naming the operation class and what to look
/// at, never an empty success.
pub(crate) fn run_host_docker_bounded(
    args: &[String],
    timeout: std::time::Duration,
) -> Result<String> {
    let op = crate::docker::classify(args);
    let rm_claim = claim_docker_container_rm(args);
    if let Some(claim) = rm_claim.as_ref()
        && claim.ids.is_empty()
    {
        return Ok(String::new());
    }
    let claimed_args = rm_claim
        .as_ref()
        .map(|claim| container_rm_args_with_claimed_ids(args, &claim.ids));
    let args = claimed_args.as_deref().unwrap_or(args);
    let mut command = host_docker_command(args)?;
    let child = command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    let started = std::time::Instant::now();
    let (output, expired) = wait_for_child_with_timeout(child, timeout)
        .with_context(|| format!("wait docker {}", args.join(" ")))?;
    crate::docker::observe(
        op,
        started.elapsed(),
        output.status.code().unwrap_or(-1),
        expired,
    );
    if expired {
        // We killed it. Reporting that as success turned a resource leak into
        // a silent one: teardown believed the object was gone.
        return Err(anyhow::Error::new(crate::docker::DockerTimeout::new(
            op, timeout,
        )));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already in progress") {
            return Ok(String::new());
        }
        bail!("docker {} failed: {}", args.join(" "), stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reap a Docker CLI child while keeping its timeout kill tied to the live
/// [`std::process::Child`] handle.
///
/// A watchdog that retains only `Child::id()` has a PID-reuse race: the child
/// can exit and be reaped just before the watchdog's timeout branch runs, and
/// the numeric PID can then belong to an unrelated host process. Polling and
/// killing through the owned handle closes that race. The pipes are drained on
/// reader threads so a verbose Docker CLI cannot deadlock while this thread
/// waits for its exit.
fn wait_for_child_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<(std::process::Output, bool)> {
    let mut stdout = child.stdout.take().context("capture timed child stdout")?;
    let mut stderr = child.stderr.take().context("capture timed child stderr")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });

    let started = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    timed_out = true;
                    // `Child::kill` addresses the still-owned child handle,
                    // never a bare PID that may already have been recycled.
                    let _ = child.kill();
                    break child.wait().context("reap timed-out Docker child");
                }
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            }
            Err(error) => {
                // Do not leave a child running if the status probe itself
                // fails. Reap it before returning the probe error.
                let _ = child.kill();
                let _ = child.wait();
                break Err(anyhow::Error::new(error).context("poll Docker child status"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("timed child stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("timed child stderr reader panicked"))??;
    Ok((
        std::process::Output {
            status: status?,
            stdout,
            stderr,
        },
        timed_out,
    ))
}

fn host_docker_command(args: &[String]) -> Result<std::process::Command> {
    let mut command = std::process::Command::new("docker");
    crate::executor::configure_host_docker_command(&mut command, "docker", args)?;
    command
        .env("DOCKER_HOST", HOST_DOCKER_ENDPOINT)
        .env_remove("DOCKER_CONTEXT");
    Ok(command)
}

fn reclaim_listed(
    list_args: &[String],
    docker: &mut impl FnMut(&[String]) -> Result<String>,
    remove_args: fn(&[String]) -> Vec<String>,
) -> Result<()> {
    let listed = docker(list_args)?;
    let ids = parse_docker_id_list(&listed);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&remove_args(&ids)).map(|_| ())
}

pub struct DockerLeaseGuard {
    listen_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
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
    pub fn bind(listen_path: PathBuf, job_id: String, daemon_id: String) -> Result<Self> {
        Self::bind_to(
            listen_path,
            PathBuf::from(HOST_DOCKER_SOCKET),
            job_id,
            daemon_id,
        )
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
fn bind_unix_lease(
    listen_path: PathBuf,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
) -> Result<DockerLeaseGuard> {
    use std::os::unix::net::UnixListener;

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
    let policy = Arc::new(DockerLeasePolicy::new(&job_id)?);
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
        let authorization = policy.authorize(&bytes)?;
        let request_method = http_request_method(&bytes)?.to_owned();
        let request_wants_close = http_request_wants_close(&bytes);
        let upgrade = request_is_upgrade(&bytes);
        if upgrade && !matches!(authorization, AuthorizedDockerRoute::Hijack(_)) {
            bail!("Docker lease denied unowned upgrade/tunnel route");
        }
        let forwarded = transform_request_buffer(bytes, &mut budget, |request| {
            rewrite_docker_api_request(request, job_id, daemon_id)
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
            match forward_http_response_with_observer(
                host,
                &mut host_buffer,
                &mut client,
                &request_method,
                create_kind.is_some(),
                |status, body| {
                    if let Some(kind) = create_kind {
                        policy.record_create_response(kind, status, body)?;
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
    forward_http_response_with_observer(host, host_buffer, client, request_method, false, |_, _| {
        Ok(())
    })
}

#[cfg(unix)]
fn forward_http_response_with_observer(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    capture_body: bool,
    mut observe: impl FnMut(u16, &[u8]) -> Result<()>,
) -> Result<bool> {
    loop {
        let head = read_http_response_head(host, host_buffer, client, request_method)?;
        client
            .write_all(&head.bytes)
            .context("forward Docker API response headers through job lease")?;
        if head.no_body {
            if (100..200).contains(&head.status) && head.status != 101 {
                continue;
            }
            observe(head.status, &[])?;
            return Ok(!head.close && head.status != 101);
        }
        let mut captured = capture_body.then(Vec::new);
        if head.chunked {
            forward_chunked_response_captured(host, host_buffer, client, &mut captured)?;
        } else if let Some(content_length) = head.content_length {
            if capture_body && content_length > MAX_CREATE_RESPONSE_BODY {
                bail!("Docker create response exceeds ownership capture limit");
            }
            forward_exact_response_body_captured(
                host,
                host_buffer,
                client,
                content_length,
                &mut captured,
            )?;
        } else {
            if capture_body {
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
) -> Result<()> {
    if !buffered.is_empty() && remaining != 0 {
        let take = remaining.min(buffered.len());
        capture_response_bytes(captured, &buffered.as_slice()[..take])?;
        client
            .write_all(&buffered.as_slice()[..take])
            .context("forward buffered Docker API response body")?;
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
        client
            .write_all(&scratch[..read])
            .context("forward Docker API response body")?;
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
        forward_exact_response_body_captured(host, buffered, client, size, captured)?;
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
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn timed_child_kill_uses_owned_handle_and_reaps_child() {
        let child = std::process::Command::new("sh")
            .args(["-c", "exec sleep 5"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn timed child");
        let (output, timed_out) =
            wait_for_child_with_timeout(child, Duration::from_millis(20)).expect("wait child");

        assert!(timed_out);
        assert!(!output.status.success());
    }

    #[test]
    fn job_network_guard_defused_drop_is_noop() {
        // No panic, no docker invocation: the guard type runs `docker` only
        // when armed, so a defused drop must be silent even on a real host.
        let guard = JobNetworkGuard::arm("velnor-net-defused");
        guard.defuse();
    }

    #[test]
    fn job_network_guard_armed_drop_attempts_forced_removal() {
        // Armed drop shells out to the host docker CLI (no injectable runner),
        // so only assert the removable-args contract it relies on: a forced
        // `network rm` of exactly the guarded network.
        let args = force_remove_network_args(&["velnor-net-guarded".to_string()]);
        assert_eq!(args, vec!["network", "rm", "velnor-net-guarded"]);
        let guard = JobNetworkGuard::arm("velnor-net-guarded");
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

    #[cfg(unix)]
    #[test]
    fn lease_policy_denies_foreign_resources_and_unsafe_routes() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let requests = [
            api_request("GET", "/v1.43/containers/foreign/json", b""),
            api_request("POST", "/v1.43/containers/foreign/kill", b""),
            api_request("DELETE", "/v1.43/containers/velnor-job-owned", b""),
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
            container_rm_args_with_claimed_ids(&non_force, &["id-a".into()]),
            remove_container_args(&["id-a".into()])
        );
        let force = force_remove_container_args(&["id-a".into(), "id-b".into()]);
        assert_eq!(
            container_rm_args_with_claimed_ids(&force, &["id-b".into()]),
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
            parent == LEASE_SOCKET_DIR || parent.ends_with("velnor-lease"),
            "lease socket must live in /run/velnor or test fallback velnor-lease, not PrivateTmp /tmp/vdl-*; got {rendered}"
        );
        assert!(
            !rendered.starts_with("/tmp/vdl-"),
            "PrivateTmp remaps daemon /tmp; dockerd would not see {rendered}"
        );
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
    fn rewrite_injects_cgroup_parent_and_overrides_nested_container_policy() {
        let body = br#"{
            "Image":"postgres:18-alpine",
            "HostConfig":{"CgroupParent":"untrusted.slice","Memory":123}
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
        assert_eq!(value["HostConfig"]["Memory"], 123);
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

    #[test]
    fn reclaim_job_owned_removes_guest_container_network_and_volume() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "aaa\tguest-postgres\nbbb\tbuildx_buildkit_velnor-builder-dead0\n".to_string(),
            String::new(),
            "guest-net\n".to_string(),
            String::new(),
            "guest-vol\n".to_string(),
            String::new(),
        ];
        reclaim_job_owned("velnor-job-1", |args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_owned_containers_args("velnor-job-1"));
        assert_eq!(calls[1], force_remove_container_args(&["aaa".into()]));
        assert_eq!(calls[2], list_owned_networks_args("velnor-job-1"));
        assert_eq!(calls[3], force_remove_network_args(&["guest-net".into()]));
        assert_eq!(calls[4], list_owned_volumes_args("velnor-job-1"));
        assert_eq!(calls[5], force_remove_volume_args(&["guest-vol".into()]));
        assert!(outputs.is_empty());
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
            String::new(),
        ];
        reclaim_stale_job_owned(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[2], remove_container_args(&["guest-id".into()]));
        assert_eq!(calls[4], force_remove_network_args(&["guest-net".into()]));
        assert_eq!(calls[6], remove_volume_args(&["guest-vol".into()]));
        assert!(calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg == "--force")));
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
    fn orphan_job_ids_keep_live_jobs_and_reclaim_finished_guest_siblings() {
        let formatted = "\
velnor-job-live\tvelnor-job-live\trunning
guest-pg\tvelnor-job-live\trunning
guest-old\tvelnor-job-dead\trunning
velnor-job-dead\tvelnor-job-dead\texited
";
        assert_eq!(
            orphan_job_ids(formatted),
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
            owned_container_ids_excluding_buildkit(formatted),
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
            let (op, deadline) = crate::docker::deadline_for(&args, MAINTENANCE_PAYLOAD_DEADLINE);
            assert!(op.is_control_plane(), "{args:?} classified as {op}");
            assert!(deadline < step_default, "{args:?} ({op}) is unbounded");
        }
        assert_eq!(
            crate::docker::deadline_for(
                &force_remove_container_args(&["id".into()]),
                MAINTENANCE_PAYLOAD_DEADLINE
            )
            .1,
            Duration::from_secs(20)
        );
    }

    #[test]
    fn claim_docker_container_rm_single_flights_same_id() {
        let args = force_remove_container_args(&["same-id".into()]);
        let first = claim_docker_container_rm(&args).unwrap();
        assert_eq!(first.ids, vec!["same-id".to_string()]);
        let second = claim_docker_container_rm(&args).unwrap();
        assert!(second.ids.is_empty());
        drop(first);
        let third = claim_docker_container_rm(&args).unwrap();
        assert_eq!(third.ids, vec!["same-id".to_string()]);
    }

    #[test]
    fn orphan_job_buildkit_ids_keep_live_created_and_reclaim_ended_created_removing() {
        let live = live_job_ids(
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
            orphan_job_buildkit_ids(formatted, &live, None),
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
            orphan_job_buildkit_ids(formatted, &BTreeSet::new(), Some(daemon)),
            vec!["owned".to_string()]
        );
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
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
            "buildx_buildkit_velnor-builder-dead0_state\nbuildx_buildkit_velnor-builder-live0_state\n"
                .to_string(),
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
            "velnor-job-race\tvelnor-job-race\trunning\n".to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_job_buildkit_format_args());
        assert_eq!(calls[1], list_owned_job_format_args());
        assert_eq!(calls[2], list_job_buildkit_format_args());
        assert_eq!(calls[3], list_owned_job_format_args());
        assert_eq!(calls[4], list_job_buildkit_volume_args());
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
            String::new(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
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
        assert_eq!(
            calls[6],
            remove_volume_args(&["buildx_buildkit_velnor-builder-race0_state".into()])
        );
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
            "velnor-job-race\tvelnor-job-race\trunning\n".to_string(),
            "buildx_buildkit_velnor-builder-race0_state\n".to_string(),
        ];
        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), None, &mut |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_job_buildkit_format_args());
        assert_eq!(calls[1], list_owned_job_format_args());
        assert_eq!(calls[2], list_owned_job_format_args());
        assert_eq!(calls[3], list_job_buildkit_volume_args());
        assert!(!calls
            .iter()
            .any(|call| { call.starts_with(&["volume".into(), "rm".into(), "--force".into()]) }));
    }

    #[test]
    fn unlabeled_testcontainer_ids_keep_only_pre_lease_orphans() {
        let formatted = "aaa\t\nbbb\tvelnor-job-live\nccc\t\n";
        assert_eq!(
            unlabeled_testcontainer_ids(formatted),
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
            error.downcast_ref::<LegacyTestcontainerReclaimError>(),
            Some(LegacyTestcontainerReclaimError::Unlabeled { ids })
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
            error.downcast_ref::<LegacyTestcontainerReclaimError>(),
            Some(LegacyTestcontainerReclaimError::MalformedRow { line: 1, row })
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
        assert!(daemon_owns_label(daemon, daemon));
        assert!(daemon_owns_label(
            "/var/lib/velnor-fleet/work/slot-3",
            daemon
        ));
        assert!(!daemon_owns_label("/var/lib/velnor-other/work", daemon));
        assert!(!daemon_owns_label(
            "/var/lib/velnor-fleet/work/slot-3/nested",
            daemon
        ));
        assert!(!daemon_owns_label(
            "/var/lib/velnor-fleet/work/slots",
            daemon
        ));
        assert!(!daemon_owns_label("", daemon));
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
            daemon_orphan_job_ids(formatted, daemon),
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
            daemon_owned_buildkit_volume_names(formatted, daemon, &protected),
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

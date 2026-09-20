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
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "docker_lease_protocol_tests.rs"]
mod protocol_tests;

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
pub const LEASE_ID_LABEL: &str = "velnor.lease-id";
/// Durable identity label on persistent BuildKit daemon containers. Owner
/// registry membership and this exact label are both required before host
/// maintenance enters or removes a named daemon.
pub const BUILDKIT_BUILDER_LABEL: &str = "velnor.buildkit-builder";
pub const BUILDKIT_OWNER_TOKEN_LABEL: &str = "velnor.buildkit-owner-token";
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
const UNIX_SOCKET_PATH_LIMIT: usize = 100;

const MAX_PROXY_BODY: usize = 32 * 1024 * 1024;
const MAX_PROXY_HEADER: usize = 64 * 1024;
const MAX_PROXY_LINE: usize = 8 * 1024;
const MAX_LEASE_CONNECTIONS: usize = 64;
const MAX_LEASE_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
const PROXY_COPY_BUFFER: usize = 64 * 1024;
const PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const PROXY_UNFRAMED_RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);
const PROXY_MAX_UPGRADE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const MAX_OWNED_DOCKER_RESOURCES: usize = 1024;
const MAX_OWNED_DOCKER_RESOURCE_ID: usize = 256;
const MAX_CREATE_RESPONSE_BODY: usize = 64 * 1024;
const MAX_CREATE_RESPONSE_WIRE_BODY: usize = 256 * 1024;
const MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS: usize = 16 * 1024;
const MAX_BUILDKIT_ARCHIVE_BODY: usize = 64 * 1024;
const MAX_BUILDKIT_ARCHIVE_FILE: usize = 16 * 1024;
const MAX_BUILDKIT_READINESS_ATTEMPTS: usize = 16;

/// The proxy is a capability boundary, not a transparent Docker socket.
/// Resource identifiers are added only after a successful create response and
/// are shared by all connections belonging to this one job lease.
#[derive(Clone)]
struct DockerLeasePolicy {
    resources: Arc<Mutex<OwnedDockerResources>>,
    lease_id: String,
    volume_namespace: String,
    #[cfg(test)]
    test_buildkit_engine: Arc<AtomicBool>,
    #[cfg(test)]
    test_persist_buildkit_bootstrap: Arc<AtomicBool>,
}

#[derive(Default)]
struct OwnedDockerResources {
    containers: BTreeSet<String>,
    /// Guest-visible names pinned to the immutable IDs returned by Engine.
    /// Forwarding the ID prevents a deleted name from retargeting another
    /// lease's object after Docker reuses that name.
    container_names: BTreeMap<String, String>,
    /// Exact persistent BuildKit daemon aliases admitted only after Velnor
    /// claimed their bounded builder record. A workflow cannot grow the
    /// durable builder registry with raw Buildx creates through its socket.
    admitted_buildkit_daemons: BTreeMap<String, PersistentBuildKitAdmission>,
    /// Buildx ExecCreate IDs tied to the exact admitted daemon and command.
    /// Generic job-container exec IDs stay in `execs` and never inherit this
    /// privileged daemon capability.
    persistent_buildkit_execs: BTreeMap<String, PersistentBuildKitExec>,
    networks: BTreeSet<String>,
    /// Same immutable-ID pinning for network aliases.
    network_names: BTreeMap<String, String>,
    volumes: BTreeSet<String>,
    volume_names: BTreeMap<String, String>,
    admitted_buildkit_volumes: BTreeSet<String>,
    execs: BTreeSet<String>,
    pending_create_reservations: usize,
    /// Runner-created network shared only by this job container and its
    /// nested services.
    job_network: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistentBuildKitAdmission {
    builder: String,
    state_volume: String,
    owner_token: String,
    container_id: Option<String>,
    network_reconciled: bool,
    approved_config: Vec<u8>,
    bootstrap_phase: crate::buildkit::BuilderBootstrapPhase,
    readiness_attempts: usize,
    archive_in_flight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistentBuildKitExecCommand {
    Workers,
    Version,
    DialStdio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistentBuildKitExecPhase {
    Created,
    Starting,
    Streamed,
    Inspecting,
    Inspected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistentBuildKitExecCreate {
    parent_container_id: String,
    command: PersistentBuildKitExecCommand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistentBuildKitExec {
    container_id: String,
    command: PersistentBuildKitExecCommand,
    phase: PersistentBuildKitExecPhase,
}

fn persistent_exec_create_phase_allowed(
    command: PersistentBuildKitExecCommand,
    phase: crate::buildkit::BuilderBootstrapPhase,
) -> bool {
    match command {
        PersistentBuildKitExecCommand::Workers | PersistentBuildKitExecCommand::Version => {
            matches!(
                phase,
                crate::buildkit::BuilderBootstrapPhase::Started
                    | crate::buildkit::BuilderBootstrapPhase::Ready
            )
        }
        PersistentBuildKitExecCommand::DialStdio => {
            phase == crate::buildkit::BuilderBootstrapPhase::Ready
        }
    }
}

struct CreateReservation {
    resources: Arc<Mutex<OwnedDockerResources>>,
    active: bool,
    retain_on_drop: bool,
}

impl CreateReservation {
    fn finish(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        resources.pending_create_reservations = resources
            .pending_create_reservations
            .checked_sub(1)
            .context("Docker lease create reservation disappeared")?;
        self.active = false;
        self.retain_on_drop = false;
        Ok(())
    }

    /// Keep the quota slot if an Engine write may have created an object but
    /// the proxy cannot read its complete response. Releasing an ambiguous
    /// reservation would let a client disconnect repeatedly and create an
    /// unbounded number of untracked resources.
    fn retain_if_create_may_have_committed(&mut self) {
        self.retain_on_drop = true;
    }
}

impl Drop for CreateReservation {
    fn drop(&mut self) {
        if !self.active || self.retain_on_drop {
            return;
        }
        if let Ok(mut resources) = self.resources.lock() {
            resources.pending_create_reservations =
                resources.pending_create_reservations.saturating_sub(1);
        }
    }
}

struct PersistentBuildKitArchiveReservation {
    policy: Arc<DockerLeasePolicy>,
    admission: PersistentBuildKitAdmission,
    container_id: String,
    completed: bool,
}

impl PersistentBuildKitArchiveReservation {
    fn complete(&mut self, status: u16) -> Result<()> {
        if (200..300).contains(&status) {
            self.policy.transition_persistent_buildkit_phase(
                &self.admission,
                &self.container_id,
                crate::buildkit::BuilderBootstrapPhase::Created,
                crate::buildkit::BuilderBootstrapPhase::Archived,
            )?;
        }
        self.policy.complete_persistent_buildkit_archive(
            &self.admission,
            &self.container_id,
            status,
        )?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PersistentBuildKitArchiveReservation {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self
                .policy
                .cancel_persistent_buildkit_archive(&self.admission, &self.container_id);
        }
    }
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
    fn new(job_container: &str) -> Result<Self> {
        let job_container = validate_owned_resource_id(job_container, "job container")?;
        let lease_id = uuid::Uuid::new_v4().simple().to_string();
        let volume_namespace = blake3::hash(lease_id.as_bytes()).to_hex().to_string();
        let mut containers = BTreeSet::new();
        containers.insert(job_container);
        Ok(Self {
            lease_id,
            volume_namespace: volume_namespace[..32].to_owned(),
            resources: Arc::new(Mutex::new(OwnedDockerResources {
                containers,
                container_names: BTreeMap::new(),
                admitted_buildkit_daemons: BTreeMap::new(),
                persistent_buildkit_execs: BTreeMap::new(),
                networks: BTreeSet::new(),
                network_names: BTreeMap::new(),
                volumes: BTreeSet::new(),
                volume_names: BTreeMap::new(),
                admitted_buildkit_volumes: BTreeSet::new(),
                execs: BTreeSet::new(),
                pending_create_reservations: 0,
                job_network: None,
            })),
            #[cfg(test)]
            test_buildkit_engine: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            test_persist_buildkit_bootstrap: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    fn enable_test_buildkit_engine(&self) {
        self.test_buildkit_engine.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn enable_test_bootstrap_persistence(&self) {
        self.test_persist_buildkit_bootstrap
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn uses_test_buildkit_engine(&self) -> bool {
        self.test_buildkit_engine.load(Ordering::SeqCst)
    }

    fn should_persist_buildkit_bootstrap(&self) -> bool {
        #[cfg(test)]
        {
            !self.uses_test_buildkit_engine()
                || self.test_persist_buildkit_bootstrap.load(Ordering::SeqCst)
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    fn set_job_network(&self, network: &str) -> Result<()> {
        let network = validate_owned_resource_id(network, "job network")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources
            .job_network
            .as_ref()
            .is_some_and(|existing| existing != &network)
        {
            bail!("Docker lease job network changed after binding");
        }
        resources.job_network = Some(network);
        Ok(())
    }

    fn current_job_network(&self) -> Result<Option<String>> {
        self.resources
            .lock()
            .map(|resources| resources.job_network.clone())
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))
    }

    fn reserve_create(&self) -> Result<CreateReservation> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let reserved =
            owned_resource_count(&resources).saturating_add(resources.pending_create_reservations);
        if reserved >= MAX_OWNED_DOCKER_RESOURCES {
            return Err(LeaseDeny::forbidden("Docker lease ownership registry is full").into());
        }
        resources.pending_create_reservations += 1;
        Ok(CreateReservation {
            resources: self.resources.clone(),
            active: true,
            retain_on_drop: false,
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
        // These routes reach dockerd's one built-in BuildKit daemon. They do
        // not carry Velnor's builder or trust-group identity, so allowing
        // them would bypass the durable trust-scoped docker-container pool.
        if matches!(segments.as_slice(), ["grpc"] | ["session"]) && method == "POST" {
            return Err(LeaseDeny::forbidden(
                "Docker lease denied shared dockerd BuildKit tunnel; use an admitted trust-scoped builder",
            ));
        }
        if segments.as_slice() == ["build"] && method == "POST" {
            return Err(LeaseDeny::forbidden(
                "Docker lease denied shared dockerd BuildKit; use an admitted trust-scoped builder",
            ));
        }
        if segments.as_slice() == ["images", "create"] && method == "POST" {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["images", "json"] && matches!(method.as_str(), "GET" | "HEAD") {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        // Image names may contain slashes (`moby/buildkit:tag`). Match
        // `/images/<ref>/json` by first/last segment, not a fixed length.
        if segments.first() == Some(&"images")
            && segments.last() == Some(&"json")
            && segments.len() >= 3
            && matches!(method.as_str(), "GET" | "HEAD")
        {
            return authorize_docker_route(AuthorizedDockerRoute::DaemonRead, upgrade);
        }
        if segments.as_slice() == ["containers", "create"] && method == "POST" {
            validate_create_request_size(request).map_err(create_capability_deny)?;
            self.validate_container_create_request(request)
                .map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Container),
                upgrade,
            );
        }
        if segments.as_slice() == ["networks", "create"] && method == "POST" {
            validate_create_request_size(request).map_err(create_capability_deny)?;
            validate_network_create_request(request).map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Network),
                upgrade,
            );
        }
        if segments.as_slice() == ["volumes", "create"] && method == "POST" {
            validate_create_request_size(request).map_err(create_capability_deny)?;
            self.persistent_buildkit_volume_admission(request)
                .map_err(create_capability_deny)?;
            validate_volume_create_request(request).map_err(create_capability_deny)?;
            return authorize_docker_route(
                AuthorizedDockerRoute::Create(DockerResourceKind::Volume),
                upgrade,
            );
        }

        match segments.as_slice() {
            ["containers", id] => {
                let persistent_buildkit =
                    self.persistent_buildkit_container_admission(id)?.is_some();
                if persistent_buildkit && method == "DELETE" {
                    return Err(LeaseDeny::forbidden(
                        "persistent BuildKit daemon removal is runner-managed",
                    ));
                }
                self.require_owned(DockerResourceKind::Container, id)?;
                if matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                        upgrade,
                    );
                }
            }
            ["containers", id, operation] => {
                let persistent_buildkit =
                    self.persistent_buildkit_container_admission(id)?.is_some();
                if persistent_buildkit {
                    let admission = self
                        .persistent_buildkit_container_admission(id)?
                        .context("persistent BuildKit admission disappeared")?;
                    if method == "GET"
                        && *operation == "json"
                        && self.initial_buildkit_bootstrap_inspect(&request)?.is_some()
                    {
                        self.require_initial_buildkit_daemon_absent(&admission)?;
                        return authorize_docker_route(
                            AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                            upgrade,
                        );
                    }
                    let container_id = self.resolve_owned_id(DockerResourceKind::Container, id)?;
                    if admission.container_id.as_deref() != Some(container_id.as_str()) {
                        return Err(LeaseDeny::forbidden(
                            "persistent BuildKit request is not bound to its registered daemon ID",
                        ));
                    }
                    match (method.as_str(), *operation) {
                        ("GET", "json") => {
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                                upgrade,
                            );
                        }
                        ("PUT", "archive") => {
                            if admission.bootstrap_phase
                                != crate::buildkit::BuilderBootstrapPhase::Created
                            {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit archive is allowed only after a fresh create and before start",
                                ));
                            }
                            validate_persistent_buildkit_archive(
                                request,
                                &admission.approved_config,
                            )
                            .map_err(create_capability_deny)?;
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                                upgrade,
                            );
                        }
                        ("POST", "start") => {
                            validate_persistent_buildkit_container_start(request)
                                .map_err(create_capability_deny)?;
                            if !matches!(
                                admission.bootstrap_phase,
                                crate::buildkit::BuilderBootstrapPhase::Archived
                                    | crate::buildkit::BuilderBootstrapPhase::Started
                                    | crate::buildkit::BuilderBootstrapPhase::Ready
                            ) {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit start requires an archived or previously admitted daemon",
                                ));
                            }
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
                                upgrade,
                            );
                        }
                        ("POST", "exec") => {
                            validate_create_request_size(request)
                                .map_err(create_capability_deny)?;
                            let (parent, command) = persistent_buildkit_exec_command(request)
                                .and_then(|parsed| {
                                    parsed.context(
                                        "persistent BuildKit ExecCreate parser lost the route",
                                    )
                                })
                                .map_err(create_capability_deny)?;
                            if self.resolve_owned_id(DockerResourceKind::Container, &parent)?
                                != container_id
                            {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit ExecCreate parent ID changed",
                                ));
                            }
                            if !persistent_exec_create_phase_allowed(
                                command,
                                admission.bootstrap_phase,
                            ) {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit ExecCreate is not allowed in this bootstrap phase",
                                ));
                            }
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Create(DockerResourceKind::Exec),
                                upgrade,
                            );
                        }
                        _ => {
                            return Err(LeaseDeny::forbidden(
                                "persistent BuildKit exposes only runner-managed inspect, archive, start, and exact Exec routes",
                            ));
                        }
                    }
                }
                self.require_owned(DockerResourceKind::Container, id)?;
                if operation == &"exec" && method == "POST" {
                    validate_create_request_size(request).map_err(create_capability_deny)?;
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
                    ) | ("PUT", "archive")
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
                if let Some(exec) = self.persistent_buildkit_exec(id)? {
                    match (method.as_str(), *operation) {
                        ("POST", "start") => {
                            if !upgrade {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit ExecStart requires Docker TCP hijack",
                                ));
                            }
                            validate_persistent_buildkit_exec_start(request)
                                .map_err(create_capability_deny)?;
                            if exec.phase != PersistentBuildKitExecPhase::Created {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit ExecStart is out of sequence",
                                ));
                            }
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Hijack(DockerResourceKind::Exec),
                                upgrade,
                            );
                        }
                        ("GET", "json") => {
                            validate_persistent_buildkit_exec_inspect(request)
                                .map_err(create_capability_deny)?;
                            if !matches!(
                                exec.command,
                                PersistentBuildKitExecCommand::Workers
                                    | PersistentBuildKitExecCommand::Version
                            ) || exec.phase != PersistentBuildKitExecPhase::Streamed
                            {
                                return Err(LeaseDeny::forbidden(
                                    "persistent BuildKit ExecInspect is out of sequence",
                                ));
                            }
                            return authorize_docker_route(
                                AuthorizedDockerRoute::Owned(DockerResourceKind::Exec),
                                upgrade,
                            );
                        }
                        _ => {
                            return Err(LeaseDeny::forbidden(
                                "persistent BuildKit exec ID is limited to its admitted Start/Inspect sequence",
                            ));
                        }
                    }
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
                    let body = docker_request_body(request)?;
                    let value =
                        parse_create_value(body).context("parse Docker network connect request")?;
                    let object = value
                        .as_object()
                        .context("Docker network request must be an object")?;
                    reject_case_insensitive_duplicate_keys(object, "Docker network request")?;
                    if let Some(endpoint_config) = object
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case("EndpointConfig"))
                        .map(|(_, value)| value)
                    {
                        reject_endpoint_network_id(endpoint_config, "Docker EndpointConfig")?;
                    }
                    let container = value
                        .get("Container")
                        .and_then(Value::as_str)
                        .context("Docker network request must name a container")?;
                    if self
                        .persistent_buildkit_container_admission(container)?
                        .is_some()
                    {
                        return Err(LeaseDeny::forbidden(
                            "persistent BuildKit network topology is runner-managed",
                        ));
                    } else {
                        self.require_owned_container_in_body(request)?;
                    }
                    return authorize_docker_route(
                        AuthorizedDockerRoute::Owned(DockerResourceKind::Network),
                        upgrade,
                    );
                }
            }
            ["volumes", id] => {
                let persistent_buildkit = self
                    .persistent_buildkit_volume_admission_by_name(id)?
                    .is_some();
                self.require_owned(DockerResourceKind::Volume, id)?;
                if persistent_buildkit && method == "DELETE" {
                    return Err(LeaseDeny::forbidden(
                        "persistent BuildKit state-volume removal is runner-managed",
                    ));
                }
                if matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
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

    fn resolve_owned_id(&self, kind: DockerResourceKind, id: &str) -> Result<String> {
        let id = validate_owned_resource_id(id, "Docker resource")?;
        let (buildkit_admission, owned_id) = {
            let resources = self
                .resources
                .lock()
                .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
            let admission = match kind {
                DockerResourceKind::Container => resources
                    .admitted_buildkit_daemons
                    .get(&id)
                    .cloned()
                    .or_else(|| {
                        resources
                            .admitted_buildkit_daemons
                            .values()
                            .find(|admission| admission.container_id.as_deref() == Some(&id))
                            .cloned()
                    }),
                DockerResourceKind::Volume => resources
                    .admitted_buildkit_daemons
                    .values()
                    .find(|admission| admission.state_volume == id)
                    .cloned(),
                DockerResourceKind::Network | DockerResourceKind::Exec => None,
            };
            let owned_id = match kind {
                DockerResourceKind::Container => resources
                    .containers
                    .get(&id)
                    .cloned()
                    .or_else(|| resources.container_names.get(&id).cloned()),
                DockerResourceKind::Network => resources
                    .networks
                    .get(&id)
                    .cloned()
                    .or_else(|| resources.network_names.get(&id).cloned())
                    .or_else(|| {
                        resources
                            .job_network
                            .as_ref()
                            .filter(|network| *network == &id)
                            .cloned()
                    }),
                DockerResourceKind::Volume => resources.volume_names.get(&id).cloned(),
                DockerResourceKind::Exec => resources
                    .execs
                    .contains(&id)
                    .then(|| id.clone())
                    .or_else(|| {
                        resources
                            .persistent_buildkit_execs
                            .contains_key(&id)
                            .then(|| id.clone())
                    }),
            };
            (admission, owned_id)
        };
        if let Some(admission) = buildkit_admission {
            let proof = match kind {
                DockerResourceKind::Container => (|| -> Result<String> {
                    let verified_id = self.verify_persistent_buildkit_container(&admission, &id)?;
                    let daemon = crate::buildkit::daemon_container_name(&admission.builder);
                    let mut resources = self.resources.lock().map_err(|_| {
                        anyhow::anyhow!("Docker lease ownership registry is poisoned")
                    })?;
                    let stored = resources
                        .admitted_buildkit_daemons
                        .get_mut(&daemon)
                        .context(
                            "persistent BuildKit admission disappeared after identity proof",
                        )?;
                    if stored.owner_token != admission.owner_token {
                        bail!("persistent BuildKit owner token changed after identity proof");
                    }
                    if stored.container_id.as_deref() != Some(&verified_id) {
                        stored.container_id = Some(verified_id.clone());
                        stored.network_reconciled = false;
                    }
                    Ok(verified_id)
                })(),
                DockerResourceKind::Volume => self
                    .require_admitted_buildkit_volume(&admission)
                    .map(|()| admission.state_volume.clone()),
                _ => Err(anyhow::anyhow!(
                    "BuildKit identity admission has an invalid resource kind"
                )),
            };
            return proof.map_err(|error| {
                LeaseDeny::forbidden(format!(
                    "persistent BuildKit identity check failed: {error:#}"
                ))
            });
        }
        owned_id.ok_or_else(|| {
            // A foreign object is invisible to this job, not forbidden:
            // answer docker's own "no such object" status so clients take
            // their absent-object branches instead of dying on a transport
            // error.
            LeaseDeny::not_found(format!(
                "Docker lease denied foreign {kind:?} resource {id:?}"
            ))
        })
    }

    fn verify_persistent_buildkit_container(
        &self,
        admission: &PersistentBuildKitAdmission,
        requested_id: &str,
    ) -> Result<String> {
        #[cfg(test)]
        if self.uses_test_buildkit_engine() {
            let registered = admission
                .container_id
                .as_deref()
                .context("test BuildKit daemon has no registered ID")?;
            if requested_id == registered
                || requested_id == crate::buildkit::daemon_container_name(&admission.builder)
            {
                return Ok(registered.to_owned());
            }
            bail!("test BuildKit request does not match its registered daemon ID");
        }
        crate::buildkit::verify_admitted_builder_daemon(
            &admission.builder,
            &admission.owner_token,
            requested_id,
        )
    }

    fn verify_persistent_buildkit_running(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
    ) -> Result<()> {
        #[cfg(test)]
        if self.uses_test_buildkit_engine() {
            return Ok(());
        }
        if !crate::buildkit::verify_admitted_builder_daemon_running(
            &admission.builder,
            &admission.owner_token,
            container_id,
        )? {
            bail!("admitted BuildKit daemon is not running");
        }
        Ok(())
    }

    fn require_owned(&self, kind: DockerResourceKind, id: &str) -> Result<()> {
        self.resolve_owned_id(kind, id).map(|_| ())
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

    fn owned_volume_names(
        &self,
        buildkit_admission: Option<&PersistentBuildKitAdmission>,
    ) -> Result<BTreeSet<String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let mut volumes = resources
            .volume_names
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(admission) = buildkit_admission {
            // The persistent state volume belongs only to this exact admitted
            // BuildKit daemon create. It must never be treated as a generally
            // mountable job volume.
            volumes.insert(admission.state_volume.clone());
        }
        Ok(volumes)
    }

    fn volume_name_ids(&self) -> Result<BTreeMap<String, String>> {
        self.resources
            .lock()
            .map(|resources| resources.volume_names.clone())
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))
    }

    fn private_volume_name(&self, alias: &str) -> Result<String> {
        let alias = validate_owned_resource_id(alias, "created Docker volume name")?;
        let digest = blake3::hash(alias.as_bytes()).to_hex().to_string();
        Ok(format!(
            "velnor-jobvol-{}-{}",
            self.volume_namespace,
            &digest[..32]
        ))
    }

    fn persistent_buildkit_container_admission(
        &self,
        id: &str,
    ) -> Result<Option<PersistentBuildKitAdmission>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources
            .admitted_buildkit_daemons
            .get(id)
            .cloned()
            .or_else(|| {
                resources
                    .admitted_buildkit_daemons
                    .values()
                    .find(|admission| admission.container_id.as_deref() == Some(id))
                    .cloned()
            }))
    }

    fn initial_buildkit_bootstrap_inspect(
        &self,
        request: &[u8],
    ) -> Result<Option<PersistentBuildKitAdmission>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("GET") {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["containers", name, "json"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(admission) = self.persistent_buildkit_container_admission(name)? else {
            return Ok(None);
        };
        if admission.container_id.is_some()
            || admission.bootstrap_phase != crate::buildkit::BuilderBootstrapPhase::Unverified
            || *name != crate::buildkit::daemon_container_name(&admission.builder)
        {
            return Ok(None);
        }
        Ok(Some(admission))
    }

    fn require_initial_buildkit_daemon_absent(
        &self,
        admission: &PersistentBuildKitAdmission,
    ) -> Result<()> {
        #[cfg(test)]
        if self.uses_test_buildkit_engine() {
            return Ok(());
        }
        crate::buildkit::require_admitted_builder_first_create_available(
            &admission.builder,
            &admission.owner_token,
        )
        .map_err(|error| {
            LeaseDeny::forbidden(format!(
                "persistent BuildKit initial create is not safe: {error:#}"
            ))
            .into()
        })
    }

    fn validate_initial_buildkit_inspect_response(&self, status: u16) -> Result<()> {
        if status == 404 {
            return Ok(());
        }
        Err(LeaseDeny::forbidden(format!(
            "initial persistent BuildKit inspect expected Engine 404, got {status}"
        ))
        .into())
    }

    fn persistent_buildkit_exec(&self, id: &str) -> Result<Option<PersistentBuildKitExec>> {
        let id = validate_owned_resource_id(id, "persistent BuildKit exec ID")?;
        self.resources
            .lock()
            .map(|resources| resources.persistent_buildkit_execs.get(&id).cloned())
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))
    }

    fn persistent_buildkit_volume_admission_by_name(
        &self,
        name: &str,
    ) -> Result<Option<PersistentBuildKitAdmission>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        Ok(resources
            .admitted_buildkit_daemons
            .values()
            .find(|admission| admission.state_volume == name)
            .cloned())
    }

    fn persistent_buildkit_volume_admission(
        &self,
        request: &[u8],
    ) -> Result<Option<PersistentBuildKitAdmission>> {
        let body = docker_request_body(request)?;
        let value = parse_create_value(body).context("parse Docker volume create request")?;
        let object = value
            .as_object()
            .context("Docker volume create body must be an object")?;
        reject_case_insensitive_duplicate_keys(object, "Docker volume create body")?;
        let name = object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
            .and_then(|(_, value)| value.as_str())
            .unwrap_or_default();
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let admission = resources
            .admitted_buildkit_daemons
            .values()
            .find(|admission| admission.state_volume == name)
            .cloned();
        if admission.is_none() && is_reserved_persistent_buildkit_volume(name) {
            return Err(LeaseDeny::forbidden(format!(
                "Docker lease denied unadmitted persistent BuildKit volume {name:?}"
            ))
            .into());
        }
        Ok(admission)
    }

    fn owned_network_names(&self) -> Result<BTreeSet<String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let mut networks = resources.networks.clone();
        networks.extend(resources.network_names.keys().cloned());
        networks.extend(resources.network_names.values().cloned());
        networks.extend(resources.job_network.iter().cloned());
        Ok(networks)
    }

    fn network_name_ids(&self) -> Result<BTreeMap<String, String>> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let mut ids = resources.network_names.clone();
        if let Some(network) = resources.job_network.as_ref() {
            ids.insert(network.clone(), network.clone());
        }
        Ok(ids)
    }

    fn validate_container_create_request(&self, request: &[u8]) -> Result<()> {
        let admission = self.persistent_buildkit_admission(&request)?;
        if let Some(admission) = admission.as_ref() {
            self.require_admitted_buildkit_volume(admission)?;
        }
        let owned_volume_names = self.owned_volume_names(admission.as_ref())?;
        let owned_network_names = self.owned_network_names()?;
        validate_container_create_request_with_context(
            request,
            &owned_volume_names,
            &owned_network_names,
            admission.as_ref(),
        )
    }

    fn persistent_buildkit_admission(
        &self,
        request: &[u8],
    ) -> Result<Option<PersistentBuildKitAdmission>> {
        let Some(name) = containers_create_query_name(request)? else {
            return Ok(None);
        };
        if !is_reserved_persistent_buildkit_daemon(&name) {
            return Ok(None);
        }
        let resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let Some(admission) = resources.admitted_buildkit_daemons.get(&name) else {
            return Err(LeaseDeny::forbidden(format!(
                "Docker lease denied unadmitted persistent BuildKit daemon {name:?}"
            ))
            .into());
        };
        Ok(Some(admission.clone()))
    }

    fn require_admitted_buildkit_volume(
        &self,
        admission: &PersistentBuildKitAdmission,
    ) -> Result<()> {
        #[cfg(test)]
        if self.uses_test_buildkit_engine() {
            return Ok(());
        }
        let was_created_through_this_lease = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?
            .admitted_buildkit_volumes
            .contains(&admission.state_volume);
        if was_created_through_this_lease {
            return Ok(());
        }
        if admission.container_id.is_none()
            && admission.bootstrap_phase == crate::buildkit::BuilderBootstrapPhase::Unverified
        {
            return self.require_initial_buildkit_daemon_absent(admission);
        }
        crate::buildkit::verify_admitted_builder_volume(
            &admission.builder,
            &admission.owner_token,
            &admission.state_volume,
        )
        .map_err(|error| {
            LeaseDeny::forbidden(format!(
                "persistent BuildKit state-volume check failed: {error:#}"
            ))
            .into()
        })
    }

    fn record_admitted_buildkit_volume_create(
        &self,
        admission: &PersistentBuildKitAdmission,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let value = parse_create_value(body)
            .context("parse successful persistent BuildKit volume create response")?;
        if value.get("Name").and_then(Value::as_str) != Some(&admission.state_volume) {
            bail!("persistent BuildKit volume create returned a different volume name");
        }
        let labels = value
            .get("Labels")
            .and_then(Value::as_object)
            .context("persistent BuildKit volume create response omitted Labels")?;
        if labels.get(BUILDKIT_BUILDER_LABEL).and_then(Value::as_str) != Some(&admission.builder)
            || labels
                .get(BUILDKIT_OWNER_TOKEN_LABEL)
                .and_then(Value::as_str)
                != Some(&admission.owner_token)
        {
            bail!("persistent BuildKit volume create response lacks the admitted owner identity");
        }
        self.resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?
            .admitted_buildkit_volumes
            .insert(admission.state_volume.clone());
        Ok(())
    }

    fn admit_persistent_buildkit_builder(
        &self,
        builder: &str,
        approved_config: &[u8],
    ) -> Result<()> {
        if !crate::buildkit::is_bounded_builder_name(builder) {
            bail!("cannot admit a noncanonical persistent BuildKit builder name");
        }
        let daemon = crate::buildkit::daemon_container_name(builder);
        let state_volume = crate::buildkit::daemon_state_volume(builder);
        let owner_token = crate::buildkit::registered_owner_token(builder)?;
        let config_digest = sha256_hex(approved_config);
        let bootstrap_state = crate::buildkit::registered_owner_bootstrap_state(builder)?;
        let (container_id, bootstrap_phase) = match bootstrap_state {
            None => (None, crate::buildkit::BuilderBootstrapPhase::Unverified),
            Some((id, phase, recorded_digest)) => {
                if recorded_digest.as_deref() != Some(config_digest.as_str()) {
                    bail!("persistent BuildKit config digest differs from its durable daemon identity");
                }
                if phase == crate::buildkit::BuilderBootstrapPhase::Unverified {
                    bail!("persistent BuildKit daemon has no durable ready proof; remove it through the owner-verified reaper before reuse");
                }
                if phase == crate::buildkit::BuilderBootstrapPhase::Created {
                    bail!("persistent BuildKit daemon was interrupted before its approved config archive; remove it through the owner-verified reaper before reuse");
                }
                (Some(id), phase)
            }
        };
        let admission = PersistentBuildKitAdmission {
            builder: builder.to_string(),
            state_volume: state_volume.clone(),
            owner_token,
            container_id,
            network_reconciled: false,
            approved_config: approved_config.to_vec(),
            bootstrap_phase,
            readiness_attempts: 0,
            archive_in_flight: false,
        };
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if let Some(existing) = resources.admitted_buildkit_daemons.get(&daemon) {
            if existing.builder != admission.builder
                || existing.state_volume != admission.state_volume
                || existing.owner_token != admission.owner_token
                || existing.container_id != admission.container_id
                || existing.approved_config != admission.approved_config
                || existing.bootstrap_phase != admission.bootstrap_phase
            {
                bail!("persistent BuildKit daemon alias was admitted with different identity");
            }
        }
        resources
            .admitted_buildkit_daemons
            .insert(daemon, admission);
        Ok(())
    }

    /// Do not expose a persistent BuildKit daemon to the guest until the
    /// runner has proved and reconciled its exact Engine network topology.
    /// New creates call this before forwarding their captured response;
    /// existing daemons call it before their first request in this lease.
    fn ensure_persistent_buildkit_network(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
        job_id: &str,
        daemon_id: &str,
    ) -> Result<()> {
        if admission.network_reconciled && admission.container_id.as_deref() == Some(container_id) {
            return Ok(());
        }
        let should_reconcile = {
            #[cfg(test)]
            {
                !self.uses_test_buildkit_engine()
            }
            #[cfg(not(test))]
            {
                true
            }
        };
        if should_reconcile {
            let network = self
                .current_job_network()?
                .context("persistent BuildKit lease has no private job network")?;
            crate::buildkit::reconcile_admitted_builder_network(
                &admission.builder,
                &admission.owner_token,
                container_id,
                &network,
                job_id,
                daemon_id,
            )?;
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(&daemon)
            .context("persistent BuildKit admission disappeared during network reconciliation")?;
        if stored.builder != admission.builder
            || stored.owner_token != admission.owner_token
            || stored.container_id.as_deref() != Some(container_id)
        {
            bail!("persistent BuildKit identity changed during network reconciliation");
        }
        stored.network_reconciled = true;
        Ok(())
    }

    fn rewrite_docker_api_request(
        &self,
        request: &[u8],
        job_id: &str,
        daemon_id: &str,
    ) -> Result<Vec<u8>> {
        let request = self.rewrite_owned_resource_references(request)?;
        let admission = self.persistent_buildkit_admission(&request)?;
        let mut owned_volume_names = self.owned_volume_names(admission.as_ref())?;
        owned_volume_names.extend(self.volume_name_ids()?.into_values());
        let owned_network_names = self.owned_network_names()?;
        let volume_admission = if docker_request_line(&request)
            .ok()
            .and_then(|(_, target)| canonical_docker_path(target).ok())
            .is_some_and(|path| path.ends_with("/volumes/create"))
        {
            self.persistent_buildkit_volume_admission(&request)?
        } else {
            None
        };
        let job_network = self.current_job_network()?;
        rewrite_docker_api_request_with_context(
            &request,
            job_id,
            daemon_id,
            &owned_volume_names,
            &owned_network_names,
            admission.as_ref(),
            volume_admission.as_ref(),
            Some(&self.lease_id),
            job_network.as_deref(),
        )
    }

    fn rewrite_owned_resource_references(&self, request: &[u8]) -> Result<Vec<u8>> {
        let (method, target) = docker_request_line(request)?;
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let mut rewritten_target = None;
        if segments.len() >= 2 {
            let (kind, index) = match segments[0] {
                "containers" => (Some(DockerResourceKind::Container), 1),
                "networks" => (Some(DockerResourceKind::Network), 1),
                "volumes" => (Some(DockerResourceKind::Volume), 1),
                _ => (None, 0),
            };
            if let Some(kind) = kind
                && !is_docker_object_create_path(method, &path)
            {
                let resolved = if kind == DockerResourceKind::Container
                    && self.initial_buildkit_bootstrap_inspect(request)?.is_some()
                {
                    segments[index].to_owned()
                } else {
                    self.resolve_owned_id(kind, segments[index])?
                };
                if resolved != segments[index] {
                    rewritten_target = Some(replace_docker_route_id(
                        target, &path, &segments, index, &resolved,
                    )?);
                }
            }
        }

        let mut rewritten_body = None;
        if segments.len() == 3
            && segments[0] == "networks"
            && matches!(segments[2], "connect" | "disconnect")
        {
            if method.eq_ignore_ascii_case("POST") {
                let mut value = parse_create_value(docker_request_body(request)?)
                    .context("parse Docker network connect request")?;
                let object = value
                    .as_object_mut()
                    .context("Docker network request must be a JSON object")?;
                reject_case_insensitive_duplicate_keys(object, "Docker network request")?;
                if let Some(endpoint_config) = object
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("EndpointConfig"))
                    .map(|(_, value)| value)
                {
                    reject_endpoint_network_id(endpoint_config, "Docker EndpointConfig")?;
                }
                let container = object
                    .get("Container")
                    .and_then(Value::as_str)
                    .context("Docker network request must name a container")?;
                let resolved = self.resolve_owned_id(DockerResourceKind::Container, container)?;
                if resolved != container {
                    object.insert("Container".into(), Value::String(resolved));
                    rewritten_body = Some(
                        serde_json::to_vec(&value)
                            .context("serialize Docker network request with pinned container ID")?,
                    );
                }
            }
        } else if segments.as_slice() == ["containers", "create"]
            && method.eq_ignore_ascii_case("POST")
        {
            let network_aliases = self.network_name_ids()?;
            let volume_aliases = self.volume_name_ids()?;
            if !network_aliases.is_empty() || !volume_aliases.is_empty() {
                let mut value = parse_create_value(docker_request_body(request)?)
                    .context("parse Docker container create request for owned aliases")?;
                let networks_changed = rewrite_networking_config_ids(&mut value, &network_aliases)?;
                let network_mode_changed = rewrite_network_mode_id(&mut value, &network_aliases)?;
                let volumes_changed = rewrite_volume_mount_ids(&mut value, &volume_aliases)?;
                if networks_changed || network_mode_changed || volumes_changed {
                    rewritten_body = Some(
                        serde_json::to_vec(&value)
                            .context("serialize Docker create with pinned resource IDs")?,
                    );
                }
            }
        } else if segments.as_slice() == ["volumes", "create"]
            && method.eq_ignore_ascii_case("POST")
            && self
                .persistent_buildkit_volume_admission(request)?
                .is_none()
        {
            let mut value = parse_create_value(docker_request_body(request)?)
                .context("parse Docker volume create request for lease namespace")?;
            let object = value
                .as_object_mut()
                .context("Docker volume create body must be an object")?;
            reject_case_insensitive_duplicate_keys(object, "Docker volume create body")?;
            let name = object
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
                .and_then(|(_, value)| value.as_str())
                .context("Docker volume create request must name the volume")?;
            let private_name = self.private_volume_name(name)?;
            let name_key = object
                .keys()
                .find(|key| key.eq_ignore_ascii_case("Name"))
                .cloned()
                .context("Docker volume create request must name the volume")?;
            object.insert(name_key, Value::String(private_name));
            rewritten_body = Some(
                serde_json::to_vec(&value)
                    .context("serialize Docker volume create with lease-private name")?,
            );
        }

        if rewritten_target.is_none() && rewritten_body.is_none() {
            return Ok(request.to_vec());
        }
        rewrite_http_request(
            request,
            rewritten_target.as_deref().unwrap_or(target),
            rewritten_body.as_deref(),
        )
    }

    fn record_persistent_buildkit_create(
        &self,
        daemon_name: &str,
        admission: &PersistentBuildKitAdmission,
        job_container: &str,
        status: u16,
        body: &[u8],
    ) -> Result<Option<String>> {
        if !(200..300).contains(&status) {
            return Ok(None);
        }
        let value = parse_create_value(body)
            .context("parse successful persistent BuildKit create response")?;
        let id = value
            .get("Id")
            .or_else(|| value.get("ID"))
            .and_then(Value::as_str)
            .context("persistent BuildKit create response omitted its ID")?;
        let id = validate_owned_resource_id(id, "BuildKit daemon ID")?;
        if self.should_persist_buildkit_bootstrap() {
            crate::buildkit::record_admitted_builder_container_created(
                &admission.builder,
                &admission.owner_token,
                daemon_name,
                job_container,
                &id,
                &sha256_hex(&admission.approved_config),
            )?;
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(daemon_name)
            .context("persistent BuildKit daemon admission disappeared during create")?;
        if stored.builder != admission.builder || stored.owner_token != admission.owner_token {
            bail!("persistent BuildKit admission changed during create");
        }
        stored.container_id = Some(id);
        stored.network_reconciled = false;
        stored.bootstrap_phase = crate::buildkit::BuilderBootstrapPhase::Created;
        stored.readiness_attempts = 0;
        Ok(Some(stored.container_id.clone().context(
            "persistent BuildKit ID disappeared during create registration",
        )?))
    }

    fn transition_persistent_buildkit_phase(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
        expected: crate::buildkit::BuilderBootstrapPhase,
        next: crate::buildkit::BuilderBootstrapPhase,
    ) -> Result<()> {
        let container_id = validate_owned_resource_id(container_id, "BuildKit daemon ID")?;
        if self.should_persist_buildkit_bootstrap() {
            crate::buildkit::transition_admitted_builder_bootstrap(
                &admission.builder,
                &admission.owner_token,
                &container_id,
                expected,
                next,
                &sha256_hex(&admission.approved_config),
            )?;
        }
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(&daemon)
            .context("persistent BuildKit admission disappeared during bootstrap transition")?;
        if stored.owner_token != admission.owner_token
            || stored.container_id.as_deref() != Some(container_id.as_str())
            || stored.bootstrap_phase != expected
        {
            bail!("persistent BuildKit identity or phase changed during bootstrap transition");
        }
        stored.bootstrap_phase = next;
        Ok(())
    }

    fn reserve_persistent_buildkit_archive(
        &self,
        request: &[u8],
    ) -> Result<Option<(PersistentBuildKitAdmission, String)>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("PUT") {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["containers", id, "archive"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(admission) = self.persistent_buildkit_container_admission(id)? else {
            return Ok(None);
        };
        validate_persistent_buildkit_archive(request, &admission.approved_config)
            .map_err(create_capability_deny)?;
        let container_id = self.resolve_owned_id(DockerResourceKind::Container, id)?;
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(&daemon)
            .context("persistent BuildKit admission disappeared before archive")?;
        if stored.owner_token != admission.owner_token
            || stored.container_id.as_deref() != Some(container_id.as_str())
            || stored.bootstrap_phase != crate::buildkit::BuilderBootstrapPhase::Created
            || stored.archive_in_flight
        {
            return Err(LeaseDeny::forbidden(
                "persistent BuildKit archive is duplicate, out of sequence, or identity-mismatched",
            )
            .into());
        }
        stored.archive_in_flight = true;
        Ok(Some((stored.clone(), container_id)))
    }

    fn complete_persistent_buildkit_archive(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
        status: u16,
    ) -> Result<()> {
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(&daemon)
            .context("persistent BuildKit admission disappeared during archive response")?;
        if stored.owner_token != admission.owner_token
            || stored.container_id.as_deref() != Some(container_id)
            || !stored.archive_in_flight
        {
            bail!("persistent BuildKit archive identity or phase changed during response");
        }
        let expected_phase = if (200..300).contains(&status) {
            crate::buildkit::BuilderBootstrapPhase::Archived
        } else {
            crate::buildkit::BuilderBootstrapPhase::Created
        };
        if stored.bootstrap_phase != expected_phase {
            bail!("persistent BuildKit archive completed in an invalid phase");
        }
        stored.archive_in_flight = false;
        Ok(())
    }

    fn cancel_persistent_buildkit_archive(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
    ) -> Result<()> {
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if let Some(stored) = resources.admitted_buildkit_daemons.get_mut(&daemon)
            && stored.owner_token == admission.owner_token
            && stored.container_id.as_deref() == Some(container_id)
            && stored.bootstrap_phase == crate::buildkit::BuilderBootstrapPhase::Created
        {
            stored.archive_in_flight = false;
        }
        Ok(())
    }

    fn persistent_buildkit_start_context(
        &self,
        request: &[u8],
    ) -> Result<Option<(PersistentBuildKitAdmission, String)>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("POST") || target.contains('?') {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["containers", id, "start"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(admission) = self.persistent_buildkit_container_admission(id)? else {
            return Ok(None);
        };
        let container_id = self.resolve_owned_id(DockerResourceKind::Container, id)?;
        if admission.container_id.as_deref() != Some(container_id.as_str()) {
            bail!("persistent BuildKit start is not bound to its registered daemon ID");
        }
        Ok(Some((admission, container_id)))
    }

    fn complete_persistent_buildkit_start(
        &self,
        admission: &PersistentBuildKitAdmission,
        container_id: &str,
        status: u16,
    ) -> Result<()> {
        if !(200..300).contains(&status) && status != 304 {
            return Ok(());
        }
        let current = self
            .persistent_buildkit_container_admission(container_id)?
            .context("persistent BuildKit admission disappeared after Start")?;
        if current.container_id.as_deref() != Some(container_id)
            || current.owner_token != admission.owner_token
        {
            bail!("persistent BuildKit identity changed during Start");
        }
        if current.bootstrap_phase == crate::buildkit::BuilderBootstrapPhase::Archived {
            self.transition_persistent_buildkit_phase(
                &current,
                container_id,
                crate::buildkit::BuilderBootstrapPhase::Archived,
                crate::buildkit::BuilderBootstrapPhase::Started,
            )?;
        } else if !matches!(
            current.bootstrap_phase,
            crate::buildkit::BuilderBootstrapPhase::Started
                | crate::buildkit::BuilderBootstrapPhase::Ready
        ) {
            bail!("persistent BuildKit Start returned success in an invalid phase");
        }
        Ok(())
    }

    fn reserve_persistent_buildkit_exec_create(
        &self,
        request: &[u8],
    ) -> Result<Option<PersistentBuildKitExecCreate>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("POST") || target.contains('?') {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["containers", parent, "exec"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(admission) = self.persistent_buildkit_container_admission(parent)? else {
            return Ok(None);
        };
        let (parsed_parent, command) = persistent_buildkit_exec_command(request)?
            .context("persistent BuildKit ExecCreate parser lost its route")?;
        if parsed_parent != *parent {
            bail!("persistent BuildKit ExecCreate parent changed during validation");
        }
        let container_id = self.resolve_owned_id(DockerResourceKind::Container, parent)?;
        if admission.container_id.as_deref() != Some(container_id.as_str()) {
            bail!("persistent BuildKit ExecCreate is not bound to its registered daemon ID");
        }
        self.verify_persistent_buildkit_running(&admission, &container_id)?;
        let daemon = crate::buildkit::daemon_container_name(&admission.builder);
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .admitted_buildkit_daemons
            .get_mut(&daemon)
            .context("persistent BuildKit admission disappeared before ExecCreate")?;
        if stored.owner_token != admission.owner_token
            || stored.container_id.as_deref() != Some(container_id.as_str())
            || !persistent_exec_create_phase_allowed(command, stored.bootstrap_phase)
        {
            return Err(
                LeaseDeny::forbidden("persistent BuildKit ExecCreate is out of sequence").into(),
            );
        }
        if command == PersistentBuildKitExecCommand::Workers {
            if stored.readiness_attempts >= MAX_BUILDKIT_READINESS_ATTEMPTS {
                return Err(LeaseDeny::forbidden(
                    "persistent BuildKit readiness probe limit was reached",
                )
                .into());
            }
            stored.readiness_attempts += 1;
        }
        Ok(Some(PersistentBuildKitExecCreate {
            parent_container_id: container_id,
            command,
        }))
    }

    fn record_persistent_buildkit_exec_create(
        &self,
        create: &PersistentBuildKitExecCreate,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        if !(200..300).contains(&status) {
            return Ok(());
        }
        let value = parse_create_value(body)
            .context("parse successful persistent BuildKit ExecCreate response")?;
        let id = value
            .get("Id")
            .or_else(|| value.get("ID"))
            .and_then(Value::as_str)
            .context("persistent BuildKit ExecCreate response omitted its ID")?;
        let id = validate_owned_resource_id(id, "persistent BuildKit exec ID")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        if resources.execs.contains(&id) || resources.persistent_buildkit_execs.contains_key(&id) {
            bail!("persistent BuildKit ExecCreate reused an owned exec ID");
        }
        if !resources
            .admitted_buildkit_daemons
            .values()
            .any(|admission| {
                admission.container_id.as_deref() == Some(create.parent_container_id.as_str())
                    && persistent_exec_create_phase_allowed(
                        create.command,
                        admission.bootstrap_phase,
                    )
            })
        {
            bail!("persistent BuildKit ExecCreate lost its daemon admission before response");
        }
        resources.persistent_buildkit_execs.insert(
            id,
            PersistentBuildKitExec {
                container_id: create.parent_container_id.clone(),
                command: create.command,
                phase: PersistentBuildKitExecPhase::Created,
            },
        );
        Ok(())
    }

    fn reserve_persistent_buildkit_exec_start(
        &self,
        request: &[u8],
    ) -> Result<Option<(String, PersistentBuildKitExec)>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("POST") {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["exec", id, "start"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(exec) = self.persistent_buildkit_exec(id)? else {
            return Ok(None);
        };
        let admission = self
            .persistent_buildkit_container_admission(&exec.container_id)?
            .context("persistent BuildKit daemon admission disappeared before ExecAttach")?;
        self.verify_persistent_buildkit_running(&admission, &exec.container_id)?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .persistent_buildkit_execs
            .get_mut(*id)
            .context("persistent BuildKit exec ID disappeared before ExecAttach")?;
        if stored != &exec || stored.phase != PersistentBuildKitExecPhase::Created {
            return Err(LeaseDeny::forbidden(
                "persistent BuildKit ExecAttach is out of sequence or replayed",
            )
            .into());
        }
        stored.phase = PersistentBuildKitExecPhase::Starting;
        Ok(Some(((*id).to_owned(), stored.clone())))
    }

    fn complete_persistent_buildkit_exec_start(&self, exec_id: &str, status: u16) -> Result<()> {
        if status != 101 {
            self.retire_deleted_alias(DockerResourceKind::Exec, exec_id)?;
            return Ok(());
        }
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .persistent_buildkit_execs
            .get_mut(exec_id)
            .context("persistent BuildKit exec ID disappeared after ExecAttach")?;
        if stored.phase != PersistentBuildKitExecPhase::Starting {
            bail!("persistent BuildKit ExecAttach response arrived out of sequence");
        }
        stored.phase = PersistentBuildKitExecPhase::Streamed;
        Ok(())
    }

    fn reserve_persistent_buildkit_exec_inspect(
        &self,
        request: &[u8],
    ) -> Result<Option<(String, PersistentBuildKitExec)>> {
        let (method, target) = docker_request_line(request)?;
        if !method.eq_ignore_ascii_case("GET") {
            return Ok(None);
        }
        let path = canonical_docker_path(target)?;
        let segments = docker_api_path_segments(&path)?;
        let ["exec", id, "json"] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(exec) = self.persistent_buildkit_exec(id)? else {
            return Ok(None);
        };
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        let stored = resources
            .persistent_buildkit_execs
            .get_mut(*id)
            .context("persistent BuildKit exec ID disappeared before ExecInspect")?;
        if stored != &exec
            || stored.phase != PersistentBuildKitExecPhase::Streamed
            || !matches!(
                stored.command,
                PersistentBuildKitExecCommand::Workers | PersistentBuildKitExecCommand::Version
            )
        {
            return Err(LeaseDeny::forbidden(
                "persistent BuildKit ExecInspect is out of sequence or replayed",
            )
            .into());
        }
        stored.phase = PersistentBuildKitExecPhase::Inspecting;
        Ok(Some(((*id).to_owned(), stored.clone())))
    }

    fn complete_persistent_buildkit_exec_inspect(
        &self,
        exec_id: &str,
        exec: &PersistentBuildKitExec,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        if status != 200 {
            self.retire_deleted_alias(DockerResourceKind::Exec, exec_id)?;
            return Ok(());
        }
        let completion = (|| -> Result<()> {
            let value = parse_create_value(body)
                .context("parse persistent BuildKit ExecInspect response")?;
            let object = value
                .as_object()
                .context("persistent BuildKit ExecInspect response must be an object")?;
            reject_case_insensitive_duplicate_keys(
                object,
                "persistent BuildKit ExecInspect response",
            )?;
            let field = |name: &str| {
                object
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value)
            };
            if field("ID").and_then(Value::as_str) != Some(exec_id)
                || field("ContainerID").and_then(Value::as_str) != Some(&exec.container_id)
                || field("Running").and_then(Value::as_bool) != Some(false)
            {
                bail!("persistent BuildKit ExecInspect identity or completion state did not match");
            }
            let exit_code = field("ExitCode")
                .and_then(Value::as_i64)
                .context("persistent BuildKit ExecInspect omitted its integer ExitCode")?;
            if exec.command == PersistentBuildKitExecCommand::Workers && exit_code == 0 {
                let admission = self
                    .persistent_buildkit_container_admission(&exec.container_id)?
                    .context(
                        "persistent BuildKit daemon admission disappeared after readiness probe",
                    )?;
                match admission.bootstrap_phase {
                    crate::buildkit::BuilderBootstrapPhase::Started => {
                        self.transition_persistent_buildkit_phase(
                            &admission,
                            &exec.container_id,
                            crate::buildkit::BuilderBootstrapPhase::Started,
                            crate::buildkit::BuilderBootstrapPhase::Ready,
                        )?;
                    }
                    crate::buildkit::BuilderBootstrapPhase::Ready => {}
                    _ => bail!("persistent BuildKit readiness result arrived in an invalid phase"),
                }
            }
            Ok(())
        })();
        // ExecInspect is terminal in Buildx's protocol, even when the Engine
        // response is malformed. Do not retain a dead exec alias and consume
        // the lease's bounded resource quota after a failed readiness attempt.
        self.retire_deleted_alias(DockerResourceKind::Exec, exec_id)?;
        completion
    }

    fn record_create_response(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
    ) -> Result<()> {
        self.record_create_response_with_alias(kind, status, body, None)
    }

    fn record_create_response_with_alias(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
        alias: Option<&str>,
    ) -> Result<()> {
        self.record_create_response_with_owner(kind, status, body, alias, None, None)
    }

    fn record_create_response_with_owner(
        &self,
        kind: DockerResourceKind,
        status: u16,
        body: &[u8],
        alias: Option<&str>,
        job_id: Option<&str>,
        daemon_id: Option<&str>,
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
        let volume_alias = if kind == DockerResourceKind::Volume {
            let alias = validate_owned_resource_id(
                alias.context("guest volume create has no requested Name alias")?,
                "created Docker volume name",
            )?;
            let expected_name = self.private_volume_name(&alias)?;
            if identifier != expected_name {
                bail!("Docker volume create returned a name outside this lease namespace");
            }
            let job_id = job_id.context("volume create is missing its expected job label")?;
            let daemon_id =
                daemon_id.context("volume create is missing its expected daemon label")?;
            let labels = value
                .get("Labels")
                .and_then(Value::as_object)
                .context("Docker volume create response omitted its Labels object")?;
            if labels.get(JOB_ID_LABEL).and_then(Value::as_str) != Some(job_id)
                || labels.get(DAEMON_ID_LABEL).and_then(Value::as_str) != Some(daemon_id)
                || labels.get(LEASE_ID_LABEL).and_then(Value::as_str)
                    != Some(self.lease_id.as_str())
            {
                bail!("Docker volume create response does not carry this lease's owner labels");
            }
            Some(alias)
        } else {
            None
        };
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
                resources.containers.insert(identifier.clone());
                if let Some(alias) = alias {
                    let alias = validate_owned_resource_id(alias, "created Docker container name")?;
                    resources.container_names.insert(alias, identifier);
                }
            }
            DockerResourceKind::Network => {
                resources.networks.insert(identifier.clone());
                if let Some(alias) = alias {
                    let alias = validate_owned_resource_id(alias, "created Docker network name")?;
                    resources.network_names.insert(alias, identifier);
                }
            }
            DockerResourceKind::Volume => {
                let alias = volume_alias.context("volume create has no alias")?;
                resources.volumes.insert(identifier.clone());
                resources.volume_names.insert(alias, identifier);
            }
            DockerResourceKind::Exec => {
                resources.execs.insert(identifier);
            }
        }
        Ok(())
    }

    fn retire_deleted_alias(&self, kind: DockerResourceKind, id: &str) -> Result<()> {
        let id = validate_owned_resource_id(id, "deleted Docker resource")?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| anyhow::anyhow!("Docker lease ownership registry is poisoned"))?;
        match kind {
            DockerResourceKind::Container => {
                resources.containers.remove(&id);
                resources
                    .container_names
                    .retain(|_, owned_id| owned_id != &id);
            }
            DockerResourceKind::Network => {
                resources.networks.remove(&id);
                resources
                    .network_names
                    .retain(|_, owned_id| owned_id != &id);
            }
            DockerResourceKind::Volume => {
                if let Some(volume) = resources.volume_names.remove(&id) {
                    resources.volumes.remove(&volume);
                } else {
                    resources.volumes.remove(&id);
                }
                resources.volume_names.retain(|_, volume| volume != &id);
                resources.admitted_buildkit_volumes.remove(&id);
            }
            DockerResourceKind::Exec => {
                resources.execs.remove(&id);
                resources.persistent_buildkit_execs.remove(&id);
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
        return Err(LeaseDeny::forbidden(
            "Docker lease denied unowned upgrade/tunnel route",
        ));
    }
    Ok(route)
}

/// Docker's `POST /containers/create?name=<name>` query. Names are plain
/// Docker name characters; anything percent-escaped beyond the basics is
/// rejected by falling back to `None` (the create still works, only the
/// by-name alias is not learned).
fn containers_create_query_name(request: &[u8]) -> Result<Option<String>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST") {
        return Ok(None);
    }
    let path = canonical_docker_path(target)?;
    if docker_api_path_segments(&path)?.as_slice() != ["containers", "create"] {
        return Ok(None);
    }
    let Some((_, query)) = target.split_once('?') else {
        return Ok(None);
    };
    // Buildx's reserved daemon names need byte-exact authorization. Reject
    // escaped or form-encoded query text rather than letting Docker decode a
    // spelling the lease did not compare. Docker-generated ordinary names
    // use these characters literally.
    if query.bytes().any(|byte| byte == b'%' || byte == b'+') {
        bail!("Docker container create query must not percent-encode or form-encode names");
    }
    let mut name = None;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("name=") {
            if name.is_some() {
                bail!("Docker container create query contains duplicate name parameters");
            }
            name = Some(value.to_owned());
        }
    }
    let Some(name) = name else {
        return Ok(None);
    };
    if name.is_empty() || name.len() > MAX_OWNED_DOCKER_RESOURCE_ID {
        bail!("invalid Docker container create name");
    }
    if name
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'/')
    {
        bail!("invalid Docker container create name");
    }
    Ok(Some(validate_owned_resource_id(&name, "Docker resource")?))
}

fn networks_create_request_name(request: &[u8]) -> Result<Option<String>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST")
        || !canonical_docker_path(target)?.ends_with("/networks/create")
    {
        return Ok(None);
    }
    let mut value = parse_create_value(docker_request_body(request)?)
        .context("parse Docker network create request name")?;
    let object = value
        .as_object_mut()
        .context("Docker network create body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "Docker network create body")?;
    let name = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
        .and_then(|(_, value)| value.as_str())
        .context("Docker network create request must name the network")?;
    Ok(Some(validate_owned_resource_id(
        name,
        "Docker network name",
    )?))
}

fn volumes_create_request_name(request: &[u8]) -> Result<Option<String>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST")
        || !canonical_docker_path(target)?.ends_with("/volumes/create")
    {
        return Ok(None);
    }
    let mut value = parse_create_value(docker_request_body(request)?)
        .context("parse Docker volume create request name")?;
    let object = value
        .as_object_mut()
        .context("Docker volume create body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "Docker volume create body")?;
    let name = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
        .and_then(|(_, value)| value.as_str())
        .context("Docker volume create request must name the volume")?;
    Ok(Some(validate_owned_resource_id(
        name,
        "Docker volume name",
    )?))
}

fn is_reserved_persistent_buildkit_daemon(name: &str) -> bool {
    name.strip_prefix("buildx_buildkit_")
        .is_some_and(|builder| builder.starts_with(crate::buildkit::PERSISTENT_BUILDER_PREFIX))
}

fn is_reserved_persistent_buildkit_volume(name: &str) -> bool {
    name.strip_suffix("_state")
        .is_some_and(is_reserved_persistent_buildkit_daemon)
}

fn owned_resource_count(resources: &OwnedDockerResources) -> usize {
    resources.containers.len()
        + resources.networks.len()
        + resources.volumes.len()
        + resources.execs.len()
        + resources.persistent_buildkit_execs.len()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

#[cfg(unix)]
fn capture_response_wire_bytes(wire: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_CREATE_RESPONSE_WIRE_BODY.saturating_sub(wire.len()) {
        bail!("Docker create response framing exceeds ownership capture limit");
    }
    wire.extend_from_slice(bytes);
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

fn persistent_buildkit_exec_command(
    request: &[u8],
) -> Result<Option<(String, PersistentBuildKitExecCommand)>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST") {
        return Ok(None);
    }
    if target.contains('?') {
        bail!("persistent BuildKit ExecCreate does not allow query parameters");
    }
    let path = canonical_docker_path(target)?;
    let segments = docker_api_path_segments(&path)?;
    let ["containers", container, "exec"] = segments.as_slice() else {
        return Ok(None);
    };
    let body = docker_request_body(request)?;
    let value = parse_create_value(body).context("parse persistent BuildKit ExecCreate")?;
    let object = value
        .as_object()
        .context("persistent BuildKit ExecCreate body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "persistent BuildKit ExecCreate")?;
    const FIELDS: [&str; 10] = [
        "attachstdin",
        "attachstdout",
        "attachstderr",
        "cmd",
        "detachkeys",
        "env",
        "privileged",
        "tty",
        "user",
        "workingdir",
    ];
    if object.len() != FIELDS.len()
        || object
            .keys()
            .any(|key| !FIELDS.contains(&key.to_ascii_lowercase().as_str()))
    {
        bail!("persistent BuildKit ExecCreate has unsupported fields");
    }
    let field = |name: &str| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    if field("User").and_then(Value::as_str) != Some("")
        || field("Privileged").and_then(Value::as_bool) != Some(false)
        || field("Tty").and_then(Value::as_bool) != Some(false)
        || field("AttachStdin").and_then(Value::as_bool) != Some(true)
        || field("AttachStdout").and_then(Value::as_bool) != Some(true)
        || field("AttachStderr").and_then(Value::as_bool) != Some(true)
        || field("DetachKeys").and_then(Value::as_str) != Some("")
        || !field("Env").is_some_and(Value::is_null)
        || field("WorkingDir").and_then(Value::as_str) != Some("")
    {
        bail!("persistent BuildKit ExecCreate must use default non-TTY stdio settings");
    }
    let command = field("Cmd")
        .and_then(Value::as_array)
        .context("persistent BuildKit ExecCreate Cmd must be a string array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .context("persistent BuildKit ExecCreate Cmd entries must be strings")
        })
        .collect::<Result<Vec<_>>>()?;
    let command = match command.as_slice() {
        ["buildctl", "debug", "workers"] => PersistentBuildKitExecCommand::Workers,
        ["buildkitd", "--version"] => PersistentBuildKitExecCommand::Version,
        ["buildctl", "dial-stdio"] => PersistentBuildKitExecCommand::DialStdio,
        _ => bail!("persistent BuildKit ExecCreate command is not approved"),
    };
    Ok(Some(((*container).to_owned(), command)))
}

fn validate_persistent_buildkit_exec_start(request: &[u8]) -> Result<()> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST") || target.contains('?') {
        bail!(
            "persistent BuildKit ExecAttach must use its exact POST route without query parameters"
        );
    }
    let path = canonical_docker_path(target)?;
    let segments = docker_api_path_segments(&path)?;
    if !matches!(segments.as_slice(), ["exec", _, "start"]) {
        bail!("persistent BuildKit ExecAttach target is not an ExecStart route");
    }
    validate_exact_content_length(request)?;
    let body = docker_request_body(request)?;
    let value = parse_create_value(body).context("parse persistent BuildKit ExecStart")?;
    let object = value
        .as_object()
        .context("persistent BuildKit ExecStart body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "persistent BuildKit ExecStart")?;
    if object.len() != 2
        || object
            .keys()
            .any(|key| !["detach", "tty"].contains(&key.to_ascii_lowercase().as_str()))
        || object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Detach"))
            .and_then(|(_, value)| value.as_bool())
            != Some(false)
        || object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Tty"))
            .and_then(|(_, value)| value.as_bool())
            != Some(false)
    {
        bail!("persistent BuildKit ExecStart must use the exact non-TTY attached shape");
    }
    let headers = docker_request_headers(request)?;
    let upgrades = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("upgrade"))
        .map(|(_, value)| value.trim())
        .collect::<Vec<_>>();
    if upgrades.as_slice() != ["tcp"] {
        bail!("persistent BuildKit ExecAttach must use the Docker TCP upgrade protocol");
    }
    let connection_upgrade = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, value)| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    if !connection_upgrade {
        bail!("persistent BuildKit ExecAttach omitted Connection: Upgrade");
    }
    Ok(())
}

fn validate_persistent_buildkit_container_start(request: &[u8]) -> Result<()> {
    validate_bodyless_docker_request(request, "POST", "containers", "start")
}

fn validate_persistent_buildkit_exec_inspect(request: &[u8]) -> Result<()> {
    validate_bodyless_docker_request(request, "GET", "exec", "json")
}

fn validate_bodyless_docker_request(
    request: &[u8],
    expected_method: &str,
    expected_kind: &str,
    expected_operation: &str,
) -> Result<()> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case(expected_method) || target.contains('?') {
        bail!("persistent BuildKit request must use its exact route without query parameters");
    }
    let path = canonical_docker_path(target)?;
    let segments = docker_api_path_segments(&path)?;
    if segments.len() != 3 || segments[0] != expected_kind || segments[2] != expected_operation {
        bail!("persistent BuildKit request target does not match its exact operation");
    }
    if !docker_request_body(request)?.is_empty() {
        bail!("persistent BuildKit start/inspect request must not have a body");
    }
    let headers = docker_request_headers(request)?;
    let lengths = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if lengths.len() > 1 || lengths.first().is_some_and(|length| *length != 0) {
        bail!("persistent BuildKit start/inspect request must have an empty body");
    }
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        bail!("persistent BuildKit start/inspect request does not allow Transfer-Encoding");
    }
    Ok(())
}

fn validate_exact_content_length(request: &[u8]) -> Result<()> {
    let headers = docker_request_headers(request)?;
    let lengths = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if lengths.len() != 1 || lengths[0] != docker_request_body(request)?.len() {
        bail!("persistent BuildKit request requires one exact Content-Length");
    }
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        bail!("persistent BuildKit request does not allow Transfer-Encoding");
    }
    Ok(())
}

fn validate_persistent_buildkit_archive(request: &[u8], expected_config: &[u8]) -> Result<()> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("PUT") {
        bail!("persistent BuildKit archive must use PUT");
    }
    let (_, query) = target
        .split_once('?')
        .context("persistent BuildKit archive omitted its query")?;
    let parameters = parse_buildkit_archive_query(query)?;
    if parameters.get("path").map(String::as_str) != Some("/etc")
        || parameters.get("noOverwriteDirNonDir").map(String::as_str) != Some("true")
        || parameters.len() != 2
    {
        bail!("persistent BuildKit archive must target /etc with noOverwriteDirNonDir=true");
    }
    let headers = docker_request_headers(request)?;
    let content_length = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if content_length.len() != 1 || content_length[0] != docker_request_body(request)?.len() {
        bail!("persistent BuildKit archive requires one exact Content-Length");
    }
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        bail!("persistent BuildKit archive does not allow Transfer-Encoding");
    }
    let content_types = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if content_types.len() > 1
        || content_types
            .first()
            .is_some_and(|value| value != "application/x-tar")
    {
        bail!("persistent BuildKit archive Content-Type must be absent or application/x-tar");
    }
    let body = docker_request_body(request)?;
    if body.len() > MAX_BUILDKIT_ARCHIVE_BODY {
        bail!("persistent BuildKit archive exceeds its size limit");
    }
    validate_buildkit_ustar(body, expected_config)
}

fn parse_buildkit_archive_query(query: &str) -> Result<BTreeMap<String, String>> {
    let mut parameters = BTreeMap::new();
    for pair in query.split('&') {
        if pair.contains('+') {
            bail!("persistent BuildKit archive query must not use form encoding");
        }
        let (raw_name, raw_value) = pair
            .split_once('=')
            .context("persistent BuildKit archive query has a malformed parameter")?;
        let decoded = url::form_urlencoded::parse(pair.as_bytes()).collect::<Vec<_>>();
        if decoded.len() != 1 {
            bail!("persistent BuildKit archive query has ambiguous encoding");
        }
        let (name, value) = &decoded[0];
        if parameters
            .insert(name.to_string(), value.to_string())
            .is_some()
        {
            bail!("persistent BuildKit archive query contains duplicate parameters");
        }
        if raw_name != name {
            bail!("persistent BuildKit archive query names must use canonical encoding");
        }
        if name == "path" && raw_value != "%2Fetc" {
            bail!("persistent BuildKit archive path must use the canonical escaped /etc value");
        }
        if name == "noOverwriteDirNonDir"
            && (raw_name != "noOverwriteDirNonDir" || raw_value != "true")
        {
            bail!("persistent BuildKit archive noOverwriteDirNonDir encoding is not canonical");
        }
        if !["path", "noOverwriteDirNonDir"].contains(&name.as_ref()) {
            bail!("persistent BuildKit archive query parameter is not permitted");
        }
    }
    Ok(parameters)
}

fn docker_request_headers(request: &[u8]) -> Result<Vec<(String, String)>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("Docker API request is missing header terminator")?;
    let header = std::str::from_utf8(&request[..header_end])
        .context("Docker API request headers must be UTF-8")?;
    let mut headers = Vec::new();
    for line in header.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .context("Docker API request has a malformed header")?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(headers)
}

fn validate_buildkit_ustar(bytes: &[u8], expected_config: &[u8]) -> Result<()> {
    if bytes.len() < 1024 || bytes.len() % 512 != 0 {
        bail!("persistent BuildKit archive is not a complete ustar stream");
    }
    let mut offset = 0;
    let mut directories = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut saw_terminator = false;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            saw_terminator = true;
            if bytes[offset..].iter().any(|byte| *byte != 0) {
                bail!("persistent BuildKit archive has nonzero bytes after its terminator");
            }
            break;
        }
        validate_ustar_checksum(header)?;
        if &header[257..263] != b"ustar\0" || &header[263..265] != b"00" {
            bail!("persistent BuildKit archive must use plain ustar entries");
        }
        let name = ustar_string(&header[..100])?;
        let prefix = ustar_string(&header[345..500])?;
        let raw_path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let typeflag = header[156];
        let path = if typeflag == b'5' {
            if raw_path.ends_with("//") {
                bail!("persistent BuildKit archive has an invalid directory path");
            }
            raw_path.strip_suffix('/').unwrap_or(&raw_path)
        } else {
            raw_path.as_str()
        };
        if path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            bail!("persistent BuildKit archive contains an unsafe path");
        }
        let mode = parse_ustar_octal(&header[100..108])?;
        let uid = parse_ustar_octal(&header[108..116])?;
        let gid = parse_ustar_octal(&header[116..124])?;
        let size = parse_ustar_octal(&header[124..136])?;
        if uid != 0 || gid != 0 {
            bail!("persistent BuildKit archive entries must be owned by root");
        }
        match typeflag {
            b'5' => {
                if !matches!(path, "buildkit" | "buildkit/provenance.d")
                    || mode != 0o755
                    || size != 0
                    || !directories.insert(path.to_owned())
                {
                    bail!("persistent BuildKit archive has an unapproved directory entry");
                }
                offset += 512;
            }
            0 | b'0' => {
                if mode != 0o644 || size > MAX_BUILDKIT_ARCHIVE_FILE {
                    bail!("persistent BuildKit archive file mode or size is not approved");
                }
                let end = offset
                    .checked_add(512)
                    .and_then(|value| value.checked_add(size))
                    .context("persistent BuildKit archive size overflow")?;
                if end > bytes.len() {
                    bail!("persistent BuildKit archive file is truncated");
                }
                let content_start = offset + 512;
                let content_end = content_start + size;
                let content = bytes[content_start..content_end].to_vec();
                if files.insert(path.to_owned(), content).is_some() {
                    bail!("persistent BuildKit archive contains a duplicate file");
                }
                let padded = size.div_ceil(512) * 512;
                let next = content_start
                    .checked_add(padded)
                    .context("persistent BuildKit archive offset overflow")?;
                if next > bytes.len() || bytes[content_end..next].iter().any(|byte| *byte != 0) {
                    bail!("persistent BuildKit archive has invalid file padding");
                }
                offset = next;
            }
            _ => bail!("persistent BuildKit archive contains a non-regular entry"),
        }
    }
    if !saw_terminator {
        bail!("persistent BuildKit archive omitted its terminator blocks");
    }
    if directories.iter().any(|directory| directory != "buildkit") {
        bail!("persistent BuildKit archive contains an unexpected parent directory");
    }
    if files.len() != 1
        || files.get("buildkit/buildkitd.toml").map(Vec::as_slice) != Some(expected_config)
    {
        bail!("persistent BuildKit archive must contain only its runner-approved TOML file");
    }
    Ok(())
}

fn ustar_string(field: &[u8]) -> Result<String> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    if field[end..].iter().any(|byte| *byte != 0 && *byte != b' ') {
        bail!("persistent BuildKit ustar field has non-padding bytes after NUL");
    }
    std::str::from_utf8(&field[..end])
        .map(str::to_owned)
        .context("persistent BuildKit ustar path is not UTF-8")
}

fn parse_ustar_octal(field: &[u8]) -> Result<usize> {
    if field
        .iter()
        .any(|byte| *byte != 0 && *byte != b' ' && !byte.is_ascii_digit())
    {
        bail!("persistent BuildKit ustar numeric field is not octal");
    }
    let text =
        std::str::from_utf8(field).context("persistent BuildKit ustar number is not UTF-8")?;
    let text = text.trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(text, 8).context("parse persistent BuildKit ustar octal value")
}

fn validate_ustar_checksum(header: &[u8]) -> Result<()> {
    if header.len() != 512 {
        bail!("persistent BuildKit ustar header has the wrong size");
    }
    let expected = parse_ustar_octal(&header[148..156])?;
    let actual = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                usize::from(b' ')
            } else {
                usize::from(*byte)
            }
        })
        .sum::<usize>();
    if expected != actual {
        bail!("persistent BuildKit ustar checksum does not match");
    }
    Ok(())
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

fn docker_delete_resource_reference(
    request: &[u8],
) -> Result<Option<(DockerResourceKind, String)>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("DELETE") {
        return Ok(None);
    }
    let path = canonical_docker_path(target)?;
    let segments = docker_api_path_segments(&path)?;
    if segments.len() != 2 {
        return Ok(None);
    }
    let kind = match segments[0] {
        "containers" => DockerResourceKind::Container,
        "networks" => DockerResourceKind::Network,
        "volumes" => DockerResourceKind::Volume,
        _ => return Ok(None),
    };
    Ok(Some((kind, segments[1].to_owned())))
}

/// Resource path used by an authorized non-create request. The immutable ID
/// is retained through the response observer so a stale 404/410 (including
/// AutoRemove containers and exec starts) retires only that exact ID and its
/// aliases. Create endpoints are excluded because their second segment is the
/// literal `create`, not an owned resource.
fn docker_resource_reference(request: &[u8]) -> Result<Option<(DockerResourceKind, String)>> {
    let (method, target) = docker_request_line(request)?;
    let path = canonical_docker_path(target)?;
    if is_docker_object_create_path(method, &path) {
        return Ok(None);
    }
    let segments = docker_api_path_segments(&path)?;
    let reference = match segments.as_slice() {
        ["containers", id, ..] => Some((DockerResourceKind::Container, (*id).to_owned())),
        ["networks", id, ..] => Some((DockerResourceKind::Network, (*id).to_owned())),
        ["volumes", id, ..] => Some((DockerResourceKind::Volume, (*id).to_owned())),
        ["exec", id, ..] => Some((DockerResourceKind::Exec, (*id).to_owned())),
        _ => None,
    };
    Ok(reference)
}

fn docker_volume_read_alias(request: &[u8]) -> Result<Option<String>> {
    let (method, target) = docker_request_line(request)?;
    if !matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD") {
        return Ok(None);
    }
    let path = canonical_docker_path(target)?;
    let segments = docker_api_path_segments(&path)?;
    if segments.len() == 2 && segments[0] == "volumes" {
        return Ok(Some(validate_owned_resource_id(
            segments[1],
            "Docker volume name",
        )?));
    }
    Ok(None)
}

fn rewrite_volume_name_response(body: &[u8], expected: &str, alias: &str) -> Result<Vec<u8>> {
    let mut value =
        parse_create_value(body).context("parse Docker volume response before alias rewrite")?;
    let object = value
        .as_object_mut()
        .context("Docker volume response must be a JSON object")?;
    reject_case_insensitive_duplicate_keys(object, "Docker volume response")?;
    let name_key = object
        .keys()
        .find(|key| key.eq_ignore_ascii_case("Name"))
        .cloned()
        .context("Docker volume response omitted Name")?;
    if object.get(&name_key).and_then(Value::as_str) != Some(expected) {
        bail!("Docker volume response identity changed during alias rewrite");
    }
    object.insert(name_key, Value::String(alias.to_owned()));
    serde_json::to_vec(&value).context("serialize Docker volume response with guest alias")
}

fn docker_volume_mount_sources(request: &[u8]) -> Result<BTreeSet<String>> {
    let (method, target) = docker_request_line(request)?;
    if !method.eq_ignore_ascii_case("POST")
        || !canonical_docker_path(target)?.ends_with("/containers/create")
    {
        return Ok(BTreeSet::new());
    }
    let value = parse_create_value(docker_request_body(request)?)
        .context("parse Docker container create volume mounts")?;
    let Some(object) = value.as_object() else {
        bail!("Docker container create body must be an object");
    };
    let Some(host_config) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(BTreeSet::new());
    };
    let Some(host_config) = host_config.as_object() else {
        return Ok(BTreeSet::new());
    };
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    let Some(mounts) = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Mounts"))
        .map(|(_, value)| value)
    else {
        return Ok(BTreeSet::new());
    };
    let Some(mounts) = mounts.as_array() else {
        return Ok(BTreeSet::new());
    };
    let mut sources = BTreeSet::new();
    for mount in mounts {
        let Some(mount) = mount.as_object() else {
            continue;
        };
        reject_case_insensitive_duplicate_keys(mount, "Docker container mount")?;
        if mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
            .and_then(|(_, value)| value.as_str())
            != Some("volume")
        {
            continue;
        }
        if let Some(source) = mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Source"))
            .and_then(|(_, value)| value.as_str())
        {
            sources.insert(validate_owned_resource_id(source, "Docker volume name")?);
        }
    }
    Ok(sources)
}

fn docker_api_version_prefix(request: &[u8]) -> Result<String> {
    let (_, target) = docker_request_line(request)?;
    let path = canonical_docker_path(target)?;
    let first = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    if first.len() > 1 && first.starts_with('v') && first.as_bytes()[1].is_ascii_digit() {
        return Ok(first.to_owned());
    }
    Ok(String::new())
}

#[cfg(unix)]
fn require_live_job_volume(
    host_socket: &Path,
    api_version: &str,
    name: &str,
    job_id: &str,
    daemon_id: &str,
    lease_id: &str,
) -> Result<()> {
    let Some(volume) = inspect_live_volume(host_socket, api_version, name)? else {
        return Err(
            LeaseDeny::not_found(format!("Docker lease volume {name:?} no longer exists")).into(),
        );
    };
    let actual_name = volume.get("Name").and_then(Value::as_str);
    let labels = volume.get("Labels").and_then(Value::as_object);
    if actual_name != Some(name)
        || labels
            .and_then(|labels| labels.get(JOB_ID_LABEL))
            .and_then(Value::as_str)
            != Some(job_id)
        || labels
            .and_then(|labels| labels.get(DAEMON_ID_LABEL))
            .and_then(Value::as_str)
            != Some(daemon_id)
        || labels
            .and_then(|labels| labels.get(LEASE_ID_LABEL))
            .and_then(Value::as_str)
            != Some(lease_id)
    {
        return Err(LeaseDeny::forbidden(format!(
            "Docker lease volume {name:?} failed live owner-label verification"
        ))
        .into());
    }
    Ok(())
}

#[cfg(unix)]
fn inspect_live_volume(host_socket: &Path, api_version: &str, name: &str) -> Result<Option<Value>> {
    use std::os::unix::net::UnixStream;

    let name = validate_owned_resource_id(name, "Docker volume name")?;
    let mut stream = UnixStream::connect(host_socket)
        .with_context(|| format!("connect Docker lease to inspect volume {name:?}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .context("configure Docker volume identity inspect timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .context("configure Docker volume identity inspect timeout")?;
    let route = if api_version.is_empty() {
        format!("/volumes/{name}")
    } else {
        format!("/{api_version}/volumes/{name}")
    };
    write!(
        stream,
        "GET {route} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n"
    )
    .context("request live Docker volume identity")?;
    let mut response = Vec::new();
    let mut scratch = [0_u8; 8192];
    loop {
        let read = stream
            .read(&mut scratch)
            .context("read live Docker volume identity response")?;
        if read == 0 {
            break;
        }
        if response.len().saturating_add(read) > 128 * 1024 {
            bail!("Docker volume identity response exceeds its bounded size");
        }
        response.extend_from_slice(&scratch[..read]);
    }
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker volume identity response omitted header terminator")?;
    let header = std::str::from_utf8(&response[..header_end])
        .context("Docker volume identity response headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let status_line = lines.next().context("Docker volume identity status line")?;
    let mut status_parts = status_line.split_ascii_whitespace();
    let _version = status_parts
        .next()
        .context("Docker volume response version")?;
    let status = status_parts
        .next()
        .context("Docker volume response status")?
        .parse::<u16>()
        .context("parse Docker volume response status")?;
    if matches!(status, 404 | 410) {
        return Ok(None);
    }
    if status != 200 {
        bail!("Docker volume identity inspect returned HTTP {status}");
    }
    let mut content_length = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            bail!("malformed Docker volume identity response header");
        };
        if field.eq_ignore_ascii_case("transfer-encoding") {
            bail!("chunked Docker volume identity response is not supported");
        }
        if field.eq_ignore_ascii_case("content-length") {
            if content_length
                .replace(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("parse Docker volume identity response length")?,
                )
                .is_some()
            {
                bail!("duplicate Docker volume identity response length");
            }
        }
    }
    let body = &response[header_end..];
    if content_length != Some(body.len()) {
        bail!("Docker volume identity response has incomplete or ambiguous framing");
    }
    let value = serde_json::from_slice(body).context("parse live Docker volume identity")?;
    Ok(Some(value))
}

fn replace_docker_route_id(
    target: &str,
    canonical_path: &str,
    segments: &[&str],
    index: usize,
    id: &str,
) -> Result<String> {
    let mut rewritten_segments = segments
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect::<Vec<_>>();
    let slot = rewritten_segments
        .get_mut(index)
        .context("Docker resource route has no identifier segment")?;
    *slot = validate_owned_resource_id(id, "Docker resource")?;

    let first = canonical_path
        .strip_prefix('/')
        .and_then(|path| path.split('/').next())
        .unwrap_or_default();
    let version_prefix =
        if first.len() > 1 && first.starts_with('v') && first.as_bytes()[1].is_ascii_digit() {
            format!("/{first}")
        } else {
            String::new()
        };
    let mut rewritten = format!("{version_prefix}/{}", rewritten_segments.join("/"));
    if let Some((_, query)) = target.split_once('?') {
        rewritten.push('?');
        rewritten.push_str(query);
    }
    Ok(rewritten)
}

fn rewrite_http_request(
    request: &[u8],
    target: &str,
    body_override: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header = std::str::from_utf8(&request[..header_end])
        .context("Docker API request headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts.next().context("Docker API request method")?;
    let _old_target = parts.next().context("Docker API request target")?;
    let version = parts.next().context("Docker API HTTP version")?;
    if parts.next().is_some() {
        bail!("malformed Docker API request line");
    }

    let body = body_override.unwrap_or(&request[header_end..]);
    let mut out = Vec::with_capacity(header_end + body.len());
    out.extend_from_slice(format!("{method} {target} {version}\r\n").as_bytes());
    let mut content_length_seen = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            bail!("malformed Docker API request header");
        };
        if body_override.is_some() && name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("refusing to rewrite a chunked Docker request body");
        }
        if body_override.is_some() && name.eq_ignore_ascii_case("content-length") {
            if content_length_seen {
                bail!("refusing to rewrite Docker request with duplicate Content-Length");
            }
            content_length_seen = true;
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if body_override.is_some() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    Ok(out)
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

pub fn remove_one_container_args(id: &str) -> Vec<String> {
    remove_container_args(&[id.to_string()])
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
    inject_ownership_labels_value(&mut value, job_id, daemon_id, None)?;
    serde_json::to_vec(&value).context("serialize labeled Docker create body")
}

fn inject_ownership_labels_value(
    value: &mut Value,
    job_id: &str,
    daemon_id: &str,
    lease_id: Option<&str>,
) -> Result<()> {
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
    if labels.keys().any(|key| {
        key.eq_ignore_ascii_case(BUILDKIT_BUILDER_LABEL)
            || key.eq_ignore_ascii_case(BUILDKIT_OWNER_TOKEN_LABEL)
    }) {
        bail!("Docker create cannot supply Velnor BuildKit owner labels");
    }
    labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.to_string()));
    labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.to_string()));
    if let Some(lease_id) = lease_id {
        labels.insert(LEASE_ID_LABEL.into(), Value::String(lease_id.to_owned()));
    }
    object.insert("Labels".into(), Value::Object(labels));
    Ok(())
}

fn inject_buildkit_owner_label_value(
    value: &mut Value,
    builder: &str,
    owner_token: &str,
) -> Result<()> {
    let object = value
        .as_object_mut()
        .context("Docker BuildKit create body must be an object")?;
    let label_keys = object
        .keys()
        .filter(|key| key.eq_ignore_ascii_case("Labels"))
        .cloned()
        .collect::<Vec<_>>();
    if label_keys.len() > 1 {
        bail!("Docker BuildKit create contains duplicate Labels keys");
    }
    let labels = label_keys
        .first()
        .and_then(|key| object.remove(key))
        .unwrap_or(Value::Object(Map::new()));
    let mut labels = match labels {
        Value::Null => Map::new(),
        Value::Object(labels) => labels,
        _ => bail!("Docker BuildKit Labels must be an object"),
    };
    if labels.keys().any(|key| {
        key.eq_ignore_ascii_case(BUILDKIT_BUILDER_LABEL)
            || key.eq_ignore_ascii_case(BUILDKIT_OWNER_TOKEN_LABEL)
    }) {
        bail!("Docker create cannot supply Velnor BuildKit owner labels");
    }
    labels.insert(
        BUILDKIT_BUILDER_LABEL.into(),
        Value::String(builder.to_owned()),
    );
    labels.insert(
        BUILDKIT_OWNER_TOKEN_LABEL.into(),
        Value::String(owner_token.to_owned()),
    );
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
    rewrite_docker_api_request_with_volumes(request, job_id, daemon_id, &BTreeSet::new())
}

fn rewrite_docker_api_request_with_volumes(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    owned_volume_names: &BTreeSet<String>,
) -> Result<Vec<u8>> {
    rewrite_docker_api_request_with_context(
        request,
        job_id,
        daemon_id,
        owned_volume_names,
        &BTreeSet::new(),
        None,
        None,
        None,
        None,
    )
}

fn rewrite_docker_api_request_with_context(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    owned_volume_names: &BTreeSet<String>,
    owned_network_names: &BTreeSet<String>,
    buildkit_admission: Option<&PersistentBuildKitAdmission>,
    buildkit_volume_admission: Option<&PersistentBuildKitAdmission>,
    lease_id: Option<&str>,
    job_network: Option<&str>,
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
        validate_networking_config(&value, owned_network_names, buildkit_admission)?;
        validate_persistent_buildkit_create(&value, buildkit_admission)?;
        inject_job_cgroup_parent_value(
            &mut value,
            owned_volume_names,
            owned_network_names,
            buildkit_admission,
        )?;
        if let Some(admission) = buildkit_admission {
            inject_persistent_buildkit_network(&mut value, admission, job_network)?;
        }
    } else if path.ends_with("/networks/create") {
        validate_network_create_value(&value)?;
    } else if path.ends_with("/volumes/create") {
        reject_unsafe_volume_create_value(&value)?;
        if let Some(admission) = buildkit_volume_admission {
            inject_buildkit_owner_label_value(
                &mut value,
                &admission.builder,
                &admission.owner_token,
            )?;
        }
    }
    if buildkit_volume_admission.is_none() || !path.ends_with("/volumes/create") {
        inject_ownership_labels_value(&mut value, job_id, daemon_id, lease_id)?;
    }
    if path.ends_with("/containers/create")
        && let Some(admission) = buildkit_admission
    {
        inject_buildkit_owner_label_value(&mut value, &admission.builder, &admission.owner_token)?;
    }
    let labeled = serde_json::to_vec(&value).context("serialize rewritten Docker create body")?;
    if labeled.len() > MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS {
        return Err(LeaseDeny::forbidden(format!(
            "rewritten Docker create body exceeds the {}-byte ownership-label limit",
            MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS
        ))
        .into());
    }
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

fn validate_create_request_size(request: &[u8]) -> Result<()> {
    let body = docker_request_body(request)?;
    if body.len() > MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS {
        return Err(LeaseDeny::forbidden(format!(
            "Docker create body exceeds the {}-byte ownership-label limit",
            MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS
        ))
        .into());
    }
    Ok(())
}

fn validate_volume_create_request(request: &[u8]) -> Result<()> {
    let body = docker_request_body(request)?;
    let value = parse_create_value(body).context("parse Docker volume create request")?;
    volumes_create_request_name(request)?;
    reject_unsafe_volume_create_value(&value)
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
    validate_container_create_request_with_context(
        request,
        owned_volume_names,
        &BTreeSet::new(),
        None,
    )
}

fn validate_container_create_request_with_context(
    request: &[u8],
    owned_volume_names: &BTreeSet<String>,
    owned_network_names: &BTreeSet<String>,
    buildkit_admission: Option<&PersistentBuildKitAdmission>,
) -> Result<()> {
    let body = docker_request_body(request)?;
    let mut value = parse_create_value(body).context("parse Docker container create request")?;
    validate_networking_config(&value, owned_network_names, buildkit_admission)?;
    validate_persistent_buildkit_create(&value, buildkit_admission)?;
    inject_job_cgroup_parent_value(
        &mut value,
        owned_volume_names,
        owned_network_names,
        buildkit_admission,
    )
}

fn validate_persistent_buildkit_create(
    value: &Value,
    admission: Option<&PersistentBuildKitAdmission>,
) -> Result<()> {
    if admission.is_none() {
        return Ok(());
    }
    let object = value
        .as_object()
        .context("Docker BuildKit create body must be an object")?;
    reject_case_insensitive_duplicate_keys(object, "Docker BuildKit create body")?;
    // Buildx v0.36.1 sends Moby's flattened Config DTO. Its non-omitempty
    // zero fields are present on the wire; accept those only at their exact
    // empty defaults. Healthcheck, Entrypoint, OnBuild, Shell, and other
    // omitempty fields stay absent because they can execute or redirect
    // commands inside this privileged daemon.
    const ALLOWED_CONFIG_FIELDS: [&str; 16] = [
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
        "image",
        "volumes",
        "workingdir",
        "entrypoint",
        "labels",
    ];
    for key in object.keys() {
        let normalized = key.to_ascii_lowercase();
        if normalized != "hostconfig"
            && normalized != "networkingconfig"
            && !ALLOWED_CONFIG_FIELDS.contains(&normalized.as_str())
        {
            bail!("persistent BuildKit Config field {key:?} is not permitted");
        }
    }
    let image = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Image"))
        .and_then(|(_, value)| value.as_str())
        .context("admitted BuildKit create must name its image")?;
    if !is_default_buildkit_image(image) {
        bail!("persistent BuildKit create must use the approved BuildKit image");
    }
    for (key, value) in object {
        match key.to_ascii_lowercase().as_str() {
            "image" | "hostconfig" => {}
            "cmd" => validate_persistent_buildkit_command(value)?,
            "networkingconfig" => {
                if !is_exact_default(value) {
                    bail!("persistent BuildKit cannot set NetworkingConfig directly");
                }
            }
            _ if !is_exact_default(value) => {
                bail!("persistent BuildKit Config field {key:?} must have its Buildx default");
            }
            _ => {}
        }
    }
    let host_config = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
        .map(|(_, value)| value)
        .and_then(Value::as_object)
        .context("persistent BuildKit create must include HostConfig")?;
    reject_case_insensitive_duplicate_keys(host_config, "Docker BuildKit HostConfig")?;
    const ALLOWED_HOST_FIELDS: [&str; 64] = [
        "binds",
        "containeridfile",
        "logconfig",
        "networkmode",
        "portbindings",
        "restartpolicy",
        "autoremove",
        "volumedriver",
        "volumesfrom",
        "consolesize",
        "capadd",
        "capdrop",
        "cgroupnsmode",
        "dns",
        "dnsoptions",
        "dnssearch",
        "extrahosts",
        "groupadd",
        "ipcmode",
        "cgroup",
        "links",
        "oomscoreadj",
        "pidmode",
        "privileged",
        "publishallports",
        "readonlyrootfs",
        "securityopt",
        "utsmode",
        "usernsmode",
        "shmsize",
        "isolation",
        "cpushares",
        "memory",
        "nanocpus",
        "cgroupparent",
        "blkioweight",
        "blkioweightdevice",
        "blkiodevicereadbps",
        "blkiodevicewritebps",
        "blkiodevicereadiops",
        "blkiodevicewriteiops",
        "cpuperiod",
        "cpuquota",
        "cpurealtimeperiod",
        "cpurealtimeruntime",
        "cpusetcpus",
        "cpusetmems",
        "devices",
        "devicecgrouprules",
        "devicerequests",
        "memoryreservation",
        "memoryswap",
        "memoryswappiness",
        "oomkilldisable",
        "pidslimit",
        "ulimits",
        "cpucount",
        "cpupercent",
        "iomaximumiops",
        "iomaximumbandwidth",
        "mounts",
        "maskedpaths",
        "readonlypaths",
        "init",
    ];
    for key in host_config.keys() {
        if !ALLOWED_HOST_FIELDS.contains(&key.to_ascii_lowercase().as_str()) {
            bail!("persistent BuildKit HostConfig field {key:?} is not permitted");
        }
    }
    for (key, value) in host_config {
        match key.to_ascii_lowercase().as_str() {
            "privileged" | "init" | "restartpolicy" | "mounts" | "cgroupparent" => {}
            "logconfig" => {
                if !is_default_buildkit_log_config(value) {
                    bail!("persistent BuildKit LogConfig must have its Moby default");
                }
            }
            "consolesize" => {
                if value != &serde_json::json!([0, 0]) {
                    bail!("persistent BuildKit ConsoleSize must have its Moby default");
                }
            }
            "networkmode" => {
                if !value.as_str().is_some_and(str::is_empty) {
                    bail!("Buildx BuildKit NetworkMode must be the empty default");
                }
            }
            _ if !is_exact_default(value) => {
                bail!("persistent BuildKit HostConfig field {key:?} must have its Buildx default");
            }
            _ => {}
        }
    }
    if host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Privileged"))
        .and_then(|(_, value)| value.as_bool())
        != Some(true)
    {
        bail!("persistent BuildKit must use the approved privileged driver shape");
    }
    if host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Init"))
        .and_then(|(_, value)| value.as_bool())
        != Some(true)
    {
        bail!("persistent BuildKit must enable the approved init process");
    }
    validate_persistent_buildkit_state_mounts(
        host_config,
        admission.context("BuildKit admission")?,
    )?;
    let cgroup_parent = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("CgroupParent"))
        .map(|(_, value)| value);
    if cgroup_parent.is_some_and(|value| {
        !value
            .as_str()
            .is_some_and(|parent| parent.is_empty() || parent == "/docker/buildx")
    }) {
        bail!("persistent BuildKit CgroupParent is not the Buildx default");
    }
    let restart = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("RestartPolicy"))
        .map(|(_, value)| value)
        .and_then(Value::as_object)
        .context("persistent BuildKit must include its restart policy")?;
    reject_case_insensitive_duplicate_keys(restart, "Docker BuildKit RestartPolicy")?;
    for key in restart.keys() {
        if !key.eq_ignore_ascii_case("Name") && !key.eq_ignore_ascii_case("MaximumRetryCount") {
            bail!("persistent BuildKit restart-policy field {key:?} is not permitted");
        }
    }
    if restart
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Name"))
        .and_then(|(_, value)| value.as_str())
        != Some("unless-stopped")
        || restart
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("MaximumRetryCount"))
            .is_some_and(|(_, value)| value.as_u64() != Some(0))
    {
        bail!("persistent BuildKit restart policy is not the approved driver default");
    }
    Ok(())
}

fn validate_persistent_buildkit_state_mounts(
    host_config: &Map<String, Value>,
    admission: &PersistentBuildKitAdmission,
) -> Result<()> {
    let mounts = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Mounts"))
        .map(|(_, value)| value)
        .and_then(Value::as_array)
        .context("persistent BuildKit must include its state volume mount")?;
    if mounts.len() != 1 {
        bail!("persistent BuildKit must mount exactly its state volume");
    }
    let mount = mounts[0]
        .as_object()
        .context("persistent BuildKit state mount must be an object")?;
    reject_case_insensitive_duplicate_keys(mount, "Docker BuildKit mount")?;
    for key in mount.keys() {
        if !["type", "source", "target", "readonly", "volumeoptions"]
            .contains(&key.to_ascii_lowercase().as_str())
        {
            bail!("persistent BuildKit mount field {key:?} is not permitted");
        }
    }
    if mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
        .and_then(|(_, value)| value.as_str())
        != Some("volume")
        || mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Source"))
            .and_then(|(_, value)| value.as_str())
            != Some(admission.state_volume.as_str())
        || mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Target"))
            .and_then(|(_, value)| value.as_str())
            != Some("/var/lib/buildkit")
    {
        bail!("persistent BuildKit must mount its exact state volume");
    }
    if mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("ReadOnly"))
        .is_some_and(|(_, value)| value.as_bool() != Some(false))
        || mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("VolumeOptions"))
            .is_some_and(|(_, value)| !is_exact_default(value))
    {
        bail!("persistent BuildKit state mount options must be empty defaults");
    }
    Ok(())
}

fn validate_persistent_buildkit_command(command: &Value) -> Result<()> {
    let Value::Array(arguments) = command else {
        if command.is_null() {
            return Ok(());
        }
        bail!("persistent BuildKit create Cmd must be a string array");
    };
    let arguments = arguments
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .context("persistent BuildKit Cmd entries must be strings")
        })
        .collect::<Result<Vec<_>>>()?;
    // The daemon's default address is its private Unix socket. Buildx only
    // adds these exact flags for Velnor-created builders: the network.host
    // entitlement and the reviewed inline config file. Any address override,
    // debug listener, root path, or other daemon flag is workflow-controlled
    // and can expose or redirect the persistent daemon.
    let allowed = [vec![
        "--config",
        "/etc/buildkit/buildkitd.toml",
        "--allow-insecure-entitlement=network.host",
    ]];
    if allowed.iter().any(|candidate| candidate == &arguments) {
        return Ok(());
    }
    bail!("persistent BuildKit create Cmd has an unapproved flag or listener")
}

fn inject_persistent_buildkit_network(
    value: &mut Value,
    _admission: &PersistentBuildKitAdmission,
    job_network: Option<&str>,
) -> Result<()> {
    let network = validate_owned_resource_id(
        job_network.context("persistent BuildKit lease has no private job network")?,
        "job network",
    )?;
    let object = value
        .as_object_mut()
        .context("Docker BuildKit create body must be an object")?;
    let host_config = object
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
        .map(|(_, value)| value)
        .and_then(Value::as_object_mut)
        .context("persistent BuildKit create must include HostConfig")?;
    reject_case_insensitive_duplicate_keys(host_config, "Docker BuildKit HostConfig")?;
    let key = host_config
        .keys()
        .find(|key| key.eq_ignore_ascii_case("NetworkMode"))
        .cloned()
        .unwrap_or_else(|| "NetworkMode".to_owned());
    if host_config
        .get(&key)
        .is_some_and(|value| !is_default_mode(value))
    {
        bail!("Buildx BuildKit NetworkMode must be its empty default before lease rewrite");
    }
    host_config.insert(key, Value::String(network));
    Ok(())
}

fn validate_networking_config(
    value: &Value,
    owned_network_names: &BTreeSet<String>,
    buildkit_admission: Option<&PersistentBuildKitAdmission>,
) -> Result<()> {
    let Some(object) = value.as_object() else {
        bail!("Docker container create body must be an object");
    };
    let Some(config) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("NetworkingConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(());
    };
    if config.is_null() {
        return Ok(());
    }
    let config = config
        .as_object()
        .context("Docker NetworkingConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(config, "Docker NetworkingConfig")?;
    for key in config.keys() {
        if !key.eq_ignore_ascii_case("EndpointsConfig") {
            bail!("Docker NetworkingConfig field {key:?} is not permitted");
        }
    }
    let Some(endpoints) = config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("EndpointsConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(());
    };
    let endpoints = endpoints
        .as_object()
        .context("Docker EndpointsConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(endpoints, "Docker EndpointsConfig")?;
    if buildkit_admission.is_some() && !endpoints.is_empty() {
        bail!("persistent BuildKit cannot attach to a lease-private network");
    }
    for (network, endpoint) in endpoints {
        if !owned_network_names.contains(network) {
            bail!("Docker container create references unowned network {network:?}");
        }
        if !endpoint.is_object() && !endpoint.is_null() {
            bail!("Docker network endpoint config must be an object or null");
        }
        reject_endpoint_network_id(endpoint, "Docker EndpointSettings")?;
    }
    Ok(())
}

/// Network names and IDs are interchangeable in several Moby API requests.
/// The lease authorizes the owned path/map key, so a second `NetworkID` field
/// must never be able to redirect the attach to another network.
fn reject_endpoint_network_id(value: &Value, context: &str) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let object = value
        .as_object()
        .with_context(|| format!("{context} must be an object or null"))?;
    reject_case_insensitive_duplicate_keys(object, context)?;
    if object
        .keys()
        .any(|key| key.eq_ignore_ascii_case("NetworkID"))
    {
        bail!("{context} cannot override its owned network identity with NetworkID");
    }
    Ok(())
}

fn rewrite_networking_config_ids(
    value: &mut Value,
    network_name_ids: &BTreeMap<String, String>,
) -> Result<bool> {
    let object = value
        .as_object_mut()
        .context("Docker container create body must be an object")?;
    let Some(config) = object
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("NetworkingConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    if config.is_null() {
        return Ok(false);
    }
    let config = config
        .as_object_mut()
        .context("Docker NetworkingConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(config, "Docker NetworkingConfig")?;
    let Some(endpoints) = config
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("EndpointsConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    if endpoints.is_null() {
        return Ok(false);
    }
    let endpoints = endpoints
        .as_object_mut()
        .context("Docker EndpointsConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(endpoints, "Docker EndpointsConfig")?;
    let replacements = endpoints
        .keys()
        .filter_map(|name| {
            network_name_ids
                .get(name)
                .filter(|id| *id != name)
                .map(|id| (name.clone(), id.clone()))
        })
        .collect::<Vec<_>>();
    for endpoint in endpoints.values() {
        reject_endpoint_network_id(endpoint, "Docker EndpointSettings")?;
    }
    for (_, id) in &replacements {
        if endpoints.contains_key(id) {
            bail!("Docker EndpointsConfig contains both a network alias and its pinned ID");
        }
    }
    for (name, id) in &replacements {
        if let Some(endpoint) = endpoints.remove(name) {
            endpoints.insert(id.clone(), endpoint);
        }
    }
    Ok(!replacements.is_empty())
}

fn rewrite_network_mode_id(
    value: &mut Value,
    network_name_ids: &BTreeMap<String, String>,
) -> Result<bool> {
    let object = value
        .as_object_mut()
        .context("Docker container create body must be an object")?;
    let Some(host_config) = object
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    if host_config.is_null() {
        return Ok(false);
    }
    let host_config = host_config
        .as_object_mut()
        .context("Docker container HostConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    let Some((key, mode)) = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("NetworkMode"))
    else {
        return Ok(false);
    };
    let Some(mode) = mode.as_str() else {
        return Ok(false);
    };
    let Some(id) = network_name_ids.get(mode) else {
        return Ok(false);
    };
    if id == mode {
        return Ok(false);
    }
    let key = key.clone();
    host_config.insert(key, Value::String(id.clone()));
    Ok(true)
}

fn rewrite_volume_mount_ids(
    value: &mut Value,
    volume_name_ids: &BTreeMap<String, String>,
) -> Result<bool> {
    let object = value
        .as_object_mut()
        .context("Docker container create body must be an object")?;
    let Some(host_config) = object
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("HostConfig"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    if host_config.is_null() {
        return Ok(false);
    }
    let host_config = host_config
        .as_object_mut()
        .context("Docker container HostConfig must be an object")?;
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    let Some(mounts) = host_config
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case("Mounts"))
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    let Some(mounts) = mounts.as_array_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    for mount in mounts {
        let Some(mount) = mount.as_object_mut() else {
            continue;
        };
        reject_case_insensitive_duplicate_keys(mount, "Docker container mount")?;
        let mount_type = mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
            .and_then(|(_, value)| value.as_str());
        if mount_type != Some("volume") {
            continue;
        }
        let source = mount
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Source"))
            .and_then(|(_, value)| value.as_str());
        let Some(source) = source else {
            continue;
        };
        let Some(id) = volume_name_ids.get(source) else {
            continue;
        };
        let source_key = mount
            .keys()
            .find(|key| key.eq_ignore_ascii_case("Source"))
            .cloned()
            .context("Docker volume mount alias lost its Source field")?;
        mount.insert(source_key, Value::String(id.clone()));
        changed = true;
    }
    Ok(changed)
}

fn is_default_buildkit_image(image: &str) -> bool {
    image == "moby/buildkit:buildx-stable-1" || image == "docker.io/moby/buildkit:buildx-stable-1"
}

fn is_default_buildkit_log_config(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return value.is_null();
    };
    if object.len() != 2 {
        return false;
    }
    let Some(log_type) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
        .map(|(_, value)| value)
    else {
        return false;
    };
    let Some(config) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Config"))
        .map(|(_, value)| value)
    else {
        return false;
    };
    log_type.as_str() == Some("") && config.is_null()
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
    owned_network_names: &BTreeSet<String>,
    buildkit_admission: Option<&PersistentBuildKitAdmission>,
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
    reject_unsafe_nested_host_controls(
        &host_config,
        owned_volume_names,
        owned_network_names,
        buildkit_admission.is_some(),
    )?;
    if let Some(admission) = buildkit_admission {
        inject_buildkit_volume_mount_label(
            &mut host_config,
            &admission.builder,
            &admission.owner_token,
        )?;
    }
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

fn inject_buildkit_volume_mount_label(
    host_config: &mut Map<String, Value>,
    builder: &str,
    owner_token: &str,
) -> Result<()> {
    let key = host_config
        .keys()
        .find(|key| key.eq_ignore_ascii_case("Mounts"))
        .cloned()
        .context("persistent BuildKit create must include HostConfig.Mounts")?;
    let Some(Value::Array(mounts)) = host_config.get_mut(&key) else {
        bail!("persistent BuildKit create Mounts must be an array");
    };
    if mounts.len() != 1 {
        bail!("persistent BuildKit create must mount exactly its state volume");
    }
    let mount = mounts[0]
        .as_object_mut()
        .context("persistent BuildKit state mount must be an object")?;
    reject_case_insensitive_duplicate_keys(mount, "Docker BuildKit mount")?;
    let source = mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Source"))
        .and_then(|(_, value)| value.as_str());
    let target = mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Target"))
        .and_then(|(_, value)| value.as_str());
    let mount_type = mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Type"))
        .and_then(|(_, value)| value.as_str());
    if mount_type != Some("volume")
        || source != Some(crate::buildkit::daemon_state_volume(builder).as_str())
        || target != Some("/var/lib/buildkit")
    {
        bail!("persistent BuildKit create must mount only its exact state volume");
    }
    if mount
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("ReadOnly"))
        .is_some_and(|(_, value)| value.as_bool() != Some(false))
    {
        bail!("persistent BuildKit state volume must be writable");
    }
    let options_key = mount
        .keys()
        .find(|key| key.eq_ignore_ascii_case("VolumeOptions"))
        .cloned();
    let options = options_key
        .and_then(|key| mount.remove(&key))
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut options = match options {
        Value::Null => Map::new(),
        Value::Object(options) => options,
        _ => bail!("persistent BuildKit VolumeOptions must be an object"),
    };
    reject_case_insensitive_duplicate_keys(&options, "Docker BuildKit VolumeOptions")?;
    for key in options.keys() {
        if !key.eq_ignore_ascii_case("NoCopy") && !key.eq_ignore_ascii_case("Labels") {
            bail!("persistent BuildKit VolumeOptions field {key:?} is not permitted");
        }
    }
    if options
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("NoCopy"))
        .is_some_and(|(_, value)| value.as_bool() != Some(false))
    {
        bail!("persistent BuildKit state volume NoCopy must be false");
    }
    let label_key = options
        .keys()
        .find(|key| key.eq_ignore_ascii_case("Labels"))
        .cloned();
    let labels = label_key
        .and_then(|key| options.remove(&key))
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut labels = match labels {
        Value::Null => Map::new(),
        Value::Object(labels) if labels.is_empty() => labels,
        _ => bail!("persistent BuildKit volume labels are runner-managed"),
    };
    labels.insert(
        BUILDKIT_BUILDER_LABEL.into(),
        Value::String(builder.to_owned()),
    );
    labels.insert(
        BUILDKIT_OWNER_TOKEN_LABEL.into(),
        Value::String(owner_token.to_owned()),
    );
    options.insert("Labels".into(), Value::Object(labels));
    mount.insert("VolumeOptions".into(), Value::Object(options));
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
        && is_strict_value_present(options)
    {
        bail!("Docker volume create field {key:?} requests host control access");
    }
    Ok(())
}

fn reject_unsafe_nested_host_controls(
    host_config: &Map<String, Value>,
    owned_volume_names: &BTreeSet<String>,
    owned_network_names: &BTreeSet<String>,
    admitted_buildkit: bool,
) -> Result<()> {
    reject_case_insensitive_duplicate_keys(host_config, "Docker container HostConfig")?;
    let network_mode = host_config
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("NetworkMode"))
        .map(|(_, value)| value);
    if !admitted_buildkit && network_mode.is_none() {
        bail!("Docker nested container must select an owned network or NetworkMode=none");
    }
    for (key, value) in host_config {
        let unsafe_control = match key.to_ascii_lowercase().as_str() {
            "networkmode" => !is_guest_network_mode(value, owned_network_names, admitted_buildkit),
            "pidmode" | "ipcmode" | "cgroupnsmode" | "usernsmode" | "utsmode" => {
                !is_default_mode(value)
            }
            // Live docker/build-push-action starts BuildKit with Privileged.
            // That is nested engine bootstrap, not a general guest escape.
            // Any other image stays denied.
            "privileged" => match value {
                Value::Null | Value::Bool(false) => false,
                Value::Bool(true) => !admitted_buildkit,
                _ => true,
            },
            "capadd" | "devices" | "devicecgrouprules" | "securityopt" | "runtime" | "sysctls"
            | "volumedriver" | "volumesfrom" | "volumeoptions" | "containeridfile" => {
                is_strict_value_present(value)
            }
            // Live API 1.55: `{"Name":"no","MaximumRetryCount":0}` is the
            // default (no restart). BuildKit uses `unless-stopped`.
            // `always` / `on-failure` stay denied.
            "restartpolicy" => !is_guest_restart_policy(value, admitted_buildkit)?,
            // Device requests can pass host GPUs and driver capabilities into
            // the privileged persistent daemon. Buildx's optional GPU probe
            // is rejected, so the real daemon must carry no device request.
            "devicerequests" => !is_guest_device_requests(value)?,
            // Binds are always host paths. Named Type:volume mounts are guest
            // objects. Live buildx reuses a persistent builder volume created
            // by an earlier job on the same daemon, so this lease cannot
            // require create-recording. Host-path sources stay denied.
            "binds" => !is_empty_mount_list(value),
            "mounts" => !is_guest_or_empty_mounts(value, owned_volume_names, admitted_buildkit)?,
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
            // Job Docker objects must not publish daemon host ports, and the
            // privileged persistent BuildKit daemon cannot expose listeners
            // outside its verified private network either.
            "portbindings" | "publishallports" => is_strict_value_present(value),
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
    value.as_str() == Some("")
}

fn is_guest_network_mode(
    value: &Value,
    owned_network_names: &BTreeSet<String>,
    admitted_buildkit: bool,
) -> bool {
    let Some(mode) = value.as_str().map(str::trim) else {
        return false;
    };
    if mode.eq_ignore_ascii_case("none") {
        return true;
    }
    if admitted_buildkit {
        // Buildx's zero value is rewritten to the current job network before
        // Engine sees it. Accept no shared/default bridge alias here.
        return mode.is_empty();
    }
    owned_network_names.contains(mode)
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

fn is_guest_or_empty_mounts(
    value: &Value,
    owned_volume_names: &BTreeSet<String>,
    admitted_buildkit: bool,
) -> Result<bool> {
    let Value::Array(items) = value else {
        return Ok(value.is_null());
    };
    if items.is_empty() {
        return Ok(true);
    }
    for item in items {
        if !is_guest_named_volume_mount(item, owned_volume_names, admitted_buildkit)? {
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
    admitted_buildkit: bool,
) -> Result<bool> {
    let Value::Object(object) = value else {
        return Ok(false);
    };
    reject_case_insensitive_duplicate_keys(object, "Docker mount object")?;
    const ALLOWED_FIELDS: [&str; 6] = [
        "type",
        "source",
        "target",
        "readonly",
        "consistency",
        "volumeoptions",
    ];
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
    if !owned_volume_names.contains(source) {
        return Ok(false);
    }
    if let Some(options) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("VolumeOptions"))
        .map(|(_, value)| value)
    {
        if !admitted_buildkit {
            return Ok(false);
        }
        if !options.is_null() && !options.as_object().is_some_and(Map::is_empty) {
            return Ok(false);
        }
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

fn is_guest_restart_policy(value: &Value, admitted_buildkit: bool) -> Result<bool> {
    if is_default_restart_policy(value)? {
        return Ok(true);
    }
    Ok(admitted_buildkit && is_named_restart_policy(value, &["unless-stopped"])?)
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
    Ok(matches!(value, Value::Array(items) if items.is_empty()))
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

/// Exact zero-value matcher for privileged BuildKit create requests. The
/// general container policy tolerates recursively empty nested values for
/// Docker CLI compatibility; in BuildKit Config, an object key such as
/// `Volumes: {"/run":{}}` is itself meaningful even when its value is empty.
fn is_exact_default(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !*value,
        Value::String(value) => value.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(object) => object.is_empty(),
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
    if !snapshot.volumes.is_empty() {
        remove(&force_remove_volume_args(&snapshot.volumes))?;
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
    reclaim_listed(
        &list_owned_volumes_args(job_id),
        &mut docker,
        remove_volume_args,
    )?;
    Ok(())
}

/// Force-remove generic containers carrying `velnor.job-id=<job_id>`, running
/// or stopped. Persistent BuildKit daemons are reserved for the durable owner
/// registry and are excluded from this label-based recovery path.
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
    Ok(())
}

pub fn reclaim_orphan_jobs(mut docker: impl FnMut(&[String]) -> Result<String>) -> Result<()> {
    let formatted = docker(&list_owned_job_format_args())?;
    for job_id in docker_client::orphan_job_ids(&formatted) {
        reclaim_stale_job_owned(&job_id, &mut docker)?;
    }
    Ok(())
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
    for job_id in docker_client::daemon_orphan_job_ids(&formatted, daemon_id) {
        reclaim_stale_job_owned(&job_id, &mut docker)?;
    }
    Ok(())
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

pub struct DockerLeaseGuard {
    listen_path: Option<PathBuf>,
    guest_docker_host: String,
    guest_docker_cert_path: Option<String>,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    #[cfg(unix)]
    conns: Arc<LeaseConnSet>,
    #[cfg(unix)]
    policy: Arc<DockerLeasePolicy>,
    #[cfg(unix)]
    shutdown_wake: Option<Arc<Mutex<std::os::unix::net::UnixStream>>>,
    #[cfg(unix)]
    tls_bundle_cleanup: Option<crate::docker_lease_tls::LeaseTlsBundleCleanup>,
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
    tcp_streams: Mutex<BTreeMap<u64, std::net::TcpStream>>,
    shutdown_wake: Mutex<Option<Arc<Mutex<std::os::unix::net::UnixStream>>>>,
}

#[cfg(unix)]
struct WatchedStream {
    set: Arc<LeaseConnSet>,
    id: u64,
}

#[cfg(unix)]
struct WatchedTcpStream {
    set: Arc<LeaseConnSet>,
    id: u64,
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
            tcp_streams: Mutex::new(BTreeMap::new()),
            shutdown_wake: Mutex::new(None),
        })
    }

    fn set_shutdown_wake(&self, wake: Arc<Mutex<std::os::unix::net::UnixStream>>) {
        *self
            .shutdown_wake
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(wake);
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
        if !self.shutdown.swap(true, Ordering::SeqCst)
            && let Some(wake) = self
                .shutdown_wake
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .as_ref()
        {
            let _ = wake
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .write_all(&[1]);
        }
        {
            let mut streams = self.streams.lock().unwrap_or_else(|err| err.into_inner());
            let drained = std::mem::take(&mut *streams);
            for (_, stream) in drained {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        {
            let mut streams = self
                .tcp_streams
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            let drained = std::mem::take(&mut *streams);
            for (_, stream) in drained {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn watch(self: &Arc<Self>, stream: &std::os::unix::net::UnixStream) -> Result<WatchedStream> {
        self.watch_with_clone_result(stream, stream.try_clone())
    }

    fn watch_with_clone_result(
        self: &Arc<Self>,
        stream: &std::os::unix::net::UnixStream,
        clone: io::Result<std::os::unix::net::UnixStream>,
    ) -> Result<WatchedStream> {
        let clone = match clone {
            Ok(clone) => clone,
            Err(error) => {
                self.abort();
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(error).context("clone Docker lease Unix stream for abort tracking");
            }
        };
        let mut next = self.next_id.lock().unwrap_or_else(|err| err.into_inner());
        let id = *next;
        *next = next.saturating_add(1);
        self.streams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(id, clone);
        drop(next);
        if self.is_shutdown() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        Ok(WatchedStream {
            set: Arc::clone(self),
            id,
        })
    }

    fn unregister(&self, id: u64) {
        self.streams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&id);
    }

    fn watch_tcp(self: &Arc<Self>, stream: &std::net::TcpStream) -> Result<WatchedTcpStream> {
        self.watch_tcp_with_clone_result(stream, stream.try_clone())
    }

    fn watch_tcp_with_clone_result(
        self: &Arc<Self>,
        stream: &std::net::TcpStream,
        clone: io::Result<std::net::TcpStream>,
    ) -> Result<WatchedTcpStream> {
        let clone = match clone {
            Ok(clone) => clone,
            Err(error) => {
                self.abort();
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(error)
                    .context("clone job Docker lease TCP client stream for abort tracking");
            }
        };
        let mut next = self.next_id.lock().unwrap_or_else(|err| err.into_inner());
        let id = *next;
        *next = next.saturating_add(1);
        self.tcp_streams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(id, clone);
        drop(next);
        if self.is_shutdown() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        Ok(WatchedTcpStream {
            set: Arc::clone(self),
            id,
        })
    }

    fn unregister_tcp(&self, id: u64) {
        self.tcp_streams
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
        self.set.unregister(self.id);
    }
}

#[cfg(unix)]
impl Drop for WatchedTcpStream {
    fn drop(&mut self) {
        self.set.unregister_tcp(self.id);
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

    /// Bind a job lease over TCP for Docker Desktop and OrbStack guests that
    /// cannot connect to a host-bound Unix socket inode. The guest reaches
    /// this per-job listener through `host.docker.internal`.
    pub fn bind_tcp(job_id: String, daemon_id: String, cert_dir: PathBuf) -> Result<Self> {
        let host_socket = crate::docker::engine::resolve_docker_endpoint()
            .context("resolve Docker endpoint for job TCP lease")?
            .socket;
        Self::bind_tcp_to(
            std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            "host.docker.internal",
            host_socket,
            job_id,
            daemon_id,
            cert_dir,
        )
    }

    #[cfg(unix)]
    fn bind_tcp_to(
        listen_addr: std::net::SocketAddr,
        guest_host: &str,
        host_socket: PathBuf,
        job_id: String,
        daemon_id: String,
        cert_dir: PathBuf,
    ) -> Result<Self> {
        bind_tcp_lease(
            listen_addr,
            guest_host,
            host_socket,
            job_id,
            daemon_id,
            cert_dir,
        )
    }

    #[cfg(not(unix))]
    fn bind_tcp_to(
        listen_addr: std::net::SocketAddr,
        guest_host: &str,
        host_socket: PathBuf,
        job_id: String,
        daemon_id: String,
        cert_dir: PathBuf,
    ) -> Result<Self> {
        let _ = (
            listen_addr,
            guest_host,
            host_socket,
            job_id,
            daemon_id,
            cert_dir,
        );
        bail!("job Docker lease proxy requires unix")
    }

    pub(crate) fn guest_docker_host(&self) -> &str {
        &self.guest_docker_host
    }

    pub(crate) fn guest_docker_cert_path(&self) -> Option<&str> {
        self.guest_docker_cert_path.as_deref()
    }

    /// Permit the one daemon container belonging to a builder whose durable
    /// aggregate-cap claim has already been published.
    pub(crate) fn admit_persistent_buildkit_builder(
        &self,
        builder: &str,
        approved_config: &[u8],
    ) -> Result<()> {
        #[cfg(unix)]
        {
            self.policy
                .admit_persistent_buildkit_builder(builder, approved_config)
        }
        #[cfg(not(unix))]
        {
            let _ = (builder, approved_config);
            bail!("job Docker lease proxy requires unix")
        }
    }

    pub(crate) fn set_job_network(&self, network: &str) -> Result<()> {
        #[cfg(unix)]
        {
            self.policy.set_job_network(network)
        }
        #[cfg(not(unix))]
        {
            let _ = network;
            bail!("job Docker lease proxy requires unix")
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
            if let Some(wake) = self.shutdown_wake.take() {
                // Wake the poll set directly. Synthetic listener connects
                // raced shutdown and could still leave the accept thread
                // inside a blocking accept on busy hosts.
                let _ = wake
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .write_all(&[1]);
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
        if let Some(listen_path) = &self.listen_path {
            let _ = std::fs::remove_file(listen_path);
            // dockerd auto-creates an empty DIRECTORY at a missing bind-mount
            // source; if this path ever became one, drop it too (remove_dir only
            // succeeds on empty dirs, so a real socket file tree is untouched).
            let _ = std::fs::remove_dir(listen_path);
        }
        #[cfg(unix)]
        drop(self.tls_bundle_cleanup.take());
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
    let shutdown_wake = Arc::new(Mutex::new(wake_writer));
    let shutdown = Arc::new(AtomicBool::new(false));
    let conns = LeaseConnSet::new(Arc::clone(&shutdown));
    conns.set_shutdown_wake(Arc::clone(&shutdown_wake));
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
        listen_path: Some(listen_path),
        guest_docker_host: "unix:///var/run/docker.sock".to_owned(),
        guest_docker_cert_path: None,
        shutdown,
        accept_thread: Some(accept_thread),
        conns,
        policy,
        shutdown_wake: Some(shutdown_wake),
        tls_bundle_cleanup: None,
    })
}

#[cfg(unix)]
fn bind_tcp_lease(
    listen_addr: std::net::SocketAddr,
    guest_host: &str,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    cert_dir: PathBuf,
) -> Result<DockerLeaseGuard> {
    use std::net::TcpListener;

    if guest_host.is_empty()
        || guest_host.contains('/')
        || guest_host.contains(':')
        || guest_host.chars().any(char::is_whitespace)
    {
        bail!("invalid host name for guest Docker TCP lease");
    }
    let listener = TcpListener::bind(listen_addr)
        .with_context(|| format!("bind job Docker TCP lease on {listen_addr}"))?;
    listener
        .set_nonblocking(true)
        .context("configure job Docker TCP lease listener")?;
    let port = listener
        .local_addr()
        .context("read job Docker TCP lease address")?
        .port();
    let guest_docker_host = format!("tcp://{guest_host}:{port}");
    let (wake_reader, wake_writer) =
        std::os::unix::net::UnixStream::pair().context("create job Docker TCP lease wake")?;
    let shutdown_wake = Arc::new(Mutex::new(wake_writer));
    let shutdown = Arc::new(AtomicBool::new(false));
    let conns = LeaseConnSet::new(Arc::clone(&shutdown));
    conns.set_shutdown_wake(Arc::clone(&shutdown_wake));
    let policy = Arc::new(DockerLeasePolicy::new(&job_id)?);
    let (tls_config, guest_cert_path, tls_bundle_cleanup) =
        crate::docker_lease_tls::create_lease_tls(&cert_dir, guest_host)
            .context("create per-job Docker lease TLS identity")?;
    let guest_docker_cert_path = guest_cert_path.to_string_lossy().into_owned();
    let accept_thread = std::thread::Builder::new()
        .name(format!("velnor-docker-lease-tcp-{job_id}"))
        .spawn({
            let conns = Arc::clone(&conns);
            let policy = Arc::clone(&policy);
            let shutdown = Arc::clone(&shutdown);
            let tls_config = Arc::clone(&tls_config);
            move || {
                accept_tcp_loop(
                    listener,
                    host_socket,
                    job_id,
                    daemon_id,
                    conns,
                    policy,
                    shutdown,
                    tls_config,
                    wake_reader,
                )
            }
        });
    let accept_thread = match accept_thread {
        Ok(thread) => thread,
        Err(error) => {
            return Err(error).context("start job Docker TCP lease proxy thread");
        }
    };
    Ok(DockerLeaseGuard {
        listen_path: None,
        guest_docker_host,
        guest_docker_cert_path: Some(guest_docker_cert_path),
        shutdown,
        accept_thread: Some(accept_thread),
        conns,
        policy,
        shutdown_wake: Some(shutdown_wake),
        tls_bundle_cleanup: Some(tls_bundle_cleanup),
    })
}

#[cfg(unix)]
fn accept_tcp_loop(
    listener: std::net::TcpListener,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    conns: Arc<LeaseConnSet>,
    policy: Arc<DockerLeasePolicy>,
    shutdown: Arc<AtomicBool>,
    tls_config: Arc<tokio_rustls::rustls::ServerConfig>,
    wake_reader: std::os::unix::net::UnixStream,
) {
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
    while !shutdown.load(Ordering::SeqCst) {
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
        let (tcp, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(_) => break,
        };
        let Some(permit) = conns.try_acquire_connection() else {
            let _ = tcp.shutdown(std::net::Shutdown::Both);
            continue;
        };
        let tcp_watch = match conns.watch_tcp(&tcp) {
            Ok(watch) => watch,
            Err(error) => {
                eprintln!("Warning: job Docker TCP lease stream registration: {error:#}");
                continue;
            }
        };
        let (proxy_client, bridge_stream) = match std::os::unix::net::UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("Warning: job Docker TCP lease pair: {error}");
                let _ = tcp.shutdown(std::net::Shutdown::Both);
                continue;
            }
        };
        let host_socket = host_socket.clone();
        let job_id = job_id.clone();
        let daemon_id = daemon_id.clone();
        let conns = Arc::clone(&conns);
        let policy = Arc::clone(&policy);
        let tls_config = Arc::clone(&tls_config);
        let _ = std::thread::Builder::new()
            .name("velnor-docker-lease-tcp-conn".into())
            .spawn(move || {
                let _permit = permit;
                let _tcp_watch = tcp_watch;
                let tls_config = Arc::clone(&tls_config);
                let bridge = std::thread::Builder::new()
                    .name("velnor-docker-lease-tcp-bridge".into())
                    .spawn(move || {
                        crate::docker_lease_tls::bridge_tls_tcp_to_unix(
                            tcp,
                            tls_config,
                            bridge_stream,
                        )
                    });
                if let Err(error) = handle_client_with(
                    proxy_client,
                    &host_socket,
                    &job_id,
                    &daemon_id,
                    conns,
                    policy,
                ) {
                    eprintln!("Warning: job Docker TCP lease proxy: {error:#}");
                }
                if let Ok(bridge) = bridge {
                    if let Err(error) = bridge.join() {
                        eprintln!("Warning: job Docker TCP lease bridge panicked: {error:?}");
                    }
                }
            });
    }
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
    let _client_watch = conns.watch(&client)?;
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
        let initial_buildkit_inspect = policy.initial_buildkit_bootstrap_inspect(&bytes)?;
        let create_buildkit_admission =
            if authorization == AuthorizedDockerRoute::Create(DockerResourceKind::Container) {
                policy.persistent_buildkit_admission(&bytes)?
            } else {
                None
            };
        let create_buildkit_volume_admission =
            if authorization == AuthorizedDockerRoute::Create(DockerResourceKind::Volume) {
                policy.persistent_buildkit_volume_admission(&bytes)?
            } else {
                None
            };
        let request_method = http_request_method(&bytes)?.to_owned();
        let request_wants_close = http_request_wants_close(&bytes);
        let upgrade = request_is_upgrade(&bytes);
        let response_volume_alias = docker_volume_read_alias(&bytes)?;
        let volume_api_version = docker_api_version_prefix(&bytes)?;
        let create_resource_alias = match authorization {
            AuthorizedDockerRoute::Create(DockerResourceKind::Container) => {
                containers_create_query_name(&bytes)?
            }
            AuthorizedDockerRoute::Create(DockerResourceKind::Network) => {
                networks_create_request_name(&bytes)?
            }
            AuthorizedDockerRoute::Create(DockerResourceKind::Volume) => {
                volumes_create_request_name(&bytes)?
            }
            _ => None,
        };
        let delete_reference = docker_delete_resource_reference(&bytes)?;
        let delete_resource = delete_reference
            .as_ref()
            .map(|(kind, id)| {
                policy
                    .resolve_owned_id(*kind, id)
                    .map(|resolved| (*kind, resolved))
            })
            .transpose()?;
        let owned_reference = docker_resource_reference(&bytes)?;
        let owned_resource = if initial_buildkit_inspect.is_some() {
            None
        } else {
            owned_reference
                .as_ref()
                .map(|(kind, id)| {
                    policy
                        .resolve_owned_id(*kind, id)
                        .map(|resolved| (*kind, resolved))
                })
                .transpose()?
        };
        if let Some((DockerResourceKind::Container, container_id)) = owned_resource.as_ref()
            && let Some(admission) = policy.persistent_buildkit_container_admission(container_id)?
        {
            policy.ensure_persistent_buildkit_network(
                &admission,
                container_id,
                job_id,
                daemon_id,
            )?;
        }
        let volume_path_reference = response_volume_alias.as_deref().or_else(|| {
            delete_reference
                .as_ref()
                .and_then(|(kind, id)| (*kind == DockerResourceKind::Volume).then_some(id.as_str()))
        });
        if let Some(alias) = volume_path_reference {
            let admission = policy.persistent_buildkit_volume_admission_by_name(alias)?;
            let checked: Result<()> = if let Some(admission) = admission {
                policy.require_admitted_buildkit_volume(&admission)
            } else {
                policy
                    .resolve_owned_id(DockerResourceKind::Volume, alias)
                    .and_then(|physical| {
                        require_live_job_volume(
                            host_socket,
                            &volume_api_version,
                            &physical,
                            job_id,
                            daemon_id,
                            &policy.lease_id,
                        )
                    })
            };
            if let Err(error) = checked {
                if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                    if deny.status == 404
                        && let Ok(physical) =
                            policy.resolve_owned_id(DockerResourceKind::Volume, alias)
                    {
                        policy.retire_deleted_alias(DockerResourceKind::Volume, &physical)?;
                    }
                    write_deny_response(&mut client, deny.status, &deny.message)?;
                }
                return Err(error);
            }
        }
        if authorization == AuthorizedDockerRoute::Create(DockerResourceKind::Container) {
            let admission = create_buildkit_admission.as_ref();
            for source in docker_volume_mount_sources(&bytes)? {
                if admission.is_some_and(|admission| admission.state_volume == source) {
                    policy.require_admitted_buildkit_volume(
                        admission.context("BuildKit volume source lost its admission")?,
                    )?;
                    continue;
                }
                let checked = policy
                    .resolve_owned_id(DockerResourceKind::Volume, &source)
                    .and_then(|physical| {
                        require_live_job_volume(
                            host_socket,
                            &volume_api_version,
                            &physical,
                            job_id,
                            daemon_id,
                            &policy.lease_id,
                        )
                    });
                if let Err(error) = checked {
                    if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                        write_deny_response(&mut client, deny.status, &deny.message)?;
                    }
                    return Err(error);
                }
            }
        }
        // Keep the original request available for persistent BuildKit
        // lifecycle checks after rewriting it for the host Engine. Account for
        // the retained copy in the per-request byte budget.
        budget.reserve(bytes.len())?;
        let lifecycle_request = bytes.clone();
        let forwarded = match transform_request_buffer(bytes, &mut budget, |request| {
            policy.rewrite_docker_api_request(request, job_id, daemon_id)
        }) {
            Ok(forwarded) => forwarded,
            Err(error) => {
                if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                    write_deny_response(&mut client, deny.status, &deny.message)?;
                }
                return Err(error);
            }
        };
        let forwarded = transform_request_buffer(forwarded, &mut budget, without_expect_continue)?;
        let mut create_reservation = if matches!(authorization, AuthorizedDockerRoute::Create(_)) {
            match policy.reserve_create() {
                Ok(reservation) => Some(reservation),
                Err(error) => {
                    if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                        write_deny_response(&mut client, deny.status, &deny.message)?;
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        if conns.is_shutdown() {
            return Ok(());
        }
        if upgrade {
            let (mut host, _host_watch) = connect_lease_host(host_socket, &conns)?;
            let persistent_exec_start =
                match policy.reserve_persistent_buildkit_exec_start(&lifecycle_request) {
                    Ok(exec) => exec,
                    Err(error) => {
                        if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                            write_deny_response(&mut client, deny.status, &deny.message)?;
                        }
                        return Err(error);
                    }
                };
            if let Some(reservation) = create_reservation.as_mut() {
                reservation.retain_if_create_may_have_committed();
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
            let mut host_buffer = ResponseBuffer::default();
            let mut switched_protocol = false;
            let mut response_status = None;
            let _response_complete = forward_http_response_with_observer(
                &mut host,
                &mut host_buffer,
                &mut client,
                &request_method,
                false,
                true,
                |status, _body| {
                    switched_protocol |= status == 101;
                    response_status = Some(status);
                    if matches!(status, 404 | 410)
                        && let Some((kind, id)) = owned_resource.as_ref()
                    {
                        policy.retire_deleted_alias(*kind, id)?;
                    }
                    Ok(None)
                },
            )?;
            if !switched_protocol {
                if let Some((exec_id, _)) = persistent_exec_start.as_ref() {
                    policy.complete_persistent_buildkit_exec_start(
                        exec_id,
                        response_status.context("persistent BuildKit ExecAttach omitted status")?,
                    )?;
                }
                return Ok(());
            }
            if !host_buffer.is_empty() {
                client
                    .write_all(host_buffer.as_slice())
                    .context("forward buffered Docker hijack bytes through job lease")?;
                host_buffer.clear();
            }
            if let Some((exec_id, exec)) = persistent_exec_start.as_ref()
                && exec.command == PersistentBuildKitExecCommand::DialStdio
            {
                clear_dial_stdio_timeouts(&host, &client)?;
                let result = proxy_until_closed_without_timeout(host, client);
                policy.retire_deleted_alias(DockerResourceKind::Exec, exec_id)?;
                return result;
            }
            if let Some((exec_id, _)) = persistent_exec_start.as_ref() {
                return match proxy_until_closed(host, client) {
                    Ok(()) => {
                        // A 101 header alone does not prove that a readiness
                        // stream was framed and completed. Only persist the
                        // Streamed phase after both sides close cleanly.
                        policy.complete_persistent_buildkit_exec_start(exec_id, 101)
                    }
                    Err(error) => {
                        policy.retire_deleted_alias(DockerResourceKind::Exec, exec_id)?;
                        Err(error)
                    }
                };
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
            let mut persistent_archive = match policy
                .reserve_persistent_buildkit_archive(&lifecycle_request)
            {
                Ok(Some((admission, container_id))) => Some(PersistentBuildKitArchiveReservation {
                    policy: Arc::clone(&policy),
                    admission,
                    container_id,
                    completed: false,
                }),
                Ok(None) => None,
                Err(error) => {
                    if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                        write_deny_response(&mut client, deny.status, &deny.message)?;
                    }
                    return Err(error);
                }
            };
            let persistent_start =
                match policy.persistent_buildkit_start_context(&lifecycle_request) {
                    Ok(start) => start,
                    Err(error) => {
                        if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                            write_deny_response(&mut client, deny.status, &deny.message)?;
                        }
                        return Err(error);
                    }
                };
            let persistent_exec_create =
                if authorization == AuthorizedDockerRoute::Create(DockerResourceKind::Exec) {
                    match policy.reserve_persistent_buildkit_exec_create(&lifecycle_request) {
                        Ok(create) => create,
                        Err(error) => {
                            if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                                write_deny_response(&mut client, deny.status, &deny.message)?;
                            }
                            return Err(error);
                        }
                    }
                } else {
                    None
                };
            let persistent_exec_inspect =
                match policy.reserve_persistent_buildkit_exec_inspect(&lifecycle_request) {
                    Ok(inspect) => inspect,
                    Err(error) => {
                        if let Some(deny) = error.downcast_ref::<LeaseDeny>() {
                            write_deny_response(&mut client, deny.status, &deny.message)?;
                        }
                        return Err(error);
                    }
                };
            if let Some(reservation) = create_reservation.as_mut() {
                reservation.retain_if_create_may_have_committed();
            }
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
            let response_deadline = docker_api_response_deadline(
                &lifecycle_request,
                Instant::now() + PROXY_UNFRAMED_RESPONSE_TIMEOUT,
            )?;
            match forward_http_response_with_observer_deadline(
                host,
                &mut host_buffer,
                &mut client,
                &request_method,
                create_kind.is_some()
                    || persistent_exec_inspect.is_some()
                    || initial_buildkit_inspect.is_some()
                    || (response_volume_alias.is_some()
                        && !request_method.eq_ignore_ascii_case("HEAD")),
                false,
                response_deadline,
                |status, body| {
                    if (100..200).contains(&status) && status != 101 {
                        return Ok(None);
                    }
                    if initial_buildkit_inspect.is_some() && status != 404 {
                        policy.validate_initial_buildkit_inspect_response(status)?;
                    }
                    if let Some(archive) = persistent_archive.as_mut() {
                        archive.complete(status)?;
                    }
                    if let Some((admission, container_id)) = persistent_start.as_ref() {
                        policy.complete_persistent_buildkit_start(
                            admission,
                            container_id,
                            status,
                        )?;
                    }
                    if let Some((exec_id, exec)) = persistent_exec_inspect.as_ref() {
                        policy.complete_persistent_buildkit_exec_inspect(
                            exec_id, exec, status, body,
                        )?;
                    }
                    if let Some(create) = persistent_exec_create.as_ref() {
                        policy.record_persistent_buildkit_exec_create(create, status, body)?;
                    }
                    if let (Some(name), Some(admission)) = (
                        create_resource_alias.as_deref(),
                        create_buildkit_admission.as_ref(),
                    ) {
                        if let Some(container_id) = policy.record_persistent_buildkit_create(
                            name, admission, job_id, status, body,
                        )? {
                            policy.ensure_persistent_buildkit_network(
                                admission,
                                &container_id,
                                job_id,
                                daemon_id,
                            )?;
                        }
                    }
                    let mut replacement = None;
                    if let Some(kind) = create_kind.filter(|_| persistent_exec_create.is_none()) {
                        match (kind, create_buildkit_volume_admission.as_ref()) {
                            (DockerResourceKind::Volume, Some(admission)) => {
                                policy.record_admitted_buildkit_volume_create(
                                    admission, status, body,
                                )?;
                            }
                            (DockerResourceKind::Volume, None) => {
                                let alias = create_resource_alias
                                    .as_deref()
                                    .context("volume create request lost its alias")?;
                                let physical = policy.private_volume_name(alias)?;
                                policy.record_create_response_with_owner(
                                    kind,
                                    status,
                                    body,
                                    Some(alias),
                                    Some(job_id),
                                    Some(daemon_id),
                                )?;
                                if (200..300).contains(&status) {
                                    replacement =
                                        Some(rewrite_volume_name_response(body, &physical, alias)?);
                                }
                            }
                            _ => policy.record_create_response_with_alias(
                                kind,
                                status,
                                body,
                                create_resource_alias.as_deref(),
                            )?,
                        }
                    }
                    if matches!(status, 404 | 410) {
                        if let Some((kind, id)) = owned_resource.as_ref() {
                            policy.retire_deleted_alias(*kind, id)?;
                        }
                    } else if (200..300).contains(&status) {
                        if let Some((kind, id)) = delete_resource.as_ref() {
                            policy.retire_deleted_alias(*kind, id)?;
                        }
                    }
                    if (200..300).contains(&status)
                        && let Some(alias) = response_volume_alias.as_deref()
                        && create_kind.is_none()
                        && !request_method.eq_ignore_ascii_case("HEAD")
                    {
                        replacement = Some(rewrite_volume_name_response(
                            body,
                            &policy.resolve_owned_id(DockerResourceKind::Volume, alias)?,
                            alias,
                        )?);
                    }
                    if let Some(reservation) = create_reservation.as_mut() {
                        reservation.finish()?;
                    }
                    Ok(replacement)
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
    let watch = conns.watch(&host)?;
    if conns.is_shutdown() {
        return Ok((host, watch));
    }
    Ok((host, watch))
}

#[cfg(unix)]
fn clear_dial_stdio_timeouts(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
) -> Result<()> {
    host.set_read_timeout(None)
        .context("remove idle timeout from host Buildx dial-stdio stream")?;
    host.set_write_timeout(None)
        .context("remove write timeout from host Buildx dial-stdio stream")?;
    client
        .set_read_timeout(None)
        .context("remove idle timeout from Buildx dial-stdio stream")?;
    client
        .set_write_timeout(None)
        .context("remove write timeout from Buildx dial-stdio stream")?;
    Ok(())
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
    proxy_until_closed_with_lifetime(host, client, Some(PROXY_MAX_UPGRADE_LIFETIME))
}

#[cfg(unix)]
fn proxy_until_closed_without_timeout(
    host: std::os::unix::net::UnixStream,
    client: std::os::unix::net::UnixStream,
) -> Result<()> {
    proxy_until_closed_with_lifetime(host, client, None)
}

#[cfg(unix)]
fn proxy_until_closed_with_lifetime(
    host: std::os::unix::net::UnixStream,
    client: std::os::unix::net::UnixStream,
    max_lifetime: Option<Duration>,
) -> Result<()> {
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
    let host_abort = host
        .try_clone()
        .context("clone host Docker lease abort stream")?;
    let client_abort = client
        .try_clone()
        .context("clone job Docker lease abort stream")?;
    let lifetime = if let Some(max_lifetime) = max_lifetime {
        let lifetime_host = host
            .try_clone()
            .context("clone host Docker lease timer stream")?;
        let lifetime_client = client
            .try_clone()
            .context("clone job Docker lease timer stream")?;
        let (lifetime_cancel, lifetime_cancelled) = std::sync::mpsc::channel();
        let lifetime = std::thread::Builder::new()
            .name("velnor-docker-lease-lifetime".into())
            .spawn(
                move || match lifetime_cancelled.recv_timeout(max_lifetime) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => false,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let _ = lifetime_host.shutdown(std::net::Shutdown::Both);
                        let _ = lifetime_client.shutdown(std::net::Shutdown::Both);
                        true
                    }
                },
            )
            .context("start job Docker lease lifetime timer")?;
        Some((lifetime_cancel, lifetime))
    } else {
        None
    };
    let up = std::thread::Builder::new()
        .name("velnor-docker-lease-io".into())
        .spawn(move || {
            let result = io::copy(&mut host_read, &mut client_write)
                .map(|_| ())
                .context("copy Engine-to-guest Docker lease stream");
            // Engine finished: EOF the guest's read side, nothing else. The
            // guest closes at its leisure; the guest→host copy below then
            // returns on its own.
            if result.is_err() {
                let _ = host_abort.shutdown(std::net::Shutdown::Both);
                let _ = client_abort.shutdown(std::net::Shutdown::Both);
            } else {
                let _ = client_half.shutdown(std::net::Shutdown::Write);
            }
            result
        })
        .context("start job Docker lease copy thread")?;
    let client_to_engine = io::copy(&mut client_read, &mut host_write)
        .map(|_| ())
        .context("copy guest-to-Engine Docker lease stream");
    if client_to_engine.is_err() {
        let _ = host.shutdown(std::net::Shutdown::Both);
        let _ = client.shutdown(std::net::Shutdown::Both);
    } else {
        // Guest FIN: stdin-EOF for the Engine, NOT a teardown of the hijacked
        // output stream still flowing in the copy thread above.
        let _ = host.shutdown(std::net::Shutdown::Write);
    }
    let engine_to_client = match up.join() {
        Ok(result) => result,
        Err(error) => {
            let _ = host.shutdown(std::net::Shutdown::Both);
            let _ = client.shutdown(std::net::Shutdown::Both);
            Err(anyhow::anyhow!(
                "Engine-to-guest Docker lease copy thread panicked: {error:?}"
            ))
        }
    };
    let lifetime_result = if let Some((lifetime_cancel, lifetime)) = lifetime {
        let _ = lifetime_cancel.send(());
        match lifetime.join() {
            Ok(true) => Err(anyhow::anyhow!(
                "Docker lease stream exceeded its maximum lifetime"
            )),
            Ok(false) => Ok(()),
            Err(error) => Err(anyhow::anyhow!(
                "Docker lease lifetime timer thread panicked: {error:?}"
            )),
        }
    } else {
        Ok(())
    };
    let failures = [client_to_engine, engine_to_client, lifetime_result]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        bail!("Docker lease stream failed: {}", failures.join("; "));
    }
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
        false,
        false,
        |_, _| Ok(None),
    )
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct ProxyResponseDeadline {
    total: Option<Instant>,
    idle: Option<Duration>,
}

#[cfg(unix)]
impl ProxyResponseDeadline {
    fn bounded_until(total: Instant) -> Self {
        Self {
            total: Some(total),
            idle: Some(PROXY_IDLE_TIMEOUT),
        }
    }

    fn unbounded_stream() -> Self {
        Self {
            total: None,
            idle: None,
        }
    }
}

#[cfg(unix)]
fn docker_api_response_deadline(
    request: &[u8],
    bounded_total_deadline: Instant,
) -> Result<ProxyResponseDeadline> {
    let (method, target) = docker_request_line(request)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = canonical_docker_path(path)?;
    let segments = docker_api_path_segments(&path)?;
    let is_container_wait = method.eq_ignore_ascii_case("POST")
        && matches!(segments.as_slice(), ["containers", _, "wait"]);
    let follows_container_logs = method.eq_ignore_ascii_case("GET")
        && matches!(segments.as_slice(), ["containers", _, "logs"])
        && url::form_urlencoded::parse(query.as_bytes()).any(|(key, value)| {
            key == "follow" && matches!(value.to_ascii_lowercase().as_str(), "1" | "t" | "true")
        });
    if is_container_wait || follows_container_logs {
        Ok(ProxyResponseDeadline::unbounded_stream())
    } else {
        Ok(ProxyResponseDeadline::bounded_until(bounded_total_deadline))
    }
}

#[cfg(unix)]
fn forward_http_response_with_observer(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    capture_body: bool,
    allow_client_half_close: bool,
    observe: impl FnMut(u16, &[u8]) -> Result<Option<Vec<u8>>>,
) -> Result<bool> {
    forward_http_response_with_observer_deadline(
        host,
        host_buffer,
        client,
        request_method,
        capture_body,
        allow_client_half_close,
        ProxyResponseDeadline::bounded_until(Instant::now() + PROXY_UNFRAMED_RESPONSE_TIMEOUT),
        observe,
    )
}

#[cfg(unix)]
fn forward_http_response_with_observer_until(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    capture_body: bool,
    allow_client_half_close: bool,
    total_deadline: Instant,
    observe: impl FnMut(u16, &[u8]) -> Result<Option<Vec<u8>>>,
) -> Result<bool> {
    forward_http_response_with_observer_deadline(
        host,
        host_buffer,
        client,
        request_method,
        capture_body,
        allow_client_half_close,
        ProxyResponseDeadline::bounded_until(total_deadline),
        observe,
    )
}

#[cfg(unix)]
fn forward_http_response_with_observer_deadline(
    host: &mut std::os::unix::net::UnixStream,
    host_buffer: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    capture_body: bool,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
    mut observe: impl FnMut(u16, &[u8]) -> Result<Option<Vec<u8>>>,
) -> Result<bool> {
    loop {
        let head = read_http_response_head(
            host,
            host_buffer,
            client,
            request_method,
            capture_body,
            allow_client_half_close,
            response_deadline,
        )?;
        if head.no_body {
            if (100..200).contains(&head.status) && head.status != 101 {
                if !capture_body {
                    client
                        .write_all(&head.bytes)
                        .context("forward interim Docker API response headers through job lease")?;
                } else {
                    let _ = client.write_all(&head.bytes);
                }
                continue;
            }
            if capture_body && (200..300).contains(&head.status) {
                bail!("successful Docker create response has no framed ownership body");
            }
            if observe(head.status, &[])?.is_some() {
                bail!("Docker response transform cannot add a body to a bodyless response");
            }
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            return Ok(!head.close && head.status != 101);
        }
        if !capture_body {
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            if head.chunked {
                forward_chunked_response_captured(
                    host,
                    host_buffer,
                    client,
                    &mut None,
                    allow_client_half_close,
                    response_deadline,
                )?;
                if observe(head.status, &[])?.is_some() {
                    bail!("Docker response body transform requires bounded capture");
                }
                return Ok(!head.close && head.status != 101);
            }
            if let Some(content_length) = head.content_length {
                forward_exact_response_body_captured(
                    host,
                    host_buffer,
                    client,
                    content_length,
                    &mut None,
                    allow_client_half_close,
                    response_deadline,
                )?;
                if observe(head.status, &[])?.is_some() {
                    bail!("Docker response body transform requires bounded capture");
                }
                return Ok(!head.close && head.status != 101);
            }
            if !host_buffer.is_empty() {
                client
                    .write_all(host_buffer.as_slice())
                    .context("forward unframed Docker API response body")?;
                host_buffer.clear();
            }
            forward_unframed_response_with_deadline(
                host,
                client,
                allow_client_half_close,
                response_deadline,
            )?;
            if observe(head.status, &[])?.is_some() {
                bail!("Docker response body transform requires bounded capture");
            }
            return Ok(false);
        }
        let mut captured = capture_body.then(Vec::new);
        let mut captured_wire_body = capture_body.then(Vec::new);
        if head.chunked {
            if let (Some(decoded), Some(wire)) = (&mut captured, &mut captured_wire_body) {
                capture_chunked_response(
                    host,
                    host_buffer,
                    client,
                    decoded,
                    wire,
                    response_deadline,
                )?;
            } else {
                forward_chunked_response_captured(
                    host,
                    host_buffer,
                    client,
                    &mut captured,
                    allow_client_half_close,
                    response_deadline,
                )?;
            }
        } else if let Some(content_length) = head.content_length {
            if capture_body && content_length > MAX_CREATE_RESPONSE_BODY {
                bail!("Docker create response exceeds ownership capture limit");
            }
            if let (Some(decoded), Some(wire)) = (&mut captured, &mut captured_wire_body) {
                capture_exact_response_body(
                    host,
                    host_buffer,
                    client,
                    content_length,
                    decoded,
                    wire,
                    response_deadline,
                )?;
            } else {
                forward_exact_response_body_captured(
                    host,
                    host_buffer,
                    client,
                    content_length,
                    &mut captured,
                    allow_client_half_close,
                    response_deadline,
                )?;
            }
        } else {
            if capture_body {
                bail!("Docker create response has no bounded body framing");
            }
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            if !host_buffer.is_empty() {
                client
                    .write_all(host_buffer.as_slice())
                    .context("forward unframed Docker API response body")?;
                host_buffer.clear();
            }
            forward_unframed_response_with_deadline(
                host,
                client,
                allow_client_half_close,
                response_deadline,
            )?;
            observe(head.status, &[])?;
            return Ok(false);
        }
        if (100..200).contains(&head.status) && head.status != 101 {
            if !capture_body {
                client
                    .write_all(&head.bytes)
                    .context("forward interim Docker API response headers through job lease")?;
                if let Some(wire) = &captured_wire_body {
                    client
                        .write_all(wire)
                        .context("forward interim Docker API response body through job lease")?;
                }
            }
            continue;
        }
        let body = captured.as_deref().unwrap_or(&[]);
        if let Some(replacement) = observe(head.status, body)? {
            if !capture_body {
                bail!("Docker response body transform requires bounded capture");
            }
            let (rewritten_head, rewritten_body) =
                rewrite_http_response_body(&head.bytes, &replacement)?;
            client
                .write_all(&rewritten_head)
                .context("forward rewritten Docker API response headers through job lease")?;
            client
                .write_all(&rewritten_body)
                .context("forward rewritten Docker API response body through job lease")?;
        } else {
            client
                .write_all(&head.bytes)
                .context("forward Docker API response headers through job lease")?;
            if let Some(wire) = &captured_wire_body {
                client
                    .write_all(wire)
                    .context("forward captured Docker API response body through job lease")?;
            }
        }
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
fn rewrite_http_response_body(head: &[u8], body: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if body.len() > MAX_CREATE_RESPONSE_BODY {
        bail!("rewritten Docker create response exceeds ownership capture limit");
    }
    let header = std::str::from_utf8(head).context("Docker API response headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let status = lines.next().context("Docker API response status line")?;
    let mut rewritten = Vec::with_capacity(head.len() + 48);
    rewritten.extend_from_slice(status.as_bytes());
    rewritten.extend_from_slice(b"\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            bail!("malformed Docker API response header");
        };
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("trailer")
        {
            continue;
        }
        rewritten.extend_from_slice(line.as_bytes());
        rewritten.extend_from_slice(b"\r\n");
    }
    rewritten.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    Ok((rewritten, body.to_vec()))
}

#[cfg(unix)]
fn read_http_response_head(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    request_method: &str,
    capture_after_disconnect: bool,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<HttpResponseHead> {
    let mut scan_from: usize = 0;
    let header_end = loop {
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
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
        if capture_after_disconnect {
            wait_for_host_response_only_until(host, response_deadline)?;
        } else {
            wait_for_host_response_until(host, client, allow_client_half_close, response_deadline)?;
        }
        let mut scratch = [0_u8; PROXY_COPY_BUFFER];
        let read = read_proxy_bytes_until(
            host,
            &mut scratch,
            response_deadline.total,
            "total Docker API response deadline elapsed",
            "configure Docker API response header deadline",
            "read Docker API response headers",
        )?;
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
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    ensure_proxy_deadline(
        response_deadline.total,
        "total Docker API response deadline elapsed",
    )?;
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
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
        let read_len = remaining.min(scratch.len());
        wait_for_host_response_until(host, client, allow_client_half_close, response_deadline)?;
        let read = read_proxy_bytes_until(
            host,
            &mut scratch[..read_len],
            response_deadline.total,
            "total Docker API response deadline elapsed",
            "configure Docker API response body deadline",
            "read Docker API response body",
        )?;
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
fn capture_exact_response_body(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    _client: &mut std::os::unix::net::UnixStream,
    mut remaining: usize,
    decoded: &mut Vec<u8>,
    wire: &mut Vec<u8>,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    ensure_proxy_deadline(
        response_deadline.total,
        "total Docker API response deadline elapsed",
    )?;
    if remaining > MAX_CREATE_RESPONSE_BODY.saturating_sub(decoded.len())
        || remaining > MAX_CREATE_RESPONSE_WIRE_BODY.saturating_sub(wire.len())
    {
        bail!("Docker create response exceeds ownership capture limit");
    }
    if !buffered.is_empty() && remaining != 0 {
        let take = remaining.min(buffered.len());
        let bytes = &buffered.as_slice()[..take];
        decoded.extend_from_slice(bytes);
        capture_response_wire_bytes(wire, bytes)?;
        buffered.consume(take);
        remaining -= take;
    }
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    while remaining != 0 {
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
        let read_len = remaining.min(scratch.len());
        wait_for_host_response_only_until(host, response_deadline)?;
        let read = read_proxy_bytes_until(
            host,
            &mut scratch[..read_len],
            response_deadline.total,
            "total Docker API response deadline elapsed",
            "configure Docker create response body deadline",
            "read Docker create response for durable ownership",
        )?;
        if read == 0 {
            bail!("host Docker API closed before create response body finished");
        }
        decoded.extend_from_slice(&scratch[..read]);
        capture_response_wire_bytes(wire, &scratch[..read])?;
        remaining -= read;
    }
    Ok(())
}

#[cfg(unix)]
fn capture_chunked_response(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    decoded: &mut Vec<u8>,
    wire: &mut Vec<u8>,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    loop {
        let line = read_response_line_for_capture(host, buffered, client, response_deadline)?;
        capture_response_wire_bytes(wire, &line)?;
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
                let trailer =
                    read_response_line_for_capture(host, buffered, client, response_deadline)?;
                capture_response_wire_bytes(wire, &trailer)?;
                if trailer != b"\r\n" {
                    validate_chunked_response_trailer(&trailer)?;
                }
                if trailer == b"\r\n" {
                    return Ok(());
                }
            }
        }
        capture_exact_response_body(
            host,
            buffered,
            client,
            size,
            decoded,
            wire,
            response_deadline,
        )?;
        let terminator = read_response_line_for_capture(host, buffered, client, response_deadline)?;
        if terminator != b"\r\n" {
            bail!("Docker API response chunk is missing its terminating CRLF");
        }
        capture_response_wire_bytes(wire, &terminator)?;
    }
}

#[cfg(unix)]
fn read_response_line(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<Vec<u8>> {
    read_response_line_inner(
        host,
        buffered,
        client,
        false,
        allow_client_half_close,
        response_deadline,
    )
}

#[cfg(unix)]
fn read_response_line_for_capture(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    response_deadline: ProxyResponseDeadline,
) -> Result<Vec<u8>> {
    read_response_line_inner(host, buffered, client, true, false, response_deadline)
}

#[cfg(unix)]
fn read_response_line_inner(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    capture_after_disconnect: bool,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<Vec<u8>> {
    let mut scan_from: usize = 0;
    loop {
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
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
        if capture_after_disconnect {
            wait_for_host_response_only_until(host, response_deadline)?;
        } else {
            wait_for_host_response_until(host, client, allow_client_half_close, response_deadline)?;
        }
        let mut scratch = [0_u8; 8192];
        let read = read_proxy_bytes_until(
            host,
            &mut scratch,
            response_deadline.total,
            "total Docker API response deadline elapsed",
            "configure Docker API response framing deadline",
            "read Docker API response framing",
        )?;
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
    forward_chunked_response_captured(
        host,
        buffered,
        client,
        &mut None,
        false,
        ProxyResponseDeadline::bounded_until(Instant::now() + PROXY_UNFRAMED_RESPONSE_TIMEOUT),
    )
}

#[cfg(unix)]
fn forward_chunked_response_captured(
    host: &mut std::os::unix::net::UnixStream,
    buffered: &mut ResponseBuffer,
    client: &mut std::os::unix::net::UnixStream,
    captured: &mut Option<Vec<u8>>,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    loop {
        let line = read_response_line(
            host,
            buffered,
            client,
            allow_client_half_close,
            response_deadline,
        )?;
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
        forward_exact_response_body_captured(
            host,
            buffered,
            client,
            size,
            captured,
            allow_client_half_close,
            response_deadline,
        )?;
        if size == 0 {
            loop {
                let trailer = read_response_line(
                    host,
                    buffered,
                    client,
                    allow_client_half_close,
                    response_deadline,
                )?;
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
        let terminator = read_response_line(
            host,
            buffered,
            client,
            allow_client_half_close,
            response_deadline,
        )?;
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
    allow_client_half_close: bool,
) -> Result<()> {
    forward_unframed_response_with_deadline(
        host,
        client,
        allow_client_half_close,
        ProxyResponseDeadline::bounded_until(Instant::now() + PROXY_UNFRAMED_RESPONSE_TIMEOUT),
    )
}

#[cfg(unix)]
fn forward_unframed_response_until(
    host: &mut std::os::unix::net::UnixStream,
    client: &mut std::os::unix::net::UnixStream,
    allow_client_half_close: bool,
    total_deadline: Instant,
) -> Result<()> {
    forward_unframed_response_with_deadline(
        host,
        client,
        allow_client_half_close,
        ProxyResponseDeadline::bounded_until(total_deadline),
    )
}

#[cfg(unix)]
fn forward_unframed_response_with_deadline(
    host: &mut std::os::unix::net::UnixStream,
    client: &mut std::os::unix::net::UnixStream,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    let mut scratch = [0_u8; PROXY_COPY_BUFFER];
    loop {
        wait_for_host_response_until(host, client, allow_client_half_close, response_deadline)?;
        let read = read_proxy_bytes_until(
            host,
            &mut scratch,
            response_deadline.total,
            "total Docker API response deadline elapsed",
            "configure unframed Docker API response deadline",
            "read unframed Docker API response",
        )?;
        if read == 0 {
            return Ok(());
        }
        client
            .write_all(&scratch[..read])
            .context("forward unframed Docker API response")?;
    }
}

#[cfg(unix)]
fn wait_for_host_response_until(
    host: &std::os::unix::net::UnixStream,
    client: &std::os::unix::net::UnixStream,
    allow_client_half_close: bool,
    response_deadline: ProxyResponseDeadline,
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
        let timeout_ms = response_poll_timeout_milliseconds(response_deadline)?;
        let polled = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, timeout_ms) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error()).context("poll Docker lease response streams");
        }
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
        if polled == 0 {
            if response_deadline.idle.is_some() {
                bail!("timed out waiting for Docker API response");
            }
            bail!("total Docker API response deadline elapsed");
        }
        // A Docker client can half-close its request side as soon as it
        // sends a hijacked request. If the Engine response and that FIN are
        // both ready in one poll result, consume the response first; treating
        // the half-close as a disconnect would drop the valid 101 stream.
        if poll_fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
            != 0
        {
            return Ok(());
        }
        if poll_fds[1].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            if allow_client_half_close {
                // Buildx's Moby client half-closes its write side immediately
                // after sending a hijacked ExecAttach request. That does not
                // mean the guest has stopped reading the Engine response.
                // Drop this FD from poll after its EOF is observed; lease
                // cancellation still shuts down the watched host stream.
                poll_fds[1].fd = -1;
                continue;
            }
            let _ = host.shutdown(std::net::Shutdown::Both);
            return Err(GuestClosed.into());
        }
    }
}

#[cfg(unix)]
fn wait_for_host_response_only_until(
    host: &std::os::unix::net::UnixStream,
    response_deadline: ProxyResponseDeadline,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut host_fd = libc::pollfd {
        fd: host.as_raw_fd(),
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    loop {
        let timeout_ms = response_poll_timeout_milliseconds(response_deadline)?;
        let polled = unsafe { libc::poll(&mut host_fd, 1, timeout_ms) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error()).context("poll Docker lease response stream");
        }
        ensure_proxy_deadline(
            response_deadline.total,
            "total Docker API response deadline elapsed",
        )?;
        if polled == 0 {
            if response_deadline.idle.is_some() {
                bail!("timed out waiting for Docker API response");
            }
            bail!("total Docker API response deadline elapsed");
        }
        if host_fd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn response_poll_timeout_milliseconds(deadline: ProxyResponseDeadline) -> Result<i32> {
    ensure_proxy_deadline(deadline.total, "total Docker API response deadline elapsed")?;
    let idle_deadline = deadline.idle.map(|idle| Instant::now() + idle);
    let poll_deadline = match (deadline.total, idle_deadline) {
        (Some(total), Some(idle)) => Some(total.min(idle)),
        (Some(total), None) => Some(total),
        (None, Some(idle)) => Some(idle),
        (None, None) => None,
    };
    let Some(poll_deadline) = poll_deadline else {
        return Ok(-1);
    };
    let remaining = poll_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        ensure_proxy_deadline(deadline.total, "total Docker API response deadline elapsed")?;
        bail!("timed out waiting for Docker API response");
    }
    Ok(poll_timeout_milliseconds(remaining))
}

#[cfg(unix)]
fn poll_timeout_milliseconds(remaining: Duration) -> i32 {
    let milliseconds = remaining.as_nanos().div_ceil(1_000_000);
    i32::try_from(milliseconds).unwrap_or(i32::MAX)
}

#[cfg(unix)]
fn read_proxy_bytes_until(
    stream: &mut std::os::unix::net::UnixStream,
    bytes: &mut [u8],
    total_deadline: Option<Instant>,
    expired_message: &'static str,
    poll_context: &'static str,
    read_context: &'static str,
) -> Result<usize> {
    wait_for_proxy_readable_until(stream, total_deadline, expired_message, poll_context)?;
    // UnixStream has no per-read deadline. Poll first with any remaining
    // absolute budget; unbounded streaming responses poll until readiness.
    // This blocking read is safe because this stream has one reader and
    // readiness cannot be consumed by another proxy task.
    let read = stream.read(bytes).context(read_context)?;
    ensure_proxy_deadline(total_deadline, expired_message)?;
    Ok(read)
}

#[cfg(unix)]
fn wait_for_proxy_readable_until(
    stream: &std::os::unix::net::UnixStream,
    total_deadline: Option<Instant>,
    expired_message: &'static str,
    poll_context: &'static str,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut stream_fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    loop {
        ensure_proxy_deadline(total_deadline, expired_message)?;
        let timeout_ms = match total_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("{expired_message}");
                }
                poll_timeout_milliseconds(remaining)
            }
            None => -1,
        };
        stream_fd.revents = 0;
        let polled = unsafe { libc::poll(&mut stream_fd, 1, timeout_ms) };
        if polled < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error()).context(poll_context);
        }
        ensure_proxy_deadline(total_deadline, expired_message)?;
        if polled == 0 {
            bail!("{expired_message}");
        }
        if stream_fd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn ensure_proxy_deadline(
    total_deadline: Option<Instant>,
    expired_message: &'static str,
) -> Result<()> {
    if total_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        bail!("{expired_message}");
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
    read_http_request_with_budget_from_until(
        stream,
        prefix,
        set,
        Instant::now() + PROXY_IDLE_TIMEOUT,
    )
}

#[cfg(unix)]
fn read_http_request_with_budget_from_until(
    stream: &mut std::os::unix::net::UnixStream,
    prefix: Vec<u8>,
    set: Option<&Arc<LeaseConnSet>>,
    total_deadline: Instant,
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
        let read = read_proxy_bytes_until(
            stream,
            &mut chunk,
            Some(total_deadline),
            "total Docker API request deadline elapsed",
            "configure Docker API request header deadline",
            "read Docker API request header",
        )?;
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
        let request = read_chunked_http_request(
            stream,
            buf,
            header_end,
            &mut chunk,
            &mut budget,
            total_deadline,
        )?;
        return Ok(request);
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
            total_deadline,
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
    total_deadline: Instant,
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
            read_request_bytes(stream, &mut buf, scratch, budget, max_raw, total_deadline)?;
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
                    read_request_bytes(stream, &mut buf, scratch, budget, max_raw, total_deadline)?;
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
            read_request_bytes(stream, &mut buf, scratch, budget, max_raw, total_deadline)?;
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
    total_deadline: Instant,
) -> Result<()> {
    let read = read_proxy_bytes_until(
        stream,
        scratch,
        Some(total_deadline),
        "total Docker API request deadline elapsed",
        "configure Docker API request body deadline",
        "read Docker API request body",
    )?;
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
    fn job_network_guard_removal_args_are_exact() {
        let args = force_remove_network_args(&["velnor-net-guarded".to_string()]);
        assert_eq!(args, vec!["network", "rm", "velnor-net-guarded"]);
        let guard = JobNetworkGuard::arm("velnor-net-guarded");
        guard.defuse();
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

    fn add_isolated_network_mode(body: &[u8]) -> Vec<u8> {
        let mut value: Value = serde_json::from_slice(body).unwrap();
        let object = value.as_object_mut().unwrap();
        let host_config = object
            .entry("HostConfig")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        if !host_config
            .keys()
            .any(|key| key.eq_ignore_ascii_case("NetworkMode"))
        {
            host_config.insert("NetworkMode".into(), Value::String("none".into()));
        }
        serde_json::to_vec(&value).unwrap()
    }

    fn buildx_0361_create_value(state_volume: &str) -> Value {
        let mut value: Value = serde_json::from_str(
            r#"{
                "Hostname":"","Domainname":"","User":"",
                "AttachStdin":false,"AttachStdout":false,"AttachStderr":false,
                "Tty":false,"OpenStdin":false,"StdinOnce":false,"Env":null,
                "Cmd":["--config","/etc/buildkit/buildkitd.toml","--allow-insecure-entitlement=network.host"],
                "Image":"moby/buildkit:buildx-stable-1","Volumes":null,
                "WorkingDir":"","Entrypoint":null,"Labels":null,
                "HostConfig":{
                    "Binds":null,"ContainerIDFile":"","LogConfig":{"Type":"","Config":null},
                    "NetworkMode":"","PortBindings":null,
                    "RestartPolicy":{"Name":"unless-stopped","MaximumRetryCount":0},
                    "AutoRemove":false,"VolumeDriver":"","VolumesFrom":null,
                    "ConsoleSize":[0,0],"CapAdd":null,"CapDrop":null,"CgroupnsMode":"",
                    "Dns":null,"DnsOptions":null,"DnsSearch":null,"ExtraHosts":null,
                    "GroupAdd":null,"IpcMode":"","Cgroup":"","Links":null,
                    "OomScoreAdj":0,"PidMode":"","Privileged":true,
                    "PublishAllPorts":false,"ReadonlyRootfs":false,"SecurityOpt":null,
                    "UTSMode":"","UsernsMode":"","ShmSize":0,"Isolation":"",
                    "CpuShares":0,"Memory":0,"NanoCpus":0,"CgroupParent":"/docker/buildx",
                    "BlkioWeight":0,"BlkioWeightDevice":null,"BlkioDeviceReadBps":null,
                    "BlkioDeviceWriteBps":null,"BlkioDeviceReadIOps":null,
                    "BlkioDeviceWriteIOps":null,"CpuPeriod":0,"CpuQuota":0,
                    "CpuRealtimePeriod":0,"CpuRealtimeRuntime":0,"CpusetCpus":"",
                    "CpusetMems":"","Devices":null,"DeviceCgroupRules":null,
                    "DeviceRequests":null,"MemoryReservation":0,"MemorySwap":0,
                    "MemorySwappiness":null,"OomKillDisable":null,"PidsLimit":null,
                    "Ulimits":null,"CpuCount":0,"CpuPercent":0,"IOMaximumIOps":0,
                    "IOMaximumBandwidth":0,
                    "Mounts":[{"Type":"volume","Source":"__STATE_VOLUME__","Target":"/var/lib/buildkit"}],
                    "MaskedPaths":null,"ReadonlyPaths":null,"Init":true
                }
            }"#,
        )
        .unwrap();
        value["HostConfig"]["Mounts"][0]["Source"] = Value::String(state_volume.to_owned());
        value
    }

    fn test_buildkit_admission(builder: &str) -> PersistentBuildKitAdmission {
        PersistentBuildKitAdmission {
            builder: builder.to_owned(),
            state_volume: crate::buildkit::daemon_state_volume(builder),
            owner_token: "owner-token-for-test".to_owned(),
            container_id: None,
            network_reconciled: false,
            approved_config: Vec::new(),
            bootstrap_phase: crate::buildkit::BuilderBootstrapPhase::Unverified,
            readiness_attempts: 0,
            archive_in_flight: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn lease_policy_denies_foreign_resources_and_unsafe_routes() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let requests = [
            api_request("GET", "/v1.43/containers/foreign/json", b""),
            api_request("POST", "/v1.43/containers/foreign/kill", b""),
            api_request("DELETE", "/v1.43/containers/foreign", b""),
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

    #[test]
    fn lease_policy_denies_dockerd_builtin_buildkit_build_grpc_and_session() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        // The Docker-driver Buildx route reaches dockerd's shared BuildKit
        // service. It does not carry the admitted persistent builder or trust
        // group identity, so these build/session/tunnel endpoints must stay
        // unavailable to every ordinary job lease.
        for route in ["build", "grpc", "session"] {
            for method in ["POST"] {
                let request = api_request(method, &format!("/v1.43/{route}"), b"{}");
                let error = policy
                    .authorize(&request)
                    .expect_err("shared dockerd BuildKit route must be denied");
                let deny = error
                    .downcast_ref::<LeaseDeny>()
                    .expect("shared BuildKit route must return a Docker denial");
                assert_eq!(deny.status, 403);
                assert!(
                    deny.message.contains("shared dockerd BuildKit"),
                    "{route} returned unexpected denial: {}",
                    deny.message
                );
            }
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
        assert!(
            error.to_string().contains("shared dockerd BuildKit"),
            "{error:#}"
        );
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
        let create = api_request(
            "POST",
            "/v1.43/containers/create",
            br#"{"HostConfig":{"NetworkMode":"none"}}"#,
        );
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
    fn lease_policy_requires_exact_admission_for_persistent_buildkit_create() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = crate::buildkit::persistent_builder_name(
            "custom",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let other_builder = crate::buildkit::persistent_builder_name(
            "other",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let daemon = crate::buildkit::daemon_container_name(&builder);
        let state_volume = crate::buildkit::daemon_state_volume(&builder);
        let buildx_body = serde_json::to_vec(&buildx_0361_create_value(&state_volume)).unwrap();
        let request = |name: &str| {
            api_request(
                "POST",
                &format!("/v1.43/containers/create?name={name}"),
                &buildx_body,
            )
        };
        let ordinary = api_request(
            "POST",
            "/v1.43/containers/create?name=buildx_buildkit_external-builder0",
            br#"{"Image":"alpine","Cmd":["true"],"HostConfig":{"NetworkMode":"none"}}"#,
        );

        let denied = policy
            .authorize(&request(&daemon))
            .expect_err("raw persistent Buildx create must be denied before admission");
        assert!(denied
            .to_string()
            .contains("unadmitted persistent BuildKit"));
        assert_eq!(
            denied.downcast_ref::<LeaseDeny>().map(|deny| deny.status),
            Some(403)
        );

        policy.enable_test_buildkit_engine();
        let admission = test_buildkit_admission(&builder);
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(daemon.clone(), admission);
        let first_inspect = api_request("GET", &format!("/v1.43/containers/{daemon}/json"), b"");
        assert_eq!(
            policy.authorize(&first_inspect).unwrap(),
            AuthorizedDockerRoute::Owned(DockerResourceKind::Container),
            "fresh Buildx must receive Docker's expected 404 before ContainerCreate"
        );
        assert!(policy
            .initial_buildkit_bootstrap_inspect(&first_inspect)
            .unwrap()
            .is_some());
        assert_eq!(
            docker_request_line(
                &policy
                    .rewrite_owned_resource_references(&first_inspect)
                    .unwrap()
            )
            .unwrap()
            .1,
            format!("/v1.43/containers/{daemon}/json")
        );
        assert!(policy
            .validate_initial_buildkit_inspect_response(404)
            .is_ok());
        assert!(policy
            .validate_initial_buildkit_inspect_response(200)
            .is_err());
        assert_eq!(
            policy.authorize(&request(&daemon)).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
        );
        assert!(policy
            .authorize(&request(&crate::buildkit::daemon_container_name(
                &other_builder
            )))
            .is_err());
        // Velnor creates one node per bounded builder. Appending arbitrary
        // nodes would add unaccounted daemons and state volumes.
        let node_one = format!("buildx_buildkit_{builder}1");
        assert!(policy.authorize(&request(&node_one)).is_err());
        assert!(policy.authorize(&ordinary).is_ok());

        let encoded = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=%76{}", &daemon[1..]),
            b"{}",
        );
        assert!(policy.authorize(&encoded).is_err());
        let duplicate = api_request(
            "POST",
            &format!("/v1.43/containers/create?name=ordinary&name={daemon}"),
            b"{}",
        );
        assert!(policy.authorize(&duplicate).is_err());
    }

    #[test]
    fn persistent_buildkit_state_volume_is_not_a_generic_nested_mount() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = crate::buildkit::persistent_builder_name(
            "custom",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let daemon = crate::buildkit::daemon_container_name(&builder);
        let state_volume = crate::buildkit::daemon_state_volume(&builder);
        let admission = test_buildkit_admission(&builder);
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(daemon.clone(), admission.clone());
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_volumes
            .insert(state_volume.clone());

        let body = format!(
            r#"{{"Image":"busybox:1.36","HostConfig":{{"NetworkMode":"none","Mounts":[{{"Type":"volume","Source":"{state_volume}","Target":"/cache"}}]}}}}"#
        );
        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=job-nested",
                body.as_bytes(),
            ))
            .expect_err("a job container cannot mount another daemon's private state volume");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("nested state-volume mount must be a Docker denial");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("Mounts"));

        let exact_buildkit_body =
            serde_json::to_vec(&buildx_0361_create_value(&state_volume)).unwrap();
        let exact_buildkit = api_request(
            "POST",
            &format!("/v1.43/containers/create?name={daemon}"),
            &exact_buildkit_body,
        );
        assert_eq!(
            policy
                .authorize(&exact_buildkit)
                .expect("only the admitted daemon may mount its state volume"),
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
        );
    }

    #[test]
    fn persistent_buildkit_daemon_denies_exec_and_archive_writes() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy.enable_test_buildkit_engine();
        let builder = crate::buildkit::persistent_builder_name(
            "custom",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let daemon = crate::buildkit::daemon_container_name(&builder);
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(
                daemon.clone(),
                PersistentBuildKitAdmission {
                    state_volume: crate::buildkit::daemon_state_volume(&builder),
                    builder,
                    owner_token: "b".repeat(32),
                    container_id: Some("c".repeat(64)),
                    network_reconciled: false,
                    approved_config: Vec::new(),
                    bootstrap_phase: crate::buildkit::BuilderBootstrapPhase::Created,
                    readiness_attempts: 0,
                    archive_in_flight: false,
                },
            );

        for request in [
            api_request(
                "POST",
                &format!("/v1.43/containers/{daemon}/exec"),
                br#"{"AttachStdout":true,"Cmd":["sh"]}"#,
            ),
            api_request(
                "PUT",
                &format!("/v1.43/containers/{daemon}/archive?path=/var/lib/buildkit"),
                b"archive bytes",
            ),
        ] {
            let error = policy
                .authorize(&request)
                .expect_err("guests cannot execute in or write files into shared BuildKit");
            let deny = error
                .downcast_ref::<LeaseDeny>()
                .expect("shared daemon mutation must be a Docker denial");
            assert_eq!(deny.status, 403);
            assert!(deny.message.contains("persistent BuildKit"));
        }
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
        assert_eq!(
            policy
                .authorize(&api_request(
                    "GET",
                    "/v1.55/images/moby/buildkit:buildx-stable-1/json",
                    b""
                ))
                .unwrap(),
            AuthorizedDockerRoute::DaemonRead
        );
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
        let redirected = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"velnor-job-owned","EndpointConfig":{"nEtWoRkId":"bridge"}}"#,
        );
        let error = policy
            .authorize(&redirected)
            .expect_err("EndpointConfig.NetworkID must not override the owned path");
        assert!(format!("{error:#}").contains("NetworkID"));
        let duplicate = api_request(
            "POST",
            "/v1.43/networks/net-owned/connect",
            br#"{"Container":"velnor-job-owned","Container":"foreign"}"#,
        );
        assert!(policy.authorize(&duplicate).is_err());
    }

    #[test]
    fn lease_policy_keeps_persistent_buildkit_network_changes_runner_managed() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response(DockerResourceKind::Network, 201, br#"{"Id":"net-owned"}"#)
            .unwrap();
        let builder = "velnor-builder-v2-0123456789abcdef0123456789abcdef".to_owned();
        let daemon = crate::buildkit::daemon_container_name(&builder);
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(daemon.clone(), test_buildkit_admission(&builder));

        for operation in ["connect", "disconnect"] {
            let request = api_request(
                "POST",
                &format!("/v1.43/networks/net-owned/{operation}"),
                format!("{{\"Container\":\"{daemon}\"}}").as_bytes(),
            );
            let error = policy.authorize(&request).unwrap_err();
            assert!(error.to_string().contains("runner-managed"));
        }
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
    fn container_create_denies_shared_bridge_and_host_published_ports() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response(DockerResourceKind::Network, 201, br#"{"Id":"net-owned"}"#)
            .unwrap();
        for body in [
            br#"{"Image":"postgres:18-alpine"}"#.as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"NetworkMode":""}}"#.as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"NetworkMode":"default"}}"#.as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"AutoRemove":true,"NetworkMode":"bridge","PublishAllPorts":true}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"NetworkMode":"net-owned","PublishAllPorts":true}}"#
                .as_slice(),
            br#"{"Image":"postgres:18-alpine","HostConfig":{"PortBindings":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"0"}]},"NetworkMode":"net-owned"}}"#
                .as_slice(),
        ] {
            let request = api_request("POST", "/v1.43/containers/create?name=tc-pg", body);
            let result = policy.authorize(&request);
            assert!(
                result.is_err(),
                "shared bridge or host-published ports must be denied for body {}: {result:#?}",
                String::from_utf8_lossy(body)
            );
        }

        let private = api_request(
            "POST",
            "/v1.43/containers/create?name=tc-pg",
            br#"{"Image":"postgres:18-alpine","HostConfig":{"NetworkMode":"net-owned"}}"#,
        );
        assert!(
            policy.authorize(&private).is_ok(),
            "a nested service may join its lease-owned private network"
        );
    }

    #[test]
    fn container_create_empty_device_requests_are_absent() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        for value in ["null", "[]"] {
            let body = add_isolated_network_mode(
                format!(r#"{{"Image":"busybox:1.36","HostConfig":{{"DeviceRequests":{value}}}}}"#)
                    .as_bytes(),
            );
            let request = api_request("POST", "/v1.43/containers/create?name=job-container", &body);
            let result = policy.authorize(&request);
            assert!(result.is_ok(), "unexpected denial: {result:#?}");
        }

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"DeviceRequests":[{"Driver":"","Count":0,"DeviceIDs":[],"Capabilities":[],"Options":{}}]}}"#),
        );
        let error = policy
            .authorize(&request)
            .expect_err("nonempty DeviceRequests stays host control");
        let deny = error
            .downcast_ref::<LeaseDeny>()
            .expect("DeviceRequests denial must answer as LeaseDeny");
        assert_eq!(deny.status, 403);
        assert!(deny.message.contains("DeviceRequests"));
    }

    #[test]
    fn container_create_rejects_all_nonempty_device_requests() {
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
                "DeviceRequests",
            ),
        ] {
            let body = add_isolated_network_mode(format!(
                r#"{{"Image":"busybox:1.36","HostConfig":{{"DeviceRequests":[{}]}}}}"#,
                std::str::from_utf8(request).unwrap()
            ).as_bytes());
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                &body,
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
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"MaximumRetryCount":0,"Name":"no"}}}"#),
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"always"}}}"#,
            ),
        );
        let error = policy
            .authorize(&request)
            .expect_err("RestartPolicy always is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"on-failure","MaximumRetryCount":3}}}"#),
        );
        let error = policy
            .authorize(&request)
            .expect_err("RestartPolicy on-failure is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"Name":"no","MaximumRetryCount":0,"FutureField":0}}}"#),
        );
        let error = policy
            .authorize(&request)
            .expect_err("unknown RestartPolicy fields must fail closed");
        assert!(error.to_string().contains("FutureField"), "{error:#}");
    }

    #[test]
    fn container_create_accepts_only_admitted_buildx_without_gpu_access() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = crate::buildkit::persistent_builder_name(
            "custom",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let daemon = crate::buildkit::daemon_container_name(&builder);
        let state_volume = crate::buildkit::daemon_state_volume(&builder);
        policy.set_job_network("velnor-net-owned").unwrap();
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(daemon.clone(), test_buildkit_admission(&builder));
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_volumes
            .insert(state_volume.clone());

        let body = serde_json::to_vec(&buildx_0361_create_value(&state_volume)).unwrap();
        let request = api_request(
            "POST",
            &format!("/v1.43/containers/create?name={daemon}"),
            &body,
        );
        assert_eq!(
            policy.authorize(&request).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Container)
        );
        let rewritten = policy
            .rewrite_docker_api_request(&request, "velnor-job-owned", "daemon-owned")
            .unwrap();
        let rewritten_value = parse_create_value(docker_request_body(&rewritten).unwrap()).unwrap();
        assert_eq!(
            rewritten_value["HostConfig"]["NetworkMode"],
            "velnor-net-owned"
        );

        let mut gpu_request_value = buildx_0361_create_value(&state_volume);
        gpu_request_value["HostConfig"]["DeviceRequests"] = serde_json::json!([{
            "Driver":"",
            "Count":-1,
            "DeviceIDs":null,
            "Capabilities":[["gpu"]],
            "Options":{}
        }]);
        let gpu_request = api_request(
            "POST",
            &format!("/v1.43/containers/create?name={daemon}"),
            &serde_json::to_vec(&gpu_request_value).unwrap(),
        );
        let error = policy
            .authorize(&gpu_request)
            .expect_err("a privileged persistent daemon cannot receive host GPUs");
        assert!(error.to_string().contains("DeviceRequests"), "{error:#}");
    }

    #[test]
    fn persistent_buildkit_create_rejects_commands_ports_and_shared_networks() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let builder = crate::buildkit::persistent_builder_name(
            "custom",
            "trusted",
            crate::buildkit::TRUST_TIER_BRANCH,
            Some("owner/repo"),
        );
        let daemon = crate::buildkit::daemon_container_name(&builder);
        let state_volume = crate::buildkit::daemon_state_volume(&builder);
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_daemons
            .insert(daemon.clone(), test_buildkit_admission(&builder));
        policy
            .resources
            .lock()
            .unwrap()
            .admitted_buildkit_volumes
            .insert(state_volume.clone());

        let mut healthcheck = buildx_0361_create_value(&state_volume);
        healthcheck["Healthcheck"] = serde_json::json!({
            "Test":["CMD-SHELL","touch /tmp/escape"]
        });
        let mut command = buildx_0361_create_value(&state_volume);
        command["Cmd"] = serde_json::json!(["--addr=tcp://0.0.0.0:1234"]);
        let mut bridge = buildx_0361_create_value(&state_volume);
        bridge["HostConfig"]["NetworkMode"] = Value::String("bridge".into());
        let mut ports = buildx_0361_create_value(&state_volume);
        ports["HostConfig"]["PortBindings"] = serde_json::json!({
            "1234/tcp":[{"HostIp":"0.0.0.0","HostPort":"1234"}]
        });
        let mut publish_all = buildx_0361_create_value(&state_volume);
        publish_all["HostConfig"]["PublishAllPorts"] = Value::Bool(true);
        let mut entrypoint = buildx_0361_create_value(&state_volume);
        entrypoint["Entrypoint"] = serde_json::json!(["/bin/sh"]);
        let mut config_volumes = buildx_0361_create_value(&state_volume);
        config_volumes["Volumes"] = serde_json::json!({"/run": {}});
        let mut whitespace_user = buildx_0361_create_value(&state_volume);
        whitespace_user["User"] = Value::String(" ".into());
        let mut userns = buildx_0361_create_value(&state_volume);
        userns["HostConfig"]["UsernsMode"] = Value::String("host".into());
        let mut wsl_mount = buildx_0361_create_value(&state_volume);
        wsl_mount["HostConfig"]["Mounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "Type":"bind",
                "Source":"/usr/lib/wsl",
                "Target":"/usr/lib/wsl",
                "ReadOnly":true
            }));
        let mut digest_pinned_image = buildx_0361_create_value(&state_volume);
        digest_pinned_image["Image"] =
            Value::String(format!("moby/buildkit@sha256:{}", "a".repeat(64)));

        for hostile in [
            healthcheck,
            command,
            bridge,
            ports,
            publish_all,
            entrypoint,
            config_volumes,
            whitespace_user,
            userns,
            wsl_mount,
            digest_pinned_image,
        ] {
            let request = api_request(
                "POST",
                &format!("/v1.43/containers/create?name={daemon}"),
                &serde_json::to_vec(&hostile).unwrap(),
            );
            let error = policy
                .authorize(&request)
                .expect_err("persistent BuildKit must match the pinned safe Buildx shape");
            assert!(error.to_string().contains("BuildKit"), "{error:#}");
        }
    }

    #[test]
    fn container_create_privileged_stays_denied_except_buildkit() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=job-container",
                &add_isolated_network_mode(
                    br#"{"Image":"busybox:1.36","HostConfig":{"Privileged":true}}"#,
                ),
            ))
            .expect_err("privileged busybox is host control");
        assert!(error.to_string().contains("Privileged"), "{error:#}");

        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=job-container",
                &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"RestartPolicy":{"MaximumRetryCount":0,"Name":"unless-stopped"}}}"#),
            ))
            .expect_err("unless-stopped busybox is host control");
        assert!(error.to_string().contains("RestartPolicy"), "{error:#}");
    }

    #[test]
    fn namespaced_volume_alias_lifecycle_covers_mount_read_delete_recreate_and_isolation() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy.set_job_network("velnor-net-owned").unwrap();
        let job_id = "velnor-job-owned";
        let daemon_id = "daemon-owned";
        let create = api_request(
            "POST",
            "/v1.43/volumes/create",
            br#"{"Name":"cache","Driver":"local","Labels":{}}"#,
        );
        assert_eq!(
            policy.authorize(&create).unwrap(),
            AuthorizedDockerRoute::Create(DockerResourceKind::Volume)
        );
        let forwarded = policy
            .rewrite_docker_api_request(&create, job_id, daemon_id)
            .unwrap();
        let forwarded_value = parse_create_value(docker_request_body(&forwarded).unwrap()).unwrap();
        let physical = forwarded_value["Name"].as_str().unwrap().to_owned();
        assert_ne!(physical, "cache");
        let labels = forwarded_value["Labels"].clone();
        assert_eq!(labels[JOB_ID_LABEL], job_id);
        assert_eq!(labels[DAEMON_ID_LABEL], daemon_id);
        assert_eq!(labels[LEASE_ID_LABEL], policy.lease_id);

        let engine_response = serde_json::json!({
            "Name": physical.clone(),
            "Driver": "local",
            "Mountpoint": "/var/lib/docker/volumes/opaque/_data",
            "Labels": labels,
        });
        let response = serde_json::to_vec(&engine_response).unwrap();
        let mut foreign_response = engine_response.clone();
        foreign_response["Labels"][LEASE_ID_LABEL] = Value::String("other-lease".into());
        assert!(policy
            .record_create_response_with_owner(
                DockerResourceKind::Volume,
                201,
                &serde_json::to_vec(&foreign_response).unwrap(),
                Some("cache"),
                Some(job_id),
                Some(daemon_id),
            )
            .is_err());
        assert!(policy
            .resolve_owned_id(DockerResourceKind::Volume, "cache")
            .is_err());

        policy
            .record_create_response_with_owner(
                DockerResourceKind::Volume,
                201,
                &response,
                Some("cache"),
                Some(job_id),
                Some(daemon_id),
            )
            .unwrap();
        let guest_response = rewrite_volume_name_response(&response, &physical, "cache").unwrap();
        assert_eq!(
            parse_create_value(&guest_response).unwrap()["Name"],
            "cache"
        );

        let mount = api_request(
            "POST",
            "/v1.43/containers/create?name=nested",
            br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"velnor-net-owned","Mounts":[{"Source":"cache","Target":"/data","Type":"volume","ReadOnly":true,"Consistency":"consistent"}]}}"#,
        );
        assert!(policy.authorize(&mount).is_ok());
        let rewritten_mount = policy
            .rewrite_docker_api_request(&mount, job_id, daemon_id)
            .unwrap();
        let rewritten_mount =
            parse_create_value(docker_request_body(&rewritten_mount).unwrap()).unwrap();
        assert_eq!(
            rewritten_mount["HostConfig"]["Mounts"][0]["Source"],
            physical
        );

        for method in ["GET", "HEAD"] {
            let read = api_request(method, "/v1.43/volumes/cache", b"");
            assert!(policy.authorize(&read).is_ok());
            assert_eq!(
                docker_volume_read_alias(&read).unwrap().as_deref(),
                Some("cache")
            );
            let rewritten_read = policy
                .rewrite_docker_api_request(&read, job_id, daemon_id)
                .unwrap();
            assert!(String::from_utf8(rewritten_read)
                .unwrap()
                .contains(&format!("/volumes/{physical}")));
        }

        let delete = api_request("DELETE", "/v1.43/volumes/cache", b"");
        assert_eq!(
            policy.authorize(&delete).unwrap(),
            AuthorizedDockerRoute::Owned(DockerResourceKind::Volume)
        );
        let rewritten_delete = policy
            .rewrite_docker_api_request(&delete, job_id, daemon_id)
            .unwrap();
        assert!(String::from_utf8(rewritten_delete)
            .unwrap()
            .contains(&format!("/volumes/{physical}")));
        policy
            .retire_deleted_alias(DockerResourceKind::Volume, &physical)
            .unwrap();
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/volumes/cache", b""))
            .is_err());

        let recreated = policy
            .rewrite_docker_api_request(&create, job_id, daemon_id)
            .unwrap();
        assert_eq!(
            parse_create_value(docker_request_body(&recreated).unwrap()).unwrap()["Name"],
            physical
        );
        policy
            .record_create_response_with_owner(
                DockerResourceKind::Volume,
                201,
                &response,
                Some("cache"),
                Some(job_id),
                Some(daemon_id),
            )
            .unwrap();

        let other = DockerLeasePolicy::new("velnor-job-other").unwrap();
        other.set_job_network("velnor-net-other").unwrap();
        let other_create = other
            .rewrite_docker_api_request(&create, "velnor-job-other", "daemon-other")
            .unwrap();
        let other_physical = parse_create_value(docker_request_body(&other_create).unwrap())
            .unwrap()["Name"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(physical, other_physical);
        assert!(policy
            .resolve_owned_id(DockerResourceKind::Volume, &other_physical)
            .is_err());

        for (field, value) in [
            ("VolumeOptions", r#"{"NoCopy":false}"#),
            ("DriverConfig", r#"{"Name":"local"}"#),
            ("FutureField", "0"),
        ] {
            let body = format!(
                r#"{{"Image":"busybox:1.36","HostConfig":{{"NetworkMode":"velnor-net-owned","Mounts":[{{"Source":"cache","Target":"/data","Type":"volume","{field}":{value}}}]}}}}"#,
            );
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=nested",
                    body.as_bytes(),
                ))
                .expect_err("unsafe or unknown volume mount fields must fail closed");
            assert!(error.to_string().contains(field), "{error:#}");
        }

        let host_path = api_request(
            "POST",
            "/v1.43/containers/create?name=nested",
            br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"velnor-net-owned","Mounts":[{"Source":"/etc","Target":"/data","Type":"volume"}]}}"#,
        );
        let error = policy
            .authorize(&host_path)
            .expect_err("host-path volume source stays host control");
        assert!(error.to_string().contains("Mounts"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn volume_alias_create_mount_read_delete_recreate_cross_lease_through_proxy() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::path::PathBuf;
        use std::thread::JoinHandle;
        use std::time::Duration;

        fn start_request(
            policy: Arc<DockerLeasePolicy>,
            engine_path: PathBuf,
            request: Vec<u8>,
        ) -> (UnixStream, JoinHandle<Result<()>>) {
            let (mut guest, server) = UnixStream::pair().unwrap();
            guest
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let worker = std::thread::spawn(move || {
                handle_client_with(
                    server,
                    &engine_path,
                    "velnor-job-e2e",
                    "daemon-e2e",
                    LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                    policy,
                )
            });
            guest.write_all(&request).unwrap();
            (guest, worker)
        }

        fn finish_request(mut guest: UnixStream, worker: JoinHandle<Result<()>>) -> Vec<u8> {
            let mut response = Vec::new();
            guest.read_to_end(&mut response).unwrap();
            worker.join().unwrap().unwrap();
            response
        }

        fn respond(host: &mut UnixStream, status: &str, body: &[u8]) {
            write!(
                host,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            host.write_all(body).unwrap();
        }

        fn volume_response(name: &str, labels: &Value) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "Name": name,
                "Labels": labels,
                "Driver": "local",
                "Mountpoint": "/var/lib/docker/volumes/opaque/_data"
            }))
            .unwrap()
        }

        let root = unique_unix_dir("vlc-e2e");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-e2e").unwrap());
        policy.set_job_network("velnor-net-e2e").unwrap();

        let (guest, proxy) = start_request(
            Arc::clone(&policy),
            engine_path.clone(),
            api_request(
                "POST",
                "/v1.43/volumes/create",
                br#"{"Name":"cache","Driver":"local","Labels":{}}"#,
            ),
        );
        let (mut create_host, _) = engine.accept().unwrap();
        let create_request = read_http_request(&mut create_host).unwrap();
        let create_value =
            parse_create_value(docker_request_body(&create_request.bytes).unwrap()).unwrap();
        let physical = create_value["Name"].as_str().unwrap().to_owned();
        assert_ne!(physical, "cache");
        let labels = create_value["Labels"].clone();
        assert_eq!(labels[JOB_ID_LABEL], "velnor-job-e2e");
        assert_eq!(labels[DAEMON_ID_LABEL], "daemon-e2e");
        assert_eq!(labels[LEASE_ID_LABEL], policy.lease_id);
        let created_body = volume_response(&physical, &labels);
        respond(&mut create_host, "201 Created", &created_body);
        let response = finish_request(guest, proxy);
        assert_eq!(
            parse_create_value(docker_request_body(&response).unwrap()).unwrap()["Name"],
            "cache"
        );
        assert_eq!(
            policy
                .resolve_owned_id(DockerResourceKind::Volume, "cache")
                .unwrap(),
            physical
        );

        let (guest, proxy) = start_request(
            Arc::clone(&policy),
            engine_path.clone(),
            api_request(
                "POST",
                "/v1.43/containers/create?name=nested",
                br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"velnor-net-e2e","Mounts":[{"Source":"cache","Target":"/data","Type":"volume","ReadOnly":true}]}}"#,
            ),
        );
        let (mut inspect, _) = engine.accept().unwrap();
        let inspect_request = read_http_request(&mut inspect).unwrap();
        assert!(String::from_utf8_lossy(&inspect_request.bytes)
            .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
        respond(&mut inspect, "200 OK", &created_body);
        drop(inspect);
        let (mut mount_host, _) = engine.accept().unwrap();
        let mount_request = read_http_request(&mut mount_host).unwrap();
        let mount_value =
            parse_create_value(docker_request_body(&mount_request.bytes).unwrap()).unwrap();
        assert_eq!(mount_value["HostConfig"]["Mounts"][0]["Source"], physical);
        respond(
            &mut mount_host,
            "201 Created",
            br#"{"Id":"nested-container-id"}"#,
        );
        let _ = finish_request(guest, proxy);

        let (guest, proxy) = start_request(
            Arc::clone(&policy),
            engine_path.clone(),
            api_request("GET", "/v1.43/volumes/cache", b""),
        );
        let (mut inspect, _) = engine.accept().unwrap();
        let inspect_request = read_http_request(&mut inspect).unwrap();
        assert!(String::from_utf8_lossy(&inspect_request.bytes)
            .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
        respond(&mut inspect, "200 OK", &created_body);
        drop(inspect);
        let (mut read_host, _) = engine.accept().unwrap();
        let read_request = read_http_request(&mut read_host).unwrap();
        assert!(String::from_utf8_lossy(&read_request.bytes)
            .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
        respond(&mut read_host, "200 OK", &created_body);
        let response = finish_request(guest, proxy);
        assert_eq!(
            parse_create_value(docker_request_body(&response).unwrap()).unwrap()["Name"],
            "cache"
        );

        let (guest, proxy) = start_request(
            Arc::clone(&policy),
            engine_path.clone(),
            api_request("DELETE", "/v1.43/volumes/cache", b""),
        );
        let (mut inspect, _) = engine.accept().unwrap();
        let inspect_request = read_http_request(&mut inspect).unwrap();
        assert!(String::from_utf8_lossy(&inspect_request.bytes)
            .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
        respond(&mut inspect, "200 OK", &created_body);
        drop(inspect);
        let (mut delete_host, _) = engine.accept().unwrap();
        let delete_request = read_http_request(&mut delete_host).unwrap();
        assert!(String::from_utf8_lossy(&delete_request.bytes)
            .contains(&format!("DELETE /v1.43/volumes/{physical} HTTP/1.1")));
        respond(&mut delete_host, "204 No Content", b"");
        let _ = finish_request(guest, proxy);
        assert!(policy
            .resolve_owned_id(DockerResourceKind::Volume, "cache")
            .is_err());

        let (guest, proxy) = start_request(
            Arc::clone(&policy),
            engine_path.clone(),
            api_request(
                "POST",
                "/v1.43/volumes/create",
                br#"{"Name":"cache","Driver":"local","Labels":{}}"#,
            ),
        );
        let (mut recreate_host, _) = engine.accept().unwrap();
        let recreate_request = read_http_request(&mut recreate_host).unwrap();
        let recreate_value =
            parse_create_value(docker_request_body(&recreate_request.bytes).unwrap()).unwrap();
        assert_eq!(recreate_value["Name"], physical);
        assert_eq!(recreate_value["Labels"], labels);
        respond(&mut recreate_host, "201 Created", &created_body);
        let response = finish_request(guest, proxy);
        assert_eq!(
            parse_create_value(docker_request_body(&response).unwrap()).unwrap()["Name"],
            "cache"
        );

        let other = DockerLeasePolicy::new("velnor-job-other").unwrap();
        assert!(other
            .resolve_owned_id(DockerResourceKind::Volume, "cache")
            .is_err());
        assert!(other
            .resolve_owned_id(DockerResourceKind::Volume, &physical)
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn literal_create_resource_aliases_are_pinned_for_container_network_and_volume_routes() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.containers.insert("container-id".into());
            resources
                .container_names
                .insert("create".into(), "container-id".into());
            resources.networks.insert("network-id".into());
            resources
                .network_names
                .insert("create".into(), "network-id".into());
            resources.volumes.insert("volume-id".into());
            resources
                .volume_names
                .insert("create".into(), "volume-id".into());
        }

        for (method, route, kind, expected) in [
            (
                "GET",
                "/v1.43/containers/create/json",
                DockerResourceKind::Container,
                "/containers/container-id/json",
            ),
            (
                "GET",
                "/v1.43/networks/create",
                DockerResourceKind::Network,
                "/networks/network-id",
            ),
            (
                "GET",
                "/v1.43/volumes/create",
                DockerResourceKind::Volume,
                "/volumes/volume-id",
            ),
        ] {
            let request = api_request(method, route, b"");
            assert_eq!(
                docker_resource_reference(&request).unwrap(),
                Some((kind, "create".into()))
            );
            let rewritten = policy
                .rewrite_docker_api_request(&request, "velnor-job-owned", "daemon-owned")
                .unwrap();
            assert!(
                String::from_utf8(rewritten).unwrap().contains(expected),
                "route {route} was not pinned to its immutable resource ID"
            );
        }

        let create_endpoint = api_request("POST", "/v1.43/containers/create?name=new", b"{}");
        assert_eq!(docker_resource_reference(&create_endpoint).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn delete_of_literal_create_alias_never_targets_foreign_create_name() {
        use std::io::Write;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        let root = unique_unix_dir("vlc-alias");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let engine_thread = std::thread::spawn(move || {
            let (mut host, _) = engine.accept().unwrap();
            let request = read_http_request(&mut host).unwrap();
            let request = String::from_utf8_lossy(&request.bytes);
            if request.contains("DELETE /v1.43/containers/create HTTP/1.1") {
                // A foreign object occupies the mutable alias. It would be
                // deleted if the proxy forwarded the name rather than its ID.
                host.write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            } else {
                assert!(
                    request.contains("DELETE /v1.43/containers/pinned-id HTTP/1.1"),
                    "delete did not pin alias to the immutable ID: {request}"
                );
                host.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            }
        });

        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.containers.insert("pinned-id".into());
            resources
                .container_names
                .insert("create".into(), "pinned-id".into());
        }
        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });

        guest
            .write_all(&api_request("DELETE", "/v1.43/containers/create", b""))
            .unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        proxy.join().unwrap().unwrap();
        engine_thread.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 404 Not Found"));
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/containers/create/json", b""))
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn network_connect_pins_create_alias_and_container_name() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.networks.insert("network-id".into());
            resources
                .network_names
                .insert("create".into(), "network-id".into());
            resources.containers.insert("container-id".into());
            resources
                .container_names
                .insert("nested".into(), "container-id".into());
        }

        let request = api_request(
            "POST",
            "/v1.43/networks/create/connect",
            br#"{"Container":"nested"}"#,
        );
        assert_eq!(
            policy.authorize(&request).unwrap(),
            AuthorizedDockerRoute::Owned(DockerResourceKind::Network)
        );
        let rewritten = policy
            .rewrite_docker_api_request(&request, "velnor-job-owned", "daemon-owned")
            .unwrap();
        let text = String::from_utf8(rewritten.clone()).unwrap();
        assert!(text.contains("/networks/network-id/connect"));
        assert_eq!(
            parse_create_value(docker_request_body(&rewritten).unwrap()).unwrap()["Container"],
            "container-id"
        );
    }

    #[cfg(unix)]
    #[test]
    fn delete_of_literal_volume_create_alias_checks_labels_then_targets_private_name() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        let root = unique_unix_dir("vlc-volume-alias");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        let job_id = "velnor-job-owned";
        let daemon_id = "daemon-owned";
        let physical = policy.private_volume_name("create").unwrap();
        let mut labels = Map::new();
        labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.into()));
        labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.into()));
        labels.insert(
            LEASE_ID_LABEL.into(),
            Value::String(policy.lease_id.clone()),
        );
        let inspect_body = serde_json::to_vec(&serde_json::json!({
            "Name": physical.clone(),
            "Labels": labels,
            "Driver": "local",
            "Mountpoint": "/var/lib/docker/volumes/opaque/_data"
        }))
        .unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.volumes.insert(physical.clone());
            resources
                .volume_names
                .insert("create".into(), physical.clone());
        }

        let engine_thread = std::thread::spawn(move || {
            let (mut inspect, _) = engine.accept().unwrap();
            let inspect_request = read_http_request(&mut inspect).unwrap();
            assert!(String::from_utf8_lossy(&inspect_request.bytes)
                .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
            write!(
                inspect,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                inspect_body.len()
            )
            .unwrap();
            inspect.write_all(&inspect_body).unwrap();
            drop(inspect);

            let (mut delete, _) = engine.accept().unwrap();
            let delete_request = read_http_request(&mut delete).unwrap();
            let delete_request = String::from_utf8_lossy(&delete_request.bytes);
            if delete_request.contains("DELETE /v1.43/volumes/create HTTP/1.1") {
                // A foreign host volume owns this name. This fake Engine
                // reports success if the mutable name reaches it.
                delete
                    .write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            } else {
                assert!(
                    delete_request.contains(&format!("DELETE /v1.43/volumes/{physical} HTTP/1.1"))
                );
                delete
                    .write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            }
        });

        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                job_id,
                daemon_id,
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });
        guest
            .write_all(&api_request("DELETE", "/v1.43/volumes/create", b""))
            .unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        proxy.join().unwrap().unwrap();
        engine_thread.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 204 No Content"));
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/volumes/create", b""))
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn head_volume_inspect_retires_alias_when_engine_reports_gone() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        let root = unique_unix_dir("vlc-volume-gone");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        let physical = policy.private_volume_name("stale").unwrap();
        {
            let mut resources = policy.resources.lock().unwrap();
            resources.volumes.insert(physical.clone());
            resources
                .volume_names
                .insert("stale".into(), physical.clone());
        }
        let engine_thread = std::thread::spawn(move || {
            let (mut host, _) = engine.accept().unwrap();
            let inspect = read_http_request(&mut host).unwrap();
            assert!(String::from_utf8_lossy(&inspect.bytes)
                .contains(&format!("GET /v1.43/volumes/{physical} HTTP/1.1")));
            host.write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });

        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });
        guest
            .write_all(&api_request("HEAD", "/v1.43/volumes/stale", b""))
            .unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        let error = proxy.join().unwrap().unwrap_err();
        assert_eq!(
            error.downcast_ref::<LeaseDeny>().map(|deny| deny.status),
            Some(404)
        );
        engine_thread.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 404 Not Found"));
        assert!(policy
            .authorize(&api_request("GET", "/v1.43/volumes/stale", b""))
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_container_and_exec_aliases_retire_on_hijack_404_and_410() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        for (resource_kind, route, resource_id, status, lookup) in [
            (
                DockerResourceKind::Container,
                "/v1.43/containers/nested/attach?stream=1",
                "container-id",
                404,
                "/v1.43/containers/nested/json",
            ),
            (
                DockerResourceKind::Exec,
                "/v1.43/exec/exec-id/start",
                "exec-id",
                410,
                "/v1.43/exec/exec-id/json",
            ),
        ] {
            let root = unique_unix_dir("vlc-hijack-gone");
            let engine_path = root.join("engine.sock");
            let engine = UnixListener::bind(&engine_path).unwrap();
            let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
            {
                let mut resources = policy.resources.lock().unwrap();
                match resource_kind {
                    DockerResourceKind::Container => {
                        resources.containers.insert(resource_id.into());
                        resources
                            .container_names
                            .insert("nested".into(), resource_id.into());
                    }
                    DockerResourceKind::Exec => {
                        resources.execs.insert(resource_id.into());
                    }
                    _ => unreachable!(),
                }
            }
            let engine_thread = std::thread::spawn(move || {
                let (mut host, _) = engine.accept().unwrap();
                let request = read_http_request(&mut host).unwrap();
                let request = String::from_utf8_lossy(&request.bytes);
                if resource_kind == DockerResourceKind::Container {
                    assert!(request.contains("/containers/container-id/attach"));
                } else {
                    assert!(request.contains("/exec/exec-id/start"));
                }
                let response = format!(
                    "HTTP/1.1 {status} Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                host.write_all(response.as_bytes()).unwrap();
            });

            let (mut guest, server) = UnixStream::pair().unwrap();
            guest
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let proxy_policy = Arc::clone(&policy);
            let proxy_engine = engine_path.clone();
            let proxy = std::thread::spawn(move || {
                handle_client_with(
                    server,
                    &proxy_engine,
                    "velnor-job-owned",
                    "daemon-owned",
                    LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                    proxy_policy,
                )
            });
            let request = format!(
                "POST {route} HTTP/1.1\r\nHost: docker\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Length: 0\r\n\r\n"
            );
            guest.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            guest.read_to_end(&mut response).unwrap();
            proxy.join().unwrap().unwrap();
            engine_thread.join().unwrap();
            assert!(response.starts_with(format!("HTTP/1.1 {status}").as_bytes()));
            assert!(policy.authorize(&api_request("GET", lookup, b"")).is_err());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn container_create_unknown_numeric_zero_is_empty_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"IOMaximumBandwidth":0,"FutureHostControl":0}}"#),
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
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"FutureHostControl":[{}]}}"#,
            ),
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
            let body = add_isolated_network_mode(
                format!(r#"{{"Image":"busybox:1.36","HostConfig":{{"{field}":{value}}}}}"#)
                    .as_bytes(),
            );
            let error = policy
                .authorize(&api_request(
                    "POST",
                    "/v1.43/containers/create?name=job-container",
                    &body,
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
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"FutureHostControl":1}}"#,
            ),
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

    #[cfg(unix)]
    #[test]
    fn create_quota_denies_before_opening_the_engine_socket() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};

        let root = unique_unix_dir("vlc");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        engine.set_nonblocking(true).unwrap();
        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        {
            let mut resources = policy.resources.lock().unwrap();
            for index in 0..MAX_OWNED_DOCKER_RESOURCES - 1 {
                resources.containers.insert(format!("owned-{index}"));
            }
            assert_eq!(owned_resource_count(&resources), MAX_OWNED_DOCKER_RESOURCES);
        }

        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            handle_client_with(
                server,
                &engine_path,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                policy,
            )
        });
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=nested",
            br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"none"}}"#,
        );
        guest.write_all(&request).unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("ownership registry is full"));
        assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
        assert!(String::from_utf8_lossy(&response).contains("ownership registry is full"));
        assert!(matches!(
            engine.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_reservations_serialize_capacity_and_release_definitive_failures() {
        use std::sync::Barrier;

        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        {
            let mut resources = policy.resources.lock().unwrap();
            for index in 0..MAX_OWNED_DOCKER_RESOURCES - 3 {
                resources.containers.insert(format!("owned-{index}"));
            }
            assert_eq!(
                owned_resource_count(&resources),
                MAX_OWNED_DOCKER_RESOURCES - 2
            );
        }

        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let handles = (0..workers)
            .map(|_| {
                let policy = Arc::clone(&policy);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    policy.reserve_create()
                })
            })
            .collect::<Vec<_>>();
        let mut reservations = Vec::new();
        let mut denied = 0;
        for handle in handles {
            match handle.join().unwrap() {
                Ok(reservation) => reservations.push(reservation),
                Err(_) => denied += 1,
            }
        }
        assert_eq!(reservations.len(), 2);
        assert_eq!(denied, workers - 2);
        assert_eq!(
            policy.resources.lock().unwrap().pending_create_reservations,
            2
        );

        // A definitive Engine failure is completed by the response observer;
        // that releases exactly its slot while the other request stays held.
        let mut reservations = reservations.into_iter();
        let mut first = reservations.next().unwrap();
        first.finish().unwrap();
        assert_eq!(
            policy.resources.lock().unwrap().pending_create_reservations,
            1
        );
        drop(reservations.next().unwrap());
        assert_eq!(
            policy.resources.lock().unwrap().pending_create_reservations,
            0
        );

        let dropped_before_forward = policy.reserve_create().unwrap();
        drop(dropped_before_forward);
        assert_eq!(
            policy.resources.lock().unwrap().pending_create_reservations,
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn definitive_engine_create_failure_releases_its_reservation() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        let root = unique_unix_dir("vlc-create-fail");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let engine_thread = std::thread::spawn(move || {
            let (mut host, _) = engine.accept().unwrap();
            let request = read_http_request(&mut host).unwrap();
            assert!(String::from_utf8_lossy(&request.bytes).contains("/containers/create"));
            let body = br#"{"message":"failed"}"#;
            write!(
                host,
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            host.write_all(body).unwrap();
        });

        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });
        guest
            .write_all(&api_request(
                "POST",
                "/v1.43/containers/create?name=nested",
                br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"none"}}"#,
            ))
            .unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        proxy.join().unwrap().unwrap();
        engine_thread.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 500 Internal Server Error"));
        let resources = policy.resources.lock().unwrap();
        assert_eq!(resources.pending_create_reservations, 0);
        assert_eq!(resources.containers.len(), 1);
        assert!(resources.containers.contains("velnor-job-owned"));
        drop(resources);
        assert!(policy.reserve_create().is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ambiguous_engine_create_response_quarantines_its_quota_slot() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};

        let root = unique_unix_dir("vlc-ambiguous");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let engine_thread = std::thread::spawn(move || {
            let (mut host, _) = engine.accept().unwrap();
            let request = read_http_request(&mut host).unwrap();
            assert!(String::from_utf8_lossy(&request.bytes).contains("/containers/create"));
            // The Engine confirms create but the response is too large to
            // capture safely. The physical object may exist, so the proxy
            // must retain the reservation instead of opening an unbounded
            // create loop with lost IDs.
            write!(
                host,
                "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_CREATE_RESPONSE_BODY + 1
            )
            .unwrap();
        });

        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        let (mut guest, server) = UnixStream::pair().unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });

        guest
            .write_all(&api_request(
                "POST",
                "/v1.43/containers/create?name=nested",
                br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"none"}}"#,
            ))
            .unwrap();
        let error = proxy.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("capture limit"), "{error:#}");
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        engine_thread.join().unwrap();
        let resources = policy.resources.lock().unwrap();
        assert_eq!(resources.pending_create_reservations, 1);
        assert_eq!(resources.containers.len(), 1);
        assert!(resources.containers.contains("velnor-job-owned"));
        drop(resources);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn created_resource_is_registered_before_response_is_forwarded_to_guest() {
        use std::io::Write;
        use std::os::unix::net::{UnixListener, UnixStream};

        let root = unique_unix_dir("vlc-before-forward");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        let body = br#"{"Id":"created-before-forward"}"#;
        let engine_thread = std::thread::spawn(move || {
            let (mut host, _) = engine.accept().unwrap();
            let request = read_http_request(&mut host).unwrap();
            assert!(String::from_utf8_lossy(&request.bytes).contains("/containers/create"));
            write!(
                host,
                "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            host.write_all(body).unwrap();
        });

        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());
        let (mut guest, server) = UnixStream::pair().unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });
        guest
            .write_all(&api_request(
                "POST",
                "/v1.43/containers/create?name=nested",
                br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"none"}}"#,
            ))
            .unwrap();
        guest.shutdown(std::net::Shutdown::Read).unwrap();
        let _ = proxy.join().unwrap();
        engine_thread.join().unwrap();
        let resources = policy.resources.lock().unwrap();
        assert!(resources.containers.contains("created-before-forward"));
        assert_eq!(
            resources.container_names.get("nested").map(String::as_str),
            Some("created-before-forward")
        );
        assert_eq!(resources.pending_create_reservations, 0);
        drop(resources);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rewritten_label_growth_is_denied_before_engine_connect() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Duration;

        let root = unique_unix_dir("vlc-label");
        let engine_path = root.join("engine.sock");
        let engine = UnixListener::bind(&engine_path).unwrap();
        engine.set_nonblocking(true).unwrap();
        let policy = Arc::new(DockerLeasePolicy::new("velnor-job-owned").unwrap());

        let body_with_padding = |padding: usize| {
            serde_json::to_vec(&serde_json::json!({
                "Image": "busybox:1.36",
                "Labels": {"padding": "x".repeat(padding)},
                "HostConfig": {"NetworkMode": "none"}
            }))
            .unwrap()
        };
        let mut low = 0usize;
        let mut high = MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS;
        while low < high {
            let middle = (low + high).div_ceil(2);
            if body_with_padding(middle).len() <= MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let body = body_with_padding(low);
        assert_eq!(body.len(), MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS);
        let request = api_request("POST", "/v1.43/containers/create?name=nested", &body);
        assert!(policy.authorize(&request).is_ok());
        let over_limit = body_with_padding(low + 1);
        assert_eq!(
            over_limit.len(),
            MAX_CREATE_REQUEST_BODY_WITH_REPEATED_LABELS + 1
        );
        let error = policy
            .authorize(&api_request(
                "POST",
                "/v1.43/containers/create?name=nested",
                &over_limit,
            ))
            .expect_err("raw body one byte over the cap must be rejected");
        assert!(
            error.to_string().contains("create body exceeds"),
            "{error:#}"
        );

        let (mut guest, server) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy_policy = Arc::clone(&policy);
        let proxy_engine = engine_path.clone();
        let proxy = std::thread::spawn(move || {
            handle_client_with(
                server,
                &proxy_engine,
                "velnor-job-owned",
                "daemon-owned",
                LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
                proxy_policy,
            )
        });
        guest.write_all(&request).unwrap();
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        let error = proxy.join().unwrap().unwrap_err();
        assert!(
            error.to_string().contains("ownership-label limit"),
            "{error:#}"
        );
        assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
        assert!(matches!(
            engine.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn container_create_allows_only_zero_console_size_as_known_default() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"ConsoleSize":[0,0]}}"#,
            ),
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"ConsoleSize":[80,24]}}"#,
            ),
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
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"BlkioWeight":0}}"#,
            ),
        );
        let result = policy.authorize(&request);
        assert!(result.is_ok(), "unexpected denial: {result:#?}");

        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","HostConfig":{"BlkioWeight":100}}"#,
            ),
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
            &add_isolated_network_mode(br#"{"Image":"busybox:1.36","HostConfig":{"BlkioDeviceReadBps":[{"Path":"/dev/sda","Rate":1024}]}}"#),
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
                "docker rm batched {} ids during stale job cleanup: {call:?}",
                ids.len()
            );
        }
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
    fn container_create_rejects_endpoint_network_id_override() {
        let policy = DockerLeasePolicy::new("velnor-job-owned").unwrap();
        policy
            .record_create_response_with_alias(
                DockerResourceKind::Network,
                201,
                br#"{"Id":"net-private-id"}"#,
                Some("velnor-net-private"),
            )
            .unwrap();
        let request = api_request(
            "POST",
            "/v1.43/containers/create?name=job-container",
            &add_isolated_network_mode(
                br#"{"Image":"busybox:1.36","NetworkingConfig":{"EndpointsConfig":{"velnor-net-private":{"nEtWoRkId":"bridge"}}}}"#,
            ),
        );
        let error = policy
            .authorize(&request)
            .expect_err("an owned endpoint key cannot carry a second network identity");
        assert!(format!("{error:#}").contains("NetworkID"));

        let error = rewrite_docker_api_request_with_context(
            &request,
            "job-a",
            "daemon-a",
            &BTreeSet::new(),
            &["velnor-net-private".to_string()].into_iter().collect(),
            None,
            None,
            None,
            None,
        )
        .expect_err("request rewrite must enforce the same endpoint identity rule");
        assert!(format!("{error:#}").contains("NetworkID"));
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
        let body = add_isolated_network_mode(
            br#"{
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
        }"#,
        );
        let request = format!(
            "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(&body).unwrap()
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
            let body = add_isolated_network_mode(body);
            let request = format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(&body).unwrap()
            );
            let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("nested Docker host control access must fail closed");
            assert!(error.to_string().contains("host control access"));
        }
    }

    #[test]
    fn rewrite_rejects_testcontainers_host_published_ports() {
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
            let error = rewrite_docker_api_request(request.as_bytes(), "job-a", "daemon-a")
                .expect_err("nested containers cannot use the shared bridge or host ports");
            assert!(error.to_string().contains("NetworkMode") || error.to_string().contains("host control"));
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
            let body = add_isolated_network_mode(body);
            let request = format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(&body).unwrap()
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
        let body = add_isolated_network_mode(br#"{"Image":"redis:5.0"}"#);
        let request = format!(
            "POST /v1.43/containers/create?name=goofy HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(&body).unwrap()
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
        let body = add_isolated_network_mode(br#"{"Image":"alpine:3.20"}"#);
        let request = format!(
            "POST /v1.43/%63ontainers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(&body).unwrap()
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
    fn force_remove_job_owned_containers_skips_reserved_buildkit_daemons() {
        let job_id = "velnor-job-orphan";
        let listing = format!(
            "job-id\t{job_id}\t{job_id}\trunning\n\
             guest-id\tguest-container\t{job_id}\texited\n\
             bk-id\t{BUILDKIT_CONTAINER_NAME_PREFIX}deadbeef\t{job_id}\trunning\n"
        );
        let mut calls = Vec::new();
        let mut outputs = vec![listing, String::new()];
        force_remove_job_owned_containers(job_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_owned_containers_state_args(job_id));
        assert_eq!(
            calls,
            vec![
                list_owned_containers_state_args(job_id),
                force_remove_container_args(&["guest-id".into(), "job-id".into()]),
            ]
        );
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
        assert_eq!(
            calls,
            vec![
                list_owned_job_format_args(),
                list_owned_containers_state_args("velnor-job-dead"),
                list_owned_containers_state_args("velnor-job-dead"),
                remove_one_container_args("guest-old"),
                list_owned_networks_args("velnor-job-dead"),
                list_owned_volumes_args("velnor-job-dead"),
            ]
        );
        assert!(!calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "rm") && call.iter().any(|arg| arg == "--force")
        }));
        assert!(calls.iter().all(|call| !call
            .iter()
            .any(|arg| { arg.contains(BUILDKIT_CONTAINER_NAME_PREFIX) || arg == "buildx" })));
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
        ];
        reclaim_daemon_orphan_jobs(daemon, |args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![
                list_daemon_owned_job_format_args(),
                list_owned_containers_state_args("velnor-job-dead"),
                list_owned_containers_state_args("velnor-job-dead"),
                remove_one_container_args("guest-old"),
                list_owned_networks_args("velnor-job-dead"),
                list_owned_volumes_args("velnor-job-dead"),
            ]
        );
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
    fn head_volume_inspect_forwards_bodyless_success_without_create_capture() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 512\r\nConnection: close\r\n\r\n";
        source.write_all(head).unwrap();
        drop(source);
        let (mut client, mut sink) = UnixStream::pair().unwrap();
        let mut buffered = ResponseBuffer::default();
        let mut observed = false;
        assert!(!forward_http_response_with_observer(
            &mut host,
            &mut buffered,
            &mut sink,
            "HEAD",
            false,
            false,
            |status, body| {
                assert_eq!(status, 200);
                assert!(body.is_empty());
                observed = true;
                Ok(None)
            },
        )
        .unwrap());
        assert!(observed);
        drop(sink);
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(response, head);
        assert!(buffered.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn slow_request_header_and_content_length_body_share_total_deadline() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            if writer
                .write_all(b"POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\n")
                .is_err()
            {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if writer.write_all(b"Content-Length: 2\r\n\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if writer.write_all(b"{").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(120));
            let _ = writer.write_all(b"}");
        });

        let started = Instant::now();
        let error = match read_http_request_with_budget_from_until(
            &mut reader,
            Vec::new(),
            None,
            Instant::now() + Duration::from_millis(220),
        ) {
            Ok(_) => panic!("slow header and body must share one request deadline"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("total Docker API request deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(reader);
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn slow_chunked_request_body_cannot_extend_total_deadline() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            if writer
                .write_all(
                    b"POST /v1.43/volumes/create HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .is_err()
            {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if writer.write_all(b"2\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if writer.write_all(b"a").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(120));
            let _ = writer.write_all(b"b\r\n0\r\n\r\n");
        });

        let started = Instant::now();
        let error = match read_http_request_with_budget_from_until(
            &mut reader,
            Vec::new(),
            None,
            Instant::now() + Duration::from_millis(220),
        ) {
            Ok(_) => panic!("slow chunked body must use the header's request deadline"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("total Docker API request deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(reader);
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dial_stdio_clears_timeouts_from_host_and_guest_streams() {
        use std::os::unix::net::UnixStream;

        let (host, _host_peer) = UnixStream::pair().unwrap();
        let (client, _client_peer) = UnixStream::pair().unwrap();
        for stream in [&host, &client] {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
        }

        clear_dial_stdio_timeouts(&host, &client).unwrap();

        assert_eq!(host.read_timeout().unwrap(), None);
        assert_eq!(host.write_timeout().unwrap(), None);
        assert_eq!(client.read_timeout().unwrap(), None);
        assert_eq!(client.write_timeout().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn failed_client_stream_registration_aborts_lease() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;

        let conns = LeaseConnSet::new(Arc::new(AtomicBool::new(false)));
        let (tracked, mut tracked_peer) = UnixStream::pair().unwrap();
        let _tracked_watch = conns.watch(&tracked).unwrap();
        let (mut wake_reader, wake_writer) = UnixStream::pair().unwrap();
        conns.set_shutdown_wake(Arc::new(Mutex::new(wake_writer)));
        let (client, mut peer) = UnixStream::pair().unwrap();
        let result = conns.watch_with_clone_result(
            &client,
            Err(io::Error::new(
                io::ErrorKind::Other,
                "simulated client clone failure",
            )),
        );

        assert!(
            result.is_err(),
            "untracked client stream must fail registration"
        );
        assert!(conns.is_shutdown(), "clone failure must close the lease");
        let mut byte = [0_u8; 1];
        assert_eq!(peer.read(&mut byte).unwrap(), 0, "client stream must close");
        assert_eq!(
            tracked_peer.read(&mut byte).unwrap(),
            0,
            "tracked streams must close"
        );
        wake_reader
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(wake_reader.read(&mut byte).unwrap(), 1);
        assert_eq!(byte, [1], "accept loop must be woken to close the lease");
    }

    #[cfg(unix)]
    #[test]
    fn failed_tcp_client_stream_registration_aborts_lease() {
        use std::io::Read;
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let conns = LeaseConnSet::new(Arc::new(AtomicBool::new(false)));
        let result = conns.watch_tcp_with_clone_result(
            &client,
            Err(io::Error::new(
                io::ErrorKind::Other,
                "simulated TCP client clone failure",
            )),
        );

        assert!(
            result.is_err(),
            "untracked TCP stream must fail registration"
        );
        assert!(conns.is_shutdown(), "clone failure must close the lease");
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(
            peer.read(&mut byte).unwrap(),
            0,
            "TCP client stream must close"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wait_and_follow_logs_responses_have_no_fixed_lifetime_deadline() {
        let fallback = Instant::now() + Duration::from_millis(50);
        let wait = api_request(
            "POST",
            "/v1.43/containers/job/wait?condition=not-running",
            b"",
        );
        let logs = api_request("GET", "/v1.43/containers/job/logs?follow=TRUE", b"");
        let finite_logs = api_request("GET", "/v1.43/containers/job/logs?follow=0", b"");
        let non_logs = api_request("GET", "/v1.43/containers/job/json?follow=1", b"");

        for request in [&wait, &logs] {
            let deadline = docker_api_response_deadline(request, fallback).unwrap();
            assert!(deadline.total.is_none());
            assert!(deadline.idle.is_none());
        }
        for request in [&finite_logs, &non_logs] {
            let deadline = docker_api_response_deadline(request, fallback).unwrap();
            assert_eq!(deadline.total, Some(fallback));
            assert_eq!(deadline.idle, Some(PROXY_IDLE_TIMEOUT));
        }
    }

    #[cfg(unix)]
    #[test]
    fn framed_wait_response_can_outlive_default_deadline() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::thread;

        let request = api_request("POST", "/v1.43/containers/job/wait", b"");
        let deadline =
            docker_api_response_deadline(&request, Instant::now() + Duration::from_millis(40))
                .unwrap();
        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut guest, mut client) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            source
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });

        let reusable = forward_http_response_with_observer_deadline(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "POST",
            false,
            false,
            deadline,
            |_, _| Ok(None),
        )
        .expect("framed wait response must outlive the short fallback deadline");
        assert!(!reusable);
        drop(client);
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        assert!(response.ends_with(b"{}"));
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn streaming_follow_logs_response_can_outlive_default_deadline() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::thread;

        let request = api_request("GET", "/v1.43/containers/job/logs?follow=1", b"");
        let deadline =
            docker_api_response_deadline(&request, Instant::now() + Duration::from_millis(40))
                .unwrap();
        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut guest, mut client) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            source
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            thread::sleep(Duration::from_millis(80));
            source.write_all(b"5\r\nhello\r\n0\r\n\r\n").unwrap();
        });

        let reusable = forward_http_response_with_observer_deadline(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "GET",
            false,
            false,
            deadline,
            |_, _| Ok(None),
        )
        .expect("follow logs response must outlive the short fallback deadline");
        assert!(!reusable);
        drop(client);
        let mut response = Vec::new();
        guest.read_to_end(&mut response).unwrap();
        assert!(response.ends_with(b"5\r\nhello\r\n0\r\n\r\n"));
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hijacked_proxy_propagates_engine_to_guest_copy_errors() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (host, mut engine) = UnixStream::pair().unwrap();
        let (client, guest) = UnixStream::pair().unwrap();
        engine.write_all(b"response").unwrap();
        drop(engine);
        drop(guest);

        let error = proxy_until_closed_with_lifetime(host, client, None)
            .expect_err("partial Engine-to-guest copy must fail the lease stream");
        assert!(format!("{error:#}").contains("copy Engine-to-guest Docker lease stream"));
    }

    #[cfg(unix)]
    #[test]
    fn hijacked_proxy_propagates_guest_to_engine_copy_errors() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (host, engine) = UnixStream::pair().unwrap();
        let (client, mut guest) = UnixStream::pair().unwrap();
        drop(engine);
        guest.write_all(b"request").unwrap();
        drop(guest);

        let error = proxy_until_closed_with_lifetime(host, client, None)
            .expect_err("partial guest-to-Engine copy must fail the lease stream");
        assert!(format!("{error:#}").contains("copy guest-to-Engine Docker lease stream"));
    }

    #[cfg(unix)]
    #[test]
    fn slow_content_length_response_body_shares_header_deadline() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut guest) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            if source.write_all(b"HTTP/1.1 200 OK\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if source.write_all(b"Content-Length: 2\r\n\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if source.write_all(b"a").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(120));
            let _ = source.write_all(b"b");
        });

        let started = Instant::now();
        let error = forward_http_response_with_observer_until(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "GET",
            false,
            false,
            Instant::now() + Duration::from_millis(220),
            |_, _| Ok(None),
        )
        .expect_err("framed response body must share the header total deadline");
        assert!(format!("{error:#}").contains("total Docker API response deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));

        drop(client);
        drop(host);
        let mut forwarded = Vec::new();
        guest.read_to_end(&mut forwarded).unwrap();
        assert!(forwarded.ends_with(b"a"));
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn slow_chunked_response_body_shares_header_deadline() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut guest) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            if source.write_all(b"HTTP/1.1 200 OK\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if source
                .write_all(b"Transfer-Encoding: chunked\r\n\r\n")
                .is_err()
            {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if source.write_all(b"2\r\n").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if source.write_all(b"a").is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(120));
            let _ = source.write_all(b"b\r\n0\r\n\r\n");
        });

        let started = Instant::now();
        let error = forward_http_response_with_observer_until(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "GET",
            false,
            false,
            Instant::now() + Duration::from_millis(260),
            |_, _| Ok(None),
        )
        .expect_err("chunked response body must share the header total deadline");
        assert!(format!("{error:#}").contains("total Docker API response deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));

        drop(client);
        drop(host);
        let mut forwarded = Vec::new();
        guest.read_to_end(&mut forwarded).unwrap();
        assert!(forwarded.ends_with(b"a"));
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unframed_response_trickle_cannot_extend_total_deadline() {
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut guest) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            for byte in b"trickle" {
                if source.write_all(&[*byte]).is_err() {
                    break;
                }
                // Every inter-byte wait is much shorter than the 300 second
                // idle timeout. Only the total deadline stops this stream.
                thread::sleep(Duration::from_millis(80));
            }
        });

        let started = Instant::now();
        let error = forward_unframed_response_until(
            &mut host,
            &mut client,
            false,
            Instant::now() + Duration::from_millis(210),
        )
        .expect_err("trickled bytes must not reset the total response deadline");
        assert!(format!("{error:#}").contains("total Docker API response deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(client);
        let mut forwarded = Vec::new();
        guest.read_to_end(&mut forwarded).unwrap();
        assert!(!forwarded.is_empty());
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn slow_response_headers_share_the_unframed_total_deadline() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::thread;

        let (mut source, mut host) = UnixStream::pair().unwrap();
        let (mut client, mut guest) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            for byte in b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n" {
                if source.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(80));
            }
        });

        let started = Instant::now();
        let error = forward_http_response_with_observer_until(
            &mut host,
            &mut ResponseBuffer::default(),
            &mut client,
            "GET",
            false,
            false,
            Instant::now() + Duration::from_millis(210),
            |_, _| Ok(None),
        )
        .expect_err("slow status/headers must not reset the total response deadline");
        assert!(format!("{error:#}").contains("total Docker API response deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));

        drop(client);
        drop(host);
        let mut forwarded = Vec::new();
        guest.read_to_end(&mut forwarded).unwrap();
        assert!(
            forwarded.is_empty(),
            "incomplete headers must not be forwarded"
        );
        writer.join().unwrap();
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

        let create_body = br#"{"Image":"busybox:1.36","HostConfig":{"NetworkMode":"none"}}"#;
        let pipelined = format!(
            "GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: keep-alive\r\n\r\nPOST /v1.43/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            create_body.len(),
            std::str::from_utf8(create_body).unwrap(),
        );
        client.write_all(pipelined.as_bytes()).unwrap();
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

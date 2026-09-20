//! Private per-worker DinD daemon provisioning.
//!
//! Each worker owns one DinD daemon container that serves ONLY its paired
//! runner container. The daemon listens on a private Unix socket on a
//! per-worker bind mount — never TCP, never the host's
//! `/var/run/docker.sock`:
//!
//! * dockerd starts with a single `-H unix://...` listener, so no TCP
//!   port exists to publish even by accident;
//! * the create argv carries no `-p`/`--publish` flag (proven by test);
//! * the state dir bind carries no Docker socket from the host (proven by
//!   test: the only socket under it is the one this daemon creates);
//! * the runner reaches the daemon through the identical absolute socket
//!   path on the shared bind (see [`super::runner`]).
//!
//! Provisioning is idempotent on the recorded [`WorkerIdentity`](super::ownership::WorkerIdentity):
//! an existing container with matching ownership labels is adopted, an
//! existing container with foreign labels fails closed (a name collision
//! outside our ownership must never be commandeered).

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result};

use super::ownership::{
    WorkerIdentity, OWNERSHIP_LABEL, ROLE_DIND, ROLE_RUNNER, WORKER_ROLE_LABEL,
};
use super::runner::{
    docker_explicitly_rejected_create, DockerCreateTarget, DockerLifecycleEvent, PinnedImage,
};
use super::WorkerRunner;

/// Guest-absolute mount point of the per-worker state dir bind.
///
/// The state dir bind (`<state-dir>:/velnor/scaleset`) is mounted at this
/// identical absolute path in BOTH containers of a pair, so the socket path
/// below names the same file on both sides without any translation.
pub const STATE_MOUNT: &str = "/velnor/scaleset";
/// Guest-absolute private daemon socket (shared bind, same path both sides).
pub const DIND_SOCKET: &str = "/velnor/scaleset/dind.sock";
/// Guest-absolute BuildKit cache dir on the shared bind (identical path
/// both sides; inner builds address `--cache-to/--cache-from
/// type=local` here).
pub const BUILDKIT_CACHE_DIR: &str = "/velnor/scaleset/buildkit-cache";
/// dockerd's data root inside the daemon container (named volume).
pub const DIND_DATA_ROOT: &str = "/var/lib/docker";
/// Bound one control-plane Docker inspect operation.
pub const DIND_INSPECT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Distinguish worker volumes so label-filtered prune targets one resource.
pub(crate) const WORKER_VOLUME_LABEL: &str = "velnor.scaleset.volume";
pub(crate) const WORKER_VOLUME_NAME_LABEL: &str = "velnor.scaleset.volume-name";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerVolume {
    Workspace,
    DindData,
}

impl WorkerVolume {
    fn target(self) -> DockerCreateTarget {
        match self {
            Self::Workspace => DockerCreateTarget::WorkspaceVolume,
            Self::DindData => DockerCreateTarget::DindDataVolume,
        }
    }

    fn name(self, identity: &WorkerIdentity) -> String {
        match self {
            Self::Workspace => identity.workspace_volume(),
            Self::DindData => identity.dind_data_volume(),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::DindData => "dind-data",
        }
    }
}

impl fmt::Display for WorkerVolume {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Fully-derived DinD provision spec: image, identity, host state dir.
#[derive(Debug, Clone)]
pub struct DindSpec {
    identity: WorkerIdentity,
    image: PinnedImage,
    state_dir: PathBuf,
}

impl DindSpec {
    /// Derive the spec. `state_dir` is the host directory for this
    /// worker (`<daemon-state>/scale-set/<slug>`); provisioning creates it.
    #[must_use]
    pub fn new(identity: WorkerIdentity, image: PinnedImage, state_dir: &Path) -> Self {
        Self {
            identity,
            image,
            state_dir: state_dir.to_path_buf(),
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

    /// Host path of the private socket once dockerd creates it.
    #[must_use]
    pub fn host_socket(&self) -> PathBuf {
        self.state_dir.join("dind.sock")
    }

    /// Host path of the BuildKit cache dir on the shared bind.
    #[must_use]
    pub fn host_cache_dir(&self) -> PathBuf {
        self.state_dir.join("buildkit-cache")
    }

    /// `docker create` argv for the daemon container.
    ///
    /// Invariants (all proven by unit tests on this vector):
    /// * `--privileged` (DinD cannot run unprivileged; documented, not hidden);
    /// * no `-p`/`--publish`/`--expose`: no TCP surface at all;
    /// * no host socket bind: the only socket is the daemon's own;
    /// * dockerd command line carries exactly one `-H unix://` listener.
    #[must_use]
    pub fn create_args(&self, network_id: &str) -> Vec<String> {
        let mut args = vec![
            "create".to_string(),
            "--privileged".to_string(),
            "--name".to_string(),
            self.identity.dind_container(),
            "--network".to_string(),
            network_id.to_string(),
            "--env".to_string(),
            // No TLS material: the only listener is a filesystem socket
            // only this worker pair can reach.
            "DOCKER_TLS_CERTDIR=".to_string(),
            "--volume".to_string(),
            format!("{}:{DIND_DATA_ROOT}", self.identity.dind_data_volume()),
            "--volume".to_string(),
            format!("{}:{STATE_MOUNT}", self.state_dir.display()),
        ];
        args.extend(self.identity.label_args(ROLE_DIND));
        args.push("--".to_string());
        args.push(self.image.reference().to_string());
        // dockerd with ONE listener: the private Unix socket. No
        // `tcp://` listener exists, so nothing can be published.
        args.push(format!("-H unix://{DIND_SOCKET}"));
        args
    }

    /// Readiness probe argv: `docker version` against the private socket
    /// from INSIDE the daemon container (proves dockerd serves the
    /// socket; the dind image ships the CLI).
    #[must_use]
    pub fn probe_args(&self, dind_id: &str) -> Vec<String> {
        vec![
            "exec".to_string(),
            dind_id.to_string(),
            "docker".to_string(),
            "-H".to_string(),
            format!("unix://{DIND_SOCKET}"),
            "version".to_string(),
            "--format".to_string(),
            "{{.Server.Version}}".to_string(),
        ]
    }
}

/// What [`ensure_dind`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DindProvision {
    /// A live container with matching ownership labels was adopted.
    Adopted,
    /// A fresh daemon container was created and started.
    Created,
}

/// Parse `docker inspect --format {{.Id}}` output for adoption.
#[must_use]
pub fn parse_container_id(output: &str) -> Option<String> {
    let id = output.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Resolve a worker container by its deterministic name and prove all
/// identity labels before returning its immutable Docker ID. Callers use
/// that ID for subsequent actions so a same-name replacement cannot be
/// stopped, captured, or removed after this check.
pub(crate) fn inspect_owned_container(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
) -> Result<Option<String>> {
    let name = match role {
        ROLE_DIND => identity.dind_container(),
        ROLE_RUNNER => identity.runner_container(),
        _ => anyhow::bail!("unsupported worker container role {role:?}"),
    };
    inspect_owned_container_ref(runner, identity, role, &name, false)
}

/// Inspect an already verified immutable container ID before an ID-bound
/// operation. Returns false when that exact ID has disappeared.
pub(crate) fn validate_owned_container_id(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
    id: &str,
) -> Result<bool> {
    let Some(found) = inspect_owned_container_ref(runner, identity, role, id, true)? else {
        return Ok(false);
    };
    if found != id {
        anyhow::bail!(
            "inspect worker container ID {id} resolved to {found}: refusing mutable or abbreviated identity"
        );
    }
    Ok(true)
}

/// Inspect an already verified ID under an operation-scoped timeout.
pub(crate) fn validate_owned_container_id_timeout(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
    id: &str,
    timeout: Duration,
) -> Result<bool> {
    let Some(found) =
        inspect_owned_container_ref_with_timeout(runner, identity, role, id, true, Some(timeout))?
    else {
        return Ok(false);
    };
    if found != id {
        anyhow::bail!(
            "inspect worker container ID {id} resolved to {found}: refusing mutable or abbreviated identity"
        );
    }
    Ok(true)
}

fn inspect_owned_container_ref(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
    reference: &str,
    reference_is_id: bool,
) -> Result<Option<String>> {
    inspect_owned_container_ref_with_timeout(
        runner,
        identity,
        role,
        reference,
        reference_is_id,
        None,
    )
}

fn inspect_owned_container_ref_with_timeout(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
    reference: &str,
    reference_is_id: bool,
    timeout: Option<Duration>,
) -> Result<Option<String>> {
    let name = match role {
        ROLE_DIND => identity.dind_container(),
        ROLE_RUNNER => identity.runner_container(),
        _ => anyhow::bail!("unsupported worker container role {role:?}"),
    };
    let args = [
        "inspect".to_string(),
        "--type".to_string(),
        "container".to_string(),
        "--format".to_string(),
        r#"{{.Id}}{{"\n"}}{{json .Config.Labels}}"#.to_string(),
        "--".to_string(),
        reference.to_string(),
    ];
    let inspect = runner
        .run_timeout(
            "docker",
            &args,
            timeout.unwrap_or(DIND_INSPECT_OPERATION_TIMEOUT),
        )
        .with_context(|| format!("inspect worker container {reference}"))?;
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return Ok(None);
        }
        anyhow::bail!(
            "inspect worker container {reference} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }

    let (id, labels) = parse_id_and_labels(&inspect.stdout, "container", &name)?;
    if reference_is_id && id != reference {
        anyhow::bail!(
            "inspect worker container ID {reference} returned {id}: refusing mutable or abbreviated identity"
        );
    }
    let mut expected = identity.labels();
    expected.insert(WORKER_ROLE_LABEL.to_string(), role.to_string());
    validate_labels("container", &name, &labels, &expected)?;
    Ok(Some(id))
}

fn parse_id_and_labels(
    output: &str,
    resource: &str,
    name: &str,
) -> Result<(String, BTreeMap<String, String>)> {
    let (id, labels) = output
        .split_once('\n')
        .with_context(|| format!("inspect {resource} {name} returned no labels"))?;
    let id = parse_container_id(id)
        .with_context(|| format!("inspect {resource} {name} returned an empty id"))?;
    let labels: Option<BTreeMap<String, String>> = serde_json::from_str(labels.trim())
        .with_context(|| format!("inspect {resource} {name} returned malformed labels"))?;
    Ok((id, labels.unwrap_or_default()))
}

fn validate_labels(
    resource: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> Result<()> {
    for (key, expected_value) in expected {
        match labels.get(key) {
            Some(found) if found == expected_value => {}
            Some(found) if key == OWNERSHIP_LABEL => anyhow::bail!(
                "{resource} {name} carries foreign ownership {found:?}, expected {expected_value:?}: refusing to adopt"
            ),
            Some(found) => anyhow::bail!(
                "{resource} {name} carries wrong {key} label {found:?}, expected {expected_value:?}: refusing to adopt"
            ),
            None => anyhow::bail!(
                "{resource} {name} carries no {key} label: refusing to adopt"
            ),
        }
    }
    Ok(())
}

fn validate_exact_labels(
    resource: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> Result<()> {
    if labels != expected {
        anyhow::bail!(
            "{resource} {name} labels do not match the expected worker spec: refusing adoption"
        );
    }
    Ok(())
}

fn docker_inspect_json(
    runner: &mut dyn WorkerRunner,
    args: &[String],
    resource: &str,
    reference: &str,
) -> Result<Option<serde_json::Value>> {
    let output = runner
        .run_timeout("docker", args, DIND_INSPECT_OPERATION_TIMEOUT)
        .with_context(|| format!("inspect {resource} {reference}"))?;
    if output.code != 0 {
        if crate::docker::client::daemon_reports_missing(&output.stderr) {
            return Ok(None);
        }
        anyhow::bail!(
            "inspect {resource} {reference} exited {}: {}",
            output.code,
            output.stderr.trim()
        );
    }
    serde_json::from_str(output.stdout.trim())
        .map(Some)
        .with_context(|| format!("parse {resource} {reference} inspect"))
}

fn container_snapshot(
    runner: &mut dyn WorkerRunner,
    reference: &str,
) -> Result<Option<serde_json::Value>> {
    docker_inspect_json(
        runner,
        &[
            "inspect".to_string(),
            "--type".to_string(),
            "container".to_string(),
            "--format".to_string(),
            "{{json .}}".to_string(),
            "--".to_string(),
            reference.to_string(),
        ],
        "worker container",
        reference,
    )
}

fn image_snapshot(runner: &mut dyn WorkerRunner, image: &PinnedImage) -> Result<serde_json::Value> {
    let reference = image.reference();
    docker_inspect_json(
        runner,
        &[
            "image".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{json .}}".to_string(),
            "--".to_string(),
            reference.clone(),
        ],
        "pinned worker image",
        &reference,
    )?
    .with_context(|| format!("pinned worker image {reference} is absent"))
}

fn string_field<'a>(
    object: &'a serde_json::Value,
    key: &str,
    description: &str,
) -> Result<&'a str> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("{description} inspect is missing string field {key}"))
}

fn object_field<'a>(
    object: &'a serde_json::Value,
    key: &str,
    description: &str,
) -> Result<&'a serde_json::Value> {
    object
        .get(key)
        .filter(|value| value.is_object())
        .with_context(|| format!("{description} inspect is missing object field {key}"))
}

fn labels_field(
    object: &serde_json::Value,
    key: &str,
    description: &str,
) -> Result<BTreeMap<String, String>> {
    let labels = object
        .get(key)
        .context("worker inspect is missing labels")?;
    string_map(labels, description)
}

fn string_map(value: &serde_json::Value, description: &str) -> Result<BTreeMap<String, String>> {
    let labels = value
        .as_object()
        .with_context(|| format!("{description} inspect is missing a string map"))?;
    labels
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .with_context(|| format!("{description} inspect contains a non-string label"))
        })
        .collect()
}

fn environment_map(
    config: &serde_json::Value,
    description: &str,
) -> Result<BTreeMap<String, String>> {
    let entries = config
        .get("Env")
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("{description} config is missing Env"))?;
    let mut environment = BTreeMap::new();
    for entry in entries {
        let entry = entry
            .as_str()
            .with_context(|| format!("{description} config has malformed Env"))?;
        let (key, value) = entry
            .split_once('=')
            .with_context(|| format!("{description} config has malformed Env entry"))?;
        if key.is_empty()
            || environment
                .insert(key.to_string(), value.to_string())
                .is_some()
        {
            anyhow::bail!("{description} config has duplicate or empty Env key");
        }
    }
    Ok(environment)
}

fn expected_image_labels(
    image_config: &serde_json::Value,
    identity: &WorkerIdentity,
    role: &str,
) -> Result<BTreeMap<String, String>> {
    let mut labels = image_config
        .get("Labels")
        .filter(|value| !value.is_null())
        .map(|value| string_map(value, "pinned image"))
        .transpose()?
        .unwrap_or_default();
    labels.extend(identity.labels());
    labels.insert(WORKER_ROLE_LABEL.to_string(), role.to_string());
    Ok(labels)
}

fn validate_image_derived_config(
    container: &serde_json::Value,
    image: &serde_json::Value,
    image_ref: &str,
    expected_labels: &BTreeMap<String, String>,
    env_overrides: &BTreeMap<String, String>,
    wildcard_env: &[&str],
    command_override: Option<&[String]>,
    description: &str,
) -> Result<()> {
    let id = string_field(container, "Id", description)?;
    let image_id = string_field(image, "Id", "pinned image")?;
    if string_field(container, "Image", description)? != image_id {
        anyhow::bail!("{description} {id} uses the wrong pinned image ID");
    }
    let config = object_field(container, "Config", description)?;
    let image_config = object_field(image, "Config", "pinned image")?;
    if string_field(config, "Image", description)? != image_ref {
        anyhow::bail!("{description} {id} uses the wrong image reference");
    }
    let actual_labels = labels_field(config, "Labels", description)?;
    validate_exact_labels(description, id, &actual_labels, expected_labels)?;

    for field in [
        "Entrypoint",
        "User",
        "WorkingDir",
        "ExposedPorts",
        "Volumes",
        "StopSignal",
        "Healthcheck",
        "Shell",
    ] {
        if config.get(field).unwrap_or(&serde_json::Value::Null)
            != image_config.get(field).unwrap_or(&serde_json::Value::Null)
        {
            anyhow::bail!("{description} {id} has mismatched image config field {field}");
        }
    }
    let expected_command = command_override
        .map(|command| serde_json::json!(command))
        .or_else(|| image_config.get("Cmd").cloned());
    if config.get("Cmd") != expected_command.as_ref() {
        anyhow::bail!("{description} {id} has mismatched image command");
    }

    let image_env = environment_map(image_config, "pinned image")?;
    let actual_env = environment_map(config, description)?;
    let mut expected_env = image_env;
    for (key, value) in env_overrides {
        expected_env.insert(key.clone(), value.clone());
    }
    for key in wildcard_env {
        let Some(value) = actual_env.get(*key) else {
            anyhow::bail!("{description} {id} is missing required environment key {key}");
        };
        if expected_env.contains_key(*key) {
            anyhow::bail!(
                "{description} image unexpectedly defines required environment key {key}"
            );
        }
        expected_env.insert((*key).to_string(), value.clone());
    }
    if actual_env != expected_env {
        anyhow::bail!("{description} {id} has mismatched runtime environment");
    }
    Ok(())
}

pub(crate) fn inspect_container_runtime(
    runner: &mut dyn WorkerRunner,
    reference: &str,
    identity: &WorkerIdentity,
    role: &str,
    image: &PinnedImage,
    mounts: &[ExpectedMount],
    network_mode: &str,
    expected_network: Option<(&str, &str)>,
    env_overrides: &BTreeMap<String, String>,
    wildcard_env: &[&str],
    command_override: Option<&[String]>,
    privileged: bool,
) -> Result<Option<String>> {
    let Some(container) = container_snapshot(runner, reference)? else {
        return Ok(None);
    };
    let id = string_field(&container, "Id", "worker container")?.to_string();
    if id != reference {
        anyhow::bail!("worker container inspect returned a different immutable ID");
    }
    let expected_name = match role {
        ROLE_DIND => identity.dind_container(),
        ROLE_RUNNER => identity.runner_container(),
        _ => anyhow::bail!("unsupported worker container role {role:?}"),
    };
    if string_field(&container, "Name", "worker container")? != format!("/{expected_name}") {
        anyhow::bail!("worker container {id} has unexpected name");
    }

    let image_snapshot = image_snapshot(runner, image)?;
    let image_config = object_field(&image_snapshot, "Config", "pinned image")?;
    let expected_labels = expected_image_labels(image_config, identity, role)?;
    validate_image_derived_config(
        &container,
        &image_snapshot,
        &image.reference(),
        &expected_labels,
        env_overrides,
        wildcard_env,
        command_override,
        "worker container",
    )?;

    let host_config = object_field(&container, "HostConfig", "worker container")?;
    let actual_network_mode = string_field(host_config, "NetworkMode", "worker container")?;
    let network_mode_matches = if role == ROLE_DIND {
        expected_network.is_some_and(|(_, network_id)| actual_network_mode == network_id)
    } else {
        actual_network_mode == network_mode
    };
    if !network_mode_matches {
        anyhow::bail!("worker container {id} has mismatched network namespace identity");
    }
    if host_config
        .get("Privileged")
        .and_then(serde_json::Value::as_bool)
        != Some(privileged)
    {
        anyhow::bail!("worker container {id} has mismatched privileged mode");
    }
    for field in [
        "PortBindings",
        "CapAdd",
        "CapDrop",
        "Devices",
        "SecurityOpt",
    ] {
        if !is_empty_config(host_config.get(field)) {
            anyhow::bail!("worker container {id} has unexpected HostConfig.{field}");
        }
    }
    for field in ["PublishAllPorts", "ReadonlyRootfs"] {
        if host_config
            .get(field)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            anyhow::bail!("worker container {id} has unexpected HostConfig.{field}");
        }
    }
    let restart_policy = object_field(host_config, "RestartPolicy", "worker container HostConfig")?;
    let restart_policy_name = string_field(
        restart_policy,
        "Name",
        "worker container HostConfig.RestartPolicy",
    )?;
    if host_config
        .get("AutoRemove")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || !matches!(restart_policy_name, "" | "no")
        || restart_policy
            .get("MaximumRetryCount")
            .and_then(serde_json::Value::as_u64)
            != Some(0)
    {
        anyhow::bail!("worker container {id} has an unexpected restart/removal policy");
    }

    validate_mounts(&container, mounts, &id)?;
    if let Some((network_name, network_id)) = expected_network {
        validate_network_attachments(&container, network_name, network_id, &id)?;
    }
    Ok(Some(id))
}

pub(crate) fn is_empty_config(value: Option<&serde_json::Value>) -> bool {
    value.is_none_or(|value| {
        value.is_null()
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty)
    })
}

fn is_empty_option_map(value: Option<&serde_json::Value>) -> bool {
    value.is_none_or(|value| {
        value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedMount {
    pub(crate) kind: &'static str,
    pub(crate) source: String,
    pub(crate) destination: &'static str,
    pub(crate) volume_name: Option<String>,
}

pub(crate) fn expected_bind(state_dir: &Path, destination: &'static str) -> Result<ExpectedMount> {
    Ok(ExpectedMount {
        kind: "bind",
        source: std::fs::canonicalize(state_dir)
            .with_context(|| format!("canonicalize worker bind {}", state_dir.display()))?
            .to_string_lossy()
            .into_owned(),
        destination,
        volume_name: None,
    })
}

pub(crate) fn expected_volume(name: String, destination: &'static str) -> ExpectedMount {
    ExpectedMount {
        kind: "volume",
        source: name.clone(),
        destination,
        volume_name: Some(name),
    }
}

fn validate_mounts(
    container: &serde_json::Value,
    expected: &[ExpectedMount],
    id: &str,
) -> Result<()> {
    let mounts = container
        .get("Mounts")
        .and_then(serde_json::Value::as_array)
        .context("worker container inspect is missing Mounts")?;
    if mounts.len() != expected.len() {
        anyhow::bail!("worker container {id} has unexpected mount count");
    }
    let mut observed = Vec::with_capacity(mounts.len());
    for mount in mounts {
        let kind = string_field(mount, "Type", "worker mount")?;
        let destination = string_field(mount, "Destination", "worker mount")?;
        let source = string_field(mount, "Source", "worker mount")?;
        let volume_name = mount
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned);
        let rw = mount
            .get("RW")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        observed.push((kind, source, destination, volume_name, rw));
    }
    for expected_mount in expected {
        let matched = observed
            .iter()
            .any(|(kind, source, destination, name, rw)| {
                *kind == expected_mount.kind
                    && *destination == expected_mount.destination
                    && *rw
                    && match &expected_mount.volume_name {
                        Some(expected_name) => name.as_deref() == Some(expected_name),
                        None => *source == expected_mount.source && name.is_none(),
                    }
            });
        if !matched {
            anyhow::bail!("worker container {id} has mismatched mount configuration");
        }
    }
    Ok(())
}

fn validate_network_attachments(
    container: &serde_json::Value,
    network_name: &str,
    network_id: &str,
    id: &str,
) -> Result<()> {
    let networks = container
        .pointer("/NetworkSettings/Networks")
        .and_then(serde_json::Value::as_object)
        .context("worker container inspect is missing NetworkSettings.Networks")?;
    if networks.len() != 1 {
        anyhow::bail!("worker container {id} has unexpected network attachments");
    }
    let attachment = networks
        .get(network_name)
        .context("DinD container is not attached to its worker network")?;
    if string_field(attachment, "NetworkID", "network attachment")? != network_id {
        anyhow::bail!("DinD container {id} is attached to the wrong worker network ID");
    }
    Ok(())
}

pub(crate) fn validate_dind_runtime(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    dind_id: &str,
    network_id: &str,
) -> Result<bool> {
    validate_unique_owned_container_claim(runner, spec.identity(), ROLE_DIND, dind_id)?;
    let expected_mounts = [
        expected_volume(spec.identity().dind_data_volume(), DIND_DATA_ROOT),
        expected_bind(spec.state_dir(), STATE_MOUNT)?,
    ];
    let env = BTreeMap::from([("DOCKER_TLS_CERTDIR".to_string(), String::new())]);
    let command = vec![format!("-H unix://{DIND_SOCKET}")];
    let network_name = spec.identity().network();
    let container = inspect_container_runtime(
        runner,
        dind_id,
        spec.identity(),
        ROLE_DIND,
        spec.image(),
        &expected_mounts,
        network_id,
        Some((&network_name, network_id)),
        &env,
        &[],
        Some(&command),
        true,
    )?;
    Ok(container.is_some())
}

pub(crate) fn validate_runner_runtime(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    image: &PinnedImage,
    state_dir: &Path,
    runner_id: &str,
    dind_id: &str,
    runner_name: &str,
    jit_config: Option<&str>,
) -> Result<bool> {
    validate_unique_owned_container_claim(runner, identity, ROLE_RUNNER, runner_id)?;
    let workspace = identity.workspace_volume();
    let cache_bind = state_dir.join("buildkit-cache");
    let expected_mounts = [
        expected_bind(state_dir, STATE_MOUNT)?,
        expected_volume(workspace.clone(), super::runner::RUNNER_WORK_DIR),
        expected_volume(workspace, super::runner::TOOL_CACHE_DIR),
        expected_bind(&cache_bind, BUILDKIT_CACHE_DIR)?,
    ];
    let mut env = BTreeMap::from([
        (
            super::runner::RUNNER_NAME_ENV.to_string(),
            runner_name.to_string(),
        ),
        ("DOCKER_HOST".to_string(), format!("unix://{DIND_SOCKET}")),
        (
            "RUNNER_WORK_FOLDER".to_string(),
            super::runner::RUNNER_WORK_DIR.to_string(),
        ),
    ]);
    let wildcard_env: &[&str] = if let Some(jit_config) = jit_config {
        env.insert(
            super::runner::JIT_CONFIG_ENV.to_string(),
            jit_config.to_string(),
        );
        &[]
    } else {
        &[super::runner::JIT_CONFIG_ENV]
    };
    let network_mode = format!("container:{dind_id}");
    let container = inspect_container_runtime(
        runner,
        runner_id,
        identity,
        ROLE_RUNNER,
        image,
        &expected_mounts,
        &network_mode,
        None,
        &env,
        wildcard_env,
        None,
        false,
    )?;
    Ok(container.is_some())
}

/// Docker's Engine defaults for omitted create flags are part of this
/// worker spec. Reject extra mappings rather than silently inheriting them.
pub(crate) fn validate_network_spec(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    network_id: &str,
) -> Result<()> {
    validate_unique_owned_network_claim(runner, identity, network_id)?;
    let value = docker_inspect_json(
        runner,
        &[
            "network".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{json .}}".to_string(),
            "--".to_string(),
            network_id.to_string(),
        ],
        "worker network",
        network_id,
    )?
    .context("worker network disappeared during configuration validation")?;
    if string_field(&value, "Id", "worker network")? != network_id
        || string_field(&value, "Name", "worker network")? != identity.network()
    {
        anyhow::bail!("worker network has mismatched immutable identity");
    }
    let labels = labels_field(&value, "Labels", "worker network")?;
    validate_exact_labels("network", &identity.network(), &labels, &identity.labels())?;
    if string_field(&value, "Driver", "worker network")? != "bridge"
        || string_field(&value, "Scope", "worker network")? != "local"
    {
        anyhow::bail!("worker network has mismatched driver or scope");
    }
    for field in [
        "Internal",
        "Attachable",
        "Ingress",
        "ConfigOnly",
        "EnableIPv6",
    ] {
        if value.get(field).and_then(serde_json::Value::as_bool) != Some(false) {
            anyhow::bail!("worker network has mismatched {field} setting");
        }
    }
    for field in ["Options"] {
        if !is_empty_option_map(value.get(field)) {
            anyhow::bail!("worker network has unexpected {field}");
        }
    }
    let ipam = object_field(&value, "IPAM", "worker network")?;
    validate_engine_allocated_ipam(ipam)?;
    Ok(())
}

/// The network create request deliberately omits subnet, gateway, IP range,
/// auxiliary addresses, and driver options. Docker Engine allocates the
/// subnet and gateway and reports them in inspect output. Accept that assigned
/// IPv4 shape while refusing any extra routing or address-pool configuration.
fn validate_engine_allocated_ipam(ipam: &serde_json::Value) -> Result<()> {
    if string_field(ipam, "Driver", "worker network IPAM")? != "default"
        || !is_empty_option_map(ipam.get("Options"))
    {
        anyhow::bail!("worker network has unexpected IPAM driver or options");
    }
    let ipam_object = ipam
        .as_object()
        .context("worker network IPAM must be an object")?;
    if ipam_object
        .keys()
        .any(|key| !matches!(key.as_str(), "Driver" | "Options" | "Config"))
    {
        anyhow::bail!("worker network has unexpected IPAM fields");
    }
    let configs = ipam
        .get("Config")
        .and_then(serde_json::Value::as_array)
        .filter(|configs| configs.len() == 1)
        .context("worker network must have one Engine-assigned IPv4 IPAM config")?;
    let config = configs[0]
        .as_object()
        .context("worker network IPAM config must be an object")?;
    if config.keys().any(|key| {
        !matches!(
            key.as_str(),
            "Subnet" | "Gateway" | "IPRange" | "AuxiliaryAddresses"
        )
    }) {
        anyhow::bail!("worker network IPAM config has unexpected fields or routes");
    }
    if !config
        .get("IPRange")
        .is_none_or(|range| range.is_null() || range.as_str() == Some(""))
        || !config.get("AuxiliaryAddresses").is_none_or(|addresses| {
            addresses.is_null() || addresses.as_object().is_some_and(serde_json::Map::is_empty)
        })
    {
        anyhow::bail!("worker network IPAM config has a custom address pool");
    }

    let subnet = config
        .get("Subnet")
        .and_then(serde_json::Value::as_str)
        .context("worker network IPAM subnet must be an IPv4 CIDR")?;
    let (subnet_address, prefix) = subnet
        .split_once('/')
        .context("worker network IPAM subnet must be an IPv4 CIDR")?;
    let subnet_address: Ipv4Addr = subnet_address
        .parse()
        .context("worker network IPAM subnet address is invalid")?;
    let prefix: u8 = prefix
        .parse()
        .context("worker network IPAM subnet prefix is invalid")?;
    if !(1..=30).contains(&prefix) {
        anyhow::bail!("worker network IPAM subnet prefix must be between /1 and /30");
    }
    if subnet != format!("{subnet_address}/{prefix}") {
        anyhow::bail!("worker network IPAM subnet is not canonical");
    }
    let mask = u32::MAX << (32 - u32::from(prefix));
    let subnet_number = u32::from(subnet_address);
    let network_number = subnet_number & mask;
    if subnet_number != network_number {
        anyhow::bail!("worker network IPAM subnet is not a canonical network CIDR");
    }

    let gateway_text = config
        .get("Gateway")
        .and_then(serde_json::Value::as_str)
        .context("worker network IPAM gateway must be an IPv4 address")?;
    let gateway: Ipv4Addr = gateway_text
        .parse()
        .context("worker network IPAM gateway is invalid")?;
    let gateway_number = u32::from(gateway);
    let broadcast_number = network_number | !mask;
    if gateway_number & mask != network_number
        || gateway_number == network_number
        || gateway_number == broadcast_number
        || gateway_number != network_number + 1
        || gateway_text != gateway.to_string()
    {
        anyhow::bail!(
            "worker network IPAM gateway must be the canonical first usable address in its subnet"
        );
    }
    Ok(())
}

fn validate_unique_owned_network_claim(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_id: &str,
) -> Result<()> {
    let mut args = vec![
        "network".to_string(),
        "ls".to_string(),
        "--no-trunc".to_string(),
        "--quiet".to_string(),
    ];
    for (key, value) in identity.labels() {
        args.push("--filter".to_string());
        args.push(format!("label={key}={value}"));
    }
    let output = runner
        .run_timeout("docker", &args, DIND_INSPECT_OPERATION_TIMEOUT)
        .context("list networks with exact worker ownership labels")?;
    if output.code != 0 {
        anyhow::bail!(
            "list networks with worker ownership labels exited {}: {}",
            output.code,
            output.stderr.trim()
        );
    }
    let ids: Vec<_> = output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.as_slice() != [expected_id] {
        anyhow::bail!(
            "worker network has duplicate or missing exact-label claims: expected {expected_id}, found {ids:?}"
        );
    }
    Ok(())
}

fn validate_unique_owned_container_claim(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    role: &str,
    expected_id: &str,
) -> Result<()> {
    let mut args = vec![
        "container".to_string(),
        "ls".to_string(),
        "--all".to_string(),
        "--no-trunc".to_string(),
        "--quiet".to_string(),
    ];
    let mut labels = identity.labels();
    labels.insert(WORKER_ROLE_LABEL.to_string(), role.to_string());
    for (key, value) in labels {
        args.push("--filter".to_string());
        args.push(format!("label={key}={value}"));
    }
    let output = runner
        .run_timeout("docker", &args, DIND_INSPECT_OPERATION_TIMEOUT)
        .with_context(|| format!("list containers with worker role {role} labels"))?;
    if output.code != 0 {
        anyhow::bail!(
            "list containers with worker role {role} labels exited {}: {}",
            output.code,
            output.stderr.trim()
        );
    }
    let ids: Vec<_> = output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.as_slice() != [expected_id] {
        anyhow::bail!(
            "worker role {role} has duplicate or missing exact-label container claims: expected {expected_id}, found {ids:?}"
        );
    }
    Ok(())
}

/// Inspect the exact Docker resource named by a durable create intent.
/// Present objects must carry this worker's ownership before the intent can
/// be considered settled.
pub(crate) fn create_target_exists(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: DockerCreateTarget,
) -> Result<bool> {
    match target {
        DockerCreateTarget::Network => {
            return Ok(inspect_owned_network(runner, identity)?.is_some())
        }
        DockerCreateTarget::WorkspaceVolume | DockerCreateTarget::DindDataVolume => {
            return Ok(verify_owned_volume_unique(runner, identity, target)?.is_some());
        }
        DockerCreateTarget::Dind | DockerCreateTarget::Runner => {
            let role = match target {
                DockerCreateTarget::Dind => ROLE_DIND,
                DockerCreateTarget::Runner => ROLE_RUNNER,
                _ => unreachable!(),
            };
            return Ok(inspect_owned_container(runner, identity, role)?.is_some());
        }
    }
}

/// Inspect a worker network by its deterministic name and prove every
/// identity label before returning its immutable Docker ID.
pub(crate) fn inspect_owned_network(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<Option<String>> {
    inspect_owned_network_ref(runner, identity, &identity.network(), false)
}

/// Verify an already inspected network ID immediately before an ID-bound
/// operation. Returns false when that exact ID has disappeared.
pub(crate) fn validate_owned_network_id(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    id: &str,
) -> Result<bool> {
    let Some(found) = inspect_owned_network_ref(runner, identity, id, true)? else {
        return Ok(false);
    };
    if found != id {
        anyhow::bail!(
            "inspect worker network ID {id} returned {found}: refusing mutable or abbreviated identity"
        );
    }
    Ok(true)
}

fn inspect_owned_network_ref(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    reference: &str,
    reference_is_id: bool,
) -> Result<Option<String>> {
    let name = identity.network();
    let inspect = runner
        .run_timeout(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{.Id}}{{"\n"}}{{json .Labels}}"#.to_string(),
                "--".to_string(),
                reference.to_string(),
            ],
            DIND_INSPECT_OPERATION_TIMEOUT,
        )
        .with_context(|| format!("inspect worker network {reference}"))?;
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return Ok(None);
        }
        anyhow::bail!(
            "inspect worker network {reference} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }

    let (id, labels) = parse_id_and_labels(&inspect.stdout, "network", &name)?;
    if reference_is_id && id != reference {
        anyhow::bail!(
            "inspect worker network ID {reference} returned {id}: refusing mutable or abbreviated identity"
        );
    }
    validate_labels("network", &name, &labels, &identity.labels())?;
    Ok(Some(id))
}

/// Inspect a named worker volume and verify identity, type, and exact-name
/// labels. Docker volumes expose no immutable ID, so cleanup uses the exact
/// labels in a daemon-side prune operation instead of removing by name.
pub(crate) fn inspect_owned_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: DockerCreateTarget,
) -> Result<Option<String>> {
    let volume = volume_for_target(target)?;
    let name = volume.name(identity);
    let inspect = runner
        .run_timeout(
            "docker",
            &[
                "volume".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{.Name}}{{"\n"}}{{json .Labels}}"#.to_string(),
                "--".to_string(),
                name.clone(),
            ],
            DIND_INSPECT_OPERATION_TIMEOUT,
        )
        .with_context(|| format!("inspect worker volume {name}"))?;
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return Ok(None);
        }
        anyhow::bail!(
            "inspect worker volume {name} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }

    let (found_name, labels) = parse_id_and_labels(&inspect.stdout, "volume", &name)?;
    if found_name != name {
        anyhow::bail!(
            "inspect worker volume {name} returned different name {found_name}: refusing adoption"
        );
    }
    validate_labels("volume", &name, &labels, &volume_labels(identity, volume))?;
    Ok(Some(found_name))
}

fn volume_for_target(target: DockerCreateTarget) -> Result<WorkerVolume> {
    match target {
        DockerCreateTarget::WorkspaceVolume => Ok(WorkerVolume::Workspace),
        DockerCreateTarget::DindDataVolume => Ok(WorkerVolume::DindData),
        _ => anyhow::bail!("{target:?} is not a worker volume"),
    }
}

fn volume_labels(identity: &WorkerIdentity, volume: WorkerVolume) -> BTreeMap<String, String> {
    let name = volume.name(identity);
    let mut labels = identity.labels();
    labels.insert(WORKER_VOLUME_LABEL.to_string(), volume.label().to_string());
    labels.insert(WORKER_VOLUME_NAME_LABEL.to_string(), name);
    labels
}

/// Prove a named volume is the only Docker volume with this exact worker,
/// resource-kind, and deterministic-name label set. Volume inspect exposes
/// no immutable ID, so ambiguity fails closed before a mount or prune.
pub(crate) fn verify_owned_volume_unique(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: DockerCreateTarget,
) -> Result<Option<String>> {
    let volume = volume_for_target(target)?;
    let expected_name = volume.name(identity);
    let inspected = inspect_owned_volume(runner, identity, target)?;
    let mut args = vec![
        "volume".to_string(),
        "ls".to_string(),
        "--quiet".to_string(),
    ];
    for (key, value) in volume_labels(identity, volume) {
        args.push("--filter".to_string());
        args.push(format!("label={key}={value}"));
    }
    let listed = runner
        .run_timeout("docker", &args, DIND_INSPECT_OPERATION_TIMEOUT)
        .with_context(|| format!("list volumes with worker {volume} ownership labels"))?;
    if listed.code != 0 {
        anyhow::bail!(
            "list volumes with worker {volume} ownership labels exited {}: {}",
            listed.code,
            listed.stderr.trim()
        );
    }
    let names: Vec<_> = listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    match (inspected.as_deref(), names.as_slice()) {
        (Some(name), [listed_name]) if name == *listed_name && name == expected_name => {
            validate_volume_spec(runner, identity, volume, name)?;
            Ok(Some(name.to_string()))
        }
        (None, []) => Ok(None),
        (Some(_), _) | (None, _) => anyhow::bail!(
            "worker {volume} volume {expected_name} has ambiguous exact ownership labels; inspect={inspected:?}, listed={names:?}: refusing to mount or prune"
        ),
    }
}

fn validate_volume_spec(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    volume: WorkerVolume,
    name: &str,
) -> Result<()> {
    let value = docker_inspect_json(
        runner,
        &[
            "volume".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{json .}}".to_string(),
            "--".to_string(),
            name.to_string(),
        ],
        "worker volume",
        name,
    )?
    .context("worker volume disappeared during configuration validation")?;
    if string_field(&value, "Name", "worker volume")? != name
        || string_field(&value, "Driver", "worker volume")? != "local"
    {
        anyhow::bail!("worker volume {name} has mismatched identity or driver");
    }
    let labels = labels_field(&value, "Labels", "worker volume")?;
    validate_exact_labels("volume", name, &labels, &volume_labels(identity, volume))?;
    if !is_empty_option_map(value.get("Options")) {
        anyhow::bail!("worker volume {name} has unexpected driver options");
    }
    Ok(())
}

fn volume_create_args(identity: &WorkerIdentity, volume: WorkerVolume) -> Vec<String> {
    let mut args = vec!["volume".to_string(), "create".to_string()];
    for (key, value) in volume_labels(identity, volume) {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push("--".to_string());
    args.push(volume.name(identity));
    args
}

/// What an explicit per-worker volume create found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeProvision {
    Adopted,
    Created,
}

/// Create/adopt the workspace volume before the runner mounts it.
pub(crate) fn ensure_workspace_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
) -> Result<VolumeProvision> {
    ensure_worker_volume(runner, identity, WorkerVolume::Workspace, lifecycle_event)
}

fn ensure_dind_data_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
) -> Result<VolumeProvision> {
    ensure_worker_volume(runner, identity, WorkerVolume::DindData, lifecycle_event)
}

fn ensure_worker_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    volume: WorkerVolume,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
) -> Result<VolumeProvision> {
    let name = volume.name(identity);
    let target = volume.target();
    if verify_owned_volume_unique(runner, identity, target)?.is_some() {
        lifecycle_event(DockerLifecycleEvent::CreateResolved(target))
            .with_context(|| format!("resolve existing {volume} volume create intent"))?;
        return Ok(VolumeProvision::Adopted);
    }

    lifecycle_event(DockerLifecycleEvent::BeforeCreateRequest(target))
        .with_context(|| format!("persist {volume} volume create intent"))?;
    let created = runner
        .run("docker", &volume_create_args(identity, volume))
        .with_context(|| format!("create worker {volume} volume {name}"))?;
    if created.code != 0 {
        if docker_explicitly_rejected_create(&created.stderr) {
            settle_explicit_rejection(
                runner,
                identity,
                target,
                lifecycle_event,
                &format!("worker {volume} volume {name}"),
                &created,
            )?;
        }
        anyhow::bail!(
            "create worker {volume} volume {name} exited {}: {}",
            created.code,
            created.stderr.trim()
        );
    }

    let returned_name = created.stdout.trim();
    if returned_name != name {
        anyhow::bail!(
            "create worker {volume} volume {name} returned unexpected name {returned_name:?}"
        );
    }
    if verify_owned_volume_unique(runner, identity, target)?.is_none() {
        anyhow::bail!("worker {volume} volume {name} disappeared after create");
    }
    lifecycle_event(DockerLifecycleEvent::CreateResolved(target))
        .with_context(|| format!("record worker {volume} volume create settlement"))?;
    Ok(VolumeProvision::Created)
}

/// Remove an unused worker volume with a server-side exact-label filter.
/// Docker exposes volume names but no immutable volume IDs; pruning with all
/// owner, kind, and name labels prevents a same-name foreign replacement from
/// being deleted. Docker skips any volume still mounted by a container.
pub(crate) fn prune_owned_volume(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: DockerCreateTarget,
) -> Result<()> {
    let volume = volume_for_target(target)?;
    let name = volume.name(identity);
    if verify_owned_volume_unique(runner, identity, target)?.is_none() {
        return Ok(());
    }

    let mut args = vec![
        "volume".to_string(),
        "prune".to_string(),
        "--all".to_string(),
        "--force".to_string(),
    ];
    for (key, value) in volume_labels(identity, volume) {
        args.push("--filter".to_string());
        args.push(format!("label={key}={value}"));
    }
    let pruned = runner
        .run("docker", &args)
        .with_context(|| format!("prune owned {volume} volume {name}"))?;
    if pruned.code != 0 {
        anyhow::bail!(
            "prune owned {volume} volume {name} exited {}: {}",
            pruned.code,
            pruned.stderr.trim()
        );
    }
    if verify_owned_volume_unique(runner, identity, target)?.is_some() {
        anyhow::bail!(
            "owned {volume} volume {name} remains after exact-label prune; it may still be in use"
        );
    }
    Ok(())
}

fn settle_explicit_rejection(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    target: DockerCreateTarget,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
    resource: &str,
    created: &super::WorkerOutput,
) -> Result<()> {
    let settlement = (|| {
        // False means the exact target is absent; true means it exists with
        // all expected labels. An error means foreign or uninspectable.
        create_target_exists(runner, identity, target)?;
        lifecycle_event(DockerLifecycleEvent::CreateResolved(target))
            .context("record rejected create settlement")
    })();
    settlement.with_context(|| {
        format!(
            "{resource} create exited {}: {}; exact target settlement failed",
            created.code,
            created.stderr.trim()
        )
    })
}

/// Parse one `key=value` label line of `docker inspect --format` output.
#[must_use]
pub fn parse_label_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Parse the readiness probe output: any non-empty server version is ready.
#[must_use]
pub fn parse_probe_output(output: &str) -> bool {
    !output.trim().is_empty()
}

/// Ensure the private daemon exists and is started, adopting on retry.
///
/// * Looks up the recorded container name; a missing container is created
///   from [`DindSpec::create_args`] and started.
/// * An existing container's ownership labels must match this worker;
///   foreign labels fail closed instead of adopting a stranger.
/// * Never pulls here: images are pulled + verified by the tool-content
///   hook ([`super::runner`]) before provisioning starts.
pub(crate) fn ensure_dind(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
) -> Result<DindProvision> {
    std::fs::create_dir_all(spec.state_dir()).with_context(|| {
        format!(
            "create scale-set worker state dir {}",
            spec.state_dir().display()
        )
    })?;
    std::fs::create_dir_all(spec.host_cache_dir()).with_context(|| {
        format!(
            "create scale-set BuildKit cache dir {}",
            spec.host_cache_dir().display()
        )
    })?;

    ensure_dind_data_volume(runner, spec.identity(), lifecycle_event)?;
    let network_id = inspect_owned_network(runner, spec.identity())?.with_context(|| {
        format!(
            "worker network {} disappeared before DinD provisioning",
            spec.identity().network()
        )
    })?;
    validate_network_spec(runner, spec.identity(), &network_id)?;
    let name = spec.identity().dind_container();
    if let Some(existing_id) = inspect_owned_container(runner, spec.identity(), ROLE_DIND)? {
        if !validate_dind_runtime(runner, spec, &existing_id, &network_id)? {
            anyhow::bail!("DinD container {name} disappeared during spec validation");
        }
        lifecycle_event(DockerLifecycleEvent::CreateResolved(
            DockerCreateTarget::Dind,
        ))
        .context("resolve existing DinD create intent")?;
        // Idempotent start: starting a running container succeeds, starting
        // a stopped owned container resumes it. Address the inspected object
        // by immutable ID so a replacement cannot receive this start.
        let started = runner
            .run_timeout(
                "docker",
                &["start".to_string(), "--".to_string(), existing_id],
                DIND_INSPECT_OPERATION_TIMEOUT,
            )
            .with_context(|| format!("start DinD container {name}"))?;
        if started.code != 0 {
            anyhow::bail!(
                "start DinD container {name} exited {}: {}",
                started.code,
                started.stderr.trim()
            );
        }
        return Ok(DindProvision::Adopted);
    }

    lifecycle_event(DockerLifecycleEvent::BeforeCreateRequest(
        DockerCreateTarget::Dind,
    ))
    .context("persist DinD create intent")?;
    let created = runner.run("docker", &spec.create_args(&network_id));
    let created = match created {
        Ok(created) => created,
        Err(error) => return Err(error).with_context(|| format!("create DinD container {name}")),
    };
    if created.code != 0 {
        if docker_explicitly_rejected_create(&created.stderr) {
            settle_explicit_rejection(
                runner,
                spec.identity(),
                DockerCreateTarget::Dind,
                lifecycle_event,
                &format!("DinD container {name}"),
                &created,
            )?;
        }
        anyhow::bail!(
            "create DinD container {name} exited {}: {}",
            created.code,
            created.stderr.trim()
        );
    }
    let id = parse_container_id(&created.stdout)
        .with_context(|| format!("create DinD container {name} returned an empty id"))?;
    if !validate_owned_container_id(runner, spec.identity(), ROLE_DIND, &id)? {
        anyhow::bail!("DinD container {name} disappeared after create");
    }
    if !validate_dind_runtime(runner, spec, &id, &network_id)? {
        anyhow::bail!("DinD container {name} disappeared during spec validation");
    }
    lifecycle_event(DockerLifecycleEvent::CreateResolved(
        DockerCreateTarget::Dind,
    ))
    .context("record DinD create settlement")?;
    let started = runner
        .run_timeout(
            "docker",
            &["start".to_string(), "--".to_string(), id],
            DIND_INSPECT_OPERATION_TIMEOUT,
        )
        .with_context(|| format!("start DinD container {name}"))?;
    if started.code != 0 {
        anyhow::bail!(
            "start DinD container {name} exited {}: {}",
            started.code,
            started.stderr.trim()
        );
    }
    Ok(DindProvision::Created)
}

/// Single readiness probe: true when dockerd answers on the private socket.
///
/// A failing probe is NOT an error — the caller retries with its own
/// backoff, then fails the worker when the deadline passes. Only a
/// transport failure (the `docker exec` itself could not run) errors.
pub(crate) fn dind_ready(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    dind_id: &str,
) -> Result<bool> {
    dind_ready_with_timeout(runner, spec, dind_id, DIND_READY_OPERATION_TIMEOUT)
}

/// Probe dockerd's private socket with one bounded Docker operation.
pub(crate) fn dind_ready_with_timeout(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    dind_id: &str,
    timeout: Duration,
) -> Result<bool> {
    let Some(timeout) = readiness_probe_timeout(timeout) else {
        return Ok(false);
    };
    let probe = runner
        .run_timeout("docker", &spec.probe_args(dind_id), timeout)
        .context("probe DinD readiness")?;
    if probe.code != 0 {
        return Ok(false);
    }
    Ok(parse_probe_output(&probe.stdout))
}

/// How long the supervisor waits for first readiness before failing.
pub const DIND_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// No single CLI operation may monopolize the full readiness budget.
pub const DIND_READY_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Total readiness budget for a post-restart dockerd recovery.
pub const DIND_RESTART_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// Interval between readiness probes while provisioning.
pub const DIND_READY_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Bound one probe by both the operation timeout and the remaining total
/// readiness interval. Zero means the total deadline has expired.
pub(crate) fn readiness_probe_timeout(remaining: Duration) -> Option<Duration> {
    if remaining.is_zero() {
        None
    } else {
        Some(remaining.min(DIND_READY_OPERATION_TIMEOUT))
    }
}

/// What [`ensure_network`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProvision {
    /// A network with matching ownership labels was adopted.
    Adopted,
    /// A fresh per-worker network was created.
    Created,
}

/// `docker network create` argv for the per-worker bridge.
///
/// The network carries the ownership labels (no role label: it belongs
/// to the pair, not one container) and publishes nothing — the runner
/// joins the DinD netns, so the bridge exists only to isolate the pair.
#[must_use]
pub fn network_create_args(identity: &WorkerIdentity) -> Vec<String> {
    let mut args = vec![
        "network".to_string(),
        "create".to_string(),
        "--driver".to_string(),
        "bridge".to_string(),
    ];
    for (key, value) in identity.labels() {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push("--".to_string());
    args.push(identity.network());
    args
}

/// Ensure the per-worker network exists, adopting on retry.
///
/// Missing → create; present → ownership labels must match this worker.
pub(crate) fn ensure_network(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    lifecycle_event: &mut dyn FnMut(DockerLifecycleEvent) -> Result<()>,
) -> Result<NetworkProvision> {
    let name = identity.network();
    if let Some(network_id) = inspect_owned_network(runner, identity)? {
        validate_network_spec(runner, identity, &network_id)?;
        lifecycle_event(DockerLifecycleEvent::CreateResolved(
            DockerCreateTarget::Network,
        ))
        .context("resolve existing network create intent")?;
        return Ok(NetworkProvision::Adopted);
    }

    lifecycle_event(DockerLifecycleEvent::BeforeCreateRequest(
        DockerCreateTarget::Network,
    ))
    .context("persist network create intent")?;
    let created = runner.run("docker", &network_create_args(identity));
    let created = match created {
        Ok(created) => created,
        Err(error) => return Err(error).with_context(|| format!("create worker network {name}")),
    };
    if created.code != 0 {
        if docker_explicitly_rejected_create(&created.stderr) {
            settle_explicit_rejection(
                runner,
                identity,
                DockerCreateTarget::Network,
                lifecycle_event,
                &format!("worker network {name}"),
                &created,
            )?;
        }
        anyhow::bail!(
            "create worker network {name} exited {}: {}",
            created.code,
            created.stderr.trim()
        );
    }
    let id = parse_container_id(&created.stdout)
        .with_context(|| format!("create worker network {name} returned an empty id"))?;
    if !validate_owned_network_id(runner, identity, &id)? {
        anyhow::bail!("worker network {name} disappeared after create");
    }
    validate_network_spec(runner, identity, &id)?;
    lifecycle_event(DockerLifecycleEvent::CreateResolved(
        DockerCreateTarget::Network,
    ))
    .context("record network create settlement")?;
    Ok(NetworkProvision::Created)
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
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::VecDeque;

    const DIND_REF: &str =
        "docker@sha256:2a232a42256f70d78e3cc5d2b5d6b3276710a0de0596c145f627ecfae90282ac";
    const RUNNER_REF: &str = "ghcr.io/actions/actions-runner@sha256:e5496277be5d09bc968b3d64911b74e219ac4a3f2edce956a3ecf9271bea1ef4";

    fn spec() -> DindSpec {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        DindSpec::new(
            identity,
            PinnedImage::parse(DIND_REF).unwrap(),
            Path::new("/tmp/velnor-test-dind-state"),
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("velnor-dind-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
        timeouts: Vec<Duration>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
                timeouts: Vec::new(),
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

        fn run_timeout(
            &mut self,
            program: &str,
            args: &[String],
            timeout: Duration,
        ) -> Result<WorkerOutput> {
            self.timeouts.push(timeout);
            self.run(program, args)
        }
    }

    fn owned_container_inspect(identity: &WorkerIdentity, id: &str, role: &str) -> String {
        let mut labels = identity.labels();
        labels.insert(WORKER_ROLE_LABEL.to_string(), role.to_string());
        format!("{id}\n{}", serde_json::to_string(&labels).unwrap())
    }

    fn owned_network_inspect(identity: &WorkerIdentity, id: &str) -> String {
        format!(
            "{id}\n{}",
            serde_json::to_string(&identity.labels()).unwrap()
        )
    }

    fn network_snapshot(identity: &WorkerIdentity, id: &str) -> String {
        serde_json::json!({
            "Id": id,
            "Name": identity.network(),
            "Labels": identity.labels(),
            "Driver": "bridge",
            "Scope": "local",
            "Internal": false,
            "Attachable": false,
            "Ingress": false,
            "ConfigOnly": false,
            "EnableIPv6": false,
            "Options": null,
            "IPAM": {
                "Driver": "default",
                "Options": null,
                "Config": [{"Subnet": "172.30.0.0/16", "IPRange": "", "Gateway": "172.30.0.1"}]
            }
        })
        .to_string()
    }

    fn volume_inspect(identity: &WorkerIdentity, target: DockerCreateTarget) -> String {
        let volume = volume_for_target(target).unwrap();
        let name = volume.name(identity);
        volume_inspect_labels(&name, volume_labels(identity, volume))
    }

    fn volume_inspect_labels(name: &str, labels: BTreeMap<String, String>) -> String {
        format!("{name}\n{}", serde_json::to_string(&labels).unwrap())
    }

    fn volume_snapshot(identity: &WorkerIdentity, target: DockerCreateTarget) -> String {
        let volume = volume_for_target(target).unwrap();
        serde_json::json!({
            "Name": volume.name(identity),
            "Driver": "local",
            "Options": null,
            "Labels": volume_labels(identity, volume)
        })
        .to_string()
    }

    fn dind_runtime_fixture(
        spec: &DindSpec,
        id: &str,
        network_id: &str,
    ) -> (WorkerOutput, WorkerOutput, WorkerOutput) {
        let labels = expected_image_labels(
            &serde_json::json!({"Labels": null}),
            spec.identity(),
            ROLE_DIND,
        )
        .unwrap();
        let state_dir = std::fs::canonicalize(spec.state_dir()).unwrap();
        let container = serde_json::json!({
            "Id": id,
            "Image": "sha256:dind-image",
            "Name": format!("/{}", spec.identity().dind_container()),
            "Config": {
                "Image": spec.image().reference(),
                "Labels": labels,
                "Env": ["DOCKER_TLS_CERTDIR="],
                "Cmd": [format!("-H unix://{DIND_SOCKET}")],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null
            },
            "HostConfig": {
                "NetworkMode": network_id,
                "Privileged": true,
                "PortBindings": null,
                "CapAdd": null,
                "CapDrop": null,
                "Devices": null,
                "SecurityOpt": null,
                "AutoRemove": false,
                "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
                "PublishAllPorts": false,
                "ReadonlyRootfs": false
            },
            "Mounts": [
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/data/_data",
                    "Destination": DIND_DATA_ROOT,
                    "Name": spec.identity().dind_data_volume(),
                    "RW": true
                },
                {
                    "Type": "bind",
                    "Source": state_dir,
                    "Destination": STATE_MOUNT,
                    "RW": true
                }
            ],
            "State": {"Running": false},
            "NetworkSettings": {
                "Networks": {
                    (spec.identity().network()): {"NetworkID": network_id}
                }
            }
        });
        let image = serde_json::json!({
            "Id": "sha256:dind-image",
            "Config": {
                "Labels": null,
                "Env": [],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null,
                "Cmd": null
            }
        });
        (
            ScriptRunner::ok(&format!("{id}\n")),
            ScriptRunner::ok(&container.to_string()),
            ScriptRunner::ok(&image.to_string()),
        )
    }

    fn push_volume_adoption(
        results: &mut Vec<WorkerOutput>,
        identity: &WorkerIdentity,
        target: DockerCreateTarget,
    ) {
        let volume = volume_for_target(target).unwrap();
        let name = volume.name(identity);
        results.push(ScriptRunner::ok(&volume_inspect(identity, target)));
        results.push(ScriptRunner::ok(&format!("{name}\n")));
        results.push(ScriptRunner::ok(&volume_snapshot(identity, target)));
    }

    fn push_network_validation(
        results: &mut Vec<WorkerOutput>,
        identity: &WorkerIdentity,
        network_id: &str,
    ) {
        results.push(ScriptRunner::ok(&owned_network_inspect(
            identity, network_id,
        )));
        results.push(ScriptRunner::ok(&format!("{network_id}\n")));
        results.push(ScriptRunner::ok(&network_snapshot(identity, network_id)));
    }

    fn push_dind_runtime(
        results: &mut Vec<WorkerOutput>,
        spec: &DindSpec,
        id: &str,
        network_id: &str,
    ) {
        let (claims, container, image) = dind_runtime_fixture(spec, id, network_id);
        results.extend([claims, container, image]);
    }

    fn runner_runtime_fixture(
        identity: &WorkerIdentity,
        runner_image: &PinnedImage,
        state_dir: &Path,
        runner_id: &str,
        dind_id: &str,
        jit_config: &str,
    ) -> (WorkerOutput, WorkerOutput, WorkerOutput) {
        let labels =
            expected_image_labels(&serde_json::json!({"Labels": null}), identity, ROLE_RUNNER)
                .unwrap();
        let state_dir = std::fs::canonicalize(state_dir).unwrap();
        let cache_dir = std::fs::canonicalize(state_dir.join("buildkit-cache")).unwrap();
        let workspace = identity.workspace_volume();
        let container = serde_json::json!({
            "Id": runner_id,
            "Image": "sha256:runner-image",
            "Name": format!("/{}", identity.runner_container()),
            "Config": {
                "Image": runner_image.reference(),
                "Labels": labels,
                "Env": [
                    format!("{}={}", super::super::runner::RUNNER_NAME_ENV, identity.ownership().runner_name()),
                    format!("DOCKER_HOST=unix://{DIND_SOCKET}"),
                    format!("RUNNER_WORK_FOLDER={}", super::super::runner::RUNNER_WORK_DIR),
                    format!("{}={jit_config}", super::super::runner::JIT_CONFIG_ENV)
                ],
                "Cmd": null,
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null
            },
            "HostConfig": {
                "NetworkMode": format!("container:{dind_id}"),
                "Privileged": false,
                "PortBindings": null,
                "CapAdd": null,
                "CapDrop": null,
                "Devices": null,
                "SecurityOpt": null,
                "AutoRemove": false,
                "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
                "PublishAllPorts": false,
                "ReadonlyRootfs": false
            },
            "Mounts": [
                {
                    "Type": "bind",
                    "Source": state_dir,
                    "Destination": STATE_MOUNT,
                    "RW": true
                },
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/workspace/_data",
                    "Destination": super::super::runner::RUNNER_WORK_DIR,
                    "Name": workspace,
                    "RW": true
                },
                {
                    "Type": "volume",
                    "Source": "/var/lib/docker/volumes/workspace/_data",
                    "Destination": super::super::runner::TOOL_CACHE_DIR,
                    "Name": identity.workspace_volume(),
                    "RW": true
                },
                {
                    "Type": "bind",
                    "Source": cache_dir,
                    "Destination": BUILDKIT_CACHE_DIR,
                    "RW": true
                }
            ],
            "State": {"Running": false},
            "NetworkSettings": {"Networks": {}}
        });
        let image = serde_json::json!({
            "Id": "sha256:runner-image",
            "Config": {
                "Labels": null,
                "Env": [],
                "Entrypoint": null,
                "User": "",
                "WorkingDir": "",
                "ExposedPorts": null,
                "Volumes": null,
                "StopSignal": null,
                "Healthcheck": null,
                "Shell": null,
                "Cmd": null
            }
        });
        (
            ScriptRunner::ok(&format!("{runner_id}\n")),
            ScriptRunner::ok(&container.to_string()),
            ScriptRunner::ok(&image.to_string()),
        )
    }

    fn missing_container(name: &str) -> WorkerOutput {
        ScriptRunner::fail(1, &format!("Error: No such container: {name}"))
    }

    fn missing_network(name: &str) -> WorkerOutput {
        ScriptRunner::fail(1, &format!("Error: No such network: {name}"))
    }

    fn missing_volume(name: &str) -> WorkerOutput {
        ScriptRunner::fail(1, &format!("Error: No such volume: {name}"))
    }

    #[test]
    fn create_argv_has_no_tcp_surface() {
        let args = spec().create_args("network-id");
        for forbidden in ["-p", "--publish", "--expose", "-P", "--publish-all"] {
            assert!(
                !args.iter().any(|arg| arg == forbidden),
                "DinD argv must not carry {forbidden}: {args:?}"
            );
        }
        // dockerd gets exactly one listener, and it is the Unix socket.
        let listeners: Vec<_> = args
            .iter()
            .filter(|arg| arg.starts_with("-H"))
            .cloned()
            .collect();
        assert_eq!(listeners, [format!("-H unix://{DIND_SOCKET}")]);
        assert!(!args.iter().any(|arg| arg.contains("tcp://")), "{args:?}");
    }

    #[test]
    fn create_argv_binds_no_host_socket() {
        let args = spec().create_args("network-id");
        assert!(
            !args.iter().any(|arg| arg.contains("/var/run/docker.sock")),
            "{args:?}"
        );
        // The state bind is the worker's own dir at the identical path.
        assert!(args.contains(&format!("/tmp/velnor-test-dind-state:{STATE_MOUNT}")));
        // Privileged is explicit (DinD requirement), pinned image, ownership labels.
        assert!(args.contains(&"--privileged".to_string()));
        assert!(args.contains(&DIND_REF.to_string()));
        let network_index = args.iter().position(|arg| arg == "--network").unwrap();
        assert_eq!(args[network_index + 1], "network-id");
        assert!(args
            .iter()
            .any(|arg| arg.contains("velnor.scaleset.ownership=")));
    }

    #[test]
    fn probe_targets_the_private_socket() {
        let args = spec().probe_args("dind-id");
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "dind-id");
        assert!(args.contains(&format!("unix://{DIND_SOCKET}")));
        assert!(!args.iter().any(|arg| arg.contains("tcp://")));
    }

    #[test]
    fn probe_output_parses() {
        assert!(parse_probe_output("28.5.2\n"));
        assert!(!parse_probe_output(""));
        assert!(!parse_probe_output("  \n"));
    }

    #[test]
    fn missing_container_is_created_and_started() {
        let dir = temp_dir("created");
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut results = vec![
            missing_volume(&spec.identity().dind_data_volume()),
            ScriptRunner::ok(""),
            ScriptRunner::ok(&spec.identity().dind_data_volume()),
        ];
        push_volume_adoption(
            &mut results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut results, spec.identity(), "network-id");
        results.extend([
            missing_container(&spec.identity().dind_container()),
            ScriptRunner::ok("deadbeef\n"),
            ScriptRunner::ok(&owned_container_inspect(
                spec.identity(),
                "deadbeef",
                ROLE_DIND,
            )),
        ]);
        push_dind_runtime(&mut results, &spec, "deadbeef", "network-id");
        results.push(ScriptRunner::ok("started\n"));
        let mut runner = ScriptRunner::scripted(results);
        let provision = ensure_dind(&mut runner, &spec, &mut |_| Ok(())).unwrap();
        assert_eq!(provision, DindProvision::Created);
        assert_eq!(runner.seen.len(), 16);
        assert_eq!(runner.seen[2][..2], ["volume", "create"]);
        assert_eq!(runner.seen[10][0], "create");
        assert_eq!(
            runner.seen[15],
            ["start", "--", "deadbeef"],
            "start must target Docker's returned immutable ID"
        );
        assert!(dir.join("buildkit-cache").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn docker_create_intents_name_the_resource_and_ambiguous_results_stay_open() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-intent-{}", uuid::Uuid::new_v4()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut dind_results = Vec::new();
        push_volume_adoption(
            &mut dind_results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut dind_results, spec.identity(), "network-id");
        dind_results.extend([
            missing_container(&spec.identity().dind_container()),
            ScriptRunner::fail(1, "error during connect: connection reset"),
        ]);
        let mut dind_runner = ScriptRunner::scripted(dind_results);
        let mut dind_events = Vec::new();
        assert!(ensure_dind(&mut dind_runner, &spec, &mut |event| {
            dind_events.push(event);
            Ok(())
        })
        .is_err());
        assert_eq!(
            dind_events,
            [
                DockerLifecycleEvent::CreateResolved(DockerCreateTarget::DindDataVolume),
                DockerLifecycleEvent::BeforeCreateRequest(DockerCreateTarget::Dind),
            ]
        );

        let mut network_runner = ScriptRunner::scripted(vec![
            missing_network(&spec.identity().network()),
            ScriptRunner::fail(1, "error during connect: connection reset"),
        ]);
        let mut network_events = Vec::new();
        assert!(
            ensure_network(&mut network_runner, spec.identity(), &mut |event| {
                network_events.push(event);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(
            network_events,
            [DockerLifecycleEvent::BeforeCreateRequest(
                DockerCreateTarget::Network
            )]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unavailable_daemon_is_not_misread_as_a_missing_object() {
        let dir =
            std::env::temp_dir().join(format!("velnor-dind-unavailable-{}", uuid::Uuid::new_v4()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut dind_runner = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "error during connect: no such file or directory",
        )]);
        assert!(ensure_dind(&mut dind_runner, &spec, &mut |_| Ok(())).is_err());
        assert_eq!(dind_runner.seen.len(), 1);

        let mut network_runner = ScriptRunner::scripted(vec![ScriptRunner::fail(
            1,
            "error during connect: no such file or directory",
        )]);
        assert!(ensure_network(&mut network_runner, spec.identity(), &mut |_| Ok(())).is_err());
        assert_eq!(network_runner.seen.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_container_is_adopted_and_started() {
        let dir = temp_dir("adopt");
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut results = Vec::new();
        push_volume_adoption(
            &mut results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut results, spec.identity(), "network-id");
        results.push(ScriptRunner::ok(&owned_container_inspect(
            spec.identity(),
            "deadbeef",
            ROLE_DIND,
        )));
        push_dind_runtime(&mut results, &spec, "deadbeef", "network-id");
        results.push(ScriptRunner::ok("started\n"));
        let mut runner = ScriptRunner::scripted(results);
        let provision = ensure_dind(&mut runner, &spec, &mut |_| Ok(())).unwrap();
        assert_eq!(provision, DindProvision::Adopted);
        assert!(!runner.seen.iter().any(|argv| argv[0] == "create"));
        assert_eq!(runner.seen.last().unwrap(), &["start", "--", "deadbeef"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn foreign_container_fails_closed() {
        let dir = temp_dir("foreign");
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut foreign_labels = spec.identity().labels();
        foreign_labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
        foreign_labels.insert(OWNERSHIP_LABEL.to_string(), "7/someone-else".to_string());
        let mut results = Vec::new();
        push_volume_adoption(
            &mut results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut results, spec.identity(), "network-id");
        results.push(ScriptRunner::ok(&format!(
            "deadbeef\n{}",
            serde_json::to_string(&foreign_labels).unwrap()
        )));
        let mut runner = ScriptRunner::scripted(results);
        let error = ensure_dind(&mut runner, &spec, &mut |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
        assert_eq!(
            runner.seen.len(),
            7,
            "foreign container must not be started"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn container_adoption_requires_every_expected_identity_label() {
        let worker = spec().identity().clone();
        let expected = owned_container_inspect(&worker, "deadbeef", ROLE_DIND);
        assert_eq!(
            inspect_owned_container(
                &mut ScriptRunner::scripted(vec![ScriptRunner::ok(&expected)]),
                &worker,
                ROLE_DIND,
            )
            .unwrap()
            .as_deref(),
            Some("deadbeef")
        );

        let mut missing_owner = worker.labels();
        missing_owner.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
        missing_owner.remove(OWNERSHIP_LABEL);
        let mut wrong_role = worker.labels();
        wrong_role.insert(WORKER_ROLE_LABEL.to_string(), ROLE_RUNNER.to_string());
        let mut wrong_runner = worker.labels();
        wrong_runner.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
        wrong_runner.insert(
            super::super::ownership::RUNNER_LABEL.to_string(),
            "another-runner".to_string(),
        );

        for labels in [
            {
                let mut labels = worker.labels();
                labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
                labels.insert(OWNERSHIP_LABEL.to_string(), "8/foreign".to_string());
                labels
            },
            missing_owner,
            wrong_role,
            wrong_runner,
        ] {
            let stdout = format!("deadbeef\n{}", serde_json::to_string(&labels).unwrap());
            let error = inspect_owned_container(
                &mut ScriptRunner::scripted(vec![ScriptRunner::ok(&stdout)]),
                &worker,
                ROLE_DIND,
            )
            .unwrap_err();
            assert!(error.to_string().contains("refusing to adopt"), "{error}");
        }
    }

    #[test]
    fn volume_inspection_requires_full_identity_kind_and_name_labels() {
        let worker = spec().identity().clone();
        let target = DockerCreateTarget::WorkspaceVolume;
        let expected = volume_inspect(&worker, target);
        assert_eq!(
            inspect_owned_volume(
                &mut ScriptRunner::scripted(vec![ScriptRunner::ok(&expected)]),
                &worker,
                target,
            )
            .unwrap()
            .as_deref(),
            Some(worker.workspace_volume().as_str())
        );

        let volume = volume_for_target(target).unwrap();
        let name = volume.name(&worker);
        let mut wrong_owner = volume_labels(&worker, volume);
        wrong_owner.insert(OWNERSHIP_LABEL.to_string(), "7/foreign".to_string());
        let mut missing_runner = volume_labels(&worker, volume);
        missing_runner.remove(super::super::ownership::RUNNER_LABEL);
        let mut wrong_kind = volume_labels(&worker, volume);
        wrong_kind.insert(WORKER_VOLUME_LABEL.to_string(), "dind-data".to_string());
        let mut wrong_name = volume_labels(&worker, volume);
        wrong_name.insert(
            WORKER_VOLUME_NAME_LABEL.to_string(),
            "other-volume".to_string(),
        );

        for labels in [wrong_owner, missing_runner, wrong_kind, wrong_name] {
            let error = inspect_owned_volume(
                &mut ScriptRunner::scripted(vec![ScriptRunner::ok(&volume_inspect_labels(
                    &name, labels,
                ))]),
                &worker,
                target,
            )
            .unwrap_err();
            assert!(error.to_string().contains("refusing to adopt"), "{error}");
        }
    }

    #[test]
    fn workspace_volume_adoption_rejects_duplicate_exact_label_claims() {
        let worker = spec().identity().clone();
        let target = DockerCreateTarget::WorkspaceVolume;
        let name = worker.workspace_volume();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok(&volume_inspect(&worker, target)),
            ScriptRunner::ok(&format!("{name}\nduplicate-volume\n")),
        ]);
        let mut events = Vec::new();
        let error = ensure_workspace_volume(&mut runner, &worker, &mut |event| {
            events.push(event);
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("ambiguous exact ownership labels"));
        assert!(
            events.is_empty(),
            "ambiguous resource must not settle intent"
        );
        assert!(!runner.seen.iter().any(|args| {
            args.first().is_some_and(|verb| verb == "volume")
                && args.get(1).is_some_and(|verb| verb == "create")
        }));

        let list_args = &runner.seen[1];
        for (key, value) in volume_labels(&worker, WorkerVolume::Workspace) {
            assert!(list_args
                .iter()
                .any(|arg| arg == &format!("label={key}={value}")));
        }
    }

    #[test]
    fn rejected_dind_and_network_creates_keep_intent_for_foreign_races() {
        let spec = spec();
        let foreign_dind_labels = {
            let mut labels = spec.identity().labels();
            labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
            labels.insert(OWNERSHIP_LABEL.to_string(), "7/foreign".to_string());
            labels
        };
        let mut dind_results = Vec::new();
        push_volume_adoption(
            &mut dind_results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut dind_results, spec.identity(), "network-id");
        dind_results.extend([
            missing_container(&spec.identity().dind_container()),
            ScriptRunner::fail(
                1,
                "Error response from daemon: Conflict. The container name is already in use",
            ),
            ScriptRunner::ok(&format!(
                "foreign-dind-id\n{}",
                serde_json::to_string(&foreign_dind_labels).unwrap()
            )),
        ]);
        let mut dind = ScriptRunner::scripted(dind_results);
        let mut dind_events = Vec::new();
        let error = ensure_dind(&mut dind, &spec, &mut |event| {
            dind_events.push(event);
            Ok(())
        })
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("foreign ownership"),
            "{error:#}"
        );
        assert!(!dind_events.contains(&DockerLifecycleEvent::CreateResolved(
            DockerCreateTarget::Dind
        )));
        assert!(
            dind_events.contains(&DockerLifecycleEvent::BeforeCreateRequest(
                DockerCreateTarget::Dind
            ))
        );
        assert!(dind.seen.iter().all(|args| args
            .first()
            .is_none_or(|verb| verb != "start" && verb != "rm")));

        let mut foreign_network_labels = spec.identity().labels();
        foreign_network_labels.insert(OWNERSHIP_LABEL.to_string(), "7/foreign".to_string());
        let mut network = ScriptRunner::scripted(vec![
            missing_network(&spec.identity().network()),
            ScriptRunner::fail(1, "Error response from daemon: network name already exists"),
            ScriptRunner::ok(&format!(
                "foreign-network-id\n{}",
                serde_json::to_string(&foreign_network_labels).unwrap()
            )),
        ]);
        let mut network_events = Vec::new();
        let error = ensure_network(&mut network, spec.identity(), &mut |event| {
            network_events.push(event);
            Ok(())
        })
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("foreign ownership"),
            "{error:#}"
        );
        assert!(
            !network_events.contains(&DockerLifecycleEvent::CreateResolved(
                DockerCreateTarget::Network
            ))
        );
        assert_eq!(
            network_events,
            [DockerLifecycleEvent::BeforeCreateRequest(
                DockerCreateTarget::Network
            )]
        );
        assert!(network.seen.iter().all(|args| args
            .first()
            .is_none_or(|verb| verb != "start" && verb != "rm")));
    }

    #[test]
    fn unlabeled_container_fails_closed() {
        let dir = temp_dir("unlabeled");
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut results = Vec::new();
        push_volume_adoption(
            &mut results,
            spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut results, spec.identity(), "network-id");
        results.push(ScriptRunner::ok("deadbeef\nnull"));
        let mut runner = ScriptRunner::scripted(results);
        let error = ensure_dind(&mut runner, &spec, &mut |_| Ok(())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no velnor.scaleset.ownership label"),
            "{error}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn network_argv_is_a_labeled_bridge_without_publish() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let args = network_create_args(&identity);
        assert_eq!(args[0], "network");
        assert_eq!(args[1], "create");
        assert!(args.contains(&"velnor-scaleset-net-s7-velnor-set-0007-2ad92676".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.contains("velnor.scaleset.ownership=7/velnor-set-0007")));
        for forbidden in [
            "-p",
            "--publish",
            "--expose",
            "--subnet",
            "--gateway",
            "--ip-range",
            "--aux-address",
            "--ipam-driver",
            "--ipam-opt",
        ] {
            assert!(!args.iter().any(|arg| arg == forbidden), "{args:?}");
        }
    }

    #[test]
    fn network_is_created_then_adopted() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let mut created_results = vec![
            missing_network(&identity.network()),
            ScriptRunner::ok("netid\n"),
        ];
        push_network_validation(&mut created_results, &identity, "netid");
        let mut runner = ScriptRunner::scripted(created_results);
        assert_eq!(
            ensure_network(&mut runner, &identity, &mut |_| Ok(())).unwrap(),
            NetworkProvision::Created
        );
        let mut adopted_results = Vec::new();
        push_network_validation(&mut adopted_results, &identity, "netid");
        let mut runner = ScriptRunner::scripted(adopted_results);
        assert_eq!(
            ensure_network(&mut runner, &identity, &mut |_| Ok(())).unwrap(),
            NetworkProvision::Adopted
        );
    }

    #[test]
    fn network_adoption_rejects_duplicate_or_changed_spec() {
        let identity = spec().identity().clone();
        let mut duplicate =
            ScriptRunner::scripted(vec![ScriptRunner::ok("network-id\nduplicate-network-id\n")]);
        let error = validate_network_spec(&mut duplicate, &identity, "network-id").unwrap_err();
        assert!(error.to_string().contains("duplicate"), "{error:#}");
        assert_eq!(duplicate.seen.len(), 1);

        for (field, mutation) in [
            ("driver", ("Driver", serde_json::json!("overlay"))),
            (
                "options",
                (
                    "Options",
                    serde_json::json!({"com.docker.network.bridge.enable_icc": "false"}),
                ),
            ),
            ("malformed options", ("Options", serde_json::json!([]))),
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(&network_snapshot(&identity, "network-id")).unwrap();
            value[mutation.0] = mutation.1;
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("network-id\n"),
                ScriptRunner::ok(&value.to_string()),
            ]);
            let error = validate_network_spec(&mut runner, &identity, "network-id").unwrap_err();
            assert!(
                format!("{error:#}").contains("mismatched")
                    || format!("{error:#}").contains("unexpected"),
                "{field}: {error:#}"
            );
        }

        let mut generated = ScriptRunner::scripted(vec![
            ScriptRunner::ok("network-id\n"),
            ScriptRunner::ok(&network_snapshot(&identity, "network-id")),
        ]);
        validate_network_spec(&mut generated, &identity, "network-id").unwrap();

        for (field, mutate) in [
            (
                "noncanonical subnet",
                Box::new(|config: &mut serde_json::Value| {
                    config["Subnet"] = serde_json::json!("172.30.1.0/16");
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "invalid subnet prefix",
                Box::new(|config: &mut serde_json::Value| {
                    config["Subnet"] = serde_json::json!("172.30.0.0/33");
                }),
            ),
            (
                "noncanonical subnet prefix",
                Box::new(|config: &mut serde_json::Value| {
                    config["Subnet"] = serde_json::json!("172.30.0.0/016");
                }),
            ),
            (
                "gateway outside subnet",
                Box::new(|config: &mut serde_json::Value| {
                    config["Gateway"] = serde_json::json!("172.31.0.1");
                }),
            ),
            (
                "gateway is subnet address",
                Box::new(|config: &mut serde_json::Value| {
                    config["Gateway"] = serde_json::json!("172.30.0.0");
                }),
            ),
            (
                "gateway is not the default first address",
                Box::new(|config: &mut serde_json::Value| {
                    config["Gateway"] = serde_json::json!("172.30.0.2");
                }),
            ),
            (
                "custom IP range",
                Box::new(|config: &mut serde_json::Value| {
                    config["IPRange"] = serde_json::json!("172.30.1.0/24");
                }),
            ),
            (
                "auxiliary address",
                Box::new(|config: &mut serde_json::Value| {
                    config["AuxiliaryAddresses"] = serde_json::json!({"host": "172.30.0.2"});
                }),
            ),
            (
                "custom route",
                Box::new(|config: &mut serde_json::Value| {
                    config["Routes"] = serde_json::json!([{"Destination": "0.0.0.0/0"}]);
                }),
            ),
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(&network_snapshot(&identity, "network-id")).unwrap();
            mutate(&mut value["IPAM"]["Config"][0]);
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("network-id\n"),
                ScriptRunner::ok(&value.to_string()),
            ]);
            let error = validate_network_spec(&mut runner, &identity, "network-id").unwrap_err();
            assert!(format!("{error:#}").contains("IPAM"), "{field}: {error:#}");
        }

        for (field, mutate) in [
            (
                "nondefault IPAM driver",
                Box::new(|ipam: &mut serde_json::Value| {
                    ipam["Driver"] = serde_json::json!("custom");
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "custom IPAM options",
                Box::new(|ipam: &mut serde_json::Value| {
                    ipam["Options"] = serde_json::json!({"route": "10.0.0.0/8"});
                }),
            ),
            (
                "malformed IPAM options",
                Box::new(|ipam: &mut serde_json::Value| {
                    ipam["Options"] = serde_json::json!([]);
                }),
            ),
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(&network_snapshot(&identity, "network-id")).unwrap();
            mutate(&mut value["IPAM"]);
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("network-id\n"),
                ScriptRunner::ok(&value.to_string()),
            ]);
            let error = validate_network_spec(&mut runner, &identity, "network-id").unwrap_err();
            assert!(format!("{error:#}").contains("IPAM"), "{field}: {error:#}");
        }
    }

    #[test]
    fn volume_adoption_rejects_changed_driver_and_options() {
        let identity = spec().identity().clone();
        let target = DockerCreateTarget::DindDataVolume;
        let volume = volume_for_target(target).unwrap();
        for (field, mutation) in [
            ("driver", ("Driver", serde_json::json!("nfs"))),
            ("options", ("Options", serde_json::json!({"type": "nfs"}))),
            ("malformed options", ("Options", serde_json::json!([]))),
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(&volume_snapshot(&identity, target)).unwrap();
            value[mutation.0] = mutation.1;
            let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(&value.to_string())]);
            let error =
                validate_volume_spec(&mut runner, &identity, volume, &identity.dind_data_volume())
                    .unwrap_err();
            assert!(
                format!("{error:#}").contains("mismatched")
                    || format!("{error:#}").contains("options"),
                "{field}: {error:#}"
            );
        }
    }

    #[test]
    fn foreign_network_fails_closed() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let mut foreign_labels = identity.labels();
        foreign_labels.insert(OWNERSHIP_LABEL.to_string(), "7/stranger".to_string());
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok(&format!(
            "netid\n{}",
            serde_json::to_string(&foreign_labels).unwrap()
        ))]);
        let error = ensure_network(&mut runner, &identity, &mut |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
    }

    #[test]
    fn network_adoption_requires_all_worker_identity_labels() {
        let worker = spec().identity().clone();
        let mut wrong_runner = worker.labels();
        wrong_runner.insert(
            super::super::ownership::RUNNER_LABEL.to_string(),
            "another-runner".to_string(),
        );
        let mut missing_set = worker.labels();
        missing_set.remove(super::super::ownership::SCALE_SET_LABEL);
        let mut wrong_set = worker.labels();
        wrong_set.insert(
            super::super::ownership::SCALE_SET_LABEL.to_string(),
            "8".to_string(),
        );

        for labels in [wrong_runner, missing_set, wrong_set] {
            let error = inspect_owned_network_ref_output(&worker, labels);
            assert!(
                format!("{error:#}").contains("refusing to adopt"),
                "{error:#}"
            );
        }
    }

    fn inspect_owned_network_ref_output(
        worker: &WorkerIdentity,
        labels: BTreeMap<String, String>,
    ) -> anyhow::Error {
        inspect_owned_network(
            &mut ScriptRunner::scripted(vec![ScriptRunner::ok(&format!(
                "network-id\n{}",
                serde_json::to_string(&labels).unwrap()
            ))]),
            worker,
        )
        .unwrap_err()
    }

    #[test]
    fn readiness_probe_failure_is_not_ready_not_error() {
        let spec = spec();
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(1, "Cannot connect")]);
        assert!(!dind_ready(&mut runner, &spec, "dind-id").unwrap());
        assert_eq!(runner.seen[0][..2], ["exec", "dind-id"]);

        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("28.5.2\n")]);
        assert!(dind_ready(&mut runner, &spec, "dind-id").unwrap());
    }

    #[test]
    fn adopted_dind_must_match_pinned_image_network_and_mounts() {
        let state_dir = temp_dir("runtime-spec");
        let mut dind_spec = spec();
        dind_spec.state_dir = state_dir.clone();
        std::fs::create_dir_all(dind_spec.host_cache_dir()).unwrap();
        let mutations: [(&str, fn(&mut serde_json::Value)); 4] = [
            ("image", |container| {
                container["Image"] = serde_json::json!("sha256:wrong")
            }),
            ("network", |container| {
                container["HostConfig"]["NetworkMode"] = serde_json::json!("shared-network-id");
                container["NetworkSettings"]["Networks"] = serde_json::json!({
                    "shared-network": {"NetworkID": "shared-network-id"}
                });
            }),
            ("mount", |container| {
                container["Mounts"][0]["Name"] = serde_json::json!("foreign-dind-data");
            }),
            ("restart policy", |container| {
                container["HostConfig"]["RestartPolicy"]["Name"] = serde_json::json!("always");
            }),
        ];
        for (field, mutate) in mutations {
            let (claims, mut container, image) =
                dind_runtime_fixture(&dind_spec, "dind-id", "network-id");
            let mut value: serde_json::Value = serde_json::from_str(&container.stdout).unwrap();
            mutate(&mut value);
            container.stdout = value.to_string();
            let mut runner = ScriptRunner::scripted(vec![claims, container, image]);
            let error = validate_dind_runtime(&mut runner, &dind_spec, "dind-id", "network-id")
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("mismatch")
                    || format!("{error:#}").contains("mount")
                    || format!("{error:#}").contains("wrong pinned image ID")
                    || format!("{error:#}").contains("restart/removal policy"),
                "{field}: {error:#}"
            );
            assert!(!runner.seen.iter().any(|args| {
                args.first()
                    .is_some_and(|arg| arg == "start" || arg == "rm")
            }));
        }
        std::fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn ensure_dind_rejects_misconfigured_owned_adoption_before_start() {
        let state_dir = temp_dir("adopt-wrong-network");
        let mut dind_spec = spec();
        dind_spec.state_dir = state_dir.clone();
        let mut results = Vec::new();
        push_volume_adoption(
            &mut results,
            dind_spec.identity(),
            DockerCreateTarget::DindDataVolume,
        );
        push_network_validation(&mut results, dind_spec.identity(), "network-id");
        results.push(ScriptRunner::ok(&owned_container_inspect(
            dind_spec.identity(),
            "dind-id",
            ROLE_DIND,
        )));
        let (claims, mut container, image) =
            dind_runtime_fixture(&dind_spec, "dind-id", "network-id");
        let mut container_json: serde_json::Value =
            serde_json::from_str(&container.stdout).unwrap();
        container_json["HostConfig"]["NetworkMode"] = serde_json::json!("shared-network-id");
        container_json["NetworkSettings"]["Networks"] = serde_json::json!({
            "shared-network": {"NetworkID": "shared-network-id"}
        });
        container.stdout = container_json.to_string();
        results.extend([claims, container, image]);
        let mut runner = ScriptRunner::scripted(results);
        let mut events = Vec::new();
        let error = ensure_dind(&mut runner, &dind_spec, &mut |event| {
            events.push(event);
            Ok(())
        })
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("network namespace identity"),
            "{error:#}"
        );
        assert!(!runner.seen.iter().any(|args| {
            args.first()
                .is_some_and(|verb| verb == "start" || verb == "restart")
        }));
        assert!(!events.contains(&DockerLifecycleEvent::CreateResolved(
            DockerCreateTarget::Dind
        )));
        std::fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn adopted_dind_rejects_duplicate_exact_label_claims() {
        let state_dir = temp_dir("runtime-duplicate");
        let mut dind_spec = spec();
        dind_spec.state_dir = state_dir.clone();
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("dind-id\nduplicate-id\n")]);
        let error =
            validate_dind_runtime(&mut runner, &dind_spec, "dind-id", "network-id").unwrap_err();
        assert!(error.to_string().contains("duplicate"), "{error:#}");
        assert_eq!(runner.seen.len(), 1);
        std::fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn adopted_runner_must_match_pinned_image_mounts_and_dind_namespace_id() {
        let state_dir = temp_dir("runner-runtime-spec");
        std::fs::create_dir_all(state_dir.join("buildkit-cache")).unwrap();
        let identity = spec().identity().clone();
        let runner_image = PinnedImage::parse(RUNNER_REF).unwrap();
        let mutations: [(&str, fn(&mut serde_json::Value)); 4] = [
            ("image", |container| {
                container["Image"] = serde_json::json!("sha256:wrong")
            }),
            ("namespace", |container| {
                container["HostConfig"]["NetworkMode"] =
                    serde_json::json!("container:replacement-dind-id");
            }),
            ("mount", |container| {
                container["Mounts"][1]["Name"] = serde_json::json!("foreign-workspace");
            }),
            ("restart policy", |container| {
                container["HostConfig"]["RestartPolicy"]["Name"] = serde_json::json!("always");
            }),
        ];
        for (field, mutate) in mutations {
            let (claims, mut container, image) = runner_runtime_fixture(
                &identity,
                &runner_image,
                &state_dir,
                "runner-id",
                "dind-id",
                "jit-blob",
            );
            let mut value: serde_json::Value = serde_json::from_str(&container.stdout).unwrap();
            mutate(&mut value);
            container.stdout = value.to_string();
            let mut runner = ScriptRunner::scripted(vec![claims, container, image]);
            let error = validate_runner_runtime(
                &mut runner,
                &identity,
                &runner_image,
                &state_dir,
                "runner-id",
                "dind-id",
                identity.ownership().runner_name(),
                Some("jit-blob"),
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("mismatch")
                    || format!("{error:#}").contains("mount")
                    || format!("{error:#}").contains("wrong pinned image ID")
                    || format!("{error:#}").contains("restart/removal policy"),
                "{field}: {error:#}"
            );
            assert!(!runner.seen.iter().any(|args| {
                args.first()
                    .is_some_and(|arg| arg == "start" || arg == "rm")
            }));
        }
        std::fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn readiness_probe_is_bounded_by_operation_and_total_deadline() {
        assert_eq!(
            readiness_probe_timeout(Duration::from_secs(30)),
            Some(DIND_READY_OPERATION_TIMEOUT)
        );
        assert_eq!(
            readiness_probe_timeout(Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(readiness_probe_timeout(Duration::ZERO), None);

        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("28.5.2\n")]);
        assert!(
            dind_ready_with_timeout(&mut runner, &spec(), "dind-id", Duration::from_secs(30),)
                .unwrap()
        );
        assert_eq!(runner.timeouts, [DIND_READY_OPERATION_TIMEOUT]);
    }
}

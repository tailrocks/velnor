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
//! an existing container with an exact attested identity is adopted, an
//! existing container with foreign or unsafe state fails closed (a name
//! collision outside our ownership must never be commandeered).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::docker::client::ContainerState;

use super::ownership::{WorkerIdentity, ROLE_DIND, ROLE_VOLUME_HOLDER, WORKER_ROLE_LABEL};
use super::runner::{PinnedImage, RUNNER_INDEX_DIGEST, RUNNER_REPOSITORY};
use super::{WorkerOutput, WorkerRunner};

/// Guest-absolute mount point of the per-worker state dir bind.
///
/// The state dir bind (`<state-dir>:/velnor/scaleset`) is mounted at this
/// identical absolute path in BOTH containers of a pair, so the socket path
/// below names the same file on both sides without any translation.
pub const STATE_MOUNT: &str = "/velnor/scaleset";
/// Guest-absolute private daemon socket (shared bind, same path both sides).
pub const DIND_SOCKET: &str = "/velnor/scaleset/dind.sock";
/// Numeric contract, independent of either image's group database. The
/// official runner image defines docker as GID 123 (actions/runner v2.337.0,
/// images/Dockerfile); supplementary membership is also explicit at create.
pub const DIND_SOCKET_GID: &str = "123";
pub const DIND_ENTRYPOINT: &str = "dockerd-entrypoint.sh";

/// Supplying `dockerd` explicitly bypasses the entrypoint's flag-only branch
/// which would add TCP 2375/2376 and a second Unix socket. Retain the upstream
/// init/iptables setup; do not bypass it by replacing the entrypoint with dockerd.
pub fn daemon_command() -> Vec<String> {
    vec![
        "dockerd".to_string(),
        format!("--host=unix://{DIND_SOCKET}"),
        format!("--group={DIND_SOCKET_GID}"),
    ]
}
/// Guest-absolute BuildKit cache dir on the shared bind (identical path
/// both sides; inner builds address `--cache-to/--cache-from
/// type=local` here).
pub const BUILDKIT_CACHE_DIR: &str = "/velnor/scaleset/buildkit-cache";
/// dockerd's data root inside the daemon container, supplied by the holder.
pub const DIND_DATA_ROOT: &str = "/var/lib/docker";
/// Guest-absolute workspace mount point (identical path in runner and DinD).
pub const WORK_DIR: &str = super::runner::RUNNER_WORK_DIR;
/// Re-export of [`super::runner::RUNNER_WORK_DIR`].
pub use super::runner::RUNNER_WORK_DIR;
/// Tool cache dir, identical absolute path in both containers of a pair.
pub use super::runner::TOOL_CACHE_DIR;

/// The holder command is never executed. It only gives Docker a lifecycle
/// object to own the three anonymous volumes.
pub const VOLUME_HOLDER_COMMAND: &str = "/bin/true";

/// Fully-derived anonymous-volume holder spec.
#[derive(Debug, Clone)]
pub struct VolumeHolderSpec {
    identity: WorkerIdentity,
    image: PinnedImage,
}

impl VolumeHolderSpec {
    #[must_use]
    pub fn new(identity: WorkerIdentity, image: PinnedImage) -> Self {
        Self { identity, image }
    }

    #[must_use]
    pub fn identity(&self) -> &WorkerIdentity {
        &self.identity
    }

    #[must_use]
    pub fn image(&self) -> &PinnedImage {
        &self.image
    }

    /// `docker create` argv for the never-started holder.
    ///
    /// No source/name is supplied for any mount. Docker therefore creates
    /// anonymous volumes and removes them with the holder's `rm --volumes`.
    #[must_use]
    pub fn create_args(&self) -> Vec<String> {
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            self.identity.volume_holder_container(),
            "--network".to_string(),
            "none".to_string(),
            "--entrypoint".to_string(),
            String::new(),
        ];
        for target in [WORK_DIR, TOOL_CACHE_DIR, DIND_DATA_ROOT] {
            args.push("--mount".to_string());
            args.push(volume_mount_arg(&self.identity, target));
        }
        args.extend(self.identity.label_args(ROLE_VOLUME_HOLDER));
        args.push("--".to_string());
        args.push(self.image.reference());
        args.push(VOLUME_HOLDER_COMMAND.to_string());
        args
    }
}

fn volume_mount_arg(identity: &WorkerIdentity, target: &str) -> String {
    let mut mount = format!("type=volume,target={target}");
    for (key, value) in identity.labels() {
        mount.push_str(",volume-label=");
        mount.push_str(&key);
        mount.push('=');
        mount.push_str(&value);
    }
    mount.push_str(",volume-label=");
    mount.push_str(WORKER_ROLE_LABEL);
    mount.push('=');
    mount.push_str(ROLE_VOLUME_HOLDER);
    mount
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
    /// * dockerd command line carries exactly one `-H unix://` listener;
    /// * workspace and tool cache volumes mounted at identical paths to the runner
    ///   (`/home/runner/_work` and `/opt/hostedtoolcache`).
    #[must_use]
    pub fn create_args(&self, network_id: &str, holder_id: &str) -> Vec<String> {
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
            "--entrypoint".to_string(),
            DIND_ENTRYPOINT.to_string(),
            "--volumes-from".to_string(),
            holder_id.to_string(),
            "--volume".to_string(),
            format!("{}:{STATE_MOUNT}", self.state_dir.display()),
        ];
        args.extend(self.identity.label_args(ROLE_DIND));
        args.extend([
            "--label".to_string(),
            format!(
                "{}={}",
                super::ownership::STATE_SOURCE_LABEL,
                self.state_dir.display()
            ),
        ]);
        args.push("--".to_string());
        args.push(self.image.reference().to_string());
        // dockerd with ONE listener: the private Unix socket. No
        // `tcp://` listener exists, so nothing can be published.
        args.extend(daemon_command());
        args
    }

    /// Readiness probe argv: `docker version` against the private socket
    /// from INSIDE the daemon container (proves dockerd serves the
    /// socket; the dind image ships the CLI).
    #[must_use]
    pub fn probe_args(&self, container_id: &str) -> Vec<String> {
        vec![
            "exec".to_string(),
            container_id.to_string(),
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
    /// An existing container passed exact identity and lifecycle attestation.
    Adopted,
    /// A fresh daemon container was created and started.
    Created,
}

/// What [`ensure_volume_holder`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeHolderProvision {
    /// An existing holder passed the exact image, labels, mounts, and state gate.
    Adopted,
    /// A new never-started holder was created and attested.
    Created,
}

/// The immutable handle captured by holder attestation. It is intentionally
/// not persisted: cleanup re-inspects the deterministic holder name immediately
/// before mutation and uses this ID for `rm --force --volumes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeHolderAttestation {
    pub id: String,
    pub mounts: Vec<super::ownership::Mount>,
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

/// Classify an adoption lookup without treating an empty or failed command
/// as proof that the object is absent.
///
/// Docker's positive missing answer is an exit failure whose stderr contains
/// `No such ...`. Every other nonzero result is an inspect failure, and a
/// zero exit with an empty projection is an invalid daemon answer.
fn adoption_inspect_is_missing(kind: &str, name: &str, inspect: &WorkerOutput) -> Result<bool> {
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return Ok(true);
        }
        anyhow::bail!(
            "inspect {kind} {name} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }
    if parse_container_id(&inspect.stdout).is_some() {
        return Ok(false);
    }
    anyhow::bail!("inspect {kind} {name} returned empty id")
}

const NETWORK_ATTEST_FORMAT: &str =
    r#"{{json .Id}}{{"\t"}}{{json .Driver}}{{"\t"}}{{json .Labels}}"#;
const DIND_ATTEST_FORMAT: &str = r#"{{json .Config.Image}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .HostConfig.NetworkMode}}{{"\t"}}{{json .HostConfig.VolumesFrom}}{{"\t"}}{{json .NetworkSettings.Networks}}{{"\t"}}{{json .State.Status}}"#;
const VOLUME_HOLDER_ATTEST_FORMAT: &str = r#"{{json .Id}}{{"\t"}}{{json .Config.Image}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .Config.Entrypoint}}{{"\t"}}{{json .Config.Cmd}}{{"\t"}}{{json .State.Status}}{{"\t"}}{{json .Mounts}}"#;

fn inspect_projection_fields<'a>(
    kind: &str,
    name: &str,
    inspect: &'a WorkerOutput,
    expected_fields: usize,
) -> Result<Vec<&'a str>> {
    if inspect.code != 0 {
        if crate::docker::client::daemon_reports_missing(&inspect.stderr) {
            return Err(super::RestartObjectMissing.into());
        }
        anyhow::bail!(
            "inspect {kind} {name} exited {}: {}",
            inspect.code,
            inspect.stderr.trim()
        );
    }

    let fields: Vec<_> = inspect.stdout.trim().split('\t').collect();
    if fields.len() != expected_fields || fields.iter().any(|field| field.is_empty()) {
        anyhow::bail!(
            "inspect {kind} {name} returned malformed projection: expected {expected_fields} fields"
        );
    }
    Ok(fields)
}

fn verify_attested_labels(
    kind: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    identity: &WorkerIdentity,
    role: Option<&str>,
) -> Result<()> {
    for (key, expected) in identity.labels() {
        if labels.get(&key) != Some(&expected) {
            anyhow::bail!(
                "{kind} {name} label {key} mismatch: expected {expected:?}, found {:?}",
                labels.get(&key)
            );
        }
    }
    if let Some(expected_role) = role
        && labels.get(WORKER_ROLE_LABEL).map(String::as_str) != Some(expected_role)
    {
        anyhow::bail!(
            "{kind} {name} role label mismatch: expected {expected_role:?}, found {:?}",
            labels.get(WORKER_ROLE_LABEL)
        );
    }
    Ok(())
}

/// Require the new worker topology's single deterministic volume-holder
/// source. An absent, named-legacy, direct-mount-only, or extra source cannot
/// prove that this worker inherited the holder's anonymous volumes.
pub(crate) fn verify_volume_holder_reference(
    kind: &str,
    name: &str,
    volumes_from: Option<&[String]>,
    holder_id: &str,
) -> Result<()> {
    let expected = [holder_id.to_string()];
    let explicit_rw = [format!("{holder_id}:rw")];
    if volumes_from != Some(expected.as_slice()) && volumes_from != Some(explicit_rw.as_slice()) {
        anyhow::bail!(
            "{kind} {name} has HostConfig.VolumesFrom {volumes_from:?}, expected exactly {:?}; refusing legacy named/direct volume wiring",
            expected
        );
    }
    Ok(())
}

/// Return the product's admitted runner image for cleanup-time attestation.
/// The homogeneous profile currently admits this exact index digest for every
/// supported platform; keeping construction here prevents cleanup from
/// accepting a tag or a mutable holder image.
pub(crate) fn admitted_runner_image() -> Result<PinnedImage> {
    PinnedImage::parse(&format!("{RUNNER_REPOSITORY}@{RUNNER_INDEX_DIGEST}"))
        .map_err(|error| anyhow::anyhow!("invalid admitted runner image: {error}"))
}

fn verify_volume_holder_mounts(name: &str, mounts: &[serde_json::Value]) -> Result<()> {
    let expected: BTreeSet<&str> = [WORK_DIR, TOOL_CACHE_DIR, DIND_DATA_ROOT]
        .into_iter()
        .collect();
    let mut destinations = BTreeSet::new();
    for mount in mounts {
        let object = mount
            .as_object()
            .with_context(|| format!("holder {name} returned a non-object mount"))?;
        let mount_type = object.get("Type").and_then(serde_json::Value::as_str);
        if mount_type != Some("volume") {
            anyhow::bail!("holder {name} has non-volume mount type {mount_type:?}");
        }
        let destination = object
            .get("Destination")
            .and_then(serde_json::Value::as_str)
            .context("holder mount omitted destination")?;
        if !expected.contains(destination) || !destinations.insert(destination) {
            anyhow::bail!("holder {name} has unexpected or duplicate mount {destination:?}");
        }
        let _volume_name = object
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .with_context(|| {
                format!("holder {name} mount {destination} has no Docker volume name")
            })?;
        if object.get("Driver").and_then(serde_json::Value::as_str) != Some("local") {
            anyhow::bail!("holder {name} mount {destination} is not a local volume");
        }
        if object.get("RW").and_then(serde_json::Value::as_bool) != Some(true) {
            anyhow::bail!("holder {name} mount {destination} is not read-write");
        }
    }
    if destinations != expected {
        anyhow::bail!(
            "holder {name} mount destinations mismatch: expected {expected:?}, found {destinations:?}"
        );
    }
    Ok(())
}

fn reject_existing_holder_dependents(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    holder_name: &str,
) -> Result<()> {
    for (kind, name) in [
        ("DinD container", identity.dind_container()),
        ("runner container", identity.runner_container()),
    ] {
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
            .with_context(|| format!("inspect {kind} before recreating holder {holder_name}"))?;
        if !adoption_inspect_is_missing(kind, &name, &inspect)? {
            anyhow::bail!(
                "refusing to recreate volume holder {holder_name}: dependent {kind} {name} still exists"
            );
        }
    }
    Ok(())
}

/// Inspect and attest the never-started anonymous-volume holder.
///
/// The projection excludes environment and host paths. It proves the exact
/// admitted runner image, ownership/role labels, empty entrypoint, `/bin/true`
/// command, `created` lifecycle, and exactly the three expected local volume
/// destinations. Missing is typed for restart recovery; every other malformed,
/// foreign, or transport result fails closed.
pub(crate) fn attest_volume_holder(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
) -> Result<VolumeHolderAttestation> {
    attest_volume_holder_at(
        runner,
        identity,
        expected_image,
        &identity.volume_holder_container(),
    )
}

fn attest_volume_holder_at(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
    target: &str,
) -> Result<VolumeHolderAttestation> {
    let name = identity.volume_holder_container();
    let inspect = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                VOLUME_HOLDER_ATTEST_FORMAT.to_string(),
                "--".to_string(),
                target.to_string(),
            ],
        )
        .with_context(|| format!("attest volume holder {name}"))?;
    let fields = inspect_projection_fields("volume holder", &name, &inspect, 7)?;
    let id: String = serde_json::from_str(fields[0])
        .with_context(|| format!("parse volume holder {name} id"))?;
    let image: String = serde_json::from_str(fields[1])
        .with_context(|| format!("parse volume holder {name} image"))?;
    let labels: BTreeMap<String, String> = serde_json::from_str(fields[2])
        .with_context(|| format!("parse volume holder {name} labels"))?;
    let entrypoint: Option<Vec<String>> = serde_json::from_str(fields[3])
        .with_context(|| format!("parse volume holder {name} entrypoint"))?;
    let command: Option<Vec<String>> = serde_json::from_str(fields[4])
        .with_context(|| format!("parse volume holder {name} command"))?;
    let status: String = serde_json::from_str(fields[5])
        .with_context(|| format!("parse volume holder {name} status"))?;
    let mounts: Vec<serde_json::Value> = serde_json::from_str(fields[6])
        .with_context(|| format!("parse volume holder {name} mounts"))?;

    if id.is_empty() {
        anyhow::bail!("volume holder {name} returned an empty id");
    }
    if target != name && id != target {
        anyhow::bail!("volume holder changed immutable identity during attestation");
    }
    if image != expected_image.reference() {
        anyhow::bail!(
            "volume holder {name} image mismatch: expected {:?}, found {image:?}",
            expected_image.reference()
        );
    }
    verify_attested_labels(
        "volume holder",
        &name,
        &labels,
        identity,
        Some(ROLE_VOLUME_HOLDER),
    )?;
    if entrypoint.unwrap_or_default() != Vec::<String>::new() {
        anyhow::bail!("volume holder {name} has a non-empty entrypoint");
    }
    if command != Some(vec![VOLUME_HOLDER_COMMAND.to_string()]) {
        anyhow::bail!(
            "volume holder {name} has command {command:?}, expected {:?}",
            [VOLUME_HOLDER_COMMAND]
        );
    }
    if ContainerState::parse(&status) != Some(ContainerState::Created) {
        anyhow::bail!("volume holder {name} has unsafe lifecycle status {status:?}");
    }
    verify_volume_holder_mounts(&name, &mounts)?;
    let mounts: Vec<super::ownership::Mount> =
        serde_json::from_value(serde_json::Value::Array(mounts)).map_err(|error| {
            eprintln!("temporary holder mount parse diagnostic: {error:#}");
            error
        })?;
    eprintln!("temporary holder mounts parsed: {mounts:?}");
    for mount in &mounts {
        let inspected = runner.run("docker", &[
            "volume".into(), "inspect".into(), "--format".into(),
            r#"{{json .Name}}{{"\t"}}{{json .Driver}}{{"\t"}}{{json .Labels}}{{"\t"}}{{json .Options}}{{"\t"}}{{json .Mountpoint}}"#.into(),
            "--".into(), mount.name.clone(),
        ])?;
        if inspected.code != 0 {
            anyhow::bail!("holder volume inspection failed (exit {})", inspected.code);
        }
        let fields: Vec<_> = inspected.stdout.trim().split('\t').collect();
        if fields.len() != 5 {
            anyhow::bail!("malformed holder volume projection");
        }
        let volume_name: String = serde_json::from_str(fields[0])?;
        let driver: String = serde_json::from_str(fields[1])?;
        let labels: BTreeMap<String, String> = serde_json::from_str(fields[2])?;
        let options: Option<BTreeMap<String, String>> = serde_json::from_str(fields[3])?;
        let source: String = serde_json::from_str(fields[4])?;
        eprintln!(
            "temporary inspected volume: name={volume_name:?} mount={:?} driver={driver:?} source={source:?} expected_source={:?} labels={labels:?}",
            mount.name, mount.source
        );
        if volume_name != mount.name
            || driver != "local"
            || source != mount.source
            || !options.unwrap_or_default().is_empty()
        {
            anyhow::bail!("holder volume identity/driver/options mismatch");
        }
        verify_attested_labels(
            "holder volume",
            &mount.name,
            &labels,
            identity,
            Some(ROLE_VOLUME_HOLDER),
        )?;
    }
    super::ownership::attest_isolation(runner, identity, &id, ROLE_VOLUME_HOLDER, &mounts, None)?;
    Ok(VolumeHolderAttestation { id, mounts })
}

/// Ensure the deterministic holder exists in the exact `created` state.
/// Creation never starts the holder; its only purpose is to own anonymous
/// volumes inherited by the DinD and runner containers.
pub(crate) fn ensure_volume_holder(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    image: &PinnedImage,
) -> Result<(VolumeHolderProvision, VolumeHolderAttestation)> {
    let spec = VolumeHolderSpec::new(identity.clone(), image.clone());
    let name = identity.volume_holder_container();
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
        .with_context(|| format!("inspect volume holder {name}"))?;
    if adoption_inspect_is_missing("volume holder", &name, &inspect)? {
        reject_existing_holder_dependents(runner, identity, &name)?;
        let created = runner
            .run("docker", &spec.create_args())
            .with_context(|| format!("create volume holder {name}"))?;
        if created.code != 0 {
            anyhow::bail!(
                "create volume holder {name} exited {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        if parse_container_id(&created.stdout).is_none() {
            anyhow::bail!("create volume holder {name} returned an empty id");
        }
        let holder = attest_volume_holder_at(runner, identity, image, created.stdout.trim())
            .with_context(|| format!("attest created volume holder {name}"))?;
        return Ok((VolumeHolderProvision::Created, holder));
    }
    let holder = attest_volume_holder_at(runner, identity, image, inspect.stdout.trim())
        .with_context(|| format!("attest existing volume holder {name}"))?;
    Ok((VolumeHolderProvision::Adopted, holder))
}

/// Read-only restart attestation for the worker's private bridge network.
///
/// The projection contains only the network ID, driver, and labels. No Docker
/// mutation is performed.
pub(crate) fn attest_restart_network(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<String> {
    let name = identity.network();
    let inspect = runner
        .run(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                NETWORK_ATTEST_FORMAT.to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("attest worker network {name}"))?;
    let fields = inspect_projection_fields("worker network", &name, &inspect, 3)?;
    let network_id: String = serde_json::from_str(fields[0])
        .with_context(|| format!("parse worker network {name} id"))?;
    let driver: String = serde_json::from_str(fields[1])
        .with_context(|| format!("parse worker network {name} driver"))?;
    let labels: BTreeMap<String, String> = serde_json::from_str(fields[2])
        .with_context(|| format!("parse worker network {name} labels"))?;

    if network_id.is_empty() {
        anyhow::bail!("worker network {name} returned an empty id");
    }
    if driver != "bridge" {
        anyhow::bail!("worker network {name} driver is {driver:?}, expected \"bridge\"");
    }
    verify_attested_labels("worker network", &name, &labels, identity, None)?;
    Ok(network_id)
}

/// Read-only restart attestation for a private DinD container.
///
/// The projection verifies the pinned image, all worker ownership labels, the
/// DinD role, network mode, network attachment ID, and lifecycle state. It
/// intentionally excludes .Config.Env and performs no Docker mutation.
pub(crate) fn attest_restart_dind(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
    expected_network_id: &str,
) -> Result<ContainerState> {
    attest_dind_with_allowed_states(
        runner,
        identity,
        expected_image,
        expected_network_id,
        &[
            ContainerState::Running,
            ContainerState::Exited,
            ContainerState::Dead,
        ],
    )
}

/// Attest a DinD container during provision retry.
///
/// A container left in `created` is safe to start after the same immutable
/// identity checks as an exited/dead container. Transitional states are never
/// mutated: they may belong to another Docker operation still in flight.
fn attest_provision_dind(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
    expected_network_id: &str,
) -> Result<ContainerState> {
    attest_dind_with_allowed_states(
        runner,
        identity,
        expected_image,
        expected_network_id,
        &[
            ContainerState::Created,
            ContainerState::Running,
            ContainerState::Exited,
            ContainerState::Dead,
        ],
    )
}

fn attest_dind_with_allowed_states(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
    expected_network_id: &str,
    allowed_states: &[ContainerState],
) -> Result<ContainerState> {
    attest_dind_at(
        runner,
        identity,
        expected_image,
        expected_network_id,
        allowed_states,
        &identity.dind_container(),
    )
}

fn attest_dind_at(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    expected_image: &PinnedImage,
    expected_network_id: &str,
    allowed_states: &[ContainerState],
    target: &str,
) -> Result<ContainerState> {
    if expected_network_id.is_empty() {
        anyhow::bail!("worker network id is empty");
    }

    let name = identity.dind_container();
    let inspect = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                DIND_ATTEST_FORMAT.to_string(),
                "--".to_string(),
                target.to_string(),
            ],
        )
        .with_context(|| format!("attest DinD container {name}"))?;
    let fields = inspect_projection_fields("DinD container", &name, &inspect, 6)?;
    let image: String = serde_json::from_str(fields[0])
        .with_context(|| format!("parse DinD container {name} image"))?;
    let labels: BTreeMap<String, String> = serde_json::from_str(fields[1])
        .with_context(|| format!("parse DinD container {name} labels"))?;
    let network_mode: String = serde_json::from_str(fields[2])
        .with_context(|| format!("parse DinD container {name} network mode"))?;
    let volumes_from: Option<Vec<String>> = serde_json::from_str(fields[3])
        .with_context(|| format!("parse DinD container {name} volume holder references"))?;
    let networks: BTreeMap<String, serde_json::Value> = serde_json::from_str(fields[4])
        .with_context(|| format!("parse DinD container {name} network attachments"))?;
    let status: String = serde_json::from_str(fields[5])
        .with_context(|| format!("parse DinD container {name} status"))?;

    if image != expected_image.reference() {
        anyhow::bail!(
            "DinD container {name} image mismatch: expected {:?}, found {image:?}",
            expected_image.reference()
        );
    }
    verify_attested_labels("DinD container", &name, &labels, identity, Some(ROLE_DIND))?;
    let holder_id = super::ownership::container_id(runner, &identity.volume_holder_container())?;
    verify_volume_holder_reference("DinD container", &name, volumes_from.as_deref(), &holder_id)?;
    if network_mode != expected_network_id {
        anyhow::bail!(
            "DinD container {name} network mode mismatch: expected {:?}, found {network_mode:?}",
            expected_network_id
        );
    }
    let attachment = networks
        .get(&identity.network())
        .and_then(serde_json::Value::as_object)
        .and_then(|network| network.get("NetworkID"))
        .and_then(serde_json::Value::as_str);
    if attachment != Some(expected_network_id) {
        anyhow::bail!(
            "DinD container {name} network attachment mismatch: expected {expected_network_id:?}, found {attachment:?}"
        );
    }

    let state = ContainerState::parse(&status).ok_or_else(|| {
        anyhow::anyhow!("DinD container {name} returned unknown status {status:?}")
    })?;
    if allowed_states.contains(&state) {
        Ok(state)
    } else {
        anyhow::bail!("DinD container {name} returned unsafe status {state:?}");
    }
}

/// Ensure the private daemon exists and is started, adopting on retry.
///
/// * Looks up the recorded container name; a missing container is created
///   from [`DindSpec::create_args`] and started.
/// * An existing container must match the exact pinned image, worker and DinD
///   labels, private network attachment, and safe lifecycle state before any
///   start or adoption; foreign/unsafe objects fail closed.
/// * Never pulls here: images are pulled + verified by the tool-content
///   hook ([`super::runner`]) before provisioning starts.
pub(crate) fn ensure_dind(
    runner: &mut dyn WorkerRunner,
    spec: &DindSpec,
    network_id: &str,
    holder: &VolumeHolderAttestation,
) -> Result<(DindProvision, String)> {
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

    let name = spec.identity().dind_container();
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
        .with_context(|| format!("inspect DinD container {name}"))?;
    if adoption_inspect_is_missing("DinD container", &name, &inspect)? {
        let created = runner
            .run("docker", &spec.create_args(network_id, &holder.id))
            .with_context(|| format!("create DinD container {name}"))?;
        if created.code != 0 {
            anyhow::bail!(
                "create DinD container {name} exited {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        let id = parse_container_id(&created.stdout).context("DinD create returned no id")?;
        super::ownership::attest_isolation(
            runner,
            spec.identity(),
            &id,
            ROLE_DIND,
            &holder.mounts,
            Some(spec.state_dir()),
        )?;
        let started = runner
            .run(
                "docker",
                &["start".to_string(), "--".to_string(), id.clone()],
            )
            .with_context(|| format!("start DinD container {name}"))?;
        if started.code != 0 {
            anyhow::bail!(
                "start DinD container {name} exited {}: {}",
                started.code,
                started.stderr.trim()
            );
        }
        return Ok((DindProvision::Created, id));
    }

    // The name lookup is only a fast existence check. Before any start or
    // adoption, prove the network and container projections against the
    // recorded identity and exact pinned image. This closes same-name
    // replacement and transitional-state races.
    let id = inspect.stdout.trim();
    let state = attest_dind_at(
        runner,
        spec.identity(),
        spec.image(),
        network_id,
        &[
            ContainerState::Created,
            ContainerState::Running,
            ContainerState::Exited,
            ContainerState::Dead,
        ],
        id,
    )
    .with_context(|| format!("attest DinD container {name} before adoption"))?;
    super::ownership::attest_isolation(
        runner,
        spec.identity(),
        id,
        ROLE_DIND,
        &holder.mounts,
        Some(spec.state_dir()),
    )?;
    if state == ContainerState::Running {
        return Ok((DindProvision::Adopted, id.to_string()));
    }

    let started = runner
        .run(
            "docker",
            &["start".to_string(), "--".to_string(), id.to_string()],
        )
        .with_context(|| format!("start DinD container {name}"))?;
    if started.code != 0 {
        anyhow::bail!(
            "start DinD container {name} exited {}: {}",
            started.code,
            started.stderr.trim()
        );
    }
    Ok((DindProvision::Adopted, id.to_string()))
}

/// Adoption gate shared by both containers of a pair: the existing
/// container's ownership label must equal the recorded ownership.
/// Foreign or missing labels fail closed — a stranger is never adopted.
pub(crate) fn verify_container_ownership(
    runner: &mut dyn WorkerRunner,
    container: &str,
    expected_ownership: &str,
) -> Result<()> {
    let labels = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{range $k, $v := .Config.Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#.to_string(),
                "--".to_string(),
                container.to_string(),
            ],
        )
        .with_context(|| format!("inspect container labels {container}"))?;
    if labels.code != 0 {
        anyhow::bail!(
            "inspect container labels {container} exited {}: {}",
            labels.code,
            labels.stderr.trim()
        );
    }
    let mut ownership = None;
    for line in labels.stdout.lines() {
        if let Some((key, value)) = parse_label_line(line)
            && key == super::ownership::OWNERSHIP_LABEL
        {
            ownership = Some(value);
        }
    }
    match ownership {
        Some(found) if found == expected_ownership => Ok(()),
        Some(found) => anyhow::bail!(
            "container {container} carries foreign ownership {found:?}, expected {expected_ownership:?}: refusing to adopt"
        ),
        None => anyhow::bail!(
            "container {container} carries no ownership label: refusing to adopt"
        ),
    }
}

/// Single readiness probe: true when dockerd answers on the private socket.
///
/// A failing probe is NOT an error — the caller retries with its own
/// backoff, then fails the worker when the deadline passes. Only a
/// transport failure (the `docker exec` itself could not run) errors.
pub(crate) fn dind_ready(runner: &mut dyn WorkerRunner, spec: &DindSpec, id: &str) -> Result<bool> {
    let probe = runner
        .run("docker", &spec.probe_args(id))
        .context("probe DinD readiness")?;
    if probe.code != 0 {
        return Ok(false);
    }
    Ok(parse_probe_output(&probe.stdout))
}

/// How long the supervisor waits for first readiness before failing.
pub const DIND_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// Interval between readiness probes while provisioning.
pub const DIND_READY_POLL_INTERVAL: Duration = Duration::from_secs(5);

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
) -> Result<(NetworkProvision, String)> {
    let name = identity.network();
    let inspect = runner
        .run(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect worker network {name}"))?;
    if adoption_inspect_is_missing("worker network", &name, &inspect)? {
        let created = runner
            .run("docker", &network_create_args(identity))
            .with_context(|| format!("create worker network {name}"))?;
        if created.code != 0 {
            anyhow::bail!(
                "create worker network {name} exited {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        let id = parse_container_id(&created.stdout).context("network create returned no id")?;
        return Ok((NetworkProvision::Created, id));
    }
    let id = inspect.stdout.trim();
    verify_network_ownership(runner, identity, id)?;
    Ok((NetworkProvision::Adopted, id.to_string()))
}

fn verify_network_ownership(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
    id: &str,
) -> Result<()> {
    let name = identity.network();
    let labels = runner
        .run(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{range $k, $v := .Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#.to_string(),
                "--".to_string(),
                id.to_string(),
            ],
        )
        .with_context(|| format!("inspect worker network labels {name}"))?;
    if labels.code != 0 {
        anyhow::bail!(
            "inspect worker network labels {name} exited {}: {}",
            labels.code,
            labels.stderr.trim()
        );
    }
    let mut ownership = None;
    for line in labels.stdout.lines() {
        if let Some((key, value)) = parse_label_line(line)
            && key == super::ownership::OWNERSHIP_LABEL
        {
            ownership = Some(value);
        }
    }
    let expected = identity.ownership().as_str();
    match ownership {
        Some(found) if found == expected => Ok(()),
        Some(found) => anyhow::bail!(
            "network {name} carries foreign ownership {found:?}, expected {expected:?}: refusing to adopt"
        ),
        None => anyhow::bail!("network {name} carries no ownership label: refusing to adopt"),
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
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::VecDeque;

    const DIND_REF: &str =
        "docker@sha256:2a232a42256f70d78e3cc5d2b5d6b3276710a0de0596c145f627ecfae90282ac";
    const RUNNER_REF: &str =
        "ghcr.io/actions/actions-runner@sha256:e5496277be5d09bc968b3d64911b74e219ac4a3f2edce956a3ecf9271bea1ef4";

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

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
        state_dir: String,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
                state_dir: "/tmp/velnor-test-dind-state".into(),
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
            let identity = spec().identity().clone();
            if args
                .iter()
                .any(|arg| arg == super::super::ownership::ISOLATION_FORMAT)
            {
                let id = args.last().unwrap();
                let role = if id == "holder-object-id" {
                    ROLE_VOLUME_HOLDER
                } else {
                    ROLE_DIND
                };
                return Ok(Self::ok(&super::super::ownership::fixtures::isolation(
                    &identity,
                    id,
                    role,
                    &self.state_dir,
                )));
            }
            if args.starts_with(&["volume".into(), "inspect".into()]) {
                return Ok(Self::ok(&super::super::ownership::fixtures::volume(
                    &identity,
                    args.last().unwrap(),
                )));
            }
            if args.iter().any(|arg| arg == "{{.Id}}")
                && args.last() == Some(&identity.volume_holder_container())
                && self
                    .results
                    .front()
                    .is_none_or(|r| !r.stdout.is_empty() && !r.stdout.contains("holder"))
            {
                return Ok(Self::ok("holder-object-id"));
            }
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
        }
    }

    fn ensure_dind(runner: &mut ScriptRunner, spec: &DindSpec) -> Result<DindProvision> {
        runner.state_dir = spec.state_dir().display().to_string();
        super::ensure_dind(
            runner,
            spec,
            "network-id",
            &super::super::ownership::fixtures::holder(),
        )
        .map(|outcome| outcome.0)
    }

    fn network_projection(spec: &DindSpec, network_id: &str) -> String {
        [
            serde_json::to_string(network_id).unwrap(),
            serde_json::to_string("bridge").unwrap(),
            serde_json::to_string(&spec.identity().labels()).unwrap(),
        ]
        .join("\t")
    }

    fn dind_projection(
        spec: &DindSpec,
        image: &str,
        labels: &BTreeMap<String, String>,
        network_mode: &str,
        network_id: &str,
        status: &str,
    ) -> String {
        dind_projection_with_volumes_from(
            spec,
            image,
            labels,
            network_mode,
            Some(vec![spec.identity().volume_holder_container()]),
            network_id,
            status,
        )
    }

    fn dind_projection_with_volumes_from(
        spec: &DindSpec,
        image: &str,
        labels: &BTreeMap<String, String>,
        network_mode: &str,
        volumes_from: Option<Vec<String>>,
        network_id: &str,
        status: &str,
    ) -> String {
        let networks = serde_json::json!({
            spec.identity().network(): {
                "NetworkID": network_id,
            },
        });
        [
            serde_json::to_string(image).unwrap(),
            serde_json::to_string(labels).unwrap(),
            serde_json::to_string(network_mode).unwrap(),
            serde_json::to_string(&volumes_from).unwrap(),
            networks.to_string(),
            serde_json::to_string(status).unwrap(),
        ]
        .join("\t")
    }

    fn valid_dind_labels(spec: &DindSpec) -> BTreeMap<String, String> {
        let mut labels = spec.identity().labels();
        labels.insert(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string());
        labels
    }

    fn valid_holder_projection(spec: &DindSpec, status: &str) -> String {
        let mut labels = spec.identity().labels();
        labels.insert(
            WORKER_ROLE_LABEL.to_string(),
            ROLE_VOLUME_HOLDER.to_string(),
        );
        let mounts = serde_json::json!([
            {"Type":"volume","Name":"anonymous-work","Source":"/var/lib/docker/volumes/anonymous-work/_data","Destination":WORK_DIR,"Driver":"local","RW":true},
            {"Type":"volume","Name":"anonymous-tools","Source":"/var/lib/docker/volumes/anonymous-tools/_data","Destination":TOOL_CACHE_DIR,"Driver":"local","RW":true},
            {"Type":"volume","Name":"anonymous-docker","Source":"/var/lib/docker/volumes/anonymous-docker/_data","Destination":DIND_DATA_ROOT,"Driver":"local","RW":true}
        ]);
        [
            serde_json::to_string("holder-object-id").unwrap(),
            serde_json::to_string(RUNNER_REF).unwrap(),
            serde_json::to_string(&labels).unwrap(),
            "null".to_string(),
            serde_json::to_string(&vec![VOLUME_HOLDER_COMMAND]).unwrap(),
            serde_json::to_string(status).unwrap(),
            mounts.to_string(),
        ]
        .join("\t")
    }

    #[test]
    fn create_argv_has_no_tcp_surface() {
        let args = spec().create_args("network-id", "holder-object-id");
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
        let spec = spec();
        let args = spec.create_args("network-id", "holder-object-id");
        assert!(
            !args.iter().any(|arg| arg.contains("/var/run/docker.sock")),
            "{args:?}"
        );
        // The state bind is the worker's own dir at the identical path.
        assert!(args.contains(&format!("/tmp/velnor-test-dind-state:{STATE_MOUNT}")));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "--volumes-from" && pair[1] == spec.identity().volume_holder_container()
        }));
        assert!(!args.iter().any(|arg| arg.contains(&format!(":{WORK_DIR}"))));
        assert!(!args
            .iter()
            .any(|arg| arg.contains(&format!(":{TOOL_CACHE_DIR}"))));
        assert!(!args
            .iter()
            .any(|arg| arg.contains(&format!(":{DIND_DATA_ROOT}"))));
        // Privileged is explicit (DinD requirement), pinned image, ownership labels.
        assert!(args.contains(&"--privileged".to_string()));
        assert!(args.contains(&DIND_REF.to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.contains("velnor.scaleset.ownership=")));
    }

    #[test]
    fn dind_argv_mounts_workspace_and_tool_cache_coherently() {
        let spec = spec();
        let args = spec
            .create_args("network-id", "holder-object-id")
            .join("\n");
        assert!(args.contains("--volumes-from"));
        assert!(args.contains(&spec.identity().volume_holder_container()));
        assert!(!args.contains(&format!("{WORK_DIR}")));
        assert!(!args.contains(&format!("{TOOL_CACHE_DIR}")));
        assert!(!args.contains(&format!("{DIND_DATA_ROOT}")));
    }

    #[test]
    fn holder_argv_uses_three_anonymous_labeled_mounts_and_never_starts() {
        let spec = spec();
        let holder = VolumeHolderSpec::new(
            spec.identity().clone(),
            PinnedImage::parse(RUNNER_REF).unwrap(),
        );
        let args = holder.create_args();
        let mounts: Vec<_> = args
            .windows(2)
            .filter_map(|pair| (pair[0] == "--mount").then_some(pair[1].clone()))
            .collect();
        assert_eq!(mounts.len(), 3, "{args:?}");
        for target in [WORK_DIR, TOOL_CACHE_DIR, DIND_DATA_ROOT] {
            let mount = mounts
                .iter()
                .find(|mount| mount.contains(&format!("target={target}")))
                .unwrap();
            assert!(mount.starts_with("type=volume,target="));
            assert!(!mount.contains("source="));
            assert!(!mount.contains("src="));
            assert!(mount.contains("volume-label=velnor.scaleset.role=volume-holder"));
        }
        assert!(args.contains(&RUNNER_REF.to_string()));
        assert!(args.contains(&VOLUME_HOLDER_COMMAND.to_string()));
        assert!(!args.contains(&"start".to_string()));
    }

    #[test]
    fn missing_holder_is_created_and_attested_but_never_started() {
        let spec = spec();
        let runner_image = PinnedImage::parse(RUNNER_REF).unwrap();
        let name = spec.identity().volume_holder_container();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &format!("Error: No such container: {name}")),
            ScriptRunner::fail(
                1,
                &format!(
                    "Error: No such container: {}",
                    spec.identity().dind_container()
                ),
            ),
            ScriptRunner::fail(
                1,
                &format!(
                    "Error: No such container: {}",
                    spec.identity().runner_container()
                ),
            ),
            ScriptRunner::ok("holder-object-id\n"),
            ScriptRunner::ok(&valid_holder_projection(&spec, "created")),
        ]);
        assert_eq!(
            ensure_volume_holder(&mut runner, spec.identity(), &runner_image)
                .unwrap()
                .0,
            VolumeHolderProvision::Created
        );
        assert_eq!(runner.seen.len(), 5);
        assert!(runner.seen[3].contains(&"create".to_string()));
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("start")));
    }

    #[test]
    fn missing_holder_with_existing_dependent_fails_closed_before_create() {
        let spec = spec();
        let runner_image = PinnedImage::parse(RUNNER_REF).unwrap();
        let name = spec.identity().volume_holder_container();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &format!("Error: No such container: {name}")),
            ScriptRunner::ok("dind-object-id\n"),
        ]);
        let error = ensure_volume_holder(&mut runner, spec.identity(), &runner_image).unwrap_err();
        assert!(
            error.to_string().contains("dependent DinD container"),
            "{error:#}"
        );
        assert_eq!(runner.seen.len(), 2);
        assert!(!runner
            .seen
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("create")));
    }

    #[test]
    fn dind_attestation_requires_exact_volume_holder() {
        let spec = spec();
        let labels = valid_dind_labels(&spec);
        let expected_image = PinnedImage::parse(DIND_REF).unwrap();
        let holder = spec.identity().volume_holder_container();
        for volumes_from in [
            None,
            Some(Vec::new()),
            Some(vec!["legacy-named-holder".to_string()]),
            Some(vec![holder.clone(), "unexpected-second-holder".to_string()]),
        ] {
            let mut runner =
                ScriptRunner::scripted(vec![ScriptRunner::ok(&dind_projection_with_volumes_from(
                    &spec,
                    DIND_REF,
                    &labels,
                    &spec.identity().network(),
                    volumes_from,
                    "netid",
                    "running",
                ))]);
            let error = attest_restart_dind(&mut runner, spec.identity(), &expected_image, "netid")
                .unwrap_err();
            assert!(
                error.to_string().contains("HostConfig.VolumesFrom"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn probe_targets_the_private_socket() {
        let args = spec().probe_args("dind-object-id");
        assert_eq!(args[0], "exec");
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
        let dir = std::env::temp_dir().join(format!("velnor-dind-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let missing_name = spec.identity().dind_container();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &format!("Error: No such container: {missing_name}")), // inspect: missing
            ScriptRunner::ok("deadbeef\n"),                                              // create
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"),      // start
        ]);
        let provision = ensure_dind(&mut runner, &spec).unwrap();
        assert_eq!(provision, DindProvision::Created);
        assert_eq!(runner.seen.len(), 3);
        assert_eq!(runner.seen[1][0], "create");
        assert!(dir.join("buildkit-cache").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn container_adoption_lookup_rejects_transport_and_empty_success() {
        for (case, inspect) in [
            (
                "transport",
                ScriptRunner::fail(1, "Cannot connect to the Docker daemon"),
            ),
            ("empty-success", ScriptRunner::ok("")),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "velnor-dind-adoption-{case}-{}",
                std::process::id()
            ));
            let mut spec = spec();
            spec.state_dir = dir.clone();
            let mut runner = ScriptRunner::scripted(vec![inspect]);
            let error = ensure_dind(&mut runner, &spec).unwrap_err();
            assert!(
                error.to_string().contains("inspect DinD container"),
                "{error}"
            );
            assert_eq!(
                runner.seen.len(),
                1,
                "{case}: must not create after bad inspect"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn owned_container_is_adopted_and_started() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-adopt-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let network_id = "netid";
        let labels = valid_dind_labels(&spec);
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"), // inspect: present
            ScriptRunner::ok(&network_projection(&spec, network_id)),
            ScriptRunner::ok(&dind_projection(
                &spec,
                DIND_REF,
                &labels,
                &spec.identity().network(),
                network_id,
                "exited",
            )),
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"), // start
        ]);
        let provision = ensure_dind(&mut runner, &spec).unwrap();
        assert_eq!(provision, DindProvision::Adopted);
        assert_eq!(runner.seen.len(), 4);
        assert!(!runner.seen.iter().any(|argv| argv[0] == "create"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn running_owned_container_is_adopted_without_start() {
        let dir =
            std::env::temp_dir().join(format!("velnor-dind-running-adopt-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let network_id = "netid";
        let labels = valid_dind_labels(&spec);
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"),
            ScriptRunner::ok(&network_projection(&spec, network_id)),
            ScriptRunner::ok(&dind_projection(
                &spec,
                DIND_REF,
                &labels,
                &spec.identity().network(),
                network_id,
                "running",
            )),
        ]);
        assert_eq!(
            ensure_dind(&mut runner, &spec).unwrap(),
            DindProvision::Adopted
        );
        assert_eq!(runner.seen.len(), 3);
        assert!(!runner
            .seen
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("start")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_container_rejects_identity_network_or_unsafe_state_before_start() {
        let cases = [
            (
                "image",
                "wrong@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "exited",
            ),
            ("role", DIND_REF, "exited"),
            ("network-mode", DIND_REF, "exited"),
            ("network-attachment", DIND_REF, "exited"),
            ("paused", DIND_REF, "paused"),
        ];
        for (case, image, status) in cases {
            let dir = std::env::temp_dir()
                .join(format!("velnor-dind-attest-{case}-{}", std::process::id()));
            let mut spec = spec();
            spec.state_dir = dir.clone();
            let network_id = "netid";
            let mut labels = valid_dind_labels(&spec);
            let network_mode = if case == "network-mode" {
                "foreign-network".to_string()
            } else {
                spec.identity().network()
            };
            let attached_network_id = if case == "network-attachment" {
                "foreign-netid"
            } else {
                network_id
            };
            if case == "role" {
                labels.insert(
                    WORKER_ROLE_LABEL.to_string(),
                    super::super::ownership::ROLE_RUNNER.to_string(),
                );
            }
            let mut runner = ScriptRunner::scripted(vec![
                ScriptRunner::ok("deadbeef\n"),
                ScriptRunner::ok(&network_projection(&spec, network_id)),
                ScriptRunner::ok(&dind_projection(
                    &spec,
                    image,
                    &labels,
                    &network_mode,
                    attached_network_id,
                    status,
                )),
            ]);
            let error = ensure_dind(&mut runner, &spec).unwrap_err();
            assert_eq!(runner.seen.len(), 3, "{case}: must not start");
            assert!(
                error.to_string().contains("DinD container"),
                "{case}: {error}"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn foreign_container_fails_closed() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-foreign-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let network_id = "netid";
        let mut labels = valid_dind_labels(&spec);
        labels.insert(
            super::super::ownership::OWNERSHIP_LABEL.to_string(),
            "7/someone-else".to_string(),
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"),
            ScriptRunner::ok(&network_projection(&spec, network_id)),
            ScriptRunner::ok(&dind_projection(
                &spec,
                DIND_REF,
                &labels,
                &spec.identity().network(),
                network_id,
                "exited",
            )),
        ]);
        let error = ensure_dind(&mut runner, &spec).unwrap_err();
        assert_eq!(runner.seen.len(), 3, "{error:#}");
        assert!(
            runner.seen.iter().all(|argv| {
                argv.first().is_some_and(|command| command == "inspect")
                    || argv
                        .first()
                        .zip(argv.get(1))
                        .is_some_and(|(command, subcommand)| {
                            command == "network" && subcommand == "inspect"
                        })
            }),
            "foreign container must fail closed before mutation: {error:#}; commands={:?}",
            runner.seen
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unlabeled_container_fails_closed() {
        let dir =
            std::env::temp_dir().join(format!("velnor-dind-unlabeled-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let network_id = "netid";
        let labels = BTreeMap::from([(WORKER_ROLE_LABEL.to_string(), ROLE_DIND.to_string())]);
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"),
            ScriptRunner::ok(&network_projection(&spec, network_id)),
            ScriptRunner::ok(&dind_projection(
                &spec,
                DIND_REF,
                &labels,
                &spec.identity().network(),
                network_id,
                "exited",
            )),
        ]);
        let error = ensure_dind(&mut runner, &spec).unwrap_err();
        assert_eq!(runner.seen.len(), 3, "{error:#}");
        assert!(
            runner.seen.iter().all(|argv| {
                argv.first().is_some_and(|command| command == "inspect")
                    || argv
                        .first()
                        .zip(argv.get(1))
                        .is_some_and(|(command, subcommand)| {
                            command == "network" && subcommand == "inspect"
                        })
            }),
            "unlabeled container must fail closed before mutation: {error:#}; commands={:?}",
            runner.seen
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
        for forbidden in ["-p", "--publish", "--expose"] {
            assert!(!args.iter().any(|arg| arg == forbidden), "{args:?}");
        }
    }

    #[test]
    fn network_is_created_then_adopted() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let missing_name = identity.network();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::fail(1, &format!("Error: No such network: {missing_name}")),
            ScriptRunner::ok("netid\n"),
        ]);
        assert_eq!(
            ensure_network(&mut runner, &identity).unwrap().0,
            NetworkProvision::Created
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("netid\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
        ]);
        assert_eq!(
            ensure_network(&mut runner, &identity).unwrap().0,
            NetworkProvision::Adopted
        );
    }

    #[test]
    fn network_adoption_lookup_rejects_transport_and_empty_success() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        for (case, inspect) in [
            (
                "transport",
                ScriptRunner::fail(1, "Cannot connect to the Docker daemon"),
            ),
            ("empty-success", ScriptRunner::ok("")),
        ] {
            let mut runner = ScriptRunner::scripted(vec![inspect]);
            let error = ensure_network(&mut runner, &identity).unwrap_err();
            assert!(
                error.to_string().contains("inspect worker network"),
                "{error}"
            );
            assert_eq!(
                runner.seen.len(),
                1,
                "{case}: must not create after bad inspect"
            );
        }
    }

    #[test]
    fn foreign_network_fails_closed() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("netid\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/stranger\n"),
        ]);
        let error = ensure_network(&mut runner, &identity).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
    }

    #[test]
    fn readiness_probe_failure_is_not_ready_not_error() {
        let spec = spec();
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(1, "Cannot connect")]);
        assert!(!dind_ready(&mut runner, &spec, "dind-object-id").unwrap());
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("28.5.2\n")]);
        assert!(dind_ready(&mut runner, &spec, "dind-object-id").unwrap());
    }
}

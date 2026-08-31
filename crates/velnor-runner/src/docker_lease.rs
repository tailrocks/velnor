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
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// Host-visible runtime dir. systemd `PrivateTmp=yes` remaps daemon `/tmp`, so
/// a lease socket there is invisible to host dockerd and the guest bind-mount
/// of `/tmp/vdl-*.sock` is not the proxy.
pub const LEASE_SOCKET_DIR: &str = "/run/velnor";
/// Job containers are owned by their Velnor name and labels. Do not use an
/// untagged `ancestor=` filter here: Docker resolves it as an image reference
/// on every scan and emits a lookup warning when only `:26.04` is tagged.
/// docker-container BuildKit daemon created by `docker buildx create --name velnor-builder-*`.
/// Job-end used a `name=-{scope}0$` filter; Docker's name filter is a match on the
/// container name, and `$` is not an end-anchor on every engine, so Created/removing
/// builders survived cancel/restart. Prefix match plus orphan-job reclaim is the
/// ownership path.
pub const BUILDKIT_CONTAINER_NAME_PREFIX: &str = "buildx_buildkit_velnor-builder-";

const MAX_PROXY_BODY: usize = 32 * 1024 * 1024;
const DOCKER_PROXY_READ_TIMEOUT: Duration = Duration::from_secs(20);

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

/// List one daemon's job-owned containers with the ownership and lifecycle
/// fields needed for a fail-closed startup cleanup decision.
pub fn list_daemon_owned_containers_state_args(job_id: &str, daemon_id: &str) -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}\t{{.State}}".into(),
    ]
}

pub fn list_daemon_owned_networks_args(job_id: &str, daemon_id: &str) -> Vec<String> {
    vec![
        "network".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
    ]
}

pub fn list_daemon_owned_volumes_args(job_id: &str, daemon_id: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
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

pub fn list_job_buildkit_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}\t{{.State}}"
            .into(),
    ]
}

/// List terminal BuildKit resources with both exact ownership labels. Name
/// matching is only a type hint; labels are the ownership boundary.
pub fn list_exact_job_buildkit_format_args(job_id: &str, daemon_id: &str) -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}\t{{.State}}"
            .into(),
    ]
}

pub fn list_exact_job_buildkit_volume_format_args(job_id: &str, daemon_id: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
        "--format".into(),
        "{{.Name}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}".into(),
    ]
}

/// List BuildKit state volumes independently of BuildKit containers. A
/// container can disappear while its state volume survives, so startup
/// reclaim must discover volumes from their exact ownership labels alone.
pub fn list_daemon_buildkit_volume_format_args() -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--no-trunc".into(),
        "--filter".into(),
        format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}"),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}"),
        "--format".into(),
        "{{.Name}}\t{{.Label \"velnor.job-id\"}}\t{{.Label \"velnor.daemon-id\"}}".into(),
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

pub fn remove_network_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["network".into(), "rm".into()];
    args.extend(ids.iter().cloned());
    args
}

/// List one exact job network by immutable id. Docker's name and label filters
/// are discovery hints; the inspect snapshot below remains the ownership gate.
pub fn list_job_networks_args(job_id: &str, daemon_id: &str, network: &str) -> Vec<String> {
    vec![
        "network".into(),
        "ls".into(),
        "--no-trunc".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("name={network}"),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
        "--filter".into(),
        format!("label={DAEMON_ID_LABEL}={daemon_id}"),
    ]
}

const JOB_NETWORK_INSPECT_FORMAT: &str = r#"{{.Name}}{{ "\t" }}{{ index .Labels "velnor.job-id" }}{{ "\t" }}{{ index .Labels "velnor.daemon-id" }}{{ "\t" }}{{ len .Containers }}"#;

fn network_inspect_args(network_id: &str) -> Vec<String> {
    vec![
        "network".into(),
        "inspect".into(),
        "--format".into(),
        JOB_NETWORK_INSPECT_FORMAT.into(),
        network_id.into(),
    ]
}

fn exact_empty_owned_network(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
    network: &str,
) -> bool {
    let fields = formatted.trim().split('\t').collect::<Vec<_>>();
    fields.len() == 4
        && fields[0].trim() == network
        && fields[1].trim() == job_id
        && fields[2].trim() == daemon_id
        && fields[3].trim().parse::<usize>() == Ok(0)
}

fn exact_empty_owned_network_ids(
    listed: &str,
    job_id: &str,
    daemon_id: &str,
    network: &str,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for id in parse_docker_id_list(listed) {
        let inspected = docker(&network_inspect_args(&id))?;
        if exact_empty_owned_network(&inspected, job_id, daemon_id, network) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn reclaim_job_network(
    job_id: &str,
    daemon_id: &str,
    network: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    if job_id.trim().is_empty() || daemon_id.trim().is_empty() || network.trim().is_empty() {
        return Ok(());
    }
    let list_args = list_job_networks_args(job_id, daemon_id, network);
    let initial = docker(&list_args)?;
    let current = docker(&list_args)?;
    // Re-list immediately before inspecting/deleting. Network IDs are
    // immutable, so only IDs present in both exact snapshots may be removed.
    let initial_ids =
        exact_empty_owned_network_ids(&initial, job_id, daemon_id, network, &mut docker)?;
    let current_ids =
        exact_empty_owned_network_ids(&current, job_id, daemon_id, network, &mut docker)?;
    let ids = initial_ids
        .into_iter()
        .filter(|id| current_ids.binary_search(id).is_ok())
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        docker(&remove_network_args(&ids)).map(|_| ())?;
    }
    Ok(())
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
    job_id: String,
    daemon_id: String,
    armed: bool,
}

impl JobNetworkGuard {
    /// Arm the guard for an exact job-owned `network`. Call
    /// [`JobNetworkGuard::defuse`] after terminal cleanup has removed it.
    pub fn arm(
        network: impl Into<String>,
        job_id: impl Into<String>,
        daemon_id: impl Into<String>,
    ) -> Self {
        Self {
            network: network.into(),
            job_id: job_id.into(),
            daemon_id: daemon_id.into(),
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
        if let Err(error) = reclaim_job_network(
            &self.job_id,
            &self.daemon_id,
            &self.network,
            run_host_docker,
        ) {
            eprintln!(
                "Warning: job network drop-guard cleanup failed for {}: {error:#}",
                self.network,
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
        // Created can still be claimed by a starting job, and Removing is an
        // Engine operation still in flight. Only terminal states are safe for
        // stale/startup cleanup. Running, Restarting, Paused, Created,
        // Removing, and unknown states are protected.
        matches!(self, Self::Exited | Self::Dead)
    }
}

struct StaleJobOwnedSnapshot {
    stopped_container_ids: Vec<String>,
    all_containers_terminal: bool,
}

/// Parse a reclaim snapshot only when it is structurally valid. The explicit
/// job-container proof is checked on both the initial and immediate snapshots
/// before any owned network or volume can be removed.
fn stale_job_owned_snapshot(
    job_id: &str,
    daemon_id: &str,
    formatted: &str,
) -> Option<StaleJobOwnedSnapshot> {
    let mut ids = Vec::new();
    let mut all_containers_terminal = true;
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            return None;
        }
        let id = fields[0].trim();
        let name = fields[1].trim();
        let labeled_job = fields[2].trim();
        let labeled_daemon = fields[3].trim();
        let state = DockerContainerState::parse(fields[4])?;
        if id.is_empty() || name.is_empty() || labeled_job != job_id || labeled_daemon != daemon_id
        {
            return None;
        }
        all_containers_terminal &= state.safe_to_reclaim();
        if !name.contains(BUILDKIT_CONTAINER_NAME_PREFIX) && state.safe_to_reclaim() {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    Some(StaleJobOwnedSnapshot {
        stopped_container_ids: ids,
        all_containers_terminal,
    })
}

/// Labeled job containers minus docker-container BuildKit daemons.
///
/// BuildKit carries `velnor.job-id`, so the generic owned-container reclaim
/// used to `docker rm --force` it with the 6h step timeout while job-end
/// and doctor also rm'd the same id. Concurrent Engine deletes of a Created
/// `buildx_buildkit_velnor-builder-*` deadlock; the leftover stays. BuildKit
/// has its own prefix reclaim with a 20s bound.
pub fn owned_container_ids_excluding_buildkit(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        let names = fields[1].trim();
        let labeled_job = fields[2].trim();
        let labeled_daemon = fields[3].trim();
        if id.is_empty()
            || names.is_empty()
            || labeled_job != job_id
            || labeled_daemon != daemon_id
            || names.contains(BUILDKIT_CONTAINER_NAME_PREFIX)
            || DockerContainerState::parse(fields[4]).is_none()
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Select one exact job-owned container by its Docker name. The name is only a
/// role selector after both ownership labels and a known lifecycle state have
/// matched; callers must pass the returned immutable IDs to Docker removal.
pub fn owned_container_ids_for_name(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
    name: &str,
) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        if id.is_empty()
            || fields[1].trim() != name
            || fields[2].trim() != job_id
            || fields[3].trim() != daemon_id
            || DockerContainerState::parse(fields[4]).is_none()
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Select stale service containers by exact Docker name. The name is only a
/// role selector after both ownership labels and a reclaim-safe lifecycle state
/// have matched; callers must pass the returned immutable IDs to Docker
/// removal. Live, foreign, unlabeled, malformed, and unknown-state rows fail
/// closed.
pub fn stale_owned_container_ids_for_name(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
    name: &str,
) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        let Some(state) = DockerContainerState::parse(fields[4]) else {
            continue;
        };
        if id.is_empty()
            || fields[1].trim() != name
            || fields[2].trim() != job_id
            || fields[3].trim() != daemon_id
            || !state.safe_to_reclaim()
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Select only live job and Docker-action sidecars from an exact job+daemon
/// label snapshot. Container names identify the cancellation roles only after
/// both ownership labels have matched; names never establish ownership.
pub fn cancellation_container_ids(formatted: &str, job_id: &str, daemon_id: &str) -> Vec<String> {
    let sidecar = format!("velnor-docker-action-{job_id}");
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        let name = fields[1].trim();
        let labeled_job = fields[2].trim();
        let labeled_daemon = fields[3].trim();
        let Some(state) = DockerContainerState::parse(fields[4]) else {
            continue;
        };
        if id.is_empty()
            || labeled_job != job_id
            || labeled_daemon != daemon_id
            || !(name == job_id || name == sidecar)
            || state.safe_to_reclaim()
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

pub fn kill_container_args(id: &str) -> Vec<String> {
    vec!["kill".into(), id.to_string()]
}

const DOCKER_RM_TIMEOUT: Duration = Duration::from_secs(20);

/// Cap `docker rm` / `volume rm` / `network rm` so a stuck Engine delete cannot
/// pin job teardown for the 6h step timeout.
pub fn docker_cli_timeout(args: &[String], requested: Duration) -> Duration {
    let is_rm = match args.first().map(String::as_str) {
        Some("rm") => true,
        Some("volume" | "network") if args.get(1).map(String::as_str) == Some("rm") => true,
        _ => false,
    };
    if is_rm {
        requested.min(DOCKER_RM_TIMEOUT)
    } else {
        requested
    }
}

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

/// Orphan job scopes restricted to containers owned by `daemon_id` (see
/// [`daemon_owns_label`]). Each result carries the exact daemon label found in
/// the snapshot, so follow-up cleanup can use an exact filter for containers,
/// networks, and volumes. Input is the
/// `name \t job-id \t daemon-id \t state` row format from
/// [`list_daemon_owned_job_format_args`].
pub fn daemon_orphan_job_scopes(formatted: &str, daemon_id: &str) -> Vec<(String, String)> {
    let mut owners_by_job = BTreeMap::<String, BTreeSet<String>>::new();
    let mut protected_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            continue;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        let owner = fields[2].trim();
        if job_id.is_empty() || owner.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        if name.is_empty() {
            protected_jobs.insert(job_id.to_string());
            continue;
        }
        let Some(state) = DockerContainerState::parse(fields[3]) else {
            protected_jobs.insert(job_id.to_string());
            continue;
        };
        owners_by_job
            .entry(job_id.to_string())
            .or_default()
            .insert(owner.to_string());
        if name == job_id && !state.safe_to_reclaim() {
            protected_jobs.insert(job_id.to_string());
        }
    }
    owners_by_job
        .into_iter()
        .filter(|(job_id, _)| !protected_jobs.contains(job_id))
        .flat_map(|(job_id, owners)| owners.into_iter().map(move |owner| (job_id.clone(), owner)))
        .collect()
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
/// A builder is stale only in a terminal state (Exited/Dead) and when its
/// labeled job is not live. Created/Removing are still in-flight lifecycle
/// states. Unlabeled builders are never reclaimed because a name is only a
/// discovery hint, not ownership.
/// Live jobs keep their bootstrapping Created builders.
pub fn orphan_job_buildkit_ids(
    formatted: &str,
    live_jobs: &std::collections::BTreeSet<String>,
    daemon_id: &str,
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
            || job_id.is_empty()
            || !state.safe_to_reclaim()
        {
            continue;
        }
        // Startup reclaim is daemon-scoped. An absent ownership label is not
        // proof that this daemon owns the builder; fail closed so a co-located
        // daemon cannot reclaim an unlabeled resource.
        if owner.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        let job_live = live_jobs.contains(job_id);
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
pub fn job_buildkit_ids_for_job(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
    scope: &str,
) -> Vec<String> {
    let needle = format!("{BUILDKIT_CONTAINER_NAME_PREFIX}{scope}");
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let id = parts.next().unwrap_or("").trim();
        let names = parts.next().unwrap_or("").trim();
        let labeled_job = parts.next().unwrap_or("").trim();
        let labeled_daemon = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if id.is_empty()
            || !names.contains(&needle)
            || labeled_job != job_id
            || labeled_daemon != daemon_id
            || DockerContainerState::parse(state).is_none_or(|state| !state.safe_to_reclaim())
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

pub fn job_buildkit_volume_names_for_job(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
    scope: &str,
) -> Vec<String> {
    let needle = format!("{BUILDKIT_CONTAINER_NAME_PREFIX}{scope}");
    let mut names = Vec::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let labeled_job = parts.next().unwrap_or("").trim();
        let labeled_daemon = parts.next().unwrap_or("").trim();
        if !name.is_empty()
            && name.contains(&needle)
            && labeled_job == job_id
            && labeled_daemon == daemon_id
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

fn reclaim_orphan_job_buildkit_with_live(
    live_jobs: &std::collections::BTreeSet<String>,
    _orphan_scopes: &[(String, String)],
    daemon_id: &str,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_job_buildkit_format_args())?;
    let initial_ids = orphan_job_buildkit_ids(&formatted, live_jobs, daemon_id);
    let job_formatted = docker(&list_daemon_owned_job_format_args())?;
    let protected_jobs = live_daemon_job_ids(&job_formatted, daemon_id);
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

    // A volume may outlive its BuildKit container. Discover all exact
    // BuildKit state volumes by their labels, independently of container
    // discovery, then revalidate both the live-job set and the exact labels
    // immediately before non-force removal.
    let initial_volumes = daemon_buildkit_volume_snapshot(daemon_id, &protected_jobs, docker)?;
    if initial_volumes.is_empty() {
        return Ok(());
    }

    // Re-list liveness, then re-list the exact volume ownership immediately
    // before DELETE. A job start or ownership change after the first volume
    // snapshot must make that volume ineligible for this sweep.
    let final_job_formatted = docker(&list_daemon_owned_job_format_args())?;
    let final_live_jobs = live_daemon_job_ids(&final_job_formatted, daemon_id);
    let final_volumes = daemon_buildkit_volume_snapshot(daemon_id, &final_live_jobs, docker)?;
    let volume_ids = initial_volumes
        .intersection(&final_volumes)
        .filter(|(_, job_id, owner)| {
            !final_live_jobs.contains(job_id) && daemon_owns_label(owner, daemon_id)
        })
        .map(|(name, _, _)| name.clone())
        .collect::<Vec<_>>();
    if !volume_ids.is_empty() {
        docker(&remove_volume_args(&volume_ids)).map(|_| ())?;
    }
    Ok(())
}

fn startup_buildkit_container_ids(
    formatted: &str,
    job_id: &str,
    daemon_id: &str,
) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    for line in formatted.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            return None;
        }
        let id = fields[0].trim();
        let name = fields[1].trim();
        let labeled_job = fields[2].trim();
        let labeled_daemon = fields[3].trim();
        // Unknown state means the snapshot cannot prove safe identity. Never
        // force-remove a live, starting, removing, or ambiguous container.
        let state = DockerContainerState::parse(fields[4])?;
        if id.is_empty()
            || name.is_empty()
            || !name.starts_with(BUILDKIT_CONTAINER_NAME_PREFIX)
            || labeled_job != job_id
            || labeled_daemon != daemon_id
            || !state.safe_to_reclaim()
        {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    Some(ids)
}

fn startup_buildkit_volume_names(
    formatted: &str,
    daemon_id: &str,
    stale_scopes: &BTreeSet<(String, String)>,
    live_jobs: &BTreeSet<String>,
) -> Option<BTreeSet<(String, String, String)>> {
    let mut volumes = BTreeSet::new();
    for line in formatted.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return None;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        let owner = fields[2].trim();
        if name.is_empty()
            || job_id.is_empty()
            || owner.is_empty()
            || !name.starts_with(BUILDKIT_CONTAINER_NAME_PREFIX)
            || live_jobs.contains(job_id)
            || !daemon_owns_label(owner, daemon_id)
            || !stale_scopes.contains(&(job_id.to_string(), owner.to_string()))
        {
            continue;
        }
        volumes.insert((name.to_string(), job_id.to_string(), owner.to_string()));
    }
    Some(volumes)
}

/// Force-remove BuildKit resources after the runner has proved that every
/// slot's in-flight marker and teardown task is absent and every canonical
/// `JobClaim` lock is inactive.
///
/// This is deliberately separate from [`reclaim_daemon_orphan_jobs`]. That
/// normal path remains fail-closed on Docker live state and never force-kills
/// a Running container. The caller supplies the scopes already proven stale;
/// `live_jobs` is an additional Docker-side protection set for races. Every
/// Docker snapshot is re-listed immediately before deletion, and only exact
/// immutable IDs/names carrying both ownership labels are passed to `rm`.
pub fn reclaim_startup_buildkit_after_liveness_proof(
    live_jobs: &BTreeSet<String>,
    stale_scopes: &[(String, String)],
    daemon_id: &str,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    if daemon_id.trim().is_empty() {
        return Ok(());
    }
    let stale_scopes = stale_scopes
        .iter()
        .filter(|(job_id, owner)| {
            !job_id.trim().is_empty()
                && !owner.trim().is_empty()
                && !live_jobs.contains(job_id)
                && daemon_owns_label(owner, daemon_id)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    if stale_scopes.is_empty() {
        return Ok(());
    }

    for (job_id, owner) in &stale_scopes {
        let list_args = list_exact_job_buildkit_format_args(job_id, owner);
        let initial = docker(&list_args)?;
        let Some(initial_ids) = startup_buildkit_container_ids(&initial, job_id, owner) else {
            continue;
        };
        if initial_ids.is_empty() {
            continue;
        }
        let current = docker(&list_args)?;
        let Some(current_ids) = startup_buildkit_container_ids(&current, job_id, owner) else {
            continue;
        };
        let ids = initial_ids
            .into_iter()
            .filter(|id| current_ids.binary_search(id).is_ok())
            .collect::<Vec<_>>();
        remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }

    // BuildKit state volumes are independently discoverable after their
    // container has vanished. Re-list both ownership and Docker liveness
    // before force removal; a newly live scope is protected.
    let initial_volumes = docker(&list_daemon_buildkit_volume_format_args())?;
    let Some(initial_volumes) =
        startup_buildkit_volume_names(&initial_volumes, daemon_id, &stale_scopes, live_jobs)
    else {
        return Ok(());
    };
    if initial_volumes.is_empty() {
        return Ok(());
    }
    let current_jobs = docker(&list_daemon_owned_job_format_args())?;
    let current_live_jobs = live_daemon_job_ids(&current_jobs, daemon_id);
    let current_volumes = docker(&list_daemon_buildkit_volume_format_args())?;
    let Some(current_volumes) = startup_buildkit_volume_names(
        &current_volumes,
        daemon_id,
        &stale_scopes,
        &current_live_jobs,
    ) else {
        return Ok(());
    };
    let volume_names = initial_volumes
        .intersection(&current_volumes)
        .filter(|(_, job_id, _)| !current_live_jobs.contains(job_id))
        .map(|(name, _, _)| name.clone())
        .collect::<Vec<_>>();
    if !volume_names.is_empty() {
        // Docker has no volume state snapshot equivalent to container State.
        // Non-force rm is the final in-use fence: Engine refuses an attached
        // volume instead of deleting it. The exact-name intersection above
        // is the immediate ownership re-list before this operation.
        docker(&remove_volume_args(&volume_names))
            .context("remove stale BuildKit state volumes")?;
    }
    Ok(())
}

fn daemon_buildkit_volume_snapshot(
    daemon_id: &str,
    live_jobs: &BTreeSet<String>,
    docker: &mut impl FnMut(&[String]) -> Result<String>,
) -> Result<BTreeSet<(String, String, String)>> {
    let formatted = docker(&list_daemon_buildkit_volume_format_args())?;
    let mut volumes = BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            continue;
        }
        let name = fields[0].trim();
        let labeled_job = fields[1].trim();
        let labeled_owner = fields[2].trim();
        if !name.starts_with(BUILDKIT_CONTAINER_NAME_PREFIX)
            || labeled_job.is_empty()
            || labeled_owner.is_empty()
            || live_jobs.contains(labeled_job)
            || !daemon_owns_label(labeled_owner, daemon_id)
        {
            continue;
        }
        volumes.insert((
            name.to_string(),
            labeled_job.to_string(),
            labeled_owner.to_string(),
        ));
    }
    Ok(volumes)
}

pub fn is_docker_object_create(method: &str, target: &str) -> bool {
    if !method.eq_ignore_ascii_case("POST") {
        return false;
    }
    let path = target.split('?').next().unwrap_or(target);
    path.ends_with("/containers/create")
        || path.ends_with("/networks/create")
        || path.ends_with("/volumes/create")
}

pub fn inject_ownership_labels(body: &[u8], job_id: &str, daemon_id: &str) -> Result<Vec<u8>> {
    if body.is_empty() {
        let mut object = Map::new();
        object.insert("Labels".into(), ownership_labels_value(job_id, daemon_id));
        return serde_json::to_vec(&Value::Object(object)).context("serialize empty create body");
    }
    reject_duplicate_json_keys(body)?;
    let mut value: Value = serde_json::from_slice(body)
        .context("parse Docker create JSON so ownership labels can be injected")?;
    let Some(object) = value.as_object_mut() else {
        bail!("Docker create body must be a JSON object");
    };
    reject_create_key_aliases(object)?;
    let labels = object
        .remove("Labels")
        .or_else(|| object.remove("labels"))
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
    serde_json::to_vec(&value).context("serialize labeled Docker create body")
}

fn ownership_labels_value(job_id: &str, daemon_id: &str) -> Value {
    let mut labels = Map::new();
    labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.to_string()));
    labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.to_string()));
    Value::Object(labels)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DockerObjectTarget {
    Container(String),
    Network(String),
    Volume(String),
    Exec(String),
}

#[derive(Debug, PartialEq, Eq)]
enum DockerRequestAuthorization {
    Allow,
    Reject(&'static str),
    Owned(DockerObjectTarget),
    CreateContainer,
    NetworkContainerMutation(String),
}

fn docker_request_line(request: &[u8]) -> Result<(&str, &str)> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("Docker API request is missing header terminator")?;
    let header =
        std::str::from_utf8(&request[..header_end]).context("Docker API headers must be UTF-8")?;
    let line = header
        .split("\r\n")
        .next()
        .context("Docker API request line")?;
    let mut parts = line.split_whitespace();
    let method = parts.next().context("Docker API request method")?;
    let target = parts.next().context("Docker API request target")?;
    let version = parts.next().context("Docker API HTTP version")?;
    if !version.starts_with("HTTP/") {
        bail!("Docker API HTTP version is malformed");
    }
    if parts.next().is_some() {
        bail!("Docker API request line has extra fields");
    }
    Ok((method, target))
}

fn docker_api_segments(target: &str) -> Result<Vec<&str>> {
    if !target.starts_with('/') || target.contains('#') {
        bail!("Docker API target must be an origin-form path without a fragment");
    }
    let path = target.split('?').next().unwrap_or(target);
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        bail!("Docker API target contains an invalid path byte");
    }
    let mut segments = path.split('/').skip(1).collect::<Vec<_>>();
    if let Some(version) = segments.first().copied()
        && version.starts_with('v')
    {
        let version = &version[1..];
        let mut parts = version.split('.');
        let major = parts.next().unwrap_or_default();
        let minor = parts.next().unwrap_or_default();
        if major.is_empty()
            || minor.is_empty()
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
            || parts.next().is_some()
        {
            bail!("Docker API version segment is malformed");
        }
        segments.remove(0);
    }
    if segments.iter().any(|segment| segment.is_empty()) {
        bail!("Docker API target contains an empty path segment");
    }
    if segments.is_empty() {
        bail!("Docker API target has no resource");
    }
    Ok(segments)
}

fn validate_docker_object_reference(reference: &str) -> Result<()> {
    if reference.is_empty()
        || reference.len() > 256
        || matches!(reference, "." | "..")
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        bail!("Docker object target is malformed");
    }
    Ok(())
}

fn validate_docker_image_reference(reference: &str) -> Result<()> {
    if reference.is_empty()
        || reference.len() > 256
        || matches!(reference, "." | "..")
        || reference
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || !reference.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b':' | b'@' | b'/')
        })
    {
        bail!("Docker image target is malformed");
    }
    Ok(())
}

fn classify_docker_request(request: &[u8]) -> Result<DockerRequestAuthorization> {
    let (method, target) = docker_request_line(request)?;
    let method_is = |expected: &str| method.eq_ignore_ascii_case(expected);
    let segments = docker_api_segments(target)?;
    let resource = segments[0];

    let owned = |target: DockerObjectTarget| Ok(DockerRequestAuthorization::Owned(target));
    match resource {
        "_ping" if segments.len() == 1 && (method_is("GET") || method_is("HEAD")) => {
            Ok(DockerRequestAuthorization::Allow)
        }
        "auth" if segments.len() == 1 && method_is("POST") => Ok(DockerRequestAuthorization::Allow),
        "version" if segments.len() == 1 && method_is("GET") => {
            Ok(DockerRequestAuthorization::Allow)
        }
        "info" | "events" if segments.len() == 1 && method_is("GET") => {
            Ok(DockerRequestAuthorization::Reject(
                "global Docker metadata/events are not available through a job Docker lease",
            ))
        }
        "containers" => match segments.as_slice() {
            ["containers"] if method_is("GET") => Ok(DockerRequestAuthorization::Reject(
                "unscoped Docker container listing is not available through a job Docker lease",
            )),
            ["containers", "create"] if method_is("POST") => {
                Ok(DockerRequestAuthorization::CreateContainer)
            }
            ["containers", "json"] if method_is("GET") => Ok(DockerRequestAuthorization::Reject(
                "unscoped Docker container listing is not available through a job Docker lease",
            )),
            ["containers", "prune", ..] => Ok(DockerRequestAuthorization::Reject(
                "container prune is not available through a job Docker lease",
            )),
            ["containers", reference] if method_is("DELETE") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Container((*reference).into()))
            }
            ["containers", reference, operation]
                if method_is("GET")
                    && matches!(
                        *operation,
                        "json" | "logs" | "top" | "stats" | "changes" | "export"
                    ) =>
            {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Container((*reference).into()))
            }
            ["containers", reference, "archive"]
                if method_is("GET") || method_is("HEAD") || method_is("PUT") =>
            {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Container((*reference).into()))
            }
            ["containers", reference, operation]
                if method_is("POST")
                    && matches!(
                        *operation,
                        "attach"
                            | "exec"
                            | "kill"
                            | "pause"
                            | "rename"
                            | "resize"
                            | "restart"
                            | "start"
                            | "stop"
                            | "unpause"
                            | "update"
                            | "wait"
                    ) =>
            {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Container((*reference).into()))
            }
            ["containers", reference, "attach", "ws"] if method_is("GET") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Container((*reference).into()))
            }
            ["containers", reference, ..] => {
                validate_docker_object_reference(reference)?;
                Ok(DockerRequestAuthorization::Reject(
                    "unknown Docker container API operation",
                ))
            }
            _ => Ok(DockerRequestAuthorization::Reject(
                "unknown Docker container API target",
            )),
        },
        "networks" => match segments.as_slice() {
            ["networks"] if method_is("GET") => Ok(DockerRequestAuthorization::Reject(
                "unscoped Docker network listing is not available through a job Docker lease",
            )),
            ["networks", "create"] if method_is("POST") => Ok(DockerRequestAuthorization::Allow),
            ["networks", "prune", ..] => Ok(DockerRequestAuthorization::Reject(
                "network prune is not available through a job Docker lease",
            )),
            ["networks", reference, "connect"] if method_is("POST") => {
                validate_docker_object_reference(reference)?;
                Ok(DockerRequestAuthorization::NetworkContainerMutation(
                    (*reference).into(),
                ))
            }
            ["networks", reference, "disconnect"] if method_is("POST") => {
                validate_docker_object_reference(reference)?;
                Ok(DockerRequestAuthorization::NetworkContainerMutation(
                    (*reference).into(),
                ))
            }
            ["networks", reference] if method_is("GET") || method_is("DELETE") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Network((*reference).into()))
            }
            _ => Ok(DockerRequestAuthorization::Reject(
                "unknown Docker network API target",
            )),
        },
        "volumes" => match segments.as_slice() {
            ["volumes"] if method_is("GET") => Ok(DockerRequestAuthorization::Reject(
                "unscoped Docker volume listing is not available through a job Docker lease",
            )),
            ["volumes", "create"] if method_is("POST") => Ok(DockerRequestAuthorization::Allow),
            ["volumes", "prune", ..] => Ok(DockerRequestAuthorization::Reject(
                "volume prune is not available through a job Docker lease",
            )),
            ["volumes", reference] if method_is("GET") || method_is("DELETE") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Volume((*reference).into()))
            }
            _ => Ok(DockerRequestAuthorization::Reject(
                "unknown Docker volume API target",
            )),
        },
        "exec" => match segments.as_slice() {
            ["exec", reference] if method_is("DELETE") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Exec((*reference).into()))
            }
            ["exec", reference, "json"] if method_is("GET") => {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Exec((*reference).into()))
            }
            ["exec", reference, operation]
                if method_is("POST") && matches!(*operation, "start" | "resize") =>
            {
                validate_docker_object_reference(reference)?;
                owned(DockerObjectTarget::Exec((*reference).into()))
            }
            _ => Ok(DockerRequestAuthorization::Reject(
                "unknown Docker exec API target",
            )),
        },
        "images" => match segments.as_slice() {
            ["images", "json"] if method_is("GET") => Ok(DockerRequestAuthorization::Reject(
                "unscoped Docker image listing is not available through a job Docker lease",
            )),
            ["images", ..] if method_is("GET") && segments.len() >= 3 => {
                let operation = segments.last().copied().unwrap_or_default();
                if !matches!(operation, "json" | "history") {
                    return Ok(DockerRequestAuthorization::Reject(
                        "unknown Docker image API target",
                    ));
                }
                let reference = segments[1..segments.len() - 1].join("/");
                validate_docker_image_reference(&reference)?;
                Ok(DockerRequestAuthorization::Allow)
            }
            ["images", "create"] if method_is("POST") => Ok(DockerRequestAuthorization::Allow),
            ["images", "prune", ..] => Ok(DockerRequestAuthorization::Reject(
                "image prune is not available through a job Docker lease",
            )),
            _ => Ok(DockerRequestAuthorization::Reject(
                "unknown Docker image API target",
            )),
        },
        "build" if segments.len() == 1 && method_is("POST") => {
            Ok(DockerRequestAuthorization::Allow)
        }
        "system" if segments.get(1) == Some(&"prune") => Ok(DockerRequestAuthorization::Reject(
            "system prune is not available through a job Docker lease",
        )),
        _ => Ok(DockerRequestAuthorization::Reject(
            "unknown or unauthorized Docker API route",
        )),
    }
}

fn exact_labels_owned(labels: Option<&Value>, job_id: &str, daemon_id: &str) -> Result<()> {
    let labels = labels
        .and_then(Value::as_object)
        .context("Docker object has malformed or missing labels")?;
    let labeled_job = labels
        .get(JOB_ID_LABEL)
        .and_then(Value::as_str)
        .context("Docker object has malformed or missing job ownership label")?;
    let labeled_daemon = labels
        .get(DAEMON_ID_LABEL)
        .and_then(Value::as_str)
        .context("Docker object has malformed or missing daemon ownership label")?;
    if labeled_job != job_id || labeled_daemon != daemon_id {
        bail!("Docker object is not owned by this job lease");
    }
    Ok(())
}

fn container_json_is_owned(value: &Value, job_id: &str, daemon_id: &str) -> Result<()> {
    exact_labels_owned(
        value.get("Config").and_then(|config| config.get("Labels")),
        job_id,
        daemon_id,
    )
}

fn docker_object_is_owned(
    target: &DockerObjectTarget,
    value: &Value,
    job_id: &str,
    daemon_id: &str,
    inspect: &mut impl FnMut(&DockerObjectTarget) -> Result<Value>,
) -> Result<()> {
    match target {
        DockerObjectTarget::Container(_) => container_json_is_owned(value, job_id, daemon_id),
        DockerObjectTarget::Network(_) | DockerObjectTarget::Volume(_) => {
            exact_labels_owned(value.get("Labels"), job_id, daemon_id)
        }
        DockerObjectTarget::Exec(_) => {
            let container_id = value
                .get("ContainerID")
                .and_then(Value::as_str)
                .context("Docker exec inspection has malformed container id")?;
            validate_docker_object_reference(container_id)?;
            let container = inspect(&DockerObjectTarget::Container(container_id.to_string()))?;
            container_json_is_owned(&container, job_id, daemon_id)
        }
    }
}

fn docker_api_request_body(request: &[u8]) -> Result<&[u8]> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header = std::str::from_utf8(&request[..header_end])
        .context("Docker API request headers must be UTF-8")?;
    let mut content_length = None;
    for line in header.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
        {
            bail!("chunked Docker API request bodies are not supported by the lease");
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                bail!("Docker API request has duplicate Content-Length headers");
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("parse Docker API Content-Length")?,
            );
        }
    }
    let body = &request[header_end..];
    let expected = content_length.unwrap_or(0);
    if body.len() != expected {
        bail!(
            "Docker API request body length is {}, expected {expected}",
            body.len()
        );
    }
    Ok(body)
}

fn docker_api_json_body(request: &[u8], operation: &str) -> Result<Value> {
    let body = docker_api_request_body(request)?;
    if body.is_empty() {
        bail!("{operation} Docker API request body is required");
    }
    serde_json::from_slice(body).with_context(|| format!("parse {operation} Docker API body"))
}

fn reject_create_key_aliases(object: &Map<String, Value>) -> Result<()> {
    for (canonical, alias, name) in [
        ("Labels", "labels", "Labels"),
        ("HostConfig", "hostConfig", "HostConfig"),
        ("NetworkingConfig", "networkingConfig", "NetworkingConfig"),
        ("NetworkMode", "networkMode", "NetworkMode"),
    ] {
        if object.contains_key(canonical) && object.contains_key(alias) {
            bail!("Docker create specifies {name} more than once")
        }
    }
    if let Some(host_config) = object
        .get("HostConfig")
        .or_else(|| object.get("hostConfig"))
        && let Some(host_config) = host_config.as_object()
    {
        if host_config.contains_key("NetworkMode") && host_config.contains_key("networkMode") {
            bail!("Docker create specifies NetworkMode more than once")
        }
    }
    if let Some(networking_config) = object
        .get("NetworkingConfig")
        .or_else(|| object.get("networkingConfig"))
        && let Some(networking_config) = networking_config.as_object()
        && networking_config.contains_key("EndpointsConfig")
        && networking_config.contains_key("endpointsConfig")
    {
        bail!("Docker create specifies EndpointsConfig more than once")
    }
    Ok(())
}

struct DuplicateJsonKeySeed;

impl<'de> DeserializeSeed<'de> for DuplicateJsonKeySeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateJsonKeyVisitor)
    }
}

struct DuplicateJsonKeyVisitor;

impl<'de> Visitor<'de> for DuplicateJsonKeyVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(())
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while seq.next_element_seed(DuplicateJsonKeySeed)?.is_some() {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key: {key}"
                )));
            }
            map.next_value_seed(DuplicateJsonKeySeed)?;
        }
        Ok(())
    }
}

fn reject_duplicate_json_keys(body: &[u8]) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    deserializer
        .deserialize_any(DuplicateJsonKeyVisitor)
        .context("reject duplicate Docker create JSON keys")?;
    deserializer
        .end()
        .context("parse trailing Docker create JSON data")
}

fn authorize_network_attachment(
    reference: &str,
    job_id: &str,
    daemon_id: &str,
    inspect: &mut impl FnMut(&DockerObjectTarget) -> Result<Value>,
) -> Result<()> {
    if matches!(reference, "default" | "bridge" | "host" | "none") {
        return Ok(());
    }
    validate_docker_object_reference(reference)?;
    let network = inspect(&DockerObjectTarget::Network(reference.to_string()))?;
    exact_labels_owned(network.get("Labels"), job_id, daemon_id)
}

fn authorize_container_create_networking_config(
    object: &Map<String, Value>,
    job_id: &str,
    daemon_id: &str,
    inspect: &mut impl FnMut(&DockerObjectTarget) -> Result<Value>,
) -> Result<()> {
    let networking_config = match (
        object.get("NetworkingConfig"),
        object.get("networkingConfig"),
    ) {
        (Some(_), Some(_)) => bail!("Docker create specifies NetworkingConfig more than once"),
        (Some(config), None) | (None, Some(config)) => Some(config),
        (None, None) => None,
    };
    let Some(networking_config) = networking_config else {
        return Ok(());
    };
    let networking_config = networking_config
        .as_object()
        .context("Docker create NetworkingConfig must be an object")?;
    let endpoints = match (
        networking_config.get("EndpointsConfig"),
        networking_config.get("endpointsConfig"),
    ) {
        (Some(_), Some(_)) => {
            bail!("Docker create specifies EndpointsConfig more than once")
        }
        (Some(endpoints), None) | (None, Some(endpoints)) => Some(endpoints),
        (None, None) => None,
    };
    let Some(endpoints) = endpoints else {
        return Ok(());
    };
    let endpoints = endpoints
        .as_object()
        .context("Docker create EndpointsConfig must be an object")?;
    for (reference, settings) in endpoints {
        if !settings.is_null() && !settings.is_object() {
            bail!("Docker create endpoint settings must be an object or null");
        }
        authorize_network_attachment(reference, job_id, daemon_id, inspect)?;
    }
    Ok(())
}

fn authorize_container_create_network_mode(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    inspect: &mut impl FnMut(&DockerObjectTarget) -> Result<Value>,
) -> Result<()> {
    let body = docker_api_request_body(request)?;
    if body.is_empty() {
        return Ok(());
    }
    reject_duplicate_json_keys(body)?;
    let value: Value = serde_json::from_slice(body).context("parse Docker create body")?;
    let object = value
        .as_object()
        .context("Docker create body must be a JSON object")?;
    reject_create_key_aliases(object)?;
    authorize_container_create_networking_config(object, job_id, daemon_id, inspect)?;
    let top_level_mode = object
        .get("NetworkMode")
        .or_else(|| object.get("networkMode"));
    let host_config = object
        .get("HostConfig")
        .or_else(|| object.get("hostConfig"));
    let host_config_mode = match host_config {
        None => None,
        Some(Value::Object(config)) => config
            .get("NetworkMode")
            .or_else(|| config.get("networkMode")),
        Some(_) => bail!("Docker create HostConfig must be an object"),
    };
    if top_level_mode.is_some() && host_config_mode.is_some() {
        bail!("Docker create specifies NetworkMode more than once");
    }
    let Some(mode) = top_level_mode.or(host_config_mode) else {
        return Ok(());
    };
    let Some(mode) = mode.as_str() else {
        if mode.is_null() {
            return Ok(());
        }
        bail!("Docker create NetworkMode must be a string or null");
    };
    let mode = mode.trim();
    if mode.is_empty()
        || ["default", "bridge", "host", "none"]
            .iter()
            .any(|builtin| mode.eq_ignore_ascii_case(builtin))
    {
        return Ok(());
    }
    if let Some(reference) = mode.strip_prefix("container:") {
        validate_docker_object_reference(reference)?;
        let container = inspect(&DockerObjectTarget::Container(reference.to_string()))?;
        return container_json_is_owned(&container, job_id, daemon_id);
    }
    if mode.contains(':') {
        bail!("Docker create NetworkMode uses an unsupported namespace: {mode}");
    }
    validate_docker_object_reference(mode)?;
    let network = inspect(&DockerObjectTarget::Network(mode.to_string()))?;
    exact_labels_owned(network.get("Labels"), job_id, daemon_id)
}

fn network_container_reference(request: &[u8]) -> Result<String> {
    let value = docker_api_json_body(request, "network container mutation")?;
    let object = value
        .as_object()
        .context("Docker network mutation body must be a JSON object")?;
    let container = object
        .get("Container")
        .or_else(|| object.get("container"))
        .context("Docker network mutation body has no Container")?;
    let container = container
        .as_str()
        .context("Docker network mutation Container must be a string")?;
    validate_docker_object_reference(container)?;
    Ok(container.to_string())
}

fn authorize_docker_api_request(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
    mut inspect: impl FnMut(&DockerObjectTarget) -> Result<Value>,
) -> Result<()> {
    if job_id.trim().is_empty() || daemon_id.trim().is_empty() {
        bail!("Docker lease ownership identity is empty");
    }
    match classify_docker_request(request)? {
        DockerRequestAuthorization::Allow => Ok(()),
        DockerRequestAuthorization::Reject(reason) => bail!("{reason}"),
        DockerRequestAuthorization::CreateContainer => {
            authorize_container_create_network_mode(request, job_id, daemon_id, &mut inspect)
        }
        DockerRequestAuthorization::NetworkContainerMutation(network) => {
            let container = network_container_reference(request)?;
            let network = inspect(&DockerObjectTarget::Network(network))?;
            exact_labels_owned(network.get("Labels"), job_id, daemon_id)?;
            let container = inspect(&DockerObjectTarget::Container(container))?;
            container_json_is_owned(&container, job_id, daemon_id)
        }
        DockerRequestAuthorization::Owned(target) => {
            let value = inspect(&target).context("inspect Docker lease target")?;
            docker_object_is_owned(&target, &value, job_id, daemon_id, &mut inspect)
        }
    }
}

#[cfg(unix)]
fn inspect_docker_object(
    host_socket: &Path,
    target: &DockerObjectTarget,
    conns: &Arc<LeaseConnSet>,
) -> Result<Value> {
    use std::os::unix::net::UnixStream;

    let (path, reference) = match target {
        DockerObjectTarget::Container(reference) => {
            (format!("/containers/{reference}/json"), reference)
        }
        DockerObjectTarget::Network(reference) => (format!("/networks/{reference}"), reference),
        DockerObjectTarget::Volume(reference) => (format!("/volumes/{reference}"), reference),
        DockerObjectTarget::Exec(reference) => (format!("/exec/{reference}/json"), reference),
    };
    validate_docker_object_reference(reference)?;
    let mut stream = UnixStream::connect(host_socket)
        .with_context(|| format!("connect Docker Engine to inspect {reference}"))?;
    stream
        .set_read_timeout(Some(DOCKER_PROXY_READ_TIMEOUT))
        .context("set Docker Engine inspect read timeout")?;
    stream
        .set_write_timeout(Some(DOCKER_PROXY_READ_TIMEOUT))
        .context("set Docker Engine inspect write timeout")?;
    let _watch = conns.watch(&stream)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .context("request Docker Engine ownership inspection")?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let response = read_docker_http_response(&mut stream)?;
    serde_json::from_slice(&response).context("parse Docker Engine ownership inspection")
}

#[cfg(unix)]
fn read_docker_http_response(stream: &mut std::os::unix::net::UnixStream) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if response.len() >= MAX_PROXY_BODY {
            bail!("Docker Engine inspection response exceeds lease proxy limit");
        }
        let read = stream
            .read(&mut chunk)
            .context("read Docker Engine ownership inspection")?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
    }
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker Engine inspection response is missing headers")?;
    let header = std::str::from_utf8(&response[..header_end])
        .context("Docker Engine inspection headers must be UTF-8")?;
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("Docker Engine inspection status is malformed")?;
    if status != 200 {
        bail!("Docker Engine ownership inspection returned HTTP {status}");
    }
    let content_length = header
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
        })
        .context("Docker Engine inspection response has no Content-Length")?
        .trim()
        .parse::<usize>()
        .context("Docker Engine inspection Content-Length is malformed")?;
    let body = &response[header_end..];
    if body.len() != content_length {
        bail!("Docker Engine inspection body length does not match Content-Length");
    }
    Ok(body.to_vec())
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
    if !is_docker_object_create(method, target) {
        return Ok(request.to_vec());
    }
    let labeled = inject_ownership_labels(body, job_id, daemon_id)?;
    let mut headers = Vec::new();
    let mut skipped_length = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.len() >= 15 && line[..15].eq_ignore_ascii_case("content-length:") {
            skipped_length = true;
            continue;
        }
        headers.push(line);
    }
    if !skipped_length
        && headers
            .iter()
            .any(|line| line.len() >= 18 && line[..18].eq_ignore_ascii_case("transfer-encoding:"))
    {
        bail!("refusing to rewrite chunked Docker create request");
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

/// Docker Engine 29 keeps HTTP/1.1 connections open after a response. This
/// proxy handles one request then `io::copy` until host EOF, so a keepalive
/// engine deadlocks the guest `docker` CLI on the second request (`docker
/// version` prints Client then hangs on Server; `docker login` never returns).
/// Forcing `Connection: close` on non-upgrade traffic matches that one-shot
/// architecture: dockerd EOFs, `io::copy` returns, the CLI reconnects.
fn with_connection_close(request: &[u8]) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header_text =
        std::str::from_utf8(&request[..header_end]).context("Docker API headers must be UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut out = Vec::new();
    out.extend_from_slice(request_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.len() >= 11 && line[..11].eq_ignore_ascii_case("connection:") {
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out.extend_from_slice(&request[header_end..]);
    Ok(out)
}

pub fn reclaim_job_owned(
    job_id: &str,
    daemon_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    if job_id.trim().is_empty() || daemon_id.trim().is_empty() {
        bail!("job and daemon identity are required for Docker cleanup");
    }
    let listed = docker(&list_daemon_owned_containers_state_args(job_id, daemon_id))?;
    let ids = owned_container_ids_excluding_buildkit(&listed, job_id, daemon_id);
    if !ids.is_empty() {
        docker(&force_remove_container_args(&ids)).map(|_| ())?;
    }
    reclaim_listed(
        &list_daemon_owned_networks_args(job_id, daemon_id),
        &mut docker,
        force_remove_network_args,
    )?;
    reclaim_listed(
        &list_daemon_owned_volumes_args(job_id, daemon_id),
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
    daemon_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    if daemon_id.trim().is_empty() {
        return Ok(());
    }
    let list_args = list_daemon_owned_containers_state_args(job_id, daemon_id);
    let initial = docker(&list_args)?;
    let Some(initial) = stale_job_owned_snapshot(job_id, daemon_id, &initial) else {
        return Ok(());
    };
    if !initial.all_containers_terminal {
        return Ok(());
    }
    let revalidated = docker(&list_args)?;
    let Some(revalidated) = stale_job_owned_snapshot(job_id, daemon_id, &revalidated) else {
        return Ok(());
    };
    if !revalidated.all_containers_terminal {
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
    reclaim_stale_listed(
        &list_daemon_owned_networks_args(job_id, daemon_id),
        &mut docker,
        force_remove_network_args,
    )?;
    reclaim_stale_listed(
        &list_daemon_owned_volumes_args(job_id, daemon_id),
        &mut docker,
        remove_volume_args,
    )?;
    Ok(())
}
/// Daemon-scoped startup reclaim: only
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
    let orphan_scopes = daemon_orphan_job_scopes(&formatted, daemon_id);
    for (job_id, owning_daemon_id) in &orphan_scopes {
        reclaim_stale_job_owned(job_id, owning_daemon_id, &mut docker)?;
    }
    let live = live_daemon_job_ids(&formatted, daemon_id);
    reclaim_orphan_job_buildkit_with_live(&live, &orphan_scopes, daemon_id, &mut docker)
}

pub fn run_host_docker(args: &[String]) -> Result<String> {
    run_host_docker_bounded(args, docker_cli_timeout(args, DOCKER_RM_TIMEOUT))
}

#[derive(Debug, PartialEq, Eq)]
enum DockerCliResult {
    Succeeded,
    RemovalAlreadyInProgress,
}

fn is_docker_removal(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("rm") => true,
        Some("network" | "volume") if args.get(1).map(String::as_str) == Some("rm") => true,
        _ => false,
    }
}

fn classify_docker_cli_result(
    args: &[String],
    success: bool,
    exit_code: Option<i32>,
    signaled: bool,
    timed_out: bool,
    stderr: &str,
) -> Result<DockerCliResult> {
    if timed_out {
        bail!(
            "docker {} timed out; cleanup result is unknown",
            args.join(" ")
        );
    }
    if success {
        return Ok(DockerCliResult::Succeeded);
    }
    if !signaled
        && is_docker_removal(args)
        && stderr.contains("removal")
        && stderr.contains("already in progress")
    {
        return Ok(DockerCliResult::RemovalAlreadyInProgress);
    }
    bail!(
        "docker {} failed (exit code {:?}): {}",
        args.join(" "),
        exit_code,
        stderr.trim()
    )
}

#[cfg(unix)]
fn configure_docker_process_group(command: &mut std::process::Command) {
    // `process_group(0)` makes the child its own process-group leader. Killing
    // `-pid` below then reaches docker plus every helper it spawned.
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_docker_process_group(_command: &mut std::process::Command) {}

fn kill_docker_process_group(pid: u32) -> std::io::Result<std::process::ExitStatus> {
    let signal_args = docker_process_group_kill_args(pid);
    #[cfg(unix)]
    {
        std::process::Command::new("/bin/kill")
            .args(signal_args)
            .status()
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("/bin/kill")
            .args(signal_args)
            .status()
    }
}

fn docker_process_group_kill_args(pid: u32) -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["-KILL".into(), "--".into(), format!("-{pid}")]
    }
    #[cfg(not(unix))]
    {
        vec!["-KILL".into(), pid.to_string()]
    }
}

pub(crate) fn run_host_docker_bounded(
    args: &[String],
    timeout: std::time::Duration,
) -> Result<String> {
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
    let mut command = std::process::Command::new("docker");
    configure_docker_process_group(&mut command);
    let child = command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    let pid = child.id();
    let (cancel, cancelled) = std::sync::mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_for_killer = Arc::clone(&timed_out);
    let killer = std::thread::spawn(move || {
        if cancelled.recv_timeout(timeout).is_err() {
            timed_out_for_killer.store(true, Ordering::SeqCst);
            let _ = kill_docker_process_group(pid);
        }
    });
    let output = child.wait_with_output();
    let _ = cancel.send(());
    let _ = killer.join();
    let output = output.with_context(|| format!("wait docker {}", args.join(" ")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    classify_docker_cli_result(
        args,
        output.status.success(),
        output.status.code(),
        #[cfg(unix)]
        output.status.signal().is_some(),
        #[cfg(not(unix))]
        false,
        timed_out.load(Ordering::SeqCst),
        &stderr,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

fn reclaim_stale_listed(
    list_args: &[String],
    docker: &mut impl FnMut(&[String]) -> Result<String>,
    remove_args: fn(&[String]) -> Vec<String>,
) -> Result<()> {
    let initial = parse_docker_id_list(&docker(list_args)?);
    if initial.is_empty() {
        return Ok(());
    }
    let current = parse_docker_id_list(&docker(list_args)?);
    let ids = initial
        .into_iter()
        .filter(|id| current.binary_search(id).is_ok())
        .collect::<Vec<_>>();
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
            next_id: Mutex::new(0),
            streams: Mutex::new(BTreeMap::new()),
        })
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn abort(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let mut streams = self.streams.lock().unwrap_or_else(|err| err.into_inner());
        let drained = std::mem::take(&mut *streams);
        for (_, stream) in drained {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn watch(self: &Arc<Self>, stream: &std::os::unix::net::UnixStream) -> Result<WatchedStream> {
        let clone = stream
            .try_clone()
            .context("track Docker lease connection for cancellation")?;
        let id = {
            let mut next = self.next_id.lock().unwrap_or_else(|err| err.into_inner());
            let id = *next;
            *next = next.saturating_add(1);
            self.streams
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .insert(id, clone);
            id
        };
        if self.is_shutdown() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        Ok(WatchedStream {
            set: Arc::clone(self),
            id: Some(id),
        })
    }

    fn unregister(&self, id: u64) {
        self.streams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&id);
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
    let conns_thread = Arc::clone(&conns);
    let listen_path_thread = listen_path.clone();
    let accept_thread = std::thread::Builder::new()
        .name(format!("velnor-docker-lease-{}", job_id))
        .spawn(move || {
            accept_loop(
                listener,
                host_socket,
                job_id,
                daemon_id,
                conns_thread,
                listen_path_thread,
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
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    conns: Arc<LeaseConnSet>,
    listen_path: PathBuf,
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
        let host_socket = host_socket.clone();
        let job_id = job_id.clone();
        let daemon_id = daemon_id.clone();
        let conns = Arc::clone(&conns);
        let _ = std::thread::Builder::new()
            .name("velnor-docker-lease-conn".into())
            .spawn(move || {
                if let Err(error) =
                    handle_client_with(stream, &host_socket, &job_id, &daemon_id, conns)
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
    )
}

#[cfg(unix)]
fn handle_client_with(
    mut client: std::os::unix::net::UnixStream,
    host_socket: &Path,
    job_id: &str,
    daemon_id: &str,
    conns: Arc<LeaseConnSet>,
) -> Result<()> {
    use std::os::unix::net::UnixStream;

    let _client_watch = conns.watch(&client)?;
    if conns.is_shutdown() {
        return Ok(());
    }
    let request = match read_http_request(&mut client) {
        Ok(request) => request,
        Err(_) if conns.is_shutdown() => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Err(error) = authorize_docker_api_request(&request, job_id, daemon_id, |target| {
        inspect_docker_object(host_socket, target, &conns)
    }) {
        if conns.is_shutdown() {
            return Ok(());
        }
        return Err(error);
    }
    if conns.is_shutdown() {
        return Ok(());
    }
    let forwarded = rewrite_docker_api_request(&request, job_id, daemon_id)?;
    let upgrade = std::str::from_utf8(&request).is_ok_and(|header| {
        header.lines().any(|line| {
            (line.len() >= 8 && line[..8].eq_ignore_ascii_case("upgrade:"))
                || (line.len() >= 11
                    && line[..11].eq_ignore_ascii_case("connection:")
                    && line.to_ascii_lowercase().contains("upgrade"))
        })
    });
    let forwarded = if upgrade {
        forwarded
    } else {
        with_connection_close(&forwarded)?
    };
    if conns.is_shutdown() {
        return Ok(());
    }
    let mut host = UnixStream::connect(host_socket).with_context(|| {
        format!(
            "connect job Docker lease to host engine {}",
            host_socket.display()
        )
    })?;
    let _host_watch = conns.watch(&host)?;
    if conns.is_shutdown() {
        return Ok(());
    }
    if let Err(error) = host
        .write_all(&forwarded)
        .context("forward Docker API request through job lease")
    {
        if conns.is_shutdown() {
            return Ok(());
        }
        return Err(error);
    }
    proxy_until_closed(host, client)
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
    Ok(())
}

#[cfg(unix)]
fn read_http_request(stream: &mut std::os::unix::net::UnixStream) -> Result<Vec<u8>> {
    read_http_request_with_timeout(stream, DOCKER_PROXY_READ_TIMEOUT)
}

#[cfg(unix)]
fn read_http_request_with_timeout(
    stream: &mut std::os::unix::net::UnixStream,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("calculate Docker API request read deadline")?;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Docker API request read deadline expired")?;
        stream
            .set_read_timeout(Some(remaining))
            .context("set Docker API request read timeout")?;
        let read = stream
            .read(&mut byte)
            .context("read Docker API request header")?;
        if read == 0 {
            bail!("client closed Docker API request before headers finished");
        }
        buf.push(byte[0]);
        if buf.len() > MAX_PROXY_BODY {
            bail!("Docker API request headers exceed lease proxy limit");
        }
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header_text =
        std::str::from_utf8(&buf[..header_end]).context("Docker API headers must be UTF-8")?;
    let mut content_length = 0_usize;
    for line in header_text.split("\r\n").skip(1) {
        if line.len() >= 15 && line[..15].eq_ignore_ascii_case("content-length:") {
            content_length = line[15..]
                .trim()
                .parse()
                .context("parse Docker API Content-Length")?;
        }
    }
    if content_length > MAX_PROXY_BODY {
        bail!("Docker API request body exceeds lease proxy limit");
    }
    while buf.len() < header_end + content_length {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Docker API request read deadline expired")?;
        stream
            .set_read_timeout(Some(remaining))
            .context("set Docker API request body read timeout")?;
        let read = stream
            .read(&mut byte)
            .context("read Docker API request body")?;
        if read == 0 {
            bail!("client closed Docker API request before body finished");
        }
        buf.push(byte[0]);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn job_network_guard_defused_drop_is_noop() {
        // No panic, no docker invocation: the guard type runs `docker` only
        // when armed, so a defused drop must be silent even on a real host.
        let guard = JobNetworkGuard::arm("velnor-net-defused", "job", "daemon");
        guard.defuse();
    }

    #[derive(Clone)]
    struct TestNetwork {
        id: String,
        name: String,
        job_id: String,
        daemon_id: String,
        endpoints: usize,
    }

    struct NetworkCleanupRunner {
        networks: Vec<TestNetwork>,
        list_count: usize,
        second_list_ids: Option<Vec<String>>,
        calls: Vec<Vec<String>>,
    }

    impl NetworkCleanupRunner {
        fn run(&mut self, args: &[String]) -> Result<String> {
            self.calls.push(args.to_vec());
            match args.first().map(String::as_str) {
                Some("network") if args.get(1).map(String::as_str) == Some("ls") => {
                    self.list_count += 1;
                    let ids = if self.list_count == 2 {
                        self.second_list_ids.clone().unwrap_or_else(|| {
                            self.networks
                                .iter()
                                .map(|network| network.id.clone())
                                .collect()
                        })
                    } else {
                        self.networks
                            .iter()
                            .map(|network| network.id.clone())
                            .collect()
                    };
                    Ok(ids.join("\n"))
                }
                Some("network") if args.get(1).map(String::as_str) == Some("inspect") => {
                    let id = args.last().context("network inspect id")?;
                    let network = self
                        .networks
                        .iter()
                        .find(|network| &network.id == id)
                        .context("network inspect fixture")?;
                    Ok(format!(
                        "{}\t{}\t{}\t{}",
                        network.name, network.job_id, network.daemon_id, network.endpoints
                    ))
                }
                Some("network") if args.get(1).map(String::as_str) == Some("rm") => {
                    assert!(!args.iter().any(|arg| arg == "--force"));
                    for id in &args[2..] {
                        let network = self
                            .networks
                            .iter()
                            .find(|network| &network.id == id)
                            .context("network rm fixture")?;
                        if network.endpoints != 0 {
                            return Err(anyhow!("active endpoints"));
                        }
                    }
                    let ids = &args[2..];
                    self.networks
                        .retain(|network| !ids.iter().any(|id| id == &network.id));
                    Ok(String::new())
                }
                _ => Err(anyhow!("unexpected Docker call: {args:?}")),
            }
        }
    }

    #[test]
    fn job_network_guard_removes_only_exact_owned_stopped_network() {
        let mut runner = NetworkCleanupRunner {
            networks: vec![
                TestNetwork {
                    id: "owned-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: "job".into(),
                    daemon_id: "daemon".into(),
                    endpoints: 0,
                },
                TestNetwork {
                    id: "foreign-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: "other-job".into(),
                    daemon_id: "daemon".into(),
                    endpoints: 0,
                },
                TestNetwork {
                    id: "unlabeled-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: String::new(),
                    daemon_id: String::new(),
                    endpoints: 0,
                },
                TestNetwork {
                    id: "live-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: "job".into(),
                    daemon_id: "daemon".into(),
                    endpoints: 1,
                },
            ],
            list_count: 0,
            second_list_ids: None,
            calls: Vec::new(),
        };

        reclaim_job_network("job", "daemon", "velnor-net-job", |args| runner.run(args)).unwrap();

        assert_eq!(runner.list_count, 2);
        assert_eq!(
            runner.calls[0],
            list_job_networks_args("job", "daemon", "velnor-net-job")
        );
        assert!(!runner
            .networks
            .iter()
            .any(|network| network.id == "owned-id"));
        assert!(runner
            .networks
            .iter()
            .any(|network| network.id == "foreign-id"));
        assert!(runner
            .networks
            .iter()
            .any(|network| network.id == "unlabeled-id"));
        assert!(runner
            .networks
            .iter()
            .any(|network| network.id == "live-id"));
        assert_eq!(
            runner.calls.last().unwrap(),
            &remove_network_args(&["owned-id".into()])
        );
    }

    #[test]
    fn job_network_guard_requires_same_owned_id_after_immediate_relist() {
        let mut runner = NetworkCleanupRunner {
            networks: vec![
                TestNetwork {
                    id: "owned-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: "job".into(),
                    daemon_id: "daemon".into(),
                    endpoints: 0,
                },
                TestNetwork {
                    id: "replacement-id".into(),
                    name: "velnor-net-job".into(),
                    job_id: "other-job".into(),
                    daemon_id: "daemon".into(),
                    endpoints: 0,
                },
            ],
            list_count: 0,
            second_list_ids: Some(vec!["replacement-id".into()]),
            calls: Vec::new(),
        };

        reclaim_job_network("job", "daemon", "velnor-net-job", |args| runner.run(args)).unwrap();

        assert_eq!(runner.list_count, 2);
        assert!(!runner.calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("network")
                && args.get(1).map(String::as_str) == Some("rm")
        }));
        assert!(runner
            .networks
            .iter()
            .any(|network| network.id == "owned-id"));
        assert!(runner
            .networks
            .iter()
            .any(|network| network.id == "replacement-id"));
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

    fn lease_request(line: &str) -> Vec<u8> {
        format!("{line} HTTP/1.1\r\nHost: docker\r\n\r\n").into_bytes()
    }

    fn lease_json_request(line: &str, body: &str) -> Vec<u8> {
        format!(
            "{line} HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn owned_container_json(job_id: &str, daemon_id: &str) -> Value {
        serde_json::json!({
            "Config": {
                "Labels": {
                    JOB_ID_LABEL: job_id,
                    DAEMON_ID_LABEL: daemon_id,
                }
            }
        })
    }

    #[test]
    fn docker_lease_allows_owned_start_and_exec_targets() {
        let start = lease_request("POST /v1.43/containers/container-1/start");
        authorize_docker_api_request(&start, "job-1", "daemon-1", |target| {
            assert_eq!(target, &DockerObjectTarget::Container("container-1".into()));
            Ok(owned_container_json("job-1", "daemon-1"))
        })
        .unwrap();

        let exec = lease_request("POST /v1.43/exec/exec-1/start");
        authorize_docker_api_request(&exec, "job-1", "daemon-1", |target| match target {
            DockerObjectTarget::Exec(id) => {
                assert_eq!(id, "exec-1");
                Ok(serde_json::json!({"ContainerID": "container-1"}))
            }
            DockerObjectTarget::Container(id) => {
                assert_eq!(id, "container-1");
                Ok(owned_container_json("job-1", "daemon-1"))
            }
            other => panic!("unexpected ownership inspection target: {other:?}"),
        })
        .unwrap();
    }

    #[test]
    fn docker_lease_rejects_foreign_malformed_unknown_and_prune_targets() {
        let foreign = owned_container_json("other-job", "daemon-1");
        let kill = lease_request("POST /v1.43/containers/foreign/kill");
        assert!(
            authorize_docker_api_request(&kill, "job-1", "daemon-1", |_| { Ok(foreign.clone()) })
                .is_err()
        );

        let malformed = lease_request("DELETE /v1.43/containers/%2Fforeign");
        assert!(
            authorize_docker_api_request(&malformed, "job-1", "daemon-1", |_| {
                panic!("malformed target must not reach Docker inspection")
            })
            .is_err()
        );

        let unknown = lease_request("POST /v1.43/containers/unknown/stop");
        assert!(
            authorize_docker_api_request(&unknown, "job-1", "daemon-1", |_| {
                bail!("Docker Engine returned HTTP 404")
            })
            .is_err()
        );

        let malformed_labels = lease_request("DELETE /v1.43/containers/container-1");
        assert!(
            authorize_docker_api_request(&malformed_labels, "job-1", "daemon-1", |_| {
                Ok(serde_json::json!({"Config": {"Labels": []}}))
            })
            .is_err()
        );

        let prune = lease_request("POST /v1.43/containers/prune");
        let mut inspected = false;
        assert!(
            authorize_docker_api_request(&prune, "job-1", "daemon-1", |_| {
                inspected = true;
                Ok(Value::Null)
            })
            .is_err()
        );
        assert!(!inspected, "prune must be rejected before host inspection");

        let unknown_destructive = lease_request("POST /v1.43/containers/container-1/prune");
        let mut inspected = false;
        assert!(
            authorize_docker_api_request(&unknown_destructive, "job-1", "daemon-1", |_| {
                inspected = true;
                Ok(owned_container_json("job-1", "daemon-1"))
            })
            .is_err()
        );
        assert!(
            !inspected,
            "unknown destructive routes must be rejected before host inspection"
        );

        for request in [
            lease_request("POST /v1.43/networks/network-1/rename"),
            lease_request("POST /v1.43/volumes/volume-1/mount"),
            lease_request("POST /v1.43/images/alpine/tag"),
            lease_request("PATCH /v1.43/containers/container-1/json"),
        ] {
            assert!(
                authorize_docker_api_request(&request, "job-1", "daemon-1", |_| {
                    panic!("unknown Docker route must not reach host inspection")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn docker_lease_validates_network_side_effect_targets_and_preserves_reads() {
        let connect = lease_json_request(
            "POST /v1.43/networks/job-network/connect",
            r#"{"Container":"job-container"}"#,
        );
        authorize_docker_api_request(&connect, "job-1", "daemon-1", |target| match target {
            DockerObjectTarget::Network(id) => {
                assert_eq!(id, "job-network");
                Ok(serde_json::json!({
                    "Labels": {
                        JOB_ID_LABEL: "job-1",
                        DAEMON_ID_LABEL: "daemon-1",
                    }
                }))
            }
            DockerObjectTarget::Container(id) => {
                assert_eq!(id, "job-container");
                Ok(owned_container_json("job-1", "daemon-1"))
            }
            other => panic!("unexpected inspection target: {other:?}"),
        })
        .unwrap();

        let create = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"Image":"alpine","HostConfig":{"NetworkMode":"job-network"}}"#,
        );
        authorize_docker_api_request(&create, "job-1", "daemon-1", |target| {
            assert_eq!(target, &DockerObjectTarget::Network("job-network".into()));
            Ok(serde_json::json!({
                "Labels": {
                    JOB_ID_LABEL: "job-1",
                    DAEMON_ID_LABEL: "daemon-1",
                }
            }))
        })
        .unwrap();

        for request in [
            lease_request("GET /v1.43/_ping"),
            lease_request("GET /v1.43/version"),
            lease_request("GET /v1.43/images/alpine:latest/json"),
            lease_request("GET /v1.43/images/alpine:latest/history"),
            lease_request("GET /v1.43/images/library/alpine:latest/json"),
            lease_request("GET /v1.43/images/library/alpine:latest/history"),
            lease_request("POST /v1.43/images/create?fromImage=alpine"),
            lease_request("POST /v1.43/build"),
        ] {
            authorize_docker_api_request(&request, "job-1", "daemon-1", |_| {
                panic!("read/build/pull must not require object inspection")
            })
            .unwrap();
        }
    }

    #[test]
    fn docker_lease_allows_exact_auth_but_rejects_global_routes() {
        let auth = lease_json_request("POST /v1.43/auth", r#"{"username":"runner"}"#);
        authorize_docker_api_request(&auth, "job-1", "daemon-1", |_| {
            panic!("native Docker login must not inspect a Docker object")
        })
        .unwrap();

        for request in [
            lease_request("GET /v1.43/auth"),
            lease_request("POST /v1.43/auth/extra"),
            lease_request("GET /v1.43/info"),
            lease_request("GET /v1.43/events"),
            lease_request("GET /v1.43/containers"),
            lease_request("GET /v1.43/containers/json"),
            lease_request("GET /v1.43/networks"),
            lease_request("GET /v1.43/volumes"),
            lease_request("GET /v1.43/images/json"),
        ] {
            assert!(
                authorize_docker_api_request(&request, "job-1", "daemon-1", |_| {
                    panic!("global or unknown route must not reach host inspection")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn docker_lease_validates_every_networking_config_endpoint() {
        let owned = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"NetworkingConfig":{"EndpointsConfig":{"job-network":{}}}}"#,
        );
        authorize_docker_api_request(&owned, "job-1", "daemon-1", |target| {
            assert_eq!(target, &DockerObjectTarget::Network("job-network".into()));
            Ok(serde_json::json!({
                "Labels": {
                    JOB_ID_LABEL: "job-1",
                    DAEMON_ID_LABEL: "daemon-1",
                }
            }))
        })
        .unwrap();

        let builtin = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"NetworkingConfig":{"EndpointsConfig":{"bridge":{},"none":null}}}"#,
        );
        authorize_docker_api_request(&builtin, "job-1", "daemon-1", |_| {
            panic!("built-in network attachments must not reach ownership inspection")
        })
        .unwrap();

        let foreign = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"NetworkingConfig":{"EndpointsConfig":{"foreign-network":{}}}}"#,
        );
        assert!(
            authorize_docker_api_request(&foreign, "job-1", "daemon-1", |target| {
                assert_eq!(
                    target,
                    &DockerObjectTarget::Network("foreign-network".into())
                );
                Ok(serde_json::json!({
                    "Labels": {
                        JOB_ID_LABEL: "other-job",
                        DAEMON_ID_LABEL: "daemon-1",
                    }
                }))
            })
            .is_err()
        );

        for body in [
            r#"{"NetworkingConfig":{"EndpointsConfig":{"bad/network":{}}}}"#,
            r#"{"NetworkingConfig":{"EndpointsConfig":[]}}"#,
            r#"{"NetworkingConfig":{"EndpointsConfig":{"job-network":[]}}}"#,
            r#"{"NetworkingConfig":{"EndpointsConfig":{"job-network":{},"job-network":{}}}}"#,
            r#"{"NetworkingConfig":{"EndpointsConfig":{}},"networkingConfig":{"EndpointsConfig":{}}}"#,
            r#"{"Labels":{},"labels":{}}"#,
            r#"{"HostConfig":{},"hostConfig":{}}"#,
            r#"{"NetworkingConfig":{},"networkingConfig":{}}"#,
            r#"{"NetworkMode":"bridge","networkMode":"none"}"#,
            r#"{"HostConfig":{"NetworkMode":"bridge","networkMode":"none"}}"#,
            r#"{"NetworkingConfig":{"EndpointsConfig":{}},"networkingConfig":{"endpointsConfig":{}}}"#,
        ] {
            let request = lease_json_request("POST /v1.43/containers/create", body);
            assert!(
                authorize_docker_api_request(&request, "job-1", "daemon-1", |_| {
                    panic!("malformed or duplicate create config must fail before inspection")
                })
                .is_err(),
                "create body should be rejected: {body}"
            );
        }
    }

    #[test]
    fn docker_lease_rejects_foreign_network_connect_and_network_modes() {
        let connect = lease_json_request(
            "POST /v1.43/networks/job-network/connect",
            r#"{"Container":"foreign-container"}"#,
        );
        assert!(authorize_docker_api_request(
            &connect,
            "job-1",
            "daemon-1",
            |target| match target {
                DockerObjectTarget::Network(_) => Ok(serde_json::json!({
                    "Labels": {
                        JOB_ID_LABEL: "job-1",
                        DAEMON_ID_LABEL: "daemon-1",
                    }
                })),
                DockerObjectTarget::Container(_) => {
                    Ok(owned_container_json("foreign-job", "daemon-1"))
                }
                other => panic!("unexpected inspection target: {other:?}"),
            }
        )
        .is_err());

        let foreign_network = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"HostConfig":{"NetworkMode":"foreign-network"}}"#,
        );
        assert!(
            authorize_docker_api_request(&foreign_network, "job-1", "daemon-1", |target| {
                assert_eq!(
                    target,
                    &DockerObjectTarget::Network("foreign-network".into())
                );
                Ok(serde_json::json!({
                    "Labels": {
                        JOB_ID_LABEL: "foreign-job",
                        DAEMON_ID_LABEL: "daemon-1",
                    }
                }))
            })
            .is_err()
        );

        let foreign_container = lease_json_request(
            "POST /v1.43/containers/create",
            r#"{"HostConfig":{"NetworkMode":"container:foreign-container"}}"#,
        );
        assert!(
            authorize_docker_api_request(&foreign_container, "job-1", "daemon-1", |target| {
                assert_eq!(
                    target,
                    &DockerObjectTarget::Container("foreign-container".into())
                );
                Ok(owned_container_json("foreign-job", "daemon-1"))
            })
            .is_err()
        );

        let missing_connect_body = lease_request("POST /v1.43/networks/job-network/connect");
        assert!(
            authorize_docker_api_request(&missing_connect_body, "job-1", "daemon-1", |_| {
                panic!("missing connect body must fail before inspection")
            })
            .is_err()
        );
    }

    #[test]
    fn with_connection_close_replaces_keepalive_and_preserves_body() {
        let request = b"POST /auth HTTP/1.1\r\nHost: docker\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\n{}";
        let closed = with_connection_close(request).unwrap();
        let text = String::from_utf8(closed).unwrap();
        assert!(text.contains("Connection: close\r\n\r\n{}"));
        assert!(!text.to_ascii_lowercase().contains("keep-alive"));
        assert_eq!(text.matches("Connection:").count(), 1);
    }

    #[test]
    fn reclaim_job_owned_removes_guest_container_network_and_volume() {
        let job_id = "velnor-job-1";
        let daemon_id = "daemon-1";
        let mut calls = Vec::new();
        let mut outputs = vec![
            format!(
                "aaa\tguest-postgres\t{job_id}\t{daemon_id}\texited\n\
                 bbb\tbuildx_buildkit_velnor-builder-dead0\t{job_id}\t{daemon_id}\tcreated\n"
            ),
            String::new(),
            "guest-net\n".to_string(),
            String::new(),
            "guest-vol\n".to_string(),
            String::new(),
        ];
        reclaim_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls[0],
            list_daemon_owned_containers_state_args(job_id, daemon_id)
        );
        assert_eq!(calls[1], force_remove_container_args(&["aaa".into()]));
        assert_eq!(calls[2], list_daemon_owned_networks_args(job_id, daemon_id));
        assert_eq!(calls[3], force_remove_network_args(&["guest-net".into()]));
        assert_eq!(calls[4], list_daemon_owned_volumes_args(job_id, daemon_id));
        assert_eq!(calls[5], force_remove_volume_args(&["guest-vol".into()]));
        assert!(outputs.is_empty());
    }

    #[test]
    fn reclaim_stale_job_owned_skips_all_deletes_for_unknown_state() {
        let job_id = "velnor-job-unknown";
        let daemon_id = "daemon-unknown";
        let mut calls = Vec::new();
        let mut outputs = vec![format!(
            "container\t{job_id}\t{job_id}\t{daemon_id}\tpaused-by-engine\n"
        )];
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![list_daemon_owned_containers_state_args(job_id, daemon_id)]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_preserves_every_nonterminal_container_state() {
        let job_id = "velnor-job-live-child";
        let daemon_id = "daemon-live-child";
        let snapshot = format!(
            "job-container\t{job_id}\t{job_id}\t{daemon_id}\texited\n\
             running-service\tservice\t{job_id}\t{daemon_id}\trunning\n\
             restarting-service\tservice\t{job_id}\t{daemon_id}\trestarting\n\
             paused-service\tservice\t{job_id}\t{daemon_id}\tpaused\n\
             created-buildkit\tbuildx_buildkit_velnor-builder-live0\t{job_id}\t{daemon_id}\tcreated\n\
             removing-buildkit\tbuildx_buildkit_velnor-builder-removing0\t{job_id}\t{daemon_id}\tremoving\n\
             unknown-service\tservice\t{job_id}\t{daemon_id}\tpaused-by-engine\n"
        );
        let mut calls = Vec::new();
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(snapshot.clone())
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![list_daemon_owned_containers_state_args(job_id, daemon_id)]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_skips_all_deletes_for_malformed_snapshot() {
        let job_id = "velnor-job-malformed";
        let daemon_id = "daemon-malformed";
        let mut calls = Vec::new();
        let mut outputs = vec![format!("container\t{job_id}\t{job_id}\n")];
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![list_daemon_owned_containers_state_args(job_id, daemon_id)]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_rejects_foreign_daemon_snapshot() {
        let job_id = "velnor-job-foreign-daemon";
        let daemon_id = "daemon-current";
        let mut calls = Vec::new();
        let mut outputs = vec![format!(
            "guest-id\tguest-container\t{job_id}\tdaemon-foreign\texited\n"
        )];
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![list_daemon_owned_containers_state_args(job_id, daemon_id)]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_skips_network_and_volume_when_job_restarts() {
        let job_id = "velnor-job-race";
        let daemon_id = "daemon-race";
        let mut calls = Vec::new();
        let mut outputs = vec![
            format!("job-container\t{job_id}\t{job_id}\t{daemon_id}\texited\n"),
            format!("job-container\t{job_id}\t{job_id}\t{daemon_id}\trunning\n"),
        ];
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![
                list_daemon_owned_containers_state_args(job_id, daemon_id),
                list_daemon_owned_containers_state_args(job_id, daemon_id),
            ]
        );
    }

    #[test]
    fn reclaim_stale_job_owned_removes_without_force() {
        let job_id = "velnor-job-stale";
        let daemon_id = "daemon-stale";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\t{daemon_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            "guest-net\n".to_string(),
            "guest-net\n".to_string(),
            String::new(),
            "guest-vol\n".to_string(),
            "guest-vol\n".to_string(),
            String::new(),
        ];
        reclaim_stale_job_owned(job_id, daemon_id, |args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[2], remove_container_args(&["guest-id".into()]));
        assert_eq!(calls[5], force_remove_network_args(&["guest-net".into()]));
        assert_eq!(calls[8], remove_volume_args(&["guest-vol".into()]));
        assert!(calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg == "--force")));
    }

    #[test]
    fn reclaim_stale_job_owned_live_race_fails_without_force() {
        let job_id = "velnor-job-live-race";
        let daemon_id = "daemon-live-race";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\t{daemon_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![snapshot.clone(), snapshot];
        let result = reclaim_stale_job_owned(job_id, daemon_id, |args| {
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
        let daemon_id = "daemon-volume-race";
        let snapshot = format!("guest-id\tguest-container\t{job_id}\t{daemon_id}\texited\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            snapshot.clone(),
            snapshot,
            String::new(),
            String::new(),
            "attached-volume\n".to_string(),
            "attached-volume\n".to_string(),
        ];
        let result = reclaim_stale_job_owned(job_id, daemon_id, |args| {
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
    fn owned_container_ids_excluding_buildkit_keep_guests_only() {
        let formatted = "\
aaa\tguest-postgres\tjob\tdaemon\texited
bbb\tbuildx_buildkit_velnor-builder-dead0\tjob\tdaemon\tcreated
ccc\tvelnor-docker-action-velnor-job-dead\tjob\tdaemon\trunning
";
        assert_eq!(
            owned_container_ids_excluding_buildkit(formatted, "job", "daemon"),
            vec!["aaa".to_string(), "ccc".to_string()]
        );
    }

    #[test]
    fn stale_owned_container_ids_for_name_selects_only_terminal_exactly_owned_services() {
        let formatted = "\
stale-id\tservice\tjob\tdaemon\texited
dead-id\tservice\tjob\tdaemon\tdead
created-id\tservice\tjob\tdaemon\tcreated
removing-id\tservice\tjob\tdaemon\tremoving
live-id\tservice\tjob\tdaemon\trunning
paused-id\tservice\tjob\tdaemon\tpaused
restarting-id\tservice\tjob\tdaemon\trestarting
foreign-id\tservice\tjob\tother\texited
unlabeled-id\tservice\t\tdaemon\texited
wrong-name-id\tother\tjob\tdaemon\texited
unknown-state-id\tservice\tjob\tdaemon\tpaused-by-engine
name-only-id\tservice
";

        assert_eq!(
            stale_owned_container_ids_for_name(formatted, "job", "daemon", "service"),
            vec!["dead-id".to_string(), "stale-id".to_string(),]
        );
    }

    #[test]
    fn cancellation_selects_only_live_exactly_owned_job_roles() {
        let formatted = "\
job-id\tjob\tjob\tdaemon\trunning
sidecar-id\tvelnor-docker-action-job\tjob\tdaemon\tpaused
foreign-id\tjob\tjob\tother\trunning
unlabeled-id\tjob\t\tdaemon\trunning
stopped-id\tjob\tjob\tdaemon\texited
";
        assert_eq!(
            cancellation_container_ids(formatted, "job", "daemon"),
            vec!["job-id".to_string(), "sidecar-id".to_string()]
        );
        assert_eq!(kill_container_args("job-id"), vec!["kill", "job-id"]);
    }

    #[test]
    fn docker_cli_timeout_caps_rm_and_leaves_other_commands() {
        let six_hours = Duration::from_secs(6 * 3600);
        assert_eq!(
            docker_cli_timeout(&force_remove_container_args(&["id".into()]), six_hours),
            Duration::from_secs(20)
        );
        assert_eq!(
            docker_cli_timeout(
                &["volume".into(), "rm".into(), "--force".into(), "v".into()],
                six_hours
            ),
            Duration::from_secs(20)
        );
        assert_eq!(
            docker_cli_timeout(&["ps".into(), "--all".into()], six_hours),
            six_hours
        );
    }

    #[test]
    fn docker_cli_result_never_treats_timeout_or_signal_as_success() {
        let remove = force_remove_container_args(&["container-id".into()]);
        assert!(classify_docker_cli_result(
            &remove,
            false,
            Some(137),
            true,
            true,
            "removal of container container-id is already in progress",
        )
        .is_err());
        assert!(classify_docker_cli_result(
            &remove,
            false,
            Some(9),
            true,
            false,
            "removal of container container-id is already in progress",
        )
        .is_err());
    }

    #[test]
    fn docker_cli_result_allows_only_explicit_removal_race() {
        let remove = force_remove_container_args(&["container-id".into()]);
        assert_eq!(
            classify_docker_cli_result(
                &remove,
                false,
                Some(1),
                false,
                false,
                "Error response from daemon: removal of container container-id is already in progress",
            )
            .unwrap(),
            DockerCliResult::RemovalAlreadyInProgress
        );
        assert!(classify_docker_cli_result(
            &["ps".into(), "--all".into()],
            false,
            Some(1),
            false,
            false,
            "removal of container container-id is already in progress",
        )
        .is_err());
        assert!(classify_docker_cli_result(
            &remove,
            false,
            Some(1),
            false,
            false,
            "already in progress",
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn docker_process_group_kill_targets_negative_private_pgid() {
        assert_eq!(
            docker_process_group_kill_args(42),
            vec!["-KILL", "--", "-42"]
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
    fn orphan_buildkit_listing_requires_velnor_job_label() {
        let args = list_job_buildkit_format_args();
        assert!(args.contains(&format!("label={JOB_ID_LABEL}")));
        assert!(args.contains(&format!("name={BUILDKIT_CONTAINER_NAME_PREFIX}")));
    }

    #[test]
    fn orphan_job_buildkit_ids_exclude_unlabeled_and_reclaim_only_terminal() {
        let daemon = "/var/lib/velnor/work";
        let live = BTreeSet::from(["velnor-job-live".to_string()]);
        let formatted = "\
id-live-created\tbuildx_buildkit_velnor-builder-live0\tvelnor-job-live\t/var/lib/velnor/work/slot-1\tcreated
id-dead-exited\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\texited
id-dead-dead\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\tdead
id-unlabeled\tbuildx_buildkit_velnor-builder-orphan0\t\t\tcreated
id-other\tpostgres\tvelnor-job-dead\t/var/lib/velnor/work/slot-2\trunning
";
        assert_eq!(
            orphan_job_buildkit_ids(formatted, &live, daemon),
            vec!["id-dead-dead".to_string(), "id-dead-exited".to_string(),]
        );
    }

    #[test]
    fn daemon_scoped_buildkit_reclaim_requires_daemon_ownership() {
        let daemon = "/var/lib/velnor-fleet/work";
        let formatted = "\
owned\tbuildx_buildkit_velnor-builder-owned0\tvelnor-job-old\t/var/lib/velnor-fleet/work/slot-1\texited
foreign\tbuildx_buildkit_velnor-builder-foreign0\tvelnor-job-foreign\t/var/lib/velnor-other/work\texited
unlabeled\tbuildx_buildkit_velnor-builder-unlabeled0\tvelnor-job-unlabeled\t\texited
";
        assert_eq!(
            orphan_job_buildkit_ids(formatted, &BTreeSet::new(), daemon),
            vec!["owned".to_string()]
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
    fn daemon_orphan_job_scopes_return_actual_root_and_slot_owners() {
        let daemon = "/var/lib/velnor-fleet/work";
        let formatted = "\
velnor-job-live\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning
guest-pg\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning
guest-old\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-2\trunning
velnor-job-dead\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-2\texited
velnor-job-root-dead\tvelnor-job-root-dead\t/var/lib/velnor-fleet/work\texited
other-dead\tother-dead\t/var/lib/velnor-other/work\texited
unlabeled-dead\tvelnor-job-unlabeled\t\texited
";
        assert_eq!(
            daemon_orphan_job_scopes(formatted, daemon),
            vec![
                (
                    "velnor-job-dead".to_string(),
                    "/var/lib/velnor-fleet/work/slot-2".to_string(),
                ),
                ("velnor-job-root-dead".to_string(), daemon.to_string(),),
            ]
        );
    }

    #[test]
    fn reclaim_daemon_orphan_jobs_reclaims_only_this_daemons_orphans() {
        let daemon = "/var/lib/velnor-fleet/work";
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning\nguest-old\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1\trunning\nother\tother\t/var/lib/velnor-other/work\texited\n"
                .to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1\texited\n"
                .to_string(),
            "guest-old\tguest-container\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1\texited\n"
                .to_string(),
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
            list_daemon_owned_containers_state_args(
                "velnor-job-dead",
                "/var/lib/velnor-fleet/work/slot-1",
            )
        );
        assert_eq!(calls[3], remove_one_container_args("guest-old"));
        assert_eq!(
            calls[4],
            list_daemon_owned_networks_args("velnor-job-dead", "/var/lib/velnor-fleet/work/slot-1",)
        );
        assert_eq!(
            calls[5],
            list_daemon_owned_volumes_args("velnor-job-dead", "/var/lib/velnor-fleet/work/slot-1",)
        );
        assert!(!calls[3].iter().any(|arg| arg == "--force"));
        assert_eq!(calls[8], list_daemon_buildkit_volume_format_args());
        // The other daemon's exited job is never looked up or reclaimed.
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "other")),
            "foreign daemon job leaked into reclaim calls: {calls:?}"
        );
    }

    #[test]
    fn startup_liveness_proof_reclaims_terminal_buildkit_and_state_volume_only() {
        let daemon = "/var/lib/velnor-fleet/work";
        let job = "velnor-job-stale";
        let owner = "/var/lib/velnor-fleet/work/slot-1";
        let container = format!(
            "stale-container\tbuildx_buildkit_velnor-builder-stale0\t{job}\t{owner}\texited\n\
             live-container\tbuildx_buildkit_velnor-builder-live0\t{job}\t{owner}\trunning\n\
             foreign-container\tbuildx_buildkit_velnor-builder-stale0\t{job}\t/var/lib/other/work\trunning\n\
             unlabeled-container\tbuildx_buildkit_velnor-builder-stale0\t{job}\t\trunning\n"
        );
        let volume = format!(
            "buildx_buildkit_velnor-builder-stale0_state\t{job}\t{owner}\n\
             buildx_buildkit_velnor-builder-live0_state\tvelnor-job-live\t{daemon}/slot-2\n\
             buildx_buildkit_velnor-builder-foreign0_state\t{job}\t/var/lib/other/work\n\
             buildx_buildkit_velnor-builder-unlabeled0_state\t{job}\t\n"
        );
        let live_jobs = BTreeSet::from(["velnor-job-live".to_string()]);
        let stale_scopes = vec![(job.to_string(), owner.to_string())];
        let mut calls = Vec::new();
        let mut outputs = vec![
            container.clone(),
            container,
            String::new(),
            volume.clone(),
            format!("live-container\tvelnor-job-live\t{daemon}/slot-2\trunning\n"),
            volume,
            String::new(),
        ];
        reclaim_startup_buildkit_after_liveness_proof(
            &live_jobs,
            &stale_scopes,
            daemon,
            &mut |args| {
                calls.push(args.to_vec());
                Ok(outputs.remove(0))
            },
        )
        .unwrap();

        assert_eq!(calls[0], list_exact_job_buildkit_format_args(job, owner));
        assert_eq!(calls[1], list_exact_job_buildkit_format_args(job, owner));
        assert_eq!(calls[2], remove_one_container_args("stale-container"));
        assert_eq!(calls[3], list_daemon_buildkit_volume_format_args());
        assert_eq!(calls[4], list_daemon_owned_job_format_args());
        assert_eq!(calls[5], list_daemon_buildkit_volume_format_args());
        assert_eq!(
            calls[6],
            remove_volume_args(&["buildx_buildkit_velnor-builder-stale0_state".into()])
        );
        assert!(!calls[2].contains(&"--force".into()));
        assert!(!calls[6].contains(&"--force".into()));
        assert!(calls.iter().all(|call| {
            !call.iter().any(|argument| {
                argument == "foreign-container"
                    || argument == "live-container"
                    || argument == "unlabeled-container"
                    || argument == "buildx_buildkit_velnor-builder-live0_state"
                    || argument == "buildx_buildkit_velnor-builder-foreign0_state"
            })
        }));
    }

    #[test]
    fn startup_liveness_proof_skips_foreign_and_live_protected_scopes() {
        let daemon = "/var/lib/velnor-fleet/work";
        let live_job = "velnor-job-live";
        let live_owner = format!("{daemon}/slot-1");
        let live_jobs = BTreeSet::from([live_job.to_string()]);
        let stale_scopes = vec![
            (live_job.to_string(), live_owner),
            (
                "velnor-job-foreign".to_string(),
                "/var/lib/other/work".to_string(),
            ),
        ];
        let mut called = false;
        reclaim_startup_buildkit_after_liveness_proof(
            &live_jobs,
            &stale_scopes,
            daemon,
            &mut |_| {
                called = true;
                Ok(String::new())
            },
        )
        .unwrap();
        assert!(
            !called,
            "foreign and live-protected scopes must not be listed"
        );
    }

    #[test]
    fn startup_buildkit_volume_live_race_skips_delete() {
        let daemon = "/var/lib/velnor-fleet/work";
        let job = "velnor-job-race";
        let owner = "/var/lib/velnor-fleet/work/slot-4";
        let builder =
            format!("builder-id\tbuildx_buildkit_velnor-builder-race0\t{job}\t{owner}\texited\n");
        let volume = format!("buildx_buildkit_velnor-builder-race0_state\t{job}\t{owner}\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            builder.clone(),
            String::new(),
            builder,
            String::new(),
            volume.clone(),
            format!("{job}\t{job}\t{owner}\trunning\n"),
            volume,
        ];
        let mut docker = |args: &[String]| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        };
        let result = reclaim_orphan_job_buildkit_with_live(
            &BTreeSet::new(),
            &[(job.to_string(), owner.to_string())],
            daemon,
            &mut docker,
        );

        result.unwrap();
        assert_eq!(calls[4], list_daemon_buildkit_volume_format_args());
        assert_eq!(calls[5], list_daemon_owned_job_format_args());
        assert_eq!(calls[6], list_daemon_buildkit_volume_format_args());
        assert!(!calls.iter().any(|call| {
            call == &remove_volume_args(&["buildx_buildkit_velnor-builder-race0_state".into()])
        }));
    }

    #[test]
    fn startup_buildkit_volume_revalidates_exact_owner_before_delete() {
        let daemon = "/var/lib/velnor-fleet/work";
        let job = "velnor-job-owner-race";
        let owner = "/var/lib/velnor-fleet/work";
        let foreign_owner = "/var/lib/velnor-other/work";
        let mut calls = Vec::new();
        let mut outputs = vec![
            String::new(),
            String::new(),
            format!("buildx_buildkit_velnor-builder-owner-race_state\t{job}\t{owner}\n"),
            String::new(),
            format!("buildx_buildkit_velnor-builder-owner-race_state\t{job}\t{foreign_owner}\n"),
        ];
        let mut docker = |args: &[String]| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        };
        let result = reclaim_orphan_job_buildkit_with_live(
            &BTreeSet::new(),
            &[(job.to_string(), owner.to_string())],
            daemon,
            &mut docker,
        );

        result.unwrap();
        assert_eq!(calls[2], list_daemon_buildkit_volume_format_args());
        assert_eq!(calls[3], list_daemon_owned_job_format_args());
        assert_eq!(calls[4], list_daemon_buildkit_volume_format_args());
        assert!(!calls.iter().any(|call| {
            call == &remove_volume_args(&["buildx_buildkit_velnor-builder-owner-race_state".into()])
        }));
    }

    #[test]
    fn startup_reclaims_labeled_buildkit_volume_without_surviving_container() {
        let daemon = "/var/lib/velnor-fleet/work";
        let owned = "buildx_buildkit_velnor-builder-volume-only_state";
        let live = "buildx_buildkit_velnor-builder-live_state";
        let foreign = "buildx_buildkit_velnor-builder-foreign_state";
        let volumes = format!(
            "{owned}\tvelnor-job-dead\t{daemon}/slot-1\n\
             {live}\tvelnor-job-live\t{daemon}/slot-2\n\
             {foreign}\tvelnor-job-foreign\t/var/lib/other/work\n\
             malformed\tvelnor-job-dead\n"
        );
        let live_jobs = format!("velnor-job-live\tvelnor-job-live\t{daemon}/slot-2\trunning\n");
        let mut calls = Vec::new();
        let mut outputs = vec![
            String::new(),
            live_jobs.clone(),
            volumes.clone(),
            live_jobs,
            volumes,
            String::new(),
        ];
        let mut docker = |args: &[String]| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        };

        reclaim_orphan_job_buildkit_with_live(&BTreeSet::new(), &[], daemon, &mut docker).unwrap();

        assert_eq!(calls[0], list_job_buildkit_format_args());
        assert_eq!(calls[1], list_daemon_owned_job_format_args());
        assert_eq!(calls[2], list_daemon_buildkit_volume_format_args());
        assert_eq!(calls[3], list_daemon_owned_job_format_args());
        assert_eq!(calls[4], list_daemon_buildkit_volume_format_args());
        assert_eq!(calls[5], remove_volume_args(&[owned.into()]));
        assert!(calls.iter().all(|call| {
            !call
                .iter()
                .any(|argument| argument == live || argument == foreign)
        }));
        assert!(!calls[5].iter().any(|argument| argument == "--force"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_client_finishes_when_engine_honors_injected_connection_close() {
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
        let engine_thread = std::thread::spawn(move || {
            let (mut sock, _) = engine.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = sock.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .unwrap();
            if req.contains("connection: close") {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            } else {
                std::thread::sleep(Duration::from_secs(30));
            }
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
        let mut buf = [0_u8; 256];
        let n = client.read(&mut buf).unwrap();
        assert!(
            std::str::from_utf8(&buf[..n]).unwrap().contains("200 OK"),
            "guest should receive the ping response, got {}",
            String::from_utf8_lossy(&buf[..n])
        );
        // The proxy EOFs our read side (Engine closed after `Connection:
        // close`); a real CLI then closes the socket. Do the same so the
        // proxy's guest→host copy can finish.
        drop(client);
        rx.recv_timeout(Duration::from_secs(2))
            .expect("handle_client must return after injecting Connection: close; keepalive io::copy deadlocks docker CLI")
            .unwrap();
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
    fn accept_owned_container_request(
        engine: &std::os::unix::net::UnixListener,
        job_id: &str,
        daemon_id: &str,
    ) -> std::os::unix::net::UnixStream {
        let (mut inspect, _) = engine.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = inspect.read(&mut request);
        let body = format!(
            "{{\"Config\":{{\"Labels\":{{\"{JOB_ID_LABEL}\":\"{job_id}\",\"{DAEMON_ID_LABEL}\":\"{daemon_id}\"}}}}}}"
        );
        inspect
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        drop(inspect);
        let (stream, _) = engine.accept().unwrap();
        stream
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
            let mut sock = accept_owned_container_request(&engine, "job", "daemon");
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
            .write_all(b"POST /v1.43/containers/abc/start HTTP/1.1\r\nHost: docker\r\nContent-Length: 0\r\n\r\n")
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
            let mut sock = accept_owned_container_request(&engine, "job", "daemon");
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
                b"POST /v1.54/containers/abc/attach?stderr=1&stdout=1&stream=1 HTTP/1.1\r\n\
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
            let mut sock = accept_owned_container_request(&engine, "job", "daemon");
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
            .write_all(b"POST /v1.43/containers/abc/start HTTP/1.1\r\nHost: docker\r\nContent-Length: 0\r\n\r\n")
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

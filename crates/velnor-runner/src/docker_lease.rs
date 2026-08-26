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
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
pub const TESTCONTAINERS_LABEL: &str = "org.testcontainers.managed-by=testcontainers";
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";
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

/// One container id. BuildKit reclaim must never batch ids into one `docker rm`.
pub fn force_remove_one_container_args(id: &str) -> Vec<String> {
    vec!["rm".into(), "--force".into(), id.to_string()]
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
        if let Err(error) = docker(&force_remove_one_container_args(id)) {
            if first_error.is_none() {
                first_error = Some(error.context(format!("remove BuildKit container {id}")));
            }
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

pub fn force_remove_volume_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["volume".into(), "rm".into(), "--force".into()];
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
    let mut live_jobs = std::collections::BTreeSet::new();
    let mut seen_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if job_id.is_empty() {
            continue;
        }
        seen_jobs.insert(job_id.to_string());
        if name == job_id && state.eq_ignore_ascii_case("running") {
            live_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !live_jobs.contains(job_id))
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
    let mut live_jobs = std::collections::BTreeSet::new();
    let mut seen_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let owner = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if job_id.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        seen_jobs.insert(job_id.to_string());
        if name == job_id && state.eq_ignore_ascii_case("running") {
            live_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !live_jobs.contains(job_id))
        .collect()
}

/// IDs of `velnor/job-ubuntu` siblings with no job label (docker-generated names).
pub fn unlabeled_job_image_ids(formatted: &str) -> Vec<String> {
    unlabeled_testcontainer_ids(formatted)
}

pub fn live_job_ids(formatted: &str) -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if !job_id.is_empty() && name == job_id && state.eq_ignore_ascii_case("running") {
            live.insert(job_id.to_string());
        }
    }
    live
}

pub fn live_daemon_job_ids(formatted: &str, daemon_id: &str) -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let owner = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if job_id.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        if name == job_id && state.eq_ignore_ascii_case("running") {
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
        let mut parts = line.split('\t');
        let id = parts.next().unwrap_or("").trim();
        let names = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let owner = parts.next().unwrap_or("").trim();
        if id.is_empty() || !names.contains(BUILDKIT_CONTAINER_NAME_PREFIX) {
            continue;
        }
        if let Some(daemon_id) = daemon_id {
            if !owner.is_empty() && !daemon_owns_label(owner, daemon_id) {
                continue;
            }
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
    let ids = orphan_job_buildkit_ids(&formatted, live_jobs, daemon_id);
    if !ids.is_empty() {
        force_remove_containers_serially(&ids, |args| docker(args).map(|_| ()))?;
    }
    let volumes = docker(&list_job_buildkit_volume_args())?;
    let volume_ids: Vec<String> = volumes
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
            !live_jobs.contains(&job)
                && !live_jobs
                    .iter()
                    .any(|live| name.contains(live.trim_start_matches("velnor-job-")))
        })
        .map(ToOwned::to_owned)
        .collect();
    if !volume_ids.is_empty() {
        docker(&force_remove_volume_args(&volume_ids)).map(|_| ())?;
    }
    Ok(())
}

/// IDs of testcontainers that were created before the lease proxy (no job label).
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
    let mut value: Value = serde_json::from_slice(body)
        .context("parse Docker create JSON so ownership labels can be injected")?;
    let Some(object) = value.as_object_mut() else {
        bail!("Docker create body must be a JSON object");
    };
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

pub fn reclaim_orphan_jobs(mut docker: impl FnMut(&[String]) -> Result<String>) -> Result<()> {
    let formatted = docker(&list_owned_job_format_args())?;
    for job_id in orphan_job_ids(&formatted) {
        reclaim_job_owned(&job_id, &mut docker)?;
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
        reclaim_job_owned(&job_id, &mut docker)?;
    }
    let live = live_daemon_job_ids(&formatted, daemon_id);
    reclaim_orphan_job_buildkit_with_live(&live, Some(daemon_id), &mut docker)
}

pub fn reclaim_unlabeled_testcontainers(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_testcontainers_format_args())?;
    let ids = unlabeled_testcontainer_ids(&formatted);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&force_remove_container_args(&ids)).map(|_| ())
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

pub fn run_host_docker(args: &[String]) -> Result<String> {
    if args.first().is_some_and(|arg| arg == "rm")
        || (matches!(args.first().map(String::as_str), Some("volume" | "network"))
            && args.get(1).map(String::as_str) == Some("rm"))
    {
        return run_host_docker_bounded(args, docker_cli_timeout(args, DOCKER_RM_TIMEOUT));
    }
    let output = std::process::Command::new("docker")
        .args(args)
        .output()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already in progress") {
            return Ok(String::new());
        }
        bail!("docker {} failed: {}", args.join(" "), stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_host_docker_bounded(args: &[String], timeout: std::time::Duration) -> Result<String> {
    let rm_claim = claim_docker_container_rm(args);
    if let Some(claim) = rm_claim.as_ref() {
        if claim.ids.is_empty() {
            return Ok(String::new());
        }
    }
    let claimed_args = rm_claim
        .as_ref()
        .map(|claim| force_remove_container_args(&claim.ids));
    let args = claimed_args.as_deref().unwrap_or(args);
    let child = std::process::Command::new("docker")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    let pid = child.id();
    let (cancel, cancelled) = std::sync::mpsc::channel();
    let killer = std::thread::spawn(move || {
        if cancelled.recv_timeout(timeout).is_err() {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    });
    let output = child
        .wait_with_output()
        .with_context(|| format!("wait docker {}", args.join(" ")))?;
    let _ = cancel.send(());
    let _ = killer.join();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.code() == Some(137)
            || output.status.code() == Some(9)
            || stderr.contains("already in progress")
        {
            return Ok(String::new());
        }
        bail!("docker {} failed: {}", args.join(" "), stderr);
    }
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

    let _client_watch = conns.watch(&client);
    if conns.is_shutdown() {
        return Ok(());
    }
    let request = match read_http_request(&mut client) {
        Ok(request) => request,
        Err(_) if conns.is_shutdown() => return Ok(()),
        Err(error) => return Err(error),
    };
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
    let _host_watch = conns.watch(&host);
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
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
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
            "guest-old\n".to_string(),
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
        assert_eq!(calls[1], list_owned_containers_args("velnor-job-dead"));
        assert_eq!(calls[2], force_remove_container_args(&["guest-old".into()]));
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
    fn reclaim_orphan_job_buildkit_force_removes_created_removing_of_ended_jobs() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\trunning\nvelnor-job-dead\tvelnor-job-dead\texited\n"
                .to_string(),
            String::new(),
            String::new(),
            String::new(),
            "id-live\tbuildx_buildkit_velnor-builder-live0\tvelnor-job-live\t\tcreated\n\
             id-created\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tcreated\n\
             id-removing\tbuildx_buildkit_velnor-builder-dead0\tvelnor-job-dead\t\tremoving\n"
                .to_string(),
            String::new(),
            String::new(),
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
            .any(|call| call == &force_remove_one_container_args("id-created")));
        assert!(calls
            .iter()
            .any(|call| call == &force_remove_one_container_args("id-removing")));
        assert!(calls
            .iter()
            .any(|call| call == &list_job_buildkit_volume_args()));
        assert!(calls.iter().any(|call| {
            call.starts_with(&["volume".into(), "rm".into(), "--force".into()])
                && call.contains(&"buildx_buildkit_velnor-builder-dead0_state".to_string())
                && !call.contains(&"buildx_buildkit_velnor-builder-live0_state".to_string())
        }));
        assert!(!calls.iter().any(|call| {
            call.get(2) == Some(&"id-live".to_string()) && call.first().is_some_and(|a| a == "rm")
        }));
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
    fn reclaim_unlabeled_testcontainers_force_removes_orphans_only() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "dead1\t\nlive1\tvelnor-job-now\ndead2\t\n".to_string(),
            String::new(),
        ];
        reclaim_unlabeled_testcontainers(|args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_testcontainers_format_args());
        assert_eq!(
            calls[1],
            force_remove_container_args(&["dead1".into(), "dead2".into()])
        );
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
    fn reclaim_daemon_orphan_jobs_reclaims_only_this_daemons_orphans() {
        let daemon = "/var/lib/velnor-fleet/work";
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\t/var/lib/velnor-fleet/work/slot-1\trunning\nguest-old\tvelnor-job-dead\t/var/lib/velnor-fleet/work/slot-1\trunning\nother\tother\t/var/lib/velnor-other/work\texited\n"
                .to_string(),
            "guest-old\n".to_string(),
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
        assert_eq!(calls[1], list_owned_containers_args("velnor-job-dead"));
        assert_eq!(calls[2], force_remove_container_args(&["guest-old".into()]));
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

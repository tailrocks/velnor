//! Typed owner of host Docker control-plane calls.
//!
//! Before this module, the runner talked to the host Engine through a dozen
//! free argument builders whose tab-separated `--format` output was re-parsed
//! by hand at every call site: eleven functions split the same shapes, and a
//! caller that needed a container's state reached past the policy layer for a
//! go-template string. That is why one missing label could fail a whole
//! snapshot in one place and silently pass in another — there was no single
//! owner of what the output means.
//!
//! [`Docker`] is that owner. Every query here returns a typed value; the
//! argument vectors and their parsers live in this file and nowhere else.
//! Control-plane methods take no timeout: the [`crate::executor`] runner seam
//! and the host transport below both apply
//! [`crate::docker::deadline_for`], so a step deadline is inexpressible at
//! these signatures by construction, not by convention.
//!
//! Two transports, one policy. The job path borrows the job's
//! [`CommandRunner`](crate::executor::CommandRunner) ([`Docker::job`]), which
//! registers the process group for cancellation and attributes metrics to the
//! running job. Maintenance, startup, doctor, and the cancellation ladder have
//! no runner, so they use the host transport ([`Docker::host`]), which spawns
//! directly with the same per-class deadlines, the same invocation metrics,
//! and the in-flight `rm` claim that keeps two daemons from deadlocking on one
//! `Created` BuildKit. Both transports observe through
//! [`crate::docker::observe`]: there is still exactly one invocation counter.
//!
//! What this module deliberately does not own:
//!
//! * `run`/`exec`/`create` argument construction. That stays at its single
//!   sites (`crate::docker_argv`, `crate::container`) per the one-construction-site
//!   rule; this client owns queries, not workload configuration.
//! * Removal tolerance. `rm` failure handling differs per path (teardown
//!   tolerates in-flight deletes and bounded timeouts, orphan sweeps do not),
//!   so removals stay at their owned call sites, all deadline-wired.
//!
//! A missing `{{.Label}}` renders as an empty field on current Engines
//! (proven live on 29.4.0); the row parsers treat empty as absent.
//! Live-shape provenance: every parser test in this module feeds output
//! captured from a real Engine 29.4.0 invocation of the exact argument vector
//! the method runs — never hand-shaped fixtures.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use crate::executor::{CommandResult, CommandRunner};

// ---------------------------------------------------------------------------
// Typed results
// ---------------------------------------------------------------------------

/// Lifecycle state of a container, as reported by the Engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainerState {
    Created,
    Running,
    Restarting,
    Removing,
    Paused,
    Exited,
    Dead,
}

impl ContainerState {
    /// Parse an Engine state word. `None` means the daemon answered with a
    /// state this client does not know; callers treat that as their own
    /// fail-closed direction (liveness checks treat unknown as alive,
    /// deletion checks skip the row).
    pub(crate) fn parse(value: &str) -> Option<Self> {
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

    /// True when deleting the container cannot interrupt running work.
    #[must_use]
    pub(crate) fn safe_to_reclaim(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Removing | Self::Exited | Self::Dead
        )
    }
}

/// A container's readiness as the service wait loop reads it: the health
/// status when a healthcheck exists, else the lifecycle status. Mirrors the
/// `{{if .State.Health}}...{{else}}...{{end}}` projection this replaces, which
/// is why `Paused` and friends stay reachable here instead of collapsing to a
/// boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Readiness {
    Healthy,
    Running,
    Starting,
    Unhealthy,
    Exited,
    Dead,
    /// Empty answer: container gone between list and inspect, or an Engine
    /// that reported nothing. The wait loop treats it as ready — there is
    /// nothing left to wait for.
    Gone,
    /// A status word this client does not know. The wait loop keeps waiting
    /// on it (never ready, never stopped): an unknown state is not evidence
    /// in either direction.
    Other,
}

impl Readiness {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim() {
            "healthy" => Self::Healthy,
            "running" => Self::Running,
            "starting" => Self::Starting,
            "unhealthy" => Self::Unhealthy,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ if value.trim().is_empty() => Self::Gone,
            _ => Self::Other,
        }
    }

    /// True when the wait loop may proceed.
    #[must_use]
    pub(crate) fn ready(self) -> bool {
        matches!(self, Self::Healthy | Self::Running | Self::Gone)
    }

    /// True when the container stopped before becoming ready: fail, do not wait.
    #[must_use]
    pub(crate) fn stopped(self) -> bool {
        matches!(self, Self::Exited | Self::Dead)
    }
}

/// One `docker port` mapping: container port with protocol, host address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortMapping {
    /// `8080/tcp` as the Engine reports it; callers strip the suffix.
    pub container_port: String,
    pub host_address: String,
}

/// The Engine's cgroup driver and version: the daemon-generation fact the job
/// cgroup boundary proof caches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CgroupDriver {
    pub driver: String,
    pub version: String,
}

/// A `docker` object the daemon positively reports missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotFound {
    pub object: String,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "docker object '{}' does not exist", self.object)
    }
}

impl std::error::Error for NotFound {}

/// True when `error` is a daemon positive-missing answer surfaced through this
/// client — the only signal any caller may treat as proof of absence.
pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NotFound>().is_some()
}

/// The daemon's missing-object vocabulary, both generations: modern Engines
/// answer `No such container|object|image|volume`, older ones `no such ...`.
/// Deliberately narrow: the cancellation ladder treats any other failure as
/// "still alive", so a loose match here would stop the ladder early.
fn daemon_reports_missing(stderr: &str) -> bool {
    stderr.contains("No such") || stderr.contains("no such")
}

// ---------------------------------------------------------------------------
// Fixed query shapes
// ---------------------------------------------------------------------------

/// Health status when a healthcheck exists, else the lifecycle status.
/// Kept as a projection rather than `--format=json` on purpose: the service
/// wait loop polls this up to ~10 times per service, and the projection
/// answers in bytes where the JSON document costs kilobytes. Owned here so no
/// call site spells a template.
const CONTAINER_READINESS_FORMAT: &str =
    "{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}";
const CONTAINER_RUNNING_FORMAT: &str = "{{.State.Running}}";
const CONTAINER_ID_FORMAT: &str = "{{.Id}}";
const IMAGE_ID_FORMAT: &str = "{{.Id}}";
const CGROUP_FORMAT: &str = "{{.CgroupDriver}} {{.CgroupVersion}}";

// Every name operand sits behind `--`: names that reach the Engine are
// runner-generated, but a leading dash must never be able to turn an operand
// into a flag.
pub(crate) fn readiness_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_READINESS_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn running_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_RUNNING_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn container_id_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_ID_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn image_id_args(reference: &str) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        "-f".to_string(),
        IMAGE_ID_FORMAT.to_string(),
        "--".to_string(),
        reference.to_string(),
    ]
}

pub(crate) fn daemon_cgroup_args() -> Vec<String> {
    vec![
        "info".to_string(),
        "--format".to_string(),
        CGROUP_FORMAT.to_string(),
    ]
}

pub(crate) fn mapped_ports_args(name: &str) -> Vec<String> {
    vec!["port".to_string(), "--".to_string(), name.to_string()]
}

// ---------------------------------------------------------------------------
// Parsers: one owner per output shape
// ---------------------------------------------------------------------------

/// Parse a `--quiet` id listing: one id per line, sorted and deduplicated.
pub(crate) fn parse_id_list(stdout: &str) -> Vec<String> {
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

/// Parse `docker port` output (`container-port/proto -> host:port`) into
/// mappings. Malformed lines are skipped: a service with one unparsable
/// mapping still reports the rest.
pub(crate) fn parse_port_mappings(text: &str) -> Vec<PortMapping> {
    let mut mappings = Vec::new();
    for line in text.lines() {
        let Some((container_port, address)) = line.split_once(" -> ") else {
            continue;
        };
        let Some((_, _host_port)) = address.rsplit_once(':') else {
            continue;
        };
        mappings.push(PortMapping {
            container_port: container_port.to_string(),
            host_address: address.to_string(),
        });
    }
    mappings
}

/// Parse the cgroup projection (`<driver> <version>`) into its typed fact.
pub(crate) fn parse_cgroup_projection(output: &str) -> Result<CgroupDriver> {
    let mut words = output.split_whitespace();
    let (Some(driver), Some(version), None) = (words.next(), words.next(), words.next()) else {
        anyhow::bail!("docker info cgroup probe answered {output:?}");
    };
    Ok(CgroupDriver {
        driver: driver.to_string(),
        version: version.to_string(),
    })
}

/// Names of the buildx builders Velnor owns, from `docker buildx ls` output.
///
/// The builder name always carries a scope suffix; ownership is the
/// `velnor-builder` prefix, so enumerate and match the prefix instead of
/// guessing one name.
pub(crate) fn owned_builder_names(buildx_ls_stdout: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in buildx_ls_stdout.lines() {
        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        // `docker buildx ls` marks the selected builder with a trailing `*` and
        // indents each builder's nodes; nodes are not builders.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let name = first.trim_end_matches('*');
        if name.starts_with(crate::cache::OWNED_BUILDER_PREFIX)
            && !names.iter().any(|seen| seen == name)
        {
            names.push(name.to_string());
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Reclaim decisions over the listings
// ---------------------------------------------------------------------------

/// True when a `velnor.daemon-id` label value belongs to `daemon_id`: either
/// the shared work root itself or one of its direct `slot-N` children (job
/// containers are labelled with the slot work directory, while daemon
/// startup knows only the shared root).
pub(crate) fn daemon_owns_label(owner: &str, daemon_id: &str) -> bool {
    if owner == daemon_id {
        return true;
    }
    std::path::Path::new(owner).parent() == Some(std::path::Path::new(daemon_id))
        && std::path::Path::new(owner)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("slot-").is_some_and(|slot| {
                    !slot.is_empty() && slot.chars().all(|c| c.is_ascii_digit())
                })
            })
}

pub(crate) struct StaleJobOwnedSnapshot {
    pub stopped_container_ids: Vec<String>,
    pub job_container_absent_or_stopped: bool,
}

/// Parse a reclaim snapshot only when it is structurally valid. The explicit
/// job-container proof is checked on both the initial and immediate snapshots
/// before any owned network or volume can be removed.
pub(crate) fn stale_job_owned_snapshot(
    job_id: &str,
    formatted: &str,
) -> Option<StaleJobOwnedSnapshot> {
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
        let state = ContainerState::parse(fields[3])?;
        if id.is_empty() || name.is_empty() || label != job_id {
            return None;
        }
        if name == job_id {
            job_container_present = true;
            job_container_stopped &= state.safe_to_reclaim();
        }
        if !name.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
            && state.safe_to_reclaim()
        {
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
pub(crate) fn owned_container_ids_excluding_buildkit(formatted: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (id, names) = line.split_once('\t').unwrap_or((line, line));
        let id = id.trim();
        let names = names.trim();
        if id.is_empty() || names.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX) {
            continue;
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Current-job builders including Created/removing. Job-end must delete them
/// even while the job container is still running (cleanup happens before rm).
pub(crate) fn job_buildkit_ids_for_job(formatted: &str, job_id: &str, scope: &str) -> Vec<String> {
    let needle = format!(
        "{}{scope}",
        crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX
    );
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

pub(crate) fn orphan_job_buildkit_ids(
    formatted: &str,
    live_jobs: &BTreeSet<String>,
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
        let Some(state) = ContainerState::parse(fields[4]) else {
            continue;
        };
        if id.is_empty()
            || !names.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
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

pub(crate) fn live_job_ids(formatted: &str) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
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
        let Some(state) = ContainerState::parse(fields[2]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

pub(crate) fn live_daemon_job_ids(formatted: &str, daemon_id: &str) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
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
        let Some(state) = ContainerState::parse(fields[3]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

/// Job ids whose job container is not running. Guest objects for those jobs are orphans.
pub(crate) fn orphan_job_ids(formatted: &str) -> Vec<String> {
    let mut seen_jobs = BTreeSet::new();
    let mut protected_jobs = BTreeSet::new();
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
        let Some(state) = ContainerState::parse(fields[2]) else {
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

/// Orphan job ids restricted to containers owned by `daemon_id` (see
/// [`daemon_owns_label`]). Input is the
/// `name \t job-id \t daemon-id \t state` row format.
pub(crate) fn daemon_orphan_job_ids(formatted: &str, daemon_id: &str) -> Vec<String> {
    let mut seen_jobs = BTreeSet::new();
    let mut protected_jobs = BTreeSet::new();
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
        let Some(state) = ContainerState::parse(fields[3]) else {
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

/// IDs of testcontainers that were created before the lease proxy (no job label).
///
/// This remains a best-effort inspection helper for compatibility. Automatic
/// cleanup must use [`validate_legacy_testcontainer_listing`], which refuses
/// both malformed rows and every unlabeled container before any delete call.
pub(crate) fn unlabeled_testcontainer_ids(formatted: &str) -> Vec<String> {
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

/// IDs of `velnor/job-ubuntu` siblings with no job label (docker-generated names).
pub(crate) fn unlabeled_job_image_ids(formatted: &str) -> Vec<String> {
    unlabeled_testcontainer_ids(formatted)
}

/// A legacy Testcontainers listing is not an ownership proof. Keep its
/// failure modes typed so callers cannot accidentally turn an inspection
/// result into a destructive cleanup decision.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LegacyTestcontainerReclaimError {
    #[error("refusing legacy Testcontainers reclaim: malformed docker ps row {line}: {row:?}")]
    MalformedRow { line: usize, row: String },
    #[error(
        "refusing legacy Testcontainers reclaim: unlabeled containers have no Velnor ownership proof: {ids:?}"
    )]
    Unlabeled { ids: Vec<String> },
}

/// Validate the legacy Testcontainers listing without producing deleteable
/// IDs. Rows carrying a Velnor job label are intentionally ignored here;
/// their cleanup belongs to the job-owned reclaim paths. An unlabeled row is
/// a hard refusal because the `org.testcontainers.managed-by` label proves
/// only Testcontainers origin, not Velnor ownership.
pub(crate) fn validate_legacy_testcontainer_listing(
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

pub(crate) fn daemon_owned_buildkit_volume_names(
    formatted: &str,
    daemon_id: &str,
    protected_jobs: &BTreeSet<String>,
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
                || !name.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
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

// ---------------------------------------------------------------------------
// In-flight `rm` claims
// ---------------------------------------------------------------------------

static IN_FLIGHT_CONTAINER_RM: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

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

pub(crate) fn claim_docker_container_rm(args: &[String]) -> Option<DockerContainerRmClaim> {
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

// ---------------------------------------------------------------------------
// Host transport (maintenance, startup, doctor, cancellation)
// ---------------------------------------------------------------------------

const HOST_DOCKER_ENDPOINT: &str = "unix:///var/run/docker.sock";

/// Host maintenance runs no job step, so a payload-classified command reaching
/// the host transport has no step deadline to inherit. Thirty minutes is the
/// registry-transfer bound: long enough for any maintenance pull, finite.
pub(crate) const MAINTENANCE_PAYLOAD_DEADLINE: Duration = Duration::from_secs(1800);

fn host_docker_command(args: &[String]) -> Result<std::process::Command> {
    let mut command = std::process::Command::new("docker");
    crate::executor::configure_host_docker_command(&mut command, "docker", args)?;
    command
        .env("DOCKER_HOST", HOST_DOCKER_ENDPOINT)
        .env_remove("DOCKER_CONTEXT");
    Ok(command)
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
    use std::io::Read as _;
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

/// Run one host `docker` command under the deadline its operation class earns.
///
/// Every path is bounded. Before the deadline policy existed only the `rm`
/// family was, and every other maintenance call — `ps`, `inspect`, reclaim
/// listings — waited on a wedged daemon forever.
pub(crate) fn host_call(args: &[String]) -> Result<String> {
    let (_, deadline) = crate::docker::deadline_for(args, MAINTENANCE_PAYLOAD_DEADLINE);
    host_call_bounded(args, deadline)
}

/// Run one host `docker` command under an explicit deadline.
///
/// Expiry is a failure. The process is SIGKILLed and the caller gets a typed
/// [`crate::docker::DockerTimeout`] naming the operation class and what to look
/// at, never an empty success.
pub(crate) fn host_call_bounded(args: &[String], timeout: Duration) -> Result<String> {
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
        anyhow::bail!("docker {} failed: {}", args.join(" "), stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

enum Transport<'r> {
    Job(&'r mut dyn CommandRunner),
    Host,
}

/// The typed owner of host Docker control-plane queries.
///
/// Construct with [`Docker::job`] on the job path (cancellation-aware,
/// metrics-attributed) or [`Docker::host`] on the maintenance/cancel path.
/// Every method runs exactly one `docker` process under its class deadline
/// and returns a typed value; a daemon that positively reports an object
/// missing surfaces as [`NotFound`], never as an empty success.
pub(crate) struct Docker<'r> {
    transport: Transport<'r>,
}

impl<'r> Docker<'r> {
    pub(crate) fn job(runner: &'r mut dyn CommandRunner) -> Self {
        Self {
            transport: Transport::Job(runner),
        }
    }

    pub(crate) fn host() -> Self {
        Self {
            transport: Transport::Host,
        }
    }

    /// Run one query and return its stdout. Non-zero exits become errors:
    /// a daemon missing-object answer becomes [`NotFound`], a runner timeout
    /// (exit 124) becomes the operation's [`crate::docker::DockerTimeout`],
    /// anything else carries the stderr.
    fn call(&mut self, args: &[String], object: &str) -> Result<String> {
        match &mut self.transport {
            Transport::Host => host_call(args).map_err(|error| {
                if daemon_reports_missing(&format!("{error:#}")) {
                    anyhow::Error::new(NotFound {
                        object: object.to_string(),
                    })
                } else {
                    error
                }
            }),
            Transport::Job(runner) => {
                let result: CommandResult = runner
                    .run("docker", args)
                    .with_context(|| format!("query docker {object}"))?;
                if result.code == 0 {
                    return Ok(result.stdout);
                }
                if result.code == 124 {
                    let (op, deadline) =
                        crate::docker::deadline_for(args, crate::executor::DEFAULT_STEP_TIMEOUT);
                    return Err(anyhow::Error::new(crate::docker::DockerTimeout::new(
                        op, deadline,
                    )));
                }
                if daemon_reports_missing(&result.stderr) {
                    return Err(anyhow::Error::new(NotFound {
                        object: object.to_string(),
                    }));
                }
                anyhow::bail!(
                    "docker query {object} exited {}: {}",
                    result.code,
                    result.stderr.trim()
                );
            }
        }
    }

    /// Readiness of one container: health status when a healthcheck exists,
    /// else the lifecycle status.
    pub(crate) fn container_readiness(&mut self, name: &str) -> Result<Readiness> {
        let args = readiness_args(name);
        Ok(Readiness::parse(&self.call(&args, name)?))
    }

    /// Whether the Engine reports the container running. A positive-missing
    /// answer reads as not running; any other failure propagates so the
    /// caller takes its own fail-closed direction.
    pub(crate) fn container_running(&mut self, name: &str) -> Result<bool> {
        let args = running_args(name);
        match self.call(&args, name) {
            Ok(output) => Ok(output.trim() == "true"),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Full id of one container.
    pub(crate) fn container_id(&mut self, name: &str) -> Result<String> {
        let args = container_id_args(name);
        Ok(self.call(&args, name)?.trim().to_string())
    }

    /// Resolved id of one image reference.
    pub(crate) fn image_id(&mut self, reference: &str) -> Result<String> {
        let args = image_id_args(reference);
        Ok(self.call(&args, reference)?.trim().to_string())
    }

    /// Published ports of one container.
    pub(crate) fn mapped_ports(&mut self, name: &str) -> Result<Vec<PortMapping>> {
        let args = mapped_ports_args(name);
        Ok(parse_port_mappings(&self.call(&args, name)?))
    }

    /// The Engine's cgroup driver and version.
    pub(crate) fn daemon_cgroup(&mut self) -> Result<CgroupDriver> {
        let args = daemon_cgroup_args();
        parse_cgroup_projection(&self.call(&args, "daemon cgroup")?)
    }

    /// Names of the buildx builders Velnor owns.
    pub(crate) fn buildx_builders(&mut self) -> Result<Vec<String>> {
        let args = vec!["buildx".to_string(), "ls".to_string()];
        Ok(owned_builder_names(&self.call(&args, "buildx builders")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Every fixture below is output captured from a real Engine 29.4.0
    /// invocation of the exact argument vector the parser consumes.

    #[test]
    fn readiness_maps_every_status_word_to_the_wait_loop_decision() {
        // ready
        for word in ["healthy", "running", ""] {
            let readiness = Readiness::parse(word);
            assert!(readiness.ready(), "{word:?} must be ready");
            assert!(!readiness.stopped(), "{word:?} must not read as stopped");
        }
        // stopped before ready: fail, do not wait
        for word in ["exited", "dead"] {
            let readiness = Readiness::parse(word);
            assert!(!readiness.ready(), "{word:?} must not be ready");
            assert!(readiness.stopped(), "{word:?} must read as stopped");
        }
        // keep waiting: transitional, unhealthy, or unknown
        for word in [
            "starting",
            "unhealthy",
            "paused",
            "restarting",
            "confused-state",
        ] {
            let readiness = Readiness::parse(word);
            assert!(!readiness.ready(), "{word:?} must not be ready");
            assert!(!readiness.stopped(), "{word:?} must not read as stopped");
        }
        assert_eq!(Readiness::parse("  running\n"), Readiness::Running);
    }

    #[test]
    fn container_state_rejects_unknown_words_and_grades_reclaim() {
        for (word, reclaimable) in [
            ("created", true),
            ("running", false),
            ("restarting", false),
            ("removing", true),
            ("paused", false),
            ("exited", true),
            ("dead", true),
        ] {
            let state = ContainerState::parse(word).unwrap_or_else(|| panic!("{word} parses"));
            assert_eq!(state.safe_to_reclaim(), reclaimable, "{word}");
        }
        assert_eq!(ContainerState::parse("vaporized"), None);
        assert_eq!(ContainerState::parse(""), None);
    }

    #[test]
    fn port_mappings_keep_both_families_and_skip_garbage() {
        // `docker port velnor-shape-probe`, live: one mapping, two lines.
        let mappings = parse_port_mappings("8080/tcp -> 0.0.0.0:41062\n8080/tcp -> [::]:41062\n");
        assert_eq!(
            mappings,
            vec![
                PortMapping {
                    container_port: "8080/tcp".to_string(),
                    host_address: "0.0.0.0:41062".to_string(),
                },
                PortMapping {
                    container_port: "8080/tcp".to_string(),
                    host_address: "[::]:41062".to_string(),
                },
            ]
        );
        assert!(parse_port_mappings("not a mapping\nno-colon -> \n").is_empty());
    }

    #[test]
    fn cgroup_projection_accepts_exactly_two_words() {
        let probed = parse_cgroup_projection("systemd 2\n").expect("two words parse");
        assert_eq!(
            probed,
            CgroupDriver {
                driver: "systemd".to_string(),
                version: "2".to_string(),
            }
        );
        assert!(parse_cgroup_projection("systemd\n").is_err());
        assert!(parse_cgroup_projection("").is_err());
        assert!(parse_cgroup_projection("a b c\n").is_err());
    }

    #[test]
    fn missing_vocabulary_covers_both_generations_and_stays_narrow() {
        // Modern Engine: `Error: No such object: <name>` on stdout `[]`.
        assert!(daemon_reports_missing("Error: No such object: velnor-x"));
        assert!(daemon_reports_missing("Error: No such container: velnor-x"));
        // Older generation.
        assert!(daemon_reports_missing("error: no such object: velnor-x"));
        assert!(daemon_reports_missing("no such container"));
        // Narrow on purpose: the cancel ladder reads anything else as alive.
        assert!(!daemon_reports_missing(""));
        assert!(!daemon_reports_missing(
            "Error response from daemon: network velnor-net-1 not found"
        ));
        assert!(!daemon_reports_missing(
            "Cannot connect to the Docker daemon"
        ));
    }

    #[test]
    fn every_facade_query_is_a_bounded_control_plane_call() {
        const SIX_HOURS: Duration = Duration::from_secs(6 * 3600);
        let mut queries: Vec<Vec<String>> = vec![
            readiness_args("velnor-service-postgres"),
            running_args("velnor-job-1"),
            container_id_args("velnor-service-postgres"),
            daemon_cgroup_args(),
            mapped_ports_args("velnor-service-postgres"),
            image_id_args("velnor/job-ubuntu:26.04"),
            vec!["buildx".to_string(), "ls".to_string()],
        ];
        // The listing builders the reclaim decisions consume through this
        // module's parsers must hold the same guarantee.
        queries.push(crate::docker_lease::list_owned_containers_args("job"));
        queries.push(crate::docker_lease::list_owned_containers_state_args("job"));
        queries.push(crate::docker_lease::list_owned_networks_args("job"));
        queries.push(crate::docker_lease::list_owned_volumes_args("job"));
        queries.push(crate::docker_lease::list_owned_job_format_args());
        queries.push(crate::docker_lease::list_daemon_owned_job_format_args());
        queries.push(crate::docker_lease::list_testcontainers_format_args());
        queries.push(crate::docker_lease::list_job_image_format_args());
        queries.push(crate::docker_lease::list_job_buildkit_format_args());
        queries.push(crate::docker_lease::list_job_buildkit_volume_args());
        queries.push(crate::docker_lease::list_daemon_owned_job_buildkit_volume_format_args());
        queries.push(crate::docker_lease::list_preflight_format_args());
        for args in &queries {
            let (op, deadline) = crate::docker::deadline_for(args, SIX_HOURS);
            assert!(
                op.is_control_plane(),
                "{args:?} must classify as control plane, got {op}"
            );
            assert!(
                deadline < SIX_HOURS,
                "{args:?} ({op}) inherited the step deadline"
            );
        }
    }

    /// Scripted [`CommandRunner`] that records every invocation.
    struct ScriptRunner {
        results: std::collections::VecDeque<CommandResult>,
        calls: AtomicUsize,
        seen_args: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<CommandResult>) -> Self {
            Self {
                results: results.into(),
                calls: AtomicUsize::new(0),
                seen_args: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for ScriptRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            assert_eq!(program, "docker");
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_args
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(args.to_vec());
            self.results.pop_front().context("script exhausted")
        }
    }

    fn ok(stdout: &str) -> CommandResult {
        CommandResult {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn failed(code: i32, stderr: &str) -> CommandResult {
        CommandResult {
            code,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn each_query_costs_exactly_one_process() {
        let mut runner = ScriptRunner::scripted(vec![
            ok("healthy\n"),
            ok("a530e70d9e1e35941b6fc12db9b51a7b19c6d02\n"),
            ok("8080/tcp -> 0.0.0.0:41062\n"),
            ok("systemd 2\n"),
        ]);
        {
            let mut docker = Docker::job(&mut runner);
            assert_eq!(
                docker.container_readiness("svc").expect("readiness"),
                Readiness::Healthy
            );
            assert!(docker
                .container_id("svc")
                .expect("id")
                .starts_with("a530e70d9e1e"));
            assert_eq!(docker.mapped_ports("svc").expect("ports").len(), 1);
            assert_eq!(docker.daemon_cgroup().expect("cgroup").driver, "systemd");
        }
        assert_eq!(runner.calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            *runner
                .seen_args
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec![
                readiness_args("svc"),
                container_id_args("svc"),
                mapped_ports_args("svc"),
                daemon_cgroup_args(),
            ]
        );
    }

    #[test]
    fn job_transport_maps_missing_timeout_and_failure() {
        // Positive-missing reads as not running (the cancel direction).
        let mut runner =
            ScriptRunner::scripted(vec![failed(1, "Error: No such object: velnor-job-9\n")]);
        assert!(!Docker::job(&mut runner)
            .container_running("velnor-job-9")
            .expect("missing reads as not running"));

        // Positive-missing on other queries is a typed error, never empty.
        let mut runner = ScriptRunner::scripted(vec![failed(1, "Error: No such object: svc\n")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("missing must error");
        assert!(is_not_found(&error), "expected NotFound, got {error:#}");
        assert!(!is_not_found(&anyhow::anyhow!("boom")));

        // Exit 124 normalizes to the query's class deadline, not the step's.
        let mut runner = ScriptRunner::scripted(vec![failed(124, "timed out")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("timeout must error");
        let timeout = error
            .downcast_ref::<crate::docker::DockerTimeout>()
            .expect("typed DockerTimeout");
        assert_eq!(timeout.op, crate::docker::DockerOp::Query);
        assert_eq!(timeout.deadline, Duration::from_secs(20));

        // Any other failure carries the stderr.
        let mut runner =
            ScriptRunner::scripted(vec![failed(1, "Cannot connect to the Docker daemon\n")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("daemon failure must error");
        assert!(!is_not_found(&error));
        assert!(
            format!("{error:#}").contains("Cannot connect"),
            "stderr must survive: {error:#}"
        );
    }

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
}

//! Persistent BuildKit builders: stable names, claims, and reclamation.
//!
//! Every containerized build in every job used to start cold: job teardown
//! force-removed the buildkitd container *and* its `_state` volume on every
//! terminal path, so a workflow's `keep-state: true` could not survive it.
//! Builders are expensive persistent infrastructure being treated as per-job
//! scratch. This module reverses that:
//!
//! * **Stable names.** A builder is keyed by (trust scope, repository,
//!   requested name) instead of the runner slot, so the next job reuses the
//!   warm daemon and its cache. Proven live: a fresh buildx client state
//!   plus `buildx create` over a kept daemon rebuilds `CACHED`.
//! * **Claims.** Concurrent jobs on one repository share one builder, so the
//!   post step cannot blindly stop it: stopping a shared daemon mid-build
//!   fails the other job. Each setup claims the builder for its job
//!   container; each post and teardown releases; the daemon stops only when
//!   the last holder releases. Claims live in the daemon's run root next to
//!   the scope leases, guarded by the same entry lock.
//! * **Reclamation.** What claims cannot cover, maintenance converges:
//!   disk-pressure reclaim stops and prunes builders with no holders
//!   (largest first, du-measured), and the horizon path deletes builders
//!   idle past [`IDLE_DELETE_AFTER`]. Builders are a cache: every destructive
//!   action degrades the next build to cold, never to wrong.
//!
//! Trust partitioning: the scope segment namespaces builders exactly like the
//! persistent stores, so a fork-PR job never shares a builder with a trusted
//! job. The repository comes from the runner-derived container spec, never
//! from step environment a workflow could override into a neighbor's cache.
//!
//! Legacy slot-scoped builders (`velnor-builder-<requested>-<slot>`, from
//! before persistence) keep their destroy-at-teardown path: teardown matches
//! them by the current job's slot suffix, and anything with the
//! [`PERSISTENT_BUILDER_PREFIX`] prefix is excluded from every
//! destroy/orphan match.
//! The exclusion direction is fail-safe: a legacy builder whose requested
//! name starts with the marker is skipped (orphaned until the horizon path),
//! while a persistent builder can never match a slot suffix unless an
//! operator names a temp directory `shared-*`, in which case the failure is
//! a loud cold-cache rebuild, not a wrong build.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Prefix that marks the persistent builder namespace:
/// `velnor-builder-shared-<scope>-<repo>[-<requested>]`. Prefix-anchored
/// matching separates persistent builders from legacy slot-scoped ones at
/// every destroy/orphan decision.
pub(crate) const PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-";

/// The setup-buildx default builder name. A job that requests exactly this
/// gets the short form without a requested-name segment.
const DEFAULT_REQUESTED_NAME: &str = "velnor-builder";

/// Builders stopped longer than this, with no holders, are deleted with
/// their state volume by the horizon path. Seven days bounds the
/// unique-name-per-job accumulation class while never touching a builder
/// any live job could restart in seconds.
pub(crate) const IDLE_DELETE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

/// Subdirectory of the daemon run root holding one claim file per builder.
const CLAIMS_DIR: &str = "buildkit-claims";

/// Job-local record of the builders this job claimed, so teardown releases
/// exactly what setup claimed even when the post step never ran (cancel).
const JOB_BUILDERS_FILE: &str = "_velnor/buildkit-builders.json";

/// Repository slug when the job carries no repository (no checkout): parseable
/// and impossible to collide with a real `owner_repo` slug.
const NO_REPO_SLUG: &str = "no_repo";

/// Stable builder name for (requested name, effective trust scope,
/// repository). Trust first, like the store namespaces; the repository slug
/// keeps one repo's cache out of another's.
pub(crate) fn persistent_builder_name(
    requested: &str,
    scope: &str,
    repository: Option<&str>,
) -> String {
    let scope = sanitize_builder_segment(scope);
    let repo = repo_slug(repository);
    let requested = sanitize_builder_segment(requested);
    if requested == DEFAULT_REQUESTED_NAME || requested.is_empty() {
        format!("{PERSISTENT_BUILDER_PREFIX}{scope}-{repo}")
    } else {
        format!("{PERSISTENT_BUILDER_PREFIX}{scope}-{repo}-{requested}")
    }
}

/// True when `builder` names a persistent builder. Prefix-anchored: a legacy
/// `velnor-builder-<requested>-<slot>` only matches when its requested name
/// starts with `shared-`, in which case teardown skips it (fail-safe) and the
/// horizon path deletes it once idle.
pub(crate) fn is_persistent_builder_name(builder: &str) -> bool {
    builder.starts_with(PERSISTENT_BUILDER_PREFIX)
}

/// True when a buildkitd container or state volume belongs to a persistent
/// builder. The object name embeds the builder name
/// (`buildx_buildkit_<builder>0[_state]`), so the marker survives the
/// embedding; legacy objects match only with a `shared-*` requested name,
/// which fails safe toward skipping.
pub(crate) fn is_persistent_builder_object(name: &str) -> bool {
    name.contains(PERSISTENT_BUILDER_PREFIX)
}

fn sanitize_builder_segment(value: &str) -> String {
    crate::container::sanitize_store_key(value.trim())
}

/// `owner/repo` becomes `owner_repo`, sanitized for builder names, temp
/// paths, and claim files alike.
fn repo_slug(repository: Option<&str>) -> String {
    let slug = repository
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(|repo| repo.replace('/', "_"))
        .unwrap_or_else(|| NO_REPO_SLUG.to_string());
    let slug = crate::container::sanitize_store_key(&slug);
    if slug.is_empty() {
        NO_REPO_SLUG.to_string()
    } else {
        slug
    }
}

/// Prefix buildx derives docker-container daemon names from:
/// `buildx_buildkit_<builder>0`. Persistent builder names start with
/// `velnor-builder-`, so the existing
/// [`crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX`] listings keep
/// matching persistent daemons with no changes.
const DAEMON_CONTAINER_PREFIX: &str = "buildx_buildkit_";

/// The docker-container daemon's container name for a single-node builder.
/// Velnor never appends nodes, so the `0` node is the whole fleet. A workflow
/// that appends its own nodes leaves the extra daemons to the reclaim paths,
/// which enumerate by prefix instead of deriving.
pub(crate) fn daemon_container_name(builder: &str) -> String {
    format!("{DAEMON_CONTAINER_PREFIX}{builder}0")
}

/// The daemon's state volume: buildx names it `<container>_state`.
pub(crate) fn daemon_state_volume(builder: &str) -> String {
    format!("{}_state", daemon_container_name(builder))
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

/// One job's hold on a shared builder.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BuilderHolder {
    /// The holder's job container name (`velnor-job-...`).
    pub container: String,
    /// The holder's slot scope, for crash repair by slot exclusivity.
    pub slot: String,
    /// When the hold was taken, unix seconds.
    pub claimed_unix: u64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct BuilderClaims {
    #[serde(default)]
    holders: BTreeMap<String, BuilderHolder>,
}

fn claims_file(run_root: &Path, builder: &str) -> PathBuf {
    run_root.join(CLAIMS_DIR).join(format!(
        "{}.json",
        crate::container::sanitize_store_key(builder)
    ))
}

fn read_claims(path: &Path) -> Result<BuilderClaims> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BuilderClaims::default())
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    // A torn write must never pin a builder claimed forever: an unreadable
    // claim file reads as empty, and the next claim rewrites it whole.
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn write_claims(path: &Path, claims: &BuilderClaims) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(claims).context("encode builder claims")?;
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Drop holds from `slot` that predate `container`. A slot runs one job at a
/// time, so any older hold from this slot is a crashed job's. Cross-slot
/// holds are live jobs sharing the builder and are never touched here.
fn repair_slot_unlocked(claims: &mut BuilderClaims, slot: &str, container: &str) {
    claims
        .holders
        .retain(|held, holder| held == container || holder.slot != slot);
}

/// Drop holds whose job container is not in `present`.
fn repair_absent_unlocked(claims: &mut BuilderClaims, present: &BTreeSet<String>) {
    claims.holders.retain(|held, _| present.contains(held));
}

/// Claim `builder` for the calling job. Idempotent: claiming twice (two
/// setup-buildx steps, one name) holds once.
pub(crate) fn claim_builder(
    run_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    let path = claims_file(run_root, builder);
    let _lock = crate::cache::CacheEntryLock::exclusive(&path)?;
    let mut claims = read_claims(&path)?;
    repair_slot_unlocked(&mut claims, slot, container);
    claims.holders.insert(
        container.to_string(),
        BuilderHolder {
            container: container.to_string(),
            slot: slot.to_string(),
            claimed_unix: unix_now(),
        },
    );
    write_claims(&path, &claims)
}

/// What a release did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReleaseOutcome {
    /// This release removed the final holder.
    pub removed_last: bool,
    /// The stop closure acted (only ever true with `removed_last`).
    pub stopped: bool,
}

/// Release the calling job's hold, stopping the daemon when this release
/// removed the final holder. The claim lock is held across the stop: a setup
/// racing the release blocks, then claims the stopped daemon and restarts it
/// on first build, so the stop can never land mid-build. Releasing a hold
/// this job does not have (post after post, teardown after post) succeeds
/// without stopping.
pub(crate) fn release_and_stop_if_last(
    run_root: &Path,
    builder: &str,
    container: &str,
    stop: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    let path = claims_file(run_root, builder);
    let _lock = crate::cache::CacheEntryLock::exclusive(&path)?;
    let mut claims = read_claims(&path)?;
    let removed = claims.holders.remove(container).is_some();
    write_claims(&path, &claims)?;
    let removed_last = removed && claims.holders.is_empty();
    let stopped = if removed_last { stop()? } else { false };
    Ok(ReleaseOutcome {
        removed_last,
        stopped,
    })
}

/// Current holders of `builder`, after dropping crashed-slot holds the
/// caller can prove dead. Maintenance callers pass no slot and get the raw
/// set; job callers pass their own slot for the exclusivity repair.
#[cfg(test)]
pub(crate) fn builder_holders(
    run_root: &Path,
    builder: &str,
    my_slot: Option<(&str, &str)>,
) -> Result<Vec<BuilderHolder>> {
    let path = claims_file(run_root, builder);
    let _lock = crate::cache::CacheEntryLock::exclusive(&path)?;
    let mut claims = read_claims(&path)?;
    if let Some((slot, container)) = my_slot {
        repair_slot_unlocked(&mut claims, slot, container);
        write_claims(&path, &claims)?;
    }
    let mut holders: Vec<BuilderHolder> = claims.holders.into_values().collect();
    holders.sort_by(|left, right| left.container.cmp(&right.container));
    Ok(holders)
}

/// Drop holds whose job container no longer exists. The container set comes
/// from one `ps` listing the caller already holds; a container that lingers
/// (including a crashed job's still-running ghost) keeps its hold, which is
/// exactly the pre-existing container-recovery boundary.
#[cfg(test)]
pub(crate) fn repair_absent_holders(
    run_root: &Path,
    builder: &str,
    present: &BTreeSet<String>,
) -> Result<Vec<BuilderHolder>> {
    let path = claims_file(run_root, builder);
    let _lock = crate::cache::CacheEntryLock::exclusive(&path)?;
    let mut claims = read_claims(&path)?;
    repair_absent_unlocked(&mut claims, present);
    write_claims(&path, &claims)?;
    let mut holders: Vec<BuilderHolder> = claims.holders.into_values().collect();
    holders.sort_by(|left, right| left.container.cmp(&right.container));
    Ok(holders)
}

/// Record that this job claimed `builder`, so teardown releases exactly what
/// setup claimed. Job-local file, no lock: steps run sequentially.
pub(crate) fn record_job_builder(temp_host: &Path, builder: &str) -> Result<()> {
    let path = temp_host.join(JOB_BUILDERS_FILE);
    let mut builders = read_job_builders(temp_host)?;
    if !builders.iter().any(|known| known == builder) {
        builders.push(builder.to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&builders)?)
        .with_context(|| format!("write {}", path.display()))
}

/// Builders this job claimed. Missing or torn file reads as none: teardown
/// then releases nothing, and the slot repair at the next setup plus the
/// horizon path converge the leak.
pub(crate) fn read_job_builders(temp_host: &Path) -> Result<Vec<String>> {
    let path = temp_host.join(JOB_BUILDERS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

/// The daemon run root holding claim files, if this process has storage
/// configured. Maintenance and job paths degrade to no-claim operation
/// without one (builders still persist by name; nothing stops them).
pub(crate) fn claims_run_root() -> Option<PathBuf> {
    crate::storage::StorageLayout::resolve().map(|layout| layout.run_root)
}

// ---------------------------------------------------------------------------
// Daemon operations (host engine, every call deadline-bounded)
// ---------------------------------------------------------------------------

/// Stop one builder's daemon. Returns true when a stop acted; a missing
/// daemon reads as already stopped. Only ever called with zero holders.
pub(crate) fn stop_builder_daemon(builder: &str) -> Result<bool> {
    let daemon = daemon_container_name(builder);
    let args = vec!["stop".to_string(), daemon.clone()];
    match crate::docker::client::host_call(&args) {
        Ok(_) => Ok(true),
        Err(error) => {
            let detail = format!("{error:#}");
            if crate::docker::client::daemon_reports_missing(&detail) {
                Ok(false)
            } else {
                Err(error).with_context(|| format!("stop BuildKit daemon {daemon}"))
            }
        }
    }
}

/// Start one builder's daemon. Missing reads as already gone.
fn start_builder_daemon(builder: &str) -> Result<bool> {
    let daemon = daemon_container_name(builder);
    let args = vec!["start".to_string(), daemon.clone()];
    match crate::docker::client::host_call(&args) {
        Ok(_) => Ok(true),
        Err(error) => {
            let detail = format!("{error:#}");
            if crate::docker::client::daemon_reports_missing(&detail) {
                Ok(false)
            } else {
                Err(error).with_context(|| format!("start BuildKit daemon {daemon}"))
            }
        }
    }
}

/// Prune one builder's cache completely, returning the du-measured bytes
/// freed. A missing builder (or one that vanishes mid-prune) frees nothing.
/// A stopped daemon is started first: `buildx du` and `buildx prune` both
/// refuse a stopped daemon (proven live). Only ever called with zero
/// holders under the claim lock: no live job can observe the cold, and a
/// racing setup blocks on the lock, then claims the pruned builder.
fn prune_builder(builder: &str) -> Result<u64> {
    let mut docker = crate::docker::Docker::host();
    let before = match docker.buildx_disk_usage(builder) {
        Ok(usage) => usage,
        Err(error) if format!("{error:#}").contains("is not running") => {
            if !start_builder_daemon(builder)? {
                return Ok(0);
            }
            docker.buildx_disk_usage(builder).unwrap_or(0)
        }
        Err(_) => return Ok(0),
    };
    let args = vec![
        "buildx".to_string(),
        "prune".to_string(),
        "--builder".to_string(),
        builder.to_string(),
        "--force".to_string(),
    ];
    if let Err(error) = crate::docker::client::host_call(&args) {
        let detail = format!("{error:#}");
        // Missing daemon (narrow engine vocabulary) or missing buildx
        // registration (`no builder "x" found`, a buildx-side answer).
        if crate::docker::client::daemon_reports_missing(&detail) || detail.contains("no builder") {
            return Ok(0);
        }
        return Err(error).with_context(|| format!("prune BuildKit builder {builder}"));
    }
    let after = docker.buildx_disk_usage(builder).unwrap_or(0);
    Ok(before.saturating_sub(after))
}

/// Delete one builder's daemon container and state volume. Only ever called
/// with zero holders past the idle horizon: the next build recreates a cold
/// daemon from the same stable name.
pub(crate) fn remove_builder(builder: &str) -> Result<()> {
    let daemon = daemon_container_name(builder);
    let rm = vec!["rm".to_string(), "--force".to_string(), daemon.clone()];
    match crate::docker::client::host_call(&rm) {
        Ok(_) => {}
        Err(error) => {
            let detail = format!("{error:#}");
            if !(crate::docker::client::daemon_reports_missing(&detail)) {
                return Err(error).with_context(|| format!("remove BuildKit daemon {daemon}"));
            }
        }
    }
    let volume = daemon_state_volume(builder);
    let rm_volume = vec![
        "volume".to_string(),
        "rm".to_string(),
        "--force".to_string(),
        volume.clone(),
    ];
    match crate::docker::client::host_call(&rm_volume) {
        Ok(_) => Ok(()),
        Err(error) => {
            let detail = format!("{error:#}");
            if crate::docker::client::daemon_reports_missing(&detail) {
                Ok(())
            } else {
                Err(error).with_context(|| format!("remove BuildKit state volume {volume}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pressure prune and horizon reaping
// ---------------------------------------------------------------------------

/// What one disk-pressure BuildKit pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PressurePruneReport {
    pub pruned: Vec<String>,
    pub freed_bytes: u64,
    pub failures: Vec<String>,
}

/// Stop and fully prune unclaimed persistent builders, largest first, until
/// `target_bytes` are freed or no unclaimed builder remains. The claim
/// boundary is what the old dead reclaim lacked: a builder with any holder
/// is skipped no matter how large, and holds from vanished job containers
/// are repaired first so a crash cannot pin disk forever.
pub(crate) fn pressure_prune_builders(run_root: &Path, target_bytes: u64) -> PressurePruneReport {
    let mut report = PressurePruneReport::default();
    if target_bytes == 0 {
        return report;
    }
    let mut docker = crate::docker::Docker::host();
    let builders = match docker.buildx_builders() {
        Ok(builders) => builders,
        Err(error) => {
            report
                .failures
                .push(format!("list builders for pressure prune: {error:#}"));
            return report;
        }
    };
    let persistent: Vec<String> = builders
        .into_iter()
        .filter(|builder| is_persistent_builder_name(builder))
        .collect();
    if persistent.is_empty() {
        return report;
    }
    let present = match running_container_names() {
        Ok(present) => present,
        Err(error) => {
            report
                .failures
                .push(format!("list containers for claim repair: {error:#}"));
            return report;
        }
    };
    // Largest first: one unlocked du per builder as an ordering hint, then
    // prune in that order. Unmeasurable builders (stopped daemons refuse du)
    // sort last; the prune still measures them after starting. The hint is
    // never a decision: the holder check and the prune below are atomic
    // under each builder's claim lock.
    let mut sized: Vec<(String, Option<u64>)> = Vec::new();
    for builder in &persistent {
        match docker.buildx_disk_usage(builder) {
            Ok(usage) => sized.push((builder.clone(), Some(usage))),
            Err(_) => sized.push((builder.clone(), None)),
        }
    }
    sized.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (builder, _hint) in sized {
        if report.freed_bytes >= target_bytes {
            break;
        }
        let path = claims_file(run_root, &builder);
        let _lock = match crate::cache::CacheEntryLock::exclusive(&path) {
            Ok(lock) => lock,
            Err(error) => {
                report
                    .failures
                    .push(format!("lock claims for {builder}: {error:#}"));
                continue;
            }
        };
        let mut claims = match read_claims(&path) {
            Ok(claims) => claims,
            Err(error) => {
                report
                    .failures
                    .push(format!("read claims for {builder}: {error:#}"));
                continue;
            }
        };
        repair_absent_unlocked(&mut claims, &present);
        if write_claims(&path, &claims).is_err() {
            report
                .failures
                .push(format!("repair claims for {builder}: write failed"));
            continue;
        }
        if !claims.holders.is_empty() {
            continue;
        }
        // Prune while running, then stop: buildx refuses both du and prune
        // on a stopped daemon, so stop-first would prune nothing.
        match prune_builder(&builder) {
            Ok(freed) => {
                report.freed_bytes = report.freed_bytes.saturating_add(freed);
                report.pruned.push(builder.clone());
            }
            Err(error) => {
                report
                    .failures
                    .push(format!("prune builder {builder}: {error:#}"));
                continue;
            }
        }
        if let Err(error) = stop_builder_daemon(&builder) {
            report
                .failures
                .push(format!("stop builder {builder}: {error:#}"));
        }
    }
    report
}

/// What one horizon pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HorizonReport {
    pub stopped: Vec<String>,
    pub deleted: Vec<String>,
    pub failures: Vec<String>,
}

/// Converge every builder leak the claims cannot see: unclaimed running
/// daemons are stopped (a next build restarts its daemon in seconds, proven
/// live), and unclaimed daemons stopped longer than [`IDLE_DELETE_AFTER`]
/// are deleted with their state volume. Called from daemon startup and
/// doctor — never from the per-job path, and never deleting a builder any
/// holder, however stale-looking, still references without the absent repair.
pub(crate) fn reap_idle_builders(run_root: &Path, now: SystemTime) -> HorizonReport {
    let mut report = HorizonReport::default();
    let mut docker = crate::docker::Docker::host();
    let builders = match docker.buildx_builders() {
        Ok(builders) => builders,
        Err(error) => {
            report
                .failures
                .push(format!("list builders for horizon reap: {error:#}"));
            return report;
        }
    };
    let present = match running_container_names() {
        Ok(present) => present,
        Err(error) => {
            report
                .failures
                .push(format!("list containers for claim repair: {error:#}"));
            return report;
        }
    };
    for builder in builders
        .into_iter()
        .filter(|builder| is_persistent_builder_name(builder))
    {
        // The claim lock spans the repair, the holder check, and the stop
        // or delete below: a setup racing the horizon blocks, then claims
        // the stopped or deleted builder and rebuilds cold, so the act can
        // never land mid-build.
        let path = claims_file(run_root, &builder);
        let _lock = match crate::cache::CacheEntryLock::exclusive(&path) {
            Ok(lock) => lock,
            Err(error) => {
                report
                    .failures
                    .push(format!("lock claims for {builder}: {error:#}"));
                continue;
            }
        };
        let mut claims = match read_claims(&path) {
            Ok(claims) => claims,
            Err(error) => {
                report
                    .failures
                    .push(format!("read claims for {builder}: {error:#}"));
                continue;
            }
        };
        repair_absent_unlocked(&mut claims, &present);
        if write_claims(&path, &claims).is_err() {
            report
                .failures
                .push(format!("repair claims for {builder}: write failed"));
            continue;
        }
        if !claims.holders.is_empty() {
            continue;
        }
        let daemon = daemon_container_name(&builder);
        let exit = match docker.inspect_exit(&daemon) {
            Ok(exit) => exit,
            Err(error) if crate::docker::client::is_not_found(&error) => {
                // Daemon gone but the builder registration lingers (a
                // `buildx rm --keep-state` past, or a crashed delete):
                // remove the registration and any orphaned volume.
                match remove_builder(&builder) {
                    Ok(()) => report.deleted.push(builder.clone()),
                    Err(error) => report
                        .failures
                        .push(format!("delete builder {builder}: {error:#}")),
                }
                continue;
            }
            Err(error) => {
                report
                    .failures
                    .push(format!("inspect daemon for {builder}: {error:#}"));
                continue;
            }
        };
        if exit.status.is_some_and(|state| !state.safe_to_reclaim()) {
            // Running with no holders: a release-time stop that failed, or a
            // daemon started outside any claim. Stopping is always safe —
            // the next build restarts it — but deleting is not considered.
            match stop_builder_daemon(&builder) {
                Ok(true) => report.stopped.push(builder.clone()),
                Ok(false) => {}
                Err(error) => report
                    .failures
                    .push(format!("stop builder {builder}: {error:#}")),
            }
            continue;
        }
        let idle = exit
            .finished
            .and_then(|finished| now.duration_since(finished).ok());
        match idle {
            // No stop time (running zero-time, unparseable): cannot prove
            // idleness, so never delete.
            None => {}
            Some(idle) if idle < IDLE_DELETE_AFTER => {}
            Some(_) => match remove_builder(&builder) {
                Ok(()) => report.deleted.push(builder.clone()),
                Err(error) => report
                    .failures
                    .push(format!("delete builder {builder}: {error:#}")),
            },
        }
    }
    report
}

/// Names of every container on the host Engine, for absent-holder repair.
/// One `ps`, parsed leniently. A failed listing propagates as an error —
/// never an empty set that would drop every hold — while a successful empty
/// listing legitimately repairs everything (no containers, no live jobs).
fn running_container_names() -> Result<BTreeSet<String>> {
    let args = vec![
        "ps".to_string(),
        "--all".to_string(),
        "--format".to_string(),
        "{{.Names}}".to_string(),
    ];
    let listed = crate::docker::client::host_call(&args)?;
    Ok(listed
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-buildkit-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn storage_env_stays_unset_so_claims_never_touch_real_state() {
        // Every claim/release/horizon call in the unit suite must take the
        // no-run-root path. A polluted environment would write claim files
        // into real daemon state and spawn docker from hermetic tests.
        assert!(
            std::env::var_os("VELNOR_STORAGE_ROOT").is_none(),
            "unset VELNOR_STORAGE_ROOT to run the suite hermetically"
        );
        assert!(claims_run_root().is_none());
    }

    #[test]
    fn persistent_names_partition_by_trust_repo_and_request() {
        // Default requested name: short form.
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", Some("octocat/hello-world")),
            "velnor-builder-shared-trusted-octocat_hello-world"
        );
        // Custom requested name rides along.
        assert_eq!(
            persistent_builder_name("mybuilder", "trusted", Some("octocat/hello-world")),
            "velnor-builder-shared-trusted-octocat_hello-world-mybuilder"
        );
        // Fork-PR jobs never share with trusted jobs.
        assert_eq!(
            persistent_builder_name("velnor-builder", "untrusted", Some("octocat/hello-world")),
            "velnor-builder-shared-untrusted-octocat_hello-world"
        );
        // No repository: parseable, never collides with owner_repo.
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", None),
            "velnor-builder-shared-trusted-no_repo"
        );
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", Some("  ")),
            "velnor-builder-shared-trusted-no_repo"
        );
        // Hostile segments sanitize identically everywhere the name travels.
        assert_eq!(
            persistent_builder_name("../../x", "trusted", Some("o/r")),
            "velnor-builder-shared-trusted-o_r-.._.._x"
        );
    }

    #[test]
    fn persistent_matching_is_prefix_anchored_and_fail_safe() {
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-trusted-o_r"
        ));
        assert!(!is_persistent_builder_name("velnor-builder-slot-3"));
        assert!(!is_persistent_builder_name(
            "velnor-builder-mybuilder-slot-3"
        ));
        // The legacy adversarial case: an old-daemon builder whose requested
        // name starts with the marker matches persistent and is therefore
        // SKIPPED by teardown (orphaned until the horizon path) rather than
        // destroyed while potentially shared.
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-foo-slot-3"
        ));
        // Object embedding preserves the marker.
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-o_r0"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-o_r0_state"
        ));
        assert!(!is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-slot-30"
        ));
        // A realistic slot scope can never suffix-match a persistent name.
        for slot in ["slot-3", "job-scope0", "job"] {
            let needle = format!("buildx_buildkit_velnor-builder-{slot}");
            assert!(
                !format!(
                    "buildx_buildkit_{}",
                    persistent_builder_name("velnor-builder", "trusted", Some("o/r"))
                )
                .contains(&needle),
                "slot {slot} must not match a persistent daemon"
            );
        }
    }

    #[test]
    fn daemon_names_derive_the_documented_container_and_volume() {
        // Shapes proven live against buildx 0.33: container
        // `buildx_buildkit_<builder>0`, volume `<container>_state`.
        assert_eq!(
            daemon_container_name("velnor-builder-shared-trusted-o_r"),
            "buildx_buildkit_velnor-builder-shared-trusted-o_r0"
        );
        assert_eq!(
            daemon_state_volume("velnor-builder-shared-trusted-o_r"),
            "buildx_buildkit_velnor-builder-shared-trusted-o_r0_state"
        );
    }

    #[test]
    fn claims_count_holders_and_report_the_final_release() {
        let root = temp_root("claims");
        let run_root = root.join("run");
        let builder = "velnor-builder-shared-trusted-o_r";

        claim_builder(&run_root, builder, "slot-1", "velnor-job-a").unwrap();
        // Idempotent: a second setup step in the same job holds once.
        claim_builder(&run_root, builder, "slot-1", "velnor-job-a").unwrap();
        claim_builder(&run_root, builder, "slot-2", "velnor-job-b").unwrap();
        assert_eq!(builder_holders(&run_root, builder, None).unwrap().len(), 2);

        // First release: another holder remains, no stop.
        let outcome = release_and_stop_if_last(&run_root, builder, "velnor-job-a", || {
            panic!("must not stop with holders")
        })
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
            }
        );
        // Final release: the only state in which stopping is safe.
        let outcome =
            release_and_stop_if_last(&run_root, builder, "velnor-job-b", || Ok(true)).unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
            }
        );
        // Releasing again (post after post, teardown after post): succeeds,
        // reports nothing to stop.
        let outcome =
            release_and_stop_if_last(&run_root, builder, "velnor-job-b", || Ok(true)).unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
            }
        );
        let outcome = release_and_stop_if_last(&run_root, builder, "velnor-job-never-held", || {
            panic!("must not stop without a hold")
        })
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
            }
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slot_repair_drops_only_crashed_same_slot_holds() {
        let root = temp_root("slot-repair");
        let run_root = root.join("run");
        let builder = "velnor-builder-shared-trusted-o_r";

        // Crashed job on slot-1 never released; live job on slot-2 holds.
        claim_builder(&run_root, builder, "slot-1", "velnor-job-crashed").unwrap();
        claim_builder(&run_root, builder, "slot-2", "velnor-job-live").unwrap();

        // The next job on slot-1 claims: the crashed hold drops, the live
        // cross-slot hold survives.
        claim_builder(&run_root, builder, "slot-1", "velnor-job-next").unwrap();
        let holders = builder_holders(&run_root, builder, None).unwrap();
        let held: Vec<&str> = holders
            .iter()
            .map(|holder| holder.container.as_str())
            .collect();
        assert_eq!(held, vec!["velnor-job-live", "velnor-job-next"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn absent_repair_drops_only_vanished_containers() {
        let root = temp_root("absent-repair");
        let run_root = root.join("run");
        let builder = "velnor-builder-shared-trusted-o_r";

        claim_builder(&run_root, builder, "slot-1", "velnor-job-gone").unwrap();
        claim_builder(&run_root, builder, "slot-2", "velnor-job-here").unwrap();

        let present: BTreeSet<String> = ["velnor-job-here".to_string()].into_iter().collect();
        let holders = repair_absent_holders(&run_root, builder, &present).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, "velnor-job-here");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn torn_claim_files_read_empty_never_pinned() {
        let root = temp_root("torn");
        let run_root = root.join("run");
        let builder = "velnor-builder-shared-trusted-o_r";
        let path = claims_file(&run_root, builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();

        assert!(builder_holders(&run_root, builder, None)
            .unwrap()
            .is_empty());
        // The next claim rewrites the file whole.
        claim_builder(&run_root, builder, "slot-1", "velnor-job-a").unwrap();
        assert_eq!(builder_holders(&run_root, builder, None).unwrap().len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn job_builder_records_round_trip_and_dedupe() {
        let root = temp_root("record");
        let temp = root.join("slot-1").join("temp");
        assert!(read_job_builders(&temp).unwrap().is_empty());

        record_job_builder(&temp, "builder-a").unwrap();
        record_job_builder(&temp, "builder-a").unwrap();
        record_job_builder(&temp, "builder-b").unwrap();
        assert_eq!(
            read_job_builders(&temp).unwrap(),
            vec!["builder-a", "builder-b"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}

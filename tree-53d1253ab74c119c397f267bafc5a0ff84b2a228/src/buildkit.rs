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
//! Safety model (red-team P0): scope and repository are not enough. A feature
//! branch and a release tag on the same repository in the same pool would
//! share one builder, and BuildKit `RUN --mount=type=cache,id=...` mounts are
//! keyed by that id *within the daemon*: the branch job writes a cache id the
//! release build then reads, poisoning the release. So the key carries a
//! third trust dimension, the [`builder_trust_tier`]: `release` for
//! affirmatively release-grade jobs (release events, tags, protected refs),
//! `branch` for affirmatively branch-grade jobs, and `unknown` for anything
//! else. Release jobs share only with release jobs; a job whose signals are
//! missing lands in `unknown`, isolated from both, cold but never wrong.
//! Every tier input comes from the runner-authoritative immutable job
//! environment (`GITHUB_REF`, `GITHUB_REF_TYPE`, `GITHUB_EVENT_NAME`,
//! `GITHUB_REF_PROTECTED`), never from mutable step env a workflow can
//! rewrite. Pre-tier builder names (no tier segment) match no new key and
//! converge through the horizon path as idle orphans.
//!
//! Lock protocol: every claim-file mutation holds the entry lock, and *only*
//! the mutation does. Slow Docker work (stop, prune, remove, inspect) always
//! runs unlocked, then re-locks and rechecks for holders that arrived
//! mid-operation; a stop that raced a new claim is undone with a restart.
//! Lock waits are bounded ([`CLAIM_LOCK_TIMEOUT`]) and every wait, race, and
//! Docker act emits `velnor.buildkit` tracing telemetry. Docker calls carry
//! their own class deadlines through `host_call`.
//!
//! Capacity: requested names are workflow-controlled and unbounded, so each
//! (scope, repository) pair keeps at most [`MAX_BUILDERS_PER_SCOPE_REPO`]
//! builders; over-cap setup evicts the least-recently-used *unclaimed*
//! builder ([`enforce_builder_cap`]). Claim files die with their builder,
//! and [`maybe_reap_idle_builders`] runs the horizon pass periodically from
//! the setup path so corpse-pinned and idle builders converge without an
//! operator visit.
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
use std::time::{Duration, Instant, SystemTime};

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

/// Bound on every claim-lock wait. A wedged holder fails the waiter loud
/// instead of parking setup, post, teardown, and maintenance forever.
pub(crate) const CLAIM_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Builders kept per (trust scope, repository) pair. Requested names are
/// workflow-controlled, so without a cap one workflow mints unbounded
/// daemons, volumes, and claim files. Over-cap setup evicts the
/// least-recently-used unclaimed builder; claimed builders are never
/// evicted no matter how far over cap.
pub(crate) const MAX_BUILDERS_PER_SCOPE_REPO: usize = 8;

/// How often the setup path runs the horizon pass. Startup and doctor run
/// it too, but a daemon that serves jobs for weeks without either must
/// still converge corpse-pinned claims and idle builders.
pub(crate) const HORIZON_REAP_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Trust tiers in the builder key. `release` shares only with `release`;
/// `unknown` is the fail-closed bucket for missing signals.
pub(crate) const TRUST_TIER_RELEASE: &str = "release";
pub(crate) const TRUST_TIER_BRANCH: &str = "branch";
pub(crate) const TRUST_TIER_UNKNOWN: &str = "unknown";

/// Subdirectory of the daemon run root holding one claim file per builder.
const CLAIMS_DIR: &str = "buildkit-claims";

/// Marker file recording the last periodic horizon pass (unix seconds).
const HORIZON_REAP_MARKER: &str = ".last-horizon-reap";

/// Job-local record of the builders this job claimed, so teardown releases
/// exactly what setup claimed even when the post step never ran (cancel).
const JOB_BUILDERS_FILE: &str = "_velnor/buildkit-builders.json";

/// Repository slug when the job carries no repository (no checkout): parseable
/// and impossible to collide with a real `owner_repo` slug.
const NO_REPO_SLUG: &str = "no_repo";

/// The trust tier for a job's immutable ref signals. Release requires
/// affirmative release-grade evidence; branch requires affirmative
/// branch-grade evidence; anything else — including missing signals — is
/// `unknown`, isolated from both. Both fail directions are cold, never
/// wrong: an unknown job shares with no known job, so it can neither
/// poison a release cache nor read a poisoned one.
///
/// Inputs are the runner-authoritative immutable values (`GITHUB_REF`,
/// `GITHUB_REF_TYPE`, `GITHUB_EVENT_NAME`, `GITHUB_REF_PROTECTED`); callers
/// must never pass mutable step env a workflow can rewrite.
pub(crate) fn builder_trust_tier(
    git_ref: Option<&str>,
    ref_type: Option<&str>,
    event_name: Option<&str>,
    ref_protected: Option<&str>,
) -> &'static str {
    fn clean(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
    }
    let git_ref = clean(git_ref);
    let ref_type = clean(ref_type);
    let event_name = clean(event_name);
    let ref_protected = clean(ref_protected);
    let is_release_event = event_name.is_some_and(|event| event.eq_ignore_ascii_case("release"));
    let is_tag_type = ref_type.is_some_and(|kind| kind.eq_ignore_ascii_case("tag"));
    let is_tag_ref = git_ref.is_some_and(|git_ref| git_ref.starts_with("refs/tags/"));
    let is_protected = ref_protected.is_some_and(|flag| flag.eq_ignore_ascii_case("true"));
    if is_release_event || is_tag_type || is_tag_ref || is_protected {
        return TRUST_TIER_RELEASE;
    }
    let is_branch_ref = git_ref.is_some_and(|git_ref| {
        git_ref.starts_with("refs/heads/")
            || git_ref.starts_with("refs/pull/")
            || git_ref.starts_with("refs/remotes/")
    });
    let is_branch_type = ref_type.is_some_and(|kind| kind.eq_ignore_ascii_case("branch"));
    if is_branch_ref || is_branch_type {
        return TRUST_TIER_BRANCH;
    }
    TRUST_TIER_UNKNOWN
}

/// Stable builder name for (requested name, effective trust scope, trust
/// tier, repository). Trust first, like the store namespaces; the tier keeps
/// branch jobs out of the release daemon's ID-keyed cache mounts, and the
/// repository slug keeps one repo's cache out of another's.
pub(crate) fn persistent_builder_name(
    requested: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
) -> String {
    let scope = sanitize_builder_segment(scope);
    let tier = sanitize_builder_segment(tier);
    let repo = repo_slug(repository);
    let requested = sanitize_builder_segment(requested);
    if requested == DEFAULT_REQUESTED_NAME || requested.is_empty() {
        format!("{PERSISTENT_BUILDER_PREFIX}{scope}-{tier}-{repo}")
    } else {
        format!("{PERSISTENT_BUILDER_PREFIX}{scope}-{tier}-{repo}-{requested}")
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
    /// Full builder name this file guards. The file name is a sanitized
    /// (and truncated) derivation of it, so the reverse mapping lives here
    /// for cap enforcement and deletion.
    #[serde(default)]
    builder: String,
    /// Canonical grouping key for the per-scope-repository cap. Stored, not
    /// parsed out of the builder name: scope, tier, and repo segments may
    /// themselves contain dashes, so splitting the name is ambiguous.
    #[serde(default)]
    scope: String,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    repo: String,
    /// Last claim or release touching this file, unix seconds: the LRU clock
    /// for cap eviction.
    #[serde(default)]
    updated_unix: u64,
}

/// The canonical identity a claim records alongside its holders.
pub(crate) struct BuilderIdentity<'a> {
    pub scope: &'a str,
    pub tier: &'a str,
    pub repository: Option<&'a str>,
}

fn claims_file(run_root: &Path, builder: &str) -> PathBuf {
    run_root.join(CLAIMS_DIR).join(format!(
        "{}.json",
        crate::container::sanitize_store_key(builder)
    ))
}

/// Read one claim file. Missing reads as empty; torn JSON fails closed as
/// an error the caller must treat as *claimed*: with atomic rename writes a
/// torn file means disk corruption or a pre-atomic crash, and the safe
/// direction is to stop, prune, and delete nothing.
fn read_claims(path: &Path) -> Result<BuilderClaims> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BuilderClaims::default())
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Atomically replace one claim file: encode, write a sibling temp file,
/// rename over the target. Readers never observe a partial write — the
/// target is either the old or the new document — so torn files can only
/// come from outside this writer.
fn write_claims(path: &Path, claims: &BuilderClaims) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(claims).context("encode builder claims")?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let write_result = (|| -> Result<()> {
        std::fs::write(&temp, &bytes).with_context(|| format!("write {}", temp.display()))?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("rename {} to {}", temp.display(), path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result
}

/// Acquire one builder's claim lock with a bounded wait, emitting the wait
/// as `velnor.buildkit` telemetry. Every claim critical section enters
/// through here so no path can park on a wedged holder unobserved.
fn lock_claims(builder: &str, path: &Path) -> Result<crate::cache::CacheEntryLock> {
    let start = Instant::now();
    let lock = crate::cache::CacheEntryLock::exclusive_timeout(path, CLAIM_LOCK_TIMEOUT)?;
    let waited_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    tracing::debug!(
        target: "velnor.buildkit",
        builder,
        lock_wait_ms = waited_ms,
        "claim lock acquired"
    );
    if waited_ms > 5_000 {
        tracing::warn!(
            target: "velnor.buildkit",
            builder,
            lock_wait_ms = waited_ms,
            "slow claim lock acquisition"
        );
    }
    Ok(lock)
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
/// setup-buildx steps, one name) holds once. The claim records the
/// builder's canonical identity for cap enforcement; a torn claim file
/// fails the claim loud rather than dropping unknown holds.
pub(crate) fn claim_builder(
    run_root: &Path,
    builder: &str,
    identity: &BuilderIdentity<'_>,
    slot: &str,
    container: &str,
) -> Result<()> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let mut claims = read_claims(&path)?;
    repair_slot_unlocked(&mut claims, slot, container);
    let now = unix_now();
    claims.holders.insert(
        container.to_string(),
        BuilderHolder {
            container: container.to_string(),
            slot: slot.to_string(),
            claimed_unix: now,
        },
    );
    claims.builder = builder.to_string();
    claims.scope = sanitize_builder_segment(identity.scope);
    claims.tier = sanitize_builder_segment(identity.tier);
    claims.repo = repo_slug(identity.repository);
    claims.updated_unix = now;
    write_claims(&path, &claims)
}

/// What a release did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReleaseOutcome {
    /// This release removed the final holder.
    pub removed_last: bool,
    /// The stop closure acted (only ever true with `removed_last`).
    pub stopped: bool,
    /// New holders arrived while the stop ran unlocked, so the start
    /// closure ran to undo it. The daemon is left running.
    pub restarted: bool,
}

/// Release the calling job's hold, stopping the daemon when this release
/// removed the final holder. The lock covers only the holder mutation: the
/// stop runs unlocked so one slow daemon cannot park every setup, and a
/// recheck under a fresh lock restarts the daemon when a setup raced the
/// stop. A racing setup whose build starts after the stop restarts the
/// daemon on first build; a build caught mid-stop may fail once and is
/// visible in telemetry — the bounded, observable race this trades for
/// never holding the lock across Docker. Releasing a hold this job does not
/// have (post after post, teardown after post) succeeds without stopping.
/// A torn claim file reads as claimed: no removal, no stop, success.
pub(crate) fn release_and_stop_if_last(
    run_root: &Path,
    builder: &str,
    container: &str,
    stop: impl FnOnce() -> Result<bool>,
    start: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    let path = claims_file(run_root, builder);
    let removed_last = {
        let _lock = lock_claims(builder, &path)?;
        let mut claims = match read_claims(&path) {
            Ok(claims) => claims,
            Err(error) => {
                tracing::warn!(
                    target: "velnor.buildkit",
                    builder,
                    error = format!("{error:#}"),
                    "torn claim file treated as claimed; release stops nothing"
                );
                return Ok(ReleaseOutcome {
                    removed_last: false,
                    stopped: false,
                    restarted: false,
                });
            }
        };
        let removed = claims.holders.remove(container).is_some();
        claims.updated_unix = unix_now();
        if claims.builder.is_empty() {
            claims.builder = builder.to_string();
        }
        write_claims(&path, &claims)?;
        removed && claims.holders.is_empty()
    };
    if !removed_last {
        return Ok(ReleaseOutcome {
            removed_last: false,
            stopped: false,
            restarted: false,
        });
    }
    let stop_started = Instant::now();
    let stopped = stop()?;
    tracing::debug!(
        target: "velnor.buildkit",
        builder,
        stop_ms = stop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        stopped,
        "release stop ran outside the claim lock"
    );
    // Recheck: a setup that claimed while the stop ran needs the daemon
    // back. A torn recheck reads as claimed and restarts too.
    let raced = {
        let _lock = lock_claims(builder, &path)?;
        match read_claims(&path) {
            Ok(claims) => !claims.holders.is_empty(),
            Err(_) => true,
        }
    };
    let restarted = if raced && stopped {
        tracing::warn!(
            target: "velnor.buildkit",
            builder,
            "holders arrived during release stop; restarting daemon"
        );
        start().unwrap_or(false)
    } else {
        false
    };
    Ok(ReleaseOutcome {
        removed_last,
        stopped,
        restarted,
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
    let _lock = lock_claims(builder, &path)?;
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
    let _lock = lock_claims(builder, &path)?;
    let mut claims = read_claims(&path)?;
    repair_absent_unlocked(&mut claims, present);
    write_claims(&path, &claims)?;
    let mut holders: Vec<BuilderHolder> = claims.holders.into_values().collect();
    holders.sort_by(|left, right| left.container.cmp(&right.container));
    Ok(holders)
}

/// What one cap-enforcement pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CapReport {
    pub evicted: Vec<String>,
    pub failures: Vec<String>,
}

/// Evict least-recently-used unclaimed builders in (scope, repository) down
/// to [`MAX_BUILDERS_PER_SCOPE_REPO`]. Selection reads claim files unlocked
/// as a hint; every eviction rechecks emptiness under the claim lock, acts
/// through `remove` unlocked, then deletes the claim file only when a final
/// locked recheck still finds it empty — a holder that arrived mid-eviction
/// keeps both its daemon and its claim. Torn files read as claimed and are
/// never evicted; legacy files without identity metadata are skipped (the
/// horizon path deletes them once idle). Best-effort by design: failures
/// are reported, never raised, so setup never fails on eviction.
pub(crate) fn enforce_builder_cap(
    run_root: &Path,
    scope: &str,
    repository: Option<&str>,
    remove: impl Fn(&str) -> Result<()>,
) -> CapReport {
    let mut report = CapReport::default();
    let scope = sanitize_builder_segment(scope);
    let repo = repo_slug(repository);
    let dir = run_root.join(CLAIMS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            report.failures.push(format!(
                "list claim files under {}: {error:#}",
                dir.display()
            ));
            return report;
        }
    };
    // Hint pass, unlocked: group members with their LRU clocks. The hint is
    // never a decision — eviction rechecks under the lock.
    let mut members: Vec<(String, u64)> = Vec::new();
    for entry in entries.flatten() {
        let claims = match std::fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BuilderClaims>(&bytes).ok())
        {
            Some(claims) => claims,
            None => continue,
        };
        if claims.scope != scope || claims.repo != repo || claims.builder.is_empty() {
            continue;
        }
        if !is_persistent_builder_name(&claims.builder) {
            continue;
        }
        members.push((claims.builder, claims.updated_unix));
    }
    members.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    let mut over = members.len().saturating_sub(MAX_BUILDERS_PER_SCOPE_REPO);
    for (builder, _) in members {
        if over == 0 {
            break;
        }
        let path = claims_file(run_root, &builder);
        let empty = {
            let _lock = match lock_claims(&builder, &path) {
                Ok(lock) => lock,
                Err(error) => {
                    report
                        .failures
                        .push(format!("lock claims for {builder}: {error:#}"));
                    continue;
                }
            };
            match read_claims(&path) {
                Ok(claims) => claims.holders.is_empty(),
                Err(_) => continue,
            }
        };
        if !empty {
            continue;
        }
        if let Err(error) = remove(&builder) {
            report
                .failures
                .push(format!("evict builder {builder}: {error:#}"));
            continue;
        }
        tracing::debug!(
            target: "velnor.buildkit",
            builder,
            "cap eviction removed unclaimed builder"
        );
        let _lock = match lock_claims(&builder, &path) {
            Ok(lock) => lock,
            Err(error) => {
                report
                    .failures
                    .push(format!("relock claims for {builder}: {error:#}"));
                continue;
            }
        };
        match read_claims(&path) {
            Ok(claims) if claims.holders.is_empty() => {
                if let Err(error) = std::fs::remove_file(&path) {
                    report
                        .failures
                        .push(format!("delete claims for {builder}: {error:#}"));
                    continue;
                }
            }
            Ok(_) => {
                tracing::warn!(
                    target: "velnor.buildkit",
                    builder,
                    "holders arrived during cap eviction; claim file kept"
                );
                continue;
            }
            Err(_) => continue,
        }
        report.evicted.push(builder);
        over -= 1;
    }
    report
}

/// True when the periodic horizon pass is due: no marker, an unreadable
/// marker, or one older than [`HORIZON_REAP_INTERVAL`].
fn horizon_reap_due(marker: &Path, now: SystemTime) -> bool {
    let elapsed = std::fs::read(marker)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .and_then(|stamp| {
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(stamp))
                .and_then(|marked| now.duration_since(marked).ok())
        });
    elapsed.is_none_or(|elapsed| elapsed >= HORIZON_REAP_INTERVAL)
}

/// Run the horizon pass when [`horizon_reap_due`], then stamp the marker.
/// Returns `None` when the pass was not due. Called best-effort from the
/// setup path so long-lived daemons converge without startup, doctor, or
/// an operator; failures are stamped too — a failing Engine must not make
/// every setup pay for a pass that cannot succeed.
pub(crate) fn maybe_reap_idle_builders(run_root: &Path, now: SystemTime) -> Option<HorizonReport> {
    let marker = run_root.join(CLAIMS_DIR).join(HORIZON_REAP_MARKER);
    if !horizon_reap_due(&marker, now) {
        return None;
    }
    let report = reap_idle_builders(run_root, now);
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
        .to_string();
    let _ = std::fs::write(&marker, stamp);
    Some(report)
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

/// Start one builder's daemon. Missing reads as already gone. The undo half
/// of every stop that runs outside the claim lock: when a recheck finds
/// holders that arrived mid-stop, this brings the daemon back for them.
pub(crate) fn start_builder_daemon(builder: &str) -> Result<bool> {
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

/// Delete one builder's daemon container and state volume, then its claim
/// file. Only ever called with zero holders past the idle horizon: the next
/// build recreates a cold daemon from the same stable name, and the next
/// claim recreates the claim file. The claim file dies with the builder so
/// workflow-minted names cannot accumulate claim files forever.
pub(crate) fn remove_builder_and_claims(run_root: &Path, builder: &str) -> Result<()> {
    remove_builder(builder)?;
    let path = claims_file(run_root, builder);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
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
        // Locked repair and holder check only: the prune and stop below run
        // unlocked, then a recheck restarts the daemon when a setup raced.
        // A torn claim file reads as claimed and is skipped.
        let unclaimed = {
            let _lock = match lock_claims(&builder, &path) {
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
            claims.holders.is_empty()
        };
        if !unclaimed {
            continue;
        }
        // Prune while running, then stop: buildx refuses both du and prune
        // on a stopped daemon, so stop-first would prune nothing.
        let prune_started = Instant::now();
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
        tracing::debug!(
            target: "velnor.buildkit",
            builder,
            prune_ms = prune_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "pressure prune ran outside the claim lock"
        );
        if let Err(error) = stop_builder_daemon(&builder) {
            report
                .failures
                .push(format!("stop builder {builder}: {error:#}"));
        }
        let raced = match holders_remain(&path, &builder) {
            Ok(raced) => raced,
            Err(error) => {
                report
                    .failures
                    .push(format!("relock claims for {builder}: {error:#}"));
                continue;
            }
        };
        if raced {
            tracing::warn!(
                target: "velnor.buildkit",
                builder,
                "holders arrived during pressure prune; restarting daemon"
            );
            if let Err(error) = start_builder_daemon(&builder) {
                report
                    .failures
                    .push(format!("restart builder {builder}: {error:#}"));
            }
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

/// Locked holder recheck after unlocked Docker work. A torn claim file
/// reads as claimed, so the caller stops, restarts, or keeps — never
/// deletes.
fn holders_remain(path: &Path, builder: &str) -> Result<bool> {
    let _lock = lock_claims(builder, path)?;
    match read_claims(path) {
        Ok(claims) => Ok(!claims.holders.is_empty()),
        Err(_) => Ok(true),
    }
}

/// Converge every builder leak the claims cannot see: unclaimed running
/// daemons are stopped (a next build restarts its daemon in seconds, proven
/// live), and unclaimed daemons stopped longer than [`IDLE_DELETE_AFTER`]
/// are deleted with their state volume and claim file. Called from daemon
/// startup, doctor, and periodically from the setup path — never deleting
/// a builder any holder, however stale-looking, still references without
/// the absent repair.
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
        // Locked repair and holder check only: inspect, stop, and delete
        // below run unlocked, each followed by a recheck that keeps a
        // builder a setup claimed mid-pass. A torn claim file reads as
        // claimed and is skipped.
        let path = claims_file(run_root, &builder);
        let unclaimed = {
            let _lock = match lock_claims(&builder, &path) {
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
            claims.holders.is_empty()
        };
        if !unclaimed {
            continue;
        }
        let daemon = daemon_container_name(&builder);
        let exit = match docker.inspect_exit(&daemon) {
            Ok(exit) => exit,
            Err(error) if crate::docker::client::is_not_found(&error) => {
                // Daemon gone but the builder registration lingers (a
                // `buildx rm --keep-state` past, or a crashed delete):
                // recheck, then remove the registration, any orphaned
                // volume, and the claim file.
                match holders_remain(&path, &builder) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        report
                            .failures
                            .push(format!("relock claims for {builder}: {error:#}"));
                        continue;
                    }
                }
                match remove_builder_and_claims(run_root, &builder) {
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
            // daemon started outside any claim. Stopping is safe — the next
            // build restarts it — but deleting is not considered; a setup
            // that raced the stop gets the daemon restarted.
            let stop_started = Instant::now();
            match stop_builder_daemon(&builder) {
                Ok(true) => report.stopped.push(builder.clone()),
                Ok(false) => {}
                Err(error) => {
                    report
                        .failures
                        .push(format!("stop builder {builder}: {error:#}"));
                    continue;
                }
            }
            tracing::debug!(
                target: "velnor.buildkit",
                builder,
                stop_ms = stop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                "horizon stop ran outside the claim lock"
            );
            match holders_remain(&path, &builder) {
                Ok(true) => {
                    tracing::warn!(
                        target: "velnor.buildkit",
                        builder,
                        "holders arrived during horizon stop; restarting daemon"
                    );
                    if let Err(error) = start_builder_daemon(&builder) {
                        report
                            .failures
                            .push(format!("restart builder {builder}: {error:#}"));
                    }
                }
                Ok(false) => {}
                Err(error) => report
                    .failures
                    .push(format!("relock claims for {builder}: {error:#}")),
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
            Some(_) => {
                match holders_remain(&path, &builder) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        report
                            .failures
                            .push(format!("relock claims for {builder}: {error:#}"));
                        continue;
                    }
                }
                match remove_builder_and_claims(run_root, &builder) {
                    Ok(()) => report.deleted.push(builder.clone()),
                    Err(error) => report
                        .failures
                        .push(format!("delete builder {builder}: {error:#}")),
                }
            }
        }
    }
    report
}

/// Arguments listing running container names on the host Engine. Running
/// only — no `--all`: a stopped container is a corpse, and counting corpses
/// as present pinned cross-slot claims forever (a crashed slot's stopped
/// job container kept its holds, so no release ever stopped the daemon and
/// no pass ever pruned or deleted it).
fn container_list_args() -> Vec<String> {
    vec![
        "ps".to_string(),
        "--format".to_string(),
        "{{.Names}}".to_string(),
    ]
}

/// Names of every running container on the host Engine, for absent-holder
/// repair. One `ps`, parsed leniently. A failed listing propagates as an
/// error — never an empty set that would drop every hold — while a
/// successful empty listing legitimately repairs everything (no containers,
/// no live jobs). Stopped corpses are absent by design and release their
/// cross-slot holds here.
fn running_container_names() -> Result<BTreeSet<String>> {
    let args = container_list_args();
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

    fn test_identity() -> BuilderIdentity<'static> {
        BuilderIdentity {
            scope: "trusted",
            tier: TRUST_TIER_BRANCH,
            repository: Some("o/r"),
        }
    }

    fn test_builder() -> String {
        persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r"))
    }

    #[test]
    fn persistent_names_partition_by_trust_repo_and_request() {
        // Default requested name: short form.
        assert_eq!(
            persistent_builder_name(
                "velnor-builder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            ),
            "velnor-builder-shared-trusted-branch-octocat_hello-world"
        );
        // Custom requested name rides along.
        assert_eq!(
            persistent_builder_name(
                "mybuilder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            ),
            "velnor-builder-shared-trusted-branch-octocat_hello-world-mybuilder"
        );
        // Fork-PR jobs never share with trusted jobs.
        assert_eq!(
            persistent_builder_name(
                "velnor-builder",
                "untrusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            ),
            "velnor-builder-shared-untrusted-branch-octocat_hello-world"
        );
        // No repository: parseable, never collides with owner_repo.
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, None),
            "velnor-builder-shared-trusted-branch-no_repo"
        );
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("  ")),
            "velnor-builder-shared-trusted-branch-no_repo"
        );
        // Hostile segments sanitize identically everywhere the name travels.
        assert_eq!(
            persistent_builder_name("../../x", "trusted", TRUST_TIER_BRANCH, Some("o/r")),
            "velnor-builder-shared-trusted-branch-o_r-.._.._x"
        );
    }

    #[test]
    fn builder_keys_separate_release_from_branch() {
        // Tier derivation: release needs affirmative release-grade evidence,
        // branch needs affirmative branch-grade evidence, anything else is
        // unknown and isolated from both.
        assert_eq!(
            builder_trust_tier(Some("refs/heads/main"), None, Some("push"), Some("true")),
            TRUST_TIER_RELEASE
        );
        assert_eq!(
            builder_trust_tier(Some("refs/tags/v1.0.0"), None, Some("push"), None),
            TRUST_TIER_RELEASE
        );
        assert_eq!(
            builder_trust_tier(None, Some("tag"), Some("push"), None),
            TRUST_TIER_RELEASE
        );
        assert_eq!(
            builder_trust_tier(Some("refs/heads/main"), None, Some("release"), None),
            TRUST_TIER_RELEASE
        );
        assert_eq!(
            builder_trust_tier(Some("refs/heads/feature"), None, Some("push"), None),
            TRUST_TIER_BRANCH
        );
        assert_eq!(
            builder_trust_tier(Some("refs/pull/42/merge"), None, Some("pull_request"), None),
            TRUST_TIER_BRANCH
        );
        assert_eq!(
            builder_trust_tier(None, None, None, None),
            TRUST_TIER_UNKNOWN
        );
        assert_eq!(
            builder_trust_tier(Some(""), Some(""), Some(""), Some("")),
            TRUST_TIER_UNKNOWN
        );
        // The P0: a branch job and a release job on the same repo in the
        // same pool must never share a daemon's ID-keyed cache mounts.
        let branch = persistent_builder_name(
            "velnor-builder",
            "trusted",
            builder_trust_tier(Some("refs/heads/feature"), None, Some("push"), None),
            Some("o/r"),
        );
        let release = persistent_builder_name(
            "velnor-builder",
            "trusted",
            builder_trust_tier(Some("refs/tags/v1.0.0"), None, Some("push"), None),
            Some("o/r"),
        );
        let unknown =
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_UNKNOWN, Some("o/r"));
        assert_ne!(branch, release);
        assert_ne!(branch, unknown);
        assert_ne!(release, unknown);
    }

    #[test]
    fn persistent_matching_is_prefix_anchored_and_fail_safe() {
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-trusted-branch-o_r"
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
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0_state"
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
                    persistent_builder_name(
                        "velnor-builder",
                        "trusted",
                        TRUST_TIER_BRANCH,
                        Some("o/r")
                    )
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
            daemon_container_name("velnor-builder-shared-trusted-branch-o_r"),
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0"
        );
        assert_eq!(
            daemon_state_volume("velnor-builder-shared-trusted-branch-o_r"),
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0_state"
        );
    }

    #[test]
    fn claims_count_holders_and_report_the_final_release() {
        let root = temp_root("claims");
        let run_root = root.join("run");
        let builder = test_builder();
        let identity = test_identity();

        claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-a").unwrap();
        // Idempotent: a second setup step in the same job holds once.
        claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-a").unwrap();
        claim_builder(&run_root, &builder, &identity, "slot-2", "velnor-job-b").unwrap();
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 2);

        // First release: another holder remains, no stop.
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || panic!("must not stop with holders"),
            || panic!("must not start with holders"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            }
        );
        // Final release: the only state in which stopping is safe.
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-b",
            || Ok(true),
            || panic!("must not restart without a race"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
                restarted: false,
            }
        );
        // Releasing again (post after post, teardown after post): succeeds,
        // reports nothing to stop.
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-b",
            || Ok(true),
            || panic!("must not restart without a race"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            }
        );
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-never-held",
            || panic!("must not stop without a hold"),
            || panic!("must not start without a hold"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            }
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn release_does_not_hold_the_claim_lock_across_stop() {
        let root = temp_root("release-unlocked");
        let run_root = root.join("run");
        let builder = test_builder();
        let identity = test_identity();
        claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-a").unwrap();

        // The stop closure claims as a racing setup would. Under the old
        // lock-across-stop this deadlocks (same-process re-entrant flock
        // blocks forever); with the shortened critical section the claim
        // succeeds, the recheck observes it, and the daemon restarts.
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || {
                claim_builder(&run_root, &builder, &identity, "slot-2", "velnor-job-b").unwrap();
                Ok(true)
            },
            || Ok(true),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
                restarted: true,
            }
        );
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slot_repair_drops_only_crashed_same_slot_holds() {
        let root = temp_root("slot-repair");
        let run_root = root.join("run");
        let builder = test_builder();
        let identity = test_identity();

        // Crashed job on slot-1 never released; live job on slot-2 holds.
        claim_builder(
            &run_root,
            &builder,
            &identity,
            "slot-1",
            "velnor-job-crashed",
        )
        .unwrap();
        claim_builder(&run_root, &builder, &identity, "slot-2", "velnor-job-live").unwrap();

        // The next job on slot-1 claims: the crashed hold drops, the live
        // cross-slot hold survives.
        claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-next").unwrap();
        let holders = builder_holders(&run_root, &builder, None).unwrap();
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
        let builder = test_builder();
        let identity = test_identity();

        claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-gone").unwrap();
        claim_builder(&run_root, &builder, &identity, "slot-2", "velnor-job-here").unwrap();

        let present: BTreeSet<String> = ["velnor-job-here".to_string()].into_iter().collect();
        let holders = repair_absent_holders(&run_root, &builder, &present).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, "velnor-job-here");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn torn_claim_files_read_claimed_never_released() {
        let root = temp_root("torn");
        let run_root = root.join("run");
        let builder = test_builder();
        let identity = test_identity();
        let path = claims_file(&run_root, &builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();

        // Torn reads fail closed: holders queries error, claims refuse to
        // drop unknown holds, and releases stop nothing.
        assert!(builder_holders(&run_root, &builder, None).is_err());
        assert!(claim_builder(&run_root, &builder, &identity, "slot-1", "velnor-job-a").is_err());
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || panic!("must not stop on a torn file"),
            || panic!("must not start on a torn file"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            }
        );
        // Atomic writes leave no temp files behind: a successful claim on
        // a sibling builder writes and renames, then the temp is gone.
        let sibling = persistent_builder_name("sibling", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        claim_builder(&run_root, &sibling, &identity, "slot-9", "velnor-job-s").unwrap();
        let leftover: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(&format!("tmp-{}", std::process::id()))
            })
            .collect();
        assert!(leftover.is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builder_cap_evicts_lru_unclaimed_builders() {
        let root = temp_root("cap");
        let run_root = root.join("run");
        let identity = test_identity();
        // One over cap: the oldest builder stays claimed (never evicted),
        // the next-oldest unclaimed builder is the victim.
        let mut builders = Vec::new();
        for index in 0..=MAX_BUILDERS_PER_SCOPE_REPO {
            let builder = persistent_builder_name(
                &format!("custom-{index}"),
                "trusted",
                TRUST_TIER_BRANCH,
                Some("o/r"),
            );
            claim_builder(
                &run_root,
                &builder,
                &identity,
                &format!("slot-{index}"),
                &format!("velnor-job-{index}"),
            )
            .unwrap();
            builders.push(builder);
        }
        for builder in builders.iter().skip(1) {
            let path = claims_file(&run_root, builder);
            let _lock = lock_claims(builder, &path).unwrap();
            let mut claims = read_claims(&path).unwrap();
            claims.holders.clear();
            write_claims(&path, &claims).unwrap();
        }
        // LRU clocks increase with the index: backdate explicitly so the
        // test does not depend on wall-clock granularity.
        for (index, builder) in builders.iter().enumerate() {
            let path = claims_file(&run_root, builder);
            let _lock = lock_claims(builder, &path).unwrap();
            let mut claims = read_claims(&path).unwrap();
            claims.updated_unix = 1_000 + index as u64;
            write_claims(&path, &claims).unwrap();
        }

        let evicted: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let report = enforce_builder_cap(&run_root, "trusted", Some("o/r"), |builder| {
            evicted.borrow_mut().push(builder.to_string());
            Ok(())
        });
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.evicted, vec![builders[1].clone()]);
        // The victim's daemon went through `remove` and its claim file is
        // gone; the claimed oldest and every other builder survive.
        assert_eq!(*evicted.borrow(), vec![builders[1].clone()]);
        assert!(!claims_file(&run_root, &builders[1]).exists());
        assert!(claims_file(&run_root, &builders[0]).exists());
        assert_eq!(
            builder_holders(&run_root, &builders[0], None)
                .unwrap()
                .len(),
            1
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn container_listing_excludes_stopped_corpses() {
        // The absent-holder repair treats "not listed" as dead, so the
        // listing must be running-only: `--all` would count stopped
        // cross-slot corpses as present and pin their claims forever.
        assert!(!container_list_args().iter().any(|arg| arg == "--all"));
        assert_eq!(
            container_list_args(),
            vec![
                "ps".to_string(),
                "--format".to_string(),
                "{{.Names}}".to_string(),
            ]
        );
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

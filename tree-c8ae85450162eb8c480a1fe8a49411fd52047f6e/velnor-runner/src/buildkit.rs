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
//! * **Unbounded.** A builder daemon carries no CPU/memory ceiling: claims
//!   record only holder identity, creation passes no resource sizing, and
//!   no setup, release, or teardown ever resizes the daemon. Host capacity
//!   is governed by the single `max_jobs=N` permit ledger, never by
//!   per-daemon ceilings. The owned name includes an unbounded generation so
//!   a builder created by older capped code cannot be silently reused.
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
//! Lifecycle gate: setup and release take the filesystem coordinator shared;
//! the horizon reaper takes it exclusive across the Buildx/container scans
//! and deletion. Cache-pressure pruning runs under the same exclusive gate
//! from cache reclamation. The per-builder lock protects claim and owner
//! record updates, including register-before-create and delete-after-remove.
//! A queued job may be admitted while the reaper runs, but cannot claim or
//! create a builder until the exclusive pass finishes.
//!
//! Workflow-requested builder names have no per-group ceiling. Runtime claims
//! live under `/run`, while a durable owner record under the storage lib root
//! preserves current-generation identity across reboot. The horizon pass
//! converges registered current builders and exact reserved pre-generation
//! names whose missing runtime claims pass a host-wide quiescence check.
//!
//! Torn claims and owner records fail closed, so their builder is never
//! stopped, pruned, or deleted. Every torn read logs an ERROR with its path;
//! the operator recovery is to quiesce jobs, repair the record, and let the
//! next claim recreate runtime state. Doctor surfaces unreadable claim files.
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
use serde::de::{MapAccess, Visitor};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Reserved prefix that marks the Velnor-owned persistent builder namespace,
/// including generations:
/// `velnor-builder-shared-<generation>-<scope>-<tier>-<repo>[-<requested>]`.
/// Prefix-anchored matching keeps arbitrary external builder names out of
/// Velnor's destroy/orphan decisions.
pub(crate) const PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-";

/// Namespace for builders created without the retired per-daemon ceilings.
/// Changing this generation makes setup create a clean, unconstrained daemon
/// instead of reusing an older Buildx container with capped HostConfig.
const CURRENT_PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-unbounded-v1-";

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

/// Durable exact-name ownership records. Claim holders are runtime state;
/// these records let maintenance recognize current builders after reboot.
const OWNER_REGISTRY_DIR: &str = "buildkit-owners";
const OWNER_REGISTRY_VERSION: u32 = 1;

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

/// Stable, generation-versioned builder name for (requested name, effective
/// trust scope, trust tier, repository). Trust first, like the store namespaces; the tier keeps
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
        format!("{CURRENT_PERSISTENT_BUILDER_PREFIX}{scope}-{tier}-{repo}")
    } else {
        format!("{CURRENT_PERSISTENT_BUILDER_PREFIX}{scope}-{tier}-{repo}-{requested}")
    }
}

/// True when `builder` names a persistent builder. Prefix-anchored: a legacy
/// `velnor-builder-<requested>-<slot>` only matches when its requested name
/// starts with `shared-`, in which case teardown skips it (fail-safe) and the
/// horizon path deletes it once idle.
pub(crate) fn is_persistent_builder_name(builder: &str) -> bool {
    builder.starts_with(PERSISTENT_BUILDER_PREFIX)
}

/// True only for names the retired formatter could generate: a sanitized
/// scope, one canonical trust tier, a sanitized repo, and an optional
/// sanitized requested name. The `unbounded-*` generation is never legacy.
fn is_legacy_capped_builder_name(builder: &str) -> bool {
    let Some(rest) = builder.strip_prefix(PERSISTENT_BUILDER_PREFIX) else {
        return false;
    };
    if builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX) || rest.starts_with("unbounded-") {
        return false;
    }
    [TRUST_TIER_BRANCH, TRUST_TIER_RELEASE, TRUST_TIER_UNKNOWN]
        .into_iter()
        .any(|tier| {
            let marker = format!("-{tier}-");
            rest.match_indices(&marker).any(|(offset, _)| {
                let scope = &rest[..offset];
                let tail = &rest[offset + marker.len()..];
                is_old_builder_segment(scope) && is_old_repo_and_requested(tail)
            })
        })
}

fn is_old_builder_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && !matches!(segment, "." | "..")
        && segment.chars().all(|character| {
            character.is_ascii_alphanumeric() || "-_".contains(character) || character == '.'
        })
}

fn is_old_repo_and_requested(value: &str) -> bool {
    if is_old_builder_segment(value) {
        return true;
    }
    value.match_indices('-').any(|(offset, _)| {
        is_old_builder_segment(&value[..offset]) && is_old_builder_segment(&value[offset + 1..])
    })
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct BuilderHolder {
    /// The holder's job container name (`velnor-job-...`).
    pub container: String,
    /// The holder's slot scope, for crash repair by slot exclusivity.
    pub slot: String,
    /// When the hold was taken, unix seconds.
    pub claimed_unix: u64,
}

#[derive(serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum BuilderHolderField {
    Container,
    Slot,
    ClaimedUnix,
    #[serde(other)]
    Unknown,
}

struct BuilderHolderVisitor;

impl<'de> Visitor<'de> for BuilderHolderVisitor {
    type Value = BuilderHolder;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a complete BuildKit builder holder")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut container = None;
        let mut slot = None;
        let mut claimed_unix = None;

        while let Some(field) = map.next_key()? {
            match field {
                BuilderHolderField::Container => {
                    if container.is_some() {
                        return Err(serde::de::Error::duplicate_field("container"));
                    }
                    container = Some(map.next_value()?);
                }
                BuilderHolderField::Slot => {
                    if slot.is_some() {
                        return Err(serde::de::Error::duplicate_field("slot"));
                    }
                    slot = Some(map.next_value()?);
                }
                BuilderHolderField::ClaimedUnix => {
                    if claimed_unix.is_some() {
                        return Err(serde::de::Error::duplicate_field("claimed_unix"));
                    }
                    claimed_unix = Some(map.next_value()?);
                }
                BuilderHolderField::Unknown => {
                    let _: serde::de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(BuilderHolder {
            container: container.ok_or_else(|| serde::de::Error::missing_field("container"))?,
            slot: slot.ok_or_else(|| serde::de::Error::missing_field("slot"))?,
            claimed_unix: claimed_unix
                .ok_or_else(|| serde::de::Error::missing_field("claimed_unix"))?,
        })
    }
}

impl<'de> serde::Deserialize<'de> for BuilderHolder {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(BuilderHolderVisitor)
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct BuilderClaims {
    holders: BTreeMap<String, BuilderHolder>,
    /// Full builder name this file guards. The file name is a sanitized
    /// (and truncated) derivation of it, so the reverse mapping lives here
    /// for ownership checks and deletion.
    builder: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderOwnerRecord {
    version: u32,
    builder: String,
}

fn claims_file(run_root: &Path, builder: &str) -> PathBuf {
    run_root.join(CLAIMS_DIR).join(format!(
        "{}.json",
        crate::container::sanitize_store_key(builder)
    ))
}

fn owner_registry_root(lib_root: &Path) -> PathBuf {
    lib_root.join(OWNER_REGISTRY_DIR)
}

pub(crate) fn claims_registry_root() -> Option<PathBuf> {
    crate::storage::StorageLayout::resolve().map(|layout| owner_registry_root(&layout.lib_root))
}

fn owner_registry_file(registry_root: &Path, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    registry_root.join(format!("{digest}.json"))
}

fn read_owner_record(registry_root: &Path, builder: &str) -> Result<Option<BuilderOwnerRecord>> {
    if !builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX) {
        return Ok(None);
    }
    let path = owner_registry_file(registry_root, builder);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let record: BuilderOwnerRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if record.version != OWNER_REGISTRY_VERSION || record.builder != builder {
        anyhow::bail!(
            "owner record {} does not match builder {builder}",
            path.display()
        );
    }
    Ok(Some(record))
}

fn ensure_owner_record(registry_root: &Path, builder: &str) -> Result<()> {
    if !builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX) {
        return Ok(());
    }
    if read_owner_record(registry_root, builder)?.is_some() {
        return Ok(());
    }
    let record = BuilderOwnerRecord {
        version: OWNER_REGISTRY_VERSION,
        builder: builder.to_string(),
    };
    let path = owner_registry_file(registry_root, builder);
    let bytes = serde_json::to_vec_pretty(&record).context("encode BuildKit owner record")?;
    write_atomic_document(&path, &bytes)
}

fn remove_owner_record(registry_root: &Path, builder: &str) -> Result<()> {
    if !builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX) {
        return Ok(());
    }
    let path = owner_registry_file(registry_root, builder);
    match std::fs::remove_file(&path) {
        Ok(()) => sync_parent(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
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
    parse_claims(path, &bytes)
}

fn parse_claims(path: &Path, bytes: &[u8]) -> Result<BuilderClaims> {
    let claims: BuilderClaims =
        serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))?;
    for (container, holder) in &claims.holders {
        if container != &holder.container {
            anyhow::bail!(
                "claim file {} maps holder key {container} to container {}",
                path.display(),
                holder.container
            );
        }
    }
    Ok(claims)
}

/// Read a Velnor ownership record without treating a missing or mismatched
/// file as an empty claim. A reserved-looking name alone is not proof that an
/// external Buildx builder belongs to this daemon.
fn read_registered_claims(path: &Path, builder: &str) -> Result<Option<BuilderClaims>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let claims = parse_claims(path, &bytes)?;
    if claims.builder == builder && is_persistent_builder_name(&claims.builder) {
        Ok(Some(claims))
    } else {
        Ok(None)
    }
}

/// Read ownership for a maintenance pass. Old capped names predate the
/// current generation and their claim files lived under `/run`, so a reboot
/// can remove the owner record while leaving Docker's builder container.
/// The old generation's reserved namespace is itself the durable owner marker;
/// an existing mismatched or unreadable file still fails closed.
fn read_claims_for_reaping(
    path: &Path,
    builder: &str,
    registry_root: Option<&Path>,
    allow_legacy_missing: bool,
) -> Result<Option<BuilderClaims>> {
    if let Some(claims) = read_registered_claims(path, builder)? {
        if builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX)
            && let Some(registry_root) = registry_root
        {
            // An existing malformed or mismatched durable record is a hard
            // stop. A valid runtime claim may recreate a missing marker below.
            let _ = read_owner_record(registry_root, builder)?;
        }
        return Ok(Some(claims));
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    }
    if is_legacy_capped_builder_name(builder) && allow_legacy_missing {
        // Explicit migration exception: this exact old formatter namespace
        // was the durable owner marker before records survived reboot.
        return Ok(Some(BuilderClaims {
            builder: builder.to_string(),
            ..BuilderClaims::default()
        }));
    }
    if builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX)
        && let Some(registry_root) = registry_root
        && read_owner_record(registry_root, builder)?.is_some()
    {
        return Ok(Some(BuilderClaims {
            builder: builder.to_string(),
            ..BuilderClaims::default()
        }));
    }
    Ok(None)
}

/// Atomically replace one runtime claim file.
fn write_claims(path: &Path, claims: &BuilderClaims) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(claims).context("encode builder claims")?;
    write_atomic_document(path, &bytes)
}

/// Atomically replace one JSON document: write and fsync a sibling temp,
/// rename it, then fsync the parent directory. A crash leaves either the
/// previous record or the new record, never a partial target.
fn write_atomic_document(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let write_result = (|| -> Result<()> {
        {
            use std::io::Write as _;
            let mut temp_file = std::fs::File::create(&temp)
                .with_context(|| format!("write {}", temp.display()))?;
            temp_file
                .write_all(bytes)
                .with_context(|| format!("write {}", temp.display()))?;
            temp_file
                .sync_all()
                .with_context(|| format!("fsync {}", temp.display()))?;
        }
        std::fs::rename(&temp, path)
            .with_context(|| format!("rename {} to {}", temp.display(), path.display()))?;
        sync_parent(path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .with_context(|| format!("open directory {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("fsync directory {}", parent.display()))?;
    }
    Ok(())
}

/// Log a torn claim file at ERROR with the operator recovery step. Torn
/// files fail closed as claimed at every read site, so without an operator
/// visit the builder pins forever: never stopped, pruned, or deleted.
fn log_torn_claims(builder: &str, path: &Path, error: &anyhow::Error) {
    tracing::error!(
        target: "velnor.buildkit",
        builder,
        path = %path.display(),
        error = format!("{error:#}"),
        "torn claim file treated as claimed; to recover: quiesce this daemon's jobs, \
         delete the claim file, and let the next claim recreate it"
    );
}

fn log_unreadable_ownership(builder: &str, claims_path: &Path, error: &anyhow::Error) {
    tracing::error!(
        target: "velnor.buildkit",
        builder,
        claims_path = %claims_path.display(),
        error = format!("{error:#}"),
        "unreadable BuildKit ownership state treated as claimed; quiesce jobs, repair the runtime claim or durable owner record, then retry"
    );
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
/// setup-buildx steps, one name) holds once. Current-generation owner records
/// are committed before setup can create or reuse the Buildx object.
pub(crate) fn claim_builder(
    run_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    let registry_root = claims_registry_root();
    claim_builder_with_registry(run_root, registry_root.as_deref(), builder, slot, container)
}

fn claim_builder_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let mut claims = match read_claims(&path) {
        Ok(claims) => claims,
        Err(error) => {
            log_torn_claims(builder, &path, &error);
            return Err(error);
        }
    };
    if !claims.builder.is_empty() && claims.builder != builder {
        anyhow::bail!(
            "claim file {} names builder {}, expected {builder}",
            path.display(),
            claims.builder
        );
    }
    if let Some(registry_root) = registry_root {
        ensure_owner_record(registry_root, builder)?;
    }
    repair_slot_unlocked(&mut claims, slot, container);
    claims.holders.insert(
        container.to_string(),
        BuilderHolder {
            container: container.to_string(),
            slot: slot.to_string(),
            claimed_unix: unix_now(),
        },
    );
    claims.builder = builder.to_string();
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
    let _lifecycle = crate::capacity::FilesystemCoordinator::lock_shared(run_root)
        .context("lock BuildKit lifecycle for release")?;
    let path = claims_file(run_root, builder);
    let removed_last = {
        let _lock = lock_claims(builder, &path)?;
        let mut claims = match read_claims(&path) {
            Ok(claims) => claims,
            Err(error) => {
                log_torn_claims(builder, &path, &error);
                return Ok(ReleaseOutcome {
                    removed_last: false,
                    stopped: false,
                    restarted: false,
                });
            }
        };
        let removed = claims.holders.remove(container).is_some();
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
            Err(error) => {
                log_torn_claims(builder, &path, &error);
                true
            }
        }
    };
    let restarted = if raced && stopped {
        tracing::warn!(
            target: "velnor.buildkit",
            builder,
            "holders arrived during release stop; restarting daemon"
        );
        match start() {
            Ok(running) => running,
            Err(error) => {
                tracing::error!(
                    target: "velnor.buildkit",
                    builder,
                    error = format!("{error:#}"),
                    "release restart failed; daemon left stopped with holders"
                );
                false
            }
        }
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
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
    maybe_reap_idle_builders_with(run_root, now, reap_idle_builders)
}

/// [`maybe_reap_idle_builders`] with the pass injected, so tests cover the
/// due/stamp logic without touching the Engine.
fn maybe_reap_idle_builders_with(
    run_root: &Path,
    now: SystemTime,
    reap: impl FnOnce(&Path, SystemTime) -> HorizonReport,
) -> Option<HorizonReport> {
    let marker = run_root.join(CLAIMS_DIR).join(HORIZON_REAP_MARKER);
    if !horizon_reap_due(&marker, now) {
        return None;
    }
    let report = reap(run_root, now);
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

/// Stop one builder's daemon. Returns true when the daemon exists (a stop
/// acted or it was already stopped — exit 0 meant true here before the
/// migration, and the CLI leg still cannot distinguish); a missing
/// daemon reads as already stopped. Only ever called with zero holders.
pub(crate) fn stop_builder_daemon(builder: &str) -> Result<bool> {
    let daemon = daemon_container_name(builder);
    match crate::docker::Docker::host().container_stop(&daemon, None) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stop BuildKit daemon {daemon}")),
    }
}

/// Start one builder's daemon. Missing reads as already gone. The undo half
/// of every stop that runs outside the claim lock: when a recheck finds
/// holders that arrived mid-stop, this brings the daemon back for them.
pub(crate) fn start_builder_daemon(builder: &str) -> Result<bool> {
    let daemon = daemon_container_name(builder);
    match crate::docker::Docker::host().container_start(&daemon) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("start BuildKit daemon {daemon}")),
    }
}

/// Prune one builder's cache completely, returning the du-measured bytes
/// freed. A missing builder (or one that vanishes mid-prune) frees nothing.
/// A stopped daemon is started first: `buildx du` and `buildx prune` both
/// refuse a stopped daemon (proven live). Called only after a locked holder
/// check found zero holders, but the prune itself runs unlocked — a racing
/// setup claims concurrently, then the recheck restarts the daemon for it.
/// A racing build caught mid-prune may observe the cold; that bounded,
/// observable race is the price of never holding the lock across Docker.
fn prune_builder(builder: &str) -> Result<u64> {
    let mut docker = crate::docker::Docker::host();
    let before = match docker.buildx_disk_usage(builder) {
        Ok(usage) => usage,
        Err(error) if crate::docker::client::is_not_running(&error) => {
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
        // Missing daemon (narrow engine vocabulary) or missing buildx
        // registration (`no builder "x" found`, typed at the Docker
        // boundary) both free nothing.
        if crate::docker::client::is_not_found(&error)
            || crate::docker::client::is_buildkit_builder_not_found(&error)
        {
            return Ok(0);
        }
        return Err(error).with_context(|| format!("prune BuildKit builder {builder}"));
    }
    let after = docker.buildx_disk_usage(builder).unwrap_or(0);
    Ok(before.saturating_sub(after))
}

/// Delete one builder's daemon and state volume, then its claim file. The
/// caller has already proved the name belongs to Velnor and has no holders.
fn remove_builder_and_claims_with(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    mut remove: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    match read_registered_claims(&path, builder) {
        Ok(Some(claims)) if claims.holders.is_empty() => {}
        Ok(Some(_)) => return Ok(false),
        Ok(None) => return Ok(false),
        Err(error) => {
            log_torn_claims(builder, &path, &error);
            return Err(error);
        }
    }
    if let Some(registry_root) = registry_root {
        ensure_owner_record(registry_root, builder)?;
    }
    // Keep the same per-builder lock from the final empty-claim proof through
    // exact daemon/volume removal and durable owner-record deletion. Setup
    // cannot publish a replacement claim between these steps.
    remove(builder)?;
    match std::fs::remove_file(&path) {
        Ok(()) => sync_parent(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
    }
    if let Some(registry_root) = registry_root {
        remove_owner_record(registry_root, builder)?;
    }
    Ok(true)
}

fn delete_registered_builder(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    remove: &mut impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    remove_builder_and_claims_with(run_root, registry_root, builder, |name| remove(name))
}

/// Delete one builder's daemon container and state volume. Only ever called
/// with zero holders past the idle horizon: the next build recreates a cold
/// daemon from the same stable name.
pub(crate) fn remove_builder(builder: &str) -> Result<()> {
    let daemon = daemon_container_name(builder);
    if let Err(error) = crate::docker::Docker::host().container_remove(&daemon, true, false) {
        return Err(error).with_context(|| format!("remove BuildKit daemon {daemon}"));
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
            if crate::docker::client::is_not_found(&error) {
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
    let registry_root = claims_registry_root();
    let mut persistent = Vec::new();
    for builder in builders.into_iter().filter(|builder| {
        is_persistent_builder_name(builder) && !is_legacy_capped_builder_name(builder)
    }) {
        match read_claims_for_reaping(
            &claims_file(run_root, &builder),
            &builder,
            registry_root.as_deref(),
            false,
        ) {
            Ok(Some(_)) => persistent.push(builder),
            Ok(None) => report.failures.push(format!(
                "skip BuildKit builder {builder}: Velnor ownership record is absent or mismatched"
            )),
            Err(error) => report.failures.push(format!(
                "skip BuildKit builder {builder}: ownership record is unreadable ({error:#})"
            )),
        }
    }
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
    // never a decision: the holder check below runs under each builder's
    // claim lock, then the prune and stop run unlocked with a recheck that
    // restarts the daemon when a setup raced.
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
            let mut claims = match read_claims_for_reaping(
                &path,
                &builder,
                registry_root.as_deref(),
                false,
            ) {
                Ok(Some(claims)) => claims,
                Ok(None) => {
                    report.failures.push(format!(
                        "skip BuildKit builder {builder}: Velnor ownership record is absent or mismatched"
                    ));
                    continue;
                }
                Err(error) => {
                    log_unreadable_ownership(&builder, &path, &error);
                    report
                        .failures
                        .push(format!("read claims for {builder}: {error:#}"));
                    continue;
                }
            };
            if let Some(registry_root) = registry_root.as_deref()
                && let Err(error) = ensure_owner_record(registry_root, &builder)
            {
                report
                    .failures
                    .push(format!("ensure durable ownership for {builder}: {error:#}"));
                continue;
            }
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
    /// Runtime claim or durable owner records the pass could not read. Each
    /// pins its builder until an operator repairs it; doctor surfaces these
    /// as actionable errors.
    pub unreadable_claims: Vec<String>,
}

fn reconcile_orphan_owner_records(
    run_root: &Path,
    registry_root: &Path,
    registered_builders: &BTreeSet<String>,
    present: &BTreeSet<String>,
    report: &mut HorizonReport,
) {
    let entries = match std::fs::read_dir(registry_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            report.failures.push(format!(
                "list durable BuildKit owner records under {}: {error:#}",
                registry_root.display()
            ));
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry)
                if entry.path().extension().and_then(|value| value.to_str()) == Some("json") =>
            {
                paths.push(entry.path());
            }
            Ok(_) => {}
            Err(error) => report
                .failures
                .push(format!("read BuildKit owner registry entry: {error:#}")),
        }
    }
    paths.sort();
    for path in paths {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                report
                    .failures
                    .push(format!("read owner record {}: {error:#}", path.display()));
                continue;
            }
        };
        let record: BuilderOwnerRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                report
                    .failures
                    .push(format!("parse owner record {}: {error:#}", path.display()));
                continue;
            }
        };
        if record.version != OWNER_REGISTRY_VERSION
            || !record
                .builder
                .starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX)
            || owner_registry_file(registry_root, &record.builder) != path
        {
            report.failures.push(format!(
                "keep mismatched BuildKit owner record {}",
                path.display()
            ));
            continue;
        }
        if registered_builders.contains(&record.builder) {
            continue;
        }

        let claim_path = claims_file(run_root, &record.builder);
        let _lock = match lock_claims(&record.builder, &claim_path) {
            Ok(lock) => lock,
            Err(error) => {
                report.failures.push(format!(
                    "lock orphan BuildKit ownership for {}: {error:#}",
                    record.builder
                ));
                continue;
            }
        };
        match read_owner_record(registry_root, &record.builder) {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(error) => {
                report.failures.push(format!(
                    "recheck owner record for {}: {error:#}",
                    record.builder
                ));
                continue;
            }
        }
        let mut claims = match read_registered_claims(&claim_path, &record.builder) {
            Ok(Some(claims)) => claims,
            Ok(None) => match std::fs::symlink_metadata(&claim_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => BuilderClaims {
                    builder: record.builder.clone(),
                    ..BuilderClaims::default()
                },
                Ok(_) => {
                    report.failures.push(format!(
                        "keep orphan BuildKit owner record {}: runtime claim mismatches",
                        record.builder
                    ));
                    continue;
                }
                Err(error) => {
                    report.failures.push(format!(
                        "stat runtime claims for {}: {error:#}",
                        record.builder
                    ));
                    continue;
                }
            },
            Err(error) => {
                report.failures.push(format!(
                    "keep orphan BuildKit owner record {}: runtime claims unreadable ({error:#})",
                    record.builder
                ));
                continue;
            }
        };
        repair_absent_unlocked(&mut claims, present);
        if !claims.holders.is_empty() {
            continue;
        }
        if claim_path.exists() {
            match std::fs::remove_file(&claim_path) {
                Ok(()) => {
                    if let Err(error) = sync_parent(&claim_path) {
                        report.failures.push(format!(
                            "sync removed claims for {}: {error:#}",
                            record.builder
                        ));
                        continue;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    report.failures.push(format!(
                        "remove orphan claims for {}: {error:#}",
                        record.builder
                    ));
                    continue;
                }
            }
        }
        if let Err(error) = remove_owner_record(registry_root, &record.builder) {
            report.failures.push(format!(
                "remove orphan owner record for {}: {error:#}",
                record.builder
            ));
        }
    }
}

/// Locked holder recheck after unlocked Docker work. A torn claim file
/// reads as claimed, so the caller stops, restarts, or keeps — never
/// deletes.
fn holders_remain(path: &Path, builder: &str) -> Result<bool> {
    let _lock = lock_claims(builder, path)?;
    match read_claims(path) {
        Ok(claims) => Ok(!claims.holders.is_empty()),
        Err(error) => {
            log_torn_claims(builder, path, &error);
            Ok(true)
        }
    }
}

/// Converge owned builders under the filesystem-wide lifecycle lock. Current
/// names require a matching durable owner record or readable claim. Missing
/// `/run` state is recoverable only for the exact old formatter namespace,
/// after the global running-job scan proves the old generation quiescent.
pub(crate) fn reap_idle_builders(run_root: &Path, now: SystemTime) -> HorizonReport {
    let _coordinator = match crate::capacity::FilesystemCoordinator::lock_exclusive(run_root) {
        Ok(coordinator) => coordinator,
        Err(error) => {
            return HorizonReport {
                failures: vec![format!(
                    "lock BuildKit lifecycle for horizon reap: {error:#}"
                )],
                ..HorizonReport::default()
            };
        }
    };
    let registry_root = claims_registry_root();
    reap_idle_builders_with_registry(
        run_root,
        registry_root.as_deref(),
        now,
        || crate::docker::Docker::host().buildx_builders(),
        running_container_names,
        |daemon| crate::docker::Docker::host().inspect_exit(daemon),
        stop_builder_daemon,
        start_builder_daemon,
        remove_builder,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper paths hermetic in tests"
)]
fn reap_idle_builders_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    now: SystemTime,
    list_builders: impl FnOnce() -> Result<Vec<String>>,
    list_present_containers: impl FnOnce() -> Result<BTreeSet<String>>,
    mut inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    mut stop: impl FnMut(&str) -> Result<bool>,
    mut start: impl FnMut(&str) -> Result<bool>,
    mut remove: impl FnMut(&str) -> Result<()>,
) -> HorizonReport {
    let mut report = HorizonReport::default();
    let builders = match list_builders() {
        Ok(builders) => builders,
        Err(error) => {
            report
                .failures
                .push(format!("list builders for horizon reap: {error:#}"));
            return report;
        }
    };
    let present = match list_present_containers() {
        Ok(present) => present,
        Err(error) => {
            report
                .failures
                .push(format!("list containers for claim repair: {error:#}"));
            return report;
        }
    };
    let active_job_container = present
        .iter()
        .any(|name| name.starts_with(crate::docker_lease::JOB_CONTAINER_NAME_PREFIX));
    let registered_builders: BTreeSet<String> = builders.iter().cloned().collect();
    for builder in builders
        .into_iter()
        .filter(|builder| is_persistent_builder_name(builder))
    {
        let path = claims_file(run_root, &builder);
        let legacy_missing_claim = if is_legacy_capped_builder_name(&builder) {
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    report
                        .failures
                        .push(format!("stat ownership for {builder}: {error:#}"));
                    continue;
                }
                Ok(_) => false,
            }
        } else {
            false
        };
        if legacy_missing_claim && active_job_container {
            report.failures.push(format!(
                "leave old BuildKit builder {builder} untouched: a running job container prevents reboot quiescence proof"
            ));
            continue;
        }
        match read_claims_for_reaping(&path, &builder, registry_root, !active_job_container) {
            Ok(Some(_)) => {}
            Ok(None) => {
                report.failures.push(format!(
                    "leave BuildKit builder {builder} untouched: Velnor ownership record is absent or mismatched"
                ));
                continue;
            }
            Err(error) => {
                log_unreadable_ownership(&builder, &path, &error);
                report
                    .failures
                    .push(format!("read ownership for {builder}: {error:#}"));
                report.unreadable_claims.push(path.display().to_string());
                continue;
            }
        }
        // Locked repair and holder check only: inspect, stop, and delete
        // below run unlocked, each followed by a recheck that keeps a
        // builder claimed mid-pass. Missing or mismatched records fail closed.
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
            let mut claims = match read_claims_for_reaping(
                &path,
                &builder,
                registry_root,
                !active_job_container,
            ) {
                Ok(Some(claims)) => claims,
                Ok(None) => {
                    report.failures.push(format!(
                        "leave BuildKit builder {builder} untouched: Velnor ownership record changed before cleanup"
                    ));
                    continue;
                }
                Err(error) => {
                    log_unreadable_ownership(&builder, &path, &error);
                    report
                        .failures
                        .push(format!("read claims for {builder}: {error:#}"));
                    report.unreadable_claims.push(path.display().to_string());
                    continue;
                }
            };
            if let Some(registry_root) = registry_root
                && builder.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX)
                && let Err(error) = ensure_owner_record(registry_root, &builder)
            {
                report
                    .failures
                    .push(format!("ensure durable ownership for {builder}: {error:#}"));
                continue;
            }
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
        let legacy_capped = is_legacy_capped_builder_name(&builder);
        let daemon = daemon_container_name(&builder);
        let exit = match inspect_exit(&daemon) {
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
                match delete_registered_builder(run_root, registry_root, &builder, &mut remove) {
                    Ok(true) => report.deleted.push(builder.clone()),
                    Ok(false) => {}
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
            match stop(&builder) {
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
            let raced = match holders_remain(&path, &builder) {
                Ok(true) => {
                    tracing::warn!(
                        target: "velnor.buildkit",
                        builder,
                        "holders arrived during horizon stop; restarting daemon"
                    );
                    if let Err(error) = start(&builder) {
                        report
                            .failures
                            .push(format!("restart builder {builder}: {error:#}"));
                    }
                    true
                }
                Ok(false) => false,
                Err(error) => {
                    report
                        .failures
                        .push(format!("relock claims for {builder}: {error:#}"));
                    true
                }
            };
            if legacy_capped && !raced {
                match delete_registered_builder(run_root, registry_root, &builder, &mut remove) {
                    Ok(true) => report.deleted.push(builder.clone()),
                    Ok(false) => {}
                    Err(error) => report
                        .failures
                        .push(format!("delete builder {builder}: {error:#}")),
                }
            }
            continue;
        }
        if legacy_capped {
            if !exit
                .status
                .is_some_and(crate::docker::client::ContainerState::safe_to_reclaim)
            {
                report.failures.push(format!(
                    "keep legacy BuildKit builder {builder}: daemon state is not proven inactive"
                ));
                continue;
            }
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
            match delete_registered_builder(run_root, registry_root, &builder, &mut remove) {
                Ok(true) => report.deleted.push(builder.clone()),
                Ok(false) => {}
                Err(error) => report
                    .failures
                    .push(format!("delete builder {builder}: {error:#}")),
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
                match delete_registered_builder(run_root, registry_root, &builder, &mut remove) {
                    Ok(true) => report.deleted.push(builder.clone()),
                    Ok(false) => {}
                    Err(error) => report
                        .failures
                        .push(format!("delete builder {builder}: {error:#}")),
                }
            }
        }
    }
    if let Some(registry_root) = registry_root {
        reconcile_orphan_owner_records(
            run_root,
            registry_root,
            &registered_builders,
            &present,
            &mut report,
        );
    }
    report
}

/// Arguments listing running container names on the host Engine. Running
/// only — no `--all`: a stopped container is a corpse, and counting corpses
/// as present pinned cross-slot claims forever (a crashed slot's stopped
/// job container kept its holds, so no release ever stopped the daemon and
/// no pass ever pruned or deleted it).
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper tests hermetic"
)]
fn reap_idle_builders_with(
    run_root: &Path,
    now: SystemTime,
    list_builders: impl FnOnce() -> Result<Vec<String>>,
    list_present_containers: impl FnOnce() -> Result<BTreeSet<String>>,
    inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    stop: impl FnMut(&str) -> Result<bool>,
    start: impl FnMut(&str) -> Result<bool>,
    remove: impl FnMut(&str) -> Result<()>,
) -> HorizonReport {
    reap_idle_builders_with_registry(
        run_root,
        None,
        now,
        list_builders,
        list_present_containers,
        inspect_exit,
        stop,
        start,
        remove,
    )
}

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

    fn test_builder() -> String {
        persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r"))
    }

    /// Test helper: drop every hold on `builder`.
    fn abandon_claims(run_root: &Path, builder: &str) {
        let path = claims_file(run_root, builder);
        let _lock = lock_claims(builder, &path).unwrap();
        let mut claims = read_claims(&path).unwrap();
        claims.holders.clear();
        write_claims(&path, &claims).unwrap();
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
            "velnor-builder-shared-unbounded-v1-trusted-branch-octocat_hello-world"
        );
        // Custom requested name rides along.
        assert_eq!(
            persistent_builder_name(
                "mybuilder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            ),
            "velnor-builder-shared-unbounded-v1-trusted-branch-octocat_hello-world-mybuilder"
        );
        // Fork-PR jobs never share with trusted jobs.
        assert_eq!(
            persistent_builder_name(
                "velnor-builder",
                "untrusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            ),
            "velnor-builder-shared-unbounded-v1-untrusted-branch-octocat_hello-world"
        );
        // No repository: parseable, never collides with owner_repo.
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, None),
            "velnor-builder-shared-unbounded-v1-trusted-branch-no_repo"
        );
        assert_eq!(
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("  ")),
            "velnor-builder-shared-unbounded-v1-trusted-branch-no_repo"
        );
        // Hostile segments sanitize identically everywhere the name travels.
        assert_eq!(
            persistent_builder_name("../../x", "trusted", TRUST_TIER_BRANCH, Some("o/r")),
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r-.._.._x"
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
        let current =
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        assert!(is_persistent_builder_name(&current));
        assert!(!is_legacy_capped_builder_name(&current));
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-trusted-branch-o_r"
        ));
        assert!(is_legacy_capped_builder_name(
            "velnor-builder-shared-trusted-branch-o_r"
        ));
        assert!(!is_legacy_capped_builder_name("arbitrary-external-builder"));
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
            daemon_container_name("velnor-builder-shared-unbounded-v1-trusted-branch-o_r"),
            "buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0"
        );
        assert_eq!(
            daemon_state_volume("velnor-builder-shared-unbounded-v1-trusted-branch-o_r"),
            "buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0_state"
        );
        let old = "velnor-builder-shared-trusted-branch-o_r";
        assert!(is_legacy_capped_builder_name(old));
        assert_eq!(
            daemon_container_name(old),
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0"
        );
        assert_eq!(
            daemon_state_volume(old),
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0_state"
        );
    }

    #[test]
    fn upgrade_reaper_deletes_only_registered_legacy_builders_immediately() {
        let root = temp_root("upgrade-reap-legacy");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let current = test_builder();
        let external = "external-buildx-cache".to_string();
        let unregistered_current = "velnor-builder-shared-unbounded-v1-external-cache".to_string();
        claim_builder(&run_root, &legacy, "slot-old", "velnor-job-old").unwrap();
        abandon_claims(&run_root, &legacy);
        claim_builder(&run_root, &current, "slot-new", "velnor-job-new").unwrap();
        abandon_claims(&run_root, &current);

        let inspected = std::cell::RefCell::new(Vec::new());
        let removed = std::cell::RefCell::new(Vec::new());
        let now = SystemTime::now();
        let report = reap_idle_builders_with(
            &run_root,
            now,
            || {
                Ok(vec![
                    legacy.clone(),
                    external.clone(),
                    unregistered_current.clone(),
                    current.clone(),
                ])
            },
            || Ok(BTreeSet::new()),
            |daemon| {
                inspected.borrow_mut().push(daemon.to_string());
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    // Legacy cleanup is immediate, not an idle-horizon wait.
                    finished: Some(now),
                })
            },
            |_| panic!("stopped builders need no stop call"),
            |_| panic!("no holder race exists"),
            |builder| {
                removed.borrow_mut().push(builder.to_string());
                Ok(())
            },
        );

        assert_eq!(report.deleted, vec![legacy.clone()]);
        assert_eq!(*removed.borrow(), vec![legacy.clone()]);
        assert_eq!(
            *inspected.borrow(),
            vec![
                daemon_container_name(&legacy),
                daemon_container_name(&current)
            ]
        );
        assert!(!claims_file(&run_root, &legacy).exists());
        assert!(claims_file(&run_root, &current).exists());
        assert!(!inspected.borrow().contains(&external));
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&unregistered_current) && failure.contains("untouched")
        }));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn upgrade_reaper_recovers_old_ownership_after_run_state_is_lost() {
        let root = temp_root("upgrade-reap-after-reboot");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let removed = std::cell::RefCell::new(Vec::new());
        let now = SystemTime::now();

        // `/run/velnor` is tmpfs. After a reboot the old BuildKit daemon can
        // remain in Docker while its runtime claim file is gone. The prior
        // generation's reserved namespace is the durable ownership marker.
        assert!(!claims_file(&run_root, &legacy).exists());
        let report = reap_idle_builders_with(
            &run_root,
            now,
            || Ok(vec![legacy.clone(), "external-builder".to_string()]),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: Some(now),
                })
            },
            |_| panic!("stopped old builder needs no stop call"),
            |_| panic!("no holder race exists"),
            |builder| {
                removed.borrow_mut().push(builder.to_string());
                Ok(())
            },
        );

        assert_eq!(report.deleted, vec![legacy.clone()]);
        assert_eq!(*removed.borrow(), vec![legacy.clone()]);
        assert!(!claims_file(&run_root, &legacy).exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_builder_without_runtime_claim_waits_for_host_quiescence() {
        let root = temp_root("legacy-reap-live-job-after-reboot");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let report = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || Ok(vec![legacy.clone()]),
            || Ok(["velnor-job-running".to_string()].into_iter().collect()),
            |_| panic!("running job blocks old-builder inspection"),
            |_| panic!("running job blocks old-builder stop"),
            |_| panic!("running job blocks old-builder restart"),
            |_| panic!("running job blocks old-builder removal"),
        );

        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| { failure.contains(&legacy) && failure.contains("quiescence") }));
        assert!(!claims_file(&run_root, &legacy).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_builder_without_runtime_claim_fails_closed_when_container_scan_fails() {
        let root = temp_root("legacy-reap-ps-error");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let report = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || Ok(vec![legacy.clone()]),
            || Err(anyhow::anyhow!("docker ps unavailable")),
            |_| panic!("failed container scan blocks old-builder inspection"),
            |_| panic!("failed container scan blocks old-builder stop"),
            |_| panic!("failed container scan blocks old-builder restart"),
            |_| panic!("failed container scan blocks old-builder removal"),
        );

        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("docker ps unavailable")));
        assert!(!claims_file(&run_root, &legacy).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_builder_claim_mismatch_or_torn_json_blocks_upgrade_cleanup() {
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let invalid_claims = [
            serde_json::to_vec(&serde_json::json!({
                "builder": "external-builder",
                "holders": {}
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "builder": legacy.clone(),
                "holders": {
                    "velnor-job-live": {
                        "container": "velnor-job-other",
                        "slot": "slot-live",
                        "claimed_unix": 1
                    }
                }
            }))
            .unwrap(),
            b"{torn".to_vec(),
        ];

        for (index, invalid) in invalid_claims.into_iter().enumerate() {
            let root = temp_root(&format!("legacy-invalid-claim-{index}"));
            let run_root = root.join("run");
            let path = claims_file(&run_root, &legacy);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, invalid).unwrap();
            let report = reap_idle_builders_with(
                &run_root,
                SystemTime::now(),
                || Ok(vec![legacy.clone()]),
                || Ok(BTreeSet::new()),
                |_| panic!("invalid claim blocks old-builder inspection"),
                |_| panic!("invalid claim blocks old-builder stop"),
                |_| panic!("invalid claim blocks old-builder restart"),
                |_| panic!("invalid claim blocks old-builder removal"),
            );

            assert!(report.deleted.is_empty());
            assert!(!report.failures.is_empty());
            assert!(path.exists());
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn current_builder_owner_record_recovers_runtime_state_after_reboot() {
        let root = temp_root("current-reap-after-reboot");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_owner_record(&registry_root, &builder).unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);
        assert!(owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000_000);
        let finished = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let removed = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |daemon| {
                assert_eq!(daemon, daemon_container_name(&builder));
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: Some(finished),
                })
            },
            |_| panic!("stopped builder needs no stop call"),
            |_| panic!("no holder race exists"),
            |removed_builder| {
                removed.borrow_mut().push(removed_builder.to_string());
                Ok(())
            },
        );

        assert_eq!(report.deleted, vec![builder.clone()]);
        assert_eq!(*removed.borrow(), vec![builder.clone()]);
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(!owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_or_corrupt_current_owner_records_fail_closed() {
        let root = temp_root("current-owner-record-fail-closed");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let missing = test_builder();
        let corrupt = persistent_builder_name("custom", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_owner_record(&registry_root, &corrupt).unwrap();
        std::fs::write(owner_registry_file(&registry_root, &corrupt), b"{torn").unwrap();

        let inspected = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![missing.clone(), corrupt.clone()]),
            || Ok(BTreeSet::new()),
            |daemon| {
                inspected.borrow_mut().push(daemon.to_string());
                panic!("missing or corrupt owner record blocks inspection")
            },
            |_| panic!("missing or corrupt owner record blocks stop"),
            |_| panic!("missing or corrupt owner record blocks restart"),
            |_| panic!("missing or corrupt owner record blocks removal"),
        );

        assert!(inspected.borrow().is_empty());
        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains(&missing)));
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains(&corrupt)));
        assert!(owner_registry_file(&registry_root, &corrupt).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_record_is_removed_only_after_successful_empty_builder_listing() {
        let root = temp_root("orphan-owner-record");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder =
            persistent_builder_name("failed-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_owner_record(&registry_root, &builder).unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec!["external-builder".to_string()]),
            || Ok(BTreeSet::new()),
            |_| panic!("orphan record is absent from the successful builder listing"),
            |_| panic!("orphan record has no daemon to stop"),
            |_| panic!("orphan record has no daemon to restart"),
            |_| panic!("orphan record has no daemon to remove"),
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(!owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_record_repairs_vanished_claim_holders_before_cleanup() {
        let root = temp_root("orphan-owner-vanished-holder");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder =
            persistent_builder_name("failed-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-gone",
            "velnor-job-gone",
        )
        .unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(Vec::new()),
            || Ok(BTreeSet::new()),
            |_| panic!("orphan record is absent from the builder listing"),
            |_| panic!("orphan record has no daemon to stop"),
            |_| panic!("orphan record has no daemon to restart"),
            |_| panic!("orphan record has no daemon to remove"),
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(!owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_records_survive_failed_builder_or_container_listings() {
        let root = temp_root("orphan-owner-list-error");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder =
            persistent_builder_name("failed-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_owner_record(&registry_root, &builder).unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);

        let builder_list_error = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Err(anyhow::anyhow!("buildx listing failed")),
            || panic!("container listing follows builder listing"),
            |_| panic!("failed listing blocks inspection"),
            |_| panic!("failed listing blocks stop"),
            |_| panic!("failed listing blocks restart"),
            |_| panic!("failed listing blocks removal"),
        );
        assert!(builder_list_error
            .failures
            .iter()
            .any(|failure| failure.contains("buildx listing failed")));
        assert!(owner_path.exists());

        let container_list_error = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(Vec::new()),
            || Err(anyhow::anyhow!("docker ps failed")),
            |_| panic!("failed listing blocks inspection"),
            |_| panic!("failed listing blocks stop"),
            |_| panic!("failed listing blocks restart"),
            |_| panic!("failed listing blocks removal"),
        );
        assert!(container_list_error
            .failures
            .iter()
            .any(|failure| failure.contains("docker ps failed")));
        assert!(owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_record_with_active_or_mismatched_claim_is_retained() {
        let root = temp_root("orphan-owner-claim-proof");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let active =
            persistent_builder_name("active-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_owner_record(&registry_root, &active).unwrap();
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &active,
            "slot-live",
            "velnor-job-live",
        )
        .unwrap();

        let mismatched = persistent_builder_name(
            "mismatched-create",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        );
        ensure_owner_record(&registry_root, &mismatched).unwrap();
        let mismatch_path = claims_file(&run_root, &mismatched);
        std::fs::create_dir_all(mismatch_path.parent().unwrap()).unwrap();
        std::fs::write(
            &mismatch_path,
            serde_json::to_vec(&serde_json::json!({
                "builder": "external-builder",
                "holders": {}
            }))
            .unwrap(),
        )
        .unwrap();

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(Vec::new()),
            || Ok(["velnor-job-live".to_string()].into_iter().collect()),
            |_| panic!("orphan records are absent from the builder listing"),
            |_| panic!("orphan records have no daemon to stop"),
            |_| panic!("orphan records have no daemon to restart"),
            |_| panic!("orphan records have no daemon to remove"),
        );

        assert!(owner_registry_file(&registry_root, &active).exists());
        assert!(owner_registry_file(&registry_root, &mismatched).exists());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains(&mismatched)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn upgrade_reaper_waits_for_old_holders_then_removes_after_absent_repair() {
        let root = temp_root("upgrade-reap-live-holder");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        claim_builder(&run_root, &legacy, "slot-old", "velnor-job-live").unwrap();

        let held = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || Ok(vec![legacy.clone()]),
            || Ok(["velnor-job-live".to_string()].into_iter().collect()),
            |_| panic!("live holder must prevent daemon inspection"),
            |_| panic!("live holder must prevent stopping"),
            |_| panic!("live holder must not require a restart"),
            |_| panic!("live holder must prevent deletion"),
        );
        assert!(held.deleted.is_empty());
        assert_eq!(builder_holders(&run_root, &legacy, None).unwrap().len(), 1);

        let removed = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || Ok(vec![legacy.clone()]),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Running),
                    finished: None,
                })
            },
            |builder| {
                assert_eq!(builder, legacy);
                Ok(true)
            },
            |_| panic!("absent repair found no racing holder"),
            |builder| {
                removed.borrow_mut().push(builder.to_string());
                Ok(())
            },
        );
        assert_eq!(report.stopped, vec![legacy.clone()]);
        assert_eq!(report.deleted, vec![legacy.clone()]);
        assert_eq!(*removed.borrow(), vec![legacy.clone()]);
        assert!(builder_holders(&run_root, &legacy, None)
            .unwrap()
            .is_empty());
        assert!(!claims_file(&run_root, &legacy).exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn upgrade_reaper_restarts_if_a_holder_arrives_during_legacy_stop() {
        let root = temp_root("upgrade-reap-stop-race");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        claim_builder(&run_root, &legacy, "slot-old", "velnor-job-old").unwrap();
        abandon_claims(&run_root, &legacy);

        let removed = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || Ok(vec![legacy.clone()]),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Running),
                    finished: None,
                })
            },
            |builder| {
                claim_builder(&run_root, builder, "slot-new", "velnor-job-new").unwrap();
                Ok(true)
            },
            |_| Ok(true),
            |builder| {
                removed.borrow_mut().push(builder.to_string());
                Ok(())
            },
        );

        assert!(report.deleted.is_empty());
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(removed.borrow().is_empty());
        assert_eq!(builder_holders(&run_root, &legacy, None).unwrap().len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn upgrade_reaper_retries_cleanup_after_missing_daemon_and_delete_failure() {
        let root = temp_root("upgrade-reap-retry");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        claim_builder(&run_root, &legacy, "slot-old", "velnor-job-old").unwrap();
        abandon_claims(&run_root, &legacy);
        let now = SystemTime::now();
        let inspect_missing = || {
            Err(anyhow::Error::new(crate::docker::client::NotFound {
                object: daemon_container_name(&legacy),
            }))
        };

        let first = reap_idle_builders_with(
            &run_root,
            now,
            || Ok(vec![legacy.clone()]),
            || Ok(BTreeSet::new()),
            |_| inspect_missing(),
            |_| panic!("missing daemon must not be stopped"),
            |_| panic!("no active holder needs restart"),
            |_| Err(anyhow::anyhow!("simulated interrupted volume cleanup")),
        );
        assert!(first.deleted.is_empty());
        assert!(first
            .failures
            .iter()
            .any(|failure| failure.contains("simulated interrupted volume cleanup")));
        assert!(claims_file(&run_root, &legacy).exists());

        let second = reap_idle_builders_with(
            &run_root,
            now,
            || Ok(vec![legacy.clone()]),
            || Ok(BTreeSet::new()),
            |_| inspect_missing(),
            |_| panic!("missing daemon must not be stopped"),
            |_| panic!("no active holder needs restart"),
            |_| Ok(()),
        );
        assert_eq!(second.deleted, vec![legacy.clone()]);
        assert!(!claims_file(&run_root, &legacy).exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn claims_count_holders_and_report_the_final_release() {
        let root = temp_root("claims");
        let run_root = root.join("run");
        let builder = test_builder();

        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        // Idempotent: a second setup step in the same job holds once.
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").unwrap();
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
    fn claim_reader_ignores_retired_entitlement_metadata() {
        let root = temp_root("claim-schema");
        let run_root = root.join("run");
        let builder = test_builder();
        let path = claims_file(&run_root, &builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Claims written before entitlements were removed carry
        // `cpu_milli`/`memory_bytes` and old grouping metadata. Unknown
        // fields are ignored; the holder identity still parses.
        let legacy_shape = serde_json::json!({
            "holders": {
                "velnor-job-old": {
                    "container": "velnor-job-old",
                    "slot": "slot-old",
                    "claimed_unix": 1,
                    "cpu_milli": 4000,
                    "memory_bytes": 1024
                }
            },
            "builder": builder.clone(),
            "scope": "trusted",
            "tier": TRUST_TIER_BRANCH,
            "repo": "o_r",
            "updated_unix": 1
        });
        std::fs::write(&path, serde_json::to_vec(&legacy_shape).unwrap()).unwrap();

        let claims = read_claims(&path).unwrap();
        assert_eq!(claims.holders.len(), 1);
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, "velnor-job-old");
        assert_eq!(holders[0].slot, "slot-old");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn holder_set_tracks_claims_by_identity() {
        let root = temp_root("holder-set");
        let run_root = root.join("run");
        let builder = test_builder();

        // No claim file yet: zero holders, not "unreadable".
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, "velnor-job-a");
        assert_eq!(holders[0].slot, "slot-1");
        claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").unwrap();
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 2);
        // A torn file is unreadable, never guessed.
        std::fs::write(claims_file(&run_root, &builder), "{torn").unwrap();
        assert!(builder_holders(&run_root, &builder, None).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn release_does_not_hold_the_claim_lock_across_stop() {
        let root = temp_root("release-unlocked");
        let run_root = root.join("run");
        let builder = test_builder();
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();

        // The stop closure claims as a racing setup would. Under the old
        // lock-across-stop this deadlocks (same-process re-entrant flock
        // blocks forever); with the shortened critical section the claim
        // succeeds, the recheck observes it, and the daemon restarts.
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || {
                claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").unwrap();
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

        // Crashed job on slot-1 never released; live job on slot-2 holds.
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-crashed").unwrap();
        claim_builder(&run_root, &builder, "slot-2", "velnor-job-live").unwrap();

        // The next job on slot-1 claims: the crashed hold drops, the live
        // cross-slot hold survives.
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-next").unwrap();
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

        claim_builder(&run_root, &builder, "slot-1", "velnor-job-gone").unwrap();
        claim_builder(&run_root, &builder, "slot-2", "velnor-job-here").unwrap();

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
        let path = claims_file(&run_root, &builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();

        // Torn reads fail closed: holders queries error, claims refuse to
        // drop unknown holds, and releases stop nothing.
        assert!(builder_holders(&run_root, &builder, None).is_err());
        assert!(claim_builder(&run_root, &builder, "slot-1", "velnor-job-a",).is_err());
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
        claim_builder(&run_root, &sibling, "slot-9", "velnor-job-s").unwrap();
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
    fn more_than_eight_concurrent_named_builders_are_allowed() {
        let root = temp_root("many-builders");
        let run_root = root.join("run");
        let mut builders = Vec::new();
        for index in 0..12 {
            let builder = persistent_builder_name(
                &format!("custom-{index}"),
                "trusted",
                TRUST_TIER_BRANCH,
                Some("o/r"),
            );
            claim_builder(
                &run_root,
                &builder,
                &format!("slot-{index}"),
                &format!("velnor-job-{index}"),
            )
            .unwrap();
            builders.push(builder);
        }

        assert_eq!(builders.len(), 12);
        for (index, builder) in builders.iter().enumerate() {
            let holders = builder_holders(&run_root, builder, None).unwrap();
            assert_eq!(holders.len(), 1);
            assert_eq!(holders[0].container, format!("velnor-job-{index}"));
        }
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

    #[test]
    fn horizon_reap_due_follows_marker_age() {
        let root = temp_root("reap-due");
        let marker = root.join("marker");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        // Missing marker: due.
        assert!(horizon_reap_due(&marker, now));
        // Fresh marker: not due.
        std::fs::write(&marker, (10_000_000 - 60).to_string()).unwrap();
        assert!(!horizon_reap_due(&marker, now));
        // Stale marker: due.
        let stale = 10_000_000 - HORIZON_REAP_INTERVAL.as_secs() - 1;
        std::fs::write(&marker, stale.to_string()).unwrap();
        assert!(horizon_reap_due(&marker, now));
        // Torn marker: due.
        std::fs::write(&marker, b"not-a-number").unwrap();
        assert!(horizon_reap_due(&marker, now));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn maybe_reap_runs_only_when_due_and_stamps() {
        let root = temp_root("maybe-reap");
        let run_root = root.join("run");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000_000);
        // Due (no marker): runs the pass and stamps.
        let report = maybe_reap_idle_builders_with(&run_root, now, |_, _| HorizonReport {
            stopped: vec!["b".to_string()],
            ..HorizonReport::default()
        });
        assert_eq!(report.unwrap().stopped, vec!["b".to_string()]);
        let marker = run_root.join(CLAIMS_DIR).join(HORIZON_REAP_MARKER);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "20000000");
        // Fresh stamp: skipped without running the pass.
        let report = maybe_reap_idle_builders_with(&run_root, now, |_, _| panic!("must not reap"));
        assert!(report.is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }
}

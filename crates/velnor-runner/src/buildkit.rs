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
//! * **Claims.** One active job owns a persistent builder at a time because
//!   the daemon joins that job's private network. Each setup claims the
//!   builder for its job container; post and teardown release it, and the
//!   daemon stops after the final holder leaves. Claims live in the daemon's
//!   run root next to the scope leases, guarded by the same entry lock.
//! * **Reclamation.** What claims cannot cover, maintenance converges:
//!   disk-pressure reclaim stops and prunes builders with no holders
//!   (largest first, du-measured), and the horizon path deletes builders
//!   idle past [`IDLE_DELETE_AFTER`]. Builders are a cache: every destructive
//!   action degrades the next build to cold, never to wrong.
//! * **Unbounded per daemon, bounded in aggregate.** A builder daemon carries
//!   no CPU/memory ceiling: creation passes no resource sizing, and no setup,
//!   release, or teardown resizes the daemon. Workflow-controlled names are
//!   capped at [`MAX_BUILDERS_PER_SCOPE_TIER_REPO`] durable builders per
//!   (scope, tier, repository), preventing arbitrary names from growing
//!   containers and state volumes without limit.
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
//! rewrite. Retired names cannot authorize admission. A v1 owner row may
//! authorize one-time cleanup only when live Velnor labels and the exact
//! state-volume reference prove the daemon; older unregistered objects stay
//! pinned with an explicit disposition.
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
//! Runtime claims live under `/run`, while a durable owner record under the
//! storage lib root preserves current-generation identity and group membership
//! across reboot. The horizon pass discovers builders from that registry,
//! because each job container has a private Buildx registry. A missing runtime
//! claim is recoverable only after a host-wide quiescence check proves there
//! are no running `velnor-job-*` containers.
//!
//! Torn claims and owner records fail closed, so their builder is never
//! stopped, pruned, or deleted. Every torn read logs an ERROR with its path;
//! the operator recovery is to quiesce jobs, repair the record, and let the
//! next claim recreate runtime state. Doctor surfaces unreadable claim files.
//!
//! Generic job cleanup excludes every Buildx daemon and state volume in the
//! reserved `velnor-builder-` namespace. Reclamation requires a current
//! canonical name and its exact durable owner identity.

use anyhow::{Context, Result};
use serde::de::{MapAccess, Visitor};
#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Reserved prefix for every Velnor Buildx daemon object, including names
/// from retired formats. Only current canonical names are admitted; one-time
/// v1 cleanup also requires exact metadata and live object proof.
pub(crate) const PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-";

/// Namespace for builders created without the retired per-daemon ceilings.
/// Changing this generation makes setup create a clean, unconstrained daemon
/// instead of reusing an older Buildx container with capped HostConfig.
const CURRENT_PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-unbounded-v2-";
/// One-time cleanup namespace emitted by the last pre-identity owner registry.
/// It is never admitted or reused by current setup.
const LEGACY_V1_BUILDER_PREFIX: &str = "velnor-builder-shared-unbounded-v1-";

/// The setup-buildx default builder name. An empty name uses the same stable
/// identity as the default, matching the prior formatter behavior.
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
// Keep v2: bootstrap metadata is additive and missing node fields deserialize
// as Unverified, while cleanup still proves ownership from the existing ID
// and state-volume identity.
const OWNER_REGISTRY_VERSION: u32 = 2;

/// Workflow names are untrusted and unbounded; cap retained names per
/// (scope, trust tier, repository) group before claiming or creating a daemon.
pub(crate) const MAX_BUILDERS_PER_SCOPE_TIER_REPO: usize = 8;

/// One short global admission lock serializes group-cap checks. Incomplete
/// v1 owner records stop admission with an operator-migration error; they are
/// never counted against guessed groups or evicted by name.
const BUILDER_ADMISSION_LOCK: &str = "buildkit-builder-admission";

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
    let requested = if requested.is_empty() || requested == DEFAULT_REQUESTED_NAME {
        DEFAULT_REQUESTED_NAME.to_string()
    } else {
        requested
    };
    let mut identity = blake3::Hasher::new();
    for segment in [scope, tier, repo, requested] {
        identity.update(&(segment.len() as u64).to_be_bytes());
        identity.update(segment.as_bytes());
    }
    let digest = identity.finalize().to_hex();
    format!("{CURRENT_PERSISTENT_BUILDER_PREFIX}{digest}")
}

fn is_owner_registry_builder_name(builder: &str) -> bool {
    is_bounded_builder_name(builder)
}

/// Current setup names are fixed-width BLAKE3 identities. Their shared
/// prefix cannot overlap another generated builder name, so the decimal
/// Buildx node suffix is unambiguous even for appended nodes.
pub(crate) fn is_bounded_builder_name(builder: &str) -> bool {
    builder
        .strip_prefix(CURRENT_PERSISTENT_BUILDER_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// Exact names produced by the retired v1 formatter. These may enter the
/// one-time cleanup pass only when their v1 owner row and live Docker labels
/// also prove ownership; they are never valid admission identities.
fn is_legacy_v1_builder_name(builder: &str) -> bool {
    let Some(rest) = builder.strip_prefix(LEGACY_V1_BUILDER_PREFIX) else {
        return false;
    };
    [TRUST_TIER_BRANCH, TRUST_TIER_RELEASE, TRUST_TIER_UNKNOWN]
        .into_iter()
        .any(|tier| {
            let marker = format!("-{tier}-");
            rest.match_indices(&marker).any(|(offset, _)| {
                is_old_builder_segment(&rest[..offset])
                    && is_old_repo_and_requested(&rest[offset + marker.len()..])
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

/// True only for the current fixed-width builder identity format. Names from
/// retired formats are never admitted or reaped based on their spelling.
pub(crate) fn is_persistent_builder_name(builder: &str) -> bool {
    is_bounded_builder_name(builder)
}

/// True when a buildkitd container or state volume belongs to a persistent
/// Velnor builder. Object names use `buildx_buildkit_<builder><node>` and
/// append `_state` for volumes; reserving the whole namespace prevents generic
/// job teardown from deleting a slot-scoped daemon.
pub(crate) fn is_persistent_builder_object(name: &str) -> bool {
    name.strip_prefix(DAEMON_CONTAINER_PREFIX)
        .is_some_and(|builder| builder.starts_with(PERSISTENT_BUILDER_PREFIX))
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
    /// Job and service containers that must all be absent before this claim
    /// can be repaired or released. Service containers can outlive a failed
    /// job-container teardown while retaining access to its private network.
    pub required_containers: Vec<String>,
    /// The holder's slot scope, for crash repair by slot exclusivity.
    pub slot: String,
    /// When the hold was taken, unix seconds.
    pub claimed_unix: u64,
    /// Whether terminal release may stop the daemon. Persist this with the
    /// holder: Buildx post runs while the job can still use the daemon.
    pub cleanup_on_release: bool,
}

#[derive(serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum BuilderHolderField {
    Container,
    RequiredContainers,
    Slot,
    ClaimedUnix,
    CleanupOnRelease,
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
        let mut required_containers = None;
        let mut slot = None;
        let mut claimed_unix = None;
        let mut cleanup_on_release = None;

        while let Some(field) = map.next_key()? {
            match field {
                BuilderHolderField::Container => {
                    if container.is_some() {
                        return Err(serde::de::Error::duplicate_field("container"));
                    }
                    container = Some(map.next_value()?);
                }
                BuilderHolderField::RequiredContainers => {
                    if required_containers.is_some() {
                        return Err(serde::de::Error::duplicate_field("required_containers"));
                    }
                    required_containers = Some(map.next_value()?);
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
                BuilderHolderField::CleanupOnRelease => {
                    if cleanup_on_release.is_some() {
                        return Err(serde::de::Error::duplicate_field("cleanup_on_release"));
                    }
                    cleanup_on_release = Some(map.next_value()?);
                }
                BuilderHolderField::Unknown => {
                    let _: serde::de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(BuilderHolder {
            container: container.ok_or_else(|| serde::de::Error::missing_field("container"))?,
            required_containers: required_containers
                .ok_or_else(|| serde::de::Error::missing_field("required_containers"))?,
            slot: slot.ok_or_else(|| serde::de::Error::missing_field("slot"))?,
            claimed_unix: claimed_unix
                .ok_or_else(|| serde::de::Error::missing_field("claimed_unix"))?,
            cleanup_on_release: cleanup_on_release
                .ok_or_else(|| serde::de::Error::missing_field("cleanup_on_release"))?,
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
    /// Holder whose teardown release is reserved or in progress. It is set
    /// before job/service removal so maintenance cannot repair the holder
    /// away after those containers disappear. New claims and cleanup remain
    /// blocked until teardown completes or retries.
    releasing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderGroup {
    scope: String,
    tier: String,
    repo: String,
}

impl BuilderGroup {
    fn new(scope: &str, tier: &str, repository: Option<&str>) -> Self {
        Self {
            scope: sanitize_builder_segment(scope),
            tier: sanitize_builder_segment(tier),
            repo: repo_slug(repository),
        }
    }

    fn is_canonical(&self) -> bool {
        !self.scope.is_empty()
            && sanitize_builder_segment(&self.scope) == self.scope
            && [TRUST_TIER_RELEASE, TRUST_TIER_BRANCH, TRUST_TIER_UNKNOWN]
                .contains(&self.tier.as_str())
            && !self.repo.is_empty()
            && sanitize_builder_segment(&self.repo) == self.repo
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BuilderOwnerRecord {
    version: u32,
    builder: String,
    /// Random identity injected by the lease into the persistent daemon and
    /// its state volume. Names alone cannot distinguish an operator-created
    /// replacement from the object Velnor admitted.
    owner_token: Option<String>,
    /// Each admitted Buildx node is keyed by exact Engine name. IDs and state
    /// volume mountpoints are durable, so cleanup never trusts names alone.
    daemon_nodes: BTreeMap<String, BuilderNodeRecord>,
    /// v1 owner files omit identity. Never infer group fields from the
    /// workflow-controlled builder name; admission and cleanup require an
    /// operator migration before using an incomplete row.
    group: Option<BuilderGroup>,
    /// Immutable time this owner identity was first registered. Used only
    /// when a daemon was created but its Engine ID was not durably recorded.
    /// Claims refresh `updated_unix`, never this creation/bootstrap bound.
    #[serde(default)]
    registered_unix: u64,
    /// Updated when setup claims the builder, for deterministic LRU ordering.
    updated_unix: u64,
}

/// The v1 row is preserved only as a cleanup locator. It has no immutable
/// daemon ID or owner token, so live Velnor labels and the exact state-volume
/// reference must supply the missing object proof before removal.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1BuilderOwnerRecord {
    version: u32,
    builder: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderNodeRecord {
    container_id: String,
    state_volume_mountpoint: String,
    /// Immutable time this exact Engine node was first recorded after create.
    /// Old v2 rows deserialize as zero and cannot authorize Created cleanup.
    #[serde(default)]
    created_unix: u64,
    /// Buildx bootstrap progress is durable because a runner restart between
    /// Engine requests must never turn a merely created daemon into a ready
    /// builder. Missing fields in existing v2 node rows intentionally mean
    /// Unverified and no approved config.
    #[serde(default)]
    bootstrap_phase: BuilderBootstrapPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_sha256: Option<String>,
}

/// Durable proof that the narrow Buildx daemon bootstrap completed in order.
/// `Unverified` includes pre-phase owner rows and recovered daemons from the
/// Engine-create / owner-ID-write crash window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BuilderBootstrapPhase {
    #[default]
    Unverified,
    Created,
    Archived,
    Started,
    Ready,
}

impl Default for BuilderOwnerRecord {
    fn default() -> Self {
        Self {
            version: OWNER_REGISTRY_VERSION,
            builder: String::new(),
            owner_token: None,
            daemon_nodes: BTreeMap::new(),
            group: None,
            registered_unix: 0,
            updated_unix: 0,
        }
    }
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
    resolved_storage_layout().map(|layout| owner_registry_root(&layout.lib_root))
}

#[cfg(test)]
thread_local! {
    static TEST_STORAGE_LAYOUT: RefCell<Option<crate::storage::StorageLayout>> = const { RefCell::new(None) };
    static TEST_RUNNING_CONTAINER_NAMES: RefCell<Option<BTreeSet<String>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestStorageLayoutGuard(Option<crate::storage::StorageLayout>);

#[cfg(test)]
impl Drop for TestStorageLayoutGuard {
    fn drop(&mut self) {
        TEST_STORAGE_LAYOUT.with(|layout| *layout.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn use_test_storage_layout(
    layout: crate::storage::StorageLayout,
) -> TestStorageLayoutGuard {
    let previous = TEST_STORAGE_LAYOUT.with(|current| current.borrow_mut().replace(layout));
    TestStorageLayoutGuard(previous)
}

#[cfg(test)]
pub(crate) struct TestRunningContainerNamesGuard(Option<BTreeSet<String>>);

#[cfg(test)]
impl Drop for TestRunningContainerNamesGuard {
    fn drop(&mut self) {
        TEST_RUNNING_CONTAINER_NAMES.with(|names| *names.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn use_test_running_container_names(
    names: BTreeSet<String>,
) -> TestRunningContainerNamesGuard {
    let previous = TEST_RUNNING_CONTAINER_NAMES.with(|current| current.borrow_mut().replace(names));
    TestRunningContainerNamesGuard(previous)
}

fn resolved_storage_layout() -> Option<crate::storage::StorageLayout> {
    #[cfg(test)]
    if let Some(layout) = TEST_STORAGE_LAYOUT.with(|current| current.borrow().clone()) {
        return Some(layout);
    }
    crate::storage::StorageLayout::resolve()
}

fn owner_registry_file(registry_root: &Path, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    registry_root.join(format!("{digest}.json"))
}

fn read_owner_record(registry_root: &Path, builder: &str) -> Result<Option<BuilderOwnerRecord>> {
    if !is_owner_registry_builder_name(builder) {
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
    if record.version != OWNER_REGISTRY_VERSION {
        anyhow::bail!(
            "BuildKit owner record {} has schema version {}; operator migration is required before admission or cleanup",
            path.display(),
            record.version
        );
    }
    if record.builder != builder {
        anyhow::bail!(
            "owner record {} does not match builder {builder}",
            path.display()
        );
    }
    if !owner_record_identity_is_valid(&record) {
        anyhow::bail!(
            "BuildKit owner record {} has incomplete or malformed v2 identity; operator migration is required",
            path.display()
        );
    }
    Ok(Some(record))
}

fn read_legacy_v1_owner_record(
    registry_root: &Path,
    builder: &str,
) -> Result<Option<LegacyV1BuilderOwnerRecord>> {
    if !is_legacy_v1_builder_name(builder) {
        return Ok(None);
    }
    let path = owner_registry_file(registry_root, builder);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let record: LegacyV1BuilderOwnerRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if record.version != 1 || record.builder != builder {
        anyhow::bail!(
            "legacy BuildKit owner record {} does not match v1 builder {builder}",
            path.display()
        );
    }
    Ok(Some(record))
}

fn owner_record_identity_is_valid(record: &BuilderOwnerRecord) -> bool {
    if record
        .owner_token
        .as_deref()
        .is_none_or(|token| !valid_owner_token(token))
        || record
            .group
            .as_ref()
            .is_none_or(|group| !group.is_canonical())
    {
        return false;
    }
    record.daemon_nodes.iter().all(|(daemon, node)| {
        is_builder_daemon_node(daemon, &record.builder)
            && valid_container_id(&node.container_id)
            && !node.state_volume_mountpoint.is_empty()
    })
}

fn ensure_owner_record(
    registry_root: &Path,
    builder: &str,
    group: Option<&BuilderGroup>,
) -> Result<String> {
    if !is_owner_registry_builder_name(builder) {
        anyhow::bail!("cannot register non-owned BuildKit builder {builder:?}");
    }
    let mut record = match read_owner_record(registry_root, builder)? {
        Some(record) => record,
        None => {
            let group = group.context(
                "cannot create BuildKit owner record without canonical scope/tier/repository identity",
            )?;
            if !group.is_canonical() {
                anyhow::bail!("cannot create BuildKit owner record with malformed group identity");
            }
            BuilderOwnerRecord {
                version: OWNER_REGISTRY_VERSION,
                builder: builder.to_string(),
                owner_token: Some(uuid::Uuid::new_v4().simple().to_string()),
                daemon_nodes: BTreeMap::new(),
                group: Some(group.clone()),
                registered_unix: unix_now(),
                updated_unix: 0,
            }
        }
    };
    let owner_token = record
        .owner_token
        .as_deref()
        .filter(|token| valid_owner_token(token))
        .context("BuildKit owner record has no valid identity token")?
        .to_string();
    if record
        .group
        .as_ref()
        .is_some_and(|stored| !stored.is_canonical())
    {
        anyhow::bail!("BuildKit owner record for {builder} has malformed group identity");
    }
    if let Some(group) = group {
        if record.group.as_ref().is_some_and(|stored| stored != group) {
            anyhow::bail!(
                "BuildKit owner record for {builder} has a different scope/tier/repository identity"
            );
        }
        record.group = Some(group.clone());
        record.updated_unix = unix_now();
    }
    let path = owner_registry_file(registry_root, builder);
    let bytes = serde_json::to_vec_pretty(&record).context("encode BuildKit owner record")?;
    write_atomic_document(&path, &bytes).map(|()| owner_token)
}

fn valid_owner_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_container_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_config_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_bootstrap_transition(
    expected_phase: BuilderBootstrapPhase,
    next_phase: BuilderBootstrapPhase,
) -> bool {
    matches!(
        (expected_phase, next_phase),
        (
            BuilderBootstrapPhase::Created,
            BuilderBootstrapPhase::Archived
        ) | (
            BuilderBootstrapPhase::Archived,
            BuilderBootstrapPhase::Started
        ) | (BuilderBootstrapPhase::Started, BuilderBootstrapPhase::Ready)
    )
}

fn validate_node_bootstrap_state(node: &BuilderNodeRecord) -> Result<()> {
    match (node.bootstrap_phase, node.config_sha256.as_deref()) {
        (BuilderBootstrapPhase::Unverified, None) => Ok(()),
        (BuilderBootstrapPhase::Created, Some(digest))
        | (BuilderBootstrapPhase::Archived, Some(digest))
        | (BuilderBootstrapPhase::Started, Some(digest))
        | (BuilderBootstrapPhase::Ready, Some(digest))
            if valid_config_sha256(digest) =>
        {
            Ok(())
        }
        _ => anyhow::bail!("BuildKit owner node has inconsistent bootstrap phase/config identity"),
    }
}

fn owner_token_from_registry(registry_root: &Path, builder: &str) -> Result<String> {
    let record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    let token = record
        .owner_token
        .filter(|token| valid_owner_token(token))
        .with_context(|| {
            format!("BuildKit owner record for {builder} has no valid identity token")
        })?;
    Ok(token)
}

fn registered_owner_record(builder: &str) -> Result<BuilderOwnerRecord> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    read_owner_record(&registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))
}

pub(crate) fn registered_owner_token(builder: &str) -> Result<String> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    owner_token_from_registry(&registry_root, builder)
}

pub(crate) fn registered_owner_container_id(builder: &str) -> Result<Option<String>> {
    let record = registered_owner_record(builder)?;
    let token = record
        .owner_token
        .as_deref()
        .filter(|token| valid_owner_token(token))
        .context("BuildKit owner record has no valid identity token")?;
    let _ = token;
    let node = record.daemon_nodes.get(&daemon_container_name(builder));
    if node.is_some_and(|node| !valid_container_id(&node.container_id)) {
        anyhow::bail!("BuildKit owner record has malformed container ID");
    }
    Ok(node.map(|node| node.container_id.clone()))
}

/// Return the exact durable daemon ID and its bootstrap proof, if a node was
/// recorded. Legacy rows deserialize to Unverified and cannot be reused as
/// ready daemons; cleanup still relies only on the independent ID/volume
/// ownership fields.
pub(crate) fn registered_owner_bootstrap_state(
    builder: &str,
) -> Result<Option<(String, BuilderBootstrapPhase, Option<String>)>> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("cannot read bootstrap state for a noncanonical BuildKit builder");
    }
    let record = registered_owner_record(builder)?;
    let Some(node) = record.daemon_nodes.get(&daemon_container_name(builder)) else {
        return Ok(None);
    };
    if !valid_container_id(&node.container_id) {
        anyhow::bail!("BuildKit owner record has malformed immutable daemon ID");
    }
    validate_node_bootstrap_state(node)?;
    Ok(Some((
        node.container_id.clone(),
        node.bootstrap_phase,
        node.config_sha256.clone(),
    )))
}

#[cfg(test)]
pub(crate) fn seed_test_admitted_builder_bootstrap_node(
    builder: &str,
    container_id: &str,
    bootstrap_phase: BuilderBootstrapPhase,
    config_sha256: &str,
) -> Result<String> {
    if !is_bounded_builder_name(builder)
        || !valid_container_id(container_id)
        || !valid_config_sha256(config_sha256)
    {
        anyhow::bail!("invalid test BuildKit bootstrap identity");
    }
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let mut record = read_owner_record(&registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    let owner_token = record
        .owner_token
        .clone()
        .filter(|token| valid_owner_token(token))
        .with_context(|| format!("BuildKit owner record for {builder} has no valid token"))?;
    let daemon_name = daemon_container_name(builder);
    let state_volume = daemon_state_volume(builder);
    record.daemon_nodes.insert(
        daemon_name,
        BuilderNodeRecord {
            container_id: container_id.to_owned(),
            state_volume_mountpoint: format!("/var/lib/docker/volumes/{state_volume}/_data"),
            created_unix: unix_now(),
            bootstrap_phase,
            config_sha256: Some(config_sha256.to_owned()),
        },
    );
    if !owner_record_identity_is_valid(&record) {
        anyhow::bail!("seeded test BuildKit owner record has invalid identity");
    }
    write_atomic_document(
        &owner_registry_file(&registry_root, builder),
        &serde_json::to_vec_pretty(&record).context("encode test BuildKit owner record")?,
    )?;
    Ok(owner_token)
}

pub(crate) fn record_admitted_builder_container_created(
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
    job_container: &str,
    created_id: &str,
    config_sha256: &str,
) -> Result<()> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    record_admitted_builder_container_created_with(
        &registry_root,
        &run_root,
        builder,
        owner_token,
        daemon_name,
        job_container,
        created_id,
        config_sha256,
        inspect_builder_container,
        inspect_builder_volume,
        container_ids_using_volume,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Engine inspections keep create identity persistence hermetic in tests"
)]
fn record_admitted_builder_container_created_with(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
    job_container: &str,
    created_id: &str,
    config_sha256: &str,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<()> {
    if !is_bounded_builder_name(builder)
        || daemon_name != daemon_container_name(builder)
        || !valid_owner_token(owner_token)
        || !valid_container_id(created_id)
        || !valid_config_sha256(config_sha256)
    {
        anyhow::bail!("invalid admitted BuildKit daemon create identity");
    }
    let claim_path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &claim_path)?;
    let mut record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token changed during daemon create");
    }
    let claims = read_registered_claims(&claim_path, builder)?
        .context("BuildKit daemon create has no registered runtime claim")?;
    if !claims.holders.contains_key(job_container) {
        anyhow::bail!("BuildKit daemon create came from a job without its builder claim");
    }

    let container_value = inspect_container(daemon_name)?
        .context("new BuildKit daemon disappeared before its identity was recorded")?;
    let daemon =
        parse_verified_builder_container(&container_value, builder, owner_token, daemon_name)?;
    if daemon.id != created_id {
        anyhow::bail!("BuildKit create response ID does not match the inspected daemon");
    }
    let volume_value = inspect_volume(&daemon.state_volume)?
        .context("new BuildKit state volume disappeared before its identity was recorded")?;
    let volume =
        parse_verified_builder_volume(&volume_value, builder, owner_token, &daemon.state_volume)?;
    if daemon.volume_mountpoint != volume.mountpoint
        || volume_users(&volume.name)?
            .iter()
            .any(|id| id != &daemon.id)
    {
        anyhow::bail!("new BuildKit daemon state volume failed ownership verification");
    }
    if let Some(previous_node) = record.daemon_nodes.get(daemon_name)
        && previous_node.container_id != created_id
    {
        let previous_id = &previous_node.container_id;
        if inspect_container(previous_id)?.is_some() {
            anyhow::bail!("previous registered BuildKit daemon ID still exists");
        }
    }
    if record
        .daemon_nodes
        .get(daemon_name)
        .is_some_and(|node| node.state_volume_mountpoint != volume.mountpoint)
    {
        anyhow::bail!("BuildKit state volume mountpoint changed from its durable identity");
    }
    record.daemon_nodes.insert(
        daemon_name.to_string(),
        BuilderNodeRecord {
            container_id: daemon.id,
            state_volume_mountpoint: volume.mountpoint,
            created_unix: unix_now(),
            bootstrap_phase: BuilderBootstrapPhase::Created,
            config_sha256: Some(config_sha256.to_string()),
        },
    );
    record.updated_unix = unix_now();
    write_atomic_document(
        &owner_registry_file(registry_root, builder),
        &serde_json::to_vec_pretty(&record).context("encode BuildKit daemon identity")?,
    )
}

/// Advance one admitted daemon's durable Buildx bootstrap proof. The exact
/// token, immutable daemon ID, previous phase, config digest, and live claim
/// are all revalidated while holding the per-builder claim lock.
pub(crate) fn transition_admitted_builder_bootstrap(
    builder: &str,
    owner_token: &str,
    container_id: &str,
    expected_phase: BuilderBootstrapPhase,
    next_phase: BuilderBootstrapPhase,
    expected_config_sha256: &str,
) -> Result<()> {
    if !is_bounded_builder_name(builder)
        || !valid_owner_token(owner_token)
        || !valid_container_id(container_id)
        || !valid_config_sha256(expected_config_sha256)
        || !valid_bootstrap_transition(expected_phase, next_phase)
    {
        anyhow::bail!("invalid admitted BuildKit bootstrap transition");
    }
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    let claim_path = claims_file(&run_root, builder);
    let _lock = lock_claims(builder, &claim_path)?;
    let mut record = read_owner_record(&registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token changed during daemon bootstrap");
    }
    let claims = read_registered_claims(&claim_path, builder)?
        .context("BuildKit bootstrap transition has no registered runtime claim")?;
    if claims.holders.len() != 1 || claims.releasing.is_some() {
        anyhow::bail!("BuildKit bootstrap transition requires one active non-releasing job claim");
    }

    let daemon_name = daemon_container_name(builder);
    let node = record
        .daemon_nodes
        .get_mut(&daemon_name)
        .context("BuildKit owner record has no identity for this daemon node")?;
    if node.container_id != container_id {
        anyhow::bail!("BuildKit daemon ID changed during bootstrap");
    }
    validate_node_bootstrap_state(node)?;
    if node.bootstrap_phase != expected_phase {
        anyhow::bail!(
            "BuildKit bootstrap phase is {:?}; expected {:?}",
            node.bootstrap_phase,
            expected_phase
        );
    }
    if node.config_sha256.as_deref() != Some(expected_config_sha256) {
        anyhow::bail!("BuildKit bootstrap config digest changed during transition");
    }
    node.bootstrap_phase = next_phase;
    write_atomic_document(
        &owner_registry_file(&registry_root, builder),
        &serde_json::to_vec_pretty(&record).context("encode BuildKit bootstrap phase")?,
    )
}

/// Durable registry inventory. The host Buildx registry is job-local state in
/// this runtime, so maintenance and admission use these records as the
/// authoritative names. Any malformed record fails the inventory closed.
fn read_owner_records(registry_root: &Path) -> Result<Vec<BuilderOwnerRecord>> {
    let entries = match std::fs::read_dir(registry_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("list {}", registry_root.display()));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read {} entry", registry_root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("stat owner record {}", path.display()))?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("owner record {} is not a regular file", path.display());
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let record: BuilderOwnerRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse owner record {}", path.display()))?;
        if record.version != OWNER_REGISTRY_VERSION {
            anyhow::bail!(
                "BuildKit owner record {} has schema version {}; operator migration is required before admission",
                path.display(),
                record.version
            );
        }
        if !is_owner_registry_builder_name(&record.builder)
            || owner_registry_file(registry_root, &record.builder) != path
        {
            anyhow::bail!("owner record {} does not match its builder", path.display());
        }
        if !owner_record_identity_is_valid(&record) {
            anyhow::bail!(
                "BuildKit owner record {} has incomplete or malformed v2 identity; operator migration is required",
                path.display()
            );
        }
        records.push(record);
    }
    Ok(records)
}

/// Claim a workflow-selected persistent name under the aggregate group cap.
/// The registry and group identity are explicit: arbitrary name suffixes are
/// never parsed to infer a scope, tier, or repository.
pub(crate) fn claim_builder_bounded(
    run_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
    required_containers: &[String],
    cleanup_on_release: bool,
) -> Result<()> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("setup-buildx requires a canonical bounded BuildKit builder name");
    }
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("setup-buildx requires configured Velnor storage"))?;
    claim_builder_bounded_with_policy(
        run_root,
        &registry_root,
        builder,
        slot,
        container,
        scope,
        tier,
        repository,
        required_containers,
        cleanup_on_release,
        running_container_names,
        remove_builder_for_capacity,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected removal and liveness operations keep admission hermetic in tests"
)]
fn claim_builder_bounded_with_ops(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
    list_present_containers: impl FnMut() -> Result<BTreeSet<String>>,
    remove: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    let required_containers = [container.to_string()];
    claim_builder_bounded_with_policy(
        run_root,
        registry_root,
        builder,
        slot,
        container,
        scope,
        tier,
        repository,
        &required_containers,
        true,
        list_present_containers,
        remove,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "cleanup policy and dependent containers are durable holder identity"
)]
fn claim_builder_bounded_with_policy(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
    required_containers: &[String],
    cleanup_on_release: bool,
    mut list_present_containers: impl FnMut() -> Result<BTreeSet<String>>,
    mut remove: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("setup-buildx requires a canonical bounded BuildKit builder name");
    }
    let group = BuilderGroup::new(scope, tier, repository);
    let admission_lock_path = run_root.join(BUILDER_ADMISSION_LOCK);
    let _admission =
        crate::cache::CacheEntryLock::exclusive_timeout(&admission_lock_path, CLAIM_LOCK_TIMEOUT)
            .context("lock BuildKit builder admission")?;

    let records = read_owner_records(registry_root).context("read BuildKit capacity registry")?;
    let existing = records.iter().find(|record| record.builder == builder);
    if existing.is_some_and(|record| {
        let canonical_node = daemon_container_name(builder);
        record
            .daemon_nodes
            .keys()
            .any(|daemon| daemon != &canonical_node)
    }) {
        anyhow::bail!(
            "BuildKit owner record for {builder} contains appended daemon nodes; operator migration is required before admission"
        );
    }
    if existing
        .and_then(|record| record.group.as_ref())
        .is_some_and(|stored| stored.is_canonical() && stored != &group)
    {
        anyhow::bail!(
            "BuildKit builder {builder} is already registered under another scope/tier/repository"
        );
    }

    // Persistent BuildKit daemons now join one job's private network. A
    // daemon cannot be shared by simultaneous jobs without crossing that
    // network boundary. Registered builders must prove their runtime claim
    // before any capacity eviction. A genuinely new identity gets its
    // one-time first-create exception only after its durable owner row exists.
    let present =
        list_present_containers().context("list active jobs before BuildKit builder admission")?;
    if existing.is_some() {
        ensure_builder_exclusive_claim(
            run_root,
            registry_root,
            builder,
            container,
            None,
            &present,
        )?;
    }

    let group_records = records
        .iter()
        .filter(|record| {
            record
                .group
                .as_ref()
                .is_some_and(|stored| stored.is_canonical() && stored == &group)
        })
        .collect::<Vec<_>>();
    let is_existing = existing.is_some();
    let mut excess = group_records
        .len()
        .saturating_add(usize::from(!is_existing))
        .saturating_sub(MAX_BUILDERS_PER_SCOPE_TIER_REPO);

    if excess > 0 {
        // The inventory contains only complete v2 identities with canonical
        // groups. Never parse scope/tier/repository from an old builder name.
        let mut candidates = records
            .iter()
            .filter(|record| {
                record.builder != builder
                    && record.group.as_ref().is_some_and(|stored| stored == &group)
            })
            .map(|record| (record.updated_unix, record.builder.clone()))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        for (_, candidate) in candidates {
            if excess == 0 {
                break;
            }
            let unclaimed = match capacity_candidate_unclaimed(
                run_root,
                registry_root,
                &candidate,
                &mut list_present_containers,
            ) {
                Ok(unclaimed) => unclaimed,
                Err(error) => {
                    tracing::warn!(target: "velnor.buildkit", builder = %candidate, error = %error, "skip capacity eviction: ownership proof failed");
                    continue;
                }
            };
            if !unclaimed {
                continue;
            }
            match remove_builder_and_claims_with(
                run_root,
                Some(registry_root),
                &candidate,
                &mut remove,
            ) {
                Ok(true) => {
                    excess -= 1;
                    tracing::debug!(target: "velnor.buildkit", builder = %candidate, "evicted least-recently-used unclaimed builder at aggregate capacity");
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(target: "velnor.buildkit", builder = %candidate, error = %error, "capacity eviction failed; retain builder ownership")
                }
            }
        }
    }

    if excess > 0 {
        anyhow::bail!(
            "builder cap exceeded for scope '{}' tier '{}' repo '{}': no safely removable unclaimed builder; cap is {}",
            group.scope,
            group.tier,
            group.repo,
            MAX_BUILDERS_PER_SCOPE_TIER_REPO
        );
    }

    let first_create_owner_token = if existing.is_none() {
        Some(
            ensure_owner_record(registry_root, builder, Some(&group))
                .context("register first BuildKit builder identity before admission")?,
        )
    } else {
        None
    };
    let admission = (|| -> Result<()> {
        // Capacity eviction may take time. Re-scan immediately before claim
        // repair so a newly started workload cannot be mistaken for an empty
        // runtime claim after the earlier snapshot.
        let present = list_present_containers()
            .context("recheck active jobs before BuildKit builder admission")?;
        ensure_builder_exclusive_claim(
            run_root,
            registry_root,
            builder,
            container,
            first_create_owner_token.as_deref(),
            &present,
        )?;
        claim_builder_with_registry_and_group(
            run_root,
            Some(registry_root),
            builder,
            slot,
            container,
            Some(&group),
            required_containers,
            cleanup_on_release,
        )
    })();
    match admission {
        Err(admission_error) => {
            if let Some(owner_token) = first_create_owner_token
                && let Err(rollback_error) =
                    remove_provisional_owner_record(run_root, registry_root, builder, &owner_token)
            {
                return Err(admission_error.context(format!(
                    "first BuildKit admission failed and its provisional owner row could not be removed: {rollback_error:#}"
                )));
            }
            Err(admission_error)
        }
        result => result,
    }
}

/// Remove a just-created owner row if first admission fails before it obtains
/// a holder or records a daemon. Keep any row whose identity or claim state
/// changed; it needs the ordinary exact-owner recovery path.
fn remove_provisional_owner_record(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    owner_token: &str,
) -> Result<()> {
    let claim_path = claims_file(run_root, builder);
    let _claim_lock = lock_claims(builder, &claim_path)?;
    let claims = read_claims(&claim_path).with_context(|| {
        format!("read claims before rolling back first admission for {builder}")
    })?;
    if (!claims.builder.is_empty() && claims.builder != builder)
        || !claims.holders.is_empty()
        || claims.releasing.is_some()
    {
        anyhow::bail!("first BuildKit claim changed before owner rollback for {builder}");
    }
    let Some(record) = read_owner_record(registry_root, builder)? else {
        return Ok(());
    };
    if record.owner_token.as_deref() != Some(owner_token) || !record.daemon_nodes.is_empty() {
        anyhow::bail!("first BuildKit owner identity changed before rollback for {builder}");
    }
    std::fs::remove_file(owner_registry_file(registry_root, builder))
        .with_context(|| format!("remove provisional BuildKit owner row for {builder}"))
}

fn ensure_builder_exclusive_claim(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    container: &str,
    first_create_owner_token: Option<&str>,
    present: &BTreeSet<String>,
) -> Result<()> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let claims = read_claims_if_present(&path)?;
    if let Some(claims) = claims.as_ref()
        && (claims.builder != builder || !is_persistent_builder_name(&claims.builder))
    {
        anyhow::bail!(
            "claim file {} has incomplete or invalid builder ownership identity for {builder}; refusing admission",
            path.display()
        );
    }
    if claims.is_none() && contains_active_job_or_service(present) {
        let first_create_is_durable = if let Some(owner_token) = first_create_owner_token {
            read_owner_record(registry_root, builder)?.is_some_and(|record| {
                record.owner_token.as_deref() == Some(owner_token) && record.daemon_nodes.is_empty()
            })
        } else {
            false
        };
        if !first_create_is_durable {
            anyhow::bail!(
                "persistent BuildKit builder {builder} has no /run claim while another job container is active; refusing network reassignment"
            );
        }
    }
    let mut claims = claims.unwrap_or_default();
    if !claims.builder.is_empty() && claims.builder != builder {
        anyhow::bail!(
            "claim file {} names builder {}, expected {builder}",
            path.display(),
            claims.builder
        );
    }
    if let Some(holder) = claims.releasing.as_deref() {
        anyhow::bail!(
            "persistent BuildKit builder {builder} has a teardown release reserved for job {holder}; retry admission after release completes"
        );
    }
    let previous_holders = claims.holders.len();
    claims
        .holders
        .retain(|held, holder| held == container || holder_has_live_container(holder, present));
    let repaired = claims.holders.len() != previous_holders;
    let conflicting_holder = claims
        .holders
        .keys()
        .find(|held| held.as_str() != container)
        .cloned();
    if repaired {
        write_claims(&path, &claims)
            .with_context(|| format!("repair stale BuildKit holders for {builder}"))?;
    }
    if let Some(holder) = conflicting_holder {
        anyhow::bail!(
            "persistent BuildKit builder {builder} is already claimed by active job {holder}; its daemon cannot join two private job networks"
        );
    }
    Ok(())
}

/// Capacity eviction only accepts a durable owner plus an empty runtime claim.
/// If `/run` state is missing, a fresh job-container scan must prove quiescence
/// before the durable owner record may stand in for that claim.
fn capacity_candidate_unclaimed(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    list_present_containers: &mut impl FnMut() -> Result<BTreeSet<String>>,
) -> Result<bool> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    match claim_file_missing(&path)? {
        false => Ok(matches!(
            read_registered_claims(&path, builder)?,
            Some(claims) if claims.holders.is_empty() && claims.releasing.is_none()
        )),
        true => {
            let present = list_present_containers()?;
            if contains_active_job_or_service(&present) {
                return Ok(false);
            }
            let Some(claims) = read_claims_for_reaping(&path, builder, Some(registry_root), true)?
            else {
                return Ok(false);
            };
            if !claims.holders.is_empty() || claims.releasing.is_some() {
                return Ok(false);
            }
            write_claims(&path, &claims)?;
            Ok(true)
        }
    }
}

fn remove_owner_record(
    registry_root: &Path,
    builder: &str,
    expected_owner_token: &str,
) -> Result<bool> {
    if !is_owner_registry_builder_name(builder) {
        return Ok(false);
    }
    let Some(record) = read_owner_record(registry_root, builder)? else {
        return Ok(false);
    };
    if record.owner_token.as_deref() != Some(expected_owner_token) {
        return Ok(false);
    }
    let path = owner_registry_file(registry_root, builder);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "BuildKit owner record {} is not a regular file",
            path.display()
        );
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            sync_parent(&path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

/// Read a claim file through a no-follow descriptor. Missing is represented
/// distinctly so callers can apply their own quiescence policy; symlinks and
/// non-regular objects always fail closed.
fn read_claims_if_present(path: &Path) -> Result<Option<BuilderClaims>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    if !file
        .metadata()
        .with_context(|| format!("stat opened claim {}", path.display()))?
        .file_type()
        .is_file()
    {
        anyhow::bail!("claim file {} is not a regular file", path.display());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    parse_claims(path, &bytes).map(Some)
}

/// Read one claim file. Missing reads as empty; torn JSON fails closed as
/// an error the caller must treat as *claimed*: with atomic rename writes a
/// torn file means disk corruption or a pre-atomic crash, and the safe
/// direction is to stop, prune, and delete nothing.
fn read_claims(path: &Path) -> Result<BuilderClaims> {
    Ok(read_claims_if_present(path)?.unwrap_or_default())
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
        let unique_required = holder.required_containers.iter().collect::<BTreeSet<_>>();
        if holder.required_containers.is_empty()
            || !unique_required.contains(&holder.container)
            || unique_required.len() != holder.required_containers.len()
            || holder.required_containers.iter().any(|name| {
                name.is_empty()
                    || name.chars().any(char::is_whitespace)
                    || name.contains('/')
                    || name.contains('\\')
            })
        {
            anyhow::bail!(
                "claim file {} has invalid required-container identity for {container}",
                path.display()
            );
        }
    }
    if claims
        .releasing
        .as_deref()
        .is_some_and(|container| container.is_empty() || !claims.holders.contains_key(container))
    {
        anyhow::bail!(
            "claim file {} has an incomplete network-release reservation",
            path.display()
        );
    }
    Ok(claims)
}

/// Read a Velnor ownership record without treating a missing or mismatched
/// file as an empty claim. A reserved-looking name alone is not proof that an
/// external Buildx builder belongs to this daemon.
fn read_registered_claims(path: &Path, builder: &str) -> Result<Option<BuilderClaims>> {
    let Some(claims) = read_claims_if_present(path)? else {
        return Ok(None);
    };
    if claims.builder == builder && is_persistent_builder_name(&claims.builder) {
        Ok(Some(claims))
    } else {
        Ok(None)
    }
}

/// Read ownership for a maintenance pass. Current builders require the
/// canonical name and current owner record. The v1 cleanup branch separately
/// requires its exact old owner row; a name or runtime claim alone is never
/// ownership proof.
fn read_claims_for_reaping(
    path: &Path,
    builder: &str,
    registry_root: Option<&Path>,
    allow_missing_after_quiescence: bool,
) -> Result<Option<BuilderClaims>> {
    if is_legacy_v1_builder_name(builder) {
        let Some(registry_root) = registry_root else {
            return Ok(None);
        };
        if read_legacy_v1_owner_record(registry_root, builder)?.is_none() {
            return Ok(None);
        }
        if let Some(claims) = read_claims_if_present(path)? {
            return Ok((claims.builder == builder).then_some(claims));
        }
        if allow_missing_after_quiescence {
            return Ok(Some(BuilderClaims {
                builder: builder.to_string(),
                ..BuilderClaims::default()
            }));
        }
        return Ok(None);
    }
    if !is_owner_registry_builder_name(builder) {
        return Ok(None);
    }
    let Some(registry_root) = registry_root else {
        return Ok(None);
    };
    if let Some(claims) = read_registered_claims(path, builder)? {
        // An existing malformed or mismatched durable record is a hard stop.
        // A Buildx name or /run claim alone is not enough to recover
        // daemon-local Buildx state after a reboot.
        if read_owner_record(registry_root, builder)?.is_none() {
            return Ok(None);
        }
        return Ok(Some(claims));
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    }
    if allow_missing_after_quiescence && read_owner_record(registry_root, builder)?.is_some() {
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
    claims.holders.retain(|held, holder| {
        Some(held.as_str()) == claims.releasing.as_deref()
            || held == container
            || holder.slot != slot
    });
}

/// Drop holds whose job container is not in `present`.
fn repair_absent_unlocked(claims: &mut BuilderClaims, present: &BTreeSet<String>) {
    claims.holders.retain(|held, holder| {
        Some(held.as_str()) == claims.releasing.as_deref()
            || holder_has_live_container(holder, present)
    });
}

fn holder_has_live_container(holder: &BuilderHolder, present: &BTreeSet<String>) -> bool {
    holder
        .required_containers
        .iter()
        .any(|required| present.contains(required))
}

/// Claim `builder` for the calling job. Idempotent: claiming twice (two
/// setup-buildx steps, one name) holds once. Current-generation owner records
/// are committed before setup can create or reuse the Buildx object.
#[cfg(test)]
pub(crate) fn claim_builder(
    run_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    let registry_root = claims_registry_root();
    claim_builder_with_registry(run_root, registry_root.as_deref(), builder, slot, container)
}

#[cfg(test)]
fn claim_builder_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    let group = registry_root.map(|_| BuilderGroup::new("trusted", TRUST_TIER_BRANCH, Some("o/r")));
    let required_containers = [container.to_string()];
    claim_builder_with_registry_and_group(
        run_root,
        registry_root,
        builder,
        slot,
        container,
        group.as_ref(),
        &required_containers,
        true,
    )
}

fn claim_builder_with_registry_and_group(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    slot: &str,
    container: &str,
    group: Option<&BuilderGroup>,
    required_containers: &[String],
    cleanup_on_release: bool,
) -> Result<()> {
    if required_containers.is_empty() || !required_containers.iter().any(|name| name == container) {
        anyhow::bail!("BuildKit holder must require its job container to remain live");
    }
    let mut required_containers = required_containers.to_vec();
    required_containers.sort();
    required_containers.dedup();
    if required_containers.iter().any(|name| {
        name.is_empty()
            || name.chars().any(char::is_whitespace)
            || name.contains('/')
            || name.contains('\\')
    }) {
        anyhow::bail!("BuildKit holder has an invalid required container name");
    }
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
    if let Some(holder) = claims.releasing.as_deref() {
        anyhow::bail!(
            "persistent BuildKit builder {builder} has a teardown release reserved for job {holder}; retry claim after release completes"
        );
    }
    if let Some(registry_root) = registry_root {
        ensure_owner_record(registry_root, builder, group)?;
    }
    repair_slot_unlocked(&mut claims, slot, container);
    claims.holders.insert(
        container.to_string(),
        BuilderHolder {
            container: container.to_string(),
            required_containers,
            slot: slot.to_string(),
            claimed_unix: unix_now(),
            cleanup_on_release,
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
}

/// Reserve this holder's final teardown before its job or service containers
/// are removed. Maintenance may observe those containers disappear and repair
/// absent holders concurrently; the durable marker keeps it from consuming
/// this holder before teardown detaches the daemon and completes release.
pub(crate) fn reserve_admitted_builder_release(
    run_root: &Path,
    builder: &str,
    container: &str,
) -> Result<bool> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    reserve_admitted_builder_release_with(run_root, &registry_root, builder, container)
}

fn reserve_admitted_builder_release_with(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    container: &str,
) -> Result<bool> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("refuse teardown reservation for noncanonical BuildKit builder");
    }
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let owner_record = read_owner_record(registry_root, builder)?.with_context(|| {
        format!("BuildKit owner record for {builder} is absent before teardown")
    })?;
    if owner_record
        .owner_token
        .as_deref()
        .is_none_or(|token| !valid_owner_token(token))
    {
        anyhow::bail!("BuildKit owner record for {builder} has no valid teardown identity");
    }
    let Some(mut claims) = read_claims_if_present(&path)? else {
        anyhow::bail!("registered BuildKit claim for {builder} is absent before teardown");
    };
    if claims.builder != builder {
        anyhow::bail!(
            "claim file {} names builder {}, expected {builder}",
            path.display(),
            claims.builder
        );
    }
    if !claims.holders.contains_key(container) {
        return Ok(false);
    }
    if claims.holders.len() != 1 {
        anyhow::bail!("teardown reservation requires one exclusive holder for builder {builder}");
    }
    if claims
        .releasing
        .as_deref()
        .is_some_and(|holder| holder != container)
    {
        anyhow::bail!(
            "builder {builder} already has a teardown reservation for job {}",
            claims.releasing.as_deref().unwrap_or_default()
        );
    }
    if claims.releasing.as_deref() != Some(container) {
        claims.releasing = Some(container.to_string());
        write_claims(&path, &claims)
            .with_context(|| format!("reserve BuildKit teardown for {builder}/{container}"))?;
    }
    Ok(true)
}

/// Detach this job's private network before releasing its exclusive builder
/// claim. Keep the holder and per-builder reservation durable through daemon
/// stop so a failed stop can be retried and a new job cannot join a daemon
/// whose cleanup preference has not completed. Teardown may reserve the
/// holder before removing job/service containers, preventing maintenance from
/// repairing it away in that interval. The callbacks run without the claim
/// lock. Any detach or stop failure retains both holder and reservation for
/// retry.
pub(crate) fn release_after_network_detach_if_last(
    run_root: &Path,
    builder: &str,
    container: &str,
    before_release: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    let path = claims_file(run_root, builder);
    let cleanup_on_release = {
        let _lock = lock_claims(builder, &path)?;
        let mut claims = read_claims(&path)
            .with_context(|| format!("read BuildKit claim before network release for {builder}"))?;
        if !claims.builder.is_empty() && claims.builder != builder {
            anyhow::bail!(
                "claim file {} names builder {}, expected {builder}",
                path.display(),
                claims.builder
            );
        }
        let Some(holder) = claims.holders.get(container) else {
            return Ok(ReleaseOutcome {
                removed_last: false,
                stopped: false,
            });
        };
        let cleanup_on_release = holder.cleanup_on_release;
        if claims.holders.len() != 1 {
            anyhow::bail!(
                "network-aware release requires one exclusive holder for builder {builder}"
            );
        }
        if claims
            .releasing
            .as_deref()
            .is_some_and(|holder| holder != container)
        {
            anyhow::bail!(
                "builder {builder} is already detaching for job {}",
                claims.releasing.as_deref().unwrap_or_default()
            );
        }
        // Same-holder retries resume the reservation after a prior detach or
        // final claim write failed. The operation remains idempotent.
        claims.releasing = Some(container.to_string());
        if claims.builder.is_empty() {
            claims.builder = builder.to_string();
        }
        write_claims(&path, &claims)?;
        cleanup_on_release
    };

    if let Err(detach_error) = before_release() {
        return Err(detach_error).context(format!(
            "detach private network before releasing BuildKit builder {builder}"
        ));
    }

    // The durable reservation blocks claims while stop runs unlocked. Keep the
    // filesystem coordinator shared through stop so global reaping cannot
    // race this exact owner operation.
    let _lifecycle = crate::capacity::FilesystemCoordinator::lock_shared(run_root)
        .context("lock BuildKit lifecycle after network detach")?;
    {
        let _lock = lock_claims(builder, &path)?;
        let claims = read_claims(&path)
            .with_context(|| format!("revalidate BuildKit release for {builder}"))?;
        if claims.releasing.as_deref() != Some(container)
            || claims.holders.len() != 1
            || claims
                .holders
                .get(container)
                .is_none_or(|holder| holder.cleanup_on_release != cleanup_on_release)
        {
            anyhow::bail!(
                "BuildKit holder or network-release reservation changed before release for {builder}/{container}"
            );
        }
    }

    let stop_started = Instant::now();
    let stopped = if cleanup_on_release {
        stop().with_context(|| {
            format!("stop BuildKit daemon for {builder} before releasing its durable claim")
        })?
    } else {
        false
    };
    tracing::debug!(
        target: "velnor.buildkit",
        builder,
        stop_ms = stop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        stopped,
        cleanup_on_release,
        "network-aware release stop ran outside the claim lock"
    );
    {
        let _lock = lock_claims(builder, &path)?;
        let mut claims = read_claims(&path)
            .with_context(|| format!("re-read BuildKit claim after stop for {builder}"))?;
        if claims.releasing.as_deref() != Some(container)
            || claims.holders.len() != 1
            || claims
                .holders
                .get(container)
                .is_none_or(|holder| holder.cleanup_on_release != cleanup_on_release)
        {
            anyhow::bail!(
                "BuildKit holder or network-release reservation changed during stop for {builder}/{container}"
            );
        }
        claims.holders.remove(container);
        claims.releasing = None;
        write_claims(&path, &claims)
            .with_context(|| format!("persist completed BuildKit release for {builder}"))?;
    }
    Ok(ReleaseOutcome {
        removed_last: true,
        stopped,
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

/// Recover claims from the durable owner registry when a job-local builder
/// journal was lost or could not be written. The journal is a hint; the
/// locked `/run` holder plus matching v2 owner record is the release source.
pub(crate) fn builders_claimed_by_job(run_root: &Path, job_holder: &str) -> Result<Vec<String>> {
    let Some(registry_root) = claims_registry_root() else {
        return Ok(Vec::new());
    };
    let records = read_owner_records(&registry_root)
        .context("read BuildKit owner registry while recovering job claims")?;
    let mut builders = Vec::new();
    for record in records {
        let path = claims_file(run_root, &record.builder);
        let _lock = lock_claims(&record.builder, &path)?;
        let Some(claims) = read_registered_claims(&path, &record.builder)? else {
            continue;
        };
        if claims.holders.contains_key(job_holder) {
            builders.push(record.builder);
        }
    }
    builders.sort();
    builders.dedup();
    Ok(builders)
}

/// The daemon run root holding claim files, if this process has storage
/// configured. Maintenance and job paths degrade to no-claim operation
/// without one (builders still persist by name; nothing stops them).
pub(crate) fn claims_run_root() -> Option<PathBuf> {
    resolved_storage_layout().map(|layout| layout.run_root)
}

// ---------------------------------------------------------------------------
// Daemon operations (host engine, every call deadline-bounded)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedBuilderDaemon {
    id: String,
    name: String,
    state_volume: String,
    volume_mountpoint: String,
    state_running: bool,
    state: crate::docker::client::ContainerState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LegacyV1OwnerLabels {
    job_id: String,
    daemon_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedLegacyV1BuilderDaemon {
    daemon: VerifiedBuilderDaemon,
    labels: LegacyV1OwnerLabels,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedBuilderVolume {
    name: String,
    mountpoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BuilderNetworkAttachment {
    name: String,
    id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InspectedBuilderNetwork {
    name: String,
    id: String,
    labels: BTreeMap<String, String>,
    container_ids: BTreeSet<String>,
}

fn inspect_json(args: &[String]) -> Result<Option<serde_json::Value>> {
    match crate::docker::client::host_call(args) {
        Ok(output) => serde_json::from_str(&output)
            .map(Some)
            .with_context(|| format!("parse Docker inspect output for {args:?}")),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect Docker object {args:?}")),
    }
}

fn inspect_builder_container(daemon: &str) -> Result<Option<serde_json::Value>> {
    let args = vec![
        "inspect".to_string(),
        "--type".to_string(),
        "container".to_string(),
        "--format".to_string(),
        "{{json .}}".to_string(),
        daemon.to_string(),
    ];
    inspect_json(&args)
}

fn inspect_builder_volume(volume: &str) -> Result<Option<serde_json::Value>> {
    let args = vec![
        "volume".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{json .}}".to_string(),
        volume.to_string(),
    ];
    inspect_json(&args)
}

fn inspect_builder_network(network: &str) -> Result<Option<InspectedBuilderNetwork>> {
    let args = vec![
        "network".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{json .}}".to_string(),
        network.to_string(),
    ];
    inspect_json(&args)?
        .map(|value| parse_inspected_builder_network(&value))
        .transpose()
}

fn parse_inspected_builder_network(value: &serde_json::Value) -> Result<InspectedBuilderNetwork> {
    let id = value
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| valid_container_id(id))
        .context("Docker network inspect omitted a valid immutable ID")?;
    let name = value
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .context("Docker network inspect omitted Name")?;
    let labels = match value.get("Labels") {
        None | Some(serde_json::Value::Null) => BTreeMap::new(),
        Some(serde_json::Value::Object(labels)) => {
            let mut parsed = BTreeMap::new();
            for (key, value) in labels {
                let value = value
                    .as_str()
                    .with_context(|| format!("Docker network label {key:?} is not a string"))?;
                parsed.insert(key.clone(), value.to_string());
            }
            parsed
        }
        Some(_) => anyhow::bail!("Docker network inspect returned malformed Labels"),
    };
    let container_ids = match value.get("Containers") {
        None | Some(serde_json::Value::Null) => BTreeSet::new(),
        Some(serde_json::Value::Object(containers)) => {
            let mut ids = BTreeSet::new();
            for id in containers.keys() {
                if !valid_container_id(id) {
                    anyhow::bail!("Docker network inspect returned an invalid member ID");
                }
                ids.insert(id.clone());
            }
            ids
        }
        Some(_) => anyhow::bail!("Docker network inspect returned malformed Containers"),
    };
    Ok(InspectedBuilderNetwork {
        name: name.to_string(),
        id: id.to_string(),
        labels,
        container_ids,
    })
}

fn parse_builder_network_attachments(
    value: &serde_json::Value,
    builder: &str,
    expected_container_id: &str,
) -> Result<Vec<BuilderNetworkAttachment>> {
    if value.get("Id").and_then(serde_json::Value::as_str) != Some(expected_container_id)
        || value.get("Name").and_then(serde_json::Value::as_str)
            != Some(format!("/{}", daemon_container_name(builder)).as_str())
    {
        anyhow::bail!("BuildKit network inspect target does not match the admitted daemon ID/name");
    }
    let networks = value
        .pointer("/NetworkSettings/Networks")
        .and_then(serde_json::Value::as_object)
        .context("BuildKit daemon inspect omitted NetworkSettings.Networks")?;
    let mut attachments = Vec::with_capacity(networks.len());
    for (name, network) in networks {
        let id = network
            .get("NetworkID")
            .and_then(serde_json::Value::as_str)
            .filter(|id| valid_container_id(id))
            .with_context(|| {
                format!("BuildKit network attachment {name:?} omitted immutable ID")
            })?;
        attachments.push(BuilderNetworkAttachment {
            name: name.clone(),
            id: id.to_string(),
        });
    }
    Ok(attachments)
}

fn inspect_admitted_builder_network_topology(
    builder: &str,
    expected_container_id: &str,
    network_name: &str,
    job_id: &str,
    daemon_id: &str,
    inspect_container: &mut impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    inspect_network: &mut impl FnMut(&str) -> Result<Option<InspectedBuilderNetwork>>,
) -> Result<(
    InspectedBuilderNetwork,
    Vec<BuilderNetworkAttachment>,
    Vec<(BuilderNetworkAttachment, InspectedBuilderNetwork)>,
)> {
    if !network_name.starts_with("velnor-net-") || job_id.is_empty() || daemon_id.is_empty() {
        anyhow::bail!("private BuildKit network identity is incomplete or noncanonical");
    }
    let target = inspect_network(network_name)?
        .with_context(|| format!("private job network {network_name} is absent"))?;
    if target.name != network_name
        || target
            .labels
            .get(crate::docker_lease::JOB_ID_LABEL)
            .map(String::as_str)
            != Some(job_id)
        || target
            .labels
            .get(crate::docker_lease::DAEMON_ID_LABEL)
            .map(String::as_str)
            != Some(daemon_id)
    {
        anyhow::bail!("private job network name or owner labels do not match this lease");
    }

    let container = inspect_container(expected_container_id)?
        .context("admitted BuildKit daemon disappeared during network inspection")?;
    let attachments =
        parse_builder_network_attachments(&container, builder, expected_container_id)?;
    let target_attached = attachments
        .iter()
        .find(|attachment| attachment.name == network_name);
    match target_attached {
        Some(attachment) if attachment.id == target.id => {
            if !target.container_ids.contains(expected_container_id) {
                anyhow::bail!("daemon inspect and target network membership disagree");
            }
        }
        Some(_) => anyhow::bail!("private job network ID changed during reconciliation"),
        None if target.container_ids.contains(expected_container_id) => {
            anyhow::bail!("target network lists the daemon but daemon inspect omits it");
        }
        None => {}
    }

    let mut old = Vec::new();
    for attachment in &attachments {
        let network = inspect_network(&attachment.id)?
            .with_context(|| format!("attached Docker network {} disappeared", attachment.name))?;
        if network.id != attachment.id || network.name != attachment.name {
            anyhow::bail!("daemon network attachment name or immutable ID changed");
        }
        if !network.container_ids.contains(expected_container_id) {
            anyhow::bail!("daemon inspect and attached network membership disagree");
        }
        if network.id == target.id {
            if network.name != network_name || attachment.name != network_name {
                anyhow::bail!("target network is attached under an unexpected name");
            }
            continue;
        }
        if network.name == "bridge" {
            // The Engine's exact default bridge is the only unlabelled
            // legacy network that may be detached from an admitted daemon.
            old.push((attachment.clone(), network));
            continue;
        }
        if network.name.starts_with("velnor-net-")
            && network
                .labels
                .get(crate::docker_lease::DAEMON_ID_LABEL)
                .map(String::as_str)
                == Some(daemon_id)
            && network
                .labels
                .get(crate::docker_lease::JOB_ID_LABEL)
                .is_some_and(|owner_job| !owner_job.is_empty())
            && network.container_ids.len() == 1
            && network.container_ids.contains(expected_container_id)
        {
            old.push((attachment.clone(), network));
            continue;
        }
        anyhow::bail!(
            "refuse to detach BuildKit daemon from unowned or shared network {}",
            network.name
        );
    }
    Ok((target, attachments, old))
}

fn verify_admitted_network_daemon(
    builder: &str,
    owner_token: &str,
    expected_container_id: &str,
    verify_identity: &mut impl FnMut(&str, &str, &str) -> Result<String>,
) -> Result<()> {
    let current = verify_identity(builder, owner_token, expected_container_id)?;
    if current != expected_container_id {
        anyhow::bail!("admitted BuildKit daemon immutable ID changed during network operation");
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "all Docker effects and inspect results are injected for hermetic proof"
)]
fn reconcile_admitted_builder_network_with(
    builder: &str,
    owner_token: &str,
    expected_container_id: &str,
    network_name: &str,
    job_id: &str,
    daemon_id: &str,
    mut verify_identity: impl FnMut(&str, &str, &str) -> Result<String>,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_network: impl FnMut(&str) -> Result<Option<InspectedBuilderNetwork>>,
    mut disconnect: impl FnMut(&str, &str) -> Result<()>,
    mut connect: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    if !is_bounded_builder_name(builder)
        || !valid_owner_token(owner_token)
        || !valid_container_id(expected_container_id)
        || network_name.trim() != network_name
        || !network_name.starts_with("velnor-net-")
        || job_id.is_empty()
        || daemon_id.is_empty()
    {
        anyhow::bail!("refuse network reconciliation for incomplete BuildKit lease identity");
    }
    verify_admitted_network_daemon(
        builder,
        owner_token,
        expected_container_id,
        &mut verify_identity,
    )?;
    let (target, _, old) = inspect_admitted_builder_network_topology(
        builder,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        &mut inspect_container,
        &mut inspect_network,
    )?;
    let target_id = target.id.clone();

    for (attachment, original_network) in old {
        verify_admitted_network_daemon(
            builder,
            owner_token,
            expected_container_id,
            &mut verify_identity,
        )?;
        let (current_target, attachments, current_old) = inspect_admitted_builder_network_topology(
            builder,
            expected_container_id,
            network_name,
            job_id,
            daemon_id,
            &mut inspect_container,
            &mut inspect_network,
        )?;
        if current_target.id != target_id {
            anyhow::bail!("private job network immutable ID changed before disconnect");
        }
        if !current_old.iter().any(|(current, network)| {
            current.id == attachment.id
                && current.name == attachment.name
                && network.id == original_network.id
        }) {
            // Another authorized actor already detached this endpoint. Its
            // current topology was still fully revalidated above.
            continue;
        }
        if !attachments.iter().any(|current| current == &attachment) {
            anyhow::bail!("daemon network attachment changed before disconnect");
        }
        disconnect(&attachment.id, expected_container_id).with_context(|| {
            format!(
                "disconnect admitted BuildKit daemon from {}",
                attachment.name
            )
        })?;
        verify_admitted_network_daemon(
            builder,
            owner_token,
            expected_container_id,
            &mut verify_identity,
        )?;
        let (after_target, after_attachments, after_old) =
            inspect_admitted_builder_network_topology(
                builder,
                expected_container_id,
                network_name,
                job_id,
                daemon_id,
                &mut inspect_container,
                &mut inspect_network,
            )?;
        if after_target.id != target_id
            || after_attachments.contains(&attachment)
            || after_old
                .iter()
                .any(|(current, _)| current.id == attachment.id)
        {
            anyhow::bail!("Docker network disconnect did not remove the exact daemon endpoint");
        }
        let detached_network = inspect_network(&attachment.id)?
            .context("disconnected BuildKit network disappeared before verification")?;
        if detached_network.id != original_network.id
            || detached_network.name != original_network.name
            || detached_network
                .container_ids
                .contains(expected_container_id)
        {
            anyhow::bail!("disconnected Docker network still contains the BuildKit daemon");
        }
        let verified_target = inspect_network(&target_id)?
            .context("private job network disappeared after disconnect")?;
        if verified_target.id != target_id || verified_target.name != network_name {
            anyhow::bail!("private job network identity changed after disconnect");
        }
    }

    verify_admitted_network_daemon(
        builder,
        owner_token,
        expected_container_id,
        &mut verify_identity,
    )?;
    let (current_target, attachments, old) = inspect_admitted_builder_network_topology(
        builder,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        &mut inspect_container,
        &mut inspect_network,
    )?;
    if current_target.id != target_id || !old.is_empty() {
        anyhow::bail!("BuildKit daemon network topology changed before target attach");
    }
    if !attachments
        .iter()
        .any(|attachment| attachment.name == network_name && attachment.id == target_id)
    {
        if attachments
            .iter()
            .any(|attachment| attachment.name == network_name)
        {
            anyhow::bail!("BuildKit daemon has a different network ID under the target name");
        }
        verify_admitted_network_daemon(
            builder,
            owner_token,
            expected_container_id,
            &mut verify_identity,
        )?;
        let before_connect = inspect_admitted_builder_network_topology(
            builder,
            expected_container_id,
            network_name,
            job_id,
            daemon_id,
            &mut inspect_container,
            &mut inspect_network,
        )?;
        if before_connect.0.id != target_id
            || !before_connect.1.is_empty()
            || !before_connect.2.is_empty()
        {
            anyhow::bail!("BuildKit network topology changed before target attach");
        }
        connect(&target_id, expected_container_id)
            .context("connect admitted BuildKit daemon to the private job network")?;
        verify_admitted_network_daemon(
            builder,
            owner_token,
            expected_container_id,
            &mut verify_identity,
        )?;
    }

    let (final_target, final_attachments, final_old) = inspect_admitted_builder_network_topology(
        builder,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        &mut inspect_container,
        &mut inspect_network,
    )?;
    if final_target.id != target_id
        || final_attachments.len() != 1
        || final_attachments[0].name != network_name
        || final_attachments[0].id != target_id
        || !final_old.is_empty()
    {
        anyhow::bail!("BuildKit daemon did not end on only the requested private network");
    }
    let final_network = inspect_network(&target_id)?
        .context("private job network disappeared after reconciliation")?;
    if final_network.id != target_id
        || final_network.name != network_name
        || final_network
            .labels
            .get(crate::docker_lease::JOB_ID_LABEL)
            .map(String::as_str)
            != Some(job_id)
        || final_network
            .labels
            .get(crate::docker_lease::DAEMON_ID_LABEL)
            .map(String::as_str)
            != Some(daemon_id)
        || !final_network.container_ids.contains(expected_container_id)
    {
        anyhow::bail!("final private network inspection does not prove this lease endpoint");
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "all Docker effects and inspect results are injected for hermetic proof"
)]
fn detach_admitted_builder_network_with(
    builder: &str,
    owner_token: &str,
    expected_container_id: &str,
    network_name: &str,
    job_id: &str,
    daemon_id: &str,
    mut verify_identity: impl FnMut(&str, &str, &str) -> Result<String>,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_network: impl FnMut(&str) -> Result<Option<InspectedBuilderNetwork>>,
    mut disconnect: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    if !is_bounded_builder_name(builder)
        || !valid_owner_token(owner_token)
        || !valid_container_id(expected_container_id)
        || network_name.trim() != network_name
        || !network_name.starts_with("velnor-net-")
        || job_id.is_empty()
        || daemon_id.is_empty()
    {
        anyhow::bail!("refuse network detach for incomplete BuildKit lease identity");
    }
    verify_admitted_network_daemon(
        builder,
        owner_token,
        expected_container_id,
        &mut verify_identity,
    )?;
    let (target, attachments, old) = inspect_admitted_builder_network_topology(
        builder,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        &mut inspect_container,
        &mut inspect_network,
    )?;
    if !old.is_empty()
        || attachments
            .iter()
            .any(|attachment| attachment.name != network_name || attachment.id != target.id)
    {
        anyhow::bail!("refuse private network detach while daemon has another network endpoint");
    }
    let Some(attachment) = attachments.first() else {
        // Idempotent release retry: the network inspect above proved that it
        // no longer contains this daemon.
        return Ok(());
    };

    verify_admitted_network_daemon(
        builder,
        owner_token,
        expected_container_id,
        &mut verify_identity,
    )?;
    let (before_target, before_attachments, before_old) =
        inspect_admitted_builder_network_topology(
            builder,
            expected_container_id,
            network_name,
            job_id,
            daemon_id,
            &mut inspect_container,
            &mut inspect_network,
        )?;
    if before_target.id != target.id
        || before_attachments.as_slice() != std::slice::from_ref(attachment)
        || !before_old.is_empty()
    {
        anyhow::bail!("private network topology changed before release detach");
    }
    disconnect(&target.id, expected_container_id)
        .context("detach admitted BuildKit daemon from its private job network")?;
    verify_admitted_network_daemon(
        builder,
        owner_token,
        expected_container_id,
        &mut verify_identity,
    )?;
    let (after_target, after_attachments, after_old) = inspect_admitted_builder_network_topology(
        builder,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        &mut inspect_container,
        &mut inspect_network,
    )?;
    if after_target.id != target.id || !after_attachments.is_empty() || !after_old.is_empty() {
        anyhow::bail!("BuildKit daemon remains attached after private network release");
    }
    let final_network = inspect_network(&target.id)?
        .context("private job network disappeared after daemon detach")?;
    if final_network.id != target.id
        || final_network.name != network_name
        || final_network
            .labels
            .get(crate::docker_lease::JOB_ID_LABEL)
            .map(String::as_str)
            != Some(job_id)
        || final_network
            .labels
            .get(crate::docker_lease::DAEMON_ID_LABEL)
            .map(String::as_str)
            != Some(daemon_id)
        || final_network.container_ids.contains(expected_container_id)
    {
        anyhow::bail!("private network inspect did not prove daemon detach");
    }
    Ok(())
}

fn parse_legacy_v1_owner_labels(
    labels: &serde_json::Map<String, serde_json::Value>,
    object: &str,
) -> Result<LegacyV1OwnerLabels> {
    if labels.keys().any(|key| {
        key.eq_ignore_ascii_case(crate::docker_lease::BUILDKIT_BUILDER_LABEL)
            || key.eq_ignore_ascii_case(crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL)
    }) {
        anyhow::bail!("legacy BuildKit {object} carries current-generation owner labels");
    }
    let job_id = labels
        .get(crate::docker_lease::JOB_ID_LABEL)
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            value.starts_with(crate::docker_lease::JOB_CONTAINER_NAME_PREFIX)
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        })
        .context("legacy BuildKit object has no valid Velnor job ownership label")?;
    let daemon_id = labels
        .get(crate::docker_lease::DAEMON_ID_LABEL)
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        })
        .context("legacy BuildKit object has no valid Velnor daemon ownership label")?;
    Ok(LegacyV1OwnerLabels {
        job_id: job_id.to_string(),
        daemon_id: daemon_id.to_string(),
    })
}

fn parse_legacy_v1_builder_container(
    value: &serde_json::Value,
    builder: &str,
    expected_name: &str,
) -> Result<VerifiedLegacyV1BuilderDaemon> {
    if !is_legacy_v1_builder_name(builder) || expected_name != daemon_container_name(builder) {
        anyhow::bail!("refuse identity verification for noncanonical v1 BuildKit daemon");
    }
    let id = value
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .context("legacy BuildKit daemon inspect omitted Id")?;
    if !valid_container_id(id) {
        anyhow::bail!("legacy BuildKit daemon inspect returned an invalid immutable ID");
    }
    let name = value
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .context("legacy BuildKit daemon inspect omitted Name")?;
    if name != format!("/{expected_name}") {
        anyhow::bail!("legacy BuildKit daemon name does not match its v1 owner row");
    }
    let labels = value
        .pointer("/Config/Labels")
        .and_then(serde_json::Value::as_object)
        .context("legacy BuildKit daemon inspect omitted Config.Labels")?;
    let labels = parse_legacy_v1_owner_labels(labels, "daemon")?;
    let mounts = value
        .get("Mounts")
        .and_then(serde_json::Value::as_array)
        .context("legacy BuildKit daemon inspect omitted Mounts")?;
    if mounts.len() != 1 {
        anyhow::bail!("legacy BuildKit daemon must mount exactly one state volume");
    }
    let mount = &mounts[0];
    let state_volume = daemon_state_volume(builder);
    if mount.get("Type").and_then(serde_json::Value::as_str) != Some("volume")
        || mount.get("Destination").and_then(serde_json::Value::as_str) != Some("/var/lib/buildkit")
        || mount.get("Name").and_then(serde_json::Value::as_str) != Some(&state_volume)
    {
        anyhow::bail!("legacy BuildKit daemon state mount does not match its v1 owner row");
    }
    let source = mount
        .get("Source")
        .and_then(serde_json::Value::as_str)
        .filter(|source| !source.is_empty())
        .context("legacy BuildKit daemon state mount omitted Source")?;
    let running = value
        .pointer("/State/Running")
        .and_then(serde_json::Value::as_bool)
        .context("legacy BuildKit daemon inspect omitted State.Running")?;
    let state = value
        .pointer("/State/Status")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::docker::client::ContainerState::parse)
        .context("legacy BuildKit daemon inspect returned an unknown State.Status")?;
    Ok(VerifiedLegacyV1BuilderDaemon {
        daemon: VerifiedBuilderDaemon {
            id: id.to_string(),
            name: expected_name.to_string(),
            state_volume,
            volume_mountpoint: source.to_string(),
            state_running: running,
            state,
        },
        labels,
    })
}

fn parse_legacy_v1_builder_volume(
    value: &serde_json::Value,
    builder: &str,
    expected_name: &str,
    expected_labels: Option<&LegacyV1OwnerLabels>,
) -> Result<VerifiedBuilderVolume> {
    if !is_legacy_v1_builder_name(builder) || expected_name != daemon_state_volume(builder) {
        anyhow::bail!("refuse identity verification for noncanonical v1 BuildKit volume");
    }
    let name = value
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .context("legacy BuildKit state volume inspect omitted Name")?;
    if name != expected_name
        || value.get("Driver").and_then(serde_json::Value::as_str) != Some("local")
    {
        anyhow::bail!(
            "legacy BuildKit state volume name or driver does not match its v1 owner row"
        );
    }
    let labels = value
        .get("Labels")
        .and_then(serde_json::Value::as_object)
        .context("legacy BuildKit state volume inspect omitted Labels")?;
    let labels = parse_legacy_v1_owner_labels(labels, "state volume")?;
    if expected_labels.is_some_and(|expected| *expected != labels) {
        anyhow::bail!("legacy BuildKit state volume ownership labels do not match its daemon");
    }
    let mountpoint = value
        .get("Mountpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|mountpoint| !mountpoint.is_empty())
        .context("legacy BuildKit state volume inspect omitted Mountpoint")?;
    Ok(VerifiedBuilderVolume {
        name: name.to_string(),
        mountpoint: mountpoint.to_string(),
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker inspections keep legacy owner proof hermetic"
)]
fn verify_legacy_v1_builder_daemon_with(
    builder: &str,
    inspect_container: &mut impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    inspect_volume: &mut impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    list_volume_users: &mut impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<Option<VerifiedLegacyV1BuilderDaemon>> {
    if !is_legacy_v1_builder_name(builder) {
        anyhow::bail!("refuse identity verification for noncanonical v1 BuildKit builder");
    }
    let daemon_name = daemon_container_name(builder);
    let Some(container) = inspect_container(&daemon_name)? else {
        return Ok(None);
    };
    let daemon = parse_legacy_v1_builder_container(&container, builder, &daemon_name)?;
    let volume_value = inspect_volume(&daemon.daemon.state_volume)?.with_context(|| {
        format!(
            "legacy BuildKit state volume {} is missing",
            daemon.daemon.state_volume
        )
    })?;
    let volume = parse_legacy_v1_builder_volume(
        &volume_value,
        builder,
        &daemon.daemon.state_volume,
        Some(&daemon.labels),
    )?;
    if daemon.daemon.volume_mountpoint != volume.mountpoint {
        anyhow::bail!("legacy BuildKit daemon mountpoint does not match its state volume");
    }
    let users = list_volume_users(&volume.name)?;
    if users != [daemon.daemon.id.clone()].into_iter().collect() {
        anyhow::bail!("legacy BuildKit state volume has an unverified user set");
    }
    Ok(Some(daemon))
}

fn parse_verified_builder_container(
    value: &serde_json::Value,
    builder: &str,
    owner_token: &str,
    expected_name: &str,
) -> Result<VerifiedBuilderDaemon> {
    let id = value
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .context("BuildKit daemon inspect omitted Id")?;
    if !valid_container_id(id) {
        anyhow::bail!("BuildKit daemon inspect returned an invalid immutable ID");
    }
    let name = value
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .context("BuildKit daemon inspect omitted Name")?;
    if name != format!("/{expected_name}") {
        anyhow::bail!("BuildKit daemon inspect name does not match its registered node");
    }
    let labels = value
        .pointer("/Config/Labels")
        .and_then(serde_json::Value::as_object)
        .context("BuildKit daemon inspect omitted Config.Labels")?;
    if labels
        .get(crate::docker_lease::BUILDKIT_BUILDER_LABEL)
        .and_then(serde_json::Value::as_str)
        != Some(builder)
        || labels
            .get(crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL)
            .and_then(serde_json::Value::as_str)
            != Some(owner_token)
    {
        anyhow::bail!("BuildKit daemon labels do not match the durable owner record");
    }
    let mounts = value
        .get("Mounts")
        .and_then(serde_json::Value::as_array)
        .context("BuildKit daemon inspect omitted Mounts")?;
    if mounts.len() != 1 {
        anyhow::bail!("BuildKit daemon must mount exactly one owned state volume");
    }
    let mount = &mounts[0];
    if mount.get("Type").and_then(serde_json::Value::as_str) != Some("volume")
        || mount.get("Destination").and_then(serde_json::Value::as_str) != Some("/var/lib/buildkit")
    {
        anyhow::bail!("BuildKit daemon state mount has an unexpected type or destination");
    }
    let state_volume = format!("{expected_name}_state");
    if mount.get("Name").and_then(serde_json::Value::as_str) != Some(&state_volume) {
        anyhow::bail!("BuildKit daemon state mount names a different volume");
    }
    let source = mount
        .get("Source")
        .and_then(serde_json::Value::as_str)
        .filter(|source| !source.is_empty())
        .context("BuildKit daemon state mount omitted Source")?;
    let running = value
        .pointer("/State/Running")
        .and_then(serde_json::Value::as_bool)
        .context("BuildKit daemon inspect omitted State.Running")?;
    let state = value
        .pointer("/State/Status")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::docker::client::ContainerState::parse)
        .context("BuildKit daemon inspect returned an unknown State.Status")?;
    Ok(VerifiedBuilderDaemon {
        id: id.to_string(),
        name: expected_name.to_string(),
        state_volume,
        volume_mountpoint: source.to_string(),
        state_running: running,
        state,
    })
}

fn parse_verified_builder_volume(
    value: &serde_json::Value,
    builder: &str,
    owner_token: &str,
    expected_name: &str,
) -> Result<VerifiedBuilderVolume> {
    let name = value
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .context("BuildKit state volume inspect omitted Name")?;
    if name != expected_name
        || value.get("Driver").and_then(serde_json::Value::as_str) != Some("local")
    {
        anyhow::bail!("BuildKit state volume identity or driver does not match its owner");
    }
    let labels = value
        .get("Labels")
        .and_then(serde_json::Value::as_object)
        .context("BuildKit state volume inspect omitted Labels")?;
    if labels
        .get(crate::docker_lease::BUILDKIT_BUILDER_LABEL)
        .and_then(serde_json::Value::as_str)
        != Some(builder)
        || labels
            .get(crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL)
            .and_then(serde_json::Value::as_str)
            != Some(owner_token)
    {
        anyhow::bail!("BuildKit state volume labels do not match the durable owner record");
    }
    let mountpoint = value
        .get("Mountpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|mountpoint| !mountpoint.is_empty())
        .context("BuildKit state volume inspect omitted Mountpoint")?;
    Ok(VerifiedBuilderVolume {
        name: name.to_string(),
        mountpoint: mountpoint.to_string(),
    })
}

fn container_ids_using_volume(volume: &str) -> Result<BTreeSet<String>> {
    let args = vec![
        "ps".to_string(),
        "--all".to_string(),
        "--quiet".to_string(),
        "--no-trunc".to_string(),
        "--filter".to_string(),
        format!("volume={volume}"),
    ];
    Ok(crate::docker::client::host_call(&args)?
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn verify_builder_daemon(
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
) -> Result<Option<VerifiedBuilderDaemon>> {
    if !is_builder_daemon_node(daemon_name, builder) {
        anyhow::bail!("refuse to verify noncanonical BuildKit daemon {daemon_name:?}");
    }
    let Some(container) = inspect_builder_container(daemon_name)? else {
        return Ok(None);
    };
    let record = registered_owner_record(builder)?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token does not match the durable registry");
    }
    let node = record
        .daemon_nodes
        .get(daemon_name)
        .context("BuildKit owner record has no identity for this daemon node")?;
    if !valid_container_id(&node.container_id) {
        anyhow::bail!("BuildKit owner record has malformed immutable daemon ID");
    }
    if node.state_volume_mountpoint.is_empty() {
        anyhow::bail!("BuildKit owner record has no durable state-volume identity");
    }
    let expected_id = &node.container_id;
    let expected_mountpoint = &node.state_volume_mountpoint;
    let daemon = parse_verified_builder_container(&container, builder, owner_token, daemon_name)?;
    if daemon.id.as_str() != expected_id.as_str() {
        anyhow::bail!("BuildKit daemon ID does not match its durable owner record");
    }
    let volume_value = inspect_builder_volume(&daemon.state_volume)?
        .with_context(|| format!("BuildKit state volume {} is missing", daemon.state_volume))?;
    let volume =
        parse_verified_builder_volume(&volume_value, builder, owner_token, &daemon.state_volume)?;
    if daemon.volume_mountpoint != volume.mountpoint {
        anyhow::bail!("BuildKit daemon mountpoint does not match its state volume");
    }
    if volume.mountpoint.as_str() != expected_mountpoint.as_str() {
        anyhow::bail!("BuildKit state-volume identity does not match its durable owner record");
    }
    let users = container_ids_using_volume(&daemon.state_volume)?;
    if users.iter().any(|id| id != &daemon.id) {
        anyhow::bail!("BuildKit state volume is also mounted by another container");
    }
    Ok(Some(daemon))
}

/// Verify an unclaimed daemon for destructive cleanup. A missing node row can
/// occur only in the Engine-create/owner-write crash window. In that case,
/// accept node 0 only after the durable builder token, exact daemon labels,
/// deterministic state-volume name, mountpoint, and exclusive volume user all
/// agree. The caller must already hold the builder lifecycle exclusion and
/// claim lock and must recheck the identity immediately before removal.
fn verify_builder_daemon_for_cleanup(
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
) -> Result<Option<VerifiedBuilderDaemon>> {
    if !is_builder_daemon_node(daemon_name, builder) {
        anyhow::bail!("refuse to verify noncanonical BuildKit daemon {daemon_name:?}");
    }
    let record = registered_owner_record(builder)?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token does not match the durable registry");
    }
    if record.daemon_nodes.contains_key(daemon_name) {
        return verify_builder_daemon(builder, owner_token, daemon_name);
    }
    if daemon_name != daemon_container_name(builder) {
        return Ok(None);
    }
    let mut inspect_container = inspect_builder_container;
    let mut inspect_volume = inspect_builder_volume;
    let mut volume_users = container_ids_using_volume;
    let daemon = inspect_admitted_builder_daemon_with(
        builder,
        owner_token,
        daemon_name,
        None,
        &mut inspect_container,
        &mut inspect_volume,
        &mut volume_users,
    )?;
    if daemon
        .as_ref()
        .is_some_and(|daemon| daemon.state != crate::docker::client::ContainerState::Created)
    {
        // No durable node identity means only the interrupted first-create
        // state is recoverable. An exited or otherwise changed object needs
        // a durable ID before cleanup.
        return Ok(None);
    }
    Ok(daemon)
}

fn verify_builder_volume(
    builder: &str,
    owner_token: &str,
    volume_name: &str,
) -> Result<Option<VerifiedBuilderVolume>> {
    let Some(value) = inspect_builder_volume(volume_name)? else {
        return Ok(None);
    };
    let record = registered_owner_record(builder)?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token does not match the durable registry");
    }
    let Some((daemon_name, node)) = record
        .daemon_nodes
        .iter()
        .find(|(daemon, _)| format!("{daemon}_state") == volume_name)
    else {
        anyhow::bail!("BuildKit state volume has no durable daemon-node identity");
    };
    if !is_builder_daemon_node(daemon_name, builder)
        || !valid_container_id(&node.container_id)
        || node.state_volume_mountpoint.is_empty()
    {
        anyhow::bail!("BuildKit owner record has malformed node identity");
    }
    let volume = parse_verified_builder_volume(&value, builder, owner_token, volume_name)?;
    if volume.mountpoint != node.state_volume_mountpoint {
        anyhow::bail!("BuildKit state-volume identity does not match its durable owner record");
    }
    if container_ids_using_volume(volume_name)?
        .iter()
        .any(|id| id != &node.container_id)
    {
        anyhow::bail!("BuildKit state volume is mounted by an unowned container");
    }
    Ok(Some(volume))
}

/// Verify the canonical state volume when create was interrupted before the
/// node row was written. A still-present daemon must prove exact ownership and
/// be its sole user; a volume-only remnant must have no users at all.
fn verify_builder_volume_for_cleanup(
    builder: &str,
    owner_token: &str,
    volume_name: &str,
) -> Result<Option<VerifiedBuilderVolume>> {
    let record = registered_owner_record(builder)?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token does not match the durable registry");
    }
    if record
        .daemon_nodes
        .keys()
        .any(|name| format!("{name}_state") == volume_name)
    {
        return verify_builder_volume(builder, owner_token, volume_name);
    }
    if volume_name != daemon_state_volume(builder) {
        return Ok(None);
    }
    let Some(value) = inspect_builder_volume(volume_name)? else {
        return Ok(None);
    };
    let volume = parse_verified_builder_volume(&value, builder, owner_token, volume_name)?;
    let daemon_name = daemon_container_name(builder);
    let daemon = verify_builder_daemon_for_cleanup(builder, owner_token, &daemon_name)?;
    let users = container_ids_using_volume(volume_name)?;
    match daemon {
        Some(daemon)
            if daemon.state_volume == volume.name
                && daemon.volume_mountpoint == volume.mountpoint
                && users == [daemon.id.clone()].into_iter().collect() => {}
        None if users.is_empty() => {}
        _ => anyhow::bail!("interrupted BuildKit state volume has an unverified user"),
    }
    Ok(Some(volume))
}

fn inspect_admitted_builder_daemon_with(
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
    expected_node: Option<&BuilderNodeRecord>,
    inspect_container: &mut impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    inspect_volume: &mut impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    volume_users: &mut impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<Option<VerifiedBuilderDaemon>> {
    if !is_bounded_builder_name(builder)
        || daemon_name != daemon_container_name(builder)
        || !valid_owner_token(owner_token)
    {
        anyhow::bail!("refuse identity verification for noncanonical BuildKit daemon");
    }
    let Some(container_value) = inspect_container(daemon_name)? else {
        return Ok(None);
    };
    let daemon =
        parse_verified_builder_container(&container_value, builder, owner_token, daemon_name)?;
    let volume_value = inspect_volume(&daemon.state_volume)?
        .with_context(|| format!("BuildKit state volume {} is missing", daemon.state_volume))?;
    let volume =
        parse_verified_builder_volume(&volume_value, builder, owner_token, &daemon.state_volume)?;
    if daemon.volume_mountpoint != volume.mountpoint {
        anyhow::bail!("BuildKit daemon mountpoint does not match its state volume");
    }
    if let Some(expected_node) = expected_node {
        if daemon.id != expected_node.container_id {
            anyhow::bail!("BuildKit daemon ID does not match its durable owner record");
        }
        if volume.mountpoint != expected_node.state_volume_mountpoint {
            anyhow::bail!("BuildKit state-volume identity does not match its durable owner record");
        }
    }
    let expected_users = [daemon.id.clone()].into_iter().collect::<BTreeSet<_>>();
    if volume_users(&daemon.state_volume)? != expected_users {
        anyhow::bail!("BuildKit state volume is not mounted exclusively by its admitted daemon");
    }
    Ok(Some(daemon))
}

fn persist_adopted_builder_node(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    daemon_name: &str,
    daemon: &VerifiedBuilderDaemon,
    reserved_holder: Option<&str>,
) -> Result<()> {
    let claim_path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &claim_path)?;
    let claims = read_registered_claims(&claim_path, builder)?
        .context("BuildKit daemon recovery has no registered runtime claim")?;
    match (claims.releasing.as_deref(), reserved_holder) {
        (Some(releasing), Some(expected)) if releasing == expected => {}
        (Some(_), _) => {
            anyhow::bail!("BuildKit daemon recovery is blocked while its holder is releasing");
        }
        (None, Some(_)) => {
            anyhow::bail!("BuildKit release recovery has no matching durable reservation");
        }
        (None, None) => {}
    }
    if claims.holders.len() != 1 {
        anyhow::bail!("BuildKit daemon recovery requires one exclusive active job claim");
    }
    if reserved_holder.is_some_and(|holder| !claims.holders.contains_key(holder)) {
        anyhow::bail!("BuildKit release recovery reservation has no matching holder");
    }
    let mut record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token changed during daemon recovery");
    }
    if daemon.name != daemon_name
        || daemon.state_volume != format!("{daemon_name}_state")
        || !valid_container_id(&daemon.id)
        || daemon.volume_mountpoint.is_empty()
    {
        anyhow::bail!("inspected BuildKit daemon has malformed recovery identity");
    }
    match record.daemon_nodes.get(daemon_name) {
        Some(existing)
            if existing.container_id == daemon.id
                && existing.state_volume_mountpoint == daemon.volume_mountpoint =>
        {
            return Ok(())
        }
        Some(_) => anyhow::bail!("BuildKit daemon identity changed during recovery"),
        None => {}
    }
    record.daemon_nodes.insert(
        daemon_name.to_string(),
        BuilderNodeRecord {
            container_id: daemon.id.clone(),
            state_volume_mountpoint: daemon.volume_mountpoint.clone(),
            created_unix: unix_now(),
            bootstrap_phase: BuilderBootstrapPhase::Unverified,
            config_sha256: None,
        },
    );
    record.updated_unix = unix_now();
    write_atomic_document(
        &owner_registry_file(registry_root, builder),
        &serde_json::to_vec_pretty(&record).context("encode recovered BuildKit daemon identity")?,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Engine inspections make crash recovery hermetic in tests"
)]
fn verify_admitted_builder_daemon_details_with(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    requested_id: &str,
    inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<VerifiedBuilderDaemon> {
    verify_admitted_builder_daemon_details_with_reservation(
        registry_root,
        run_root,
        builder,
        owner_token,
        requested_id,
        None,
        inspect_container,
        inspect_volume,
        volume_users,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "release adoption is allowed only for the named durable holder reservation"
)]
fn verify_admitted_builder_daemon_details_with_reservation(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    requested_id: &str,
    reserved_holder: Option<&str>,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<VerifiedBuilderDaemon> {
    let daemon_name = daemon_container_name(builder);
    let record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit daemon admission token does not match durable ownership");
    }
    if record.daemon_nodes.keys().any(|name| name != &daemon_name) {
        anyhow::bail!("BuildKit owner record contains an appended daemon node");
    }

    if let Some(node) = record.daemon_nodes.get(&daemon_name) {
        if requested_id != daemon_name && requested_id != node.container_id {
            anyhow::bail!("BuildKit daemon request does not match its admitted name or ID");
        }
        let daemon = inspect_admitted_builder_daemon_with(
            builder,
            owner_token,
            &daemon_name,
            Some(node),
            &mut inspect_container,
            &mut inspect_volume,
            &mut volume_users,
        )?
        .context("registered BuildKit daemon is absent")?;
        return Ok(daemon);
    }

    // A crash can happen after Engine creates the daemon but before the
    // create response's ID is durably recorded. Recover only from the one
    // canonical node name; an ID request is accepted only if its inspected
    // object is that exact, fully labelled daemon and state volume.
    if requested_id != daemon_name && !valid_container_id(requested_id) {
        anyhow::bail!("BuildKit daemon recovery requires its canonical node name or immutable ID");
    }
    let candidate = inspect_admitted_builder_daemon_with(
        builder,
        owner_token,
        &daemon_name,
        None,
        &mut inspect_container,
        &mut inspect_volume,
        &mut volume_users,
    )?
    .context("canonical BuildKit daemon is absent during owner recovery")?;
    if requested_id != daemon_name && requested_id != candidate.id {
        anyhow::bail!("BuildKit daemon request does not match the inspected canonical node");
    }
    persist_adopted_builder_node(
        registry_root,
        run_root,
        builder,
        owner_token,
        &daemon_name,
        &candidate,
        reserved_holder,
    )?;

    let recovered_record = read_owner_record(registry_root, builder)?
        .context("BuildKit owner record disappeared after daemon recovery")?;
    if recovered_record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token changed after daemon recovery");
    }
    let recovered_node = recovered_record
        .daemon_nodes
        .get(&daemon_name)
        .context("BuildKit daemon identity was not persisted during recovery")?;
    let current = inspect_admitted_builder_daemon_with(
        builder,
        owner_token,
        &daemon_name,
        Some(recovered_node),
        &mut inspect_container,
        &mut inspect_volume,
        &mut volume_users,
    )?
    .context("canonical BuildKit daemon disappeared after owner recovery")?;
    if current.id != candidate.id
        || current.state_volume != candidate.state_volume
        || current.volume_mountpoint != candidate.volume_mountpoint
    {
        anyhow::bail!("BuildKit daemon identity changed during owner recovery");
    }
    Ok(current)
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Engine inspections keep crash recovery hermetic in tests"
)]
fn verify_admitted_builder_daemon_with(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    requested_id: &str,
    inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<String> {
    Ok(verify_admitted_builder_daemon_details_with(
        registry_root,
        run_root,
        builder,
        owner_token,
        requested_id,
        inspect_container,
        inspect_volume,
        volume_users,
    )?
    .id)
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Engine inspections prove release safety without Docker in tests"
)]
fn resolve_admitted_builder_release_id_with(
    registry_root: &Path,
    run_root: &Path,
    builder: &str,
    owner_token: &str,
    releasing_holder: &str,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<Option<String>> {
    if !is_bounded_builder_name(builder) || !valid_owner_token(owner_token) {
        anyhow::bail!("refuse release identity lookup for noncanonical BuildKit owner");
    }
    let record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit owner token does not match durable release ownership");
    }
    let claim_path = claims_file(run_root, builder);
    let claims = read_registered_claims(&claim_path, builder)?
        .context("BuildKit release has no registered runtime claim")?;
    if claims.releasing.as_deref() != Some(releasing_holder)
        || claims.holders.len() != 1
        || !claims.holders.contains_key(releasing_holder)
    {
        anyhow::bail!("BuildKit release recovery lacks its exclusive holder reservation");
    }
    let daemon_name = daemon_container_name(builder);
    if record.daemon_nodes.keys().any(|name| name != &daemon_name) {
        anyhow::bail!("BuildKit owner record contains an appended daemon node during release");
    }
    if inspect_container(&daemon_name)?.is_some() {
        // Existing daemon: apply normal exact-name, owner-label, state-volume,
        // and exclusive-user proof. This also adopts a valid create that
        // crashed before the immutable ID write.
        let daemon = verify_admitted_builder_daemon_details_with_reservation(
            registry_root,
            run_root,
            builder,
            owner_token,
            &daemon_name,
            Some(releasing_holder),
            &mut inspect_container,
            &mut inspect_volume,
            &mut volume_users,
        )?;
        let updated = read_owner_record(registry_root, builder)?
            .context("BuildKit owner record disappeared during release identity proof")?;
        let durable = updated
            .daemon_nodes
            .get(&daemon_name)
            .context("BuildKit release identity was not persisted after inspection")?;
        if durable.container_id != daemon.id {
            anyhow::bail!("BuildKit release ID differs from its durable owner record");
        }
        return Ok(Some(daemon.id));
    }

    // Setup may fail before Engine creates the daemon (or after creating
    // only its volume). Release the claim only if the canonical daemon is
    // absent, its deterministic state volume is absent or exactly owner
    // labelled, and no container of any state uses that volume.
    let volume_name = daemon_state_volume(builder);
    if let Some(value) = inspect_volume(&volume_name)? {
        let volume = parse_verified_builder_volume(&value, builder, owner_token, &volume_name)?;
        if let Some(node) = record.daemon_nodes.get(&daemon_name)
            && (node.container_id.is_empty() || node.state_volume_mountpoint != volume.mountpoint)
        {
            anyhow::bail!("BuildKit state volume differs from its durable daemon identity");
        }
    }
    let users = volume_users(&volume_name)?;
    if !users.is_empty() {
        anyhow::bail!("BuildKit state volume is still mounted by a container without its daemon");
    }
    Ok(None)
}

fn recheck_builder_daemon(
    builder: &str,
    owner_token: &str,
    expected: &VerifiedBuilderDaemon,
) -> Result<VerifiedBuilderDaemon> {
    let current = verify_builder_daemon(builder, owner_token, &expected.name)?
        .with_context(|| format!("registered BuildKit daemon {} disappeared", expected.name))?;
    if current.id != expected.id
        || current.state_volume != expected.state_volume
        || current.volume_mountpoint != expected.volume_mountpoint
    {
        anyhow::bail!("registered BuildKit daemon identity changed during maintenance");
    }
    Ok(current)
}

pub(crate) fn verify_admitted_builder_daemon(
    builder: &str,
    owner_token: &str,
    requested_id: &str,
) -> Result<String> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    verify_admitted_builder_daemon_with(
        &registry_root,
        &run_root,
        builder,
        owner_token,
        requested_id,
        inspect_builder_container,
        inspect_builder_volume,
        container_ids_using_volume,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Engine inspections keep first-create admission hermetic in tests"
)]
fn require_admitted_builder_first_create_available_with(
    registry_root: &Path,
    builder: &str,
    owner_token: &str,
    mut inspect_container: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
) -> Result<()> {
    if !is_bounded_builder_name(builder) || !valid_owner_token(owner_token) {
        anyhow::bail!("refuse first-create check for noncanonical BuildKit owner");
    }
    let record = read_owner_record(registry_root, builder)?
        .with_context(|| format!("BuildKit owner record for {builder} is absent"))?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit first-create token does not match durable ownership");
    }
    let daemon_name = daemon_container_name(builder);
    if !record.daemon_nodes.is_empty() {
        anyhow::bail!("BuildKit first-create is forbidden after daemon identity was recorded");
    }
    if inspect_container(&daemon_name)?.is_some() {
        anyhow::bail!("BuildKit daemon already exists without a completed bootstrap identity");
    }
    let state_volume = daemon_state_volume(builder);
    if let Some(value) = inspect_volume(&state_volume)? {
        parse_verified_builder_volume(&value, builder, owner_token, &state_volume)?;
        if !volume_users(&state_volume)?.is_empty() {
            anyhow::bail!("BuildKit first-create state volume is already mounted by a container");
        }
    }
    Ok(())
}

/// Check the only safe pre-create state for a newly admitted Buildx daemon.
/// Engine itself returns the expected 404 to Buildx's initial inspect. An
/// existing daemon is never treated as absent merely because its owner ID
/// write or bootstrap record is missing; only exact owner-labelled, unused
/// volume state may be reused after a volume-only interrupted create.
pub(crate) fn require_admitted_builder_first_create_available(
    builder: &str,
    owner_token: &str,
) -> Result<()> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    require_admitted_builder_first_create_available_with(
        &registry_root,
        builder,
        owner_token,
        inspect_builder_container,
        inspect_builder_volume,
        container_ids_using_volume,
    )
}

/// Verify that the exact durably admitted daemon is currently running. The
/// lease applies bootstrap-phase policy separately so it can probe readiness
/// while this helper remains phase-agnostic.
pub(crate) fn verify_admitted_builder_daemon_running(
    builder: &str,
    owner_token: &str,
    requested_id: &str,
) -> Result<bool> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    let daemon = verify_admitted_builder_daemon_details_with(
        &registry_root,
        &run_root,
        builder,
        owner_token,
        requested_id,
        inspect_builder_container,
        inspect_builder_volume,
        container_ids_using_volume,
    )?;
    Ok(daemon.state_running)
}

fn require_exclusive_network_claim(
    run_root: &Path,
    builder: &str,
    job_id: &str,
    releasing: bool,
) -> Result<()> {
    let path = claims_file(run_root, builder);
    let claims = read_registered_claims(&path, builder)?
        .context("persistent BuildKit builder has no registered runtime claim")?;
    if claims.holders.len() != 1 || !claims.holders.contains_key(job_id) {
        anyhow::bail!(
            "persistent BuildKit network operation requires the exclusive claim for job {job_id}"
        );
    }
    if releasing {
        if claims.releasing.as_deref() != Some(job_id) {
            anyhow::bail!(
                "persistent BuildKit network detach requires this job's release reservation"
            );
        }
    } else if let Some(holder) = claims.releasing.as_deref() {
        anyhow::bail!("persistent BuildKit builder {builder} has a teardown release reserved for job {holder}");
    }
    Ok(())
}

/// Reconcile an already admitted daemon onto exactly this job's private
/// network. The only held lock is this builder's claim lock; it serializes
/// network changes with claim release without blocking unrelated builders.
pub(crate) fn reconcile_admitted_builder_network(
    builder: &str,
    owner_token: &str,
    expected_container_id: &str,
    network_name: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<()> {
    let recovered_id = verify_admitted_builder_daemon(builder, owner_token, expected_container_id)?;
    if recovered_id != expected_container_id {
        anyhow::bail!("BuildKit daemon identity changed before private network reconciliation");
    }
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    let path = claims_file(&run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    require_exclusive_network_claim(&run_root, builder, job_id, false)?;

    reconcile_admitted_builder_network_with(
        builder,
        owner_token,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        verify_admitted_builder_daemon,
        inspect_builder_container,
        inspect_builder_network,
        |network_id, container_id| {
            let args = vec![
                "network".to_string(),
                "disconnect".to_string(),
                "--force".to_string(),
                network_id.to_string(),
                container_id.to_string(),
            ];
            crate::docker::client::host_call(&args)
                .map(|_| ())
                .with_context(|| format!("disconnect Docker network {network_id}"))
        },
        |network_id, container_id| {
            let args = vec![
                "network".to_string(),
                "connect".to_string(),
                network_id.to_string(),
                container_id.to_string(),
            ];
            crate::docker::client::host_call(&args)
                .map(|_| ())
                .with_context(|| format!("connect Docker network {network_id}"))
        },
    )
}

/// Detach a daemon from its private job network while the matching claim is
/// reserved for release. A failed or unverifiable detach returns an error so
/// the caller retains the holder and retries later.
pub(crate) fn detach_admitted_builder_network(
    builder: &str,
    owner_token: &str,
    expected_container_id: &str,
    network_name: &str,
    job_holder: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<()> {
    let recovered_id = verify_admitted_builder_daemon(builder, owner_token, expected_container_id)?;
    if recovered_id != expected_container_id {
        anyhow::bail!("BuildKit daemon identity changed before private network detach");
    }
    let run_root =
        claims_run_root().ok_or_else(|| anyhow::anyhow!("BuildKit claim root is unavailable"))?;
    let path = claims_file(&run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    require_exclusive_network_claim(&run_root, builder, job_holder, true)?;

    detach_admitted_builder_network_with(
        builder,
        owner_token,
        expected_container_id,
        network_name,
        job_id,
        daemon_id,
        verify_admitted_builder_daemon,
        inspect_builder_container,
        inspect_builder_network,
        |network_id, container_id| {
            let args = vec![
                "network".to_string(),
                "disconnect".to_string(),
                "--force".to_string(),
                network_id.to_string(),
                container_id.to_string(),
            ];
            crate::docker::client::host_call(&args)
                .map(|_| ())
                .with_context(|| format!("disconnect Docker network {network_id}"))
        },
    )
}

/// Release one admitted builder holder only after its exact daemon has been
/// detached from this lease's private network. The daemon ID comes from the
/// durable v2 owner record (or exact crash recovery) and is reinspected
/// before detach; unverified identity leaves the claim untouched.
pub(crate) fn release_admitted_builder_after_network_detach(
    run_root: &Path,
    builder: &str,
    job_holder: &str,
    network_name: &str,
    job_id: &str,
    daemon_id: &str,
) -> Result<ReleaseOutcome> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("refuse network-aware release for noncanonical BuildKit builder");
    }
    // Setup journals a name only after the durable admission claim is
    // written. Older/partial journals and capacity eviction can still leave
    // a builder name with no holder; that case must be an idempotent no-op
    // before requiring owner or daemon metadata.
    let claim_path = claims_file(run_root, builder);
    let holder = {
        let _lock = lock_claims(builder, &claim_path)?;
        let claims = read_claims(&claim_path)
            .with_context(|| format!("read BuildKit claim before release for {builder}"))?;
        claims.holders.get(job_holder).cloned()
    };
    let Some(holder) = holder else {
        return Ok(ReleaseOutcome {
            removed_last: false,
            stopped: false,
        });
    };
    let present =
        running_container_names().context("list containers before releasing BuildKit job claim")?;
    let still_live = holder
        .required_containers
        .iter()
        .filter(|required| present.contains(*required))
        .cloned()
        .collect::<Vec<_>>();
    if !still_live.is_empty() {
        anyhow::bail!(
            "refuse BuildKit claim release for {builder}; required job/service containers remain live: {}",
            still_live.join(", ")
        );
    }
    let owner_token = registered_owner_token(builder)?;
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    let daemon_container_id = resolve_admitted_builder_release_id_with(
        &registry_root,
        run_root,
        builder,
        &owner_token,
        job_holder,
        inspect_builder_container,
        inspect_builder_volume,
        container_ids_using_volume,
    )
    .context("prove exact BuildKit daemon or safe no-daemon state before network release")?;
    release_after_network_detach_if_last(
        run_root,
        builder,
        job_holder,
        || {
            if let Some(container_id) = daemon_container_id.as_deref() {
                detach_admitted_builder_network(
                    builder,
                    &owner_token,
                    container_id,
                    network_name,
                    job_holder,
                    job_id,
                    daemon_id,
                )
            } else {
                Ok(())
            }
        },
        || stop_builder_daemon(builder),
    )
}

pub(crate) fn verify_admitted_builder_volume(
    builder: &str,
    owner_token: &str,
    requested_name: &str,
) -> Result<()> {
    let expected_name = daemon_state_volume(builder);
    if requested_name != expected_name {
        anyhow::bail!("BuildKit volume request does not match its admitted state name");
    }
    let record = registered_owner_record(builder)?;
    if record.owner_token.as_deref() != Some(owner_token) {
        anyhow::bail!("BuildKit volume admission token does not match durable ownership");
    }
    if !record
        .daemon_nodes
        .contains_key(&daemon_container_name(builder))
    {
        verify_admitted_builder_daemon(builder, owner_token, &daemon_container_name(builder))?;
    }
    verify_builder_volume(builder, owner_token, requested_name)?
        .context("registered BuildKit state volume is absent")?;
    Ok(())
}

/// Stop one builder's daemon. Returns true when the daemon exists (a stop
/// acted or it was already stopped — exit 0 meant true here before the
/// migration, and the CLI leg still cannot distinguish); a missing daemon
/// reads as already stopped. Network-aware release callers hold the durable
/// final-holder reservation after detaching the job network; maintenance
/// callers hold the exclusive lifecycle lock after proving there are no
/// holders.
pub(crate) fn stop_builder_daemon(builder: &str) -> Result<bool> {
    let owner_token = registered_owner_token(builder)?;
    let Some(daemon) =
        verify_builder_daemon(builder, &owner_token, &daemon_container_name(builder))?
    else {
        return Ok(false);
    };
    let daemon = recheck_builder_daemon(builder, &owner_token, &daemon)?;
    match crate::docker::Docker::host().container_stop(&daemon.id, None) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stop BuildKit daemon {}", daemon.name)),
    }
}

fn verify_legacy_v1_builder_daemon(builder: &str) -> Result<Option<VerifiedLegacyV1BuilderDaemon>> {
    if !is_legacy_v1_builder_name(builder) {
        anyhow::bail!("refuse legacy BuildKit operation for noncanonical builder {builder:?}");
    }
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    read_legacy_v1_owner_record(&registry_root, builder)?
        .with_context(|| format!("legacy v1 BuildKit owner row for {builder} is absent"))?;
    verify_legacy_v1_builder_daemon_with(
        builder,
        &mut inspect_builder_container,
        &mut inspect_builder_volume,
        &mut container_ids_using_volume,
    )
}

fn stop_legacy_v1_builder_daemon(builder: &str) -> Result<bool> {
    let Some(daemon) = verify_legacy_v1_builder_daemon(builder)? else {
        return Ok(false);
    };
    match crate::docker::Docker::host().container_stop(&daemon.daemon.id, None) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("stop legacy BuildKit daemon {}", daemon.daemon.name)),
    }
}

fn start_legacy_v1_builder_daemon(builder: &str) -> Result<bool> {
    let Some(daemon) = verify_legacy_v1_builder_daemon(builder)? else {
        return Ok(false);
    };
    match crate::docker::Docker::host().container_start(&daemon.daemon.id) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("start legacy BuildKit daemon {}", daemon.daemon.name)),
    }
}

/// Start one builder's daemon. Missing reads as already gone. The undo half
/// of every stop that runs outside the claim lock: when a recheck finds
/// holders that arrived mid-stop, this brings the daemon back for them.
pub(crate) fn start_builder_daemon(builder: &str) -> Result<bool> {
    let owner_token = registered_owner_token(builder)?;
    let Some(daemon) =
        verify_builder_daemon(builder, &owner_token, &daemon_container_name(builder))?
    else {
        return Ok(false);
    };
    let daemon = recheck_builder_daemon(builder, &owner_token, &daemon)?;
    match crate::docker::Docker::host().container_start(&daemon.id) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("start BuildKit daemon {}", daemon.name)),
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
const BUILDKIT_DAEMON_SOCKET: &str = "unix:///run/buildkit/buildkitd.sock";

fn buildctl_args(container_id: &str, command: &[&str]) -> Result<Vec<String>> {
    if container_id.len() != 64 || !container_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("refuse BuildKit maintenance for invalid container ID");
    }
    let mut args = vec![
        "exec".to_string(),
        container_id.to_string(),
        "buildctl".to_string(),
        "--addr".to_string(),
        BUILDKIT_DAEMON_SOCKET.to_string(),
    ];
    args.extend(command.iter().map(|part| (*part).to_string()));
    Ok(args)
}

fn parse_buildctl_disk_usage(output: &str) -> Result<u64> {
    let records: Vec<serde_json::Value> =
        serde_json::from_str(output).context("parse BuildKit daemon disk-usage JSON")?;
    records.into_iter().try_fold(0_u64, |total, record| {
        let size = record
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .context("BuildKit daemon disk-usage record has no nonnegative size")?;
        total
            .checked_add(size)
            .context("BuildKit daemon disk-usage total overflowed")
    })
}

/// Query the exact Velnor daemon by its Engine name. Buildx client state is
/// stored inside each job at `/github/home/.docker`, so host `buildx du` can
/// never be the inventory or usage source for these builders.
fn builder_disk_usage(builder: &str) -> Result<u64> {
    let owner_token = registered_owner_token(builder)?;
    let daemon = verify_builder_daemon(builder, &owner_token, &daemon_container_name(builder))?
        .with_context(|| format!("registered BuildKit daemon for {builder} is absent"))?;
    if !daemon.state_running {
        anyhow::bail!("registered BuildKit daemon for {builder} is stopped");
    }
    let daemon = recheck_builder_daemon(builder, &owner_token, &daemon)?;
    let args = buildctl_args(&daemon.id, &["du", "--format", "{{json .}}"])?;
    let output = crate::docker::client::host_call(&args)
        .with_context(|| format!("query BuildKit daemon disk usage for {builder}"))?;
    parse_buildctl_disk_usage(&output)
}

/// Prune the exact daemon registered by its durable owner record. This does
/// not depend on the host Buildx registry, which is separate from the one
/// mounted inside every Velnor job container.
fn prune_builder(builder: &str) -> Result<u64> {
    let owner_token = registered_owner_token(builder)?;
    let daemon_name = daemon_container_name(builder);
    let initial = verify_builder_daemon(builder, &owner_token, &daemon_name)?
        .with_context(|| format!("registered BuildKit daemon for {builder} is absent"))?;
    if !initial.state_running {
        let current = recheck_builder_daemon(builder, &owner_token, &initial)?;
        crate::docker::Docker::host()
            .container_start(&current.id)
            .with_context(|| format!("start BuildKit daemon {builder}"))?;
    }
    let daemon = verify_builder_daemon(builder, &owner_token, &daemon_name)?
        .with_context(|| format!("registered BuildKit daemon for {builder} disappeared"))?;
    if daemon.id != initial.id {
        anyhow::bail!("registered BuildKit daemon identity changed while starting");
    }
    let before = builder_disk_usage(builder)?;
    let current = recheck_builder_daemon(builder, &owner_token, &daemon)?;
    let args = buildctl_args(&current.id, &["prune", "--all", "--format", "{{json .}}"])?;
    crate::docker::client::host_call(&args)
        .with_context(|| format!("prune BuildKit daemon {builder}"))?;
    let after_daemon =
        verify_builder_daemon(builder, &owner_token, &daemon_name)?.with_context(|| {
            format!("registered BuildKit daemon for {builder} disappeared after prune")
        })?;
    if after_daemon.id != initial.id {
        anyhow::bail!("registered BuildKit daemon identity changed during prune");
    }
    let after = builder_disk_usage(builder)?;
    Ok(before.saturating_sub(after))
}

/// Delete one builder's daemon and state volume, then its claim file. The
/// caller has already proved the name belongs to Velnor and has no holders.
fn remove_builder_and_claims_with(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    mut remove: impl FnMut(&str) -> Result<bool>,
) -> Result<bool> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    match read_registered_claims(&path, builder) {
        Ok(Some(claims)) if claims.holders.is_empty() && claims.releasing.is_none() => {}
        Ok(Some(_)) => return Ok(false),
        Ok(None) => return Ok(false),
        Err(error) => {
            log_torn_claims(builder, &path, &error);
            return Err(error);
        }
    }
    let Some(registry_root) = registry_root else {
        // Durable membership is the only source of the physical owner token.
        // Never delete a named daemon or its metadata without it.
        return Ok(false);
    };
    let owner_token = owner_token_from_registry(registry_root, builder)?;
    // Keep the same per-builder lock from the final empty-claim proof through
    // exact daemon/volume removal and durable owner-record deletion. Setup
    // cannot publish a replacement claim between these steps.
    if !remove(builder)? {
        return Ok(false);
    }
    if owner_token_from_registry(registry_root, builder)? != owner_token {
        return Ok(false);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => sync_parent(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
    }
    if !remove_owner_record(registry_root, builder, &owner_token)? {
        anyhow::bail!("BuildKit owner record changed while deleting {builder}");
    }
    Ok(true)
}

fn delete_registered_builder(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    observed: Option<&crate::docker::client::ExitInfo>,
    remove: &mut impl FnMut(&str, Option<&crate::docker::client::ExitInfo>) -> Result<bool>,
) -> Result<bool> {
    remove_builder_and_claims_with(run_root, registry_root, builder, |name| {
        remove(name, observed)
    })
}

fn delete_legacy_v1_builder(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    observed: Option<&crate::docker::client::ExitInfo>,
    remove: &mut impl FnMut(&str, Option<&crate::docker::client::ExitInfo>) -> Result<bool>,
) -> Result<bool> {
    if !is_legacy_v1_builder_name(builder) {
        anyhow::bail!("refuse cleanup for noncanonical legacy BuildKit builder {builder:?}");
    }
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    if let Some(claims) = read_claims_if_present(&path)?
        && (claims.builder != builder || !claims.holders.is_empty() || claims.releasing.is_some())
    {
        return Ok(false);
    }
    let owner_path = owner_registry_file(registry_root, builder);
    let owner_bytes = std::fs::read(&owner_path)
        .with_context(|| format!("read legacy BuildKit owner row {}", owner_path.display()))?;
    let owner: LegacyV1BuilderOwnerRecord = serde_json::from_slice(&owner_bytes)
        .with_context(|| format!("parse legacy BuildKit owner row {}", owner_path.display()))?;
    if owner.version != 1 || owner.builder != builder {
        anyhow::bail!("legacy BuildKit owner row changed before cleanup for {builder}");
    }
    if !remove(builder, observed)? {
        return Ok(false);
    }
    let current_owner_bytes = std::fs::read(&owner_path)
        .with_context(|| format!("re-read legacy BuildKit owner row {}", owner_path.display()))?;
    if current_owner_bytes != owner_bytes {
        anyhow::bail!("legacy BuildKit owner row changed while deleting {builder}");
    }
    match std::fs::remove_file(&path) {
        Ok(()) => sync_parent(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
    }
    std::fs::remove_file(&owner_path)
        .with_context(|| format!("remove legacy BuildKit owner row {}", owner_path.display()))?;
    sync_parent(&owner_path)?;
    Ok(true)
}

/// Delete one builder's daemon container and state volume. Only ever called
/// with zero holders past the idle horizon: the next build recreates a cold
/// daemon from the same stable name.
fn remove_builder_after_reap_observation(
    builder: &str,
    observed: Option<&crate::docker::client::ExitInfo>,
) -> Result<bool> {
    if is_legacy_v1_builder_name(builder) {
        return remove_legacy_v1_builder_after_reap_observation(builder, observed);
    }
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("refuse cleanup for noncanonical BuildKit builder {builder:?}");
    }
    let owner_token = registered_owner_token(builder)?;
    let owner_record = registered_owner_record(builder)?;
    remove_builder_objects_after_reap_with(
        builder,
        &owner_token,
        &owner_record.daemon_nodes,
        observed,
        list_buildkit_container_names,
        |daemon| verify_builder_daemon_for_cleanup(builder, &owner_token, daemon),
        |container_id| match crate::docker::Docker::host().container_remove(
            container_id,
            true,
            false,
        ) {
            Ok(_) => Ok(()),
            Err(error) if crate::docker::client::is_not_found(&error) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("remove BuildKit daemon ID {container_id}"))
            }
        },
        list_buildkit_volume_names,
        |volume| verify_builder_volume_for_cleanup(builder, &owner_token, volume),
        container_ids_using_volume,
        |volume| {
            let args = vec![
                "volume".to_string(),
                "rm".to_string(),
                "--force".to_string(),
                volume.to_string(),
            ];
            match crate::docker::client::host_call(&args) {
                Ok(_) => Ok(()),
                Err(error) if crate::docker::client::is_not_found(&error) => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("remove BuildKit state volume {volume}"))
                }
            }
        },
    )
}

fn remove_legacy_v1_builder_after_reap_observation(
    builder: &str,
    observed: Option<&crate::docker::client::ExitInfo>,
) -> Result<bool> {
    let registry_root = claims_registry_root()
        .ok_or_else(|| anyhow::anyhow!("BuildKit owner registry is unavailable"))?;
    read_legacy_v1_owner_record(&registry_root, builder)?
        .with_context(|| format!("legacy v1 BuildKit owner row for {builder} is absent"))?;
    let daemon_name = daemon_container_name(builder);
    remove_legacy_v1_builder_objects_with(
        builder,
        observed,
        list_buildkit_container_names,
        |name| {
            if name != daemon_name {
                anyhow::bail!("refuse to inspect noncanonical legacy BuildKit daemon {name}");
            }
            inspect_builder_container(name)?.map_or(Ok(None), |value| {
                parse_legacy_v1_builder_container(&value, builder, name).map(Some)
            })
        },
        |container_id| match crate::docker::Docker::host().container_remove(
            container_id,
            true,
            false,
        ) {
            Ok(_) => Ok(()),
            Err(error) if crate::docker::client::is_not_found(&error) => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("remove legacy BuildKit daemon ID {container_id}")),
        },
        list_buildkit_volume_names,
        inspect_builder_volume,
        container_ids_using_volume,
        |volume| {
            let args = vec![
                "volume".to_string(),
                "rm".to_string(),
                "--force".to_string(),
                volume.to_string(),
            ];
            match crate::docker::client::host_call(&args) {
                Ok(_) => Ok(()),
                Err(error) if crate::docker::client::is_not_found(&error) => Ok(()),
                Err(error) => Err(error)
                    .with_context(|| format!("remove legacy BuildKit state volume {volume}")),
            }
        },
    )
}

/// Capacity eviction has no age observation, but still binds removal to one
/// immutable Engine ID and state snapshot. A live daemon is retained: capacity
/// pressure cannot bypass the lifecycle stop/recheck protocol.
fn remove_builder_for_capacity(builder: &str) -> Result<bool> {
    if !is_bounded_builder_name(builder) {
        anyhow::bail!("refuse capacity cleanup for noncanonical BuildKit builder {builder:?}");
    }
    let daemon = daemon_container_name(builder);
    match crate::docker::Docker::host().inspect_exit(&daemon) {
        Ok(observed) => remove_builder_after_reap_observation(builder, Some(&observed)),
        Err(error) if crate::docker::client::is_not_found(&error) => {
            remove_builder_after_reap_observation(builder, None)
        }
        Err(error) => Err(error).with_context(|| format!("inspect capacity candidate {builder}")),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected exact-object operations keep reaper removal hermetic"
)]
fn remove_builder_objects_after_reap_with(
    builder: &str,
    owner_token: &str,
    expected_nodes: &BTreeMap<String, BuilderNodeRecord>,
    observed: Option<&crate::docker::client::ExitInfo>,
    list_daemons: impl FnMut() -> Result<Vec<String>>,
    mut inspect_daemon: impl FnMut(&str) -> Result<Option<VerifiedBuilderDaemon>>,
    remove_daemon_by_id: impl FnMut(&str) -> Result<()>,
    list_volumes: impl FnMut() -> Result<Vec<String>>,
    inspect_volume: impl FnMut(&str) -> Result<Option<VerifiedBuilderVolume>>,
    list_volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
    remove_volume: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    let observed = match observed {
        Some(observed) => {
            let id = observed
                .id
                .as_deref()
                .filter(|id| valid_container_id(id))
                .context("reaper observation omitted a valid immutable daemon ID")?;
            let state = observed
                .status
                .context("reaper observation has an unknown daemon state")?;
            if !state.safe_to_reclaim() || state == crate::docker::client::ContainerState::Removing
            {
                anyhow::bail!("reaper observation is not in a stable reclaimable state");
            }
            Some((id.to_string(), state))
        }
        None => None,
    };
    if !is_bounded_builder_name(builder) || !valid_owner_token(owner_token) {
        anyhow::bail!("refuse exact BuildKit reaper removal without canonical owner identity");
    }
    let canonical_daemon = daemon_container_name(builder);
    remove_builder_objects_with(
        builder,
        owner_token,
        expected_nodes,
        list_daemons,
        |daemon_name| {
            let current = inspect_daemon(daemon_name)?;
            if daemon_name == canonical_daemon {
                match (observed.as_ref(), current.as_ref()) {
                    (Some((expected_id, expected_state)), Some(current))
                        if current.id == *expected_id && current.state == *expected_state => {}
                    (Some(_), Some(_)) => {
                        anyhow::bail!(
                            "BuildKit daemon ID or state changed after the reaper age observation"
                        );
                    }
                    (None, Some(_)) => {
                        anyhow::bail!(
                            "BuildKit daemon appeared after the reaper observed it absent"
                        );
                    }
                    (_, None) => {}
                }
            }
            Ok(current)
        },
        remove_daemon_by_id,
        list_volumes,
        inspect_volume,
        list_volume_users,
        remove_volume,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected exact-object operations keep v1 cleanup hermetic"
)]
fn remove_legacy_v1_builder_objects_with(
    builder: &str,
    observed: Option<&crate::docker::client::ExitInfo>,
    mut list_daemons: impl FnMut() -> Result<Vec<String>>,
    mut inspect_daemon: impl FnMut(&str) -> Result<Option<VerifiedLegacyV1BuilderDaemon>>,
    mut remove_daemon_by_id: impl FnMut(&str) -> Result<()>,
    mut list_volumes: impl FnMut() -> Result<Vec<String>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<serde_json::Value>>,
    mut list_volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
    mut remove_volume: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    if !is_legacy_v1_builder_name(builder) {
        anyhow::bail!("refuse legacy cleanup for a noncanonical v1 BuildKit builder");
    }
    let observed = match observed {
        Some(observed) => {
            let id = observed
                .id
                .as_deref()
                .filter(|id| valid_container_id(id))
                .context("legacy reaper observation omitted an immutable daemon ID")?;
            let state = observed
                .status
                .context("legacy reaper observation has an unknown daemon state")?;
            if !state.safe_to_reclaim() || state == crate::docker::client::ContainerState::Removing
            {
                anyhow::bail!("legacy reaper observation is not stably reclaimable");
            }
            Some((id.to_string(), state))
        }
        None => None,
    };
    let daemon_name = daemon_container_name(builder);
    let volume_name = daemon_state_volume(builder);
    let listed_daemons = list_daemons()?.into_iter().collect::<BTreeSet<_>>();
    if listed_daemons
        .iter()
        .any(|name| is_potential_builder_daemon_node(name, builder) && name != &daemon_name)
    {
        anyhow::bail!(
            "legacy v1 BuildKit builder {builder} has an unrecorded or ambiguous appended daemon node"
        );
    }
    let listed_volumes = list_volumes()?.into_iter().collect::<BTreeSet<_>>();
    if listed_volumes
        .iter()
        .any(|name| is_potential_builder_state_volume(name, builder) && name != &volume_name)
    {
        anyhow::bail!(
            "legacy v1 BuildKit builder {builder} has an unrecorded or ambiguous appended state volume"
        );
    }
    let daemon = inspect_daemon(&daemon_name)?;
    if listed_daemons.contains(&daemon_name) && daemon.is_none() {
        anyhow::bail!("listed legacy v1 BuildKit daemon could not be inspected");
    }
    if daemon.is_some() && !listed_daemons.contains(&daemon_name) {
        anyhow::bail!("legacy v1 BuildKit daemon appeared after the object inventory");
    }
    match (observed.as_ref(), daemon.as_ref()) {
        (Some((expected_id, expected_state)), Some(current))
            if current.daemon.id == *expected_id && current.daemon.state == *expected_state => {}
        (Some(_), Some(_)) => {
            anyhow::bail!(
                "legacy v1 BuildKit daemon ID or state changed after its age observation"
            );
        }
        (None, Some(_)) => {
            anyhow::bail!("legacy v1 BuildKit daemon appeared after it was observed absent");
        }
        (_, None) => {}
    }
    if let Some(daemon) = daemon.as_ref()
        && (daemon.daemon.name != daemon_name
            || daemon.daemon.state_volume != volume_name
            || !daemon.daemon.state.safe_to_reclaim()
            || daemon.daemon.state == crate::docker::client::ContainerState::Removing
            || daemon.daemon.state_running)
    {
        anyhow::bail!("legacy v1 BuildKit daemon is not in a stable reclaimable state");
    }

    let volume_value = inspect_volume(&volume_name)?;
    if listed_volumes.contains(&volume_name) && volume_value.is_none() {
        anyhow::bail!("listed legacy v1 BuildKit state volume could not be inspected");
    }
    if volume_value.is_some() && !listed_volumes.contains(&volume_name) {
        anyhow::bail!("legacy v1 BuildKit state volume appeared after the object inventory");
    }
    let volume = volume_value
        .as_ref()
        .map(|value| {
            parse_legacy_v1_builder_volume(
                value,
                builder,
                &volume_name,
                daemon.as_ref().map(|daemon| &daemon.labels),
            )
        })
        .transpose()?;
    if let Some((daemon, volume)) = daemon.as_ref().zip(volume.as_ref())
        && daemon.daemon.volume_mountpoint != volume.mountpoint
    {
        anyhow::bail!("legacy v1 BuildKit daemon mountpoint changed from its state volume");
    }
    if daemon.is_some() && volume.is_none() {
        anyhow::bail!("legacy v1 BuildKit daemon has no verifiable state volume");
    }
    let users = list_volume_users(&volume_name)?;
    match daemon.as_ref() {
        Some(daemon) if users == [daemon.daemon.id.clone()].into_iter().collect() => {}
        None if users.is_empty() => {}
        _ => anyhow::bail!("legacy v1 BuildKit state volume has an unverified user set"),
    }

    if let Some(daemon) = daemon {
        remove_daemon_by_id(&daemon.daemon.id)
            .context("remove exact label-verified legacy BuildKit daemon ID")?;
        if inspect_daemon(&daemon_name)?.is_some() {
            anyhow::bail!("legacy v1 BuildKit daemon reappeared during removal");
        }
    }
    if volume.is_some() {
        if !list_volume_users(&volume_name)?.is_empty() {
            anyhow::bail!("legacy v1 BuildKit state volume gained a user during removal");
        }
        remove_volume(&volume_name)
            .context("remove exact label-verified legacy BuildKit state volume")?;
        if inspect_volume(&volume_name)?.is_some() {
            anyhow::bail!("legacy v1 BuildKit state volume reappeared during removal");
        }
    }
    if list_daemons()?
        .iter()
        .any(|name| is_potential_builder_daemon_node(name, builder))
    {
        anyhow::bail!("legacy v1 BuildKit daemon remains after removal");
    }
    if list_volumes()?
        .iter()
        .any(|name| is_potential_builder_state_volume(name, builder))
    {
        anyhow::bail!("legacy v1 BuildKit state volume remains after removal");
    }
    Ok(true)
}

fn list_buildkit_container_names() -> Result<Vec<String>> {
    let args = vec![
        "ps".to_string(),
        "--all".to_string(),
        "--format".to_string(),
        "{{.Names}}".to_string(),
    ];
    Ok(crate::docker::client::host_call(&args)?
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn list_buildkit_volume_names() -> Result<Vec<String>> {
    let args = vec![
        "volume".to_string(),
        "ls".to_string(),
        "--format".to_string(),
        "{{.Name}}".to_string(),
    ];
    Ok(crate::docker::client::host_call(&args)?
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn is_builder_daemon_node(name: &str, builder: &str) -> bool {
    if !is_potential_builder_daemon_node(name, builder) {
        return false;
    }
    // Only fixed-width v2 names can safely claim every numeric node suffix:
    // a v1 builder `foo` cannot distinguish its node 10 (`foo10`) from v1
    // builder `foo1`'s node 0. For v1, node 0 is the only exact resource name.
    is_bounded_builder_name(builder) || name == daemon_container_name(builder)
}

fn is_builder_state_volume(name: &str, builder: &str) -> bool {
    name.strip_suffix("_state")
        .is_some_and(|daemon| is_builder_daemon_node(daemon, builder))
}

fn is_potential_builder_daemon_node(name: &str, builder: &str) -> bool {
    name.strip_prefix(&format!("{DAEMON_CONTAINER_PREFIX}{builder}"))
        .is_some_and(|node| {
            !node.is_empty() && node.chars().all(|character| character.is_ascii_digit())
        })
}

fn is_potential_builder_state_volume(name: &str, builder: &str) -> bool {
    name.strip_suffix("_state")
        .is_some_and(|daemon| is_potential_builder_daemon_node(daemon, builder))
}

fn remove_builder_objects_with(
    builder: &str,
    owner_token: &str,
    expected_nodes: &BTreeMap<String, BuilderNodeRecord>,
    mut list_daemons: impl FnMut() -> Result<Vec<String>>,
    mut inspect_daemon: impl FnMut(&str) -> Result<Option<VerifiedBuilderDaemon>>,
    mut remove_daemon_by_id: impl FnMut(&str) -> Result<()>,
    mut list_volumes: impl FnMut() -> Result<Vec<String>>,
    mut inspect_volume: impl FnMut(&str) -> Result<Option<VerifiedBuilderVolume>>,
    mut list_volume_users: impl FnMut(&str) -> Result<BTreeSet<String>>,
    mut remove_volume: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    if !is_bounded_builder_name(builder) {
        // v1 names and records lack exact node identity. Keep them visible as
        // a migration blocker; never reconstruct ownership from their names.
        return Ok(false);
    }
    if !valid_owner_token(owner_token) {
        anyhow::bail!("refuse BuildKit cleanup without a durable owner token");
    }
    let listed_daemons = list_daemons()?;
    let listed_volumes = list_volumes()?;
    for (daemon, identity) in expected_nodes {
        if !is_builder_daemon_node(daemon, builder)
            || !valid_container_id(&identity.container_id)
            || identity.state_volume_mountpoint.is_empty()
        {
            anyhow::bail!("BuildKit owner record has malformed daemon-node identity");
        }
    }

    let listed_daemons = listed_daemons.into_iter().collect::<BTreeSet<_>>();
    let listed_volumes = listed_volumes.into_iter().collect::<BTreeSet<_>>();
    let mut daemon_names = expected_nodes.keys().cloned().collect::<BTreeSet<_>>();
    daemon_names.insert(daemon_container_name(builder));
    for name in &listed_daemons {
        if is_potential_builder_daemon_node(name, builder) {
            if !is_builder_daemon_node(name, builder) {
                return Ok(false);
            }
            daemon_names.insert(name.clone());
        }
    }

    let mut daemon_identities = BTreeMap::new();
    let mut volume_names = expected_nodes
        .keys()
        .map(|daemon| format!("{daemon}_state"))
        .collect::<BTreeSet<_>>();
    volume_names.insert(daemon_state_volume(builder));
    for daemon_name in &daemon_names {
        let daemon = inspect_daemon(daemon_name)?;
        if listed_daemons.contains(daemon_name) && daemon.is_none() {
            // A listed candidate that cannot be tied to this registry's
            // immutable identity blocks the entire deletion pass. Do not
            // remove node 0 first and discover an unrecorded appended node
            // only during the final inventory recheck.
            return Ok(false);
        }
        if let Some(daemon) = daemon {
            let expected = expected_nodes.get(daemon_name);
            if expected.is_none() && daemon_name != &daemon_container_name(builder) {
                // An appended node without a durable immutable ID is not
                // recoverable from its name, even when another node is owned.
                return Ok(false);
            }
            if daemon.name != *daemon_name
                || daemon.state_volume != format!("{daemon_name}_state")
                || !valid_container_id(&daemon.id)
                || daemon.volume_mountpoint.is_empty()
                || !daemon.state.safe_to_reclaim()
                || daemon.state_running
                || expected.is_some_and(|expected| {
                    daemon.id != expected.container_id
                        || daemon.volume_mountpoint != expected.state_volume_mountpoint
                })
            {
                return Ok(false);
            }
            volume_names.insert(daemon.state_volume.clone());
            daemon_identities.insert(daemon_name.clone(), daemon);
        }
    }
    for name in &listed_volumes {
        if is_potential_builder_state_volume(name, builder) {
            if !is_builder_state_volume(name, builder) {
                return Ok(false);
            }
            volume_names.insert(name.clone());
        }
    }

    let mut volume_identities = BTreeMap::new();
    for volume_name in &volume_names {
        let volume = inspect_volume(volume_name)?;
        if listed_volumes.contains(volume_name) && volume.is_none() {
            // As with daemon nodes, require a positive identity proof for
            // every listed state volume before the first destructive call.
            return Ok(false);
        }
        let Some(volume) = volume else {
            continue;
        };
        let daemon_name = volume_name
            .strip_suffix("_state")
            .context("BuildKit state volume name has no suffix")?;
        let expected = expected_nodes.get(daemon_name);
        let daemon = daemon_identities.get(daemon_name);
        if expected.is_none() && daemon_name != daemon_container_name(builder) {
            return Ok(false);
        }
        if volume.name != *volume_name
            || volume.mountpoint.is_empty()
            || expected
                .is_some_and(|expected| volume.mountpoint != expected.state_volume_mountpoint)
            || daemon.is_some_and(|daemon| {
                daemon.state_volume != volume.name || daemon.volume_mountpoint != volume.mountpoint
            })
        {
            return Ok(false);
        }
        let users = list_volume_users(volume_name)?;
        let expected_user = expected
            .map(|expected| expected.container_id.as_str())
            .or_else(|| daemon.map(|daemon| daemon.id.as_str()));
        if expected_user.is_none() && !users.is_empty()
            || expected_user.is_some_and(|expected_user| {
                users
                    .iter()
                    .any(|container_id| container_id != expected_user)
            })
        {
            return Ok(false);
        }
        if daemon.is_some_and(|daemon| {
            expected.is_some_and(|expected| daemon.id != expected.container_id)
        }) {
            return Ok(false);
        }
        volume_identities.insert(volume_name.clone(), volume);
    }

    for (daemon_name, expected) in &daemon_identities {
        let current = inspect_daemon(daemon_name)?;
        let Some(current) = current else {
            continue;
        };
        if current.id != expected.id
            || current.state_volume != expected.state_volume
            || current.volume_mountpoint != expected.volume_mountpoint
            || current.state != expected.state
            || current.state_running
        {
            return Ok(false);
        }
        remove_daemon_by_id(&expected.id)
            .with_context(|| format!("remove BuildKit daemon {}", expected.name))?;
        if inspect_daemon(daemon_name)?.is_some() {
            return Ok(false);
        }
    }

    for (volume_name, expected) in &volume_identities {
        let daemon_name = volume_name
            .strip_suffix("_state")
            .context("BuildKit state volume name has no suffix")?;
        if !expected_nodes.contains_key(daemon_name) && inspect_daemon(daemon_name)?.is_some() {
            // A volume-only interrupted create is removable only while its
            // deterministic daemon is still absent.
            return Ok(false);
        }
        let Some(current) = inspect_volume(volume_name)? else {
            continue;
        };
        if current != *expected || !list_volume_users(volume_name)?.is_empty() {
            return Ok(false);
        }
        remove_volume(volume_name)
            .with_context(|| format!("remove BuildKit state volume {volume_name}"))?;
        if inspect_volume(volume_name)?.is_some() {
            return Ok(false);
        }
    }

    // Metadata survives until the fresh inventories and exact inspect routes
    // prove every recorded node, daemon and state volume is physically absent.
    let daemons_remain = list_daemons()?
        .iter()
        .any(|name| is_potential_builder_daemon_node(name, builder));
    if daemons_remain {
        return Ok(false);
    }
    if list_volumes()?
        .iter()
        .any(|name| is_potential_builder_state_volume(name, builder))
    {
        return Ok(false);
    }
    for daemon_name in daemon_names {
        if inspect_daemon(&daemon_name)?.is_some() {
            return Ok(false);
        }
    }
    for volume_name in volume_names {
        if inspect_volume(&volume_name)?.is_some() {
            return Ok(false);
        }
    }
    Ok(true)
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
    let registry_root = claims_registry_root();
    pressure_prune_builders_with_registry(
        run_root,
        registry_root.as_deref(),
        target_bytes,
        running_container_names,
        builder_disk_usage,
        prune_builder,
        stop_builder_daemon,
        start_builder_daemon,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep pressure-prune tests hermetic"
)]
fn pressure_prune_builders_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    target_bytes: u64,
    mut list_present_containers: impl FnMut() -> Result<BTreeSet<String>>,
    mut disk_usage: impl FnMut(&str) -> Result<u64>,
    mut prune: impl FnMut(&str) -> Result<u64>,
    mut stop: impl FnMut(&str) -> Result<bool>,
    mut start: impl FnMut(&str) -> Result<bool>,
) -> PressurePruneReport {
    let mut report = PressurePruneReport::default();
    if target_bytes == 0 {
        return report;
    }
    let Some(registry_root) = registry_root else {
        report.failures.push(
            "skip BuildKit pressure prune: durable owner registry is unavailable".to_string(),
        );
        return report;
    };
    let mut owner_report = HorizonReport::default();
    let owner_builders = registered_owner_builders(registry_root, &mut owner_report);
    report.failures.extend(owner_report.failures);
    for builder in owner_builders
        .iter()
        .filter(|builder| is_legacy_v1_builder_name(builder))
    {
        report.failures.push(format!(
            "skip BuildKit pressure prune for legacy builder {builder}: retain it until horizon cleanup proves exact daemon and state-volume ownership"
        ));
    }
    let mut present = match list_present_containers() {
        Ok(present) => present,
        Err(error) => {
            report
                .failures
                .push(format!("list containers for claim repair: {error:#}"));
            return report;
        }
    };
    let active_job_container = contains_active_job_or_service(&present);
    let mut persistent = Vec::new();
    for builder in owner_builders
        .into_iter()
        .filter(|builder| is_persistent_builder_name(builder))
    {
        match claim_file_missing(&claims_file(run_root, &builder)) {
            Ok(true) if active_job_container => {
                report.failures.push(format!(
                    "skip BuildKit builder {builder}: missing /run claim is not empty while a job container is active"
                ));
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                report.failures.push(format!(
                    "skip BuildKit builder {builder}: inspect claim path failed ({error:#})"
                ));
                continue;
            }
        }
        match read_claims_for_reaping(
            &claims_file(run_root, &builder),
            &builder,
            Some(registry_root),
            !active_job_container,
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
    persistent.retain(|builder| {
        match claim_file_missing(&claims_file(run_root, builder)) {
            Ok(true) if active_job_container => {
                report.failures.push(format!(
                    "skip BuildKit builder {builder}: missing /run claim is not empty while a job container is active"
                ));
                false
            }
            Ok(_) => true,
            Err(error) => {
                report.failures.push(format!(
                    "skip BuildKit builder {builder}: inspect claim path failed ({error:#})"
                ));
                false
            }
        }
    });
    if persistent.is_empty() {
        return report;
    }
    // Largest first: one unlocked du per builder as an ordering hint, then
    // prune in that order. Unmeasurable builders (stopped daemons refuse du)
    // sort last; the prune still measures them after starting. The hint is
    // never a decision: the holder check below runs under each builder's
    // claim lock, then the prune and stop run unlocked with a recheck that
    // restarts the daemon when a setup raced.
    let mut sized: Vec<(String, Option<u64>)> = Vec::new();
    for builder in &persistent {
        match disk_usage(builder) {
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
            let allow_missing_claim = match claim_file_missing(&path) {
                Ok(true) => match list_present_containers() {
                    Ok(current_present) => {
                        let active = contains_active_job_or_service(&current_present);
                        present = current_present;
                        if active {
                            report.failures.push(format!(
                                "skip BuildKit builder {builder}: missing /run claim is not empty while a job container is active"
                            ));
                            continue;
                        }
                        true
                    }
                    Err(error) => {
                        report.failures.push(format!(
                            "skip BuildKit builder {builder}: cannot prove job quiescence ({error:#})"
                        ));
                        continue;
                    }
                },
                Ok(false) => false,
                Err(error) => {
                    report.failures.push(format!(
                        "skip BuildKit builder {builder}: inspect claim path failed ({error:#})"
                    ));
                    continue;
                }
            };
            let mut claims = match read_claims_for_reaping(
                &path,
                &builder,
                Some(registry_root),
                allow_missing_claim,
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
            repair_absent_unlocked(&mut claims, &present);
            if write_claims(&path, &claims).is_err() {
                report
                    .failures
                    .push(format!("repair claims for {builder}: write failed"));
                continue;
            }
            claims.holders.is_empty() && claims.releasing.is_none()
        };
        if !unclaimed {
            continue;
        }
        // Prune while running, then stop: buildx refuses both du and prune
        // on a stopped daemon, so stop-first would prune nothing.
        let prune_started = Instant::now();
        match prune(&builder) {
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
        if let Err(error) = stop(&builder) {
            report
                .failures
                .push(format!("stop builder {builder}: {error:#}"));
        }
        let raced = match holders_remain(&path, &builder, &mut list_present_containers) {
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
            if let Err(error) = start(&builder) {
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

fn registered_owner_builders(registry_root: &Path, report: &mut HorizonReport) -> BTreeSet<String> {
    let entries = match std::fs::read_dir(registry_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeSet::new(),
        Err(error) => {
            report.failures.push(format!(
                "list durable BuildKit owner records under {}: {error:#}",
                registry_root.display()
            ));
            return BTreeSet::new();
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
    let mut builders = BTreeSet::new();
    for path in paths {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                report
                    .failures
                    .push(format!("stat owner record {}: {error:#}", path.display()));
                continue;
            }
        };
        if !metadata.file_type().is_file() {
            report
                .failures
                .push(format!("keep non-regular owner record {}", path.display()));
            continue;
        }
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
        if record.version == 1 {
            let legacy: LegacyV1BuilderOwnerRecord = match serde_json::from_slice(&bytes) {
                Ok(legacy) => legacy,
                Err(error) => {
                    report.failures.push(format!(
                        "retain legacy BuildKit owner record {}: v1 identity is unreadable ({error:#})",
                        path.display()
                    ));
                    continue;
                }
            };
            if !is_legacy_v1_builder_name(&legacy.builder)
                || owner_registry_file(registry_root, &legacy.builder) != path
            {
                report.failures.push(format!(
                    "retain legacy BuildKit owner record {}: builder name or registry path is not an exact v1 identity",
                    path.display()
                ));
                continue;
            }
            builders.insert(legacy.builder);
            continue;
        }
        if record.version != OWNER_REGISTRY_VERSION {
            report.failures.push(format!(
                "BuildKit owner record {} has schema version {}; operator migration is required before cleanup",
                path.display(),
                record.version
            ));
            continue;
        }
        if !is_owner_registry_builder_name(&record.builder)
            || owner_registry_file(registry_root, &record.builder) != path
        {
            report.failures.push(format!(
                "keep mismatched BuildKit owner record {}",
                path.display()
            ));
            continue;
        }
        if !owner_record_identity_is_valid(&record) {
            report.failures.push(format!(
                "BuildKit owner record {} has incomplete or malformed v2 identity; operator migration is required before cleanup",
                path.display()
            ));
            continue;
        }
        builders.insert(record.builder);
    }
    builders
}

fn report_unowned_reserved_buildkit_names(
    registry_root: &Path,
    listed_daemons: &[String],
    listed_volumes: &[String],
    report: &mut HorizonReport,
) {
    let mut ignored_report = HorizonReport::default();
    let builders = registered_owner_builders(registry_root, &mut ignored_report);
    let mut known_daemons = BTreeSet::new();
    let mut known_volumes = BTreeSet::new();
    for builder in builders {
        if is_legacy_v1_builder_name(&builder) {
            known_daemons.insert(daemon_container_name(&builder));
            known_volumes.insert(daemon_state_volume(&builder));
            continue;
        }
        let Ok(Some(record)) = read_owner_record(registry_root, &builder) else {
            continue;
        };
        known_daemons.insert(daemon_container_name(&builder));
        known_volumes.insert(daemon_state_volume(&builder));
        for daemon in record.daemon_nodes.keys() {
            known_daemons.insert(daemon.clone());
            known_volumes.insert(format!("{daemon}_state"));
        }
    }
    for daemon in listed_daemons {
        if is_persistent_builder_object(daemon) && !known_daemons.contains(daemon) {
            report.failures.push(format!(
                "retain reserved BuildKit daemon {daemon}: no exact current or v1 owner row authorizes cleanup"
            ));
        }
    }
    for volume in listed_volumes {
        if is_persistent_builder_object(volume) && !known_volumes.contains(volume) {
            report.failures.push(format!(
                "retain reserved BuildKit state volume {volume}: no exact current or v1 owner row authorizes cleanup"
            ));
        }
    }
}

fn claim_file_missing(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(false),
        Ok(_) => anyhow::bail!("claim path {} is not a regular file", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

fn contains_active_job_or_service(present: &BTreeSet<String>) -> bool {
    present.iter().any(|name| {
        name.starts_with(crate::docker_lease::JOB_CONTAINER_NAME_PREFIX)
            || name.starts_with("velnor-service-")
    })
}

/// Locked holder recheck after unlocked Docker work. A missing `/run` claim
/// is not empty while any job container is active; refresh the running list
/// under the lifecycle guard before allowing cleanup to proceed. Torn claim
/// files also fail closed as held.
fn holders_remain(
    path: &Path,
    builder: &str,
    list_present_containers: &mut impl FnMut() -> Result<BTreeSet<String>>,
) -> Result<bool> {
    let _lock = lock_claims(builder, path)?;
    if claim_file_missing(path)? && contains_active_job_or_service(&list_present_containers()?) {
        return Ok(true);
    }
    match read_claims(path) {
        Ok(claims) => Ok(!claims.holders.is_empty() || claims.releasing.is_some()),
        Err(error) => {
            log_torn_claims(builder, path, &error);
            Ok(true)
        }
    }
}

/// A `Created` container has no `FinishedAt`. Reclaim it only when both the
/// exact Engine creation time and an immutable durable creation/bootstrap
/// timestamp are past the idle horizon, its bootstrap proof never advanced
/// beyond creation, and the claim pass already proved there are no holders.
/// The Engine time covers the create-response/owner-ID crash window; the
/// durable timestamp never changes when a later job claims the builder.
fn created_builder_idle_age(
    builder: &str,
    registry_root: &Path,
    observed_id: &str,
    created: Option<SystemTime>,
    now: SystemTime,
    report: &mut HorizonReport,
) -> Option<Duration> {
    if !is_bounded_builder_name(builder) {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: builder name is not canonical"
        ));
        return None;
    }
    if !valid_container_id(observed_id) {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: Engine immutable ID is invalid"
        ));
        return None;
    }
    let record = match read_owner_record(registry_root, builder) {
        Ok(Some(record)) => record,
        Ok(None) => {
            report.failures.push(format!(
                "leave Created BuildKit daemon {builder} untouched: durable owner record disappeared"
            ));
            return None;
        }
        Err(error) => {
            report.failures.push(format!(
                "leave Created BuildKit daemon {builder} untouched: owner record is unreadable ({error:#})"
            ));
            return None;
        }
    };
    let daemon_name = daemon_container_name(builder);
    if record.daemon_nodes.iter().any(|(name, node)| {
        name != &daemon_name
            || !matches!(
                node.bootstrap_phase,
                BuilderBootstrapPhase::Unverified | BuilderBootstrapPhase::Created
            )
    }) {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: durable bootstrap phase conflicts with Engine state"
        ));
        return None;
    }
    let durable_created_unix = match record.daemon_nodes.get(&daemon_name) {
        Some(node) if node.container_id == observed_id => node.created_unix,
        Some(_) => {
            report.failures.push(format!(
                "leave Created BuildKit daemon {builder} untouched: durable daemon ID does not match the Engine observation"
            ));
            return None;
        }
        None => record.registered_unix,
    };
    let Some(created) = created else {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: Engine creation time is unavailable"
        ));
        return None;
    };
    if durable_created_unix == 0 {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: durable creation/bootstrap timestamp is missing"
        ));
        return None;
    }
    let Some(durable_created) =
        std::time::UNIX_EPOCH.checked_add(Duration::from_secs(durable_created_unix))
    else {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: durable creation/bootstrap timestamp is invalid"
        ));
        return None;
    };
    let Some(container_age) = now.duration_since(created).ok() else {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: Engine creation time is in the future"
        ));
        return None;
    };
    let Some(bootstrap_age) = now.duration_since(durable_created).ok() else {
        report.failures.push(format!(
            "leave Created BuildKit daemon {builder} untouched: durable creation/bootstrap time is in the future"
        ));
        return None;
    };
    Some(container_age.min(bootstrap_age))
}

fn legacy_v1_created_idle_age(
    builder: &str,
    registry_root: &Path,
    created: Option<SystemTime>,
    now: SystemTime,
    report: &mut HorizonReport,
) -> Option<Duration> {
    if !is_legacy_v1_builder_name(builder) {
        report.failures.push(format!(
            "leave legacy Created BuildKit daemon {builder} untouched: builder name is not exact v1"
        ));
        return None;
    }
    match read_legacy_v1_owner_record(registry_root, builder) {
        Ok(Some(_)) => {}
        Ok(None) => {
            report.failures.push(format!(
                "leave legacy Created BuildKit daemon {builder} untouched: v1 owner row is absent"
            ));
            return None;
        }
        Err(error) => {
            report.failures.push(format!(
                "leave legacy Created BuildKit daemon {builder} untouched: v1 owner row is unreadable ({error:#})"
            ));
            return None;
        }
    }
    let Some(created) = created else {
        report.failures.push(format!(
            "leave legacy Created BuildKit daemon {builder} untouched: Engine creation time is unavailable"
        ));
        return None;
    };
    match now.duration_since(created) {
        Ok(age) => Some(age),
        Err(_) => {
            report.failures.push(format!(
                "leave legacy Created BuildKit daemon {builder} untouched: Engine creation time is in the future"
            ));
            None
        }
    }
}

/// Converge owned builders under the filesystem-wide lifecycle lock. Current
/// names require a matching durable owner record or readable claim. Missing
/// `/run` state is recoverable only after a fresh running-job scan proves the
/// host quiescent. Current names come from durable owner records because the
/// host Buildx registry is not the job containers' registry.
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
    let mut report = reap_idle_builders_with_registry(
        run_root,
        registry_root.as_deref(),
        now,
        running_container_names,
        |daemon| crate::docker::Docker::host().inspect_exit(daemon),
        |builder| {
            if is_legacy_v1_builder_name(builder) {
                stop_legacy_v1_builder_daemon(builder)
            } else {
                stop_builder_daemon(builder)
            }
        },
        |builder| {
            if is_legacy_v1_builder_name(builder) {
                start_legacy_v1_builder_daemon(builder)
            } else {
                start_builder_daemon(builder)
            }
        },
        |builder, observed| remove_builder_after_reap_observation(builder, observed),
    );
    if let Some(registry_root) = registry_root.as_deref() {
        match (
            list_buildkit_container_names(),
            list_buildkit_volume_names(),
        ) {
            (Ok(daemons), Ok(volumes)) => report_unowned_reserved_buildkit_names(
                registry_root,
                &daemons,
                &volumes,
                &mut report,
            ),
            (Err(error), _) | (_, Err(error)) => report.failures.push(format!(
                "retain unregistered reserved BuildKit objects: Engine inventory failed ({error:#})"
            )),
        }
    }
    report
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper paths hermetic in tests"
)]
fn reap_idle_builders_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    now: SystemTime,
    mut list_present_containers: impl FnMut() -> Result<BTreeSet<String>>,
    mut inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    mut stop: impl FnMut(&str) -> Result<bool>,
    mut start: impl FnMut(&str) -> Result<bool>,
    mut remove: impl FnMut(&str, Option<&crate::docker::client::ExitInfo>) -> Result<bool>,
) -> HorizonReport {
    let mut report = HorizonReport::default();
    let builders = registry_root
        .map(|registry_root| registered_owner_builders(registry_root, &mut report))
        .unwrap_or_default();
    if builders.is_empty() {
        return report;
    }
    let present = match list_present_containers() {
        Ok(present) => present,
        Err(error) => {
            report
                .failures
                .push(format!("list containers for claim repair: {error:#}"));
            return report;
        }
    };
    let active_job_container = contains_active_job_or_service(&present);
    for builder in builders {
        let legacy_v1 = is_legacy_v1_builder_name(&builder);
        if !legacy_v1 && !is_persistent_builder_name(&builder) {
            continue;
        }
        let path = claims_file(run_root, &builder);
        if legacy_v1 && active_job_container {
            report.failures.push(format!(
                "leave legacy v1 BuildKit builder {builder} untouched: exact-owner cleanup requires host-wide job and service quiescence"
            ));
            continue;
        }
        let missing_claim = match claim_file_missing(&path) {
            Ok(missing) => missing,
            Err(error) => {
                report
                    .failures
                    .push(format!("stat ownership for {builder}: {error:#}"));
                continue;
            }
        };
        if missing_claim && active_job_container {
            report.failures.push(format!(
                "leave BuildKit builder {builder} untouched: a missing /run claim is not empty while a job container is active"
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
            let mut active_jobs_now = active_job_container;
            let mut repair_present = present.clone();
            let missing_now = match claim_file_missing(&path) {
                Ok(missing) => missing,
                Err(error) => {
                    report.failures.push(format!(
                        "leave BuildKit builder {builder} untouched: stat claims failed ({error:#})"
                    ));
                    continue;
                }
            };
            if legacy_v1 || missing_now {
                let current_present = match list_present_containers() {
                    Ok(present) => present,
                    Err(error) => {
                        report.failures.push(format!(
                            "leave BuildKit builder {builder} untouched: cannot prove job quiescence ({error:#})"
                        ));
                        continue;
                    }
                };
                active_jobs_now = contains_active_job_or_service(&current_present);
                if active_jobs_now {
                    let reason = if legacy_v1 {
                        "host-wide job quiescence is not proven"
                    } else {
                        "a missing /run claim is not empty while a job container is active"
                    };
                    report.failures.push(format!(
                        "leave BuildKit builder {builder} untouched: {reason}"
                    ));
                    continue;
                }
                repair_present = current_present;
            }
            let mut claims = match read_claims_for_reaping(
                &path,
                &builder,
                registry_root,
                !active_jobs_now,
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
                && is_owner_registry_builder_name(&builder)
                && let Err(error) = ensure_owner_record(registry_root, &builder, None)
            {
                report
                    .failures
                    .push(format!("ensure durable ownership for {builder}: {error:#}"));
                continue;
            }
            repair_absent_unlocked(&mut claims, &repair_present);
            if (!legacy_v1 || !missing_now) && write_claims(&path, &claims).is_err() {
                report
                    .failures
                    .push(format!("repair claims for {builder}: write failed"));
                continue;
            }
            claims.holders.is_empty() && claims.releasing.is_none()
        };
        if !unclaimed {
            continue;
        }
        let daemon = daemon_container_name(&builder);
        let exit = match inspect_exit(&daemon) {
            Ok(exit) => exit,
            Err(error) if crate::docker::client::is_not_found(&error) => {
                // Daemon gone but the builder registration lingers (a
                // `buildx rm --keep-state` past, or a crashed delete):
                // recheck, then remove the registration, any orphaned
                // volume, and the claim file.
                match holders_remain(&path, &builder, &mut list_present_containers) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        report
                            .failures
                            .push(format!("relock claims for {builder}: {error:#}"));
                        continue;
                    }
                }
                let deleted = if legacy_v1 {
                    registry_root
                        .map(|registry_root| {
                            delete_legacy_v1_builder(
                                run_root,
                                registry_root,
                                &builder,
                                None,
                                &mut remove,
                            )
                        })
                        .unwrap_or_else(|| Ok(false))
                } else {
                    delete_registered_builder(run_root, registry_root, &builder, None, &mut remove)
                };
                match deleted {
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
        let Some(state) = exit.status else {
            report.failures.push(format!(
                "leave BuildKit builder {builder} untouched: Engine returned an unknown container state"
            ));
            continue;
        };
        if !state.safe_to_reclaim() {
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
            match holders_remain(&path, &builder, &mut list_present_containers) {
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
                }
                Ok(false) => {}
                Err(error) => {
                    report
                        .failures
                        .push(format!("relock claims for {builder}: {error:#}"));
                }
            }
            continue;
        }
        let idle = match state {
            crate::docker::client::ContainerState::Created => {
                let Some(id) = exit.id.as_deref().filter(|id| valid_container_id(id)) else {
                    report.failures.push(format!(
                        "leave Created BuildKit daemon {builder} untouched: Engine immutable ID is unavailable"
                    ));
                    continue;
                };
                registry_root.and_then(|registry_root| {
                    if legacy_v1 {
                        legacy_v1_created_idle_age(
                            &builder,
                            registry_root,
                            exit.created,
                            now,
                            &mut report,
                        )
                    } else {
                        created_builder_idle_age(
                            &builder,
                            registry_root,
                            id,
                            exit.created,
                            now,
                            &mut report,
                        )
                    }
                })
            }
            crate::docker::client::ContainerState::Exited
            | crate::docker::client::ContainerState::Dead => exit
                .finished
                .and_then(|finished| now.duration_since(finished).ok()),
            crate::docker::client::ContainerState::Removing => {
                report.failures.push(format!(
                    "leave BuildKit daemon {builder} untouched: Engine removal is already in progress"
                ));
                continue;
            }
            crate::docker::client::ContainerState::Running
            | crate::docker::client::ContainerState::Restarting
            | crate::docker::client::ContainerState::Paused => {
                unreachable!("non-reclaimable Engine states were stopped above")
            }
        };
        match idle {
            // No stop time (running zero-time, unparseable): cannot prove
            // idleness, so never delete.
            None => {}
            Some(idle) if idle < IDLE_DELETE_AFTER => {}
            Some(_) => {
                if exit.id.as_deref().is_none_or(|id| !valid_container_id(id)) {
                    report.failures.push(format!(
                        "leave BuildKit daemon {builder} untouched: Engine immutable ID is unavailable"
                    ));
                    continue;
                }
                match holders_remain(&path, &builder, &mut list_present_containers) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        report
                            .failures
                            .push(format!("relock claims for {builder}: {error:#}"));
                        continue;
                    }
                }
                let deleted = if legacy_v1 {
                    registry_root
                        .map(|registry_root| {
                            delete_legacy_v1_builder(
                                run_root,
                                registry_root,
                                &builder,
                                Some(&exit),
                                &mut remove,
                            )
                        })
                        .unwrap_or_else(|| Ok(false))
                } else {
                    delete_registered_builder(
                        run_root,
                        registry_root,
                        &builder,
                        Some(&exit),
                        &mut remove,
                    )
                };
                match deleted {
                    Ok(true) => report.deleted.push(builder.clone()),
                    Ok(false) => {}
                    Err(error) => report
                        .failures
                        .push(format!("delete builder {builder}: {error:#}")),
                }
            }
        }
    }
    report
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
    #[cfg(test)]
    if let Some(names) = TEST_RUNNING_CONTAINER_NAMES.with(|current| current.borrow().clone()) {
        return Ok(names);
    }
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

    fn test_owner_group() -> BuilderGroup {
        BuilderGroup::new("trusted", TRUST_TIER_BRANCH, Some("o/r"))
    }

    fn ensure_test_owner_record(registry_root: &Path, builder: &str) -> String {
        ensure_owner_record(registry_root, builder, Some(&test_owner_group())).unwrap()
    }

    fn setup_test_bootstrap_node(
        name: &str,
        phase: BuilderBootstrapPhase,
    ) -> (
        PathBuf,
        TestStorageLayoutGuard,
        String,
        String,
        String,
        String,
    ) {
        let root = temp_root(name);
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let run_root = layout.run_root.clone();
        let registry_root = owner_registry_root(&layout.lib_root);
        let _layout_guard = use_test_storage_layout(layout);
        let builder = test_builder();
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-bootstrap",
            "velnor-job-bootstrap",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        let mut record = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        let owner_token = record.owner_token.clone().unwrap();
        let id = "b".repeat(64);
        let config_sha256 = "c".repeat(64);
        record.daemon_nodes.insert(
            daemon_container_name(&builder),
            BuilderNodeRecord {
                container_id: id.clone(),
                state_volume_mountpoint: format!(
                    "/var/lib/docker/volumes/{}_state/_data",
                    daemon_container_name(&builder)
                ),
                created_unix: unix_now(),
                bootstrap_phase: phase,
                config_sha256: Some(config_sha256.clone()),
            },
        );
        write_atomic_document(
            &owner_registry_file(&registry_root, &builder),
            &serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        (root, _layout_guard, builder, owner_token, id, config_sha256)
    }

    #[test]
    fn admitted_builder_bootstrap_transitions_are_durable_and_sequential() {
        let (root, _layout, builder, owner_token, id, digest) =
            setup_test_bootstrap_node("bootstrap-sequential", BuilderBootstrapPhase::Created);
        let phases = [
            (
                BuilderBootstrapPhase::Created,
                BuilderBootstrapPhase::Archived,
            ),
            (
                BuilderBootstrapPhase::Archived,
                BuilderBootstrapPhase::Started,
            ),
            (BuilderBootstrapPhase::Started, BuilderBootstrapPhase::Ready),
        ];
        for (expected, next) in phases {
            transition_admitted_builder_bootstrap(
                &builder,
                &owner_token,
                &id,
                expected,
                next,
                &digest,
            )
            .unwrap();
            assert_eq!(
                registered_owner_bootstrap_state(&builder).unwrap(),
                Some((id.clone(), next, Some(digest.clone())))
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_builder_create_persists_created_phase_and_approved_config_digest() {
        let root = temp_root("bootstrap-created-persistence");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let run_root = layout.run_root.clone();
        let registry_root = owner_registry_root(&layout.lib_root);
        let _layout_guard = use_test_storage_layout(layout);
        let builder = test_builder();
        let job_container = "velnor-job-bootstrap";
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-bootstrap",
            job_container,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap()
            .daemon_nodes
            .is_empty());
        let owner_token = owner_token_from_registry(&registry_root, &builder).unwrap();
        let daemon_name = daemon_container_name(&builder);
        let id = "b".repeat(64);
        let config_sha256 = "c".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &id, &mountpoint);

        record_admitted_builder_container_created_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &daemon_name,
            job_container,
            &id,
            &config_sha256,
            |name| {
                assert_eq!(name, daemon_name);
                Ok(Some(container.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                Ok(Some(volume.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                Ok([id.clone()].into_iter().collect())
            },
        )
        .unwrap();

        assert_eq!(
            registered_owner_bootstrap_state(&builder).unwrap(),
            Some((id, BuilderBootstrapPhase::Created, Some(config_sha256)))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_running_verification_is_phase_agnostic_and_checks_engine_state() {
        let (root, _layout, builder, owner_token, id, _digest) =
            setup_test_bootstrap_node("bootstrap-running", BuilderBootstrapPhase::Started);
        let daemon_name = daemon_container_name(&builder);
        let mountpoint = format!(
            "/var/lib/docker/volumes/{}_state/_data",
            daemon_container_name(&builder)
        );
        let (mut container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &id, &mountpoint);
        container["State"]["Running"] = serde_json::Value::Bool(true);
        let run_root = claims_run_root().unwrap();
        let registry_root = claims_registry_root().unwrap();
        let daemon = verify_admitted_builder_daemon_details_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &id,
            |name| {
                assert_eq!(name, daemon_name);
                Ok(Some(container.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                Ok(Some(volume.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                Ok([id.clone()].into_iter().collect())
            },
        )
        .unwrap();
        assert!(daemon.state_running);
        assert_eq!(
            registered_owner_bootstrap_state(&builder)
                .unwrap()
                .unwrap()
                .1,
            BuilderBootstrapPhase::Started
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_builder_bootstrap_rejects_wrong_owner_token_and_immutable_id() {
        let (root, _layout, builder, owner_token, id, digest) =
            setup_test_bootstrap_node("bootstrap-identity", BuilderBootstrapPhase::Created);
        let wrong_token = "d".repeat(32);
        let wrong_token_error = transition_admitted_builder_bootstrap(
            &builder,
            &wrong_token,
            &id,
            BuilderBootstrapPhase::Created,
            BuilderBootstrapPhase::Archived,
            &digest,
        )
        .unwrap_err();
        assert!(format!("{wrong_token_error:#}").contains("owner token"));

        let wrong_id = "e".repeat(64);
        let wrong_id_error = transition_admitted_builder_bootstrap(
            &builder,
            &owner_token,
            &wrong_id,
            BuilderBootstrapPhase::Created,
            BuilderBootstrapPhase::Archived,
            &digest,
        )
        .unwrap_err();
        assert!(format!("{wrong_id_error:#}").contains("daemon ID changed"));
        assert_eq!(
            registered_owner_bootstrap_state(&builder).unwrap(),
            Some((id, BuilderBootstrapPhase::Created, Some(digest)))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_builder_node_rows_are_unverified_but_remain_reapable() {
        let root = temp_root("bootstrap-old-node-row");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let _layout_guard = use_test_storage_layout(layout.clone());
        let registry_root = owner_registry_root(&layout.lib_root);
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let id = "f".repeat(64);
        let daemon_name = daemon_container_name(&builder);

        // A v2 record written before bootstrap tracking has only the two
        // identity fields in each node. Its missing phase must never be
        // inferred as Ready.
        let path = owner_registry_file(&registry_root, &builder);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut old_node_rows = serde_json::Map::new();
        old_node_rows.insert(
            daemon_name.clone(),
            serde_json::json!({
                "container_id": id.clone(),
                "state_volume_mountpoint": "/var/lib/docker/volumes/legacy_state/_data"
            }),
        );
        value["daemon_nodes"] = serde_json::Value::Object(old_node_rows);
        write_atomic_document(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let record = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        let node = record.daemon_nodes.get(&daemon_name).unwrap();
        assert_eq!(node.bootstrap_phase, BuilderBootstrapPhase::Unverified);
        assert_eq!(node.config_sha256, None);
        assert!(owner_record_identity_is_valid(&record));
        assert_eq!(read_owner_records(&registry_root).unwrap().len(), 1);
        assert_eq!(
            registered_owner_bootstrap_state(&builder).unwrap(),
            Some((id, BuilderBootstrapPhase::Unverified, None))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn interrupted_builder_bootstrap_stays_unready_and_cannot_skip_phases() {
        let (root, _layout, builder, owner_token, id, digest) =
            setup_test_bootstrap_node("bootstrap-interrupted", BuilderBootstrapPhase::Started);
        let interrupted = registered_owner_bootstrap_state(&builder).unwrap().unwrap();
        assert_eq!(interrupted.1, BuilderBootstrapPhase::Started);
        assert_ne!(interrupted.1, BuilderBootstrapPhase::Ready);

        let skipped = transition_admitted_builder_bootstrap(
            &builder,
            &owner_token,
            &id,
            BuilderBootstrapPhase::Created,
            BuilderBootstrapPhase::Ready,
            &digest,
        )
        .unwrap_err();
        assert!(format!("{skipped:#}").contains("invalid admitted BuildKit bootstrap transition"));
        assert_eq!(
            registered_owner_bootstrap_state(&builder).unwrap(),
            Some((id, BuilderBootstrapPhase::Started, Some(digest)))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn test_verified_node(
        name: &str,
        id_byte: char,
    ) -> (
        BuilderNodeRecord,
        VerifiedBuilderDaemon,
        VerifiedBuilderVolume,
    ) {
        let id = id_byte.to_string().repeat(64);
        let state_volume = format!("{name}_state");
        let mountpoint = format!("/var/lib/docker/volumes/{state_volume}/_data");
        (
            BuilderNodeRecord {
                container_id: id.clone(),
                state_volume_mountpoint: mountpoint.clone(),
                created_unix: unix_now(),
                bootstrap_phase: BuilderBootstrapPhase::Unverified,
                config_sha256: None,
            },
            VerifiedBuilderDaemon {
                id,
                name: name.to_owned(),
                state_volume: state_volume.clone(),
                volume_mountpoint: mountpoint.clone(),
                state_running: false,
                state: crate::docker::client::ContainerState::Exited,
            },
            VerifiedBuilderVolume {
                name: state_volume,
                mountpoint,
            },
        )
    }

    fn fake_inspected_buildkit_objects(
        builder: &str,
        owner_token: &str,
        id: &str,
        mountpoint: &str,
    ) -> (serde_json::Value, serde_json::Value) {
        fake_inspected_buildkit_objects_with_state(builder, owner_token, id, mountpoint, "exited")
    }

    fn fake_inspected_buildkit_objects_with_state(
        builder: &str,
        owner_token: &str,
        id: &str,
        mountpoint: &str,
        status: &str,
    ) -> (serde_json::Value, serde_json::Value) {
        let daemon_name = daemon_container_name(builder);
        let volume_name = daemon_state_volume(builder);
        let labels = [
            (
                crate::docker_lease::BUILDKIT_BUILDER_LABEL.to_string(),
                serde_json::Value::String(builder.to_string()),
            ),
            (
                crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL.to_string(),
                serde_json::Value::String(owner_token.to_string()),
            ),
        ]
        .into_iter()
        .collect::<serde_json::Map<_, _>>();
        let container = serde_json::json!({
            "Id": id,
            "Name": format!("/{daemon_name}"),
            "Config": { "Labels": labels.clone() },
            "Mounts": [{
                "Type": "volume",
                "Name": volume_name,
                "Destination": "/var/lib/buildkit",
                "Source": mountpoint,
            }],
            "State": { "Running": false, "Status": status },
        });
        let volume = serde_json::json!({
            "Name": volume_name,
            "Driver": "local",
            "Labels": labels,
            "Mountpoint": mountpoint,
        });
        (container, volume)
    }

    fn fake_legacy_v1_buildkit_objects(
        builder: &str,
        id: &str,
        job_id: &str,
        daemon_id: &str,
        mountpoint: &str,
        status: &str,
    ) -> (serde_json::Value, serde_json::Value) {
        let daemon_name = daemon_container_name(builder);
        let volume_name = daemon_state_volume(builder);
        let labels = [
            (
                crate::docker_lease::JOB_ID_LABEL.to_string(),
                serde_json::Value::String(job_id.to_string()),
            ),
            (
                crate::docker_lease::DAEMON_ID_LABEL.to_string(),
                serde_json::Value::String(daemon_id.to_string()),
            ),
        ]
        .into_iter()
        .collect::<serde_json::Map<_, _>>();
        let container = serde_json::json!({
            "Id": id,
            "Name": format!("/{daemon_name}"),
            "Config": { "Labels": labels.clone() },
            "Mounts": [{
                "Type": "volume",
                "Name": volume_name,
                "Destination": "/var/lib/buildkit",
                "Source": mountpoint,
            }],
            "State": { "Running": false, "Status": status },
        });
        let volume = serde_json::json!({
            "Name": volume_name,
            "Driver": "local",
            "Labels": labels,
            "Mountpoint": mountpoint,
        });
        (container, volume)
    }

    fn remove_fake_builder_objects(
        builder: &str,
        owner_token: &str,
        expected_nodes: &BTreeMap<String, BuilderNodeRecord>,
        daemons: &std::cell::RefCell<BTreeMap<String, serde_json::Value>>,
        volumes: &std::cell::RefCell<BTreeMap<String, serde_json::Value>>,
        observed: Option<&crate::docker::client::ExitInfo>,
    ) -> Result<bool> {
        let list_daemons = || Ok(daemons.borrow().keys().cloned().collect());
        let inspect_daemon = |name: &str| {
            let daemon_values = daemons.borrow();
            let volume_values = volumes.borrow();
            let mut inspect_container =
                |requested: &str| -> Result<_> { Ok(daemon_values.get(requested).cloned()) };
            let mut inspect_volume =
                |requested: &str| -> Result<_> { Ok(volume_values.get(requested).cloned()) };
            let mut volume_users = |volume: &str| -> Result<BTreeSet<String>> {
                Ok(daemon_values
                    .iter()
                    .filter_map(|(_, value)| {
                        let mounts = value.get("Mounts")?.as_array()?;
                        mounts
                            .iter()
                            .any(|mount| {
                                mount.get("Name").and_then(serde_json::Value::as_str)
                                    == Some(volume)
                            })
                            .then(|| value.get("Id")?.as_str().map(str::to_owned))
                            .flatten()
                    })
                    .collect())
            };
            inspect_admitted_builder_daemon_with(
                builder,
                owner_token,
                name,
                expected_nodes.get(name),
                &mut inspect_container,
                &mut inspect_volume,
                &mut volume_users,
            )
        };
        let remove_daemon = |container_id: &str| {
            let name = daemons
                .borrow()
                .iter()
                .find(|(_, value)| {
                    value.get("Id").and_then(serde_json::Value::as_str) == Some(container_id)
                })
                .map(|(name, _)| name.clone())
                .context("test daemon ID missing")?;
            daemons.borrow_mut().remove(&name);
            Ok(())
        };
        let list_volumes = || Ok(volumes.borrow().keys().cloned().collect());
        let inspect_volume = |name: &str| {
            let Some(value) = volumes.borrow().get(name).cloned() else {
                return Ok(None);
            };
            parse_verified_builder_volume(&value, builder, owner_token, name).map(Some)
        };
        let list_volume_users = |volume: &str| {
            Ok(daemons
                .borrow()
                .values()
                .filter_map(|value| {
                    let mounts = value.get("Mounts")?.as_array()?;
                    mounts
                        .iter()
                        .any(|mount| {
                            mount.get("Name").and_then(serde_json::Value::as_str) == Some(volume)
                        })
                        .then(|| value.get("Id")?.as_str().map(str::to_owned))
                        .flatten()
                })
                .collect())
        };
        let remove_volume = |volume: &str| {
            volumes.borrow_mut().remove(volume);
            Ok(())
        };
        match observed {
            Some(observed) => remove_builder_objects_after_reap_with(
                builder,
                owner_token,
                expected_nodes,
                Some(observed),
                list_daemons,
                inspect_daemon,
                remove_daemon,
                list_volumes,
                inspect_volume,
                list_volume_users,
                remove_volume,
            ),
            None => remove_builder_objects_with(
                builder,
                owner_token,
                expected_nodes,
                list_daemons,
                inspect_daemon,
                remove_daemon,
                list_volumes,
                inspect_volume,
                list_volume_users,
                remove_volume,
            ),
        }
    }

    #[derive(Clone)]
    struct FakeBuilderNetworkState {
        container_id: String,
        attachments: BTreeMap<String, String>,
        networks: BTreeMap<String, InspectedBuilderNetwork>,
        mutations: Vec<(String, String, String)>,
    }

    fn fake_builder_network(
        name: &str,
        id: &str,
        job_id: Option<&str>,
        daemon_id: Option<&str>,
        members: impl IntoIterator<Item = String>,
    ) -> InspectedBuilderNetwork {
        let mut labels = BTreeMap::new();
        if let Some(job_id) = job_id {
            labels.insert(
                crate::docker_lease::JOB_ID_LABEL.to_string(),
                job_id.to_string(),
            );
        }
        if let Some(daemon_id) = daemon_id {
            labels.insert(
                crate::docker_lease::DAEMON_ID_LABEL.to_string(),
                daemon_id.to_string(),
            );
        }
        InspectedBuilderNetwork {
            name: name.to_string(),
            id: id.to_string(),
            labels,
            container_ids: members.into_iter().collect(),
        }
    }

    fn fake_builder_container_network_inspect(
        state: &FakeBuilderNetworkState,
        builder: &str,
    ) -> serde_json::Value {
        let networks = state
            .attachments
            .iter()
            .map(|(name, id)| (name.clone(), serde_json::json!({ "NetworkID": id })))
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "Id": state.container_id,
            "Name": format!("/{}", daemon_container_name(builder)),
            "NetworkSettings": { "Networks": networks },
        })
    }

    fn fake_builder_network_inspect(
        state: &FakeBuilderNetworkState,
        query: &str,
    ) -> Option<InspectedBuilderNetwork> {
        state
            .networks
            .values()
            .find(|network| network.id == query || network.name == query)
            .cloned()
    }

    fn fake_builder_network_state(
        container_id: &str,
        attachments: impl IntoIterator<Item = (String, String)>,
        networks: impl IntoIterator<Item = InspectedBuilderNetwork>,
    ) -> FakeBuilderNetworkState {
        FakeBuilderNetworkState {
            container_id: container_id.to_string(),
            attachments: attachments.into_iter().collect(),
            networks: networks
                .into_iter()
                .map(|network| (network.id.clone(), network))
                .collect(),
            mutations: Vec::new(),
        }
    }

    fn run_fake_builder_network_reconcile(
        state: &std::rc::Rc<std::cell::RefCell<FakeBuilderNetworkState>>,
        builder: &str,
        owner_token: &str,
        container_id: &str,
        target_name: &str,
        job_id: &str,
        daemon_id: &str,
    ) -> Result<()> {
        let identity_id = container_id.to_string();
        let mut verify = move |got_builder: &str, got_token: &str, got_id: &str| {
            if got_builder != builder || got_token != owner_token || got_id != identity_id {
                anyhow::bail!("fake owner proof mismatch");
            }
            Ok(identity_id.clone())
        };
        let inspect_state = state.clone();
        let mut inspect_container = move |id: &str| {
            let state = inspect_state.borrow();
            if id != state.container_id {
                anyhow::bail!("fake container inspection used a nonimmutable ID");
            }
            Ok(Some(fake_builder_container_network_inspect(
                &state, builder,
            )))
        };
        let inspect_state = state.clone();
        let mut inspect_network = move |id_or_name: &str| {
            Ok(fake_builder_network_inspect(
                &inspect_state.borrow(),
                id_or_name,
            ))
        };
        let disconnect_state = state.clone();
        let mut disconnect = move |network_id: &str, id: &str| {
            if id != container_id {
                anyhow::bail!("fake disconnect used a nonimmutable container ID");
            }
            let mut state = disconnect_state.borrow_mut();
            let network = state
                .networks
                .get(network_id)
                .with_context(|| format!("unknown fake network ID {network_id}"))?;
            let network_name = network.name.clone();
            state.attachments.remove(&network_name);
            state
                .networks
                .get_mut(network_id)
                .context("fake network disappeared")?
                .container_ids
                .remove(id);
            state.mutations.push((
                "disconnect".to_string(),
                network_id.to_string(),
                id.to_string(),
            ));
            Ok(())
        };
        let connect_state = state.clone();
        let mut connect = move |network_id: &str, id: &str| {
            if id != container_id {
                anyhow::bail!("fake connect used a nonimmutable container ID");
            }
            let mut state = connect_state.borrow_mut();
            let network = state
                .networks
                .get(network_id)
                .with_context(|| format!("unknown fake target ID {network_id}"))?;
            let network_name = network.name.clone();
            state
                .attachments
                .insert(network_name, network_id.to_string());
            state
                .networks
                .get_mut(network_id)
                .context("fake target network disappeared")?
                .container_ids
                .insert(id.to_string());
            state.mutations.push((
                "connect".to_string(),
                network_id.to_string(),
                id.to_string(),
            ));
            Ok(())
        };
        reconcile_admitted_builder_network_with(
            builder,
            owner_token,
            container_id,
            target_name,
            job_id,
            daemon_id,
            &mut verify,
            &mut inspect_container,
            &mut inspect_network,
            &mut disconnect,
            &mut connect,
        )
    }

    fn admit_test_builder(
        run_root: &Path,
        registry_root: &Path,
        builder: &str,
        slot: &str,
        container: &str,
        scope: &str,
        tier: &str,
        repository: Option<&str>,
    ) -> Result<()> {
        claim_builder_bounded_with_ops(
            run_root,
            registry_root,
            builder,
            slot,
            container,
            scope,
            tier,
            repository,
            || Ok(BTreeSet::new()),
            |_| Ok(false),
        )
    }

    #[test]
    fn admitted_builder_network_reconcile_uses_immutable_ids_and_exact_private_target() {
        let builder = test_builder();
        let owner_token = "e".repeat(32);
        let container_id = "d".repeat(64);
        let target_name = "velnor-net-job-current";
        let target_id = "a".repeat(64);
        let bridge_id = "b".repeat(64);
        let previous_id = "c".repeat(64);
        let other_member = "f".repeat(64);
        let state = std::rc::Rc::new(std::cell::RefCell::new(fake_builder_network_state(
            &container_id,
            [
                ("bridge".to_string(), bridge_id.clone()),
                ("velnor-net-job-old".to_string(), previous_id.clone()),
            ],
            [
                fake_builder_network(
                    "bridge",
                    &bridge_id,
                    None,
                    None,
                    [container_id.clone(), other_member.clone()],
                ),
                fake_builder_network(
                    "velnor-net-job-old",
                    &previous_id,
                    Some("velnor-job-old"),
                    Some("daemon-current"),
                    [container_id.clone()],
                ),
                fake_builder_network(
                    target_name,
                    &target_id,
                    Some("velnor-job-current"),
                    Some("daemon-current"),
                    std::iter::empty(),
                ),
            ],
        )));

        run_fake_builder_network_reconcile(
            &state,
            &builder,
            &owner_token,
            &container_id,
            target_name,
            "velnor-job-current",
            "daemon-current",
        )
        .unwrap();

        let state = state.borrow();
        assert_eq!(
            state.attachments,
            BTreeMap::from([(target_name.to_string(), target_id.clone())])
        );
        assert!(state.networks[&bridge_id]
            .container_ids
            .contains(&other_member));
        assert!(state.networks[&previous_id].container_ids.is_empty());
        assert!(state.networks[&target_id]
            .container_ids
            .contains(&container_id));
        assert_eq!(state.mutations.len(), 3);
        assert_eq!(
            state.mutations[0],
            ("disconnect".to_string(), bridge_id, container_id.clone())
        );
        assert_eq!(
            state.mutations[1],
            ("disconnect".to_string(), previous_id, container_id.clone())
        );
        assert_eq!(
            state.mutations[2],
            ("connect".to_string(), target_id, container_id)
        );
    }

    #[test]
    fn admitted_builder_network_reconcile_rejects_unsafe_old_networks_before_mutation() {
        let builder = test_builder();
        let owner_token = "e".repeat(32);
        let container_id = "d".repeat(64);
        let target_name = "velnor-net-job-current";
        let target_id = "a".repeat(64);
        let cases = [
            ("unknown", "host", None, None, vec![container_id.clone()]),
            (
                "foreign-private",
                "velnor-net-foreign",
                Some("velnor-job-old"),
                Some("daemon-foreign"),
                vec![container_id.clone()],
            ),
            (
                "shared-private",
                "velnor-net-shared",
                Some("velnor-job-old"),
                Some("daemon-current"),
                vec![container_id.clone(), "f".repeat(64)],
            ),
        ];
        for (case, old_name, old_job, old_daemon, members) in cases {
            let old_id = match case {
                "unknown" => "b".repeat(64),
                "foreign-private" => "c".repeat(64),
                _ => "f".repeat(64),
            };
            let state = std::rc::Rc::new(std::cell::RefCell::new(fake_builder_network_state(
                &container_id,
                [(old_name.to_string(), old_id.clone())],
                [
                    fake_builder_network(old_name, &old_id, old_job, old_daemon, members),
                    fake_builder_network(
                        target_name,
                        &target_id,
                        Some("velnor-job-current"),
                        Some("daemon-current"),
                        std::iter::empty(),
                    ),
                ],
            )));
            let error = run_fake_builder_network_reconcile(
                &state,
                &builder,
                &owner_token,
                &container_id,
                target_name,
                "velnor-job-current",
                "daemon-current",
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("network") || format!("{error:#}").contains("detach"),
                "case {case} returned an unexpected error: {error:#}"
            );
            assert!(
                state.borrow().mutations.is_empty(),
                "case {case} mutated Docker"
            );
        }
    }

    #[test]
    fn release_allows_missing_daemon_only_with_unused_exact_owner_volume() {
        let root = temp_root("release-no-daemon-proof");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-release",
            "velnor-job-release",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        let owner_token = owner_token_from_registry(&registry_root, &builder).unwrap();
        let daemon_name = daemon_container_name(&builder);
        let volume_name = daemon_state_volume(&builder);
        let holder = "velnor-job-release";
        assert!(
            reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, holder,)
                .unwrap()
        );

        let absent = resolve_admitted_builder_release_id_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            holder,
            |name| {
                assert_eq!(name, daemon_name);
                Ok(None)
            },
            |name| {
                assert_eq!(name, volume_name);
                Ok(None)
            },
            |name| {
                assert_eq!(name, volume_name);
                Ok(BTreeSet::new())
            },
        )
        .unwrap();
        assert_eq!(absent, None);

        let (_unused_container, owned_volume) = fake_inspected_buildkit_objects(
            &builder,
            &owner_token,
            &"b".repeat(64),
            "/var/lib/docker/volumes/owned/_data",
        );
        let resolved_owned_volume = resolve_admitted_builder_release_id_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            holder,
            |_| Ok(None),
            |_| Ok(Some(owned_volume.clone())),
            |_| Ok(BTreeSet::new()),
        )
        .unwrap();
        assert_eq!(resolved_owned_volume, None);

        let (_unused_container, mut foreign_volume) = fake_inspected_buildkit_objects(
            &builder,
            &owner_token,
            &"b".repeat(64),
            "/var/lib/docker/volumes/owned/_data",
        );
        foreign_volume["Labels"][crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL] =
            serde_json::Value::String("c".repeat(32));
        let error = resolve_admitted_builder_release_id_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            holder,
            |_| Ok(None),
            |_| Ok(Some(foreign_volume.clone())),
            |_| Ok(BTreeSet::new()),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("labels"));

        let error = resolve_admitted_builder_release_id_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            holder,
            |_| Ok(None),
            |_| Ok(None),
            |_| Ok(["f".repeat(64)].into_iter().collect()),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("still mounted"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_builder_recovers_engine_create_before_owner_id_write() {
        let root = temp_root("daemon-create-crash-recovery");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-recovery",
            "velnor-job-recovery",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();

        // Setup registered its durable v2 owner before Engine create. This
        // is the process-crash window: Engine made the daemon, but its
        // immutable ID has not reached the owner record.
        let initial = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        assert!(initial.daemon_nodes.is_empty());
        let owner_token = initial.owner_token.unwrap();
        let daemon_name = daemon_container_name(&builder);
        let id = "b".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &id, &mountpoint);
        let container_inspections = std::cell::Cell::new(0);
        let volume_inspections = std::cell::Cell::new(0);

        let recovered = verify_admitted_builder_daemon_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &daemon_name,
            |name| {
                assert_eq!(name, daemon_name);
                let calls = container_inspections.get();
                container_inspections.set(calls + 1);
                if calls == 1 {
                    let durable = read_owner_record(&registry_root, &builder)
                        .unwrap()
                        .unwrap();
                    let node = durable.daemon_nodes.get(&daemon_name).unwrap();
                    assert_eq!(node.container_id, id);
                    assert_eq!(node.state_volume_mountpoint, mountpoint);
                }
                Ok(Some(container.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                volume_inspections.set(volume_inspections.get() + 1);
                Ok(Some(volume.clone()))
            },
            |_| Ok([id.clone()].into_iter().collect()),
        )
        .unwrap();

        assert_eq!(recovered, id);
        assert_eq!(container_inspections.get(), 2);
        assert_eq!(volume_inspections.get(), 2);
        let durable = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        let node = durable.daemon_nodes.get(&daemon_name).unwrap();
        assert_eq!(node.container_id, id);
        assert_eq!(node.state_volume_mountpoint, mountpoint);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn teardown_release_adopts_create_crash_while_reservation_is_held() {
        let root = temp_root("release-create-crash-adoption");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let job = "velnor-job-release-crash";
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-release-crash",
            job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        let owner_token = owner_token_from_registry(&registry_root, &builder).unwrap();
        let daemon_name = daemon_container_name(&builder);
        let container_id = "d".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (container, volume) = fake_inspected_buildkit_objects_with_state(
            &builder,
            &owner_token,
            &container_id,
            &mountpoint,
            "created",
        );
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap()
            .daemon_nodes
            .is_empty());
        assert!(
            reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job,)
                .unwrap()
        );

        // Create succeeded but its observer had not persisted the ID. The
        // teardown reservation is the only allowed path to adopt this
        // exact object now that setup cannot complete and no new holder can
        // enter.
        let recovered_id = resolve_admitted_builder_release_id_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            job,
            |name| {
                assert_eq!(name, daemon_name);
                Ok(Some(container.clone()))
            },
            |name| {
                assert_eq!(name, daemon_state_volume(&builder));
                Ok(Some(volume.clone()))
            },
            |_| Ok([container_id.clone()].into_iter().collect()),
        )
        .unwrap();
        assert_eq!(recovered_id.as_deref(), Some(container_id.as_str()));
        let durable = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(
            durable.daemon_nodes[&daemon_name].container_id,
            container_id
        );

        let detached = std::cell::Cell::new(0);
        let stopped = std::cell::Cell::new(0);
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || {
                detached.set(detached.get() + 1);
                assert_eq!(
                    read_claims(&claims_file(&run_root, &builder))?
                        .releasing
                        .as_deref(),
                    Some(job)
                );
                Ok(())
            },
            || {
                stopped.set(stopped.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert!(outcome.removed_last);
        assert_eq!(detached.get(), 1);
        assert_eq!(stopped.get(), 1);
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_builder_recovery_rejects_wrong_owner_and_appended_node() {
        let root = temp_root("daemon-recovery-rejects-foreign");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-recovery",
            "velnor-job-recovery",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        let owner_token = owner_token_from_registry(&registry_root, &builder).unwrap();
        let daemon_name = daemon_container_name(&builder);
        let id = "c".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (mut container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &id, &mountpoint);
        container["Config"]["Labels"][crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL] =
            serde_json::Value::String("d".repeat(32));

        let appended_node = format!("buildx_buildkit_{builder}1");
        let appended_error = verify_admitted_builder_daemon_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &appended_node,
            |_| panic!("appended node name must not be inspected for adoption"),
            |_| panic!("appended node name must not inspect a state volume"),
            |_| panic!("appended node name must not scan volume users"),
        )
        .unwrap_err();
        assert!(format!("{appended_error:#}").contains("canonical node name"));

        let wrong_owner_error = verify_admitted_builder_daemon_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &daemon_name,
            |_| Ok(Some(container.clone())),
            |_| Ok(Some(volume.clone())),
            |_| Ok([id.clone()].into_iter().collect()),
        )
        .unwrap_err();
        assert!(format!("{wrong_owner_error:#}").contains("labels"));
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap()
            .daemon_nodes
            .is_empty());

        let mut record = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        record.daemon_nodes.insert(
            appended_node.clone(),
            BuilderNodeRecord {
                container_id: "a".repeat(64),
                state_volume_mountpoint: "/var/lib/docker/volumes/appended/_data".to_string(),
                created_unix: unix_now(),
                bootstrap_phase: BuilderBootstrapPhase::Unverified,
                config_sha256: None,
            },
        );
        write_atomic_document(
            &owner_registry_file(&registry_root, &builder),
            &serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        let durable_append_error = verify_admitted_builder_daemon_with(
            &registry_root,
            &run_root,
            &builder,
            &owner_token,
            &daemon_name,
            |_| panic!("recorded appended node must block canonical daemon use"),
            |_| panic!("recorded appended node must block volume inspection"),
            |_| panic!("recorded appended node must block volume-user inspection"),
        )
        .unwrap_err();
        assert!(format!("{durable_append_error:#}").contains("appended daemon node"));
        let admission_error = claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-recovery",
            "velnor-job-recovery",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || panic!("appended owner node must block before admission scans"),
            |_| panic!("appended owner node must not trigger capacity deletion"),
        )
        .unwrap_err();
        assert!(format!("{admission_error:#}").contains("appended daemon nodes"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn first_create_requires_absent_daemon_and_exact_unused_volume() {
        let root = temp_root("first-create-preflight");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let owner_token = ensure_test_owner_record(&registry_root, &builder);
        let daemon_name = daemon_container_name(&builder);
        let state_volume = daemon_state_volume(&builder);
        let id = "c".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{state_volume}/_data");
        let (container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &id, &mountpoint);

        require_admitted_builder_first_create_available_with(
            &registry_root,
            &builder,
            &owner_token,
            |name| {
                assert_eq!(name, daemon_name);
                Ok(None)
            },
            |name| {
                assert_eq!(name, state_volume);
                Ok(None)
            },
            |_| Ok(BTreeSet::new()),
        )
        .unwrap();

        let existing_daemon = require_admitted_builder_first_create_available_with(
            &registry_root,
            &builder,
            &owner_token,
            |_| Ok(Some(container.clone())),
            |_| panic!("an existing daemon must block volume inspection"),
            |_| panic!("an existing daemon must block volume-user inspection"),
        )
        .unwrap_err();
        assert!(format!("{existing_daemon:#}").contains("already exists"));

        // Moby creates Buildx's named volume during ContainerCreate. A volume
        // from an interrupted earlier request is reusable only with the
        // injected owner token and no live mounts.
        require_admitted_builder_first_create_available_with(
            &registry_root,
            &builder,
            &owner_token,
            |_| Ok(None),
            |_| Ok(Some(volume.clone())),
            |_| Ok(BTreeSet::new()),
        )
        .unwrap();

        let mounted_volume = require_admitted_builder_first_create_available_with(
            &registry_root,
            &builder,
            &owner_token,
            |_| Ok(None),
            |_| Ok(Some(volume.clone())),
            |_| Ok(["foreign-container-id".to_owned()].into_iter().collect()),
        )
        .unwrap_err();
        assert!(format!("{mounted_volume:#}").contains("already mounted"));

        let mut foreign_volume = volume;
        foreign_volume["Labels"][crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL] =
            serde_json::Value::String("d".repeat(32));
        let foreign = require_admitted_builder_first_create_available_with(
            &registry_root,
            &builder,
            &owner_token,
            |_| Ok(None),
            |_| Ok(Some(foreign_volume.clone())),
            |_| panic!("foreign volume labels must fail before checking users"),
        )
        .unwrap_err();
        assert!(format!("{foreign:#}").contains("labels"));

        std::fs::remove_dir_all(&root).unwrap();
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
        let default = persistent_builder_name(
            "velnor-builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world"),
        );
        assert_eq!(
            default,
            persistent_builder_name(
                "velnor-builder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("octocat/hello-world")
            )
        );
        assert!(is_bounded_builder_name(&default));
        assert_eq!(default.len(), CURRENT_PERSISTENT_BUILDER_PREFIX.len() + 64);

        let custom = persistent_builder_name(
            "mybuilder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world"),
        );
        let untrusted = persistent_builder_name(
            "velnor-builder",
            "untrusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world"),
        );
        let no_repo = persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, None);
        assert_eq!(
            no_repo,
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("  "))
        );
        let hostile = persistent_builder_name("../../x", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        assert!([&custom, &untrusted, &no_repo, &hostile]
            .into_iter()
            .all(|name| is_bounded_builder_name(name)));
        assert_ne!(default, custom);
        assert_ne!(default, untrusted);
        assert_ne!(no_repo, hostile);

        // Prefix extension by workflow-controlled names used to make a
        // sibling node 0 look like an appended node on the shorter builder.
        let cache = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let cache1 = persistent_builder_name("cache1", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        assert_ne!(cache, cache1);
        assert_eq!(cache.len(), cache1.len());
        assert!(!cache1.starts_with(&cache));
        let cache1_node0 = daemon_container_name(&cache1);
        let cache_node10 = format!("buildx_buildkit_{cache}10");
        assert_ne!(cache1_node0, cache_node10);
        assert!(!is_builder_daemon_node(&cache1_node0, &cache));
        assert!(!is_builder_state_volume(
            &daemon_state_volume(&cache1),
            &cache
        ));
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
    fn persistent_matching_requires_current_identity_and_reserves_objects() {
        let current =
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let retired_capped = "velnor-builder-shared-trusted-branch-o_r";
        let retired_slot = "velnor-builder-cache-slot-3";
        let legacy_v1 = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        assert!(is_persistent_builder_name(&current));
        assert!(!is_persistent_builder_name(retired_capped));
        assert!(!is_persistent_builder_name(retired_slot));
        assert!(!is_persistent_builder_name(legacy_v1));
        assert!(is_legacy_v1_builder_name(legacy_v1));
        assert!(!is_legacy_v1_builder_name(
            "velnor-builder-shared-unbounded-v1-bad"
        ));
        assert!(!is_persistent_builder_name("external-buildx-cache"));

        // The whole reserved object namespace stays out of generic job
        // teardown, while only current canonical names enter maintenance.
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0_state"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-cache-slot-30"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-cache-slot-30_state"
        ));
        assert!(!is_persistent_builder_object("buildx_buildkit_external0"));
        assert!(!is_persistent_builder_object(
            "prefix-buildx_buildkit_velnor-builder-cache-slot-30"
        ));
    }

    #[test]
    fn exclusive_admission_never_replaces_present_blank_or_invalid_claim_identity() {
        let root = temp_root("exclusive-claim-invalid-identity");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let path = claims_file(&run_root, &builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let invalid_claims = [
            b"{}".to_vec(),
            serde_json::to_vec(&serde_json::json!({
                "builder": "velnor-builder-retired-name",
                "holders": {},
                "releasing": null
            }))
            .unwrap(),
        ];
        for bytes in invalid_claims {
            std::fs::write(&path, bytes).unwrap();
            let before = std::fs::read(&path).unwrap();
            let error = ensure_builder_exclusive_claim(
                &run_root,
                &registry_root,
                &builder,
                "velnor-job-new",
                None,
                &BTreeSet::new(),
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("incomplete or invalid builder ownership"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn legacy_v1_cleanup_requires_exact_owner_row_and_live_label_volume_proof() {
        let builder = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        assert!(is_legacy_v1_builder_name(builder));
        for mismatched_volume_labels in [false, true] {
            let root = temp_root(if mismatched_volume_labels {
                "legacy-v1-cleanup-mismatch"
            } else {
                "legacy-v1-cleanup-proven"
            });
            let run_root = root.join("run");
            let registry_root = owner_registry_root(&root.join("lib"));
            let owner_path = owner_registry_file(&registry_root, builder);
            let owner_bytes = serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "builder": builder,
            }))
            .unwrap();
            write_atomic_document(&owner_path, &owner_bytes).unwrap();

            let daemon_name = daemon_container_name(builder);
            let volume_name = daemon_state_volume(builder);
            let daemon_id = "a".repeat(64);
            let mountpoint = format!("/var/lib/docker/volumes/{volume_name}/_data");
            let (container, mut volume) = fake_legacy_v1_buildkit_objects(
                builder,
                &daemon_id,
                "velnor-job-legacy",
                "daemon-legacy",
                &mountpoint,
                "exited",
            );
            if mismatched_volume_labels {
                volume["Labels"][crate::docker_lease::DAEMON_ID_LABEL] =
                    serde_json::Value::String("daemon-other".to_string());
            }
            let daemons = std::cell::RefCell::new(Some(container));
            let volumes = std::cell::RefCell::new(Some(volume));
            let removed_daemons = std::cell::RefCell::new(Vec::new());
            let removed_volumes = std::cell::RefCell::new(Vec::new());
            let observed = crate::docker::client::ExitInfo {
                id: Some(daemon_id.clone()),
                status: Some(crate::docker::client::ContainerState::Exited),
                finished: Some(SystemTime::now()),
                created: None,
            };
            let mut remove =
                |candidate: &str, observed: Option<&crate::docker::client::ExitInfo>| {
                    assert_eq!(candidate, builder);
                    remove_legacy_v1_builder_objects_with(
                        candidate,
                        observed,
                        || {
                            Ok(if daemons.borrow().is_some() {
                                vec![daemon_name.clone()]
                            } else {
                                Vec::new()
                            })
                        },
                        |name| {
                            assert_eq!(name, daemon_name);
                            daemons
                                .borrow()
                                .as_ref()
                                .map(|value| {
                                    parse_legacy_v1_builder_container(value, builder, name)
                                })
                                .transpose()
                        },
                        |id| {
                            removed_daemons.borrow_mut().push(id.to_string());
                            daemons.borrow_mut().take();
                            Ok(())
                        },
                        || {
                            Ok(if volumes.borrow().is_some() {
                                vec![volume_name.clone()]
                            } else {
                                Vec::new()
                            })
                        },
                        |name| {
                            assert_eq!(name, volume_name);
                            Ok(volumes.borrow().clone())
                        },
                        |name| {
                            assert_eq!(name, volume_name);
                            Ok(if daemons.borrow().is_some() {
                                [daemon_id.clone()].into_iter().collect()
                            } else {
                                BTreeSet::new()
                            })
                        },
                        |name| {
                            assert_eq!(name, volume_name);
                            removed_volumes.borrow_mut().push(name.to_string());
                            volumes.borrow_mut().take();
                            Ok(())
                        },
                    )
                };
            let result = delete_legacy_v1_builder(
                &run_root,
                &registry_root,
                builder,
                Some(&observed),
                &mut remove,
            );

            if mismatched_volume_labels {
                let error = result.unwrap_err();
                assert!(format!("{error:#}").contains("ownership labels do not match"));
                assert!(daemons.borrow().is_some());
                assert!(volumes.borrow().is_some());
                assert!(!claims_file(&run_root, builder).exists());
                assert!(owner_path.exists());
                assert!(removed_daemons.borrow().is_empty());
                assert!(removed_volumes.borrow().is_empty());
            } else {
                assert!(result.unwrap());
                assert!(daemons.borrow().is_none());
                assert!(volumes.borrow().is_none());
                assert!(!claims_file(&run_root, builder).exists());
                assert!(!owner_path.exists());
                assert_eq!(*removed_daemons.borrow(), vec![daemon_id]);
                assert_eq!(*removed_volumes.borrow(), vec![volume_name]);
            }
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn unregistered_retired_buildkit_objects_get_an_explicit_retained_disposition() {
        let root = temp_root("retired-buildkit-disposition");
        let registry_root = owner_registry_root(&root.join("lib"));
        let legacy_v1 = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r";
        write_atomic_document(
            &owner_registry_file(&registry_root, legacy_v1),
            &serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "builder": legacy_v1,
            }))
            .unwrap(),
        )
        .unwrap();
        let retired = "velnor-builder-shared-trusted-branch-o_r";
        let retired_daemon = format!("buildx_buildkit_{retired}0");
        let retired_volume = format!("{retired_daemon}_state");
        let external_daemon = "buildx_buildkit_external0".to_string();
        let mut report = HorizonReport::default();

        report_unowned_reserved_buildkit_names(
            &registry_root,
            &[
                daemon_container_name(legacy_v1),
                retired_daemon.clone(),
                external_daemon.clone(),
            ],
            &[daemon_state_volume(legacy_v1), retired_volume.clone()],
            &mut report,
        );

        assert_eq!(report.failures.len(), 2);
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&retired_daemon) && failure.contains("retain reserved BuildKit daemon")
        }));
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&retired_volume)
                && failure.contains("retain reserved BuildKit state volume")
        }));
        assert!(!report.failures.iter().any(|failure| {
            failure.contains(&daemon_container_name(legacy_v1))
                || failure.contains(&external_daemon)
        }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn daemon_names_derive_the_documented_container_and_volume() {
        // Shapes proven live against buildx 0.33: container
        // `buildx_buildkit_<builder>0`, volume `<container>_state`.
        let current = test_builder();
        assert_eq!(
            daemon_container_name(&current),
            format!("buildx_buildkit_{current}0")
        );
        assert_eq!(
            daemon_state_volume(&current),
            format!("buildx_buildkit_{current}0_state")
        );
    }

    #[test]
    fn removal_preflights_unrecorded_appended_node_before_deleting_node_zero() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node0 = daemon_container_name(&builder);
        let node1 = format!("buildx_buildkit_{builder}1");
        let node0_volume = format!("{node0}_state");
        let node1_volume = format!("{node1}_state");
        let (node0_record, node0_identity, node0_volume_identity) = test_verified_node(&node0, 'a');
        let expected_nodes = [(node0.clone(), node0_record)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let delete_calls = std::cell::Cell::new(0);

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &expected_nodes,
            || Ok(vec![node0.clone(), node1.clone()]),
            |name| Ok((name == node0).then(|| node0_identity.clone())),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || Ok(vec![node0_volume.clone(), node1_volume.clone()]),
            |name| Ok((name == node0_volume).then(|| node0_volume_identity.clone())),
            |_| Ok(BTreeSet::new()),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(!removed, "unrecorded appended node must block cleanup");
        assert_eq!(
            delete_calls.get(),
            0,
            "all listed identities must be proven before deleting recorded node zero"
        );
    }

    #[test]
    fn removal_preflights_unrecorded_state_volume_before_deleting_node_zero() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node0 = daemon_container_name(&builder);
        let node0_volume = format!("{node0}_state");
        let node1_volume = format!("buildx_buildkit_{builder}1_state");
        let (node0_record, node0_identity, node0_volume_identity) = test_verified_node(&node0, 'a');
        let expected_nodes = [(node0.clone(), node0_record)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let delete_calls = std::cell::Cell::new(0);

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &expected_nodes,
            || Ok(vec![node0.clone()]),
            |name| Ok((name == node0).then(|| node0_identity.clone())),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || Ok(vec![node0_volume.clone(), node1_volume.clone()]),
            |name| Ok((name == node0_volume).then(|| node0_volume_identity.clone())),
            |volume| {
                Ok((volume == node0_volume)
                    .then(|| [node0_identity.id.clone()].into_iter().collect())
                    .unwrap_or_default())
            },
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(!removed, "unrecorded appended volume must block cleanup");
        assert_eq!(
            delete_calls.get(),
            0,
            "all listed volumes must be proven before deleting recorded node zero"
        );
    }

    #[test]
    fn removal_rejects_created_state_that_engine_reports_running() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node = daemon_container_name(&builder);
        let volume = format!("{node}_state");
        let (record, mut daemon, volume_identity) = test_verified_node(&node, 'a');
        daemon.state = crate::docker::client::ContainerState::Created;
        daemon.state_running = true;
        let expected_nodes = [(node.clone(), record)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let delete_calls = std::cell::Cell::new(0);

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &expected_nodes,
            || Ok(vec![node.clone()]),
            |name| Ok((name == node).then(|| daemon.clone())),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || Ok(vec![volume.clone()]),
            |name| Ok((name == volume).then(|| volume_identity.clone())),
            |_| Ok([daemon.id.clone()].into_iter().collect()),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(
            !removed,
            "contradictory live Engine state must retain ownership"
        );
        assert_eq!(
            delete_calls.get(),
            0,
            "no object may be removed on uncertainty"
        );
    }

    #[test]
    fn removal_rechecks_created_state_immediately_before_delete() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node = daemon_container_name(&builder);
        let volume = format!("{node}_state");
        let (record, mut daemon, volume_identity) = test_verified_node(&node, 'a');
        daemon.state = crate::docker::client::ContainerState::Created;
        let expected_nodes = [(node.clone(), record)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let delete_calls = std::cell::Cell::new(0);
        let inspections = std::cell::Cell::new(0);

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &expected_nodes,
            || Ok(vec![node.clone()]),
            |name| {
                if name != node {
                    return Ok(None);
                }
                let count = inspections.get();
                inspections.set(count + 1);
                let mut observed = daemon.clone();
                observed.state_running = count >= 1;
                Ok(Some(observed))
            },
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || Ok(vec![volume.clone()]),
            |name| Ok((name == volume).then(|| volume_identity.clone())),
            |_| Ok([daemon.id.clone()].into_iter().collect()),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(!removed, "a live recheck must retain ownership");
        assert_eq!(inspections.get(), 2, "preflight and delete recheck ran");
        assert_eq!(
            delete_calls.get(),
            0,
            "no removal may start after liveness changes"
        );
    }

    #[test]
    fn unrecorded_running_created_daemon_blocks_first_create_recovery() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node = daemon_container_name(&builder);
        let volume = format!("{node}_state");
        let (_, mut daemon, volume_identity) = test_verified_node(&node, 'a');
        daemon.state = crate::docker::client::ContainerState::Created;
        daemon.state_running = true;
        let delete_calls = std::cell::Cell::new(0);

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &BTreeMap::new(),
            || Ok(vec![node.clone()]),
            |name| Ok((name == node).then(|| daemon.clone())),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || Ok(vec![volume.clone()]),
            |name| Ok((name == volume).then(|| volume_identity.clone())),
            |_| Ok([daemon.id.clone()].into_iter().collect()),
            |_| {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(
            !removed,
            "an unrecorded running daemon is not a recoverable create"
        );
        assert_eq!(
            delete_calls.get(),
            0,
            "uncertain create state remains untouched"
        );
    }

    #[test]
    fn removal_deletes_every_v2_node_without_touching_a_prefix_sibling() {
        let builder = persistent_builder_name("cache", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let sibling = persistent_builder_name("cache1", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let node0 = daemon_container_name(&builder);
        let node1 = format!("buildx_buildkit_{builder}1");
        let sibling_node0 = daemon_container_name(&sibling);
        let node0_volume = format!("{node0}_state");
        let node1_volume = format!("{node1}_state");
        let sibling_volume = format!("{sibling_node0}_state");
        let (node0_record, node0_identity, node0_volume_identity) = test_verified_node(&node0, 'a');
        let (node1_record, node1_identity, node1_volume_identity) = test_verified_node(&node1, 'b');
        let (_, sibling_identity, sibling_volume_identity) =
            test_verified_node(&sibling_node0, 'c');
        let expected_nodes = [(node0.clone(), node0_record), (node1.clone(), node1_record)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let daemons = std::cell::RefCell::new(
            [
                (node0.clone(), node0_identity),
                (node1.clone(), node1_identity),
                (sibling_node0.clone(), sibling_identity),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        );
        let volumes = std::cell::RefCell::new(
            [
                (node0_volume.clone(), node0_volume_identity),
                (node1_volume.clone(), node1_volume_identity),
                (sibling_volume.clone(), sibling_volume_identity),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        );
        let removed_daemons = std::cell::RefCell::new(Vec::new());
        let removed_volumes = std::cell::RefCell::new(Vec::new());

        let removed = remove_builder_objects_with(
            &builder,
            &"a".repeat(32),
            &expected_nodes,
            || Ok(daemons.borrow().keys().cloned().collect()),
            |daemon| Ok(daemons.borrow().get(daemon).cloned()),
            |container_id| {
                let name = daemons
                    .borrow()
                    .iter()
                    .find(|(_, daemon)| daemon.id == container_id)
                    .map(|(name, _)| name.clone());
                let Some(name) = name else {
                    anyhow::bail!("test daemon ID missing")
                };
                removed_daemons.borrow_mut().push(name.clone());
                daemons.borrow_mut().remove(&name);
                Ok(())
            },
            || Ok(volumes.borrow().keys().cloned().collect()),
            |volume| Ok(volumes.borrow().get(volume).cloned()),
            |volume| {
                let daemon = volume.strip_suffix("_state").unwrap();
                Ok(daemons
                    .borrow()
                    .get(daemon)
                    .map(|daemon| [daemon.id.clone()].into_iter().collect())
                    .unwrap_or_default())
            },
            |volume| {
                removed_volumes.borrow_mut().push(volume.to_string());
                volumes.borrow_mut().remove(volume);
                Ok(())
            },
        )
        .unwrap();

        assert!(removed);
        assert_eq!(*removed_daemons.borrow(), vec![node0, node1]);
        assert_eq!(*removed_volumes.borrow(), vec![node0_volume, node1_volume]);
        assert_eq!(
            daemons.borrow().keys().cloned().collect::<BTreeSet<_>>(),
            [sibling_node0].into_iter().collect()
        );
        assert_eq!(
            volumes.borrow().keys().cloned().collect::<BTreeSet<_>>(),
            [sibling_volume].into_iter().collect()
        );
    }

    #[test]
    fn v1_owner_record_outside_legacy_namespace_fails_closed_before_cleanup_or_admission() {
        let root = temp_root("v1-owner-migration-blocker");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        std::fs::create_dir_all(&registry_root).unwrap();
        let fixture = serde_json::json!({
            "version": 1,
            "builder": builder,
        });
        std::fs::write(
            owner_registry_file(&registry_root, &builder),
            serde_json::to_vec(&fixture).unwrap(),
        )
        .unwrap();

        let owner_error = read_owner_record(&registry_root, &builder)
            .err()
            .expect("v1 owner row must stop direct admission");
        assert!(format!("{owner_error:#}").contains("operator migration"));
        let mut report = HorizonReport::default();
        assert!(registered_owner_builders(&registry_root, &mut report).is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("not an exact v1 identity")));

        let admission_error = claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-v1",
            "velnor-job-v1",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || panic!("v1 row must fail before container listing"),
            |_| panic!("v1 row must fail before capacity removal"),
        )
        .unwrap_err();
        assert!(format!("{admission_error:#}").contains("operator migration"));
        assert!(!claims_file(&run_root, &builder).exists());

        let reap = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |_| panic!("v1 owner rows must fail before daemon inspection"),
            |_| panic!("v1 owner rows must fail before stop"),
            |_| panic!("v1 owner rows must fail before restart"),
            |_, _observed| panic!("v1 owner rows must fail before deletion"),
        );
        assert!(reap.deleted.is_empty());
        assert!(reap
            .failures
            .iter()
            .any(|failure| failure.contains("not an exact v1 identity")));
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn retired_names_and_unregistered_claims_fail_closed() {
        let root = temp_root("retired-builder-names");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let retired_capped = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let retired_slot = "velnor-builder-cache-slot-3".to_string();
        let current_without_registry = test_builder();

        for builder in [&retired_capped, &retired_slot] {
            let error = claim_builder_bounded_with_ops(
                &run_root,
                &registry_root,
                builder,
                "slot-old",
                "velnor-job-old",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("o/r"),
                || panic!("retired builder must fail before container listing"),
                |_| panic!("retired builder must fail before eviction"),
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("canonical bounded"));
            assert!(!claims_file(&run_root, builder).exists());
        }

        for builder in [&retired_capped, &retired_slot, &current_without_registry] {
            let path = claims_file(&run_root, builder);
            write_claims(
                &path,
                &BuilderClaims {
                    builder: builder.clone(),
                    ..BuilderClaims::default()
                },
            )
            .unwrap();
            assert!(
                read_claims_for_reaping(&path, builder, Some(&registry_root), true)
                    .unwrap()
                    .is_none(),
                "builder {builder} cannot authorize reaping from a runtime claim without a matching current owner"
            );
        }
        let current_claim = claims_file(&run_root, &current_without_registry);
        assert!(
            read_claims_for_reaping(&current_claim, &current_without_registry, None, false)
                .unwrap()
                .is_none(),
            "a current-format runtime claim without a durable registry is not ownership proof"
        );

        let missing_claim = claims_file(&run_root, &retired_capped);
        std::fs::remove_file(&missing_claim).unwrap();
        assert!(
            read_claims_for_reaping(&missing_claim, &retired_capped, Some(&registry_root), true)
                .unwrap()
                .is_none(),
            "quiescence does not turn a retired name into ownership proof"
        );

        assert!(claims_file(&run_root, &retired_slot).exists());
        assert!(current_claim.exists());
        assert!(!missing_claim.exists());

        for builder in [&retired_capped, &retired_slot] {
            let record = BuilderOwnerRecord {
                version: OWNER_REGISTRY_VERSION,
                builder: builder.clone(),
                owner_token: Some("a".repeat(32)),
                daemon_nodes: BTreeMap::new(),
                group: Some(test_owner_group()),
                registered_unix: 1,
                updated_unix: 1,
            };
            write_atomic_document(
                &owner_registry_file(&registry_root, builder),
                &serde_json::to_vec_pretty(&record).unwrap(),
            )
            .unwrap();
        }
        let pressure = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            1,
            || Ok(BTreeSet::new()),
            |_| panic!("retired owner records must not reach disk usage"),
            |_| panic!("retired owner records must not reach prune"),
            |_| panic!("retired owner records must not reach stop"),
            |_| panic!("retired owner records must not reach restart"),
        );
        assert!(pressure.pruned.is_empty());
        assert_eq!(pressure.freed_bytes, 0);
        assert!(pressure
            .failures
            .iter()
            .any(|failure| failure.contains("keep mismatched BuildKit owner record")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn current_builder_owner_record_recovers_runtime_state_after_reboot() {
        let root = temp_root("current-reap-after-reboot");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
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
            || Ok(BTreeSet::new()),
            |daemon| {
                assert_eq!(daemon, daemon_container_name(&builder));
                Ok(crate::docker::client::ExitInfo {
                    id: Some("d".repeat(64)),
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: Some(finished),
                    created: None,
                })
            },
            |_| panic!("stopped builder needs no stop call"),
            |_| panic!("no holder race exists"),
            |removed_builder, _observed| {
                removed.borrow_mut().push(removed_builder.to_string());
                Ok(true)
            },
        );

        assert_eq!(report.deleted, vec![builder.clone()]);
        assert_eq!(*removed.borrow(), vec![builder.clone()]);
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(!owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaps_stale_recorded_created_bootstrap() {
        let root = temp_root("created-daemon-crash-reap");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let job = "velnor-job-created-crash";
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-created-crash",
            job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();

        // Engine created the exact owned daemon and the owner row persisted
        // phase=Created, but the runner crashed before Buildx could start it.
        // Created has no FinishedAt, so the old reaper skipped it forever.
        // Removal requires no active job, an empty claim, both immutable
        // creation ages beyond the horizon, and exact ID/token/mount/user
        // proof for the daemon and state volume.
        abandon_claims(&run_root, &builder);
        let now = SystemTime::now();
        let created = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let mut owner = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        let owner_token = owner.owner_token.clone().unwrap();
        let daemon_name = daemon_container_name(&builder);
        let container_id = "b".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        owner.daemon_nodes.insert(
            daemon_name.clone(),
            BuilderNodeRecord {
                container_id: container_id.clone(),
                state_volume_mountpoint: mountpoint.clone(),
                created_unix: created
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                bootstrap_phase: BuilderBootstrapPhase::Created,
                config_sha256: Some("a".repeat(64)),
            },
        );
        // Claims refresh the LRU timestamp, never the Created daemon age.
        owner.updated_unix = unix_now();
        write_atomic_document(
            &owner_registry_file(&registry_root, &builder),
            &serde_json::to_vec_pretty(&owner).unwrap(),
        )
        .unwrap();
        let (container, volume) = fake_inspected_buildkit_objects_with_state(
            &builder,
            &owner_token,
            &container_id,
            &mountpoint,
            "created",
        );
        let daemons = std::cell::RefCell::new([(daemon_name.clone(), container)].into());
        let volumes = std::cell::RefCell::new([(daemon_state_volume(&builder), volume)].into());

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(BTreeSet::new()),
            |name| {
                assert_eq!(name, daemon_name);
                Ok(crate::docker::client::ExitInfo {
                    id: Some(container_id.clone()),
                    status: Some(crate::docker::client::ContainerState::Created),
                    finished: None,
                    created: Some(created),
                })
            },
            |_| panic!("Created daemons are safe to remove without a stop"),
            |_| panic!("no live job can race the exclusive lifecycle pass"),
            |name, observed| {
                assert_eq!(name, builder);
                let record = read_owner_record(&registry_root, name)?.unwrap();
                remove_fake_builder_objects(
                    name,
                    record.owner_token.as_deref().unwrap(),
                    &record.daemon_nodes,
                    &daemons,
                    &volumes,
                    observed,
                )
            },
        );

        assert_eq!(report.deleted, vec![builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(daemons.borrow().is_empty());
        assert!(volumes.borrow().is_empty());
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_recovers_created_daemon_before_owner_node_id_was_persisted() {
        let root = temp_root("created-daemon-before-owner-id-reap");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let job = "velnor-job-created-owner-id-crash";
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-created-owner-id-crash",
            job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        abandon_claims(&run_root, &builder);

        let now = SystemTime::now();
        let created = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let mut owner = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        assert!(owner.daemon_nodes.is_empty());
        let owner_token = owner.owner_token.clone().unwrap();
        // The immutable owner-registration time covers the crash window
        // before the Engine ID reached `daemon_nodes`; claims may refresh
        // `updated_unix` independently.
        owner.registered_unix = created
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        owner.updated_unix = unix_now();
        write_atomic_document(
            &owner_registry_file(&registry_root, &builder),
            &serde_json::to_vec_pretty(&owner).unwrap(),
        )
        .unwrap();

        let daemon_name = daemon_container_name(&builder);
        let container_id = "c".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (container, volume) = fake_inspected_buildkit_objects_with_state(
            &builder,
            &owner_token,
            &container_id,
            &mountpoint,
            "created",
        );
        let daemons = std::cell::RefCell::new([(daemon_name.clone(), container)].into());
        let volumes = std::cell::RefCell::new([(daemon_state_volume(&builder), volume)].into());

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(BTreeSet::new()),
            |name| {
                assert_eq!(name, daemon_name);
                Ok(crate::docker::client::ExitInfo {
                    id: Some(container_id.clone()),
                    status: Some(crate::docker::client::ContainerState::Created),
                    finished: None,
                    created: Some(created),
                })
            },
            |_| panic!("Created daemon is safe to remove without a stop"),
            |_| panic!("no live holder can race the exclusive lifecycle pass"),
            |name, observed| {
                assert_eq!(name, builder);
                let record = read_owner_record(&registry_root, name)?.unwrap();
                assert!(record.daemon_nodes.is_empty());
                remove_fake_builder_objects(
                    name,
                    record.owner_token.as_deref().unwrap(),
                    &record.daemon_nodes,
                    &daemons,
                    &volumes,
                    observed,
                )
            },
        );

        assert_eq!(report.deleted, vec![builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(daemons.borrow().is_empty());
        assert!(volumes.borrow().is_empty());
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn created_daemon_rechecks_immutable_id_and_state_before_removal() {
        let builder = test_builder();
        let token = "e".repeat(32);
        let id = "a".repeat(64);
        let replacement_id = "b".repeat(64);
        let daemon_name = daemon_container_name(&builder);
        let volume_name = daemon_state_volume(&builder);
        let mountpoint = format!("/var/lib/docker/volumes/{volume_name}/_data");
        let observed = crate::docker::client::ExitInfo {
            id: Some(id.clone()),
            status: Some(crate::docker::client::ContainerState::Created),
            finished: None,
            created: Some(SystemTime::now()),
        };
        let daemon = VerifiedBuilderDaemon {
            id: id.clone(),
            name: daemon_name.clone(),
            state_volume: volume_name.clone(),
            volume_mountpoint: mountpoint.clone(),
            state_running: false,
            state: crate::docker::client::ContainerState::Created,
        };
        let replacement = VerifiedBuilderDaemon {
            id: replacement_id,
            ..daemon.clone()
        };
        let inspections = std::cell::Cell::new(0);
        let removals = std::cell::Cell::new(0);

        let result = remove_builder_objects_after_reap_with(
            &builder,
            &token,
            &BTreeMap::new(),
            Some(&observed),
            || Ok(vec![daemon_name.clone()]),
            |_| {
                let inspection = inspections.get();
                inspections.set(inspection + 1);
                Ok(Some(if inspection == 0 {
                    daemon.clone()
                } else {
                    replacement.clone()
                }))
            },
            |_| {
                removals.set(removals.get() + 1);
                Ok(())
            },
            || Ok(vec![volume_name.clone()]),
            |_| {
                Ok(Some(VerifiedBuilderVolume {
                    name: volume_name.clone(),
                    mountpoint: mountpoint.clone(),
                }))
            },
            |_| Ok([id.clone()].into_iter().collect()),
            |_| Ok(()),
        );

        assert!(result.is_err(), "changed immutable ID must fail closed");
        assert_eq!(
            inspections.get(),
            2,
            "identity rechecked immediately before removal"
        );
        assert_eq!(
            removals.get(),
            0,
            "replacement container must never be deleted"
        );
    }

    #[test]
    fn created_cleanup_preserves_claim_and_owner_when_stopped_foreign_container_uses_volume() {
        let root = temp_root("created-daemon-foreign-volume-user");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-created-foreign-user",
            "velnor-job-created-foreign-user",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();
        abandon_claims(&run_root, &builder);

        let owner = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        let owner_token = owner.owner_token.as_deref().unwrap();
        let daemon_name = daemon_container_name(&builder);
        let daemon_id = "c".repeat(64);
        let foreign_id = "d".repeat(64);
        let volume_name = daemon_state_volume(&builder);
        let mountpoint = format!("/var/lib/docker/volumes/{volume_name}/_data");
        let (container, volume) = fake_inspected_buildkit_objects_with_state(
            &builder,
            owner_token,
            &daemon_id,
            &mountpoint,
            "created",
        );
        let foreign = serde_json::json!({
            "Id": foreign_id,
            "Name": "/velnor-job-stopped-foreign",
            "Mounts": [{
                "Type": "volume",
                "Name": volume_name,
                "Destination": "/var/lib/buildkit",
                "Source": mountpoint,
            }],
            "State": { "Running": false, "Status": "exited" },
        });
        let daemons = std::cell::RefCell::new(
            [
                (daemon_name.clone(), container),
                ("stopped-foreign".into(), foreign),
            ]
            .into(),
        );
        let volumes = std::cell::RefCell::new([(volume_name.clone(), volume)].into());
        let observed = crate::docker::client::ExitInfo {
            id: Some(daemon_id),
            status: Some(crate::docker::client::ContainerState::Created),
            finished: None,
            created: Some(SystemTime::now()),
        };
        let mut remove = |name: &str, observed: Option<&crate::docker::client::ExitInfo>| {
            let record = read_owner_record(&registry_root, name)?.unwrap();
            remove_fake_builder_objects(
                name,
                record.owner_token.as_deref().unwrap(),
                &record.daemon_nodes,
                &daemons,
                &volumes,
                observed,
            )
        };

        let result = delete_registered_builder(
            &run_root,
            Some(&registry_root),
            &builder,
            Some(&observed),
            &mut remove,
        );
        assert!(
            result.is_err(),
            "stopped foreign volume user blocks cleanup"
        );
        assert!(claims_file(&run_root, &builder).exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
        assert!(daemons.borrow().contains_key(&daemon_name));
        assert!(volumes.borrow().contains_key(&volume_name));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reaper_rejects_created_age_without_durable_owner_timestamp() {
        let root = temp_root("created-daemon-zero-owner-time");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let mut owner = read_owner_record(&registry_root, &builder)
            .unwrap()
            .unwrap();
        owner.registered_unix = 0;
        write_atomic_document(
            &owner_registry_file(&registry_root, &builder),
            &serde_json::to_vec_pretty(&owner).unwrap(),
        )
        .unwrap();
        let now = SystemTime::now();
        let created = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let mut report = HorizonReport::default();

        assert!(created_builder_idle_age(
            &builder,
            &registry_root,
            &"e".repeat(64),
            Some(created),
            now,
            &mut report,
        )
        .is_none());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("creation/bootstrap timestamp is missing")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_never_removes_unknown_engine_state() {
        let root = temp_root("unknown-engine-state");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let inspected = std::cell::Cell::new(false);
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |_| {
                inspected.set(true);
                Ok(crate::docker::client::ExitInfo {
                    id: Some("e".repeat(64)),
                    status: None,
                    finished: None,
                    created: None,
                })
            },
            |_| panic!("unknown state is not safely stoppable"),
            |_| panic!("unknown state has no holder race"),
            |_, _observed| panic!("unknown state must not be removed"),
        );

        assert!(inspected.get());
        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| { failure.contains("unknown container state") }));
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_never_deletes_running_builder_after_stop_failure() {
        let root = temp_root("running-builder-stop-failure");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let stopped = std::cell::Cell::new(false);
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    id: Some("f".repeat(64)),
                    status: Some(crate::docker::client::ContainerState::Running),
                    finished: None,
                    created: Some(SystemTime::now()),
                })
            },
            |_| {
                stopped.set(true);
                Err(anyhow::anyhow!("simulated stop failure"))
            },
            |_| panic!("failed stop must not trigger restart"),
            |_, _observed| panic!("failed stop must not reach deletion"),
        );

        assert!(stopped.get());
        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("simulated stop failure")));
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reclaims_owner_only_unused_state_volume_after_interrupted_create() {
        let root = temp_root("volume-only-create-crash-reap");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let owner_token = ensure_test_owner_record(&registry_root, &builder);
        let volume_name = daemon_state_volume(&builder);
        let mountpoint = format!("/var/lib/docker/volumes/{volume_name}/_data");
        let (_unused_daemon, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &"c".repeat(64), &mountpoint);
        let daemons = std::cell::RefCell::new(BTreeMap::new());
        let volumes = std::cell::RefCell::new([(volume_name.clone(), volume)].into());
        let now = SystemTime::now();

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(BTreeSet::new()),
            |_| {
                Err(crate::docker::client::NotFound {
                    object: daemon_container_name(&builder),
                }
                .into())
            },
            |_| panic!("a missing daemon needs no stop"),
            |_| panic!("no holder race exists"),
            |name, observed| {
                let record = read_owner_record(&registry_root, name)?.unwrap();
                remove_fake_builder_objects(
                    name,
                    record.owner_token.as_deref().unwrap(),
                    &record.daemon_nodes,
                    &daemons,
                    &volumes,
                    observed,
                )
            },
        );

        assert_eq!(report.deleted, vec![builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(daemons.borrow().is_empty());
        assert!(volumes.borrow().is_empty());
        assert!(read_owner_records(&registry_root).unwrap().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cleanup_recovers_unrecorded_daemon_only_from_exact_owner_proof() {
        let root = temp_root("cleanup-unrecorded-daemon-exact-proof");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let owner_token = ensure_test_owner_record(&registry_root, &builder);
        let daemon_name = daemon_container_name(&builder);
        let container_id = "f".repeat(64);
        let mountpoint = format!("/var/lib/docker/volumes/{}_state/_data", daemon_name);
        let (container, volume) =
            fake_inspected_buildkit_objects(&builder, &owner_token, &container_id, &mountpoint);
        let daemons = std::cell::RefCell::new([(daemon_name.clone(), container.clone())].into());
        let volumes =
            std::cell::RefCell::new([(daemon_state_volume(&builder), volume.clone())].into());
        let empty_nodes = BTreeMap::new();

        assert!(remove_fake_builder_objects(
            &builder,
            &owner_token,
            &empty_nodes,
            &daemons,
            &volumes,
            None,
        )
        .unwrap());
        assert!(daemons.borrow().is_empty());
        assert!(volumes.borrow().is_empty());

        // A same-name replacement with a different token is not a recoverable
        // create crash and cannot be removed or converted into a node row.
        let mut foreign = container;
        foreign["Config"]["Labels"][crate::docker_lease::BUILDKIT_OWNER_TOKEN_LABEL] =
            serde_json::Value::String("a".repeat(32));
        daemons.borrow_mut().insert(daemon_name.clone(), foreign);
        volumes
            .borrow_mut()
            .insert(daemon_state_volume(&builder), volume);
        let error = remove_fake_builder_objects(
            &builder,
            &owner_token,
            &empty_nodes,
            &daemons,
            &volumes,
            None,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("labels"));
        assert!(daemons.borrow().contains_key(&daemon_name));
        assert!(volumes
            .borrow()
            .contains_key(&daemon_state_volume(&builder)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builder_capacity_evicts_exact_unused_volume_only_owner() {
        let root = temp_root("capacity-volume-only-create-crash");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let group = test_owner_group();
        let candidate =
            persistent_builder_name("volume-only", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let mut owners = Vec::new();
        for index in 0..MAX_BUILDERS_PER_SCOPE_TIER_REPO - 1 {
            let name = persistent_builder_name(
                &format!("capacity-{index}"),
                "trusted",
                TRUST_TIER_BRANCH,
                Some("o/r"),
            );
            ensure_owner_record(&registry_root, &name, Some(&group)).unwrap();
            let path = claims_file(&run_root, &name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_claims(
                &path,
                &BuilderClaims {
                    holders: BTreeMap::new(),
                    builder: name.clone(),
                    releasing: None,
                },
            )
            .unwrap();
            owners.push(name);
        }
        ensure_owner_record(&registry_root, &candidate, Some(&group)).unwrap();
        let candidate_path = claims_file(&run_root, &candidate);
        std::fs::create_dir_all(candidate_path.parent().unwrap()).unwrap();
        write_claims(
            &candidate_path,
            &BuilderClaims {
                holders: BTreeMap::new(),
                builder: candidate.clone(),
                releasing: None,
            },
        )
        .unwrap();
        let mut old_candidate = read_owner_record(&registry_root, &candidate)
            .unwrap()
            .unwrap();
        old_candidate.updated_unix = 1;
        write_atomic_document(
            &owner_registry_file(&registry_root, &candidate),
            &serde_json::to_vec_pretty(&old_candidate).unwrap(),
        )
        .unwrap();

        let owner_token = old_candidate.owner_token.unwrap();
        let volume_name = daemon_state_volume(&candidate);
        let mountpoint = format!("/var/lib/docker/volumes/{volume_name}/_data");
        let (_unused_daemon, volume) =
            fake_inspected_buildkit_objects(&candidate, &owner_token, &"e".repeat(64), &mountpoint);
        let daemons = std::cell::RefCell::new(BTreeMap::new());
        let volumes = std::cell::RefCell::new([(volume_name, volume)].into());
        let evicted = std::cell::RefCell::new(Vec::new());
        let new_builder =
            persistent_builder_name("capacity-new", "trusted", TRUST_TIER_BRANCH, Some("o/r"));

        claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &new_builder,
            "slot-capacity-new",
            "velnor-job-capacity-new",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok(BTreeSet::new()),
            |name| {
                evicted.borrow_mut().push(name.to_string());
                let record = read_owner_record(&registry_root, name)?.unwrap();
                remove_fake_builder_objects(
                    name,
                    record.owner_token.as_deref().unwrap(),
                    &record.daemon_nodes,
                    &daemons,
                    &volumes,
                    None,
                )
            },
        )
        .unwrap();

        assert_eq!(*evicted.borrow(), vec![candidate.clone()]);
        assert!(volumes.borrow().is_empty());
        assert!(read_owner_record(&registry_root, &candidate)
            .unwrap()
            .is_none());
        assert_eq!(
            builder_holders(&run_root, &new_builder, None)
                .unwrap()
                .len(),
            1
        );
        let after = read_owner_records(&registry_root).unwrap();
        assert_eq!(after.len(), MAX_BUILDERS_PER_SCOPE_TIER_REPO);
        assert!(after.iter().any(|record| record.builder == new_builder));
        assert!(owners
            .iter()
            .all(|name| after.iter().any(|record| &record.builder == name)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_keeps_registered_v2_builder_when_claim_missing_and_job_active() {
        let root = temp_root("current-missing-claim-live-job");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let owner_path = owner_registry_file(&registry_root, &builder);
        let inspected = std::cell::Cell::new(false);

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(["velnor-job-active".to_string()].into_iter().collect()),
            |_| {
                inspected.set(true);
                panic!("a missing claim plus active job blocks daemon inspection")
            },
            |_| panic!("a missing claim plus active job blocks stop"),
            |_| panic!("a missing claim plus active job blocks restart"),
            |_, _observed| panic!("a missing claim plus active job blocks removal"),
        );

        assert!(!inspected.get());
        assert!(report.deleted.is_empty());
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("missing /run claim")
        }));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_rechecks_containers_under_claim_lock_before_repair() {
        let root = temp_root("current-missing-claim-second-scan");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let scans = std::cell::Cell::new(0);

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || {
                let scan = scans.get();
                scans.set(scan + 1);
                if scan == 0 {
                    Ok(BTreeSet::new())
                } else {
                    Ok(["velnor-job-started-during-scan".to_string()]
                        .into_iter()
                        .collect())
                }
            },
            |_| panic!("second active-container scan blocks inspection"),
            |_| panic!("second active-container scan blocks stop"),
            |_| panic!("second active-container scan blocks restart"),
            |_, _observed| panic!("second active-container scan blocks removal"),
        );

        assert_eq!(scans.get(), 2, "the locked missing-claim path must rescan");
        assert!(report.deleted.is_empty());
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("missing /run claim")
        }));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_rejects_v1_owner_outside_legacy_namespace() {
        let root = temp_root("unbounded-v1-reap-blocker");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        std::fs::create_dir_all(&registry_root).unwrap();
        let fixture = serde_json::json!({
            "version": 1,
            "builder": builder,
        });
        std::fs::write(
            owner_registry_file(&registry_root, &builder),
            serde_json::to_vec(&fixture).unwrap(),
        )
        .unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);
        let removed = std::cell::RefCell::new(Vec::new());

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |_| panic!("unproven v1 daemon must not be inspected"),
            |_| panic!("unproven v1 daemon must not be stopped"),
            |_| panic!("unproven v1 daemon must not be restarted"),
            |removed_builder, _observed| {
                removed.borrow_mut().push(removed_builder.to_string());
                Ok(true)
            },
        );

        assert!(report.deleted.is_empty());
        assert!(removed.borrow().is_empty());
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&owner_path.to_string_lossy().to_string())
                && failure.contains("not an exact v1 identity")
        }));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pressure_pruner_discovers_owner_only_builder_without_host_registration() {
        let root = temp_root("pressure-owner-only");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let du_seen = std::cell::RefCell::new(Vec::new());
        let pruned = std::cell::RefCell::new(Vec::new());
        let stopped = std::cell::RefCell::new(Vec::new());

        let report = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            64,
            || Ok(BTreeSet::new()),
            |seen| {
                du_seen.borrow_mut().push(seen.to_string());
                Ok(64)
            },
            |seen| {
                pruned.borrow_mut().push(seen.to_string());
                Ok(64)
            },
            |seen| {
                stopped.borrow_mut().push(seen.to_string());
                Ok(true)
            },
            |_| panic!("no holder raced the pressure prune"),
        );

        assert_eq!(report.pruned, vec![builder.clone()]);
        assert_eq!(report.freed_bytes, 64);
        assert_eq!(*du_seen.borrow(), vec![builder.clone()]);
        assert_eq!(*pruned.borrow(), vec![builder.clone()]);
        assert_eq!(*stopped.borrow(), vec![builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pressure_pruner_keeps_missing_claim_when_any_job_is_active() {
        let root = temp_root("pressure-missing-claim-live-job");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let owner_path = owner_registry_file(&registry_root, &builder);

        let report = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            1,
            || Ok(["velnor-job-active".to_string()].into_iter().collect()),
            |_| panic!("missing claim plus active job blocks disk-usage inspection"),
            |_| panic!("missing claim plus active job blocks prune"),
            |_| panic!("missing claim plus active job blocks stop"),
            |_| panic!("missing claim plus active job blocks restart"),
        );

        assert!(report.pruned.is_empty());
        assert_eq!(report.freed_bytes, 0);
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("missing /run claim")
        }));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pressure_pruner_rechecks_containers_under_claim_lock_before_prune() {
        let root = temp_root("pressure-missing-claim-second-scan");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let scans = std::cell::Cell::new(0);

        let report = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            1,
            || {
                let scan = scans.get();
                scans.set(scan + 1);
                if scan == 0 {
                    Ok(BTreeSet::new())
                } else {
                    Ok(["velnor-job-started-during-scan".to_string()]
                        .into_iter()
                        .collect())
                }
            },
            |_| Ok(100),
            |_| panic!("the locked active-job scan blocks prune"),
            |_| panic!("the locked active-job scan blocks stop"),
            |_| panic!("no prune ran, so no restart is needed"),
        );

        assert_eq!(scans.get(), 2, "the locked missing-claim path must rescan");
        assert!(report.pruned.is_empty());
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("missing /run claim")
        }));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pressure_pruner_orders_owner_builders_by_measured_usage() {
        let root = temp_root("pressure-largest-first");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let small = persistent_builder_name("small", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let large = persistent_builder_name("large", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_test_owner_record(&registry_root, &small);
        ensure_test_owner_record(&registry_root, &large);
        let pruned = std::cell::RefCell::new(Vec::new());

        let report = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            3,
            || Ok(BTreeSet::new()),
            |builder| Ok(if builder == large { 10 } else { 2 }),
            |builder| {
                pruned.borrow_mut().push(builder.to_string());
                Ok(2)
            },
            |_| Ok(true),
            |_| panic!("no holder raced the pressure prune"),
        );

        assert_eq!(*pruned.borrow(), vec![large.clone(), small.clone()]);
        assert_eq!(report.pruned, vec![large, small]);
        assert_eq!(report.freed_bytes, 4);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pressure_pruner_restarts_builder_when_holder_arrives_during_prune() {
        let root = temp_root("pressure-prune-race");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-old",
            "velnor-job-old",
        )
        .unwrap();
        abandon_claims(&run_root, &builder);
        let stopped = std::cell::RefCell::new(Vec::new());
        let restarted = std::cell::RefCell::new(Vec::new());

        let report = pressure_prune_builders_with_registry(
            &run_root,
            Some(&registry_root),
            1,
            || Ok(BTreeSet::new()),
            |_| Ok(100),
            |pruned| {
                assert_eq!(pruned, builder);
                claim_builder_with_registry(
                    &run_root,
                    Some(&registry_root),
                    pruned,
                    "slot-new",
                    "velnor-job-new",
                )
                .unwrap();
                Ok(1)
            },
            |stopped_builder| {
                stopped.borrow_mut().push(stopped_builder.to_string());
                Ok(true)
            },
            |started_builder| {
                restarted.borrow_mut().push(started_builder.to_string());
                Ok(true)
            },
        );

        assert_eq!(report.pruned, vec![builder.clone()]);
        assert_eq!(*stopped.borrow(), vec![builder.clone()]);
        assert_eq!(*restarted.borrow(), vec![builder.clone()]);
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn corrupt_current_owner_record_fails_closed() {
        let root = temp_root("current-owner-record-fail-closed");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let corrupt = persistent_builder_name("custom", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_test_owner_record(&registry_root, &corrupt);
        std::fs::write(owner_registry_file(&registry_root, &corrupt), b"{torn").unwrap();
        let corrupt_path = owner_registry_file(&registry_root, &corrupt);

        let inspected = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |daemon| {
                inspected.borrow_mut().push(daemon.to_string());
                panic!("missing or corrupt owner record blocks inspection")
            },
            |_| panic!("missing or corrupt owner record blocks stop"),
            |_| panic!("missing or corrupt owner record blocks restart"),
            |_, _observed| panic!("missing or corrupt owner record blocks removal"),
        );

        assert!(inspected.borrow().is_empty());
        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains(&corrupt_path.to_string_lossy().to_string())));
        assert!(owner_registry_file(&registry_root, &corrupt).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_record_is_removed_after_exact_daemon_is_missing() {
        let root = temp_root("orphan-owner-record");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder =
            persistent_builder_name("failed-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_test_owner_record(&registry_root, &builder);
        let owner_path = owner_registry_file(&registry_root, &builder);

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |daemon| {
                assert_eq!(daemon, daemon_container_name(&builder));
                Err(anyhow::Error::new(crate::docker::client::NotFound {
                    object: daemon.to_string(),
                }))
            },
            |_| panic!("orphan record has no daemon to stop"),
            |_| panic!("orphan record has no daemon to restart"),
            |removed_builder, _observed| {
                assert_eq!(removed_builder, builder);
                Ok(true)
            },
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.deleted, vec![builder.clone()]);
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
            || Ok(BTreeSet::new()),
            |daemon| {
                assert_eq!(daemon, daemon_container_name(&builder));
                Err(anyhow::Error::new(crate::docker::client::NotFound {
                    object: daemon.to_string(),
                }))
            },
            |_| panic!("orphan record has no daemon to stop"),
            |_| panic!("orphan record has no daemon to restart"),
            |removed_builder, _observed| {
                assert_eq!(removed_builder, builder);
                Ok(true)
            },
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.deleted, vec![builder.clone()]);
        assert!(!owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn orphan_owner_cleanup_uses_registry_and_requires_job_listing() {
        let root = temp_root("orphan-owner-list-error");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder =
            persistent_builder_name("failed-create", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        ensure_test_owner_record(&registry_root, &builder);
        let owner_path = owner_registry_file(&registry_root, &builder);

        let container_list_error = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Err(anyhow::anyhow!("docker ps failed")),
            |_| panic!("failed listing blocks inspection"),
            |_| panic!("failed listing blocks stop"),
            |_| panic!("failed listing blocks restart"),
            |_, _observed| panic!("failed listing blocks removal"),
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
        ensure_test_owner_record(&registry_root, &active);
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
        ensure_test_owner_record(&registry_root, &mismatched);
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
            || Ok(["velnor-job-live".to_string()].into_iter().collect()),
            |_| panic!("orphan records are absent from the builder listing"),
            |_| panic!("orphan records have no daemon to stop"),
            |_| panic!("orphan records have no daemon to restart"),
            |_, _observed| panic!("orphan records have no daemon to remove"),
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
    fn claims_count_holders_and_report_the_final_release() {
        let root = temp_root("claims");
        let run_root = root.join("run");
        let builder = test_builder();

        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        // Idempotent: a second setup step in the same job holds once.
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);

        // Release only follows a successful network proof.
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || Ok(()),
            || Ok(true),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
            }
        );

        // Releasing again (post after post, teardown after post) is a no-op.
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || panic!("no holder means no network detach retry"),
            || panic!("must not stop without a hold"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
            }
        );
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-never-held",
            || panic!("must not detach without a hold"),
            || panic!("must not stop without a hold"),
        )
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
    fn release_of_unclaimed_setup_journal_entry_needs_no_owner_record() {
        let root = temp_root("release-unclaimed-builder");
        let run_root = root.join("run");
        let builder = test_builder();

        let outcome = release_admitted_builder_after_network_detach(
            &run_root,
            &builder,
            "velnor-job-unadmitted",
            "velnor-net-unadmitted",
            "velnor-job-unadmitted",
            "test-daemon",
        )
        .expect("unclaimed name is an idempotent cleanup no-op");

        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
            }
        );
        assert!(!claims_file(&run_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn admitted_release_keeps_claim_when_job_is_absent_but_service_remains() {
        let root = temp_root("release-service-still-live");
        let run_root = root.join("run");
        let builder = test_builder();
        let job = "velnor-job-service-live";
        let service = "velnor-service-live";
        let required = [job.to_string(), service.to_string()];
        claim_builder_with_registry_and_group(
            &run_root,
            None,
            &builder,
            "slot-service-live",
            job,
            None,
            &required,
            true,
        )
        .unwrap();
        let _containers = use_test_running_container_names([service.to_string()].into());

        let error = release_admitted_builder_after_network_detach(
            &run_root,
            &builder,
            job,
            "velnor-net-service-live",
            job,
            "daemon-service-live",
        )
        .expect_err("service container still has access to the private network");

        assert!(format!("{error:#}").contains("required job/service containers remain live"));
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, job);
        assert!(holders[0]
            .required_containers
            .contains(&service.to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn durable_cleanup_preference_survives_release_recovery() {
        let root = temp_root("release-durable-cleanup-preference");
        let run_root = root.join("run");
        let builder = test_builder();
        let job = "velnor-job-keep-builder";
        let required = [job.to_string()];
        claim_builder_with_registry_and_group(
            &run_root,
            None,
            &builder,
            "slot-keep-builder",
            job,
            None,
            &required,
            false,
        )
        .unwrap();

        // Re-read the atomically persisted claim as a restarted cleanup worker
        // would; the post action does not get to override this stored choice.
        assert!(!builder_holders(&run_root, &builder, None).unwrap()[0].cleanup_on_release);
        let detaches = std::cell::Cell::new(0);
        let stops = std::cell::Cell::new(0);
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || {
                detaches.set(detaches.get() + 1);
                Ok(())
            },
            || {
                stops.set(stops.get() + 1);
                Ok(true)
            },
        )
        .unwrap();

        assert_eq!(detaches.get(), 1, "the per-job network must still detach");
        assert_eq!(stops.get(), 0, "cleanup=false must survive restart");
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: false,
            }
        );
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());
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
                    "required_containers": ["velnor-job-old"],
                    "cleanup_on_release": false,
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
    fn release_keeps_durable_reservation_during_stop() {
        let root = temp_root("release-unlocked");
        let run_root = root.join("run");
        let builder = test_builder();
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();

        // The stop callback runs outside the per-builder flock, but the
        // durable release reservation keeps new holders from joining while
        // the daemon may be stopping.
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || Ok(()),
            || {
                let claims = read_claims(&claims_file(&run_root, &builder))?;
                assert_eq!(claims.releasing.as_deref(), Some("velnor-job-a"));
                assert_eq!(claims.holders.len(), 1);
                assert!(
                    claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").is_err(),
                    "a new job cannot claim a daemon while final stop is in flight"
                );
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
            }
        );
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn teardown_reservation_survives_absent_holder_repair_until_single_release() {
        let root = temp_root("teardown-reaper-interleave");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let job = "velnor-job-teardown-race";
        claim_builder_with_registry_and_group(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-teardown-race",
            job,
            Some(&test_owner_group()),
            &[job.to_string()],
            true,
        )
        .unwrap();

        // Teardown persists intent while the holder still exists. After the
        // containers disappear, a concurrent horizon/pressure repair must
        // preserve the reserved holder rather than consume it as an orphan.
        assert!(
            reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job,)
                .unwrap()
        );
        let reaper_root = run_root.clone();
        let reaper_builder = builder.clone();
        let repaired = std::thread::spawn(move || {
            repair_absent_holders(&reaper_root, &reaper_builder, &BTreeSet::new()).unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].container, job);
        assert!(claim_builder(&run_root, &builder, "slot-next", "velnor-job-next").is_err());

        let detach_count = std::cell::Cell::new(0);
        let stop_count = std::cell::Cell::new(0);
        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || {
                detach_count.set(detach_count.get() + 1);
                let claims = read_claims(&claims_file(&run_root, &builder))?;
                assert_eq!(claims.releasing.as_deref(), Some(job));
                assert!(claims.holders.contains_key(job));
                Ok(())
            },
            || {
                stop_count.set(stop_count.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
            }
        );
        assert_eq!(detach_count.get(), 1);
        assert_eq!(stop_count.get(), 1);
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn teardown_reservation_requires_registered_owner_and_matching_claim() {
        let root = temp_root("teardown-reservation-owner-gate");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let job = "velnor-job-unregistered-holder";

        // A raw claim without a durable owner record cannot authorize teardown
        // to destroy job containers before the release path later fails.
        claim_builder(&run_root, &builder, "slot-raw", job).unwrap();
        let error = reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job)
            .unwrap_err();
        assert!(format!("{error:#}").contains("owner record"));
        assert_eq!(
            read_claims(&claims_file(&run_root, &builder))
                .unwrap()
                .releasing,
            None
        );

        // A registered owner record is insufficient when the claim file
        // does not identify that builder; the reservation write must fail.
        ensure_test_owner_record(&registry_root, &builder);
        let claim_path = claims_file(&run_root, &builder);
        let mut claims = read_claims(&claim_path).unwrap();
        claims.builder.clear();
        write_claims(&claim_path, &claims).unwrap();
        let error = reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job)
            .unwrap_err();
        assert!(format!("{error:#}").contains("expected"));
        assert_eq!(read_claims(&claim_path).unwrap().releasing, None);

        // Missing runtime ownership is likewise a hard stop before teardown.
        std::fs::remove_file(&claim_path).unwrap();
        let error = reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job)
            .unwrap_err();
        assert!(format!("{error:#}").contains("claim"));
        assert!(!claim_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_reaper_preserves_teardown_reservation_until_network_release() {
        let root = temp_root("horizon-teardown-reservation");
        let run_root = root.join("run");
        let registry_root = root.join("owners");
        let builder = test_builder();
        let job = "velnor-job-horizon-teardown";
        ensure_test_owner_record(&registry_root, &builder);
        claim_builder_with_registry_and_group(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-horizon-teardown",
            job,
            Some(&test_owner_group()),
            &[job.to_string()],
            true,
        )
        .unwrap();
        assert!(
            reserve_admitted_builder_release_with(&run_root, &registry_root, &builder, job,)
                .unwrap()
        );

        // Model the interval after service/job rm succeeds but before
        // teardown detaches BuildKit. The reaper sees no live job container;
        // the durable reservation must keep it from pruning or deleting.
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(BTreeSet::new()),
            |_| panic!("reserved builder must not be inspected"),
            |_| panic!("reserved builder must not be stopped"),
            |_| panic!("reserved builder must not be restarted"),
            |_, _observed| panic!("reserved builder must not be deleted"),
        );
        assert!(report.stopped.is_empty());
        assert!(report.deleted.is_empty());
        let claims = read_claims(&claims_file(&run_root, &builder)).unwrap();
        assert_eq!(claims.releasing.as_deref(), Some(job));
        assert!(claims.holders.contains_key(job));

        let events = std::cell::RefCell::new(Vec::new());
        let first_release = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || {
                events.borrow_mut().push("network-detach");
                Ok(())
            },
            || {
                events.borrow_mut().push("daemon-stop");
                Ok(true)
            },
        )
        .unwrap();
        assert!(first_release.removed_last);
        events.borrow_mut().push("claim-release");
        events.borrow_mut().push("job-network-remove");
        let second_release = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || panic!("claim release is idempotent"),
            || panic!("daemon stop is idempotent"),
        )
        .unwrap();
        assert!(!second_release.removed_last);
        assert_eq!(
            *events.borrow(),
            [
                "network-detach",
                "daemon-stop",
                "claim-release",
                "job-network-remove"
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn release_stop_failure_keeps_claim_and_cleanup_preference_for_retry() {
        let root = temp_root("release-stop-retry");
        let run_root = root.join("run");
        let builder = test_builder();
        let job = "velnor-job-stop-retry";
        claim_builder_with_registry_and_group(
            &run_root,
            None,
            &builder,
            "slot-stop-retry",
            job,
            None,
            &[job.to_string()],
            true,
        )
        .unwrap();

        let first_stop = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || Ok(()),
            || Err(anyhow::anyhow!("simulated daemon stop failure")),
        )
        .expect_err("failed stop must leave a durable retry reservation");
        assert!(format!("{first_stop:#}").contains("simulated daemon stop failure"));
        let claims = read_claims(&claims_file(&run_root, &builder)).unwrap();
        assert_eq!(claims.releasing.as_deref(), Some(job));
        assert!(claims.holders[job].cleanup_on_release);
        assert!(claim_builder(&run_root, &builder, "slot-next", "velnor-job-next").is_err());

        // A restarted cleanup worker observes the same preference and
        // reservation, repeats the idempotent detach, then releases only
        // after stop succeeds.
        let claims_after_restart = read_claims(&claims_file(&run_root, &builder)).unwrap();
        assert_eq!(claims_after_restart.releasing.as_deref(), Some(job));
        assert!(claims_after_restart.holders[job].cleanup_on_release);
        let retry_detached = std::cell::Cell::new(false);
        let retried = release_after_network_detach_if_last(
            &run_root,
            &builder,
            job,
            || {
                retry_detached.set(true);
                Ok(())
            },
            || {
                let claims = read_claims(&claims_file(&run_root, &builder))?;
                assert_eq!(claims.releasing.as_deref(), Some(job));
                assert!(claims.holders[job].cleanup_on_release);
                Ok(true)
            },
        )
        .unwrap();
        assert!(retry_detached.get());
        assert_eq!(
            retried,
            ReleaseOutcome {
                removed_last: true,
                stopped: true
            }
        );
        assert!(builder_holders(&run_root, &builder, None)
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn network_detach_failure_keeps_holder_and_blocks_claims_until_retry() {
        let root = temp_root("release-network-detach");
        let run_root = root.join("run");
        let builder = test_builder();
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();

        let error = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || {
                // Reentering the claim path proves the Docker-like callback
                // runs without the per-builder flock. The reservation must
                // keep a competing holder out while it is unlocked.
                assert_eq!(builder_holders(&run_root, &builder, None)?.len(), 1);
                assert!(claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").is_err());
                Err(anyhow::anyhow!("simulated network detach failure"))
            },
            || panic!("failed detach must retain the holder"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("simulated network detach failure"));
        let claims = read_claims(&claims_file(&run_root, &builder)).unwrap();
        assert_eq!(claims.holders.len(), 1);
        assert!(claims.holders.contains_key("velnor-job-a"));
        assert_eq!(claims.releasing.as_deref(), Some("velnor-job-a"));

        let outcome = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || {
                assert_eq!(builder_holders(&run_root, &builder, None)?.len(), 1);
                let claims = read_claims(&claims_file(&run_root, &builder))?;
                assert_eq!(claims.releasing.as_deref(), Some("velnor-job-a"));
                assert!(claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").is_err());
                Ok(())
            },
            || Ok(true),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: true,
                stopped: true,
            }
        );
        let claims = read_claims(&claims_file(&run_root, &builder)).unwrap();
        assert!(claims.holders.is_empty());
        assert!(claims.releasing.is_none());

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
        // drop unknown holds, and release stops before its network callback.
        assert!(builder_holders(&run_root, &builder, None).is_err());
        assert!(claim_builder(&run_root, &builder, "slot-1", "velnor-job-a",).is_err());
        let error = release_after_network_detach_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || panic!("must not detach on a torn file"),
            || panic!("must not stop on a torn file"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("parse"));
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
    fn bounded_admission_enforces_eight_per_stored_group_and_isolates_siblings() {
        let root = temp_root("bounded-builder-admission");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let scope = "trusted";
        let tier = TRUST_TIER_BRANCH;
        let repository = Some("o/r");
        let group = BuilderGroup::new(scope, tier, repository);
        let mut builders = Vec::new();
        for index in 0..MAX_BUILDERS_PER_SCOPE_TIER_REPO {
            let builder =
                persistent_builder_name(&format!("custom-{index}"), scope, tier, repository);
            admit_test_builder(
                &run_root,
                &registry_root,
                &builder,
                &format!("slot-{index}"),
                &format!("velnor-job-{index}"),
                scope,
                tier,
                repository,
            )
            .unwrap();
            builders.push(builder);
        }

        assert_eq!(builders.len(), MAX_BUILDERS_PER_SCOPE_TIER_REPO);
        let stored = read_owner_record(&registry_root, &builders[0])
            .unwrap()
            .unwrap();
        assert_eq!(stored.group, Some(group.clone()));

        let rejected = persistent_builder_name("custom-over-cap", scope, tier, repository);
        let error = admit_test_builder(
            &run_root,
            &registry_root,
            &rejected,
            "slot-over-cap",
            "velnor-job-over-cap",
            scope,
            tier,
            repository,
        )
        .unwrap_err();
        assert!(error.to_string().contains("builder cap exceeded"));
        assert!(!owner_registry_file(&registry_root, &rejected).exists());

        let records = read_owner_records(&registry_root).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.group.as_ref() == Some(&group))
                .count(),
            MAX_BUILDERS_PER_SCOPE_TIER_REPO,
            "a full group of active builders must not be evicted"
        );

        // Each axis belongs to the stored group identity. A sibling repo,
        // trust scope, or trust tier gets its own independent eight-builder cap.
        for (index, (sibling_scope, sibling_tier, sibling_repo)) in [
            ("trusted", TRUST_TIER_BRANCH, Some("o/s")),
            ("untrusted", TRUST_TIER_BRANCH, Some("o/r")),
            ("trusted", TRUST_TIER_RELEASE, Some("o/r")),
        ]
        .into_iter()
        .enumerate()
        {
            let sibling = persistent_builder_name(
                &format!("sibling-{index}"),
                sibling_scope,
                sibling_tier,
                sibling_repo,
            );
            admit_test_builder(
                &run_root,
                &registry_root,
                &sibling,
                &format!("sibling-slot-{index}"),
                &format!("velnor-job-sibling-{index}"),
                sibling_scope,
                sibling_tier,
                sibling_repo,
            )
            .unwrap();
        }

        // A canonical builder name cannot be reassigned to a different
        // scope, tier, or repository after the owner record stores its identity.
        for (sibling_scope, sibling_tier, sibling_repo) in [
            ("untrusted", TRUST_TIER_BRANCH, Some("o/r")),
            ("trusted", TRUST_TIER_RELEASE, Some("o/r")),
            ("trusted", TRUST_TIER_BRANCH, Some("o/s")),
        ] {
            let error = admit_test_builder(
                &run_root,
                &registry_root,
                &builders[0],
                "slot-reassigned",
                "velnor-job-reassigned",
                sibling_scope,
                sibling_tier,
                sibling_repo,
            )
            .unwrap_err();
            assert!(error.to_string().contains("another scope/tier/repository"));
        }

        for (index, builder) in builders.iter().enumerate() {
            let holders = builder_holders(&run_root, builder, None).unwrap();
            assert_eq!(holders.len(), 1);
            assert_eq!(holders[0].container, format!("velnor-job-{index}"));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bounded_admission_keeps_one_active_job_per_private_builder_network() {
        let root = temp_root("private-builder-network-claim");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let first = "velnor-job-private-a";
        let second = "velnor-job-private-b";
        admit_test_builder(
            &run_root,
            &registry_root,
            &builder,
            "slot-private-a",
            first,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
        )
        .unwrap();

        let both_present = [first.to_string(), second.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let error = claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-private-b",
            second,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok(both_present.clone()),
            |_| panic!("active conflicting claim must be rejected before eviction"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("already claimed by active job"));
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, first);

        // Repeated setup by one job remains idempotent.
        claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-private-a",
            first,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok([first.to_string()].into_iter().collect()),
            |_| panic!("same-job claim must not evict another builder"),
        )
        .unwrap();
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);

        // A vanished old job is repaired from the live-container inventory,
        // so its replacement can claim and use the private network.
        claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-private-b",
            second,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok([second.to_string()].into_iter().collect()),
            |_| panic!("stale same-builder claim must not trigger cap eviction"),
        )
        .unwrap();
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, second);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bounded_admission_does_not_treat_registered_missing_claim_as_empty_with_job_active() {
        let root = temp_root("private-builder-missing-claim-active-job");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_test_owner_record(&registry_root, &builder);
        let old_job = "velnor-job-old";
        let current_job = "velnor-job-current";
        let present = [old_job.to_string(), current_job.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();

        let error = claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-current",
            current_job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok(present.clone()),
            |_| panic!("missing claim with a live prior job must block admission"),
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("no /run claim while another job"));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());

        let sole_current_job = claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-current",
            current_job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok([current_job.to_string()].into_iter().collect()),
            |_| panic!("the sole current job may recover its missing claim"),
        )
        .expect_err("an active current job cannot prove a registered missing claim is empty");
        assert!(format!("{sole_current_job:#}").contains("no /run claim while another job"));
        assert!(!claims_file(&run_root, &builder).exists());

        claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-current",
            current_job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || Ok(BTreeSet::new()),
            |_| panic!("the registered missing claim is recoverable after quiescence"),
        )
        .unwrap();
        assert_eq!(
            builder_holders(&run_root, &builder, None).unwrap()[0].container,
            current_job
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bounded_admission_allows_a_first_claim_for_a_new_builder_with_active_job() {
        let root = temp_root("private-builder-first-claim-active-job");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        let current_job = "velnor-job-current";
        let scans = std::cell::Cell::new(0);

        claim_builder_bounded_with_ops(
            &run_root,
            &registry_root,
            &builder,
            "slot-current",
            current_job,
            "trusted",
            TRUST_TIER_BRANCH,
            Some("o/r"),
            || {
                let scan = scans.get();
                scans.set(scan + 1);
                if scan > 0 {
                    let record = read_owner_record(&registry_root, &builder)
                        .unwrap()
                        .expect("first-create identity is durable before the exception");
                    assert!(record.daemon_nodes.is_empty());
                }
                Ok([current_job.to_string()].into_iter().collect())
            },
            |_| panic!("a first claim does not evict a builder"),
        )
        .expect("a durably registered first-create identity can be claimed by the current job");

        assert_eq!(scans.get(), 2, "liveness is rechecked after registration");
        let holders = builder_holders(&run_root, &builder, None).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].container, current_job);
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn claim_symlinks_block_admission_reaping_and_pressure_prune() {
        use std::os::unix::fs::symlink;

        for dangling in [false, true] {
            let root = temp_root(if dangling {
                "claim-dangling-symlink"
            } else {
                "claim-symlink"
            });
            let run_root = root.join("run");
            let registry_root = owner_registry_root(&root.join("lib"));
            let builder = test_builder();
            ensure_test_owner_record(&registry_root, &builder);
            let claim = claims_file(&run_root, &builder);
            std::fs::create_dir_all(claim.parent().unwrap()).unwrap();
            let outside = root.join("outside-claim.json");
            if !dangling {
                std::fs::write(
                    &outside,
                    serde_json::to_vec(&BuilderClaims {
                        builder: builder.clone(),
                        ..BuilderClaims::default()
                    })
                    .unwrap(),
                )
                .unwrap();
            }
            symlink(&outside, &claim).unwrap();
            let present = [
                "velnor-job-active".to_string(),
                "velnor-service-active".to_string(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>();

            let admission = claim_builder_bounded_with_ops(
                &run_root,
                &registry_root,
                &builder,
                "slot-current",
                "velnor-job-current",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("o/r"),
                || Ok(present.clone()),
                |_| panic!("an unsafe claim path blocks before capacity removal"),
            )
            .unwrap_err();
            assert!(format!("{admission:#}").contains("claim"));

            let reaped = reap_idle_builders_with_registry(
                &run_root,
                Some(&registry_root),
                SystemTime::now(),
                || Ok(present.clone()),
                |_| panic!("a claim symlink blocks daemon inspection"),
                |_| panic!("a claim symlink blocks stop"),
                |_| panic!("a claim symlink blocks restart"),
                |_, _observed| panic!("a claim symlink blocks removal"),
            );
            assert!(reaped.deleted.is_empty());
            assert!(reaped
                .failures
                .iter()
                .any(|failure| failure.contains("claim") && failure.contains("regular")));

            let pruned = pressure_prune_builders_with_registry(
                &run_root,
                Some(&registry_root),
                1,
                || Ok(present.clone()),
                |_| panic!("a claim symlink blocks disk-usage inspection"),
                |_| panic!("a claim symlink blocks prune"),
                |_| panic!("a claim symlink blocks stop"),
                |_| panic!("a claim symlink blocks restart"),
            );
            assert!(pruned.pruned.is_empty());
            assert!(pruned
                .failures
                .iter()
                .any(|failure| failure.contains("claim")));
            assert!(claim.symlink_metadata().unwrap().file_type().is_symlink());
            if !dangling {
                assert!(outside.is_file());
            }
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn low_level_claims_bypass_bounded_builder_admission() {
        // `claim_builder` is a low-level claim-file helper. Admission caps
        // belong to `claim_builder_bounded`, so this test intentionally does
        // not make a product-level assertion about builders being unlimited.
        let root = temp_root("many-low-level-claims");
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

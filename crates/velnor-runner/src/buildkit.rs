//! Persistent BuildKit builders: stable names, claims, and reclamation.
//!
//! Every containerized build in every job used to start cold: job teardown
//! force-removed the buildkitd container *and* its `_state` volume on every
//! terminal path, so a workflow's `keep-state: true` could not survive it.
//! Builders are expensive persistent infrastructure being treated as per-job
//! scratch. This module reverses that:
//!
//! * **Stable names.** A builder is keyed by Engine, durable storage root,
//!   trust scope, repository, and requested name instead of the runner slot,
//!   so slots in one local domain reuse the warm daemon and its cache.
//! * **Claims.** Concurrent jobs on one repository share one builder, so the
//!   post step cannot blindly stop it: stopping a shared daemon mid-build
//!   fails the other job. Each setup claims the builder for its job
//!   container; each post and teardown releases; the daemon stops only when
//!   the last holder releases. Claims, owner records, and lifecycle locks
//!   live below the same durable BuildKit domain root.
//! * **Reclamation.** What claims cannot cover, maintenance converges:
//!   disk-pressure reclaim stops and prunes builders with no holders, and
//!   the horizon path deletes builders idle past [`IDLE_DELETE_AFTER`].
//!   Builders are a cache: every destructive
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
//! rewrite. Pre-tier and pre-domain builder names cannot prove current
//! ownership and remain untouched for explicit operator cleanup.
//!
//! Lock protocol: every claim-file mutation holds the claim lock. Setup and
//! release take locks in coordinator → per-builder lifecycle → claim →
//! Engine/volume order. The per-builder lock spans setup and a final-holder
//! stop/recovery, so a new holder cannot start a build during stop. Maintenance
//! takes the coordinator exclusively before claim and volume locks.
//! Lock waits are bounded ([`CLAIM_LOCK_TIMEOUT`]) and every wait, race, and
//! Docker act emits `velnor.buildkit` tracing telemetry. Docker calls carry
//! their own class deadlines through `host_call`.
//!
//! Lifecycle gate: setup and release take the filesystem coordinator shared;
//! the horizon reaper takes it exclusive across owner/container inspection
//! and deletion. Cache-pressure pruning runs under the same exclusive gate
//! from cache reclamation. The per-builder lock protects claim and owner
//! record updates, including register-before-create and delete-after-remove.
//! A queued job may be admitted while the reaper runs, but cannot claim or
//! create a builder until the exclusive pass finishes.
//!
//! Workflow-requested builder names have no per-group ceiling. Claims and
//! durable owner records live below the selected storage root's domain
//! directory. Only owners in this Engine+storage domain are cleanup
//! candidates. Pre-domain resources are left for operator cleanup. Missing
//! claims are pinned because container snapshots cannot prove runner
//! admission quiescence.
//!
//! Torn claims, missing claims, and owner records fail closed, so their
//! builder is never stopped, pruned, or deleted. Every unreadable ownership
//! read logs an ERROR with its path. For a current builder, after all jobs
//! using its Docker endpoint are quiescent, remove the owner record to permit
//! a fresh claim. Doctor surfaces unreadable claim files.
//!
//! Pre-domain persistent names, including the former `unbounded-v1`
//! generation, remain reserved and untouched. They lack enough identity to
//! prove Engine/storage ownership and need explicit operator cleanup.

use anyhow::{Context, Result};
use serde::de::{MapAccess, Visitor};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Reserved prefix that marks the Velnor-owned persistent builder namespace,
/// including generations:
/// `velnor-builder-shared-<generation>-<scope>-<tier>-<repo>[-<requested>]`.
/// Prefix-anchored matching keeps arbitrary external builder names out of
/// Velnor's destroy/orphan decisions.
pub(crate) const PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-";

/// Namespace for builders created with a durable Engine/storage domain.
/// Changing the generation leaves every pre-domain resource for explicit
/// operator cleanup instead of attributing an old owner to this domain.
const CURRENT_PERSISTENT_BUILDER_PREFIX: &str = "velnor-builder-shared-unbounded-v2-";
const BUILDKIT_DOMAIN_TOKEN_HEX_LEN: usize = 32;
const BUILDER_SCOPE_SEGMENT_MAX: usize = 42;
const BUILDER_REPOSITORY_SEGMENT_MAX: usize = 48;
const BUILDER_REQUESTED_SEGMENT_MAX: usize = 40;
const MAX_BUILDKIT_CONTROL_FILE_BYTES: u64 = 16 * 1024 * 1024;

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
/// Exact-ID evidence that Buildx copied its approved daemon config and the
/// resulting daemon passed the worker readiness probe.
const BUILDER_READINESS_DIR: &str = "buildkit-readiness";
const BUILDER_LIFECYCLE_LOCKS_DIR: &str = "builder-lifecycle-locks";
/// Process-shared setup leases bind one in-flight Buildx create/archive/start
/// sequence to its domain, expected config, and immutable daemon ID.
const BUILDER_CREATOR_LEASES_DIR: &str = "buildkit-creator-leases";
const BUILDER_CREATE_TRANSACTIONS_DIR: &str = "buildkit-create-transactions";
const OWNER_REGISTRY_VERSION: u32 = 2;
const BUILDER_READINESS_LEGACY_VERSION: u32 = 1;
const BUILDER_READINESS_VERSION: u32 = 2;
const BUILDER_CREATOR_LEASE_VERSION: u32 = 1;
const BUILDER_CREATE_TRANSACTION_VERSION: u32 = 1;
const LEGACY_CREATE_QUARANTINE_VERSION: u32 = 2;
const MAX_BUILDER_CREATOR_LEASE_BYTES: u64 = 4096;
const MAX_PENDING_BUILDKIT_CREATE_BYTES: u64 = 64 * 1024;

/// Marker file recording the last periodic horizon pass (unix seconds).
const HORIZON_REAP_MARKER: &str = ".last-horizon-reap";
const MAX_HORIZON_REAP_MARKER_BYTES: u64 = 32;

/// Job-local record of the builders this job claimed, so teardown releases
/// exactly what setup claimed even when the post step never ran (cancel).
const JOB_BUILDERS_FILE: &str = "_velnor/buildkit-builders.json";

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
/// canonical repository key keeps one repo's cache out of another's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersistentBuildKitDomain {
    pub(crate) token: String,
    pub(crate) engine_id: String,
    /// Durable, process-shared root derived from the selected StorageLayout.
    /// Domain ledgers live below `root`; Engine-wide volume locks live below
    /// this path and deliberately omit the storage UUID from their key.
    pub(crate) identity_root: PathBuf,
    pub(crate) root: PathBuf,
}

impl PersistentBuildKitDomain {
    pub(crate) fn from_identities(
        identity_root: &Path,
        storage_id: &str,
        engine_id: &str,
    ) -> Result<Self> {
        if storage_id.trim().is_empty() || engine_id.trim().is_empty() {
            anyhow::bail!("BuildKit storage and Docker Engine identities must be nonempty");
        }
        let root_dir =
            crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(identity_root)
                .with_context(|| {
                    format!("secure BuildKit storage root {}", identity_root.display())
                })?;
        let (device, inode) = root_dir
            .physical_identity()
            .context("read physical BuildKit storage-root identity")?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"velnor-buildkit-domain-v1\0");
        hash_component(&mut hasher, b"storage", Some(storage_id));
        hash_component(&mut hasher, b"engine", Some(engine_id));
        hash_component_bytes(
            &mut hasher,
            b"storage-root-device",
            Some(&device.to_be_bytes()),
        );
        hash_component_bytes(
            &mut hasher,
            b"storage-root-inode",
            Some(&inode.to_be_bytes()),
        );
        let token = hasher.finalize().to_hex()[..BUILDKIT_DOMAIN_TOKEN_HEX_LEN].to_string();
        let root = identity_root.join("buildkit-domains").join(&token);
        #[cfg(unix)]
        crate::fs_copy::NoFollowDestinationDir::open_trusted_rooted_destination(
            identity_root,
            Path::new("buildkit-domains").join(&token).as_path(),
        )
        .with_context(|| format!("secure BuildKit domain directory {}", root.display()))?;
        #[cfg(not(unix))]
        anyhow::bail!(
            "persistent BuildKit domain directories require unix no-follow filesystem support"
        );
        let domain_dir = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&root)
            .with_context(|| format!("open BuildKit domain directory {}", root.display()))?;
        domain_dir
            .open_relative_directory(Path::new(CLAIMS_DIR))
            .context("initialize BuildKit claims directory")?;
        domain_dir
            .open_relative_directory(Path::new(OWNER_REGISTRY_DIR))
            .context("initialize BuildKit owner registry directory")?;
        domain_dir
            .open_relative_directory(Path::new(BUILDER_READINESS_DIR))
            .context("initialize BuildKit readiness directory")?;
        domain_dir
            .open_relative_directory(Path::new(BUILDER_LIFECYCLE_LOCKS_DIR))
            .context("initialize BuildKit lifecycle lock directory")?;
        domain_dir
            .open_relative_directory(Path::new(BUILDER_CREATOR_LEASES_DIR))
            .context("initialize BuildKit creator lease directory")?;
        domain_dir
            .open_relative_directory(Path::new(BUILDER_CREATE_TRANSACTIONS_DIR))
            .context("initialize BuildKit create transaction directory")?;
        Ok(Self {
            token,
            engine_id: engine_id.to_string(),
            identity_root: identity_root.to_path_buf(),
            root,
        })
    }

    pub(crate) fn resolve() -> Result<Self> {
        let layout = crate::storage::selected_layout()
            .or_else(crate::storage::StorageLayout::resolve)
            .context("persistent BuildKit requires the selected Velnor storage layout")?;
        Self::resolve_from_layout(layout)
    }

    pub(crate) fn try_resolve() -> Result<Option<Self>> {
        let Some(layout) =
            crate::storage::selected_layout().or_else(crate::storage::StorageLayout::resolve)
        else {
            return Ok(None);
        };
        Self::resolve_from_layout(layout).map(Some)
    }

    pub(crate) fn resolve_from_layout(layout: crate::storage::StorageLayout) -> Result<Self> {
        let identity_root = layout.buildkit_identity_root();
        let storage_id = crate::storage::ensure_buildkit_storage_identity(&identity_root)?;
        let endpoint = crate::docker::engine::resolve_docker_endpoint()
            .context("resolve Docker endpoint for BuildKit domain")?;
        let engine_id = crate::docker::engine::daemon_identity_blocking(&endpoint.socket)
            .map(|identity| identity.id)
            .filter(|identity| !identity.trim().is_empty())
            .context("Docker Engine /info.ID is unavailable; persistent BuildKit is disabled")?;
        Self::from_identities(&identity_root, &storage_id, &engine_id)
    }
}

pub(crate) fn persistent_builder_name_for_domain(
    domain_token: &str,
    requested: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
) -> String {
    assert_domain_token(domain_token);
    let scope = bounded_builder_segment("s", Some(scope), BUILDER_SCOPE_SEGMENT_MAX);
    let tier = sanitize_builder_segment(tier);
    let repo = bounded_builder_segment("r", repository, BUILDER_REPOSITORY_SEGMENT_MAX);
    let requested = requested.trim().to_ascii_lowercase();
    let requested_default = requested.is_empty() || requested == DEFAULT_REQUESTED_NAME;
    let base = format!("{CURRENT_PERSISTENT_BUILDER_PREFIX}d{domain_token}-{scope}-{tier}-{repo}");
    let name = if requested_default {
        base
    } else {
        format!(
            "{base}-{}",
            bounded_builder_segment("n", Some(&requested), BUILDER_REQUESTED_SEGMENT_MAX)
        )
    };
    debug_assert!(daemon_state_volume(&name).len() <= 255);
    name
}

fn bounded_builder_segment(tag: &str, value: Option<&str>, max_len: usize) -> String {
    let identity = value.map(str::trim).filter(|value| !value.is_empty());
    let slug = identity
        .as_deref()
        .map(|value| sanitize_builder_segment(&value.to_ascii_lowercase()))
        .unwrap_or_else(|| "none".to_string());
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"velnor-buildkit-name-component-v1\0");
    // Keep the readable part Buildx-safe lowercase, but preserve original
    // identity bytes in the digest. Trust scope keys are case-sensitive.
    hash_component(&mut hasher, tag.as_bytes(), identity);
    let digest = hasher.finalize().to_hex();
    // A 128-bit component digest keeps unrelated long identities out of the
    // same builder while the fixed segment budgets keep Buildx's derived
    // daemon container and state-volume names under Docker's 255-byte limit.
    let suffix = format!("-{tag}{}", &digest[..32]);
    let readable_budget = max_len.saturating_sub(suffix.len());
    let readable = slug.chars().take(readable_budget).collect::<String>();
    format!("{readable}{suffix}")
}

fn hash_component(hasher: &mut blake3::Hasher, tag: &[u8], value: Option<&str>) {
    hash_component_bytes(hasher, tag, value.map(str::as_bytes));
}

fn hash_component_bytes(hasher: &mut blake3::Hasher, tag: &[u8], value: Option<&[u8]>) {
    hasher.update(&(tag.len() as u64).to_be_bytes());
    hasher.update(tag);
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&(value.len() as u64).to_be_bytes());
            hasher.update(value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

#[cfg(test)]
pub(crate) const TEST_BUILDKIT_DOMAIN_TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[cfg(test)]
pub(crate) fn persistent_builder_name(
    requested: &str,
    scope: &str,
    tier: &str,
    repository: Option<&str>,
) -> String {
    persistent_builder_name_for_domain(
        TEST_BUILDKIT_DOMAIN_TOKEN,
        requested,
        scope,
        tier,
        repository,
    )
}

fn assert_domain_token(token: &str) {
    debug_assert_eq!(token.len(), BUILDKIT_DOMAIN_TOKEN_HEX_LEN);
    debug_assert!(token
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
}

/// Return the stable Engine/storage token embedded in a current-generation
/// builder name. Old unscoped generations deliberately have no token and
/// therefore cannot be adopted, released, or reaped by domain-aware code.
pub(crate) fn persistent_builder_domain_token(builder: &str) -> Option<&str> {
    let rest = builder.strip_prefix(CURRENT_PERSISTENT_BUILDER_PREFIX)?;
    let rest = rest.strip_prefix('d')?;
    let (token, _) = rest.split_once('-')?;
    (token.len() == BUILDKIT_DOMAIN_TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(token)
}

pub(crate) fn is_current_domain_builder_name(builder: &str, domain_token: &str) -> bool {
    persistent_builder_domain_token(builder) == Some(domain_token)
}

pub(crate) fn is_current_domained_persistent_builder(builder: &str) -> bool {
    persistent_builder_domain_token(builder).is_some()
}

/// True when a builder name uses Velnor's reserved persistent marker. This
/// blocks guest adoption and generic cleanup; only exact current-domain
/// records grant maintenance authority. Retired names remain quarantined for
/// explicit operator cleanup because their Engine/storage identity is absent.
pub(crate) fn is_persistent_builder_name(builder: &str) -> bool {
    builder.starts_with(PERSISTENT_BUILDER_PREFIX)
}

/// True when a buildkitd container or state volume belongs to a persistent
/// builder. Generated object names embed the builder name
/// (`buildx_buildkit_<builder><node>[_state]`), so the marker survives the
/// embedding. Retired generations and generated numeric child nodes stay
/// quarantined for explicit operator cleanup rather than infer ownership.
/// Custom `--node` names carry no parent-domain proof; the Docker lease denies
/// their privileged Buildx create path instead of adopting them here.
pub(crate) fn is_persistent_builder_object(name: &str) -> bool {
    name.split(',')
        .any(|name| buildkit_daemon_builder_name(name).is_some_and(is_persistent_builder_name))
}

/// Return the structurally encoded Velnor Buildx builder part of a daemon
/// container or state volume name. This recognizes node indexes only for
/// cleanup classification; lifecycle authority still comes from exact owner
/// records and node-zero attestation.
pub(crate) fn buildkit_daemon_builder_name(name: &str) -> Option<&str> {
    let name = name.trim().trim_start_matches('/');
    let container = name.strip_suffix("_state").unwrap_or(name);
    let rest = container.strip_prefix(DAEMON_CONTAINER_PREFIX)?;
    let node_start = rest
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(index, _)| index)?;
    let builder = &rest[..node_start];
    let node = &rest[node_start..];
    (builder
        .strip_prefix("velnor-builder-")
        .is_some_and(|scope| !scope.is_empty())
        && node.bytes().all(|byte| byte.is_ascii_digit()))
    .then_some(builder)
}

/// Recognize Buildx daemon rows using exact name structure. Docker's `name=`
/// filter is substring-based, so destructive callers must use this after the
/// listing query to avoid treating guest names containing the marker as a
/// BuildKit daemon.
pub(crate) fn is_velnor_buildkit_daemon_name(names: &str) -> bool {
    names
        .split(',')
        .any(|name| buildkit_daemon_builder_name(name).is_some())
}

fn sanitize_builder_segment(value: &str) -> String {
    crate::container::sanitize_store_key(value.trim())
}

/// `owner/repo` becomes `owner_repo`, sanitized for builder names, temp
/// paths, and claim files alike.
/// Prefix buildx derives docker-container daemon names from:
/// `buildx_buildkit_<builder>0`. Persistent builder names start with
/// `velnor-builder-`, so the existing
/// [`crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX`] listings keep
/// matching persistent daemons with no changes.
const DAEMON_CONTAINER_PREFIX: &str = "buildx_buildkit_";

/// The docker-container daemon's container name for a single-node builder.
/// Velnor owns the generated node 0 only. The lease rejects reserved numeric
/// children and denies Buildx's privileged generic create path for custom
/// `--node` names, so current Buildx append requests cannot create untracked
/// daemons. Pre-existing custom-name objects lack this reserved numeric shape;
/// domain maintenance does not adopt them and leaves unproven objects for
/// explicit operator cleanup.
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
struct BuilderClaims {
    holders: BTreeMap<String, BuilderHolder>,
    /// Full builder name this file guards. The file name is a full-name
    /// digest, so the reverse mapping lives here for ownership checks.
    builder: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderOwnerRecord {
    version: u32,
    builder: String,
    phase: BuilderOwnerPhase,
}

#[derive(serde::Deserialize)]
struct BuilderOwnerIdentity {
    version: u32,
    builder: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BuilderOwnerPhase {
    Active,
    Deleting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BuilderReadinessPhase {
    Stopping,
    Stopped,
    Starting,
    Ready,
}

/// Historical readiness schema written before durable start epochs existed.
/// It is parsed only by the explicit locked promotion path below. Ordinary
/// readers stay v2-only so a stale v1 proof can never authorize a request.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderReadinessRecordV1 {
    version: u32,
    builder: String,
    domain_token: String,
    state_volume: String,
    container_id: String,
    config_fingerprint: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderReadinessRecord {
    version: u32,
    builder: String,
    domain_token: String,
    state_volume: String,
    container_id: String,
    config_fingerprint: String,
    /// Monotonic per-builder start attempt. An older worker probe cannot
    /// republish readiness after a later start has invalidated it.
    epoch: u64,
    phase: BuilderReadinessPhase,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuilderCreatorLeaseRecord {
    version: u32,
    builder: String,
    domain_token: String,
    generation: u64,
    config_fingerprint: String,
    container_id: Option<String>,
    archived_config_fingerprint: Option<String>,
}

/// RAII ownership of one process-shared BuildKit create transaction. Its
/// flock proves liveness; the sibling record binds the in-flight creator to
/// the exact domain/config/container until readiness is durably published.
pub(crate) struct PersistentBuildKitCreatorLease {
    domain: PersistentBuildKitDomain,
    builder: String,
    config_fingerprint: String,
    generation: u64,
    _lock: std::fs::File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingBuildKitCreatePhase {
    Dispatched,
    ContainerBound,
    ArchiveAccepted,
    Started,
    ExistingReady,
}

/// Durable transaction evidence for a potentially late Docker
/// ContainerCreate. The record remains until the exact container has passed
/// archive, start, and worker-readiness proof. `/run` only serializes the
/// Engine-volume lock; this journal survives reboot with Docker state.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingBuildKitCreateTransaction {
    pub(crate) version: u32,
    pub(crate) transaction_id: String,
    pub(crate) engine_id: String,
    pub(crate) domain_token: String,
    pub(crate) builder: String,
    pub(crate) generation: u64,
    pub(crate) config_fingerprint: String,
    pub(crate) state_volume: String,
    pub(crate) container_name: String,
    /// Exact forwarded request bytes, retained for audit. Recovery compares
    /// the normalized shape below because the creator job label varies by job.
    pub(crate) request_sha256: String,
    pub(crate) normalized_shape_sha256: String,
    /// Normalized, label-stable create request projection. Retained so a
    /// crash before first inspect binding can compare the Docker object with
    /// create intent rather than trusting only its name and labels.
    pub(crate) expected_create_shape: serde_json::Value,
    pub(crate) expected_image_id: String,
    pub(crate) expects_config: bool,
    pub(crate) phase: PendingBuildKitCreatePhase,
    pub(crate) container_id: Option<String>,
    pub(crate) attested_shape_sha256: Option<String>,
    pub(crate) archived_config_fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingBuildKitCreateAccess {
    pub(crate) builder: String,
    pub(crate) generation: u64,
    pub(crate) transaction_id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPendingBuildKitCreateQuarantine {
    version: u32,
    engine_id: String,
    domain_token: String,
    volume: String,
    legacy_marker_sha256: String,
}

/// `None` is an explicit no-config mode; an auto-discovered default config is
/// rejected because its archive cannot match this mode. The configured value
/// is limited to the single approved mirror config. Buildx
/// v0.36.1/Pelletier TOML v2.3.1 reserializes that input to the archived bytes
/// below, so this fingerprint binds the exact payload BuildKit receives
/// rather than the source spelling supplied to Buildx.
pub(crate) fn persistent_buildkit_config_fingerprint(config: Option<&str>) -> Result<String> {
    let Some(config) = config.filter(|config| !config.trim().is_empty()) else {
        return Ok("no-config-v1".to_owned());
    };
    if !crate::docker_lease::is_approved_persistent_buildkit_config(config) {
        anyhow::bail!("persistent BuildKit config is not approved");
    }
    Ok("sha256:333c40f4fee6f473bee299aed751bb40967b9e8315af90378a5de0b5dc69a76b".to_owned())
}

fn builder_readiness_file(domain: &PersistentBuildKitDomain, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    domain
        .root
        .join(BUILDER_READINESS_DIR)
        .join(format!("{digest}.json"))
}

fn builder_creator_file(domain: &PersistentBuildKitDomain, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    domain
        .root
        .join(BUILDER_CREATOR_LEASES_DIR)
        .join(format!("{digest}.json"))
}

fn builder_create_transaction_file(domain: &PersistentBuildKitDomain, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    domain
        .root
        .join(BUILDER_CREATE_TRANSACTIONS_DIR)
        .join(format!("{digest}.json"))
}

fn legacy_create_quarantine_file(domain: &PersistentBuildKitDomain, volume: &str) -> PathBuf {
    let digest = blake3::hash(volume.as_bytes()).to_hex();
    domain
        .root
        .join(BUILDER_CREATE_TRANSACTIONS_DIR)
        .join(format!("legacy-{digest}.json"))
}

fn validate_pending_buildkit_create(
    record: &PendingBuildKitCreateTransaction,
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<()> {
    let digest = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    let expected_shape_bytes = serde_json::to_vec(&record.expected_create_shape)
        .context("serialize pending BuildKit expected create shape")?;
    let expected_shape_digest = Sha256::digest(expected_shape_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if record.version != BUILDER_CREATE_TRANSACTION_VERSION
        || record.transaction_id.is_empty()
        || record.engine_id != domain.engine_id
        || record.domain_token != domain.token
        || record.builder != builder
        || record.state_volume != daemon_state_volume(builder)
        || record.container_name != daemon_container_name(builder)
        || record.config_fingerprint.trim().is_empty()
        || !digest(&record.request_sha256)
        || !digest(&record.normalized_shape_sha256)
        || record.normalized_shape_sha256 != expected_shape_digest
        || record
            .expected_create_shape
            .get("name")
            .and_then(serde_json::Value::as_str)
            != Some(record.container_name.as_str())
        || record
            .expected_create_shape
            .get("volume")
            .and_then(serde_json::Value::as_str)
            != Some(record.state_volume.as_str())
        || record
            .expected_create_shape
            .get("image_id")
            .and_then(serde_json::Value::as_str)
            != Some(record.expected_image_id.as_str())
        || record
            .expected_create_shape
            .get("create")
            .and_then(serde_json::Value::as_object)
            .is_none()
        || !record.expected_image_id.starts_with("sha256:")
    {
        anyhow::bail!("pending BuildKit create transaction identity is invalid for {builder}");
    }
    if let Some(container_id) = record.container_id.as_deref()
        && (container_id.trim().is_empty() || container_id.chars().any(char::is_control))
    {
        anyhow::bail!("pending BuildKit create transaction has an invalid container ID");
    }
    if let Some(shape) = record.attested_shape_sha256.as_deref()
        && !digest(shape)
    {
        anyhow::bail!("pending BuildKit create has an invalid inspected-shape fingerprint");
    }
    if let Some(archive) = record.archived_config_fingerprint.as_deref()
        && archive != record.config_fingerprint
    {
        anyhow::bail!("pending BuildKit create archive fingerprint does not match config");
    }
    match record.phase {
        PendingBuildKitCreatePhase::Dispatched if record.container_id.is_some() => {
            anyhow::bail!("dispatched BuildKit create already has a container ID");
        }
        PendingBuildKitCreatePhase::ContainerBound
            if record.container_id.is_none() || record.attested_shape_sha256.is_none() =>
        {
            anyhow::bail!("bound BuildKit create is missing ID or full shape proof");
        }
        PendingBuildKitCreatePhase::ArchiveAccepted | PendingBuildKitCreatePhase::Started
            if record.container_id.is_none() || record.archived_config_fingerprint.is_none() =>
        {
            anyhow::bail!("BuildKit create phase is missing ID or archive proof");
        }
        PendingBuildKitCreatePhase::ExistingReady
            if record.container_id.is_none()
                || record.attested_shape_sha256.is_none()
                || record.archived_config_fingerprint.is_none() =>
        {
            anyhow::bail!("existing BuildKit readiness phase is missing ID or shape proof");
        }
        _ => {}
    }
    Ok(())
}

/// Read the durable pending-create record using a bounded no-follow regular
/// file open. Callers must retain fail-closed behavior for malformed records.
pub(crate) fn pending_buildkit_create_transaction(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<Option<PendingBuildKitCreateTransaction>> {
    let path = builder_create_transaction_file(domain, builder);
    let Some(bytes) =
        read_control_file_no_follow_with_limit(&path, MAX_PENDING_BUILDKIT_CREATE_BYTES)?
    else {
        return Ok(None);
    };
    let record: PendingBuildKitCreateTransaction =
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "parse pending BuildKit create transaction {}",
                path.display()
            )
        })?;
    validate_pending_buildkit_create(&record, domain, builder)?;
    Ok(Some(record))
}

/// Persist the intent before ContainerCreate can reach dockerd. The caller
/// already owns this builder's creator flock, so an existing journal is an
/// unresolved transaction and cannot be overwritten by a retry.
pub(crate) fn begin_pending_buildkit_create_transaction(
    domain: &PersistentBuildKitDomain,
    record: PendingBuildKitCreateTransaction,
) -> Result<()> {
    validate_pending_buildkit_create(&record, domain, &record.builder)?;
    let directory = open_builder_creator_directory(domain)?;
    let _state_lock = lock_creator_state_file(&directory, &record.builder)?;
    if !creator_lock_is_held(&directory, &record.builder)? {
        anyhow::bail!("pending BuildKit create has no live creator lease");
    }
    if legacy_pending_buildkit_create_is_quarantined(domain, &record.state_volume)? {
        anyhow::bail!("legacy BuildKit create remains quarantined for operator repair");
    }
    let path = builder_create_transaction_file(domain, &record.builder);
    if read_control_file_no_follow_with_limit(&path, MAX_PENDING_BUILDKIT_CREATE_BYTES)?.is_some() {
        anyhow::bail!(
            "pending BuildKit create transaction already exists for {}",
            record.builder
        );
    }
    let bytes =
        serde_json::to_vec(&record).context("encode pending BuildKit create transaction")?;
    write_atomic_document(&path, &bytes)
}

fn update_pending_buildkit_create_transaction(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    update: impl FnOnce(&mut PendingBuildKitCreateTransaction) -> Result<()>,
) -> Result<PendingBuildKitCreateTransaction> {
    let directory = open_builder_creator_directory(domain)?;
    let _state_lock = lock_creator_state_file(&directory, builder)?;
    if !creator_lock_is_held(&directory, builder)? {
        anyhow::bail!("pending BuildKit create transaction has no live creator lease");
    }
    let path = builder_create_transaction_file(domain, builder);
    let bytes = read_control_file_no_follow_with_limit(&path, MAX_PENDING_BUILDKIT_CREATE_BYTES)?
        .context("pending BuildKit create transaction is missing")?;
    let mut record: PendingBuildKitCreateTransaction = serde_json::from_slice(&bytes)
        .with_context(|| {
            format!(
                "parse pending BuildKit create transaction {}",
                path.display()
            )
        })?;
    validate_pending_buildkit_create(&record, domain, builder)?;
    if record.transaction_id != transaction_id {
        anyhow::bail!("pending BuildKit create transaction ID changed");
    }
    update(&mut record)?;
    validate_pending_buildkit_create(&record, domain, builder)?;
    let bytes = serde_json::to_vec(&record).context("encode pending BuildKit create update")?;
    write_atomic_document(&path, &bytes)?;
    Ok(record)
}

pub(crate) fn bind_pending_buildkit_create_container(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    container_id: &str,
    attested_shape_sha256: &str,
) -> Result<PendingBuildKitCreateTransaction> {
    update_pending_buildkit_create_transaction(domain, builder, transaction_id, |record| {
        if record
            .container_id
            .as_deref()
            .is_some_and(|existing| existing != container_id)
        {
            anyhow::bail!("pending BuildKit create was already bound to another container ID");
        }
        if record
            .attested_shape_sha256
            .as_deref()
            .is_some_and(|existing| existing != attested_shape_sha256)
        {
            anyhow::bail!("pending BuildKit create shape changed after binding");
        }
        if record.phase == PendingBuildKitCreatePhase::Dispatched {
            record.phase = PendingBuildKitCreatePhase::ContainerBound;
        }
        record.container_id = Some(container_id.to_owned());
        record.attested_shape_sha256 = Some(attested_shape_sha256.to_owned());
        Ok(())
    })
}

pub(crate) fn record_pending_buildkit_create_archive(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    container_id: &str,
    archive_fingerprint: &str,
) -> Result<PendingBuildKitCreateTransaction> {
    update_pending_buildkit_create_transaction(domain, builder, transaction_id, |record| {
        if record.container_id.as_deref() != Some(container_id)
            || record.config_fingerprint != archive_fingerprint
        {
            anyhow::bail!("BuildKit archive does not match pending transaction identity");
        }
        record.archived_config_fingerprint = Some(archive_fingerprint.to_owned());
        record.phase = PendingBuildKitCreatePhase::ArchiveAccepted;
        Ok(())
    })
}

pub(crate) fn mark_pending_buildkit_create_started(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    container_id: &str,
) -> Result<PendingBuildKitCreateTransaction> {
    update_pending_buildkit_create_transaction(domain, builder, transaction_id, |record| {
        if record.container_id.as_deref() != Some(container_id)
            || record.archived_config_fingerprint.as_deref()
                != Some(record.config_fingerprint.as_str())
        {
            anyhow::bail!("BuildKit start lacks matching immutable ID and archive proof");
        }
        record.phase = PendingBuildKitCreatePhase::Started;
        Ok(())
    })
}

pub(crate) fn mark_pending_buildkit_create_existing_ready(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    container_id: &str,
    state: &str,
) -> Result<PendingBuildKitCreateTransaction> {
    if state != "running" {
        anyhow::bail!("existing BuildKit readiness is valid only for a running daemon");
    }
    let transaction = pending_buildkit_create_transaction(domain, builder)?
        .context("pending BuildKit create transaction is missing")?;
    if transaction.transaction_id != transaction_id
        || transaction.container_id.as_deref() != Some(container_id)
        || !builder_readiness_matches(
            domain,
            builder,
            container_id,
            &transaction.config_fingerprint,
        )?
    {
        anyhow::bail!("existing BuildKit ready state does not match this container");
    }
    update_pending_buildkit_create_transaction(domain, builder, transaction_id, |record| {
        if record.container_id.as_deref() != Some(container_id) {
            anyhow::bail!("existing BuildKit readiness does not match transaction ID");
        }
        record.archived_config_fingerprint = Some(record.config_fingerprint.clone());
        record.phase = PendingBuildKitCreatePhase::ExistingReady;
        Ok(())
    })
}

/// Remove a journal only after the durable readiness record already matches
/// its transaction. A crash between readiness publication and this unlink is
/// therefore idempotently recoverable.
pub(crate) fn finish_pending_buildkit_create_transaction(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    transaction_id: &str,
    container_id: &str,
    config_fingerprint: &str,
) -> Result<()> {
    let directory = open_builder_creator_directory(domain)?;
    let _state_lock = lock_creator_state_file(&directory, builder)?;
    if !creator_lock_is_held(&directory, builder)? {
        anyhow::bail!("cannot finish BuildKit transaction without its live creator lease");
    }
    let path = builder_create_transaction_file(domain, builder);
    let bytes = read_control_file_no_follow_with_limit(&path, MAX_PENDING_BUILDKIT_CREATE_BYTES)?
        .context("pending BuildKit create transaction is missing")?;
    let record: PendingBuildKitCreateTransaction =
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "parse pending BuildKit create transaction {}",
                path.display()
            )
        })?;
    validate_pending_buildkit_create(&record, domain, builder)?;
    if record.transaction_id != transaction_id
        || record.container_id.as_deref() != Some(container_id)
        || record.config_fingerprint != config_fingerprint
        || record.archived_config_fingerprint.as_deref() != Some(config_fingerprint)
        || !builder_readiness_matches(domain, builder, container_id, config_fingerprint)?
    {
        anyhow::bail!("BuildKit transaction lacks matching durable readiness proof");
    }
    let domain_dir = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&domain.root)?;
    let transactions =
        domain_dir.open_relative_directory(Path::new(BUILDER_CREATE_TRANSACTIONS_DIR))?;
    transactions.remove_tree_entry(std::ffi::OsStr::new(
        path.file_name().context("transaction filename missing")?,
    ))?;
    transactions
        .sync_directory()
        .context("sync completed BuildKit transaction removal")
}

/// Preserve a pre-journal runtime marker durably before removing it. Such a
/// marker has no config or shape fingerprint and is intentionally not
/// recoverable; it remains quarantined for operator repair.
pub(crate) fn quarantine_legacy_pending_buildkit_create(
    domain: &PersistentBuildKitDomain,
    volume: &str,
    marker_bytes: &[u8],
) -> Result<()> {
    let path = legacy_create_quarantine_file(domain, volume);
    let marker_sha256 = Sha256::digest(marker_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if let Some(bytes) = read_control_file_no_follow_with_limit(&path, 4096)? {
        let existing: LegacyPendingBuildKitCreateQuarantine = serde_json::from_slice(&bytes)
            .context("parse existing legacy BuildKit create quarantine")?;
        validate_legacy_create_quarantine(&existing, domain, volume, Some(&marker_sha256))?;
        return Ok(());
    }
    let record = LegacyPendingBuildKitCreateQuarantine {
        version: LEGACY_CREATE_QUARANTINE_VERSION,
        engine_id: domain.engine_id.clone(),
        domain_token: domain.token.clone(),
        volume: volume.to_owned(),
        legacy_marker_sha256: marker_sha256,
    };
    let bytes = serde_json::to_vec(&record).context("encode legacy BuildKit create quarantine")?;
    write_atomic_document(&path, &bytes)
}

fn validate_legacy_create_quarantine(
    record: &LegacyPendingBuildKitCreateQuarantine,
    domain: &PersistentBuildKitDomain,
    volume: &str,
    expected_marker_sha256: Option<&str>,
) -> Result<()> {
    let digest_is_canonical = record.legacy_marker_sha256.len() == 64
        && record
            .legacy_marker_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if record.version != LEGACY_CREATE_QUARANTINE_VERSION
        || record.engine_id != domain.engine_id
        || record.domain_token != domain.token
        || record.volume != volume
        || !digest_is_canonical
        || expected_marker_sha256.is_some_and(|expected| expected != record.legacy_marker_sha256)
    {
        anyhow::bail!("legacy BuildKit create quarantine identity or marker digest mismatch");
    }
    Ok(())
}

pub(crate) fn legacy_pending_buildkit_create_is_quarantined(
    domain: &PersistentBuildKitDomain,
    volume: &str,
) -> Result<bool> {
    let path = legacy_create_quarantine_file(domain, volume);
    let Some(bytes) = read_control_file_no_follow_with_limit(&path, 4096)? else {
        return Ok(false);
    };
    let record: LegacyPendingBuildKitCreateQuarantine =
        serde_json::from_slice(&bytes).context("parse legacy BuildKit create quarantine")?;
    validate_legacy_create_quarantine(&record, domain, volume, None)?;
    Ok(true)
}

fn builder_creator_lock_name(builder: &str) -> String {
    format!("{}.lock", blake3::hash(builder.as_bytes()).to_hex())
}

fn builder_creator_state_lock_name(builder: &str) -> String {
    format!("{}.state.lock", blake3::hash(builder.as_bytes()).to_hex())
}

fn open_builder_creator_directory(
    domain: &PersistentBuildKitDomain,
) -> Result<crate::fs_copy::NoFollowDestinationDir> {
    crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&domain.root)
        .with_context(|| format!("open BuildKit domain {} safely", domain.root.display()))?
        .open_relative_directory(Path::new(BUILDER_CREATOR_LEASES_DIR))
        .context("open BuildKit creator lease directory safely")
}

fn lock_creator_state_file(
    directory: &crate::fs_copy::NoFollowDestinationDir,
    builder: &str,
) -> Result<std::fs::File> {
    let file = directory
        .open_or_create_lock_file(std::ffi::OsStr::new(&builder_creator_state_lock_name(
            builder,
        )))
        .context("open BuildKit creator state lock")?;
    let started = Instant::now();
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if started.elapsed() < CLAIM_LOCK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                anyhow::bail!("timed out locking BuildKit creator state for {builder}");
            }
            Err(error) => return Err(anyhow::Error::new(error).context("lock creator state")),
        }
    }
}

fn creator_lock_is_held(
    directory: &crate::fs_copy::NoFollowDestinationDir,
    builder: &str,
) -> Result<bool> {
    let Some(file) = directory
        .open_relative_file_if_exists(Path::new(&builder_creator_lock_name(builder)))
        .context("open BuildKit creator lease lock safely")?
    else {
        return Ok(false);
    };
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(false),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(true),
        Err(error) => Err(anyhow::Error::new(error).context("probe BuildKit creator lease")),
    }
}

fn validate_creator_record(
    record: &BuilderCreatorLeaseRecord,
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<()> {
    if record.version != BUILDER_CREATOR_LEASE_VERSION
        || record.builder != builder
        || record.domain_token != domain.token
        || record.generation == 0
        || record.config_fingerprint.trim().is_empty()
    {
        anyhow::bail!("BuildKit creator lease identity does not match {builder}");
    }
    Ok(())
}

fn read_live_builder_creator(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<Option<BuilderCreatorLeaseRecord>> {
    let path = builder_creator_file(domain, builder);
    let Some(bytes) =
        read_control_file_no_follow_with_limit(&path, MAX_BUILDER_CREATOR_LEASE_BYTES)?
    else {
        return Ok(None);
    };
    let record: BuilderCreatorLeaseRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse BuildKit creator lease {}", path.display()))?;
    validate_creator_record(&record, domain, builder)
        .with_context(|| format!("validate BuildKit creator lease {}", path.display()))?;
    if !creator_lock_is_held(&open_builder_creator_directory(domain)?, builder)? {
        return Ok(None);
    }
    Ok(Some(record))
}

/// A process-shared lock protects the whole create/archive/start transition.
/// A stale record is never authority: readers also require the stable lock
/// inode to be held by a live process.
pub(crate) fn begin_persistent_builder_creator_lease(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
    generation: u64,
) -> Result<PersistentBuildKitCreatorLease> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse creator lease for a BuildKit builder from another domain");
    }
    if config_fingerprint.trim().is_empty() {
        anyhow::bail!("BuildKit creator lease requires an exact config fingerprint");
    }
    let directory = open_builder_creator_directory(domain)?;
    let lock_name = builder_creator_lock_name(builder);
    let lock = directory
        .open_or_create_lock_file(std::ffi::OsStr::new(&lock_name))
        .with_context(|| format!("open BuildKit creator lease lock {lock_name}"))?;
    let started = Instant::now();
    loop {
        match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(rustix::io::Errno::WOULDBLOCK) if started.elapsed() < CLAIM_LOCK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                anyhow::bail!("timed out acquiring BuildKit creator lease for {builder}");
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("lock BuildKit creator lease for {builder}")));
            }
        }
    }
    if let Some(transaction) = pending_buildkit_create_transaction(domain, builder)?
        && transaction.config_fingerprint != config_fingerprint
    {
        anyhow::bail!(
            "pending BuildKit create for {builder} has a different config fingerprint; operator repair required"
        );
    }
    let lease = PersistentBuildKitCreatorLease {
        domain: domain.clone(),
        builder: builder.to_owned(),
        config_fingerprint: config_fingerprint.to_owned(),
        generation,
        _lock: lock,
    };
    write_builder_creator_record(
        &lease.domain,
        &lease.builder,
        lease.generation,
        &lease.config_fingerprint,
        None,
        None,
    )?;
    Ok(lease)
}

pub(crate) fn pending_buildkit_create_access(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
    generation: u64,
) -> Result<Option<PendingBuildKitCreateAccess>> {
    let Some(record) = pending_buildkit_create_transaction(domain, builder)? else {
        if legacy_pending_buildkit_create_is_quarantined(domain, &daemon_state_volume(builder))? {
            anyhow::bail!("legacy BuildKit create remains quarantined for operator repair");
        }
        return Ok(None);
    };
    if record.config_fingerprint != config_fingerprint {
        anyhow::bail!("pending BuildKit create does not match this setup generation");
    }
    if !creator_lock_is_held(&open_builder_creator_directory(domain)?, builder)? {
        anyhow::bail!("pending BuildKit create has no live matching creator lease");
    }
    let live = read_live_builder_creator(domain, builder)?
        .context("pending BuildKit create lacks a live creator record")?;
    if live.config_fingerprint != config_fingerprint || live.generation != generation {
        anyhow::bail!("pending BuildKit create creator config does not match");
    }
    Ok(Some(PendingBuildKitCreateAccess {
        builder: builder.to_owned(),
        generation: record.generation,
        transaction_id: record.transaction_id,
    }))
}

impl PersistentBuildKitCreatorLease {
    pub(crate) fn matches(
        &self,
        domain: &PersistentBuildKitDomain,
        builder: &str,
        config: &str,
        generation: u64,
    ) -> bool {
        self.domain.token == domain.token
            && self.domain.root == domain.root
            && self.builder == builder
            && self.config_fingerprint == config
            && self.generation == generation
    }
}

fn write_builder_creator_record(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    container_id: Option<&str>,
    archived_config_fingerprint: Option<&str>,
) -> Result<()> {
    let record = BuilderCreatorLeaseRecord {
        version: BUILDER_CREATOR_LEASE_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        generation,
        config_fingerprint: config_fingerprint.to_owned(),
        container_id: container_id.map(str::to_owned),
        archived_config_fingerprint: archived_config_fingerprint.map(str::to_owned),
    };
    let bytes = serde_json::to_vec(&record).context("encode BuildKit creator lease")?;
    write_atomic_document(&builder_creator_file(domain, builder), &bytes)
}

fn update_live_builder_creator(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    container_id: &str,
    archived_config_fingerprint: Option<&str>,
) -> Result<()> {
    let directory = open_builder_creator_directory(domain)?;
    let _state_lock = lock_creator_state_file(&directory, builder)?;
    if !creator_lock_is_held(&directory, builder)? {
        anyhow::bail!("no live BuildKit creator lease for {builder}");
    }
    let path = builder_creator_file(domain, builder);
    let bytes = read_control_file_no_follow_with_limit(&path, MAX_BUILDER_CREATOR_LEASE_BYTES)?
        .context("live BuildKit creator lease record is missing")?;
    let mut record: BuilderCreatorLeaseRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse BuildKit creator lease {}", path.display()))?;
    validate_creator_record(&record, domain, builder)?;
    if record.generation != generation || record.config_fingerprint != config_fingerprint {
        anyhow::bail!("BuildKit creator config changed while creating {builder}");
    }
    if record
        .container_id
        .as_deref()
        .is_some_and(|known| known != container_id)
    {
        anyhow::bail!("BuildKit creator lease for {builder} is bound to another container ID");
    }
    record.container_id = Some(container_id.to_owned());
    if let Some(archive_fingerprint) = archived_config_fingerprint {
        if archive_fingerprint != config_fingerprint {
            anyhow::bail!("BuildKit creator archive does not match requested config");
        }
        record.archived_config_fingerprint = Some(archive_fingerprint.to_owned());
    }
    if !creator_lock_is_held(&directory, builder)? {
        anyhow::bail!("BuildKit creator lease for {builder} ended during attestation");
    }
    let bytes = serde_json::to_vec(&record).context("encode updated BuildKit creator lease")?;
    write_atomic_document(&path, &bytes)
}

pub(crate) fn bind_persistent_builder_creator_container(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    container_id: &str,
) -> Result<()> {
    update_live_builder_creator(
        domain,
        builder,
        generation,
        config_fingerprint,
        container_id,
        None,
    )
}

pub(crate) fn record_persistent_builder_creator_archive(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    generation: u64,
    config_fingerprint: &str,
    container_id: &str,
    archive_fingerprint: &str,
) -> Result<()> {
    update_live_builder_creator(
        domain,
        builder,
        generation,
        config_fingerprint,
        container_id,
        Some(archive_fingerprint),
    )
}

fn remove_builder_auxiliary_metadata(domain_root: &Path, builder: &str) -> Result<()> {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    let root = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(domain_root)
        .with_context(|| {
            format!(
                "open BuildKit domain {} for metadata cleanup",
                domain_root.display()
            )
        })?;
    for (directory_name, file_name) in [
        (BUILDER_READINESS_DIR, format!("{digest}.json")),
        (BUILDER_LIFECYCLE_LOCKS_DIR, format!("{digest}.lock")),
    ] {
        // Open beneath the already secured domain descriptor. Creating an
        // absent empty directory is harmless and keeps deletion path handling
        // uniform; no path component is re-resolved through the filesystem.
        let directory = root.open_relative_directory(Path::new(directory_name))?;
        match directory.remove_tree_entry(std::ffi::OsStr::new(&file_name)) {
            Ok(()) => directory.sync_directory().with_context(|| {
                format!("sync removed BuildKit metadata directory {directory_name}")
            })?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("remove BuildKit metadata {directory_name}/{file_name}")
                });
            }
        }
    }
    Ok(())
}

fn read_builder_readiness_state(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<Option<BuilderReadinessRecord>> {
    let path = builder_readiness_file(domain, builder);
    let Some(bytes) = read_control_file_no_follow_with_limit(&path, 4096)? else {
        return Ok(None);
    };
    let proof: BuilderReadinessRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse BuildKit readiness proof {}", path.display()))?;
    if proof.version != BUILDER_READINESS_VERSION
        || proof.builder != builder
        || proof.domain_token != domain.token
        || proof.state_volume != daemon_state_volume(builder)
        || persistent_builder_domain_token(builder) != Some(domain.token.as_str())
    {
        anyhow::bail!(
            "BuildKit readiness proof {} has mismatched identity",
            path.display()
        );
    }
    Ok(Some(proof))
}

/// Promote the exact historical v1 readiness document after a one-time
/// locked re-attestation. Package installation drains every Velnor fleet
/// unit before replacing the binary, so no v1 writer can overlap this
/// transition. The atomic rename leaves the original v1 bytes intact until
/// v2 is fully written and synced. Downgrade is deliberately fail-closed:
/// old strict readers reject v2's epoch/phase fields, so rollback requires
/// roll-forward or an operator-controlled builder rebootstrap; stale v1
/// readiness is never restored as authority.
#[cfg(unix)]
pub(crate) fn promote_builder_readiness_v1_with<G>(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_config_fingerprint: &str,
    acquire_volume_lock: impl Fn(&str) -> Result<G>,
    verify_engine: impl Fn() -> Result<()>,
    attest_container: impl Fn(&str, &str, &str) -> Result<()>,
    probe_workers: impl Fn(&str) -> Result<()>,
) -> Result<bool> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse readiness migration for a builder from another domain");
    }
    if expected_config_fingerprint.trim().is_empty() {
        anyhow::bail!("BuildKit readiness migration requires an exact config fingerprint");
    }
    let volume = daemon_state_volume(builder);
    let path = builder_readiness_file(domain, builder);
    let (legacy, original_bytes) = {
        let _volume_lock = acquire_volume_lock(&volume)?;
        verify_engine()?;
        let Some(bytes) = read_control_file_no_follow_with_limit(&path, 4096)? else {
            return Ok(false);
        };
        let envelope: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse BuildKit readiness proof {}", path.display()))?;
        match envelope.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == u64::from(BUILDER_READINESS_VERSION) => {
                let proof = read_builder_readiness_state(domain, builder)?
                    .context("v2 BuildKit readiness proof disappeared during migration check")?;
                if proof.config_fingerprint != expected_config_fingerprint {
                    anyhow::bail!(
                        "persistent BuildKit config mode changed for existing builder {builder}"
                    );
                }
                return Ok(false);
            }
            Some(version) if version == u64::from(BUILDER_READINESS_LEGACY_VERSION) => {}
            _ => anyhow::bail!(
                "unsupported BuildKit readiness schema in {}",
                path.display()
            ),
        }
        let legacy: BuilderReadinessRecordV1 = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse legacy BuildKit readiness proof {}", path.display()))?;
        if legacy.version != BUILDER_READINESS_LEGACY_VERSION
            || legacy.builder != builder
            || legacy.domain_token != domain.token
            || legacy.state_volume != volume
            || legacy.container_id.trim().is_empty()
            || legacy.container_id.chars().any(char::is_control)
            || legacy.config_fingerprint != expected_config_fingerprint
        {
            anyhow::bail!(
                "legacy BuildKit readiness proof {} has mismatched identity or config",
                path.display()
            );
        }
        if pending_buildkit_create_transaction(domain, builder)?.is_some()
            || legacy_pending_buildkit_create_is_quarantined(domain, &volume)?
        {
            anyhow::bail!("BuildKit create state must settle before readiness migration");
        }
        attest_container(builder, &volume, &legacy.container_id)
            .context("re-attest legacy BuildKit container before readiness migration")?;
        (legacy, bytes)
    };

    // Worker polling happens outside the volume flock. We reacquire and
    // re-attest the same immutable object before publishing the new epoch.
    probe_workers(&legacy.container_id)
        .context("re-probe BuildKit workers before v1 readiness promotion")?;

    let _volume_lock = acquire_volume_lock(&volume)?;
    verify_engine()?;
    let current_bytes = read_control_file_no_follow_with_limit(&path, 4096)?
        .context("legacy BuildKit readiness proof disappeared before promotion")?;
    if current_bytes != original_bytes {
        anyhow::bail!("BuildKit readiness proof changed during v1 promotion");
    }
    if pending_buildkit_create_transaction(domain, builder)?.is_some()
        || legacy_pending_buildkit_create_is_quarantined(domain, &volume)?
    {
        anyhow::bail!("BuildKit create state changed during readiness migration");
    }
    attest_container(builder, &volume, &legacy.container_id)
        .context("re-attest legacy BuildKit container before v2 publication")?;
    let proof = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        state_volume: volume,
        container_id: legacy.container_id,
        config_fingerprint: legacy.config_fingerprint,
        epoch: 1,
        phase: BuilderReadinessPhase::Ready,
    };
    let bytes = serde_json::to_vec(&proof).context("encode promoted BuildKit readiness proof")?;
    write_atomic_document(&path, &bytes).context("atomically promote BuildKit readiness schema")?;
    Ok(true)
}

fn read_builder_readiness(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<Option<BuilderReadinessRecord>> {
    Ok(read_builder_readiness_state(domain, builder)?
        .filter(|proof| proof.phase == BuilderReadinessPhase::Ready))
}

pub(crate) fn builder_readiness_matches(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
) -> Result<bool> {
    let Some(proof) = read_builder_readiness(domain, builder)? else {
        return Ok(false);
    };
    Ok(proof.container_id == container_id && proof.config_fingerprint == config_fingerprint)
}

/// Current durable start epoch. Missing state is epoch zero for a builder
/// that has not dispatched its first persistent daemon start.
pub(crate) fn builder_readiness_epoch(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<u64> {
    Ok(read_builder_readiness_state(domain, builder)?.map_or(0, |state| state.epoch))
}

/// Seed a newly admitted lease with the persisted epoch while the caller
/// holds the shared Engine/state-volume lock. A start already in flight or a
/// config-mode mismatch must settle before setup exposes guest capabilities.
pub(crate) fn builder_readiness_epoch_for_setup(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
) -> Result<u64> {
    let Some(state) = read_builder_readiness_state(domain, builder)? else {
        return Ok(0);
    };
    if state.phase != BuilderReadinessPhase::Ready {
        anyhow::bail!("BuildKit start is still in progress for {builder}");
    }
    if state.config_fingerprint != config_fingerprint {
        anyhow::bail!("persistent BuildKit config mode changed for existing builder {builder}");
    }
    Ok(state.epoch)
}

pub(crate) fn builder_readiness_matches_epoch(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<bool> {
    let Some(state) = read_builder_readiness_state(domain, builder)? else {
        return Ok(false);
    };
    Ok(state.phase == BuilderReadinessPhase::Ready
        && state.epoch == expected_epoch
        && state.container_id == container_id
        && state.config_fingerprint == config_fingerprint)
}

pub(crate) fn builder_starting_readiness_matches_epoch(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<bool> {
    let Some(state) = read_builder_readiness_state(domain, builder)? else {
        return Ok(false);
    };
    Ok(state.phase == BuilderReadinessPhase::Starting
        && state.epoch == expected_epoch
        && state.container_id == container_id
        && state.config_fingerprint == config_fingerprint)
}

/// Publish readiness only for the current start epoch. Callers must hold the
/// shared Engine/state-volume lock and have just re-attested the immutable
/// container ID and worker state.
pub(crate) fn publish_builder_readiness_for_epoch(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<()> {
    let current = read_builder_readiness_state(domain, builder)?
        .context("BuildKit start epoch disappeared before readiness publication")?;
    if current.phase != BuilderReadinessPhase::Starting
        || current.epoch != expected_epoch
        || current.container_id != container_id
        || current.config_fingerprint != config_fingerprint
    {
        anyhow::bail!("stale BuildKit worker probe cannot publish readiness");
    }
    let proof = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        state_volume: daemon_state_volume(builder),
        container_id: container_id.to_owned(),
        config_fingerprint: config_fingerprint.to_owned(),
        epoch: expected_epoch,
        phase: BuilderReadinessPhase::Ready,
    };
    let bytes = serde_json::to_vec(&proof).context("encode BuildKit readiness proof")?;
    write_atomic_document(&builder_readiness_file(domain, builder), &bytes)
}

/// Remove old restart authority before Docker receives a start request. The
/// caller holds the shared Engine/volume lock. A missing record is valid for
/// a newly created daemon; a mismatched or corrupt record fails closed.
pub(crate) fn invalidate_builder_readiness_before_start(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<u64> {
    // Caller holds the shared Engine/state-volume flock. Replacing Ready with
    // Starting both revokes old authority and advances the publication epoch
    // in one durable rename. A concurrent start advances it again; an older
    // probe can then never restore readiness.
    let previous = read_builder_readiness_state(domain, builder)?;
    if let Some(previous) = previous.as_ref()
        && (previous.container_id != container_id
            || previous.config_fingerprint != config_fingerprint)
    {
        anyhow::bail!("refuse BuildKit start with mismatched durable readiness state");
    }
    let current_epoch = previous.as_ref().map_or(0, |proof| proof.epoch);
    if current_epoch != expected_epoch {
        anyhow::bail!("stale BuildKit start admission: expected epoch {expected_epoch}, current epoch {current_epoch}");
    }
    let epoch = current_epoch
        .checked_add(1)
        .context("BuildKit readiness epoch exhausted")?;
    let record = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        state_volume: daemon_state_volume(builder),
        container_id: container_id.to_owned(),
        config_fingerprint: config_fingerprint.to_owned(),
        epoch,
        phase: BuilderReadinessPhase::Starting,
    };
    let bytes = serde_json::to_vec(&record).context("encode BuildKit start epoch")?;
    write_atomic_document(&builder_readiness_file(domain, builder), &bytes)
        .context("persist BuildKit start epoch before Docker start")?;
    Ok(epoch)
}

/// Revoke exec readiness and advance the epoch before dispatching a host
/// stop. The caller holds the exact Engine/state-volume flock. A failed or
/// ambiguous stop leaves `Stopping`, which can only regain authority after a
/// later exact-ID worker probe publishes a newer `Ready` epoch.
pub(crate) fn invalidate_builder_readiness_before_stop(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
) -> Result<Option<(String, u64)>> {
    let Some(previous) = read_builder_readiness_state(domain, builder)? else {
        return Ok(None);
    };
    if previous.container_id != container_id {
        anyhow::bail!("refuse stop: durable BuildKit readiness names another container ID");
    }
    let epoch = previous
        .epoch
        .checked_add(1)
        .context("BuildKit readiness epoch exhausted before stop")?;
    let record = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        state_volume: daemon_state_volume(builder),
        container_id: container_id.to_owned(),
        config_fingerprint: previous.config_fingerprint.clone(),
        epoch,
        phase: BuilderReadinessPhase::Stopping,
    };
    write_atomic_document(
        &builder_readiness_file(domain, builder),
        &serde_json::to_vec(&record).context("encode BuildKit stopping epoch")?,
    )
    .context("persist BuildKit readiness revocation before Docker stop")?;
    Ok(Some((record.config_fingerprint, epoch)))
}

pub(crate) fn publish_builder_stopped_after_stop(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<()> {
    let current = read_builder_readiness_state(domain, builder)?
        .context("BuildKit stop epoch disappeared before stopped publication")?;
    if current.phase != BuilderReadinessPhase::Stopping
        || current.epoch != expected_epoch
        || current.container_id != container_id
        || current.config_fingerprint != config_fingerprint
    {
        anyhow::bail!("stale BuildKit stop cannot publish a stopped state");
    }
    let stopped = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        phase: BuilderReadinessPhase::Stopped,
        ..current
    };
    write_atomic_document(
        &builder_readiness_file(domain, builder),
        &serde_json::to_vec(&stopped).context("encode BuildKit stopped state")?,
    )
}

/// Stop one already-attested immutable ID while its caller owns the shared
/// volume lock. Readiness is durably revoked before Docker can stop it.
fn stop_builder_under_volume_lock(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
) -> Result<bool> {
    stop_builder_under_volume_lock_with(
        domain,
        builder,
        container_id,
        stop_attested_builder_confirmed,
        |id| crate::docker::Docker::host().inspect_exit(id),
    )
}

fn stop_builder_under_volume_lock_with(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    mut stop: impl FnMut(&str) -> Result<bool>,
    mut inspect: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
) -> Result<bool> {
    let stopping = invalidate_builder_readiness_before_stop(domain, builder, container_id)?;
    let stopped = stop(container_id)?;
    let Some((config_fingerprint, epoch)) = stopping else {
        return Ok(stopped);
    };
    if !stopped {
        // The stop helper returns false only for a definitive immutable-ID
        // 404, so this exact daemon is absent and cannot serve exec traffic.
        publish_builder_stopped_after_stop(
            domain,
            builder,
            container_id,
            &config_fingerprint,
            epoch,
        )?;
        return Ok(false);
    }
    let state = match inspect(container_id) {
        Ok(state) => state.status,
        Err(error) if crate::docker::client::is_not_found(&error) => None,
        Err(error) => {
            return Err(error).context("verify BuildKit daemon state after stop");
        }
    };
    if !matches!(
        state,
        None | Some(
            crate::docker::client::ContainerState::Created
                | crate::docker::client::ContainerState::Exited
                | crate::docker::client::ContainerState::Dead
        )
    ) {
        anyhow::bail!("BuildKit stop did not leave daemon stopped: {state:?}");
    }
    publish_builder_stopped_after_stop(domain, builder, container_id, &config_fingerprint, epoch)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn write_test_builder_readiness(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container_id: &str,
    config_fingerprint: &str,
) -> Result<()> {
    let proof = BuilderReadinessRecord {
        version: BUILDER_READINESS_VERSION,
        builder: builder.to_owned(),
        domain_token: domain.token.clone(),
        state_volume: daemon_state_volume(builder),
        container_id: container_id.to_owned(),
        config_fingerprint: config_fingerprint.to_owned(),
        epoch: read_builder_readiness_state(domain, builder)?.map_or(1, |current| current.epoch),
        phase: BuilderReadinessPhase::Ready,
    };
    let bytes = serde_json::to_vec(&proof).context("encode test BuildKit readiness proof")?;
    write_atomic_document(&builder_readiness_file(domain, builder), &bytes)
}

pub(crate) fn builder_readiness_for_config(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
) -> Result<Option<String>> {
    let Some(proof) = read_builder_readiness(domain, builder)? else {
        return Ok(None);
    };
    if proof.config_fingerprint != config_fingerprint {
        anyhow::bail!("persistent BuildKit config mode changed for existing builder {builder}");
    }
    Ok(Some(proof.container_id))
}

pub(crate) fn config_mode_matches_command(fingerprint: &str, has_config_flag: bool) -> bool {
    (fingerprint == "no-config-v1") != has_config_flag
}

/// Persist restart authority only after Buildx's successful `--config`
/// archive, Docker's successful ContainerStart response, an Engine-volume
/// and immutable container re-attestation, and a passing worker readiness
/// probe. The lock is dropped during worker polling and reacquired before the
/// proof is published.
pub(crate) fn persist_builder_readiness_after_start(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
) -> Result<()> {
    let pending = probe_builder_readiness_for_epoch(
        domain,
        builder,
        expected_container_id,
        config_fingerprint,
        expected_epoch,
        || wait_for_attested_buildkit_ready(expected_container_id),
    )?;
    let host_socket = crate::docker::engine::resolve_docker_endpoint()
        .context("resolve Docker endpoint for BuildKit readiness attestation")?
        .socket;
    with_attested_domain_builder_with(
        domain,
        builder,
        |_, _, volume| {
            if let Some(transaction) = pending.as_ref() {
                let access = PendingBuildKitCreateAccess {
                    builder: builder.to_owned(),
                    generation: transaction.generation,
                    transaction_id: transaction.transaction_id.clone(),
                };
                crate::docker_lease::lock_host_volume_name_for_pending_create(
                    domain, volume, &access,
                )
            } else {
                crate::docker_lease::lock_host_volume_name_for_domain(domain, volume)
            }
        },
        attest_buildkit_removal_volume,
        attest_buildkit_removal_container,
        |_, daemon, volume, volume_present, container_id| {
            if !volume_present {
                anyhow::bail!("BuildKit state volume for {daemon} disappeared after start");
            }
            let container_id = container_id
                .context("BuildKit daemon disappeared before readiness proof publication")?;
            if container_id != expected_container_id {
                anyhow::bail!("BuildKit daemon {daemon} changed immutable ID after start");
            }
            let state = crate::docker::Docker::host()
                .inspect_exit(container_id)
                .with_context(|| {
                    format!("inspect BuildKit daemon {daemon} after readiness probe")
                })?;
            if state.status != Some(crate::docker::client::ContainerState::Running) {
                anyhow::bail!(
                    "BuildKit daemon {daemon} stopped before readiness proof publication"
                );
            }
            if let Some(transaction) = pending.as_ref() {
                let (status, body) = crate::docker_lease::inspect_container_on_host(
                    &host_socket,
                    expected_container_id,
                )
                .context("re-inspect BuildKit daemon shape before readiness publication")?;
                if !(200..300).contains(&status) {
                    anyhow::bail!(
                        "BuildKit daemon full-shape re-attestation returned HTTP {status}"
                    );
                }
                let (id, shape, full_state) =
                    crate::docker_lease::attest_pending_buildkit_create_inspect(
                        &body,
                        transaction,
                    )?;
                let expected_shape = transaction
                    .attested_shape_sha256
                    .as_deref()
                    .context("BuildKit create transaction lacks a full-shape fingerprint")?;
                if id != expected_container_id || shape != expected_shape || full_state != "running"
                {
                    anyhow::bail!(
                        "BuildKit daemon shape or state changed before readiness publication"
                    );
                }
            }
            publish_builder_readiness_for_epoch(
                domain,
                builder,
                container_id,
                config_fingerprint,
                expected_epoch,
            )
        },
    )?;
    if let Some(transaction) = pending {
        if transaction.container_id.as_deref() != Some(expected_container_id)
            || transaction.config_fingerprint != config_fingerprint
            || transaction.archived_config_fingerprint.as_deref() != Some(config_fingerprint)
        {
            anyhow::bail!("BuildKit readiness cannot settle a different create transaction");
        }
        finish_pending_buildkit_create_transaction(
            domain,
            builder,
            &transaction.transaction_id,
            expected_container_id,
            config_fingerprint,
        )?;
    }
    Ok(())
}

pub(crate) fn probe_builder_readiness_for_epoch(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
    probe: impl FnOnce() -> Result<()>,
) -> Result<Option<PendingBuildKitCreateTransaction>> {
    let start_state = read_builder_readiness_state(domain, builder)?
        .context("BuildKit start epoch disappeared before readiness probe")?;
    if start_state.phase != BuilderReadinessPhase::Starting
        || start_state.epoch != expected_epoch
        || start_state.container_id != expected_container_id
        || start_state.config_fingerprint != config_fingerprint
    {
        anyhow::bail!("BuildKit readiness probe does not match the current start epoch");
    }
    let pending = pending_buildkit_create_transaction(domain, builder)?;
    if let Some(transaction) = pending.as_ref()
        && (transaction.container_id.as_deref() != Some(expected_container_id)
            || transaction.config_fingerprint != config_fingerprint
            || transaction.archived_config_fingerprint.as_deref() != Some(config_fingerprint)
            || !matches!(
                transaction.phase,
                PendingBuildKitCreatePhase::ArchiveAccepted | PendingBuildKitCreatePhase::Started
            ))
    {
        anyhow::bail!("BuildKit start readiness does not match its durable create transaction");
    }
    probe().with_context(|| format!("wait for BuildKit daemon {builder} after start"))?;
    Ok(pending)
}

fn claims_file(domain_root: &Path, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    domain_root.join(CLAIMS_DIR).join(format!("{digest}.json"))
}

fn owner_registry_root(domain_root: &Path) -> PathBuf {
    domain_root.join(OWNER_REGISTRY_DIR)
}

fn owner_registry_file(registry_root: &Path, builder: &str) -> PathBuf {
    let digest = blake3::hash(builder.as_bytes()).to_hex();
    registry_root.join(format!("{digest}.json"))
}

/// List the domain's durable builder owners. Buildx registration state lives in
/// each disposable job container, so a host `docker buildx ls` is not an owner
/// inventory and must never authorize or erase shared ownership records.
fn registered_domain_builders(registry_root: &Path, domain_token: &str) -> Result<Vec<String>> {
    if domain_token.len() != BUILDKIT_DOMAIN_TOKEN_HEX_LEN
        || !domain_token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("invalid BuildKit domain token");
    }
    match std::fs::symlink_metadata(registry_root) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(_) => anyhow::bail!(
            "BuildKit owner registry is not a real directory: {}",
            registry_root.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect BuildKit owner registry {}",
                    registry_root.display()
                )
            });
        }
    }
    let root = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(registry_root)
        .with_context(|| format!("secure BuildKit owner registry {}", registry_root.display()))?;
    let entries = match std::fs::read_dir(registry_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("list BuildKit owner registry {}", registry_root.display())
            });
        }
    };
    let mut builders = Vec::new();
    for entry in entries {
        let entry = entry.context("read BuildKit owner registry entry")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let file_name = path
            .file_name()
            .context("BuildKit owner registry entry has no filename")?;
        let mut file = root
            .open_relative_file(Path::new(file_name))
            .with_context(|| format!("open BuildKit owner record {} safely", path.display()))?;
        let bytes = read_bounded_control_file(&mut file, &path, MAX_BUILDKIT_CONTROL_FILE_BYTES)?;
        let identity: BuilderOwnerIdentity = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse BuildKit owner identity {}", path.display()))?;
        if identity.version != OWNER_REGISTRY_VERSION {
            if is_current_domain_builder_name(&identity.builder, domain_token) {
                anyhow::bail!(
                    "current BuildKit owner record {} uses unsupported schema version {}; expected {}",
                    path.display(),
                    identity.version,
                    OWNER_REGISTRY_VERSION
                );
            }
            // Retired/unscoped records and records copied from another domain
            // carry no deletion authority here. Keep their bytes untouched;
            // old on-disk versions are not migrated or adopted.
            continue;
        }
        let record: BuilderOwnerRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse current BuildKit owner record {}", path.display()))?;
        if !is_current_domain_builder_name(&record.builder, domain_token)
            || owner_registry_file(registry_root, &record.builder) != path
        {
            anyhow::bail!(
                "current BuildKit owner record {} has mismatched domain identity",
                path.display()
            );
        }
        builders.push(record.builder);
    }
    builders.sort();
    builders.dedup();
    Ok(builders)
}

fn read_owner_record(registry_root: &Path, builder: &str) -> Result<Option<BuilderOwnerRecord>> {
    if !is_current_domained_persistent_builder(builder) {
        return Ok(None);
    }
    ensure_owner_registry_directory(registry_root)?;
    let path = owner_registry_file(registry_root, builder);
    let Some(bytes) = read_control_file_no_follow(&path)? else {
        return Ok(None);
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

/// Read runner-owned BuildKit metadata through a descriptor-bound parent and
/// a no-follow regular-file open. A matching JSON document behind a symlink
/// has no ownership authority.
fn read_control_file_no_follow(path: &Path) -> Result<Option<Vec<u8>>> {
    read_control_file_no_follow_with_limit(path, MAX_BUILDKIT_CONTROL_FILE_BYTES)
}

fn read_control_file_no_follow_with_limit(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>> {
    let parent = path
        .parent()
        .with_context(|| format!("BuildKit metadata path has no parent: {}", path.display()))?;
    let name = path
        .file_name()
        .with_context(|| format!("BuildKit metadata path has no filename: {}", path.display()))?;
    let directory = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(parent)
        .with_context(|| {
            format!(
                "open BuildKit metadata directory {} safely",
                parent.display()
            )
        })?;
    let Some(mut file) = directory
        .open_relative_file_if_exists(Path::new(name))
        .with_context(|| format!("open BuildKit metadata file {} safely", path.display()))?
    else {
        return Ok(None);
    };
    read_bounded_control_file(&mut file, path, maximum).map(Some)
}

fn read_bounded_control_file(
    file: &mut std::fs::File,
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>> {
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect BuildKit metadata file {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > maximum {
        anyhow::bail!(
            "BuildKit metadata file {} is not regular or exceeds {} bytes",
            path.display(),
            maximum
        );
    }
    let limit = maximum.saturating_add(1);
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut limited = std::io::Read::take(file, limit);
    std::io::Read::read_to_end(&mut limited, &mut bytes)
        .with_context(|| format!("read BuildKit metadata file {}", path.display()))?;
    if bytes.len() as u64 > maximum {
        anyhow::bail!(
            "BuildKit metadata file {} exceeds {} bytes",
            path.display(),
            maximum
        );
    }
    Ok(bytes)
}

fn ensure_owner_registry_directory(registry_root: &Path) -> Result<()> {
    crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(registry_root)
        .map(|_| ())
        .with_context(|| format!("secure BuildKit owner registry {}", registry_root.display()))
}

fn ensure_owner_record(registry_root: &Path, builder: &str) -> Result<()> {
    if !is_current_domained_persistent_builder(builder) {
        return Ok(());
    }
    ensure_owner_registry_directory(registry_root)?;
    if let Some(record) = read_owner_record(registry_root, builder)? {
        if record.phase == BuilderOwnerPhase::Deleting {
            anyhow::bail!("BuildKit builder {builder} is durably marked for deletion");
        }
        return Ok(());
    }
    let record = BuilderOwnerRecord {
        version: OWNER_REGISTRY_VERSION,
        builder: builder.to_string(),
        phase: BuilderOwnerPhase::Active,
    };
    let path = owner_registry_file(registry_root, builder);
    let bytes = serde_json::to_vec_pretty(&record).context("encode BuildKit owner record")?;
    write_atomic_document(&path, &bytes)
}

fn ensure_active_owner_record(registry_root: &Path, builder: &str) -> Result<()> {
    ensure_owner_record(registry_root, builder)
}

fn mark_owner_record_deleting(registry_root: &Path, builder: &str) -> Result<()> {
    if !is_current_domained_persistent_builder(builder) {
        anyhow::bail!("refuse deletion tombstone for an unscoped BuildKit builder");
    }
    let mut record = read_owner_record(registry_root, builder)?
        .context("cannot tombstone a BuildKit builder without an active owner record")?;
    if record.phase == BuilderOwnerPhase::Deleting {
        return Ok(());
    }
    record.phase = BuilderOwnerPhase::Deleting;
    let path = owner_registry_file(registry_root, builder);
    let bytes = serde_json::to_vec_pretty(&record).context("encode BuildKit deletion tombstone")?;
    write_atomic_document(&path, &bytes)
}

fn remove_owner_record(registry_root: &Path, builder: &str) -> Result<()> {
    if !is_current_domained_persistent_builder(builder) {
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
fn read_claims(path: &Path, builder: &str) -> Result<BuilderClaims> {
    let Some(bytes) = read_control_file_no_follow(path)? else {
        return Ok(BuilderClaims {
            builder: builder.to_owned(),
            ..BuilderClaims::default()
        });
    };
    let claims = parse_claims(path, &bytes)?;
    ensure_claims_builder(path, &claims, builder)?;
    Ok(claims)
}

fn runtime_claims_missing(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("runtime claims file {} is a symlink", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "runtime claims path {} is not a regular file",
                path.display()
            );
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
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

fn ensure_claims_builder(path: &Path, claims: &BuilderClaims, expected: &str) -> Result<()> {
    if claims.builder != expected {
        anyhow::bail!(
            "claim file {} names builder {:?}, expected {expected}",
            path.display(),
            claims.builder
        );
    }
    Ok(())
}

/// Read a Velnor ownership record without treating a missing or mismatched
/// file as an empty claim. A reserved-looking name alone is not proof that an
/// external Buildx builder belongs to this daemon.
fn read_registered_claims(path: &Path, builder: &str) -> Result<Option<BuilderClaims>> {
    let Some(bytes) = read_control_file_no_follow(path)? else {
        return Ok(None);
    };
    let claims = parse_claims(path, &bytes)?;
    ensure_claims_builder(path, &claims, builder)?;
    if !is_current_domained_persistent_builder(&claims.builder) {
        anyhow::bail!(
            "claim file {} names a retired or unscoped builder",
            path.display()
        );
    }
    Ok(Some(claims))
}

/// A failed ownership read during a maintenance pass, carrying the exact file
/// the operator must repair. Runtime-claim and stat failures report the claim
/// file; durable owner-record failures report the owner record. Pointing the
/// operator at the claim path for an owner-record failure would delete a
/// healthy file while the corrupt record pins its builder every pass.
#[derive(Debug)]
struct OwnershipReadError {
    source: anyhow::Error,
    path: PathBuf,
}

impl OwnershipReadError {
    fn claims(path: &Path, source: anyhow::Error) -> Self {
        Self {
            source,
            path: path.to_path_buf(),
        }
    }

    fn owner_record(registry_root: &Path, builder: &str, source: anyhow::Error) -> Self {
        Self {
            source,
            path: owner_registry_file(registry_root, builder),
        }
    }
}

/// Read ownership for a maintenance pass. Only current domain names reach
/// this function. Missing claims never become an empty holder set: Docker's
/// container snapshot does not cover runner admission markers, and a current
/// owner record cannot identify which jobs hold its builder.
fn read_claims_for_reaping(
    path: &Path,
    builder: &str,
    registry_root: Option<&Path>,
) -> Result<Option<BuilderClaims>, OwnershipReadError> {
    if let Some(claims) = read_registered_claims(path, builder)
        .map_err(|source| OwnershipReadError::claims(path, source))?
    {
        if is_current_domained_persistent_builder(builder)
            && let Some(registry_root) = registry_root
        {
            // An existing malformed or mismatched durable record is a hard
            // stop. A valid runtime claim may recreate a missing owner record.
            let _ = read_owner_record(registry_root, builder).map_err(|source| {
                OwnershipReadError::owner_record(registry_root, builder, source)
            })?;
        }
        return Ok(Some(claims));
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OwnershipReadError::claims(
                path,
                anyhow::Error::new(error).context(format!("stat {}", path.display())),
            ));
        }
    }
    if is_current_domained_persistent_builder(builder)
        && let Some(registry_root) = registry_root
        && read_owner_record(registry_root, builder)
            .map_err(|source| OwnershipReadError::owner_record(registry_root, builder, source))?
            .is_some()
    {
        return Err(OwnershipReadError::owner_record(
            registry_root,
            builder,
            anyhow::anyhow!(
                "runtime claim file {} is missing; durable ownership cannot prove this builder has no holders; after all jobs using this Docker endpoint are quiescent, remove the owner record to allow a fresh claim",
                path.display(),
            ),
        ));
    }
    Ok(None)
}

fn repair_pressure_claims(
    run_root: &Path,
    registry_root: &Path,
    builder: &str,
    present: &BTreeSet<String>,
) -> Result<bool> {
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let mut claims = match read_claims_for_reaping(&path, builder, Some(registry_root)) {
        Ok(Some(claims)) => claims,
        Ok(None) => anyhow::bail!("BuildKit claims for {builder} are missing or mismatched"),
        Err(error) => {
            return Err(error.source).context(format!(
                "read BuildKit ownership at {}",
                error.path.display()
            ));
        }
    };
    ensure_active_owner_record(registry_root, builder)?;
    repair_absent_unlocked(&mut claims, present);
    write_claims(&path, &claims)?;
    Ok(claims.holders.is_empty())
}

/// Atomically replace one runtime claim file.
fn write_claims(path: &Path, claims: &BuilderClaims) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(claims).context("encode builder claims")?;
    write_atomic_document(path, &bytes)
}

fn publish_claims_before_owner(
    path: &Path,
    claims: &BuilderClaims,
    publish_owner: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write_claims(path, claims)?;
    publish_owner()
}

/// Atomically replace one JSON document: write and fsync a sibling temp,
/// rename it, then fsync the parent directory. A crash leaves either the
/// previous record or the new record, never a partial target.
fn write_atomic_document(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("BuildKit document has no parent: {}", path.display()))?;
    let destination = path
        .file_name()
        .with_context(|| format!("BuildKit document has no filename: {}", path.display()))?;
    let directory =
        crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(parent)
            .with_context(|| {
                format!(
                    "open BuildKit document directory {} safely",
                    parent.display()
                )
            })?;
    let (staged, staging_name) = directory.create_temporary_file("velnor-buildkit")?;
    let mut staged = Some(staged);
    let write_result = (|| -> Result<()> {
        use std::io::Write as _;
        staged
            .as_mut()
            .context("staged BuildKit document was already closed")?
            .write_all(bytes)
            .with_context(|| format!("write BuildKit document {}", path.display()))?;
        staged
            .as_ref()
            .context("staged BuildKit document was already closed")?
            .sync_all()
            .with_context(|| format!("fsync BuildKit document {}", path.display()))?;
        drop(staged.take());
        directory.publish_temporary_file(&staging_name, destination)?;
        directory.sync_directory()?;
        Ok(())
    })();
    if write_result.is_err() {
        drop(staged.take());
        let _ = directory.remove_tree_entry(&staging_name);
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
        "torn claim file treated as claimed; to recover: quiesce all jobs using this Docker endpoint, \
         delete the claim file, and let the next claim recreate it"
    );
}

fn log_unreadable_ownership(builder: &str, ownership_path: &Path, error: &anyhow::Error) {
    tracing::error!(
        target: "velnor.buildkit",
        builder,
        ownership_path = %ownership_path.display(),
        error = format!("{error:#}"),
        "unreadable BuildKit ownership state treated as claimed; quiesce all jobs using this Docker endpoint, repair the runtime claim or durable owner record, then retry"
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

/// Serialize one builder's setup and last-holder release across slow Docker
/// operations. Both callers take the domain coordinator before this lock.
pub(crate) fn lock_builder_lifecycle(domain_root: &Path, builder: &str) -> Result<std::fs::File> {
    if !is_current_domained_persistent_builder(builder) {
        anyhow::bail!("refuse lifecycle lock for an unscoped or retired BuildKit builder");
    }
    let lock_root = domain_root.join(BUILDER_LIFECYCLE_LOCKS_DIR);
    let directory =
        crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(&lock_root)
            .with_context(|| {
                format!(
                    "secure BuildKit lifecycle lock root {}",
                    lock_root.display()
                )
            })?;
    let file_name = format!("{}.lock", blake3::hash(builder.as_bytes()).to_hex());
    let file = directory
        .open_or_create_lock_file(std::ffi::OsStr::new(&file_name))
        .with_context(|| format!("open BuildKit lifecycle lock {file_name}"))?;
    let started = Instant::now();
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                let waited_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                tracing::debug!(
                    target: "velnor.buildkit",
                    builder,
                    lifecycle_lock_wait_ms = waited_ms,
                    "builder lifecycle lock acquired"
                );
                return Ok(file);
            }
            Err(rustix::io::Errno::WOULDBLOCK) if started.elapsed() < CLAIM_LOCK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                anyhow::bail!(
                    "timed out acquiring BuildKit lifecycle lock for {builder} after {:?}",
                    CLAIM_LOCK_TIMEOUT
                );
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("lock BuildKit lifecycle for {builder}")));
            }
        }
    }
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
#[cfg(test)]
pub(crate) fn claim_builder(
    domain_root: &Path,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    if !is_current_domained_persistent_builder(builder) {
        anyhow::bail!("refuse claims for an unscoped or retired BuildKit builder");
    }
    let registry_root = owner_registry_root(domain_root);
    claim_builder_with_registry(domain_root, Some(&registry_root), builder, slot, container)
}

pub(crate) fn claim_domain_builder(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse claims for a BuildKit builder from another domain");
    }
    let registry_root = owner_registry_root(&domain.root);
    claim_builder_with_registry(&domain.root, Some(&registry_root), builder, slot, container)
}

fn claim_builder_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    slot: &str,
    container: &str,
) -> Result<()> {
    if !is_current_domained_persistent_builder(builder) {
        anyhow::bail!("refuse claims for an unscoped or retired BuildKit builder");
    }
    let path = claims_file(run_root, builder);
    let _lock = lock_claims(builder, &path)?;
    let owner = registry_root
        .map(|registry_root| read_owner_record(registry_root, builder))
        .transpose()?
        .flatten();
    if owner
        .as_ref()
        .is_some_and(|record| record.phase == BuilderOwnerPhase::Deleting)
    {
        anyhow::bail!("BuildKit builder {builder} is durably marked for deletion");
    }
    if runtime_claims_missing(&path)?
        && is_current_domained_persistent_builder(builder)
        && let Some(registry_root) = registry_root
        && owner.is_some()
    {
        anyhow::bail!(
            "runtime claims for registered BuildKit builder {builder} are missing; after all jobs using this Docker endpoint are quiescent, remove owner record {} before setup creates a fresh claim",
            owner_registry_file(registry_root, builder).display()
        );
    }
    let mut claims = match read_claims(&path, builder) {
        Ok(claims) => claims,
        Err(error) => {
            log_torn_claims(builder, &path, &error);
            return Err(error);
        }
    };
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
    // Claims publish first. A crash before the owner record leaves no Docker
    // side effects authorized and a recoverable claims-only ledger; the
    // reverse order could leave an Active owner with missing claims, which
    // must remain permanently fail-closed.
    publish_claims_before_owner(&path, &claims, || {
        if let Some(registry_root) = registry_root {
            ensure_active_owner_record(registry_root, builder)?;
        }
        Ok(())
    })
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
/// removed the final holder. The shared lifecycle coordinator and per-builder
/// lock remain held across claim mutation, Docker stop, and recovery, so setup
/// cannot create or use this builder until the stop is complete. Releasing a
/// hold this job does not have (post after post, teardown after post) succeeds
/// without stopping. A torn claim file reads as claimed: no removal, no stop,
/// success.
#[cfg(test)]
pub(crate) fn release_and_stop_if_last(
    domain_root: &Path,
    builder: &str,
    container: &str,
    stop: impl FnOnce() -> Result<bool>,
    start: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    let registry_root = owner_registry_root(domain_root);
    release_and_stop_if_last_with_registry(
        domain_root,
        Some(&registry_root),
        builder,
        container,
        stop,
        start,
    )
}

pub(crate) fn release_domain_builder_if_last(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    container: &str,
    stop: impl FnOnce() -> Result<bool>,
    start: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse release for a BuildKit builder from another domain");
    }
    let registry_root = owner_registry_root(&domain.root);
    release_and_stop_if_last_with_registry(
        &domain.root,
        Some(&registry_root),
        builder,
        container,
        stop,
        start,
    )
}

fn release_and_stop_if_last_with_registry(
    domain_root: &Path,
    registry_root: Option<&Path>,
    builder: &str,
    container: &str,
    stop: impl FnOnce() -> Result<bool>,
    start: impl FnOnce() -> Result<bool>,
) -> Result<ReleaseOutcome> {
    if !is_current_domained_persistent_builder(builder) {
        anyhow::bail!("refuse release for an unscoped or retired BuildKit builder");
    }
    let _lifecycle = crate::capacity::FilesystemCoordinator::lock_shared(domain_root)
        .context("lock BuildKit lifecycle for release")?;
    let _builder_lifecycle = lock_builder_lifecycle(domain_root, builder)?;
    let path = claims_file(domain_root, builder);
    let removed_last = {
        let _lock = lock_claims(builder, &path)?;
        if runtime_claims_missing(&path)? && is_persistent_builder_name(builder) {
            if is_current_domained_persistent_builder(builder)
                && let Some(registry_root) = registry_root
                && read_owner_record(registry_root, builder)?.is_some()
            {
                tracing::warn!(
                    target: "velnor.buildkit",
                    builder,
                    container,
                    owner_record = %owner_registry_file(registry_root, builder).display(),
                    "skip BuildKit release because registered runtime claims are missing; after all jobs using this Docker endpoint are quiescent, remove the owner record before a fresh claim"
                );
            } else {
                tracing::warn!(
                    target: "velnor.buildkit",
                    builder,
                    container,
                    "skip BuildKit release because runtime claims are missing; no holder can be removed safely"
                );
            }
            return Ok(ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            });
        }
        let mut claims = match read_claims(&path, builder) {
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
    let stop_result = stop();
    let stopped = stop_result.as_ref().copied().unwrap_or(false);
    let stop_error = stop_result.err();
    tracing::debug!(
        target: "velnor.buildkit",
        builder,
        stop_ms = stop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        stopped,
        stop_uncertain = stop_error.is_some(),
        "release stop ran outside the claim lock"
    );
    // Recheck: a setup that claimed while the stop ran needs the daemon
    // back. An ambiguous stop result still reaches this recheck; lock or
    // ownership uncertainty triggers a best-effort start before returning.
    let recheck = (|| -> Result<(bool, bool)> {
        let _lock = lock_claims(builder, &path)?;
        Ok(
            match read_claims_for_reaping(&path, builder, registry_root) {
                Ok(Some(claims)) => (!claims.holders.is_empty(), false),
                Ok(None) => (true, true),
                Err(error) => {
                    log_unreadable_ownership(builder, &error.path, &error.source);
                    (true, true)
                }
            },
        )
    })();
    let (raced, recheck_uncertain) = match recheck {
        Ok(recheck) => recheck,
        Err(error) => {
            // Lock/read uncertainty cannot establish that no holder arrived.
            // Start the daemon unconditionally; this is idempotent for a
            // still-running daemon and repairs a stop that acted then errored.
            if let Err(start_error) = start() {
                return Err(error).context(format!(
                    "BuildKit claim recheck failed and safe restart also failed: {start_error:#}"
                ));
            }
            return Err(error).context(
                "BuildKit claim recheck failed; attempted safe restart after ambiguous stop",
            );
        }
    };
    let restarted = if raced && (stopped || stop_error.is_some() || recheck_uncertain) {
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
    if raced && !restarted {
        anyhow::bail!(
            "BuildKit holders arrived during release stop, but the attested daemon did not become ready"
        );
    }
    if let Some(error) = stop_error {
        if raced && restarted {
            tracing::warn!(
                target: "velnor.buildkit",
                builder,
                error = format!("{error:#}"),
                "release stop returned an error; daemon restarted after holder recheck"
            );
        } else {
            return Err(error)
                .context("BuildKit release stop returned an error after claim recheck");
        }
    }
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
    let mut claims = read_claims(&path, builder)?;
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
    let mut claims = read_claims(&path, builder)?;
    repair_absent_unlocked(&mut claims, present);
    write_claims(&path, &claims)?;
    let mut holders: Vec<BuilderHolder> = claims.holders.into_values().collect();
    holders.sort_by(|left, right| left.container.cmp(&right.container));
    Ok(holders)
}

/// True when the periodic horizon pass is due: no marker, an unreadable
/// marker, or one older than [`HORIZON_REAP_INTERVAL`].
fn horizon_reap_due(marker: &Path, now: SystemTime) -> bool {
    let elapsed = read_control_file_no_follow_with_limit(marker, MAX_HORIZON_REAP_MARKER_BYTES)
        .ok()
        .flatten()
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
pub(crate) fn maybe_reap_idle_builders(
    domain: &PersistentBuildKitDomain,
    now: SystemTime,
) -> Option<HorizonReport> {
    maybe_reap_idle_builders_with(&domain.root, now, |_, _| reap_idle_builders(domain, now))
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
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
        .to_string();
    if let Err(error) = write_atomic_document(&marker, stamp.as_bytes()) {
        tracing::warn!(
            target: "velnor.buildkit",
            marker = %marker.display(),
            error = %error,
            "failed to stamp BuildKit horizon marker; next setup will retry"
        );
    }
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

// ---------------------------------------------------------------------------
// Daemon operations (host engine, every call deadline-bounded)
// ---------------------------------------------------------------------------

/// Run a lifecycle command only after locking and re-attesting the domain's
/// exact state volume and daemon. Docker containers are addressed by the
/// immutable ID returned by that inspect, never by a reusable name.
fn with_attested_domain_builder<T>(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    operation: impl FnOnce(&str, &str, &str, bool, Option<&str>) -> Result<T>,
) -> Result<T> {
    with_attested_domain_builder_with(
        domain,
        builder,
        |_, _, volume| crate::docker_lease::lock_host_volume_name_for_domain(domain, volume),
        attest_buildkit_removal_volume,
        attest_buildkit_removal_container,
        operation,
    )
}

#[cfg(unix)]
fn lock_domain_buildkit_volume_for_current_transaction(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
    volume: &str,
) -> Result<crate::docker_lease::VolumeOperationLocks> {
    if pending_buildkit_create_transaction(domain, builder)?.is_none() {
        return crate::docker_lease::lock_host_volume_name_for_domain(domain, volume);
    }
    let creator = read_live_builder_creator(domain, builder)?
        .context("pending BuildKit volume operation has no live creator lease")?;
    let access =
        pending_buildkit_create_access(domain, builder, config_fingerprint, creator.generation)?
            .context("pending BuildKit volume operation has no matching transaction access")?;
    crate::docker_lease::lock_host_volume_name_for_pending_create(domain, volume, &access)
}

fn with_attested_domain_builder_with<G, T>(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    acquire_volume_lock: impl FnOnce(&Path, &str, &str) -> Result<G>,
    mut inspect_volume: impl FnMut(&PersistentBuildKitDomain, &str) -> Result<bool>,
    inspect_container: impl FnOnce(
        &PersistentBuildKitDomain,
        &str,
        &str,
        &str,
    ) -> Result<Option<String>>,
    operation: impl FnOnce(&str, &str, &str, bool, Option<&str>) -> Result<T>,
) -> Result<T> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse operation on a BuildKit builder from another domain");
    }
    let daemon = daemon_container_name(builder);
    let volume = daemon_state_volume(builder);
    let _volume_lock = acquire_volume_lock(&domain.identity_root, &domain.engine_id, &volume)?;
    // Inspect up front so uncertainty prevents container mutation.
    let volume_present = inspect_volume(domain, &volume)?;
    let container_id = if volume_present {
        inspect_container(domain, builder, &daemon, &volume)?
    } else {
        None
    };
    operation(
        builder,
        &daemon,
        &volume,
        volume_present,
        container_id.as_deref(),
    )
}

/// Stop only the inspected persistent daemon, holding the same volume-name
/// lock used by setup and removal. Missing attested objects are already gone.
pub(crate) fn stop_builder_in_domain(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<bool> {
    with_attested_domain_builder(
        domain,
        builder,
        |_, daemon, _, volume_present, container_id| {
            if !volume_present {
                return Ok(false);
            }
            let Some(container_id) = container_id else {
                return Ok(false);
            };
            stop_builder_under_volume_lock(domain, builder, container_id)
                .with_context(|| format!("stop BuildKit daemon {daemon}"))
        },
    )
}

/// Restart a re-attested daemon after a holder appears during unlocked work.
pub(crate) fn start_builder_in_domain(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<bool> {
    start_builder_in_domain_matching_id(domain, builder, None)
}

/// Start a host-inspected persistent daemon only if a fresh attestation still
/// resolves the same immutable ID observed by the lease conflict path.
pub(crate) fn start_builder_in_domain_matching_id(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: Option<&str>,
) -> Result<bool> {
    let readiness = read_builder_readiness_state(domain, builder)?
        .context("refuse host restart without durable BuildKit readiness state")?;
    if expected_container_id.is_some_and(|expected| expected != readiness.container_id) {
        anyhow::bail!("refuse host restart: readiness state names another immutable container ID");
    }
    let Some((container_id, start_epoch)) = start_builder_container_in_domain_matching_id(
        domain,
        builder,
        expected_container_id,
        &readiness.config_fingerprint,
    )?
    else {
        anyhow::bail!(
            "attested BuildKit daemon {} disappeared before restart",
            daemon_container_name(builder)
        );
    };
    persist_builder_readiness_after_start(
        domain,
        builder,
        &container_id,
        &readiness.config_fingerprint,
        start_epoch,
    )?;
    Ok(true)
}

/// Recover a previously-ready daemon whose last start attempt was durable but
/// did not publish Ready. The exact immutable ID and config remain bound by
/// the Starting record; restart logic re-attests both under the shared volume
/// lock and advances the epoch before another start/probe attempt.
pub(crate) fn recover_starting_builder_in_domain(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    config_fingerprint: &str,
) -> Result<()> {
    let Some(state) = read_builder_readiness_state(domain, builder)? else {
        return Ok(());
    };
    if state.config_fingerprint != config_fingerprint {
        anyhow::bail!("persistent BuildKit config mode changed for existing builder {builder}");
    }
    if state.phase == BuilderReadinessPhase::Ready {
        return Ok(());
    }
    if pending_buildkit_create_transaction(domain, builder)?.is_some() {
        anyhow::bail!("pending BuildKit create must settle before start recovery");
    }
    if !start_builder_in_domain_matching_id(domain, builder, Some(&state.container_id))? {
        anyhow::bail!("attested BuildKit daemon disappeared during start recovery");
    }
    if !builder_readiness_matches(domain, builder, &state.container_id, config_fingerprint)? {
        anyhow::bail!("BuildKit start recovery did not publish current Ready proof");
    }
    Ok(())
}

fn require_buildkit_restart(result: Result<bool>) -> Result<()> {
    match result? {
        true => Ok(()),
        false => anyhow::bail!("attested BuildKit daemon was not restarted"),
    }
}

fn start_builder_container_in_domain_matching_id(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: Option<&str>,
    expected_config_fingerprint: &str,
) -> Result<Option<(String, u64)>> {
    start_builder_container_in_domain_matching_id_with_lock(
        domain,
        builder,
        expected_container_id,
        Some(expected_config_fingerprint),
        |_, _, volume| crate::docker_lease::lock_host_volume_name_for_domain(domain, volume),
    )
}

#[cfg(unix)]
fn start_builder_container_in_domain_matching_id_with_pending_create(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: Option<&str>,
    config_fingerprint: &str,
) -> Result<Option<(String, u64)>> {
    start_builder_container_in_domain_matching_id_with_lock(
        domain,
        builder,
        expected_container_id,
        Some(config_fingerprint),
        |_, _, volume| {
            lock_domain_buildkit_volume_for_current_transaction(
                domain,
                builder,
                config_fingerprint,
                volume,
            )
        },
    )
}

fn start_builder_container_in_domain_matching_id_with_lock<G>(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: Option<&str>,
    expected_config_fingerprint: Option<&str>,
    acquire_volume_lock: impl FnOnce(&Path, &str, &str) -> Result<G>,
) -> Result<Option<(String, u64)>> {
    with_attested_domain_builder_with(
        domain,
        builder,
        acquire_volume_lock,
        attest_buildkit_removal_volume,
        attest_buildkit_removal_container,
        |_, daemon, _, volume_present, container_id| {
            if !volume_present {
                return Ok(None);
            }
            let Some(container_id) = container_id else {
                return Ok(None);
            };
            if expected_container_id.is_some_and(|expected| expected != container_id) {
                anyhow::bail!(
                    "attested BuildKit daemon {daemon} changed immutable ID before restart"
                );
            }
            let proof = read_builder_readiness_state(domain, builder)?
                .context("refuse host restart without durable BuildKit readiness state")?;
            if proof.container_id != container_id {
                anyhow::bail!(
                    "refuse host restart of {daemon}: readiness proof names another immutable ID"
                );
            }
            if expected_config_fingerprint
                .is_some_and(|expected| expected != proof.config_fingerprint)
            {
                anyhow::bail!(
                    "refuse host restart of {daemon}: readiness proof has another config fingerprint"
                );
            }
            if !matches!(
                proof.phase,
                BuilderReadinessPhase::Ready
                    | BuilderReadinessPhase::Starting
                    | BuilderReadinessPhase::Stopping
                    | BuilderReadinessPhase::Stopped
            ) {
                anyhow::bail!("refuse start recovery from an unsupported readiness phase");
            }
            let start_epoch = start_builder_from_readiness_under_lock_with(
                domain,
                builder,
                daemon,
                container_id,
                &proof.config_fingerprint,
                proof.epoch,
                |id| crate::docker::Docker::host().inspect_exit(id),
                |id| {
                    crate::docker::Docker::host()
                        .container_start(id)
                        .map(|_| ())
                },
            )?;
            if let Some(start_epoch) = start_epoch {
                return Ok(Some((container_id.to_owned(), start_epoch)));
            }
            Ok(None)
        },
    )
}

/// Complete Buildx's name-conflict reuse path for one freshly attested
/// immutable container. A `Created` container may belong to the winning
/// concurrent create request, which still needs to copy its config before
/// starting it; wait for that owner. Restart only a previously-started
/// container (Docker supplies a nonzero FinishedAt for stopped instances).
pub(crate) fn ensure_conflicting_builder_ready_in_domain(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: &str,
    config_fingerprint: &str,
    mut other_create_in_flight: impl FnMut() -> Result<bool>,
    mut recover_unbound_created: impl FnMut(&str) -> Result<bool>,
) -> Result<bool> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse conflict recovery for a BuildKit builder from another domain");
    }
    if !builder_readiness_matches(domain, builder, expected_container_id, config_fingerprint)? {
        if let Some(starting) = read_builder_readiness_state(domain, builder)?
            && starting.phase == BuilderReadinessPhase::Starting
            && starting.container_id == expected_container_id
            && starting.config_fingerprint == config_fingerprint
            && pending_buildkit_create_transaction(domain, builder)?.is_none()
        {
            // A previously-ready daemon may have lost its response or failed
            // the worker probe after `/start`. Retry only after the normal
            // host path re-attests this same ID/volume under the shared lock.
            recover_starting_builder_in_domain(domain, builder, config_fingerprint)?;
            return Ok(true);
        }
        ensure_conflicting_builder_creator_or_readiness(
            expected_container_id,
            config_fingerprint,
            Duration::from_secs(30),
            || {
                let volume = daemon_state_volume(builder);
                let _volume_lock = lock_domain_buildkit_volume_for_current_transaction(
                    domain,
                    builder,
                    config_fingerprint,
                    &volume,
                )?;
                if !attest_buildkit_removal_volume(domain, &volume)? {
                    return Ok(None);
                }
                attest_buildkit_removal_container(
                    domain,
                    builder,
                    &daemon_container_name(builder),
                    &volume,
                )
            },
            || {
                builder_readiness_matches(
                    domain,
                    builder,
                    expected_container_id,
                    config_fingerprint,
                )
            },
            &mut other_create_in_flight,
            &mut recover_unbound_created,
            || read_live_builder_creator(domain, builder),
            |delay| std::thread::sleep(delay),
        )?;
    }
    ensure_conflicting_builder_ready_with_attestation(
        expected_container_id,
        || {
            lock_domain_buildkit_volume_for_current_transaction(
                domain,
                builder,
                config_fingerprint,
                &daemon_state_volume(builder),
            )
        },
        || attest_buildkit_removal_volume(domain, &daemon_state_volume(builder)),
        || {
            attest_buildkit_removal_container(
                domain,
                builder,
                &daemon_container_name(builder),
                &daemon_state_volume(builder),
            )
        },
        Duration::from_secs(30),
        |delay| std::thread::sleep(delay),
        |id, timeout| crate::docker::Docker::host().inspect_exit_bounded(id, timeout),
        |id| {
            let started = start_builder_container_in_domain_matching_id_with_pending_create(
                domain,
                builder,
                Some(id),
                config_fingerprint,
            )?;
            let Some((started_id, start_epoch)) = started else {
                return Ok(false);
            };
            persist_builder_readiness_after_start(
                domain,
                builder,
                &started_id,
                config_fingerprint,
                start_epoch,
            )?;
            Ok(true)
        },
        wait_for_attested_buildkit_ready,
    )
}

fn ensure_conflicting_builder_creator_or_readiness(
    expected_container_id: &str,
    config_fingerprint: &str,
    timeout: Duration,
    mut attest_candidate: impl FnMut() -> Result<Option<String>>,
    mut readiness_matches: impl FnMut() -> Result<bool>,
    mut other_create_in_flight: impl FnMut() -> Result<bool>,
    mut recover_unbound_created: impl FnMut(&str) -> Result<bool>,
    mut live_creator: impl FnMut() -> Result<Option<BuilderCreatorLeaseRecord>>,
    sleep: impl FnMut(Duration),
) -> Result<()> {
    if readiness_matches()? {
        return Ok(());
    }
    let Some(creator) = live_creator()? else {
        if readiness_matches()? {
            return Ok(());
        }
        anyhow::bail!(
            "refuse conflicting BuildKit container without a live matching creator lease"
        );
    };
    if creator.config_fingerprint != config_fingerprint {
        anyhow::bail!("live BuildKit creator config does not match the conflicting builder");
    }
    let attested_id = attest_candidate()?
        .context("conflicting BuildKit container disappeared before creator wait")?;
    if attested_id != expected_container_id {
        anyhow::bail!("conflicting BuildKit name changed immutable ID before creator wait");
    }
    match creator.container_id.as_deref() {
        Some(id) if id != expected_container_id => {
            anyhow::bail!("live BuildKit creator is bound to another immutable container ID");
        }
        Some(_) => {}
        None if other_create_in_flight()? => {}
        None => {
            if recover_unbound_created(expected_container_id)? && readiness_matches()? {
                return Ok(());
            }
        }
    };
    wait_for_live_creator_readiness_with(
        expected_container_id,
        config_fingerprint,
        timeout,
        &mut readiness_matches,
        &mut other_create_in_flight,
        &mut recover_unbound_created,
        &mut live_creator,
        sleep,
    )
}

fn wait_for_live_creator_readiness_with(
    expected_container_id: &str,
    config_fingerprint: &str,
    timeout: Duration,
    mut readiness_matches: impl FnMut() -> Result<bool>,
    mut other_create_in_flight: impl FnMut() -> Result<bool>,
    mut recover_unbound_created: impl FnMut(&str) -> Result<bool>,
    mut live_creator: impl FnMut() -> Result<Option<BuilderCreatorLeaseRecord>>,
    mut sleep: impl FnMut(Duration),
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if readiness_matches()? {
            return Ok(());
        }
        let Some(creator) = live_creator()? else {
            if readiness_matches()? {
                return Ok(());
            }
            anyhow::bail!(
                "refuse conflicting BuildKit container {expected_container_id} without a live creator lease or exact readiness proof"
            );
        };
        if creator.config_fingerprint != config_fingerprint {
            anyhow::bail!("live BuildKit creator config does not match the conflicting builder");
        }
        match creator.container_id.as_deref() {
            Some(id) if id != expected_container_id => {
                anyhow::bail!("live BuildKit creator is bound to another immutable container ID");
            }
            Some(_) => {}
            None if other_create_in_flight()? => {}
            None => {
                if recover_unbound_created(expected_container_id)? && readiness_matches()? {
                    return Ok(());
                }
            }
        }
        if creator
            .archived_config_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != config_fingerprint)
        {
            anyhow::bail!("live BuildKit creator archive does not match the expected config");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!(
                "live BuildKit creator did not publish exact readiness for {expected_container_id} before timeout"
            );
        }
        sleep(Duration::from_millis(50).min(remaining));
    }
}

fn ensure_conflicting_builder_ready_with_attestation<G>(
    expected_container_id: &str,
    acquire_volume_lock: impl FnOnce() -> Result<G>,
    inspect_volume: impl FnOnce() -> Result<bool>,
    inspect_container: impl FnOnce() -> Result<Option<String>>,
    timeout: Duration,
    sleep: impl FnMut(Duration),
    inspect_state: impl FnMut(&str, Duration) -> Result<crate::docker::client::ExitInfo>,
    start_if_attested: impl FnMut(&str) -> Result<bool>,
    wait_ready: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    // Hold the Engine-wide volume flock only for the identity check. Created
    // may be waiting for the winning Buildx request to call ContainerStart;
    // retaining this lock while polling would block that request from starting.
    let container_id = {
        let _volume_lock = acquire_volume_lock()?;
        if !inspect_volume()? {
            return Ok(false);
        }
        inspect_container()?
            .context("persistent BuildKit container has no attested immutable ID")?
    };
    if container_id != expected_container_id {
        anyhow::bail!("attested BuildKit container ID changed before conflict recovery");
    }
    ensure_conflicting_container_ready_with(
        &container_id,
        timeout,
        sleep,
        inspect_state,
        start_if_attested,
        wait_ready,
    )?;
    Ok(true)
}

fn ensure_conflicting_container_ready_with(
    container_id: &str,
    timeout: Duration,
    mut sleep: impl FnMut(Duration),
    mut inspect: impl FnMut(&str, Duration) -> Result<crate::docker::client::ExitInfo>,
    mut start_if_attested: impl FnMut(&str) -> Result<bool>,
    mut wait_ready: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("conflicting BuildKit container {container_id} remained unstarted after bounded wait");
        }
        let inspect_timeout = remaining.min(Duration::from_secs(2));
        let state = inspect(container_id, inspect_timeout)
            .context("inspect conflicting BuildKit container")?;
        match state.status {
            Some(crate::docker::client::ContainerState::Running) => {
                return wait_ready(container_id)
                    .context("wait for conflicting BuildKit daemon workers");
            }
            Some(crate::docker::client::ContainerState::Created) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    anyhow::bail!("conflicting BuildKit container {container_id} remained unstarted after bounded wait");
                }
                sleep(Duration::from_millis(250).min(remaining));
            }
            Some(
                crate::docker::client::ContainerState::Exited
                | crate::docker::client::ContainerState::Dead,
            ) if state.finished.is_some() => {
                if !start_if_attested(container_id)? {
                    anyhow::bail!("conflicting BuildKit container disappeared before restart");
                }
                return wait_ready(container_id)
                    .context("wait for restarted conflicting BuildKit daemon workers");
            }
            status => anyhow::bail!(
                "cannot safely reuse conflicting BuildKit container in state {status:?}"
            ),
        }
    }
}

/// Buildx registrations are job-local, so host `docker buildx` cannot address
/// these builders. Run buildctl inside the already attested immutable daemon
/// container instead; the explicit socket ignores any inherited BUILDKIT_HOST.
fn buildctl_prune_args(container_id: &str) -> Vec<String> {
    vec![
        "exec".into(),
        container_id.into(),
        "buildctl".into(),
        "--addr".into(),
        "unix:///run/buildkit/buildkitd.sock".into(),
        "prune".into(),
    ]
}

fn buildctl_ready_args(container_id: &str) -> Vec<String> {
    vec![
        "exec".into(),
        container_id.into(),
        "buildctl".into(),
        "--addr".into(),
        "unix:///run/buildkit/buildkitd.sock".into(),
        "debug".into(),
        "workers".into(),
    ]
}

fn retry_until_buildkit_ready(
    timeout: Duration,
    mut probe: impl FnMut(Duration) -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match probe(remaining.min(Duration::from_secs(2))) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            std::thread::sleep(remaining.min(Duration::from_millis(250)));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("BuildKit readiness deadline elapsed")))
        .context("BuildKit daemon did not become ready before the pressure-prune deadline")
}

fn ensure_pressure_builder_ready(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    expected_container_id: &str,
    docker_root: &Path,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
    pressure_predicate: &dyn Fn(&crate::host_capacity::HostCapacity) -> bool,
) -> Result<bool> {
    // Decide whether this daemon is a candidate and, if stopped, persist its
    // new Starting epoch and issue ContainerStart under the Engine-volume
    // flock. Drop that flock before the worker probe; the readiness helper
    // reacquires it and CAS-publishes only this start's epoch.
    let start = with_attested_domain_builder(
        domain,
        builder,
        |builder, daemon, volume, volume_present, container_id| {
            if !volume_present {
                return Ok(None);
            }
            let Some(container_id) = container_id else {
                return Ok(None);
            };
            if container_id != expected_container_id {
                anyhow::bail!("pressure candidate {daemon} changed immutable container ID");
            }
            let Some(proof) = read_builder_readiness_state(domain, builder)? else {
                return Ok(None);
            };
            if proof.container_id != container_id {
                anyhow::bail!("pressure candidate {daemon} has mismatched readiness identity");
            }
            if proof.config_fingerprint.is_empty() {
                anyhow::bail!("pressure candidate {daemon} has an empty config fingerprint");
            }
            let projection = crate::docker::client::host_call(
                &crate::docker_lease::inspect_persistent_buildkit_volume_pressure_args(volume),
            )
            .with_context(|| format!("inspect BuildKit pressure volume {volume}"))?;
            let mountpoint = crate::docker_lease::attest_persistent_buildkit_volume_mountpoint(
                &projection,
                volume,
                &domain.token,
            )?;
            let mountpoint = PathBuf::from(mountpoint);
            if !pressure_mountpoint_matches_device(
                &mountpoint,
                docker_root,
                expected_pressure.device_id(),
            )? {
                return Ok(None);
            }
            let mount_pin = crate::host_capacity::HostCapacityPin::open(&mountpoint)
                .context("pin BuildKit volume before pressure restart")?;
            let mount_sample = mount_pin
                .probe()
                .context("inspect BuildKit volume filesystem before pressure restart")?;
            if mount_sample.filesystem_device != expected_pressure.device_id()
                || mount_sample.volume_fingerprint.as_deref() != Some(expected_volume_uuid)
            {
                return Ok(None);
            }
            let pressure = pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)?;
            if !pressure_predicate(&pressure) {
                return Ok(None);
            }
            let state = crate::docker::Docker::host()
                .inspect_exit(container_id)
                .context("inspect pressure candidate before restart")?;
            if state.status == Some(crate::docker::client::ContainerState::Running) {
                if proof.phase == BuilderReadinessPhase::Ready {
                    return Ok(Some((proof.config_fingerprint, None)));
                }
                let epoch = invalidate_builder_readiness_before_start(
                    domain,
                    builder,
                    container_id,
                    &proof.config_fingerprint,
                    proof.epoch,
                )?;
                return Ok(Some((proof.config_fingerprint, Some(epoch))));
            }
            if !matches!(
                state.status,
                Some(
                    crate::docker::client::ContainerState::Created
                        | crate::docker::client::ContainerState::Exited
                        | crate::docker::client::ContainerState::Dead
                )
            ) {
                anyhow::bail!(
                    "cannot safely pressure-start BuildKit daemon in state {:?}",
                    state.status
                );
            }
            let epoch = invalidate_builder_readiness_before_start(
                domain,
                builder,
                container_id,
                &proof.config_fingerprint,
                proof.epoch,
            )?;
            if !start_attested_builder_confirmed(container_id)? {
                anyhow::bail!("attested BuildKit daemon disappeared during pressure restart");
            }
            Ok(Some((proof.config_fingerprint, Some(epoch))))
        },
    )?;
    let Some((config_fingerprint, start_epoch)) = start else {
        return Ok(false);
    };
    if let Some(start_epoch) = start_epoch {
        persist_builder_readiness_after_start(
            domain,
            builder,
            expected_container_id,
            &config_fingerprint,
            start_epoch,
        )?;
    } else {
        wait_for_attested_buildkit_ready(expected_container_id)?;
    }
    Ok(true)
}

pub(crate) fn wait_for_attested_buildkit_ready(container_id: &str) -> Result<()> {
    let args = buildctl_ready_args(container_id);
    retry_until_buildkit_ready(Duration::from_secs(30), |timeout| {
        crate::docker::client::host_call_bounded(&args, timeout).map(|_| ())
    })
}

fn stop_attested_builder_confirmed(container_id: &str) -> Result<bool> {
    stop_attested_builder_confirmed_with(
        container_id,
        |id| {
            crate::docker::Docker::host()
                .container_stop(id, None)
                .map(|_| ())
        },
        |id| crate::docker::Docker::host().inspect_exit(id),
    )
}

fn stop_attested_builder_confirmed_with(
    container_id: &str,
    mut stop: impl FnMut(&str) -> Result<()>,
    mut inspect: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
) -> Result<bool> {
    match stop(container_id) {
        Ok(_) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(stop_error) => {
            let inspected = inspect(container_id)
                .context("inspect attested BuildKit daemon after ambiguous stop")?;
            if inspected
                .status
                .is_some_and(crate::docker::client::ContainerState::safe_to_reclaim)
            {
                tracing::warn!(
                    target: "velnor.buildkit",
                    container_id,
                    error = format!("{stop_error:#}"),
                    "BuildKit stop returned an error but inspect confirms it stopped"
                );
                Ok(true)
            } else {
                Err(stop_error).context(format!(
                    "BuildKit stop returned an error; inspect reports {:?}",
                    inspected.status
                ))
            }
        }
    }
}

fn start_attested_builder_confirmed(container_id: &str) -> Result<bool> {
    start_attested_builder_confirmed_with(
        container_id,
        |id| crate::docker::Docker::host().inspect_exit(id),
        |id| {
            crate::docker::Docker::host()
                .container_start(id)
                .map(|_| ())
        },
    )
}

fn start_attested_builder_confirmed_with(
    container_id: &str,
    mut inspect: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    mut start: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    let before = inspect(container_id).context("inspect attested BuildKit daemon before start")?;
    match before.status {
        Some(crate::docker::client::ContainerState::Running) => return Ok(true),
        Some(
            crate::docker::client::ContainerState::Created
            | crate::docker::client::ContainerState::Exited
            | crate::docker::client::ContainerState::Dead,
        ) => {}
        state => anyhow::bail!("cannot safely start BuildKit daemon in state {state:?}"),
    }
    match start(container_id) {
        Ok(()) => Ok(true),
        Err(error) if crate::docker::client::is_not_found(&error) => Ok(false),
        Err(start_error) => {
            let after = inspect(container_id)
                .context("verify attested BuildKit daemon after ambiguous start")?;
            if after.status == Some(crate::docker::client::ContainerState::Running) {
                tracing::warn!(
                    target: "velnor.buildkit",
                    container_id,
                    error = format!("{start_error:#}"),
                    "BuildKit start returned an error but inspect confirms the daemon is running"
                );
                Ok(true)
            } else {
                Err(start_error).context(format!(
                    "BuildKit start returned an error; inspect reports {:?}",
                    after.status
                ))
            }
        }
    }
}

/// Advance and retry one durable start while the caller holds the shared
/// Engine/state-volume lock and has attested the immutable container shape.
/// `Starting` is retryable only for the same ID, config, and epoch; an older
/// probe can never publish over the new attempt.
fn start_builder_from_readiness_under_lock_with(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    daemon: &str,
    container_id: &str,
    config_fingerprint: &str,
    expected_epoch: u64,
    inspect: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    start: impl FnMut(&str) -> Result<()>,
) -> Result<Option<u64>> {
    let current = read_builder_readiness_state(domain, builder)?
        .context("BuildKit start recovery lost its durable readiness record")?;
    if !matches!(
        current.phase,
        BuilderReadinessPhase::Ready
            | BuilderReadinessPhase::Starting
            | BuilderReadinessPhase::Stopping
            | BuilderReadinessPhase::Stopped
    ) || current.epoch != expected_epoch
        || current.container_id != container_id
        || current.config_fingerprint != config_fingerprint
    {
        anyhow::bail!(
            "BuildKit start recovery no longer matches the exact durable ID/config epoch"
        );
    }
    let next_epoch = invalidate_builder_readiness_before_start(
        domain,
        builder,
        container_id,
        config_fingerprint,
        expected_epoch,
    )?;
    if start_attested_builder_confirmed_with(container_id, inspect, start)
        .with_context(|| format!("retry start for BuildKit daemon {daemon}"))?
    {
        Ok(Some(next_epoch))
    } else {
        Ok(None)
    }
}

fn inspect_domain_builder_exit(
    domain: &PersistentBuildKitDomain,
    builder: &str,
) -> Result<Option<crate::docker::client::ExitInfo>> {
    with_attested_domain_builder(domain, builder, |_, daemon, _, volume_present, id| {
        if !volume_present {
            return Ok(None);
        }
        let Some(id) = id else {
            return Ok(None);
        };
        crate::docker::Docker::host()
            .inspect_exit(id)
            .map(Some)
            .with_context(|| format!("inspect BuildKit daemon {daemon} after stop"))
    })
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
    if let Some(registry_root) = registry_root
        && is_current_domained_persistent_builder(builder)
    {
        let owner = read_owner_record(registry_root, builder)?
            .context("refuse BuildKit cleanup without a durable owner record")?;
        if owner.phase != BuilderOwnerPhase::Deleting {
            mark_owner_record_deleting(registry_root, builder)?;
        }
    }
    // Keep the same per-builder lock from the final empty-claim proof through
    // a durable Deleting tombstone, exact daemon/volume absence proof, owner
    // unlink, and claims unlink. Setup rejects Deleting and cannot publish a
    // replacement claim between these steps.
    remove(builder)?;
    remove_builder_auxiliary_metadata(run_root, builder)?;
    unlink_owner_before_claims(registry_root, builder, &path, || Ok(()))?;
    Ok(true)
}

fn unlink_owner_before_claims(
    registry_root: Option<&Path>,
    builder: &str,
    claims_path: &Path,
    after_owner_unlink: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Some(registry_root) = registry_root {
        remove_owner_record(registry_root, builder)?;
    }
    after_owner_unlink()?;
    match std::fs::remove_file(claims_path) {
        Ok(()) => sync_parent(claims_path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("remove {}", claims_path.display()))
        }
    }
    Ok(())
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
pub(crate) fn remove_builder(domain: &PersistentBuildKitDomain, builder: &str) -> Result<()> {
    remove_builder_with(
        domain,
        builder,
        |_, _, volume| crate::docker_lease::lock_host_volume_name_for_domain(domain, volume),
        attest_buildkit_removal_volume,
        attest_buildkit_removal_container,
        |container_id, daemon| {
            if let Err(error) =
                crate::docker::Docker::host().container_remove(container_id, true, false)
                && !crate::docker::client::is_not_found(&error)
            {
                return Err(error).with_context(|| format!("remove BuildKit daemon {daemon}"));
            }
            Ok(())
        },
        |volume| {
            let args = vec![
                "volume".to_string(),
                "rm".to_string(),
                "--force".to_string(),
                "--".to_string(),
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

fn remove_builder_with<G>(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    acquire_volume_lock: impl FnOnce(&Path, &str, &str) -> Result<G>,
    mut inspect_volume: impl FnMut(&PersistentBuildKitDomain, &str) -> Result<bool>,
    mut inspect_container: impl FnMut(
        &PersistentBuildKitDomain,
        &str,
        &str,
        &str,
    ) -> Result<Option<String>>,
    mut remove_container: impl FnMut(&str, &str) -> Result<()>,
    mut remove_volume: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    if persistent_builder_domain_token(builder) != Some(domain.token.as_str()) {
        anyhow::bail!("refuse removal of a BuildKit builder from another domain");
    }
    let daemon = daemon_container_name(builder);
    let volume = daemon_state_volume(builder);
    let _volume_lock = acquire_volume_lock(&domain.identity_root, &domain.engine_id, &volume)?;

    // Only a definitive 404 means absent; inspect uncertainty prevents every
    // mutation. The container's inspected immutable ID survives a name race.
    let _volume_present = inspect_volume(domain, &volume)?;
    let container_id = inspect_container(domain, builder, &daemon, &volume)?;
    let removal_epoch = if let Some(container_id) = container_id.as_deref() {
        invalidate_builder_readiness_before_stop(domain, builder, container_id)?
            .map(|(fingerprint, epoch)| (container_id.to_owned(), fingerprint, epoch))
    } else {
        None
    };
    if let Some(container_id) = container_id {
        remove_container(&container_id, &daemon)?;
    }

    // Force-removing a running container also stops it. Confirm the exact
    // immutable ID is absent, then publish Stopped before deleting its volume
    // or releasing the shared lifecycle lock. An uncertain delete keeps the
    // durable Stopping phase and therefore cannot restore exec authority.
    if inspect_container(domain, builder, &daemon, &volume)?.is_some() {
        anyhow::bail!("BuildKit daemon {daemon} remains after removal");
    }
    if let Some((container_id, config_fingerprint, epoch)) = removal_epoch {
        publish_builder_stopped_after_stop(
            domain,
            builder,
            &container_id,
            &config_fingerprint,
            epoch,
        )?;
    }

    // Docker volumes have no immutable ID. Re-attest after container removal
    // and immediately before rm so a same-name replacement survives.
    if inspect_volume(domain, &volume)? {
        remove_volume(&volume)?;
    }
    if inspect_volume(domain, &volume)? {
        anyhow::bail!("BuildKit state volume {volume} remains after removal");
    }
    Ok(())
}

fn attest_buildkit_removal_volume(domain: &PersistentBuildKitDomain, volume: &str) -> Result<bool> {
    let args = crate::docker_lease::inspect_persistent_buildkit_volume_args(volume);
    let output = match crate::docker::client::host_call(&args) {
        Ok(output) => output,
        Err(error) if crate::docker::client::is_not_found(&error) => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect BuildKit state volume {volume}"));
        }
    };
    crate::docker_lease::attest_persistent_buildkit_volume_identity(
        &output,
        volume,
        &domain.token,
    )?;
    Ok(true)
}

fn attest_buildkit_removal_container(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    daemon: &str,
    volume: &str,
) -> Result<Option<String>> {
    let args = crate::docker_lease::inspect_persistent_buildkit_container_args(daemon);
    let output = match crate::docker::client::host_call(&args) {
        Ok(output) => output,
        Err(error) if crate::docker::client::is_not_found(&error) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect BuildKit daemon {daemon}"));
        }
    };
    crate::docker_lease::attest_persistent_buildkit_container_identity(
        &output,
        builder,
        volume,
        &domain.token,
    )
    .map(Some)
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

/// Reclaim BuildKit cache only when the exact domain volume is a local host
/// mount on the pressured filesystem. `buildctl du` deltas do not prove host
/// space was released, so `freed_bytes` reports only the observed `statvfs`
/// increase on the pinned pressure filesystem; `pruned` lists successful calls.
pub(crate) fn reclaim_domain_buildkit_for_device(
    domain: &PersistentBuildKitDomain,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    pressure_predicate: &dyn Fn(&crate::host_capacity::HostCapacity) -> bool,
) -> Result<PressurePruneReport> {
    let mut report = PressurePruneReport::default();
    let initial = expected_pressure
        .probe()
        .context("revalidate pinned pressure filesystem before BuildKit inventory")?;
    if initial.filesystem_device != expected_pressure.device_id() {
        anyhow::bail!("pinned pressure filesystem device changed before BuildKit reclaim");
    }
    let expected_volume_uuid = initial
        .volume_fingerprint
        .as_deref()
        .filter(|uuid| !uuid.is_empty())
        .context("pinned pressure filesystem has no stable UUID")?
        .to_owned();
    if !pressure_predicate(&initial) {
        return Ok(report);
    }
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(&domain.root)
        .context("lock BuildKit domain for device-bound pressure reclaim")?;
    let current = pressure_sample_matches_pin(expected_pressure, &expected_volume_uuid)?;
    if !pressure_predicate(&current) {
        return Ok(report);
    }
    let docker_root = trusted_host_docker_root_for_pressure(domain)?;
    let docker_root_pin = crate::host_capacity::HostCapacityPin::open(&docker_root)
        .context("pin attested Docker root for pressure prune")?;
    let registry_root = owner_registry_root(&domain.root);
    let builders = registered_domain_builders(&registry_root, &domain.token)
        .context("list domain BuildKit owners for device-bound pressure reclaim")?;
    if builders.is_empty() {
        return Ok(report);
    }
    let present = running_container_names()
        .context("list Engine containers before device-bound BuildKit pressure reclaim")?;
    for builder in builders {
        let current = pressure_sample_matches_pin(expected_pressure, &expected_volume_uuid)?;
        if !pressure_predicate(&current) {
            break;
        }
        let _builder_lifecycle = lock_builder_lifecycle(&domain.root, &builder)
            .with_context(|| format!("lock BuildKit builder {builder} for pressure prune"))?;
        let current = pressure_sample_matches_pin(expected_pressure, &expected_volume_uuid)?;
        if !pressure_predicate(&current) {
            break;
        }
        if !repair_pressure_claims(&domain.root, &registry_root, &builder, &present)
            .with_context(|| format!("repair pressure claims for {builder}"))?
        {
            continue;
        }
        let prune_succeeded = std::cell::Cell::new(false);
        match prune_domain_builder_on_device(
            domain,
            &builder,
            &docker_root,
            &docker_root_pin,
            expected_pressure,
            &expected_volume_uuid,
            pressure_predicate,
            &prune_succeeded,
        ) {
            Ok(Some(bytes)) => {
                report.pruned.push(builder);
                report.freed_bytes = report.freed_bytes.saturating_add(bytes);
            }
            Ok(None) => {}
            Err(error) => {
                if prune_succeeded.get() {
                    report.pruned.push(builder.clone());
                }
                report.failures.push(format!(
                    "skip BuildKit pressure prune for {builder}: {error:#}"
                ));
                tracing::warn!(
                    target: "velnor.buildkit",
                    builder,
                    error = format!("{error:#}"),
                    "skipping BuildKit pressure prune because host storage identity is unproven"
                );
                break;
            }
        }
    }
    Ok(report)
}

fn prune_domain_builder_on_device(
    domain: &PersistentBuildKitDomain,
    builder: &str,
    docker_root: &Path,
    docker_root_pin: &crate::host_capacity::HostCapacityPin,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
    pressure_predicate: &dyn Fn(&crate::host_capacity::HostCapacity) -> bool,
    prune_succeeded: &std::cell::Cell<bool>,
) -> Result<Option<u64>> {
    let Some(ready) = read_builder_readiness(domain, builder)? else {
        return Ok(None);
    };
    if !ensure_pressure_builder_ready(
        domain,
        builder,
        &ready.container_id,
        docker_root,
        expected_pressure,
        expected_volume_uuid,
        pressure_predicate,
    )? {
        return Ok(None);
    }
    with_attested_domain_builder(domain, builder, |builder, daemon, _, volume_present, id| {
        if !volume_present {
            return Ok(None);
        }
        let Some(id) = id else {
            return Ok(None);
        };
        let Some(proof) = read_builder_readiness(domain, builder)? else {
            return Ok(None);
        };
        if proof.container_id != id {
            anyhow::bail!(
                "refuse pressure prune of daemon {daemon} without matching readiness proof"
            );
        }
        let volume = daemon_state_volume(builder);
        let projection = crate::docker::client::host_call(
            &crate::docker_lease::inspect_persistent_buildkit_volume_pressure_args(&volume),
        )
        .with_context(|| format!("inspect BuildKit pressure volume {volume}"))?;
        let mountpoint = crate::docker_lease::attest_persistent_buildkit_volume_mountpoint(
            &projection,
            &volume,
            &domain.token,
        )?;
        let mountpoint = PathBuf::from(mountpoint);
        let mount_pin = crate::host_capacity::HostCapacityPin::open(&mountpoint)
            .context("pin attested BuildKit volume mountpoint")?;
        pressure_mountpoint_matches_pin(
            &mount_pin,
            expected_volume_uuid,
            expected_pressure.device_id(),
        )?;
        let freed = prune_candidate_for_device_with(
            Some(&mountpoint),
            docker_root,
            expected_pressure.device_id(),
            pressure_mountpoint_matches_device,
            || {
                pressure_mountpoint_matches_pin(
                    &mount_pin,
                    expected_volume_uuid,
                    expected_pressure.device_id(),
                )?;
                let before = pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)?;
                if !pressure_predicate(&before) {
                    return Ok(0);
                }
                let state = crate::docker::Docker::host()
                    .inspect_exit(id)
                    .with_context(|| {
                        format!("inspect BuildKit daemon {daemon} before pressure prune")
                    })?;
                if state.status != Some(crate::docker::client::ContainerState::Running) {
                    anyhow::bail!("BuildKit daemon {daemon} stopped before pressure prune");
                }
                let prune_result = run_pressure_prune_with_pin(
                    expected_pressure,
                    expected_volume_uuid,
                    pressure_predicate,
                    || {
                        docker_root_pin
                            .revalidate()
                            .context("revalidate pinned Docker root before buildctl")?;
                        pressure_mountpoint_matches_pin(
                            &mount_pin,
                            expected_volume_uuid,
                            expected_pressure.device_id(),
                        )?;
                        let before_buildctl =
                            pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)?;
                        if !pressure_predicate(&before_buildctl) {
                            return Ok(());
                        }
                        let buildctl_result =
                            crate::docker::client::host_call(&buildctl_prune_args(id))
                                .with_context(|| format!("prune BuildKit cache in {daemon}"))
                                .map(|_| ());
                        let docker_root_after = docker_root_pin.revalidate();
                        let mountpoint_after = pressure_mountpoint_matches_pin(
                            &mount_pin,
                            expected_volume_uuid,
                            expected_pressure.device_id(),
                        );
                        buildctl_result?;
                        prune_succeeded.set(true);
                        docker_root_after
                            .context("revalidate pinned Docker root after buildctl")?;
                        mountpoint_after
                    },
                );
                let before_stop =
                    pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)
                        .map(|capacity| pressure_predicate(&capacity));
                let stop_result = stop_builder_under_volume_lock(domain, builder, id)
                    .with_context(|| format!("stop BuildKit daemon {daemon} after pressure prune"));
                let after_stop =
                    pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)
                        .map(|capacity| pressure_predicate(&capacity));
                stop_result?;
                let _pressure_before_stop = before_stop?;
                let _pressure_after_stop = after_stop?;
                let prune_result = prune_result?;
                Ok(prune_result.unwrap_or(0))
            },
        )?;
        Ok(prune_succeeded.get().then_some(freed))
    })
}

fn pressure_sample_matches_pin(
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
) -> Result<crate::host_capacity::HostCapacity> {
    let capacity = expected_pressure
        .probe()
        .context("revalidate pinned pressure filesystem during BuildKit prune")?;
    if capacity.filesystem_device != expected_pressure.device_id()
        || capacity.volume_fingerprint.as_deref() != Some(expected_volume_uuid)
    {
        anyhow::bail!("pinned pressure filesystem UUID or device changed during BuildKit prune");
    }
    Ok(capacity)
}

fn pressure_mountpoint_matches_pin(
    mount_pin: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
    expected_device: u64,
) -> Result<()> {
    let capacity = mount_pin
        .probe()
        .context("revalidate attested BuildKit volume mountpoint")?;
    if capacity.filesystem_device != expected_device
        || capacity.volume_fingerprint.as_deref() != Some(expected_volume_uuid)
    {
        anyhow::bail!("attested BuildKit volume mountpoint changed UUID or device");
    }
    Ok(())
}

fn run_pressure_prune_with_pin(
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
    pressure_predicate: &dyn Fn(&crate::host_capacity::HostCapacity) -> bool,
    prune: impl FnOnce() -> Result<()>,
) -> Result<Option<u64>> {
    let before = pressure_sample_matches_pin(expected_pressure, expected_volume_uuid)?;
    if !pressure_predicate(&before) {
        return Ok(None);
    }
    let prune_result = prune();
    let after = pressure_sample_matches_pin(expected_pressure, expected_volume_uuid);
    let after = after?;
    prune_result?;
    Ok(Some(
        after.available_bytes.saturating_sub(before.available_bytes),
    ))
}

fn prune_candidate_for_device_with(
    mountpoint: Option<&Path>,
    docker_root: &Path,
    pressure_device: u64,
    same_device: impl Fn(&Path, &Path, u64) -> Result<bool>,
    prune: impl FnOnce() -> Result<u64>,
) -> Result<u64> {
    let Some(mountpoint) = mountpoint else {
        return Ok(0);
    };
    if !same_device(mountpoint, docker_root, pressure_device)? {
        return Ok(0);
    }
    prune()
}

fn pressure_mountpoint_matches_device(
    mountpoint: &Path,
    docker_root: &Path,
    pressure_device: u64,
) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let entry = std::fs::symlink_metadata(mountpoint)
            .with_context(|| format!("inspect BuildKit mountpoint {}", mountpoint.display()))?;
        if !entry.file_type().is_dir() {
            anyhow::bail!("BuildKit volume mountpoint is not a real directory");
        }
        let root = std::fs::canonicalize(docker_root)
            .with_context(|| format!("resolve Docker root {}", docker_root.display()))?;
        let mounted = std::fs::canonicalize(mountpoint)
            .with_context(|| format!("resolve BuildKit mountpoint {}", mountpoint.display()))?;
        if !mounted.starts_with(&root) {
            return Ok(false);
        }
        Ok(std::fs::metadata(&mounted)
            .with_context(|| format!("stat BuildKit mountpoint {}", mounted.display()))?
            .dev()
            == pressure_device)
    }
    #[cfg(not(unix))]
    {
        let _ = (mountpoint, docker_root, pressure_device);
        anyhow::bail!("device-bound BuildKit pressure reclaim requires Unix filesystem identity")
    }
}

fn trusted_host_docker_root_for_pressure(domain: &PersistentBuildKitDomain) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let endpoint = crate::docker::engine::resolve_docker_endpoint()
            .context("resolve local Docker Engine for pressure reclaim")?;
        let identity = crate::docker::engine::daemon_identity_blocking(&endpoint.socket)
            .context("read local Docker Engine identity for pressure reclaim")?;
        if identity.id != domain.engine_id {
            anyhow::bail!("Docker Engine changed before pressure reclaim");
        }
        let host_kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .context("read host kernel identity for pressure reclaim")?;
        if identity.kernel_version.trim() != host_kernel.trim() {
            anyhow::bail!("Docker Engine does not share the host kernel namespace");
        }
        let host_mount_namespace = std::fs::read_link("/proc/self/ns/mnt")
            .context("read host mount namespace for pressure reclaim")?;
        let stream =
            std::os::unix::net::UnixStream::connect(&endpoint.socket).with_context(|| {
                format!("connect local Docker socket {}", endpoint.socket.display())
            })?;
        let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
        let mut credentials_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the returned struct is initialized by SO_PEERCRED when the
        // call succeeds, and its length matches the supplied buffer.
        let peer_status = unsafe {
            libc::getsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&stream),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                credentials.as_mut_ptr().cast(),
                &mut credentials_len,
            )
        };
        if peer_status != 0 || credentials_len as usize != std::mem::size_of::<libc::ucred>() {
            anyhow::bail!("cannot identify the local Docker socket peer");
        }
        // SAFETY: getsockopt succeeded and wrote the full ucred structure.
        let credentials = unsafe { credentials.assume_init() };
        if credentials.pid <= 0 {
            anyhow::bail!("Docker socket peer has no process identity");
        }
        let peer_namespace_path = PathBuf::from(format!("/proc/{}/ns/mnt", credentials.pid));
        let peer_mount_namespace = std::fs::read_link(&peer_namespace_path).with_context(|| {
            format!(
                "read Docker peer mount namespace {}",
                peer_namespace_path.display()
            )
        })?;
        if peer_mount_namespace != host_mount_namespace {
            anyhow::bail!("Docker Engine does not share the host mount namespace");
        }
        let host_root = Path::new("/proc/self/root");
        let peer_root = PathBuf::from(format!("/proc/{}/root", credentials.pid));
        if !same_filesystem_object(host_root, &peer_root).with_context(|| {
            format!(
                "compare host and Docker peer roots {} and {}",
                host_root.display(),
                peer_root.display()
            )
        })? {
            anyhow::bail!("Docker Engine does not share the host filesystem root");
        }
        let peer_name = std::fs::read_to_string(format!("/proc/{}/comm", credentials.pid))
            .context("read Docker socket peer process name")?;
        if peer_name.trim() != "dockerd" {
            anyhow::bail!("Docker socket is served by an untrusted host proxy");
        }
        drop(stream);

        let info = crate::docker::client::host_call(&[
            "info".to_owned(),
            "--format".to_owned(),
            "{{.ID}}\n{{json .DockerRootDir}}".to_owned(),
        ])
        .context("read Docker Engine root for pressure reclaim")?;
        let mut lines = info.lines();
        let engine_id = lines.next().context("Docker info omitted Engine ID")?;
        let root_json = lines.next().context("Docker info omitted root directory")?;
        if lines.next().is_some() || engine_id != domain.engine_id {
            anyhow::bail!("Docker CLI root identity does not match the Engine API domain");
        }
        let root: String = serde_json::from_str(root_json)
            .context("parse Docker root directory from Engine info")?;
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            anyhow::bail!("Docker Engine root directory is not absolute");
        }
        std::fs::canonicalize(&root)
            .with_context(|| format!("resolve trusted Docker root {}", root.display()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = domain;
        anyhow::bail!("BuildKit pressure pruning requires a native Linux Docker Engine")
    }
}

#[cfg(target_os = "linux")]
fn same_filesystem_object(left: &Path, right: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = std::fs::metadata(left)
        .with_context(|| format!("stat filesystem identity {}", left.display()))?;
    let right = std::fs::metadata(right)
        .with_context(|| format!("stat filesystem identity {}", right.display()))?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
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
    registry_root: &Path,
    domain_token: Option<&str>,
    registered_builders: &BTreeSet<String>,
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
        let bytes = match read_control_file_no_follow(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(error) => {
                report.failures.push(format!(
                    "open owner record {} safely: {error:#}",
                    path.display()
                ));
                continue;
            }
        };
        let identity: BuilderOwnerIdentity = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                report.failures.push(format!(
                    "parse owner identity {}: {error:#}",
                    path.display()
                ));
                continue;
            }
        };
        if identity.version != OWNER_REGISTRY_VERSION {
            let belongs_to_domain = domain_token.map_or_else(
                || is_current_domained_persistent_builder(&identity.builder),
                |token| is_current_domain_builder_name(&identity.builder, token),
            );
            if belongs_to_domain {
                report.failures.push(format!(
                    "retain current BuildKit owner record {} with unsupported schema version {}",
                    path.display(),
                    identity.version
                ));
            }
            continue;
        }
        let record: BuilderOwnerRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                report.failures.push(format!(
                    "parse current owner record {}: {error:#}",
                    path.display()
                ));
                continue;
            }
        };
        if !domain_token.map_or_else(
            || is_current_domained_persistent_builder(&record.builder),
            |token| is_current_domain_builder_name(&record.builder, token),
        ) || owner_registry_file(registry_root, &record.builder) != path
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
        // Owner inventory absence says nothing about the corresponding
        // Docker daemon or volume. Only the normal Deleting tombstone path,
        // with strict Docker re-attestation, may remove durable metadata.
        report.failures.push(format!(
            "retain BuildKit owner record for {}: absent from owner inventory without Docker absence proof",
            record.builder
        ));
    }
}

/// Recover the claims-first crash cut. Setup writes a valid claim before its
/// Active owner record and performs no Docker side effect until both exist;
/// deletion removes the owner only after Docker absence is proved, then
/// unlinks claims. Thus a current-domain claims-only file is either a setup
/// interrupted before Docker work or a completed deletion interrupted during
/// metadata cleanup. Recheck holders under the claim lock and never infer
/// Docker deletion authority from an absent owner record.
fn reconcile_claims_without_owner(
    run_root: &Path,
    registry_root: &Path,
    domain_token: Option<&str>,
    present: &BTreeSet<String>,
    report: &mut HorizonReport,
) -> Vec<String> {
    let claims_root = run_root.join(CLAIMS_DIR);
    let entries = match std::fs::read_dir(&claims_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            report.failures.push(format!(
                "list BuildKit claims without owner under {}: {error:#}",
                claims_root.display()
            ));
            return Vec::new();
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
                .push(format!("read BuildKit claims entry: {error:#}")),
        }
    }
    paths.sort();
    let mut recovered = Vec::new();
    for path in paths {
        let bytes = match read_control_file_no_follow(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(error) => {
                report.failures.push(format!(
                    "read ownerless BuildKit claims {}: {error:#}",
                    path.display()
                ));
                continue;
            }
        };
        let claims = match parse_claims(&path, &bytes) {
            Ok(claims) => claims,
            Err(error) => {
                report.failures.push(format!(
                    "keep unreadable ownerless BuildKit claims {}: {error:#}",
                    path.display()
                ));
                continue;
            }
        };
        let builder = claims.builder.as_str();
        if !domain_token.map_or_else(
            || is_current_domained_persistent_builder(builder),
            |token| is_current_domain_builder_name(builder, token),
        ) || claims_file(run_root, builder) != path
        {
            report.failures.push(format!(
                "keep mismatched ownerless BuildKit claims {}",
                path.display()
            ));
            continue;
        }
        let _lock = match lock_claims(builder, &path) {
            Ok(lock) => lock,
            Err(error) => {
                report.failures.push(format!(
                    "lock ownerless BuildKit claims for {builder}: {error:#}"
                ));
                continue;
            }
        };
        match read_owner_record(registry_root, builder) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) => {
                report.failures.push(format!(
                    "read owner for ownerless BuildKit claims {builder}: {error:#}"
                ));
                continue;
            }
        }
        let mut claims = match read_registered_claims(&path, builder) {
            Ok(Some(claims)) => claims,
            Ok(None) => continue,
            Err(error) => {
                report.failures.push(format!(
                    "keep ownerless BuildKit claims for {builder}: {error:#}"
                ));
                continue;
            }
        };
        repair_absent_unlocked(&mut claims, present);
        if let Err(error) = write_claims(&path, &claims) {
            report.failures.push(format!(
                "repair ownerless BuildKit claims for {builder}: {error:#}"
            ));
            continue;
        }
        if claims.holders.is_empty() {
            // No owner record means this setup never gained Docker mutation
            // authority, or an earlier tombstoned removal already proved both
            // objects absent. Claim-lock + exclusive coordinator prevents a
            // new setup from racing this metadata-only cleanup.
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if let Err(error) = sync_parent(&path) {
                        report.failures.push(format!(
                            "sync removed ownerless claims for {builder}: {error:#}"
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => report
                    .failures
                    .push(format!("remove ownerless claims for {builder}: {error:#}")),
            }
        } else {
            match ensure_active_owner_record(registry_root, builder) {
                Ok(()) => recovered.push(builder.to_owned()),
                Err(error) => report.failures.push(format!(
                    "publish Active owner after claims recovery for {builder}: {error:#}"
                )),
            }
        }
    }
    recovered
}

/// Locked holder recheck after unlocked Docker work. A torn claim file
/// reads as claimed, so the caller stops, restarts, or keeps — never
/// deletes.
fn holders_remain(path: &Path, builder: &str, registry_root: Option<&Path>) -> Result<bool> {
    let _lock = lock_claims(builder, path)?;
    match read_claims_for_reaping(path, builder, registry_root) {
        Ok(Some(claims)) => Ok(!claims.holders.is_empty()),
        Ok(None) => Ok(true),
        Err(error) => {
            log_unreadable_ownership(builder, &error.path, &error.source);
            Ok(true)
        }
    }
}

/// Converge owned builders under the filesystem-wide lifecycle lock. Current
/// names require matching owner records and readable runtime claims. Missing
/// claims fail closed because a Docker snapshot cannot prove runner admission
/// quiescence.
pub(crate) fn reap_idle_builders(
    domain: &PersistentBuildKitDomain,
    now: SystemTime,
) -> HorizonReport {
    let run_root = &domain.root;
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
    let registry_root = owner_registry_root(run_root);
    reap_idle_builders_with_domain_and_volume_gate(
        run_root,
        Some(&registry_root),
        Some(&domain.token),
        now,
        || registered_domain_builders(&registry_root, &domain.token),
        running_container_names,
        |builder| {
            let _lock = crate::docker_lease::lock_host_volume_name_for_domain(
                domain,
                &daemon_state_volume(builder),
            )?;
            Ok(())
        },
        |daemon| crate::docker::Docker::host().inspect_exit(daemon),
        |builder| stop_builder_in_domain(domain, builder),
        |builder| start_builder_in_domain(domain, builder),
        |builder| remove_builder(domain, builder),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper paths hermetic in tests"
)]
#[cfg(test)]
fn reap_idle_builders_with_registry(
    run_root: &Path,
    registry_root: Option<&Path>,
    now: SystemTime,
    list_builders: impl FnOnce() -> Result<Vec<String>>,
    list_present_containers: impl FnOnce() -> Result<BTreeSet<String>>,
    inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    stop: impl FnMut(&str) -> Result<bool>,
    start: impl FnMut(&str) -> Result<bool>,
    remove: impl FnMut(&str) -> Result<()>,
) -> HorizonReport {
    reap_idle_builders_with_domain(
        run_root,
        registry_root,
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

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper paths hermetic in tests"
)]
fn reap_idle_builders_with_domain(
    run_root: &Path,
    registry_root: Option<&Path>,
    domain_token: Option<&str>,
    now: SystemTime,
    list_builders: impl FnOnce() -> Result<Vec<String>>,
    list_present_containers: impl FnOnce() -> Result<BTreeSet<String>>,
    inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    stop: impl FnMut(&str) -> Result<bool>,
    start: impl FnMut(&str) -> Result<bool>,
    remove: impl FnMut(&str) -> Result<()>,
) -> HorizonReport {
    reap_idle_builders_with_domain_and_volume_gate(
        run_root,
        registry_root,
        domain_token,
        now,
        list_builders,
        list_present_containers,
        |_| Ok(()),
        inspect_exit,
        stop,
        start,
        remove,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "injected Docker operations keep destructive reaper paths hermetic in tests"
)]
fn reap_idle_builders_with_domain_and_volume_gate(
    run_root: &Path,
    registry_root: Option<&Path>,
    domain_token: Option<&str>,
    now: SystemTime,
    list_builders: impl FnOnce() -> Result<Vec<String>>,
    list_present_containers: impl FnOnce() -> Result<BTreeSet<String>>,
    mut check_volume_gate: impl FnMut(&str) -> Result<()>,
    mut inspect_exit: impl FnMut(&str) -> Result<crate::docker::client::ExitInfo>,
    mut stop: impl FnMut(&str) -> Result<bool>,
    mut start: impl FnMut(&str) -> Result<bool>,
    mut remove: impl FnMut(&str) -> Result<()>,
) -> HorizonReport {
    let mut report = HorizonReport::default();
    let mut builders = match list_builders() {
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
    if let Some(registry_root) = registry_root {
        builders.extend(reconcile_claims_without_owner(
            run_root,
            registry_root,
            domain_token,
            &present,
            &mut report,
        ));
        builders.sort();
        builders.dedup();
    }
    let registered_builders: BTreeSet<String> = builders.iter().cloned().collect();
    for builder in builders.into_iter().filter(|builder| {
        domain_token.map_or_else(
            || is_current_domained_persistent_builder(builder),
            |token| is_current_domain_builder_name(builder, token),
        )
    }) {
        if let Err(error) = check_volume_gate(&builder) {
            report.failures.push(format!(
                "keep BuildKit builder {builder}: Engine-volume create fence blocks reaping: {error:#}"
            ));
            continue;
        }
        let path = claims_file(run_root, &builder);
        let initial_claims = match read_claims_for_reaping(&path, &builder, registry_root) {
            Ok(Some(claims)) => claims,
            Ok(None) => {
                report.failures.push(format!(
                    "leave BuildKit builder {builder} untouched: Velnor ownership record is absent or mismatched"
                ));
                continue;
            }
            Err(error) => {
                log_unreadable_ownership(&builder, &error.path, &error.source);
                report
                    .failures
                    .push(format!("read ownership for {builder}: {:#}", error.source));
                report
                    .unreadable_claims
                    .push(error.path.display().to_string());
                continue;
            }
        };
        if let Some(registry_root) = registry_root
            && is_current_domained_persistent_builder(&builder)
        {
            match read_owner_record(registry_root, &builder) {
                Ok(Some(owner)) if owner.phase == BuilderOwnerPhase::Deleting => {
                    if !initial_claims.holders.is_empty() {
                        report.failures.push(format!(
                            "keep Deleting BuildKit builder {builder}: its claims are not empty"
                        ));
                        continue;
                    }
                    match delete_registered_builder(
                        run_root,
                        Some(registry_root),
                        &builder,
                        &mut remove,
                    ) {
                        Ok(true) => report.deleted.push(builder.clone()),
                        Ok(false) => {}
                        Err(error) => report.failures.push(format!(
                            "retry deletion of BuildKit builder {builder}: {error:#}"
                        )),
                    }
                    continue;
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    report.failures.push(format!(
                        "read owner record for BuildKit builder {builder}: {error:#}"
                    ));
                    continue;
                }
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
            let mut claims = match read_claims_for_reaping(&path, &builder, registry_root) {
                Ok(Some(claims)) => claims,
                Ok(None) => {
                    report.failures.push(format!(
                        "leave BuildKit builder {builder} untouched: Velnor ownership record changed before cleanup"
                    ));
                    continue;
                }
                Err(error) => {
                    log_unreadable_ownership(&builder, &error.path, &error.source);
                    report
                        .failures
                        .push(format!("read claims for {builder}: {:#}", error.source));
                    report
                        .unreadable_claims
                        .push(error.path.display().to_string());
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
        let daemon = daemon_container_name(&builder);
        let exit = match inspect_exit(&daemon) {
            Ok(exit) => exit,
            Err(error) if crate::docker::client::is_not_found(&error) => {
                // Daemon gone but the builder registration lingers (a
                // `buildx rm --keep-state` past, or a crashed delete):
                // recheck, then remove the registration, any orphaned
                // volume, and the claim file.
                match holders_remain(&path, &builder, registry_root) {
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
        if exit.status == Some(crate::docker::client::ContainerState::Created) {
            // Buildx can leave a container in Created if setup crashes before
            // its config transfer/start sequence completes. Created is never
            // proof that the daemon is safe to start. With the exclusive
            // lifecycle coordinator, valid empty claims, and exact removal
            // re-attestation, delete this disposable cache and let the next
            // setup create it from scratch.
            if registry_root.is_none() {
                report.failures.push(format!(
                    "keep Created BuildKit builder {builder}: durable owner inventory is unavailable"
                ));
                continue;
            }
            match holders_remain(&path, &builder, registry_root) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    report.failures.push(format!(
                        "relock claims for Created builder {builder}: {error:#}"
                    ));
                    continue;
                }
            }
            match delete_registered_builder(run_root, registry_root, &builder, &mut remove) {
                Ok(true) => report.deleted.push(builder.clone()),
                Ok(false) => {}
                Err(error) => report.failures.push(format!(
                    "delete unstarted BuildKit builder {builder}: {error:#}"
                )),
            }
            continue;
        }
        if exit.status.is_some_and(|state| !state.safe_to_reclaim()) {
            // Running with no holders: a release-time stop that failed, or a
            // daemon started outside any claim. Stopping is safe — the next
            // build restarts it — but deleting is not considered; a setup
            // that raced the stop gets the daemon restarted.
            let stop_started = Instant::now();
            let stop_result = stop(&builder);
            match &stop_result {
                Ok(true) => report.stopped.push(builder.clone()),
                Ok(false) => {}
                Err(error) => {
                    report
                        .failures
                        .push(format!("stop builder {builder}: {error:#}"));
                }
            }
            tracing::debug!(
                target: "velnor.buildkit",
                builder,
                stop_ms = stop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                stop_uncertain = stop_result.is_err(),
                "horizon stop ran outside the claim lock"
            );
            let raced = match holders_remain(&path, &builder, registry_root) {
                Ok(true) => {
                    tracing::warn!(
                        target: "velnor.buildkit",
                        builder,
                        "holders arrived during horizon stop; restarting daemon"
                    );
                    if let Err(error) = require_buildkit_restart(start(&builder)) {
                        report
                            .failures
                            .push(format!("restart builder {builder}: {error:#}"));
                        match inspect_exit(&daemon) {
                            Ok(recovered)
                                if recovered
                                    .status
                                    .is_some_and(|state| !state.safe_to_reclaim()) =>
                            {
                                tracing::warn!(
                                    target: "velnor.buildkit",
                                    builder,
                                    "restart returned an error, but inspect confirms the daemon is running"
                                );
                            }
                            Ok(recovered) => report.failures.push(format!(
                                "cannot prove BuildKit daemon {builder} running after restart error: {:?}",
                                recovered.status
                            )),
                            Err(inspect_error) => report.failures.push(format!(
                                "inspect BuildKit daemon {builder} after restart error: {inspect_error:#}"
                            )),
                        }
                    }
                    true
                }
                Ok(false) => false,
                Err(error) => {
                    report
                        .failures
                        .push(format!("relock claims for {builder}: {error:#}"));
                    // A lock error cannot prove there are no new holders.
                    // Start even when stop returned false; the Engine may
                    // have changed state between its response and this read.
                    if let Err(start_error) = require_buildkit_restart(start(&builder)) {
                        report
                            .failures
                            .push(format!("recover builder {builder}: {start_error:#}"));
                        match inspect_exit(&daemon) {
                            Ok(recovered)
                                if recovered
                                    .status
                                    .is_some_and(|state| !state.safe_to_reclaim()) =>
                            {
                                tracing::warn!(
                                    target: "velnor.buildkit",
                                    builder,
                                    "recovery returned an error, but inspect confirms the daemon is running"
                                );
                            }
                            Ok(recovered) => report.failures.push(format!(
                                "cannot prove BuildKit daemon {builder} running after claim recheck failure: {:?}",
                                recovered.status
                            )),
                            Err(inspect_error) => report.failures.push(format!(
                                "inspect BuildKit daemon {builder} after claim recheck failure: {inspect_error:#}"
                            )),
                        }
                    }
                    true
                }
            };
            if stop_result.is_err() && !raced {
                // A timeout/error can follow a successful stop. Inspect the
                // result before reporting it and before any later deletion.
                match inspect_exit(&daemon) {
                    Ok(recovered)
                        if recovered.status.is_some_and(
                            crate::docker::client::ContainerState::safe_to_reclaim,
                        ) =>
                    {
                        report.stopped.push(builder.clone());
                    }
                    Ok(recovered) => report.failures.push(format!(
                        "BuildKit daemon {builder} remains active after ambiguous stop: {:?}",
                        recovered.status
                    )),
                    Err(inspect_error) => report.failures.push(format!(
                        "inspect BuildKit daemon {builder} after ambiguous stop: {inspect_error:#}"
                    )),
                }
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
                match holders_remain(&path, &builder, registry_root) {
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
            registry_root,
            domain_token,
            &registered_builders,
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

    fn legacy_readiness_fixture(
        domain: &PersistentBuildKitDomain,
        builder: &str,
        container_id: &str,
        config_fingerprint: &str,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": BUILDER_READINESS_LEGACY_VERSION,
            "builder": builder,
            "domain_token": domain.token.clone(),
            "state_volume": daemon_state_volume(builder),
            "container_id": container_id,
            "config_fingerprint": config_fingerprint,
        }))
        .unwrap()
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
        assert!(crate::storage::StorageLayout::resolve().is_none());
    }

    fn test_builder() -> String {
        persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r"))
    }

    #[test]
    fn configured_builder_fingerprint_matches_buildx_normalized_archive_payload() {
        let source = "[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]";
        assert_eq!(
            persistent_buildkit_config_fingerprint(Some(source)).unwrap(),
            "sha256:333c40f4fee6f473bee299aed751bb40967b9e8315af90378a5de0b5dc69a76b"
        );
        assert_eq!(
            persistent_buildkit_config_fingerprint(None).unwrap(),
            "no-config-v1"
        );
    }

    #[test]
    fn legacy_marker_quarantine_records_sha256_for_exact_bytes() {
        let root = temp_root("legacy-marker-quarantine-sha256");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("repo-key-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let volume = daemon_state_volume(&builder);
        let marker_bytes = b"legacy runtime marker bytes";
        quarantine_legacy_pending_buildkit_create(&domain, &volume, marker_bytes).unwrap();

        let quarantine_path = legacy_create_quarantine_file(&domain, &volume);
        let quarantine: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&quarantine_path).unwrap()).unwrap();
        let expected_sha256 = Sha256::digest(marker_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(quarantine["version"], 2);
        assert_eq!(quarantine["legacy_marker_sha256"], expected_sha256);
        assert!(legacy_pending_buildkit_create_is_quarantined(&domain, &volume).unwrap());
        let mut stale_v1 = quarantine;
        stale_v1["version"] = serde_json::Value::from(1);
        stale_v1["legacy_marker_sha256"] =
            serde_json::Value::String(blake3::hash(marker_bytes).to_hex().to_string());
        std::fs::write(&quarantine_path, serde_json::to_vec(&stale_v1).unwrap()).unwrap();
        assert!(legacy_pending_buildkit_create_is_quarantined(&domain, &volume).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Test helper: drop every hold on `builder`.
    fn abandon_claims(run_root: &Path, builder: &str) {
        let path = claims_file(run_root, builder);
        let _lock = lock_claims(builder, &path).unwrap();
        let mut claims = read_claims(&path, builder).unwrap();
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
        assert!(default.starts_with(CURRENT_PERSISTENT_BUILDER_PREFIX));
        assert_eq!(
            persistent_builder_domain_token(&default),
            Some(TEST_BUILDKIT_DOMAIN_TOKEN)
        );
        assert!(persistent_builder_name(
            "mybuilder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world")
        )
        .starts_with(&default));

        // Fork-PR jobs never share with trusted jobs; `None` is distinct from
        // any real repository after tagged full-input hashing.
        let untrusted = persistent_builder_name(
            "velnor-builder",
            "untrusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world"),
        );
        assert_ne!(default, untrusted);
        let case_distinct_scope = persistent_builder_name(
            "velnor-builder",
            "Trusted",
            TRUST_TIER_BRANCH,
            Some("octocat/hello-world"),
        );
        assert_ne!(default, case_distinct_scope);
        assert_eq!(
            case_distinct_scope,
            case_distinct_scope.to_ascii_lowercase()
        );
        let no_repo = persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, None);
        let named_no_repo = persistent_builder_name(
            "velnor-builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("no/repo"),
        );
        assert_ne!(no_repo, named_no_repo);
        assert_ne!(
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o/r")),
            persistent_builder_name("velnor-builder", "trusted", TRUST_TIER_BRANCH, Some("o_r")),
        );
        assert!(persistent_builder_domain_token(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r"
        )
        .is_none());

        // Hostile input is sanitized, and all generated resource IDs stay
        // within Docker's name length budget.
        let hostile = persistent_builder_name("../../X", "TRUSTED", TRUST_TIER_BRANCH, Some("O/R"));
        assert!(hostile.is_ascii());
        let long = persistent_builder_name(
            &format!("{}A", "x".repeat(200)),
            "trusted",
            TRUST_TIER_BRANCH,
            Some(&format!("{}/{}A", "owner".repeat(100), "repo".repeat(100))),
        );
        let long_other = persistent_builder_name(
            &format!("{}B", "x".repeat(200)),
            "trusted",
            TRUST_TIER_BRANCH,
            Some(&format!("{}/{}B", "owner".repeat(100), "repo".repeat(100))),
        );
        assert_ne!(long, long_other);
        let component = bounded_builder_segment("r", Some("same-long-prefix-a"), 48);
        assert_eq!(
            component.rsplit_once("-r").map(|(_, digest)| digest.len()),
            Some(32),
            "name-component digest carries 128 bits"
        );
        assert!(long.len() <= 240);
        assert!(daemon_state_volume(&long).len() <= 255);
    }

    #[test]
    fn persistent_domain_partitions_engine_and_storage_but_reuses_slots() {
        let root = temp_root("domain-partition");
        let same =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let same_again =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let other_storage =
            PersistentBuildKitDomain::from_identities(&root, "storage-b", "engine-a").unwrap();
        let other_engine =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-b").unwrap();
        assert_eq!(same.token, same_again.token);
        assert_eq!(same.root, same_again.root);
        assert_ne!(same.token, other_storage.token);
        assert_ne!(same.token, other_engine.token);
        assert_eq!(
            persistent_builder_name_for_domain(
                &same.token,
                "builder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("org/repo"),
            ),
            persistent_builder_name_for_domain(
                &same_again.token,
                "builder",
                "trusted",
                TRUST_TIER_BRANCH,
                Some("org/repo"),
            ),
            "same Engine/storage domain reuses across runner slots"
        );
        assert!(PersistentBuildKitDomain::from_identities(&root, " ", "engine-a").is_err());
        assert!(PersistentBuildKitDomain::from_identities(&root, "storage-a", "").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_domain_claim_uses_initialized_owner_registry() {
        let root = temp_root("first-domain-claim-owner-registry");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        assert!(registry_root.is_dir());
        assert!(registered_domain_builders(&registry_root, &domain.token)
            .unwrap()
            .is_empty());

        claim_domain_builder(&domain, &builder, "slot-1", "first-job").unwrap();

        assert_eq!(
            registered_domain_builders(&registry_root, &domain.token).unwrap(),
            [builder.clone()]
        );
        assert_eq!(
            read_claims(&claims_file(&domain.root, &builder), &builder)
                .unwrap()
                .holders
                .len(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_claims_publish_before_owner_and_owner_failure_leaves_recoverable_claims_only() {
        let root = temp_root("claims-before-owner-crash-cut");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let path = claims_file(&domain.root, &builder);
        let claims = BuilderClaims {
            holders: BTreeMap::from([(
                "job-container".to_owned(),
                BuilderHolder {
                    container: "job-container".to_owned(),
                    slot: "slot-1".to_owned(),
                    claimed_unix: unix_now(),
                },
            )]),
            builder: builder.clone(),
        };
        let error = publish_claims_before_owner(&path, &claims, || {
            assert_eq!(read_claims(&path, &builder)?.holders.len(), 1);
            Err(anyhow::anyhow!("simulated owner write failure"))
        })
        .unwrap_err();

        assert!(error.to_string().contains("simulated owner write failure"));
        assert_eq!(read_claims(&path, &builder).unwrap().holders.len(), 1);
        assert!(
            read_owner_record(&owner_registry_root(&domain.root), &builder)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_tombstone_and_empty_claims_survive_failed_removal_then_retry() {
        let root = temp_root("deleting-tombstone-retry");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        claim_domain_builder(&domain, &builder, "slot-1", "job-container").unwrap();
        let path = claims_file(&domain.root, &builder);
        let readiness_path = builder_readiness_file(&domain, &builder);
        write_atomic_document(&readiness_path, b"stale readiness proof").unwrap();
        let lifecycle_lock = lock_builder_lifecycle(&domain.root, &builder).unwrap();
        drop(lifecycle_lock);
        let lifecycle_lock_path = domain.root.join(BUILDER_LIFECYCLE_LOCKS_DIR).join(format!(
            "{}.lock",
            blake3::hash(builder.as_bytes()).to_hex()
        ));
        let mut claims = read_claims(&path, &builder).unwrap();
        claims.holders.clear();
        write_claims(&path, &claims).unwrap();

        let failed =
            remove_builder_and_claims_with(&domain.root, Some(&registry_root), &builder, |name| {
                assert_eq!(name, builder);
                assert_eq!(
                    read_owner_record(&registry_root, &builder)?.unwrap().phase,
                    BuilderOwnerPhase::Deleting
                );
                assert!(read_claims(&path, &builder)?.holders.is_empty());
                Err(anyhow::anyhow!(
                    "simulated crash after partial daemon removal"
                ))
            });
        assert!(failed.is_err());
        assert_eq!(
            read_owner_record(&registry_root, &builder)
                .unwrap()
                .unwrap()
                .phase,
            BuilderOwnerPhase::Deleting
        );
        assert!(read_claims(&path, &builder).unwrap().holders.is_empty());

        assert!(remove_builder_and_claims_with(
            &domain.root,
            Some(&registry_root),
            &builder,
            |_| Ok(())
        )
        .unwrap());
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        assert!(read_claims(&path, &builder).is_err());
        assert!(!readiness_path.exists());
        assert!(!lifecycle_lock_path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_owner_rejects_new_claims_without_changing_the_claim_file() {
        let root = temp_root("deleting-owner-blocks-claim");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "job-container").unwrap();
        let registry_root = owner_registry_root(&domain.root);
        let claims_path = claims_file(&domain.root, &builder);
        let before = std::fs::read(&claims_path).unwrap();
        mark_owner_record_deleting(&registry_root, &builder).unwrap();

        assert!(claim_domain_builder(&domain, &builder, "slot-2", "new-job").is_err());
        assert_eq!(std::fs::read(&claims_path).unwrap(), before);
        assert_eq!(
            read_owner_record(&registry_root, &builder)
                .unwrap()
                .unwrap()
                .phase,
            BuilderOwnerPhase::Deleting
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn horizon_retries_deleting_owner_before_inspection_or_stop() {
        let root = temp_root("horizon-deleting-owner-retry");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        claim_domain_builder(&domain, &builder, "slot-1", "job-container").unwrap();
        let claims_path = claims_file(&domain.root, &builder);
        let mut claims = read_claims(&claims_path, &builder).unwrap();
        claims.holders.clear();
        write_claims(&claims_path, &claims).unwrap();
        mark_owner_record_deleting(&registry_root, &builder).unwrap();

        let report = reap_idle_builders_with_registry(
            &domain.root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| panic!("Deleting owner retry does not inspect or stop first"),
            |_| panic!("Deleting owner retry does not stop first"),
            |_| panic!("Deleting owner retry does not restart"),
            |name| {
                assert_eq!(name, builder);
                assert_eq!(
                    read_owner_record(&registry_root, &builder)?.unwrap().phase,
                    BuilderOwnerPhase::Deleting
                );
                assert!(read_claims(&claims_path, &builder)?.holders.is_empty());
                Ok(())
            },
        );

        assert_eq!(report.deleted, [builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        assert!(read_claims(&claims_path, &builder).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn horizon_deletes_unstarted_created_builder_without_starting_it() {
        use crate::docker::client::{ContainerState, ExitInfo};

        let root = temp_root("horizon-created-unstarted-recovery");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        claim_domain_builder(&domain, &builder, "slot-old", "old-job").unwrap();
        let claims_path = claims_file(&domain.root, &builder);
        let mut claims = read_claims(&claims_path, &builder).unwrap();
        claims.holders.clear();
        write_claims(&claims_path, &claims).unwrap();

        let report = reap_idle_builders_with_registry(
            &domain.root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(ExitInfo {
                    status: Some(ContainerState::Created),
                    finished: None,
                })
            },
            |_| panic!("uninitialized Created daemon must never be started/stopped"),
            |_| panic!("uninitialized Created daemon must never be started"),
            |name| {
                assert_eq!(name, builder);
                assert_eq!(
                    read_owner_record(&registry_root, &builder)?.unwrap().phase,
                    BuilderOwnerPhase::Deleting
                );
                assert!(read_claims(&claims_path, &builder)?.holders.is_empty());
                Ok(())
            },
        );

        assert_eq!(report.deleted, [builder.clone()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        assert!(read_claims(&claims_path, &builder).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_after_owner_unlink_leaves_claims_only_and_next_setup_recovers() {
        let root = temp_root("owner-before-claims-unlink-crash-cut");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        claim_domain_builder(&domain, &builder, "slot-old", "old-job").unwrap();
        let path = claims_file(&domain.root, &builder);
        let mut claims = read_claims(&path, &builder).unwrap();
        claims.holders.clear();
        write_claims(&path, &claims).unwrap();

        let error = unlink_owner_before_claims(Some(&registry_root), &builder, &path, || {
            Err(anyhow::anyhow!("simulated crash after owner unlink"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("simulated crash"));
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        assert!(read_claims(&path, &builder).unwrap().holders.is_empty());

        claim_domain_builder(&domain, &builder, "slot-new", "new-job").unwrap();
        assert_eq!(read_claims(&path, &builder).unwrap().holders.len(), 1);
        assert_eq!(
            read_owner_record(&registry_root, &builder)
                .unwrap()
                .unwrap()
                .phase,
            BuilderOwnerPhase::Active
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reaper_recovers_ownerless_claims_after_setup_cut_and_cleans_empty_delete_cut() {
        let root = temp_root("ownerless-claims-recovery");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        let path = claims_file(&domain.root, &builder);
        let claims = BuilderClaims {
            builder: builder.clone(),
            holders: BTreeMap::from([(
                "job-container".to_owned(),
                BuilderHolder {
                    container: "job-container".to_owned(),
                    slot: "slot-1".to_owned(),
                    claimed_unix: unix_now(),
                },
            )]),
        };
        write_claims(&path, &claims).unwrap();

        let present = BTreeSet::from(["job-container".to_owned()]);
        let mut report = HorizonReport::default();
        assert_eq!(
            reconcile_claims_without_owner(
                &domain.root,
                &registry_root,
                Some(&domain.token),
                &present,
                &mut report,
            ),
            [builder.clone()]
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            read_owner_record(&registry_root, &builder)
                .unwrap()
                .unwrap()
                .phase,
            BuilderOwnerPhase::Active
        );

        // Simulate the deletion crash cut after the Deleting owner was
        // unlinked but before the empty claims file was removed.
        write_claims(
            &path,
            &BuilderClaims {
                builder: builder.clone(),
                holders: BTreeMap::new(),
            },
        )
        .unwrap();
        remove_owner_record(&registry_root, &builder).unwrap();
        let mut report = HorizonReport::default();
        assert!(reconcile_claims_without_owner(
            &domain.root,
            &registry_root,
            Some(&domain.token),
            &BTreeSet::new(),
            &mut report,
        )
        .is_empty());
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(!path.exists());
        assert!(read_owner_record(&registry_root, &builder)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_domain_reaper_leaves_another_domains_claim_and_owner_bytes_untouched() {
        let root = temp_root("domain-owner-reaper-isolation");
        let domain_a =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let domain_b =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-b").unwrap();
        let builder_a = persistent_builder_name_for_domain(
            &domain_a.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let builder_b = persistent_builder_name_for_domain(
            &domain_b.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain_a, &builder_a, "slot-1", "velnor-job-a").unwrap();
        claim_domain_builder(&domain_b, &builder_b, "slot-2", "velnor-job-b").unwrap();
        let claim_b = claims_file(&domain_b.root, &builder_b);
        let owner_b = owner_registry_file(&owner_registry_root(&domain_b.root), &builder_b);
        let claim_b_before = std::fs::read(&claim_b).unwrap();
        let owner_b_before = std::fs::read(&owner_b).unwrap();
        assert_eq!(
            registered_domain_builders(&owner_registry_root(&domain_a.root), &domain_a.token)
                .unwrap(),
            [builder_a.clone()],
            "domain owner registry is the candidate source, not host Buildx state"
        );

        let report = reap_idle_builders_with_domain(
            &domain_a.root,
            Some(&owner_registry_root(&domain_a.root)),
            Some(&domain_a.token),
            SystemTime::now(),
            || Ok(vec![builder_a.clone(), builder_b.clone()]),
            || Ok(BTreeSet::from(["velnor-job-a".to_string()])),
            |_| panic!("active same-domain claim prevents inspect"),
            |_| panic!("active same-domain claim prevents stop"),
            |_| panic!("active same-domain claim prevents restart"),
            |_| panic!("other-domain builder must not be removed"),
        );

        assert!(report.deleted.is_empty());
        assert!(report.stopped.is_empty());
        assert!(report.failures.is_empty());
        assert_eq!(std::fs::read(&claim_b).unwrap(), claim_b_before);
        assert_eq!(std::fs::read(&owner_b).unwrap(), owner_b_before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copied_storage_roots_with_the_same_uuid_and_engine_are_isolated() {
        let root = temp_root("copied-domain-roots");
        let first_root = root.join("first");
        let copied_root = root.join("copy");
        std::fs::create_dir_all(&first_root).unwrap();
        std::fs::create_dir_all(&copied_root).unwrap();
        let first = PersistentBuildKitDomain::from_identities(
            &first_root,
            "copied-storage-uuid",
            "same-engine-id",
        )
        .unwrap();
        let copy = PersistentBuildKitDomain::from_identities(
            &copied_root,
            "copied-storage-uuid",
            "same-engine-id",
        )
        .unwrap();
        let first_builder = persistent_builder_name_for_domain(
            &first.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let copied_builder = persistent_builder_name_for_domain(
            &copy.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        assert_ne!(first.token, copy.token);
        assert_ne!(first_builder, copied_builder);
        claim_domain_builder(&first, &first_builder, "slot-1", "job-first").unwrap();
        abandon_claims(&first.root, &first_builder);
        claim_domain_builder(&copy, &copied_builder, "slot-2", "job-copy").unwrap();
        let copied_claim = claims_file(&copy.root, &copied_builder);
        let copied_owner = owner_registry_file(&owner_registry_root(&copy.root), &copied_builder);
        let claim_before = std::fs::read(&copied_claim).unwrap();
        let owner_before = std::fs::read(&copied_owner).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000_000);
        let old_finished = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let removed = std::cell::RefCell::new(Vec::new());

        let report = reap_idle_builders_with_domain(
            &first.root,
            Some(&owner_registry_root(&first.root)),
            Some(&first.token),
            now,
            || Ok(vec![first_builder.clone(), copied_builder.clone()]),
            || Ok(BTreeSet::new()),
            |daemon| {
                assert_eq!(daemon, daemon_container_name(&first_builder));
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: Some(old_finished),
                })
            },
            |_| panic!("stopped daemon needs no stop"),
            |_| panic!("copied-root claim must not reach restart"),
            |builder| {
                removed.borrow_mut().push(builder.to_string());
                Ok(())
            },
        );

        assert_eq!(removed.borrow().as_slice(), [first_builder.as_str()]);
        assert_eq!(report.deleted, [first_builder]);
        assert_eq!(std::fs::read(copied_claim).unwrap(), claim_before);
        assert_eq!(std::fs::read(copied_owner).unwrap(), owner_before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn horizon_reaper_skips_every_action_when_create_fence_gate_fails() {
        let root = temp_root("unresolved-create-reaper-gate");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-a", "job-a").unwrap();
        abandon_claims(&domain.root, &builder);
        let registry = owner_registry_root(&domain.root);
        let claim_path = claims_file(&domain.root, &builder);
        let owner_path = owner_registry_file(&registry, &builder);
        let claim_before = std::fs::read(&claim_path).unwrap();
        let owner_before = std::fs::read(&owner_path).unwrap();

        let report = reap_idle_builders_with_domain_and_volume_gate(
            &domain.root,
            Some(&registry),
            Some(&domain.token),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |candidate| {
                assert_eq!(candidate, builder);
                anyhow::bail!("persistent BuildKit create remains unresolved")
            },
            |_| panic!("fenced builder must not be inspected"),
            |_| panic!("fenced builder must not be stopped"),
            |_| panic!("fenced builder must not be restarted"),
            |_| panic!("fenced builder must not be removed"),
        );

        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("create fence blocks reaping")
        }));
        assert!(report.stopped.is_empty());
        assert!(report.deleted.is_empty());
        assert_eq!(std::fs::read(&claim_path).unwrap(), claim_before);
        assert_eq!(std::fs::read(&owner_path).unwrap(), owner_before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slot_roots_share_only_the_explicit_storage_domain_and_engine_identity() {
        let root = temp_root("slot-domain-identity");
        let identity_root = root.join("shared-durable-lib");
        std::fs::create_dir_all(&identity_root).unwrap();
        let slot_a = crate::storage::StorageLayout {
            cache_root: root.join("slot-a/cache"),
            lib_root: root.join("slot-a/lib"),
            run_root: root.join("slot-a/run"),
            log_root: root.join("slot-a/log"),
            mode: "test-slot-a",
        };
        let slot_b = crate::storage::StorageLayout {
            cache_root: root.join("slot-b/cache"),
            lib_root: root.join("slot-b/lib"),
            run_root: root.join("slot-b/run"),
            log_root: root.join("slot-b/log"),
            mode: "test-slot-b",
        };
        assert_ne!(slot_a.lib_root, slot_b.lib_root);
        assert_ne!(slot_a.run_root, slot_b.run_root);
        let identity_a = slot_a.buildkit_identity_root_for_override(Some(&identity_root));
        let identity_b = slot_b.buildkit_identity_root_for_override(Some(&identity_root));
        assert_eq!(identity_a, identity_b);

        let domain_a =
            PersistentBuildKitDomain::from_identities(&identity_a, "storage-uuid", "engine-id")
                .unwrap();
        let domain_b =
            PersistentBuildKitDomain::from_identities(&identity_b, "storage-uuid", "engine-id")
                .unwrap();
        let builder_a = persistent_builder_name_for_domain(
            &domain_a.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let builder_b = persistent_builder_name_for_domain(
            &domain_b.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        assert_eq!(domain_a.token, domain_b.token);
        assert_eq!(builder_a, builder_b);
        claim_domain_builder(&domain_a, &builder_a, "slot-a", "job-a").unwrap();
        claim_domain_builder(&domain_b, &builder_b, "slot-b", "job-b").unwrap();
        let holders = builder_holders(&domain_a.root, &builder_a, None).unwrap();
        assert_eq!(holders.len(), 2);
        assert_eq!(
            holders
                .iter()
                .map(|holder| holder.slot.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["slot-a", "slot-b"])
        );

        let other_engine =
            PersistentBuildKitDomain::from_identities(&identity_a, "storage-uuid", "other-engine")
                .unwrap();
        let other_builder = persistent_builder_name_for_domain(
            &other_engine.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        assert_ne!(domain_a.token, other_engine.token);
        assert_ne!(builder_a, other_builder);
        claim_domain_builder(&other_engine, &other_builder, "slot-a", "job-other-engine").unwrap();
        assert_eq!(
            builder_holders(&other_engine.root, &other_builder, None)
                .unwrap()
                .iter()
                .map(|holder| holder.container.as_str())
                .collect::<Vec<_>>(),
            ["job-other-engine"]
        );
        assert_eq!(
            builder_holders(&domain_a.root, &builder_a, None)
                .unwrap()
                .len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_claim_file_pins_orphan_owner_even_when_job_container_is_live() {
        let root = temp_root("missing-claim-live-job-pin");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "live-job-container").unwrap();
        let claim_path = claims_file(&domain.root, &builder);
        let owner_path = owner_registry_file(&owner_registry_root(&domain.root), &builder);
        let owner_before = std::fs::read(&owner_path).unwrap();
        std::fs::remove_file(&claim_path).unwrap();
        let mut report = HorizonReport::default();

        reconcile_orphan_owner_records(
            &owner_registry_root(&domain.root),
            Some(&domain.token),
            &BTreeSet::new(),
            &mut report,
        );

        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("without Docker absence proof")));
        assert!(!claim_path.exists());
        assert_eq!(std::fs::read(owner_path).unwrap(), owner_before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_claim_records_require_both_holders_and_builder_fields() {
        let builder =
            persistent_builder_name("builder", "trusted", TRUST_TIER_BRANCH, Some("org/repo"));
        assert!(parse_claims(
            Path::new("claims.json"),
            format!(
                r#"{{"builder":{}}}"#,
                serde_json::to_string(&builder).unwrap()
            )
            .as_bytes(),
        )
        .is_err());
        assert!(parse_claims(Path::new("claims.json"), br#"{"holders":{}}"#).is_err());
        assert_eq!(
            parse_claims(Path::new("claims.json"), br#"{"builder":"x","holders":{}}"#)
                .unwrap()
                .holders
                .len(),
            0
        );
    }

    #[test]
    fn malformed_persisted_claims_preserve_owner_and_prevent_reaping() {
        let root = temp_root("malformed-claims-pin-builder");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "live-job").unwrap();
        let claim_path = claims_file(&domain.root, &builder);
        let owner_path = owner_registry_file(&owner_registry_root(&domain.root), &builder);
        let malformed = format!(
            r#"{{"builder":{}}}"#,
            serde_json::to_string(&builder).unwrap()
        );
        std::fs::write(&claim_path, &malformed).unwrap();
        let owner_before = std::fs::read(&owner_path).unwrap();

        let report = reap_idle_builders_with_domain(
            &domain.root,
            Some(&owner_registry_root(&domain.root)),
            Some(&domain.token),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| panic!("malformed claims must prevent daemon inspection"),
            |_| panic!("malformed claims must prevent stop"),
            |_| panic!("malformed claims must prevent restart"),
            |_| panic!("malformed claims must prevent removal"),
        );

        assert!(!report.failures.is_empty());
        assert_eq!(std::fs::read(&claim_path).unwrap(), malformed.as_bytes());
        assert_eq!(std::fs::read(&owner_path).unwrap(), owner_before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_claims_fail_closed_and_preserve_owner_state() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-claims-pin-builder");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "live-job").unwrap();
        let claim_path = claims_file(&domain.root, &builder);
        let owner_path = owner_registry_file(&owner_registry_root(&domain.root), &builder);
        let owner_before = std::fs::read(&owner_path).unwrap();
        let target = root.join("matching-empty-claims.json");
        let target_bytes = serde_json::to_vec(&BuilderClaims {
            holders: BTreeMap::new(),
            builder: builder.clone(),
        })
        .unwrap();
        std::fs::write(&target, &target_bytes).unwrap();
        std::fs::remove_file(&claim_path).unwrap();
        symlink(&target, &claim_path).unwrap();

        assert!(read_claims(&claim_path, &builder).is_err());
        assert!(read_registered_claims(&claim_path, &builder).is_err());

        let report = reap_idle_builders_with_domain(
            &domain.root,
            Some(&owner_registry_root(&domain.root)),
            Some(&domain.token),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| panic!("symlinked claims must prevent daemon inspection"),
            |_| panic!("symlinked claims must prevent stop"),
            |_| panic!("symlinked claims must prevent restart"),
            |_| panic!("symlinked claims must prevent removal"),
        );

        assert!(!report.failures.is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
        assert_eq!(std::fs::read(&owner_path).unwrap(), owner_before);
        assert!(std::fs::symlink_metadata(&claim_path)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_owner_records_fail_closed_for_lookup_and_reaping() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-owner-record-pin-builder");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "live-job").unwrap();
        let registry_root = owner_registry_root(&domain.root);
        let owner_path = owner_registry_file(&registry_root, &builder);
        let target = root.join("matching-owner-record.json");
        let target_bytes = std::fs::read(&owner_path).unwrap();
        std::fs::write(&target, &target_bytes).unwrap();
        std::fs::remove_file(&owner_path).unwrap();
        symlink(&target, &owner_path).unwrap();

        assert!(read_owner_record(&registry_root, &builder).is_err());
        assert!(registered_domain_builders(&registry_root, &domain.token).is_err());
        let mut report = HorizonReport::default();
        reconcile_orphan_owner_records(
            &registry_root,
            Some(&domain.token),
            &BTreeSet::new(),
            &mut report,
        );
        assert!(!report.failures.is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
        assert!(std::fs::symlink_metadata(&owner_path)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn claims_and_owner_metadata_reads_reject_fifo_and_oversized_files() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let root = temp_root("fifo-and-large-buildkit-control-files");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let claims_path = claims_file(&domain.root, &builder);
        let claims_parent = claims_path.parent().unwrap();
        std::fs::create_dir_all(claims_parent).unwrap();
        let fifo = CString::new(claims_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string and mkfifo only
        // creates the named FIFO; the no-follow reader must reject its type.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_claims(&claims_path, &builder).is_err());
        std::fs::remove_file(&claims_path).unwrap();

        let oversized = vec![b'x'; (MAX_BUILDKIT_CONTROL_FILE_BYTES + 1) as usize];
        std::fs::write(&claims_path, &oversized).unwrap();
        assert!(read_claims(&claims_path, &builder).is_err());
        assert!(read_registered_claims(&claims_path, &builder).is_err());
        let owner_path = owner_registry_file(&owner_registry_root(&domain.root), &builder);
        std::fs::write(&owner_path, &oversized).unwrap();
        assert!(read_owner_record(&owner_registry_root(&domain.root), &builder).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_claim_builder_cannot_release_or_overwrite_claims() {
        let root = temp_root("mismatched-claim-builder");
        let run_root = root.join("run");
        let builder = test_builder();
        let other_builder = persistent_builder_name(
            "another-builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let path = claims_file(&run_root, &builder);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let claims = BuilderClaims {
            holders: BTreeMap::from([(
                "velnor-job-live".to_string(),
                BuilderHolder {
                    container: "velnor-job-live".to_string(),
                    slot: "slot-1".to_string(),
                    claimed_unix: 1,
                },
            )]),
            builder: other_builder,
        };
        write_claims(&path, &claims).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert!(read_claims(&path, &builder).is_err());
        assert!(claim_builder(&run_root, &builder, "slot-2", "velnor-job-new").is_err());
        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-live",
            || panic!("mismatched claims must prevent stop"),
            || panic!("mismatched claims must prevent restart"),
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
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builder_removal_never_deletes_when_lock_or_inspection_is_uncertain() {
        use std::cell::Cell;

        let root = temp_root("remove-builder-fail-closed");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let inspect_called = Cell::new(false);
        let delete_called = Cell::new(false);
        let lock_error = remove_builder_with(
            &domain,
            &builder,
            |_, _, _| -> Result<()> { Err(anyhow::anyhow!("lock uncertainty")) },
            |_, _| {
                inspect_called.set(true);
                Ok(true)
            },
            |_, _, _, _| panic!("must not inspect without the volume lock"),
            |_, _| {
                delete_called.set(true);
                Ok(())
            },
            |_| {
                delete_called.set(true);
                Ok(())
            },
        );
        assert!(lock_error.is_err());
        assert!(!inspect_called.get());
        assert!(!delete_called.get());

        let delete_called = Cell::new(false);
        let volume_error = remove_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| Err(anyhow::anyhow!("volume inspect uncertainty")),
            |_, _, _, _| panic!("must not inspect the container after volume uncertainty"),
            |_, _| {
                delete_called.set(true);
                Ok(())
            },
            |_| {
                delete_called.set(true);
                Ok(())
            },
        );
        assert!(volume_error.is_err());
        assert!(!delete_called.get());

        let delete_called = Cell::new(false);
        let container_error = remove_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| Ok(true),
            |_, _, _, _| Err(anyhow::anyhow!("container inspect uncertainty")),
            |_, _| {
                delete_called.set(true);
                Ok(())
            },
            |_| {
                delete_called.set(true);
                Ok(())
            },
        );
        assert!(container_error.is_err());
        assert!(!delete_called.get());

        let operation_called = Cell::new(false);
        let pressure_lock_error = with_attested_domain_builder_with(
            &domain,
            &builder,
            |_, _, _| -> Result<()> { Err(anyhow::anyhow!("lock uncertainty")) },
            |_, _| panic!("must not inspect without the volume lock"),
            |_, _, _, _| panic!("must not inspect the container without the lock"),
            |_, _, _, _, _| {
                operation_called.set(true);
                Ok(())
            },
        );
        assert!(pressure_lock_error.is_err());
        assert!(!operation_called.get());

        let pressure_inspect_error = with_attested_domain_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| Err(anyhow::anyhow!("volume inspect uncertainty")),
            |_, _, _, _| panic!("must not inspect container after volume uncertainty"),
            |_, _, _, _, _| {
                operation_called.set(true);
                Ok(())
            },
        );
        assert!(pressure_inspect_error.is_err());
        assert!(!operation_called.get());

        let pressure_container_error = with_attested_domain_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| Ok(true),
            |_, _, _, _| Err(anyhow::anyhow!("container inspect uncertainty")),
            |_, _, _, _, _| {
                operation_called.set(true);
                Ok(())
            },
        );
        assert!(pressure_container_error.is_err());
        assert!(!operation_called.get());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builder_removal_reattests_volume_before_name_based_delete() {
        use std::cell::Cell;

        let root = temp_root("remove-builder-volume-reattest");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let volume_inspections = Cell::new(0);
        let container_removed = Cell::new(false);
        let volume_removed = Cell::new(false);
        let result = remove_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| {
                let inspection = volume_inspections.get() + 1;
                volume_inspections.set(inspection);
                if inspection == 1 {
                    Ok(true)
                } else {
                    Err(anyhow::anyhow!("volume identity changed before deletion"))
                }
            },
            |_, _, _, _| Ok(Some("immutable-container-id".to_string())),
            |container_id, _| {
                assert_eq!(container_id, "immutable-container-id");
                container_removed.set(true);
                Ok(())
            },
            |_| {
                volume_removed.set(true);
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(volume_inspections.get(), 2);
        assert!(container_removed.get());
        assert!(!volume_removed.get());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builder_force_removal_revokes_readiness_before_mutation() {
        use std::cell::Cell;

        let root = temp_root("remove-builder-revokes-readiness");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let container_id = "immutable-container-id";
        let config_fingerprint = "no-config-v1";
        write_test_builder_readiness(&domain, &builder, container_id, config_fingerprint).unwrap();
        let container_inspections = Cell::new(0);
        let result = remove_builder_with(
            &domain,
            &builder,
            |_, _, _| Ok(()),
            |_, _| Ok(true),
            |_, _, _, _| {
                let inspection = container_inspections.get() + 1;
                container_inspections.set(inspection);
                if inspection == 1 {
                    Ok(Some(container_id.to_owned()))
                } else {
                    Ok(None)
                }
            },
            |id, _| {
                assert_eq!(id, container_id);
                let stopping = read_builder_readiness_state(&domain, &builder)
                    .unwrap()
                    .unwrap();
                assert_eq!(stopping.phase, BuilderReadinessPhase::Stopping);
                assert_eq!(stopping.epoch, 2);
                Ok(())
            },
            |_| {
                let stopped = read_builder_readiness_state(&domain, &builder)
                    .unwrap()
                    .unwrap();
                assert_eq!(stopped.phase, BuilderReadinessPhase::Stopped);
                assert_eq!(stopped.epoch, 2);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(container_inspections.get(), 3);
        let stopped = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(stopped.phase, BuilderReadinessPhase::Stopped);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buildctl_pressure_commands_use_attested_immutable_id() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            buildctl_prune_args(id),
            [
                "exec",
                id,
                "buildctl",
                "--addr",
                "unix:///run/buildkit/buildkitd.sock",
                "prune",
            ]
        );
        assert_eq!(
            buildctl_ready_args(id),
            [
                "exec",
                id,
                "buildctl",
                "--addr",
                "unix:///run/buildkit/buildkitd.sock",
                "debug",
                "workers",
            ]
        );
    }

    #[test]
    fn buildkit_readiness_retries_until_success_and_reports_last_error() {
        let attempts = std::cell::Cell::new(0);
        retry_until_buildkit_ready(Duration::from_secs(1), |probe_timeout| {
            assert!(probe_timeout <= Duration::from_secs(2));
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            if attempt < 3 {
                anyhow::bail!("probe {attempt} failed");
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(attempts.get(), 3);

        let attempts = std::cell::Cell::new(0);
        let error = retry_until_buildkit_ready(Duration::from_millis(5), |_| {
            attempts.set(attempts.get() + 1);
            anyhow::bail!("daemon is not ready")
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("daemon is not ready"));
        assert!(attempts.get() > 0);
    }

    #[test]
    fn conflicting_create_waits_for_created_container_and_only_restarts_initialized_daemon() {
        use crate::docker::client::{ContainerState, ExitInfo};

        let inspections = std::cell::Cell::new(0);
        let starts = std::cell::Cell::new(0);
        let waits = std::cell::Cell::new(0);
        let sleeps = std::cell::Cell::new(0);
        ensure_conflicting_container_ready_with(
            "immutable-container-id",
            Duration::from_secs(1),
            |_| sleeps.set(sleeps.get() + 1),
            |_, timeout| {
                assert!(timeout > Duration::ZERO);
                assert!(timeout <= Duration::from_secs(2));
                inspections.set(inspections.get() + 1);
                Ok(ExitInfo {
                    status: Some(if inspections.get() == 1 {
                        ContainerState::Created
                    } else {
                        ContainerState::Running
                    }),
                    finished: None,
                })
            },
            |_| {
                starts.set(starts.get() + 1);
                Ok(true)
            },
            |_| {
                waits.set(waits.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(inspections.get(), 2);
        assert_eq!(sleeps.get(), 1);
        assert_eq!(
            starts.get(),
            0,
            "do not start before winning create copies config"
        );
        assert_eq!(waits.get(), 1);

        let starts = std::cell::Cell::new(0);
        let waits = std::cell::Cell::new(0);
        let finished = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        ensure_conflicting_container_ready_with(
            "previously-started-container-id",
            Duration::from_secs(1),
            |_| panic!("stopped daemon does not wait for another creator"),
            |_, _| {
                Ok(ExitInfo {
                    status: Some(ContainerState::Exited),
                    finished: Some(finished),
                })
            },
            |_| {
                starts.set(starts.get() + 1);
                Ok(true)
            },
            |_| {
                waits.set(waits.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(starts.get(), 1);
        assert_eq!(
            waits.get(),
            1,
            "container start ack is not worker readiness"
        );

        let starts = std::cell::Cell::new(0);
        let error = ensure_conflicting_container_ready_with(
            "uninitialized-container-id",
            Duration::from_secs(1),
            |_| {},
            |_, _| {
                Ok(ExitInfo {
                    status: Some(ContainerState::Exited),
                    finished: None,
                })
            },
            |_| {
                starts.set(starts.get() + 1);
                Ok(true)
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot safely reuse"));
        assert_eq!(starts.get(), 0, "unknown initialization must fail closed");

        let starts = std::cell::Cell::new(0);
        let error = ensure_conflicting_container_ready_with(
            "stuck-created-container-id",
            Duration::from_millis(3),
            std::thread::sleep,
            |_, timeout| {
                assert!(timeout <= Duration::from_secs(2));
                Ok(ExitInfo {
                    status: Some(ContainerState::Created),
                    finished: None,
                })
            },
            |_| {
                starts.set(starts.get() + 1);
                Ok(true)
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("remained unstarted"));
        assert_eq!(starts.get(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn created_conflict_wait_releases_same_engine_volume_flock_for_winning_start() {
        use crate::docker::client::{ContainerState, ExitInfo};
        use std::collections::BTreeSet;

        let lock_namespace = temp_root("buildkit-created-conflict-lock");
        std::fs::create_dir_all(&lock_namespace).unwrap();
        let volume = "buildx_buildkit_builder0_state";
        let policy = crate::docker_lease::DockerLeasePolicy::new_with_volume_lock_root(
            "conflict-wait-test",
            Some(lock_namespace.clone()),
        )
        .unwrap();
        let inspections = std::cell::Cell::new(0);
        let starts = std::cell::Cell::new(0);
        let ready = std::cell::Cell::new(0);
        let report = ensure_conflicting_builder_ready_with_attestation(
            "immutable-container-id",
            || policy.lock_volume_names(&BTreeSet::from([volume.to_owned()])),
            || Ok(true),
            || Ok(Some("immutable-container-id".to_owned())),
            Duration::from_secs(1),
            |_| {
                // This is the same Engine/volume flock that the winner's
                // Docker POST /start must take before reaching dockerd.
                let acquired =
                    crate::docker_lease::try_lock_volume_name_at_for_test(&lock_namespace, volume)
                        .unwrap();
                assert!(
                    acquired.is_some(),
                    "start path was blocked by conflict wait"
                );
                drop(acquired);
            },
            |_, _| {
                inspections.set(inspections.get() + 1);
                Ok(ExitInfo {
                    status: Some(if inspections.get() == 1 {
                        ContainerState::Created
                    } else {
                        ContainerState::Running
                    }),
                    finished: None,
                })
            },
            |_| {
                starts.set(starts.get() + 1);
                Ok(false)
            },
            |_| {
                ready.set(ready.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(report);
        assert_eq!(inspections.get(), 2);
        assert_eq!(starts.get(), 0, "the creator's start request wins");
        assert_eq!(ready.get(), 1);
        std::fs::remove_dir_all(lock_namespace).unwrap();
    }

    #[test]
    fn device_bound_pressure_skips_unmatched_or_missing_mountpoints() {
        let docker_root = Path::new("/docker-root");
        let mountpoint = Path::new("/docker-root/volumes/builder");
        let calls = std::cell::Cell::new(0);
        let freed = prune_candidate_for_device_with(
            Some(mountpoint),
            docker_root,
            11,
            |mount, root, device| Ok(mount.starts_with(root) && device == 11),
            || {
                calls.set(calls.get() + 1);
                Ok(700)
            },
        )
        .unwrap();
        assert_eq!(freed, 700);
        assert_eq!(calls.get(), 1);

        let other_device = prune_candidate_for_device_with(
            Some(mountpoint),
            docker_root,
            12,
            |mount, root, device| Ok(mount.starts_with(root) && device == 11),
            || {
                calls.set(calls.get() + 1);
                Ok(1000)
            },
        )
        .unwrap();
        assert_eq!(other_device, 0);
        let missing_mountpoint = prune_candidate_for_device_with(
            None,
            docker_root,
            11,
            |_, _, _| panic!("missing mountpoint must not be inspected"),
            || {
                calls.set(calls.get() + 1);
                Ok(1000)
            },
        )
        .unwrap();
        assert_eq!(missing_mountpoint, 0);
        assert_eq!(calls.get(), 1, "unproven candidates must not prune");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_prune_revalidates_pin_and_predicate_around_destructive_call() {
        use std::os::unix::fs::symlink;

        let root = temp_root("pressure-pin-revalidation");
        let pressure_path = root.join("pressure");
        let replacement = root.join("replacement");
        std::fs::create_dir_all(&pressure_path).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        let pin = crate::host_capacity::HostCapacityPin::open(&pressure_path).unwrap();
        let volume_uuid = pin
            .probe()
            .unwrap()
            .volume_fingerprint
            .expect("test filesystem must expose a stable UUID");
        let prune_calls = std::cell::Cell::new(0);

        let recovered = run_pressure_prune_with_pin(&pin, &volume_uuid, &|_| false, || {
            prune_calls.set(prune_calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(recovered, None);
        assert_eq!(prune_calls.get(), 0, "recovered pressure still pruned");

        let displaced = root.join("pressure-pinned");
        let error = run_pressure_prune_with_pin(&pin, &volume_uuid, &|_| true, || {
            prune_calls.set(prune_calls.get() + 1);
            std::fs::rename(&pressure_path, &displaced).unwrap();
            symlink(&replacement, &pressure_path).unwrap();
            Ok(())
        })
        .expect_err("replacement during the prune must fail post-call validation");
        assert!(format!("{error:#}").contains("pressure filesystem"));
        assert_eq!(prune_calls.get(), 1);

        let docker_root = root.join("docker-root");
        std::fs::create_dir(&docker_root).unwrap();
        let docker_root_pin = crate::host_capacity::HostCapacityPin::open(&docker_root).unwrap();
        let displaced_docker_root = root.join("docker-root-pinned");
        std::fs::rename(&docker_root, &displaced_docker_root).unwrap();
        std::fs::create_dir(&docker_root).unwrap();
        assert!(
            docker_root_pin.revalidate().is_err(),
            "same-device Docker-root replacement passed the retained pin"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_prune_skips_a_different_docker_process_root() {
        let host_root = temp_root("pressure-host-root");
        let daemon_root = temp_root("pressure-daemon-root");
        assert!(same_filesystem_object(&host_root, &host_root).unwrap());
        assert!(!same_filesystem_object(&host_root, &daemon_root).unwrap());

        let mountpoint = host_root.join("volumes/builder");
        let prunes = std::cell::Cell::new(0);
        let freed = prune_candidate_for_device_with(
            Some(&mountpoint),
            &host_root,
            1,
            |_, _, _| same_filesystem_object(&host_root, &daemon_root),
            || {
                prunes.set(prunes.get() + 1);
                Ok(1_000)
            },
        )
        .unwrap();
        assert_eq!(freed, 0);
        assert_eq!(prunes.get(), 0, "different-root candidate must not prune");
    }

    #[test]
    fn conflict_created_recovers_own_unbound_lease_but_rejects_stale_unleased_record() {
        let root = temp_root("conflict-created-creator-lease");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let config = "no-config-v1";
        let container_id = "immutable-container-id";
        let lease = begin_persistent_builder_creator_lease(&domain, &builder, config, 1).unwrap();
        let attested = std::cell::Cell::new(0);
        let ready = std::cell::Cell::new(false);
        let recovered = std::cell::Cell::new(0);
        ensure_conflicting_builder_creator_or_readiness(
            container_id,
            config,
            Duration::from_secs(1),
            || {
                attested.set(attested.get() + 1);
                Ok(Some(container_id.to_owned()))
            },
            || Ok(ready.get()),
            || Ok(false),
            |id| {
                assert_eq!(id, container_id);
                recovered.set(recovered.get() + 1);
                ready.set(true);
                Ok(true)
            },
            || read_live_builder_creator(&domain, &builder),
            |_| panic!("recovered stale Created container should be ready"),
        )
        .unwrap();
        assert_eq!(attested.get(), 1, "candidate must be attested first");
        assert_eq!(recovered.get(), 1, "only the current lease may recover");

        // A crash/drop before create attestation leaves a stale unbound
        // record, but the flock makes it non-authoritative for the next 409.
        drop(lease);
        let unauthenticated_attests = std::cell::Cell::new(0);
        let error = ensure_conflicting_builder_creator_or_readiness(
            container_id,
            config,
            Duration::from_millis(1),
            || {
                unauthenticated_attests.set(unauthenticated_attests.get() + 1);
                Ok(Some(container_id.to_owned()))
            },
            || Ok(false),
            || Ok(false),
            |_| panic!("stale unleased creator must not authorize recovery"),
            || read_live_builder_creator(&domain, &builder),
            |_| panic!("stale creator must not enter Created polling"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("live matching creator lease"));
        assert_eq!(unauthenticated_attests.get(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn start_epoch_invalidates_only_matching_durable_readiness() {
        let root = temp_root("builder-readiness-start-invalidation");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let container_id = "immutable-container-id";
        let fingerprint = "no-config-v1";
        write_test_builder_readiness(&domain, &builder, container_id, fingerprint).unwrap();

        assert!(invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            "replacement-id",
            fingerprint,
            1,
        )
        .is_err());
        assert!(builder_readiness_matches(&domain, &builder, container_id, fingerprint).unwrap());

        let first_epoch = invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            fingerprint,
            1,
        )
        .unwrap();
        assert_eq!(first_epoch, 2);
        assert!(!builder_readiness_matches(&domain, &builder, container_id, fingerprint).unwrap());
        // A second admitted start advances the epoch again; an old worker
        // probe cannot publish after this point.
        let second_epoch = invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            fingerprint,
            first_epoch,
        )
        .unwrap();
        assert_eq!(second_epoch, 3);
        assert!(!builder_readiness_matches(&domain, &builder, container_id, fingerprint).unwrap());
        // A genuinely new builder begins at epoch one.
        let new_builder = persistent_builder_name_for_domain(
            &domain.token,
            "other-builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        assert_eq!(
            invalidate_builder_readiness_before_start(
                &domain,
                &new_builder,
                "new-container-id",
                fingerprint,
                0,
            )
            .unwrap(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn host_stop_revokes_ready_epoch_before_dispatch_and_requires_probe_to_restore() {
        use crate::docker::client::{ContainerState, ExitInfo};

        let root = temp_root("builder-readiness-stop-invalidation");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let container_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let fingerprint = "no-config-v1";
        write_test_builder_readiness(&domain, &builder, container_id, fingerprint).unwrap();

        let stop_calls = std::cell::Cell::new(0);
        let stopped = stop_builder_under_volume_lock_with(
            &domain,
            &builder,
            container_id,
            |id| {
                assert_eq!(id, container_id);
                let state = read_builder_readiness_state(&domain, &builder)
                    .unwrap()
                    .unwrap();
                assert_eq!(state.phase, BuilderReadinessPhase::Stopping);
                assert_eq!(state.epoch, 2);
                assert!(
                    !builder_readiness_matches(&domain, &builder, container_id, fingerprint)
                        .unwrap(),
                    "readiness must be revoked before Docker receives stop"
                );
                stop_calls.set(stop_calls.get() + 1);
                Ok(true)
            },
            |_| {
                Ok(ExitInfo {
                    status: Some(ContainerState::Exited),
                    finished: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
                })
            },
        )
        .unwrap();
        assert!(stopped);
        assert_eq!(stop_calls.get(), 1);
        let durable_stopped = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(durable_stopped.phase, BuilderReadinessPhase::Stopped);
        assert_eq!(durable_stopped.epoch, 2);
        assert!(!builder_readiness_matches(&domain, &builder, container_id, fingerprint).unwrap());

        let next_epoch = start_builder_from_readiness_under_lock_with(
            &domain,
            &builder,
            &daemon_container_name(&builder),
            container_id,
            fingerprint,
            durable_stopped.epoch,
            |_| {
                Ok(ExitInfo {
                    status: Some(ContainerState::Exited),
                    finished: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
                })
            },
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(next_epoch, 3);
        assert!(!builder_readiness_matches(&domain, &builder, container_id, fingerprint).unwrap());
        probe_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            next_epoch,
            || Ok(()),
        )
        .unwrap();
        publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            next_epoch,
        )
        .unwrap();
        assert!(builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            next_epoch,
        )
        .unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn legacy_v1_readiness_is_promoted_atomically_after_exact_probe_and_is_idempotent() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };

        struct TestVolumeLock(Arc<AtomicBool>);

        impl Drop for TestVolumeLock {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }

        let root = temp_root("builder-readiness-v1-promotion");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let container_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let fingerprint = persistent_buildkit_config_fingerprint(Some(
            "[registry.\"docker.io\"]\n mirrors = [\"mirror.gcr.io\"]\n",
        ))
        .unwrap();
        assert_eq!(
            fingerprint, "sha256:333c40f4fee6f473bee299aed751bb40967b9e8315af90378a5de0b5dc69a76b",
            "v1 and v2 use the same Buildx-normalized approved-config fingerprint"
        );
        let path = builder_readiness_file(&domain, &builder);
        let original = legacy_readiness_fixture(&domain, &builder, container_id, &fingerprint);
        let legacy_value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(legacy_value["version"], BUILDER_READINESS_LEGACY_VERSION);
        assert!(legacy_value.get("epoch").is_none());
        assert!(legacy_value.get("phase").is_none());
        write_atomic_document(&path, &original).unwrap();

        let lock_active = Arc::new(AtomicBool::new(false));
        let attestations = Arc::new(AtomicUsize::new(0));
        let probes = Arc::new(AtomicUsize::new(0));
        let expected_volume = daemon_state_volume(&builder);
        let promoted = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            &fingerprint,
            {
                let lock_active = Arc::clone(&lock_active);
                let expected_volume = expected_volume.clone();
                move |volume| {
                    assert_eq!(volume, expected_volume);
                    assert!(!lock_active.swap(true, Ordering::SeqCst));
                    Ok(TestVolumeLock(Arc::clone(&lock_active)))
                }
            },
            {
                let lock_active = Arc::clone(&lock_active);
                move || {
                    assert!(lock_active.load(Ordering::SeqCst));
                    Ok(())
                }
            },
            {
                let lock_active = Arc::clone(&lock_active);
                let attestations = Arc::clone(&attestations);
                let builder = builder.clone();
                let container_id = container_id.to_owned();
                move |candidate_builder, volume, id| {
                    assert!(lock_active.load(Ordering::SeqCst));
                    assert_eq!(candidate_builder, builder);
                    assert_eq!(volume, daemon_state_volume(&builder));
                    assert_eq!(id, container_id);
                    attestations.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            {
                let lock_active = Arc::clone(&lock_active);
                let probes = Arc::clone(&probes);
                let container_id = container_id.to_owned();
                move |id| {
                    assert_eq!(id, container_id);
                    assert!(
                        !lock_active.load(Ordering::SeqCst),
                        "worker readiness polling must release the volume flock"
                    );
                    probes.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .unwrap();
        assert!(promoted);
        assert_eq!(attestations.load(Ordering::SeqCst), 2);
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        let promoted_bytes = std::fs::read(&path).unwrap();
        assert_ne!(promoted_bytes, original);
        assert!(
            serde_json::from_slice::<BuilderReadinessRecordV1>(&promoted_bytes).is_err(),
            "a downgraded strict v1 reader must fail closed on v2 fields"
        );
        let promoted_state = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(promoted_state.version, BUILDER_READINESS_VERSION);
        assert_eq!(promoted_state.container_id, container_id);
        assert_eq!(promoted_state.config_fingerprint, fingerprint);
        assert_eq!(promoted_state.epoch, 1);
        assert_eq!(promoted_state.phase, BuilderReadinessPhase::Ready);

        let repeated = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            &fingerprint,
            |_| Ok(()),
            || Ok(()),
            |_, _, _| panic!("v2 readiness is not re-attested as v1"),
            |_| panic!("v2 readiness does not repeat the migration probe"),
        )
        .unwrap();
        assert!(!repeated, "v2 promotion is idempotent");
        assert_eq!(std::fs::read(&path).unwrap(), promoted_bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn legacy_v1_readiness_mismatch_or_probe_failure_preserves_bytes_and_can_retry() {
        let root = temp_root("builder-readiness-v1-fail-closed");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let container_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let fingerprint = "no-config-v1";
        let path = builder_readiness_file(&domain, &builder);
        let original = legacy_readiness_fixture(&domain, &builder, container_id, fingerprint);
        write_atomic_document(&path, &original).unwrap();

        let mismatch = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            "sha256:another-config",
            |_| Ok(()),
            || Ok(()),
            |_, _, _| panic!("config mismatch must fail before container operations"),
            |_| panic!("config mismatch must fail before readiness probe"),
        )
        .unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("mismatched identity or config"));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let attestation_error = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            fingerprint,
            |_| Ok(()),
            || Ok(()),
            |_, _, id| {
                assert_eq!(id, container_id);
                anyhow::bail!("injected immutable-container attestation failure")
            },
            |_| panic!("failed attestation must not probe workers"),
        )
        .unwrap_err();
        assert!(format!("{attestation_error:#}")
            .contains("injected immutable-container attestation failure"));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let probe_error = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            fingerprint,
            |_| Ok(()),
            || Ok(()),
            |_, _, id| {
                assert_eq!(id, container_id);
                Ok(())
            },
            |_| anyhow::bail!("injected worker probe failure"),
        )
        .unwrap_err();
        assert!(format!("{probe_error:#}").contains("injected worker probe failure"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "failed promotion keeps the recoverable v1 source document"
        );

        let retried = promote_builder_readiness_v1_with(
            &domain,
            &builder,
            fingerprint,
            |_| Ok(()),
            || Ok(()),
            |candidate_builder, volume, id| {
                assert_eq!(candidate_builder, builder);
                assert_eq!(volume, daemon_state_volume(&builder));
                assert_eq!(id, container_id);
                Ok(())
            },
            |id| {
                assert_eq!(id, container_id);
                Ok(())
            },
        )
        .unwrap();
        assert!(retried, "an unchanged v1 record remains retryable");
        let promoted = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(promoted.phase, BuilderReadinessPhase::Ready);
        assert_eq!(promoted.epoch, 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_start_probe_cannot_publish_over_newer_epoch() {
        let root = temp_root("builder-readiness-epoch-cas");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("repo-key-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let container_id = "immutable-container-id";
        let fingerprint = "no-config-v1";

        let older_epoch = invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            fingerprint,
            0,
        )
        .unwrap();
        let newer_epoch = invalidate_builder_readiness_before_start(
            &domain,
            &builder,
            container_id,
            fingerprint,
            older_epoch,
        )
        .unwrap();

        assert!(publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            older_epoch,
        )
        .is_err());
        assert!(!builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            older_epoch,
        )
        .unwrap());
        assert!(publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            newer_epoch,
        )
        .is_ok());
        assert!(builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            fingerprint,
            newer_epoch,
        )
        .unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_reused_builder_start_is_retryable_from_exact_starting_epoch() {
        let root = temp_root("builder-readiness-start-retry");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("repo-key-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let container_id = "immutable-container-id";
        let config = "no-config-v1";
        write_test_builder_readiness(&domain, &builder, container_id, config).unwrap();

        let initial = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(initial.phase, BuilderReadinessPhase::Ready);
        assert_eq!(initial.epoch, 1);
        let first_start_inspections = std::cell::Cell::new(0);
        let failed = start_builder_from_readiness_under_lock_with(
            &domain,
            &builder,
            "buildkitd",
            container_id,
            config,
            initial.epoch,
            |id| {
                assert_eq!(id, container_id);
                first_start_inspections.set(first_start_inspections.get() + 1);
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: None,
                })
            },
            |id| {
                assert_eq!(id, container_id);
                anyhow::bail!("ContainerStart response was lost")
            },
        );
        assert!(failed.is_err());
        assert_eq!(first_start_inspections.get(), 2);
        let failed_attempt = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(failed_attempt.phase, BuilderReadinessPhase::Starting);
        assert_eq!(failed_attempt.epoch, 2);
        assert!(!builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            config,
            failed_attempt.epoch,
        )
        .unwrap());

        // A new setup/recovery attempt must revalidate the same ID/config,
        // advance again, and retry under its Engine-volume lock. Treat this
        // retry's later worker-probe failure as an unpublished Starting
        // epoch; a further retry must advance once more.
        let retry_start_ids = std::cell::RefCell::new(Vec::new());
        let retry_epoch = start_builder_from_readiness_under_lock_with(
            &domain,
            &builder,
            "buildkitd",
            container_id,
            config,
            failed_attempt.epoch,
            |id| {
                assert_eq!(id, container_id);
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: None,
                })
            },
            |id| {
                retry_start_ids.borrow_mut().push(id.to_owned());
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(retry_start_ids.into_inner(), vec![container_id]);
        assert_eq!(retry_epoch, 3);
        let probe_failed = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(probe_failed.phase, BuilderReadinessPhase::Starting);
        assert_eq!(probe_failed.epoch, retry_epoch);
        assert!(!builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            config,
            retry_epoch,
        )
        .unwrap());
        let probe_calls = std::cell::Cell::new(0);
        let failed_probe = probe_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config,
            retry_epoch,
            || {
                probe_calls.set(probe_calls.get() + 1);
                anyhow::bail!("BuildKit worker probe failed")
            },
        );
        assert!(failed_probe.is_err());
        assert_eq!(probe_calls.get(), 1);
        let after_failed_probe = read_builder_readiness_state(&domain, &builder)
            .unwrap()
            .unwrap();
        assert_eq!(after_failed_probe.phase, BuilderReadinessPhase::Starting);
        assert_eq!(after_failed_probe.epoch, retry_epoch);
        assert!(!builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            config,
            retry_epoch,
        )
        .unwrap());

        let final_start_ids = std::cell::RefCell::new(Vec::new());
        let final_epoch = start_builder_from_readiness_under_lock_with(
            &domain,
            &builder,
            "buildkitd",
            container_id,
            config,
            retry_epoch,
            |id| {
                assert_eq!(id, container_id);
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: None,
                })
            },
            |id| {
                final_start_ids.borrow_mut().push(id.to_owned());
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(final_start_ids.into_inner(), vec![container_id]);
        assert_eq!(final_epoch, 4);
        assert!(probe_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config,
            final_epoch,
            || Ok(()),
        )
        .unwrap()
        .is_none());
        assert!(publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config,
            failed_attempt.epoch,
        )
        .is_err());
        assert!(publish_builder_readiness_for_epoch(
            &domain,
            &builder,
            container_id,
            config,
            retry_epoch,
        )
        .is_err());
        assert!(!builder_readiness_matches(&domain, &builder, container_id, config).unwrap());
        publish_builder_readiness_for_epoch(&domain, &builder, container_id, config, final_epoch)
            .unwrap();
        assert!(builder_readiness_matches_epoch(
            &domain,
            &builder,
            container_id,
            config,
            final_epoch,
        )
        .unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn horizon_restart_false_is_an_error() {
        assert!(require_buildkit_restart(Ok(false)).is_err());
        assert!(require_buildkit_restart(Ok(true)).is_ok());
    }

    #[test]
    fn conflict_wait_rechecks_readiness_when_creator_disappears() {
        let root = temp_root("conflict-readiness-published-before-creator-disappears");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let config = "no-config-v1";
        let container_id = "immutable-container-id";
        let ready = std::cell::Cell::new(false);
        let attestations = std::cell::Cell::new(0);

        ensure_conflicting_builder_creator_or_readiness(
            container_id,
            config,
            Duration::from_secs(1),
            || {
                attestations.set(attestations.get() + 1);
                Ok(Some(container_id.to_owned()))
            },
            || builder_readiness_matches(&domain, &builder, container_id, config),
            || Ok(false),
            |_| panic!("readiness was published by the winner"),
            || {
                let proof = BuilderReadinessRecord {
                    version: BUILDER_READINESS_VERSION,
                    builder: builder.clone(),
                    domain_token: domain.token.clone(),
                    state_volume: daemon_state_volume(&builder),
                    container_id: container_id.to_owned(),
                    config_fingerprint: config.to_owned(),
                    epoch: 1,
                    phase: BuilderReadinessPhase::Ready,
                };
                write_atomic_document(
                    &builder_readiness_file(&domain, &builder),
                    &serde_json::to_vec(&proof).unwrap(),
                )?;
                ready.set(true);
                Ok(None)
            },
            |_| panic!("exact readiness must win over a vanished creator record"),
        )
        .unwrap();

        assert!(ready.get());
        assert_eq!(attestations.get(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflict_created_waits_for_distinct_create_to_bind_and_publish_readiness() {
        let root = temp_root("conflict-created-distinct-creator");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let config = "no-config-v1";
        let container_id = "immutable-container-id";
        let _lease = begin_persistent_builder_creator_lease(&domain, &builder, config, 1).unwrap();
        let attested = std::cell::Cell::new(0);
        let mut winner_published = false;

        ensure_conflicting_builder_creator_or_readiness(
            container_id,
            config,
            Duration::from_secs(1),
            || {
                attested.set(attested.get() + 1);
                Ok(Some(container_id.to_owned()))
            },
            || builder_readiness_matches(&domain, &builder, container_id, config),
            || Ok(true),
            |_| panic!("distinct live create must win; do not recover its container"),
            || read_live_builder_creator(&domain, &builder),
            |_| {
                if !winner_published {
                    // This models the separate POST /containers/create that
                    // owns the successful 201 response; only its observer may
                    // bind the immutable ID and archive fingerprint.
                    bind_persistent_builder_creator_container(
                        &domain,
                        &builder,
                        1,
                        config,
                        container_id,
                    )
                    .unwrap();
                    record_persistent_builder_creator_archive(
                        &domain,
                        &builder,
                        1,
                        config,
                        container_id,
                        config,
                    )
                    .unwrap();
                    let proof = BuilderReadinessRecord {
                        version: BUILDER_READINESS_VERSION,
                        builder: builder.clone(),
                        domain_token: domain.token.clone(),
                        state_volume: daemon_state_volume(&builder),
                        container_id: container_id.to_owned(),
                        config_fingerprint: config.to_owned(),
                        epoch: 1,
                        phase: BuilderReadinessPhase::Ready,
                    };
                    write_atomic_document(
                        &builder_readiness_file(&domain, &builder),
                        &serde_json::to_vec(&proof).unwrap(),
                    )
                    .unwrap();
                    winner_published = true;
                }
            },
        )
        .unwrap();
        assert_eq!(attested.get(), 1);
        assert!(winner_published);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_buildkit_lifecycle_errors_use_immutable_id_inspection() {
        let start_inspections = std::cell::Cell::new(0);
        assert!(start_attested_builder_confirmed_with(
            "immutable-container-id",
            |_| {
                let inspection = start_inspections.get() + 1;
                start_inspections.set(inspection);
                Ok(crate::docker::client::ExitInfo {
                    status: Some(if inspection == 1 {
                        crate::docker::client::ContainerState::Exited
                    } else {
                        crate::docker::client::ContainerState::Running
                    }),
                    finished: None,
                })
            },
            |_| anyhow::bail!("start response was lost"),
        )
        .unwrap());
        assert_eq!(start_inspections.get(), 2);

        assert!(stop_attested_builder_confirmed_with(
            "immutable-container-id",
            |_| anyhow::bail!("stop response was lost"),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: None,
                })
            },
        )
        .unwrap());

        let stop_inspections = std::cell::Cell::new(0);
        assert!(stop_attested_builder_confirmed_with(
            "immutable-container-id",
            |_| anyhow::bail!("stop response was lost"),
            |_| {
                stop_inspections.set(stop_inspections.get() + 1);
                Err(anyhow::anyhow!("inspect unavailable"))
            },
        )
        .is_err());
        assert_eq!(stop_inspections.get(), 1);

        let start_attempts = std::cell::Cell::new(0);
        assert!(start_attested_builder_confirmed_with(
            "immutable-container-id",
            |_| Err(anyhow::anyhow!("inspect unavailable")),
            |_| {
                start_attempts.set(start_attempts.get() + 1);
                Ok(())
            },
        )
        .is_err());
        assert_eq!(start_attempts.get(), 0);
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
        assert!(is_current_domained_persistent_builder(&current));
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-trusted-branch-o_r"
        ));
        assert!(!is_current_domained_persistent_builder(
            "velnor-builder-shared-trusted-branch-o_r"
        ));
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r"
        ));
        assert!(!is_current_domained_persistent_builder(
            "velnor-builder-shared-unbounded-v1-trusted-branch-o_r"
        ));
        assert!(!is_persistent_builder_name("velnor-builder-slot-3"));
        assert!(!is_persistent_builder_name(
            "velnor-builder-mybuilder-slot-3"
        ));
        // The legacy adversarial case: a marker-bearing old name remains
        // reserved from guest/generic cleanup. Without current domain proof it
        // stays for explicit operator cleanup.
        assert!(is_persistent_builder_name(
            "velnor-builder-shared-foo-slot-3"
        ));
        // Exact Buildx object shapes preserve both current and retired
        // canonical resources, while arbitrary marker-bearing job names are
        // ordinary cleanup targets.
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-trusted-branch-o_r0_state"
        ));
        assert!(is_persistent_builder_object(&daemon_container_name(
            &current
        )));
        assert!(is_persistent_builder_object(&format!(
            "buildx_buildkit_{current}10_state"
        )));
        assert!(is_velnor_buildkit_daemon_name(
            "buildx_buildkit_velnor-builder-custom10"
        ));
        assert!(is_persistent_builder_object(
            "buildx_buildkit_velnor-builder-shared-unbounded-v1-trusted-branch-o_r0"
        ));
        assert!(!is_persistent_builder_object(
            "guest-velnor-builder-shared-cache"
        ));
        assert!(!is_persistent_builder_object(
            "buildx_buildkit_guest-velnor-builder-shared-cache0_state"
        ));
        assert!(!is_velnor_buildkit_daemon_name(
            "guest-buildx_buildkit_velnor-builder-marker0"
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
        assert!(!is_current_domained_persistent_builder(old));
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
    fn upgrade_reaper_leaves_every_unscoped_generation_untouched() {
        let root = temp_root("upgrade-reap-unscoped");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let old_unbounded = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r".to_string();
        let current = test_builder();
        let external = "external-buildx-cache".to_string();
        let mut legacy_claims = BuilderClaims {
            builder: legacy.clone(),
            ..BuilderClaims::default()
        };
        legacy_claims.holders.insert(
            "velnor-job-old".to_string(),
            BuilderHolder {
                container: "velnor-job-old".to_string(),
                slot: "slot-old".to_string(),
                claimed_unix: 1,
            },
        );
        let mut v1_claims = BuilderClaims {
            builder: old_unbounded.clone(),
            ..BuilderClaims::default()
        };
        v1_claims.holders.insert(
            "velnor-job-v1".to_string(),
            BuilderHolder {
                container: "velnor-job-v1".to_string(),
                slot: "slot-v1".to_string(),
                claimed_unix: 2,
            },
        );
        let legacy_path = claims_file(&run_root, &legacy);
        let v1_path = claims_file(&run_root, &old_unbounded);
        write_claims(&legacy_path, &legacy_claims).unwrap();
        write_claims(&v1_path, &v1_claims).unwrap();
        let legacy_bytes = std::fs::read(&legacy_path).unwrap();
        let v1_bytes = std::fs::read(&v1_path).unwrap();
        claim_builder(&run_root, &current, "slot-new", "velnor-job-new").unwrap();
        let present = BTreeSet::from(["velnor-job-new".to_string()]);
        let now = SystemTime::now();
        let report = reap_idle_builders_with(
            &run_root,
            now,
            || {
                Ok(vec![
                    legacy.clone(),
                    old_unbounded.clone(),
                    external.clone(),
                    current.clone(),
                ])
            },
            || Ok(present.clone()),
            |_| panic!("unscoped/current-held builders must not be inspected"),
            |_| panic!("unscoped/current-held builders must not be stopped"),
            |_| panic!("unscoped/current-held builders must not be restarted"),
            |_| panic!("unscoped/current-held builders must not be removed"),
        );

        assert!(report.deleted.is_empty());
        assert!(report.stopped.is_empty());
        assert!(report.failures.is_empty());
        assert_eq!(std::fs::read(legacy_path).unwrap(), legacy_bytes);
        assert_eq!(std::fs::read(v1_path).unwrap(), v1_bytes);
        assert!(claims_file(&run_root, &current).exists());
        assert!(!claims_file(&run_root, &external).exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn retired_unscoped_missing_claim_stays_untouched() {
        let root = temp_root("legacy-reap-after-reboot");
        let run_root = root.join("run");
        let legacy = "velnor-builder-shared-trusted-branch-o_r".to_string();
        let old_unbounded = "velnor-builder-shared-unbounded-v1-trusted-branch-o_r".to_string();
        // Missing old-generation claims are not evidence of quiescence; the
        // retired resource remains outside this generation's ownership.
        assert!(!claims_file(&run_root, &legacy).exists());
        let report = reap_idle_builders_with(
            &run_root,
            SystemTime::now(),
            || {
                Ok(vec![
                    legacy.clone(),
                    old_unbounded.clone(),
                    "external-builder".to_string(),
                ])
            },
            || Ok(BTreeSet::new()),
            |_| panic!("unscoped builders must not be inspected"),
            |_| panic!("unscoped builders must not be stopped"),
            |_| panic!("unscoped builders must not be restarted"),
            |_| panic!("unscoped builders must not be removed"),
        );

        assert!(report.deleted.is_empty());
        assert!(report.failures.is_empty());
        assert!(!claims_file(&run_root, &legacy).exists());
        assert!(!claims_file(&run_root, &old_unbounded).exists());
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
    fn current_builder_missing_claim_stays_pinned_after_reboot() {
        let root = temp_root("current-reap-after-reboot");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_owner_record(&registry_root, &builder).unwrap();
        let owner_path = owner_registry_file(&registry_root, &builder);
        assert!(owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());

        let now = SystemTime::now();
        let inspected = std::cell::RefCell::new(Vec::new());
        let removed = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |daemon| {
                inspected.borrow_mut().push(daemon.to_string());
                Ok(crate::docker::client::ExitInfo {
                    status: None,
                    finished: None,
                })
            },
            |_| Ok(true),
            |_| Ok(true),
            |removed_builder| {
                removed.borrow_mut().push(removed_builder.to_string());
                Ok(())
            },
        );

        assert!(inspected.borrow().is_empty());
        assert!(removed.borrow().is_empty());
        assert!(report.deleted.is_empty());
        assert!(report.stopped.is_empty());
        assert!(report
            .unreadable_claims
            .contains(&owner_path.display().to_string()));
        assert!(!claims_file(&run_root, &builder).exists());
        assert!(owner_path.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn current_builder_missing_claim_blocks_setup_and_release_but_fresh_owner_claims() {
        let root = temp_root("current-claim-missing-owner-guard");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let registered = test_builder();
        ensure_owner_record(&registry_root, &registered).unwrap();
        let registered_claim = claims_file(&run_root, &registered);
        assert!(!registered_claim.exists());

        assert!(claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &registered,
            "slot-new",
            "velnor-job-new",
        )
        .is_err());
        assert!(!registered_claim.exists());

        let stopped = std::cell::RefCell::new(Vec::new());
        let restarted = std::cell::RefCell::new(Vec::new());
        let outcome = release_and_stop_if_last_with_registry(
            &run_root,
            Some(&registry_root),
            &registered,
            "velnor-job-old",
            || {
                stopped.borrow_mut().push(registered.clone());
                Ok(true)
            },
            || {
                restarted.borrow_mut().push(registered.clone());
                Ok(true)
            },
        )
        .unwrap();
        assert!(!outcome.removed_last);
        assert!(!outcome.stopped);
        assert!(!outcome.restarted);
        assert!(stopped.borrow().is_empty());
        assert!(restarted.borrow().is_empty());
        assert!(!registered_claim.exists());

        let fresh = persistent_builder_name("fresh", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let fresh_claim = claims_file(&run_root, &fresh);
        assert!(!owner_registry_file(&registry_root, &fresh).exists());
        let outcome = release_and_stop_if_last_with_registry(
            &run_root,
            Some(&registry_root),
            &fresh,
            "velnor-job-old",
            || {
                stopped.borrow_mut().push(fresh.clone());
                Ok(true)
            },
            || {
                restarted.borrow_mut().push(fresh.clone());
                Ok(true)
            },
        )
        .unwrap();
        assert!(!outcome.removed_last);
        assert!(!outcome.stopped);
        assert!(!outcome.restarted);
        assert!(stopped.borrow().is_empty());
        assert!(restarted.borrow().is_empty());
        assert!(!fresh_claim.exists());
        assert!(!owner_registry_file(&registry_root, &fresh).exists());

        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &fresh,
            "slot-new",
            "velnor-job-new",
        )
        .unwrap();
        assert!(fresh_claim.exists());
        assert!(owner_registry_file(&registry_root, &fresh).exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn release_recheck_restarts_when_current_claim_disappears() {
        let root = temp_root("release-current-claim-disappears");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-1",
            "velnor-job-a",
        )
        .unwrap();
        let claim_path = claims_file(&run_root, &builder);
        let stopped = std::cell::RefCell::new(Vec::new());
        let restarted = std::cell::RefCell::new(Vec::new());

        let outcome = release_and_stop_if_last_with_registry(
            &run_root,
            Some(&registry_root),
            &builder,
            "velnor-job-a",
            || {
                stopped.borrow_mut().push(builder.clone());
                std::fs::remove_file(&claim_path).unwrap();
                Ok(true)
            },
            || {
                restarted.borrow_mut().push(builder.clone());
                Ok(true)
            },
        )
        .unwrap();

        assert!(outcome.removed_last);
        assert!(outcome.stopped);
        assert!(outcome.restarted);
        assert_eq!(*stopped.borrow(), vec![builder.clone()]);
        assert_eq!(*restarted.borrow(), vec![builder.clone()]);
        assert!(!claim_path.exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_recheck_treats_a_missing_current_claim_as_still_held() {
        let root = temp_root("horizon-current-claim-disappears-after-inspect");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &builder,
            "slot-gone",
            "velnor-job-gone",
        )
        .unwrap();
        let claim_path = claims_file(&run_root, &builder);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000_000);
        let old_finished = now
            .checked_sub(IDLE_DELETE_AFTER + Duration::from_secs(1))
            .unwrap();
        let removed = std::cell::RefCell::new(Vec::new());

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            now,
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| {
                std::fs::remove_file(&claim_path).unwrap();
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Exited),
                    finished: Some(old_finished),
                })
            },
            |_| Ok(true),
            |_| Ok(true),
            |name| {
                removed.borrow_mut().push(name.to_string());
                Ok(())
            },
        );

        assert!(report.deleted.is_empty());
        assert!(removed.borrow().is_empty());
        assert!(!claim_path.exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn horizon_stop_error_rechecks_claims_and_restarts_after_possible_stop() {
        let root = temp_root("horizon-ambiguous-stop");
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
        let restarted = std::cell::RefCell::new(Vec::new());

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(BTreeSet::new()),
            |_| {
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Running),
                    finished: None,
                })
            },
            |_| {
                claim_builder_with_registry(
                    &run_root,
                    Some(&registry_root),
                    &builder,
                    "slot-new",
                    "velnor-job-new",
                )?;
                Err(anyhow::anyhow!("stop timed out after acting"))
            },
            |_| {
                restarted.borrow_mut().push(builder.clone());
                Ok(true)
            },
            |_| panic!("a racing holder prevents removal"),
        );

        assert!(report.deleted.is_empty());
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("stop timed out after acting")));
        assert_eq!(*restarted.borrow(), vec![builder.clone()]);
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn current_builder_missing_claim_with_live_job_is_not_stopped_or_removed() {
        let root = temp_root("current-reap-missing-claim-live-job");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let builder = test_builder();
        ensure_owner_record(&registry_root, &builder).unwrap();
        let claim_path = claims_file(&run_root, &builder);
        assert!(!claim_path.exists());

        let inspected = std::cell::RefCell::new(Vec::new());
        let stopped = std::cell::RefCell::new(Vec::new());
        let restarted = std::cell::RefCell::new(Vec::new());
        let removed = std::cell::RefCell::new(Vec::new());
        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![builder.clone()]),
            || Ok(["velnor-job-live".to_string()].into_iter().collect()),
            |daemon| {
                inspected.borrow_mut().push(daemon.to_string());
                Ok(crate::docker::client::ExitInfo {
                    status: Some(crate::docker::client::ContainerState::Running),
                    finished: None,
                })
            },
            |name| {
                stopped.borrow_mut().push(name.to_string());
                Ok(true)
            },
            |name| {
                restarted.borrow_mut().push(name.to_string());
                Ok(true)
            },
            |name| {
                removed.borrow_mut().push(name.to_string());
                Ok(())
            },
        );

        assert!(inspected.borrow().is_empty());
        assert!(stopped.borrow().is_empty());
        assert!(restarted.borrow().is_empty());
        assert!(removed.borrow().is_empty());
        assert!(report.stopped.is_empty());
        assert!(report.deleted.is_empty());
        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("runtime claim file")
        }));
        assert!(!claim_path.exists());
        assert!(owner_registry_file(&registry_root, &builder).exists());
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
    fn current_domain_owner_with_old_schema_is_reported_and_preserved() {
        let root = temp_root("old-owner-schema-current-domain");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        let registry_root = owner_registry_root(&domain.root);
        let path = owner_registry_file(&registry_root, &builder);
        let legacy_current_record = serde_json::json!({
            "version": OWNER_REGISTRY_VERSION - 1,
            "builder": builder,
        });
        let bytes = serde_json::to_vec(&legacy_current_record).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let error = registered_domain_builders(&registry_root, &domain.token).unwrap_err();
        assert!(error.to_string().contains("unsupported schema version"));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owner_record_absent_from_inventory_is_retained_without_docker_absence_proof() {
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

        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("without Docker absence proof")
        }));
        assert!(owner_path.exists());
        assert!(!claims_file(&run_root, &builder).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn owner_record_absent_from_inventory_does_not_repair_or_delete_claims() {
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
        let claims_path = claims_file(&run_root, &builder);
        let claims_before = std::fs::read(&claims_path).unwrap();

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

        assert!(report.failures.iter().any(|failure| {
            failure.contains(&builder) && failure.contains("without Docker absence proof")
        }));
        assert!(owner_path.exists());
        assert_eq!(std::fs::read(claims_path).unwrap(), claims_before);
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
    fn setup_lifecycle_lock_prevents_final_release_stop_during_new_claim() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};

        let root = temp_root("setup-release-lifecycle-lock");
        let domain =
            PersistentBuildKitDomain::from_identities(&root, "storage-a", "engine-a").unwrap();
        let builder = persistent_builder_name_for_domain(
            &domain.token,
            "builder",
            "trusted",
            TRUST_TIER_BRANCH,
            Some("org/repo"),
        );
        claim_domain_builder(&domain, &builder, "slot-1", "old-job").unwrap();

        // Model setup after it acquired the per-builder lifecycle lock and
        // before it publishes its new claim. Release must wait until setup
        // completes, then observe both holders and skip stop.
        let setup_lock = lock_builder_lifecycle(&domain.root, &builder).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let stop_called = Arc::new(AtomicBool::new(false));
        let release_domain = domain.clone();
        let release_builder = builder.clone();
        let stop_flag = Arc::clone(&stop_called);
        let release_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            release_domain_builder_if_last(
                &release_domain,
                &release_builder,
                "old-job",
                || {
                    stop_flag.store(true, Ordering::SeqCst);
                    Ok(true)
                },
                || panic!("no stop means no restart"),
            )
        });
        started_rx.recv().unwrap();
        claim_domain_builder(&domain, &builder, "slot-2", "new-job").unwrap();
        drop(setup_lock);

        let outcome = release_thread.join().unwrap().unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome {
                removed_last: false,
                stopped: false,
                restarted: false,
            }
        );
        assert!(!stop_called.load(Ordering::SeqCst));
        assert_eq!(
            builder_holders(&domain.root, &builder, None).unwrap().len(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
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

        let claims = read_claims(&path, &builder).unwrap();
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
    fn release_stop_error_rechecks_claims_and_restarts_after_possible_stop() {
        let root = temp_root("release-ambiguous-stop");
        let run_root = root.join("run");
        let builder = test_builder();
        claim_builder(&run_root, &builder, "slot-1", "velnor-job-a").unwrap();
        let restarted = std::cell::RefCell::new(Vec::new());

        let outcome = release_and_stop_if_last(
            &run_root,
            &builder,
            "velnor-job-a",
            || {
                // Simulate a new setup arriving while Docker stop times out
                // after it may already have stopped the daemon.
                claim_builder(&run_root, &builder, "slot-2", "velnor-job-b").unwrap();
                Err(anyhow::anyhow!("stop timed out after acting"))
            },
            || {
                restarted.borrow_mut().push(builder.clone());
                Ok(true)
            },
        )
        .unwrap();

        assert!(outcome.removed_last);
        assert!(
            !outcome.stopped,
            "ambiguous stop cannot be reported as acted"
        );
        assert!(outcome.restarted);
        assert_eq!(*restarted.borrow(), vec![builder.clone()]);
        assert_eq!(builder_holders(&run_root, &builder, None).unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
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
        std::fs::write(
            &marker,
            vec![b'0'; (MAX_HORIZON_REAP_MARKER_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(
            horizon_reap_due(&marker, now),
            "oversized marker is unreadable"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn horizon_marker_symlink_is_not_followed_for_read_or_write() {
        use std::os::unix::fs::symlink;

        let root = temp_root("reap-marker-symlink");
        let marker = root.join("marker");
        let external = root.join("external-marker");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000_000);
        std::fs::write(&external, b"19999999").unwrap();
        symlink(&external, &marker).unwrap();

        assert!(horizon_reap_due(&marker, now));
        let runs = std::cell::Cell::new(0);
        maybe_reap_idle_builders_with(&marker, now, |_, _| {
            runs.set(runs.get() + 1);
            HorizonReport::default()
        })
        .unwrap();

        assert_eq!(runs.get(), 1);
        assert_eq!(std::fs::read(&external).unwrap(), b"19999999");
        assert!(std::fs::symlink_metadata(&marker)
            .unwrap()
            .file_type()
            .is_symlink());
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

    #[test]
    fn unreadable_owner_record_reports_owner_path_not_runtime_claim() {
        let root = temp_root("owner-record-unreadable-path");
        let run_root = root.join("run");
        let registry_root = owner_registry_root(&root.join("lib"));
        let owner_corrupt =
            persistent_builder_name("owner-corrupt", "trusted", TRUST_TIER_BRANCH, Some("o/r"));
        let claim_corrupt =
            persistent_builder_name("claim-corrupt", "trusted", TRUST_TIER_BRANCH, Some("o/r"));

        // Valid runtime claim plus a torn durable owner record: the failure
        // is the owner record, so the report must name it — never the
        // healthy runtime claim the operator would otherwise delete.
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &owner_corrupt,
            "slot-1",
            "velnor-job-a",
        )
        .unwrap();
        std::fs::write(
            owner_registry_file(&registry_root, &owner_corrupt),
            b"{torn",
        )
        .unwrap();

        // Torn runtime claim plus a valid owner record: the companion case
        // still names the runtime claim file.
        claim_builder_with_registry(
            &run_root,
            Some(&registry_root),
            &claim_corrupt,
            "slot-2",
            "velnor-job-b",
        )
        .unwrap();
        let torn_claim_path = claims_file(&run_root, &claim_corrupt);
        std::fs::write(&torn_claim_path, b"{torn").unwrap();

        let report = reap_idle_builders_with_registry(
            &run_root,
            Some(&registry_root),
            SystemTime::now(),
            || Ok(vec![owner_corrupt.clone(), claim_corrupt.clone()]),
            || Ok(BTreeSet::new()),
            |_| panic!("unreadable ownership blocks inspection"),
            |_| panic!("unreadable ownership blocks stop"),
            |_| panic!("unreadable ownership blocks restart"),
            |_| panic!("unreadable ownership blocks removal"),
        );

        assert!(report.deleted.is_empty());
        assert_eq!(
            report.unreadable_claims,
            vec![
                owner_registry_file(&registry_root, &owner_corrupt)
                    .display()
                    .to_string(),
                torn_claim_path.display().to_string(),
            ]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}

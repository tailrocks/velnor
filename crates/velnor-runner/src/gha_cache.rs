//! Velnor-native GitHub Actions cache service (Plan P1).
//!
//! Self-hosted job messages never carry a `CacheServerUrl`, so BuildKit's
//! `type=gha` backend and `actions/cache@v4` silently no-op on Velnor while
//! working on GitHub-hosted runners. This module hosts the two cache service
//! generations on a small hyper server so the same YAML is warm on every lane:
//!
//! * v1 artifactcache (`actions/cache`, BuildKit):
//!   `GET _apis/artifactcache/cache`, `POST _apis/artifactcache/caches`,
//!   `PATCH _apis/artifactcache/caches/{id}`, `POST .../caches/{id}`
//! * v2 Results CacheService (buildkit selects via
//!   `ACTIONS_CACHE_SERVICE_V2=True`): `CreateCacheEntry`,
//!   `FinalizeCacheEntryUpload`, `GetCacheEntryDownloadURL`
//!
//! Storage is content-addressed beneath the durable cache root, namespaced by
//! canonical GitHub server origin, numeric repository ID, and ref:
//! `tenants/repo-<sha256(repository-key \0 ref)>/{blobs,entries,reservations}`
//! plus at most 4096 stable sharded entry-lock files at the cache root and tiny JSON
//! entry records keyed by a length-framed SHA-256 of `(key, version)`. The runner
//! registers each job's token-to-repository binding before the job runs
//! (`register_job_cache_session`, backed by `sessions/<token-sha256>.json`);
//! lookups then search the job's ref namespace first and its base (PR target)
//! namespace second, so runs share caches exactly the way GitHub's "current
//! branch, then base branch" scope rules describe. Writes always land in the
//! job's own ref namespace, so a pull-request run can restore base-branch
//! entries but can never overwrite them — the read-across/write-isolated
//! direction the cache-poisoning rules require. Key matching within each scope
//! follows GitHub semantics: exact `(key, version)` first, then restore-keys
//! prefix order, newest wins.
//!
//! Fork isolation rides on the same chain. The session also carries the job's
//! [`TrustClass`](crate::trust_class::TrustClass): a job classified `ForkPR`
//! or `Unknown` resolves to a fork-headed chain — its isolated `fork-`
//! namespace first, then the base repository's ref and base namespaces for
//! read-through — while a `Trusted` job resolves to the base namespaces only.
//! Every write path (v1 reserve/upload, v2 upload/finalize) lands in the
//! chain head, so an untrusted job's writes can only ever reach its fork
//! namespace however its ref is shaped (a fork-controlled run on a base
//! branch ref cannot overwrite the base scope), and a trusted job's chain
//! never names a fork namespace, so it can never restore one. The `fork-`
//! and `repo-` namespaces are disjoint by hash domain as well as by prefix.
//!
//! Requests whose token has no registered session (registration failed, or a
//! caller outside the job path) fall back to an isolated per-token namespace
//! — the pre-scoping behavior — and emit one forensic line per token hash so a
//! fleet stuck without sessions is visible instead of silently cold. The token
//! itself is never persisted, logged, or compared; only hashes leave the
//! request path. Retained blobs use an LRU byte budget per namespace by
//! deleting oldest-hit entries. Multipart assembly scratch uses one shared
//! root-level byte budget so the per-namespace cap cannot multiply across
//! tenants.
//!
//! Known deviation, recorded for the follow-up: GitHub also retries the lookup
//! on the repository's default branch, which the job message does not carry,
//! so the fallback chain ends at the base ref. PR runs (whose base is usually
//! the default branch) are covered; direct pushes to a side branch cannot yet
//! restore default-branch entries.
//!
//! The service is OFF unless the operator exports `VELNOR_ACTIONS_CACHE_URL`
//! into the runner environment (strict capability contract: no behavior
//! change without explicit enablement). Control requests must carry the
//! job-scoped `ACTIONS_RUNTIME_TOKEN`; the operator's enablement variable is
//! never used as a job credential. Returned Azure transfer URLs carry
//! short-lived, resource-scoped capabilities so SDK requests need not forward
//! the bearer token.

use crate::trust_class::TrustClass;
use anyhow::{Context, Result};
use base64::Engine as _;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Channel, Full, Limited};
use hyper::body::{Body, Bytes};
use hyper::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const DEFAULT_BUDGET_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_BODY: u64 = 16 * 1024 * 1024 * 1024;
const MAX_JSON_BODY: usize = 64 * 1024;
const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;
const DOWNLOAD_BUFFERED_CHUNKS: usize = 1;
/// Sanity caps for identity fields. Generous on purpose: rejection degrades a
/// job to its isolated namespace, so the bounds only exclude garbage.
const REPOSITORY_KEY_PREFIX: &str = "repo-key-v1-";
const REPOSITORY_KEY_HEX_LEN: usize = 64;
const MAX_REF_LEN: usize = 512;
/// Session and fallback-marker retention. Job tokens live hours; a week keeps
/// the registry bounded while surviving weekends and daemon restarts.
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// A reservation without uploads can be reclaimed after one day. Active
/// uploads hold a per-attempt temp inode flock and refresh this timestamp
/// before releasing the short-lived entry shard.
const V2_RESERVATION_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// V1 reservations have no refresh field, so their file mtime is the age.
const V1_RESERVATION_TTL_MS: u64 = V2_RESERVATION_TTL_MS;
/// One global lock-file set keeps lock inodes bounded across namespaces.
/// Unrelated entry/namespace pairs in the same shard serialize (about 1/4096
/// pairwise collision chance).
const CACHE_ENTRY_LOCK_SHARDS: usize = 4096;
/// Namespace accounting/transition locks are root-level and finite, shared
/// across daemon processes, and acquired before an entry shard.
const CACHE_NAMESPACE_LOCK_SHARDS: usize = 256;
/// Shared route-activity leases let idle-tenant GC prove a namespace has no
/// request or streamed body in flight without serializing unrelated requests.
const CACHE_ACTIVITY_LOCK_SHARDS: usize = 4096;
/// Per-namespace budget locks serialize concurrent eviction passes. A small
/// global shard set bounds inode growth across the unbounded tenant set.
const CACHE_BUDGET_LOCK_SHARDS: usize = 256;
/// All multipart assembly scratch across namespaces shares one root-level cap.
/// Namespace-local cache budgets must not multiply transient copies by tenant.
const CACHE_ASSEMBLY_SCRATCH_LEASE_DIR: &str = "assembly-scratch-leases";
const CACHE_ASSEMBLY_SCRATCH_LOCK: &str = "assembly-scratch.lock";
#[cfg(unix)]
const CACHE_GC_QUARANTINE_PREFIX: &str = ".velnor-cache-gc-";
#[cfg(unix)]
const CACHE_GC_QUARANTINE_ENTRY: &str = "entry";
#[cfg(unix)]
const CACHE_GC_QUARANTINE_IDENTITY: &str = "identity.json";
#[cfg(unix)]
const CACHE_GC_QUARANTINE_PRESERVED: &str = "preserved";
/// JavaScript represents JSON numbers exactly only through 2^53-1. V1
/// `cacheId` is numeric in both actions/cache and BuildKit clients.
const MAX_SAFE_CACHE_ID: u64 = (1u64 << 53) - 1;
/// Every blocking lock path uses nonblocking attempts and a deadline, so even
/// a stuck local process cannot hold an HTTP request forever.
const CACHE_LOCK_WAIT_SECS: u64 = 30;
/// Cap simultaneous bodies independently of stripe collisions.
const MAX_CONCURRENT_CACHE_UPLOADS: usize = 32;
/// Match Azure BlockBlob's uncommitted and committed block count ceilings.
const MAX_V2_UNCOMMITTED_BLOCKS: usize = 100_000;
const MAX_V2_COMMITTED_BLOCKS: usize = 50_000;
const MAX_V1_UPLOAD_CHUNKS: usize = 100_000;
/// Bounds data and metadata dirents per namespace, including empty entries,
/// V1 reservation/index pairs, staged parts, canonical blobs, and live claims.
const MAX_CACHE_NAMESPACE_FILES: usize = 100_000;
/// Reserve headroom for an admission lease/temp plus the final data/manifest
/// links created by one protocol transition.
const CACHE_ADMISSION_TRANSITION_HEADROOM: usize = 4;
/// A token can name one fallback tenant, but arbitrary valid bearer strings
/// cannot create an unbounded number of tenant directories.
const MAX_CACHE_TENANT_NAMESPACES: usize = 100_000;
const MAX_RESERVATION_BYTES: u64 = 32 * 1024 * 1024;
/// Full cache-root sweeps are throttled to avoid rescanning every tenant on
/// each HTTP request and budget enforcement. Busy locks retry quickly.
const UPLOAD_TEMP_SWEEP_INTERVAL_SECS: u64 = 5 * 60;
const UPLOAD_TEMP_BUSY_RETRY_SECS: u64 = 60;
/// Signed archive locations live long enough for ordinary cache restore, but
/// expire if copied out of the job logs or delayed for days.
const READ_CAPABILITY_TTL_SECS: u64 = 60 * 60;
const READ_CAPABILITY_DOMAIN: &[u8] = b"velnor-actions-cache-read-v1\0";
/// Persist recent cache hits at most once per second. Eviction uses this
/// durable checkpoint, so this bounds LRU resolution while avoiding a
/// manifest fsync for every read.
const CACHE_ACCESS_CHECKPOINT_INTERVAL_NS: u64 = 1_000_000_000;
/// Bound how long a stalled body can retain a temp upload lease. Progressing
/// uploads may be large; only an idle frame read times out.
const V2_UPLOAD_IDLE_TIMEOUT_SECS: u64 = 5 * 60;
/// Bound total stream time even if a sender keeps a trickle stream alive.
/// The entry shard is released for the entire body stream.
const V2_UPLOAD_TIMEOUT_SECS: u64 = 6 * 60 * 60;

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub(crate) fn entry_hash(key: &str, version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-entry-v2\0");
    hasher.update((key.len() as u64).to_be_bytes());
    hasher.update(key.as_bytes());
    hasher.update((version.len() as u64).to_be_bytes());
    hasher.update(version.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_or_create_read_capability_secret(root: &Path) -> Result<[u8; 32]> {
    let path = root.join("read-capability.key");
    if let Some(secret) = read_read_capability_secret(&path)? {
        return Ok(secret);
    }

    let mut secret = [0u8; 32];
    secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let temporary = root.join(format!(
        ".read-capability-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<bool> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("create temporary cache read-capability key")?;
        file.write_all(&secret)
            .context("write temporary cache read-capability key")?;
        file.sync_all()
            .context("sync temporary cache read-capability key")?;
        let root_directory =
            std::fs::File::open(root).context("open cache root for key publish")?;
        let temporary_name = temporary
            .file_name()
            .context("temporary cache key has no file name")?;
        let destination_name = path.file_name().context("cache key has no file name")?;
        match rustix::fs::renameat_with(
            &root_directory,
            temporary_name,
            &root_directory,
            destination_name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
        {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error).context("publish cache read-capability key"),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    let created = result?;
    if created {
        sync_directory(root).context("sync cache read-capability key directory")?;
        read_read_capability_secret(&path)?
            .context("cache read-capability key disappeared during creation")
    } else {
        read_read_capability_secret(&path)?
            .context("cache read-capability key disappeared during creation")
    }
}

fn read_read_capability_secret(path: &Path) -> Result<Option<[u8; 32]>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("open cache read-capability key"),
    };

    // Validate the opened inode, not a path stat taken before opening. On Unix,
    // O_NOFOLLOW prevents a last-component symlink swap and these checks prove
    // the signing key is private to this runner identity.
    let metadata = file
        .metadata()
        .context("stat opened cache read-capability key")?;
    if !metadata.file_type().is_file() {
        anyhow::bail!("cache read-capability key is not a regular file");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("cache read-capability key has the wrong owner");
        }
        if metadata.mode() & 0o7777 != 0o600 {
            anyhow::bail!("cache read-capability key permissions are not 0600");
        }
        if metadata.nlink() != 1 {
            anyhow::bail!("cache read-capability key has unexpected hard links");
        }
    }

    #[cfg(not(unix))]
    anyhow::bail!(
        "cannot verify cache read-capability key ownership and permissions on this platform"
    );

    let mut bytes = Vec::with_capacity(32);
    std::io::Read::by_ref(&mut file)
        .take(33)
        .read_to_end(&mut bytes)
        .context("read cache read-capability key")?;
    let secret: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("cache read-capability key has invalid length"))?;
    Ok(Some(secret))
}

fn hmac_sha256(key: &[u8; 32], domain: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..key.len() {
        inner_pad[index] ^= key[index];
        outer_pad[index] ^= key[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(domain);
    inner.update(payload);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

fn valid_capability_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= 128
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// One resolved cache entry. `key` is the entry key that actually matched —
/// the primary key on an exact hit, or the stored key a restore key matched by
/// prefix. Both cache wire generations report it back to the client
/// (`cacheKey` in v1's `ArtifactCacheEntry`, `matched_key` in v2's
/// `GetCacheEntryDownloadURLResponse`), and `actions/cache` compares it against
/// the requested primary key to decide whether the hit was exact.
#[derive(Debug, Clone)]
struct CacheHit {
    hash: String,
    key: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CacheService {
    /// Durable storage root; tenant, session, and marker files live beneath it.
    pub(crate) root: PathBuf,
    /// Root-level HMAC key for read URLs. Tenant GC cannot remove or replace it.
    read_capability_secret: Arc<[u8; 32]>,
    /// LRU byte budget per cache namespace (one ref scope or one isolated token).
    pub(crate) budget_bytes: u64,
    /// Directory for the isolated-fallback forensic line (`daemon.log`). `None`
    /// keeps the line on stderr; the daemon sets it to its log directory.
    pub(crate) forensic_log_dir: Option<PathBuf>,
    /// Shared root-wide sweep deadline so request clones cannot trigger
    /// concurrent repeated scans.
    upload_temp_cleanup_schedule: Arc<Mutex<Option<std::time::Instant>>>,
    /// Bound simultaneous streams.
    upload_slots: Arc<tokio::sync::Semaphore>,
    /// Test-overridable to exercise namespace exhaustion without creating a
    /// hundred thousand files. Production uses the fixed constant above.
    namespace_file_limit: usize,
    tenant_namespace_limit: usize,
}

impl CacheService {
    pub(crate) fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("tenants")).context("create gha-cache tenants dir")?;
        let read_capability_secret = Arc::new(read_or_create_read_capability_secret(&root)?);
        let budget = std::env::var("VELNOR_GHA_CACHE_BUDGET_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BUDGET_BYTES);
        let service = Self {
            root,
            read_capability_secret,
            budget_bytes: budget,
            forensic_log_dir: None,
            upload_temp_cleanup_schedule: Arc::new(Mutex::new(None)),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CACHE_UPLOADS)),
            namespace_file_limit: MAX_CACHE_NAMESPACE_FILES,
            tenant_namespace_limit: MAX_CACHE_TENANT_NAMESPACES,
        };
        service.cleanup_stale_upload_temps(true)?;
        Ok(service)
    }

    fn create_read_capability(&self, namespace: &str, id: &str, methods: u8) -> Result<String> {
        let expires_ms =
            now_unix_millis()?.saturating_add(READ_CAPABILITY_TTL_SECS.saturating_mul(1000));
        self.create_read_capability_until(namespace, id, methods, expires_ms)
    }

    fn create_read_capability_until(
        &self,
        namespace: &str,
        id: &str,
        methods: u8,
        expires_ms: u64,
    ) -> Result<String> {
        validate_cache_id(id)?;
        if !valid_capability_namespace(namespace) || !matches!(methods, 1 | 3) {
            anyhow::bail!("invalid cache read-capability scope");
        }
        let mut payload = Vec::with_capacity(12 + namespace.len() + id.len());
        payload.push(1); // token format version
        payload.push(methods); // bit 0 = GET, bit 1 = HEAD
        payload.extend_from_slice(&expires_ms.to_be_bytes());
        payload.extend_from_slice(&(namespace.len() as u16).to_be_bytes());
        payload.extend_from_slice(namespace.as_bytes());
        payload.extend_from_slice(id.as_bytes());
        let mac = hmac_sha256(
            &self.read_capability_secret,
            READ_CAPABILITY_DOMAIN,
            &payload,
        );
        payload.extend_from_slice(&mac);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload))
    }

    /// Validate a signed read URL against its exact resource, HTTP method, and
    /// expiry. The returned namespace is the only scope the URL may read.
    fn verify_read_capability(
        &self,
        token: &str,
        id: &str,
        method: &hyper::Method,
    ) -> Option<String> {
        validate_cache_id(id).ok()?;
        let method_bit = match *method {
            hyper::Method::GET => 1,
            hyper::Method::HEAD => 2,
            _ => return None,
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .ok()?;
        if bytes.len() < 12 + 64 + 32 {
            return None;
        }
        let payload_len = bytes.len().checked_sub(32)?;
        let (payload, supplied_mac) = bytes.split_at(payload_len);
        let expected_mac = hmac_sha256(
            &self.read_capability_secret,
            READ_CAPABILITY_DOMAIN,
            payload,
        );
        let different = supplied_mac
            .iter()
            .zip(expected_mac)
            .fold(0u8, |different, (supplied, expected)| {
                different | (supplied ^ expected)
            });
        if different != 0 || payload[0] != 1 {
            return None;
        }
        let methods = payload[1];
        if !matches!(methods, 1 | 3) || methods & method_bit == 0 {
            return None;
        }
        let expires_ms = u64::from_be_bytes(payload[2..10].try_into().ok()?);
        let namespace_len = u16::from_be_bytes(payload[10..12].try_into().ok()?) as usize;
        let id_start = 12usize.checked_add(namespace_len)?;
        let expected_payload_len = id_start.checked_add(64)?;
        if payload.len() != expected_payload_len {
            return None;
        }
        let namespace = std::str::from_utf8(payload.get(12..id_start)?).ok()?;
        let scoped_id = std::str::from_utf8(payload.get(id_start..)?).ok()?;
        if !valid_capability_namespace(namespace) || scoped_id != id {
            return None;
        }
        let now_ms = now_unix_millis().ok()?;
        if expires_ms <= now_ms
            || expires_ms > now_ms.saturating_add(READ_CAPABILITY_TTL_SECS.saturating_mul(1000))
        {
            return None;
        }
        Some(namespace.to_owned())
    }

    fn tenant_root(&self, namespace: Option<&str>) -> PathBuf {
        namespace.map_or_else(
            || self.root.clone(),
            |namespace| self.root.join("tenants").join(namespace),
        )
    }

    fn ensure_tenant(&self, namespace: &str) -> Result<()> {
        if !valid_capability_namespace(namespace) {
            anyhow::bail!("invalid cache namespace");
        }
        let root = self.tenant_root(Some(namespace));
        let _registry = lock_cache_tenant_registry(&self.root)?;
        prune_stale_sessions(&self.root.join("sessions"), std::time::SystemTime::now());
        let tenants = self.root.join("tenants");
        let cache_root_directory = open_cache_directory(&self.root, "cache root")?;
        let tenants_directory = open_or_create_cache_directory_child_with_sync(
            &cache_root_directory,
            std::ffi::OsStr::new("tenants"),
            &tenants,
            &self.root,
            &mut sync_directory,
            "cache tenants directory",
        )?;
        if open_cache_directory_child(
            &tenants_directory,
            std::ffi::OsStr::new(namespace),
            &root,
            "cache tenant",
        )?
        .is_none()
        {
            let mut count = count_tenant_directories(&tenants)?;
            if count >= self.tenant_namespace_limit {
                self.reclaim_idle_tenant_namespaces(namespace)?;
                count = count_tenant_directories(&tenants)?;
            }
            if count >= self.tenant_namespace_limit {
                return Err(anyhow::Error::new(CacheNamespaceLimit));
            }
        }
        let tenant_directory = open_or_create_cache_directory_child_with_sync(
            &tenants_directory,
            std::ffi::OsStr::new(namespace),
            &root,
            &tenants,
            &mut sync_directory,
            "cache tenant directory",
        )?;
        for name in ["blobs", "entries", "reservations"] {
            open_or_create_cache_directory_child_with_sync(
                &tenant_directory,
                std::ffi::OsStr::new(name),
                &root.join(name),
                &root,
                &mut sync_directory,
                "cache tenant subdirectory",
            )?;
        }
        self.refresh_fallback_activity_marker(namespace)?;
        Ok(())
    }

    /// Evict one old tenant only while holding the root registry lock. An
    /// exclusive activity-shard and namespace lock prove no route, stream,
    /// reservation transition, or upload can still use the candidate.
    fn reclaim_idle_tenant_namespaces(&self, keep: &str) -> Result<usize> {
        self.reclaim_idle_tenant_namespaces_with_hook(keep, |_, _| Ok(()))
    }

    fn reclaim_idle_tenant_namespaces_with_hook(
        &self,
        keep: &str,
        mut at_stage: impl FnMut(IdleTenantGcStage, &Path) -> Result<()>,
    ) -> Result<usize> {
        let root_directory = open_cache_directory(&self.root, "GHA cache root")?;
        let root_identity = crate::leftover_disk::filesystem_object_identity(&root_directory)
            .context("identify GHA cache root for tenant GC")?;
        let cache_root =
            std::fs::canonicalize(&self.root).context("resolve GHA cache root for tenant GC")?;
        let tenants = cache_root.join("tenants");
        let tenants_directory = open_cache_directory_child(
            &root_directory,
            std::ffi::OsStr::new("tenants"),
            &tenants,
            "GHA cache tenants directory",
        )?
        .context("GHA cache tenants directory is missing")?;
        let tenants_identity = crate::leftover_disk::filesystem_object_identity(&tenants_directory)
            .context("identify pinned GHA tenants directory")?;
        verify_cache_mount_identity(&root_identity, &tenants_identity, "GHA tenants directory")?;
        let candidates = crate::leftover_disk::filesystem_directory_children_under(
            &cache_root,
            &tenants,
            &root_identity,
        )
        .context("scan pinned GHA cache tenant directories")?;
        let now = std::time::SystemTime::now();
        for candidate in candidates {
            let Some(namespace) = candidate.name.to_str() else {
                continue;
            };
            if namespace == keep || !valid_capability_namespace(namespace) {
                continue;
            }
            let tenant_path = tenants.join(&candidate.name);
            let (pinned_tenant, tenant_identity) =
                crate::leftover_disk::filesystem_pin_directory_under(
                    &cache_root,
                    &tenant_path,
                    &root_identity,
                )
                .context("pin idle GHA tenant candidate")?;
            if tenant_identity != candidate.identity {
                anyhow::bail!("GHA tenant changed after secure inventory");
            }
            if cache_namespace_is_retained(&cache_root, namespace, now)? {
                continue;
            }
            let Some(_tenant_gc_guard) =
                try_lock_tenant_for_cache_gc(&cache_root, namespace, &tenant_path, &pinned_tenant)?
            else {
                continue;
            };
            let current_root = open_cache_directory(&cache_root, "GHA cache root")?;
            verify_pinned_cache_directory_identity(
                &root_directory,
                &current_root,
                "GHA cache root",
            )?;
            let current_tenants = open_cache_directory_child(
                &current_root,
                std::ffi::OsStr::new("tenants"),
                &tenants,
                "GHA cache tenants directory",
            )?
            .context("GHA cache tenants directory disappeared during tenant GC")?;
            verify_pinned_cache_directory_identity(
                &tenants_directory,
                &current_tenants,
                "GHA tenants directory",
            )?;
            let current_tenant = open_cache_directory_child(
                &current_tenants,
                &candidate.name,
                &tenant_path,
                "GHA tenant candidate",
            )?
            .context("GHA tenant candidate disappeared during tenant GC")?;
            verify_pinned_tenant_identity(&pinned_tenant, &current_tenant)?;

            let admission_directory = open_pinned_cache_admission_directory(
                &root_directory,
                &cache_root,
                namespace,
                &root_identity,
            )?;
            if let Some(admission_directory) = admission_directory.as_ref() {
                let current_admission_parent = open_cache_directory_child(
                    &current_root,
                    std::ffi::OsStr::new("admission-leases"),
                    &admission_directory.parent_path,
                    "cache admission root",
                )?
                .context("cache admission root disappeared during tenant GC")?;
                verify_pinned_cache_directory_identity(
                    &admission_directory.parent,
                    &current_admission_parent,
                    "cache admission root",
                )?;
                verify_cache_descendant_mount(
                    &root_identity,
                    &current_admission_parent,
                    "cache admission root",
                )?;
                let current_admission = open_cache_directory_child(
                    &current_admission_parent,
                    &admission_directory.name,
                    &admission_directory.path,
                    "cache admission namespace directory",
                )?
                .context("cache admission namespace directory disappeared during tenant GC")?;
                verify_pinned_cache_directory_identity(
                    &admission_directory.directory,
                    &current_admission,
                    "cache admission namespace directory",
                )?;
                verify_cache_descendant_mount(
                    &root_identity,
                    &current_admission,
                    "cache admission namespace directory",
                )?;
                at_stage(IdleTenantGcStage::BeforeAdmissionReap, &tenant_path)?;
                reap_stale_upload_admissions_at_pinned(
                    &admission_directory.directory,
                    &pinned_tenant,
                    &tenant_path,
                    &admission_directory.path,
                    &root_identity,
                )?;
                if namespace_has_active_admission_lease_at_pinned(
                    admission_directory,
                    &root_identity,
                )? {
                    continue;
                }
            }
            if namespace_has_reservations_at_pinned(&pinned_tenant, &tenant_path, &root_identity)?
                || namespace_has_live_upload_temp_at_pinned(
                    &pinned_tenant,
                    &tenant_path,
                    &root_identity,
                )?
            {
                continue;
            }
            if cache_namespace_is_retained(&cache_root, namespace, now)? {
                continue;
            }
            if let Some(admission_directory) = admission_directory.as_ref() {
                if !cache_directory_entry_names(
                    &admission_directory.directory,
                    "cache admission namespace directory",
                    &root_identity,
                )?
                .is_empty()
                {
                    continue;
                }
                if !crate::leftover_disk::remove_empty_directory_under_pinned_parent(
                    &cache_root,
                    &admission_directory.path,
                    &root_identity,
                    &admission_directory.parent_identity,
                    &admission_directory.parent,
                    &admission_directory.identity,
                    &admission_directory.directory,
                )? {
                    continue;
                }
            }

            let final_root = open_cache_directory(&cache_root, "GHA cache root")?;
            verify_pinned_cache_directory_identity(&root_directory, &final_root, "GHA cache root")?;
            let final_tenants = open_cache_directory_child(
                &final_root,
                std::ffi::OsStr::new("tenants"),
                &tenants,
                "GHA cache tenants directory",
            )?
            .context("GHA cache tenants directory disappeared before eviction")?;
            verify_pinned_cache_directory_identity(
                &tenants_directory,
                &final_tenants,
                "GHA tenants directory",
            )?;
            let final_tenant = open_cache_directory_child(
                &final_tenants,
                &candidate.name,
                &tenant_path,
                "GHA tenant candidate",
            )?
            .context("GHA tenant candidate disappeared before eviction")?;
            verify_pinned_tenant_identity(&pinned_tenant, &final_tenant)?;
            at_stage(IdleTenantGcStage::BeforeTenantQuarantine, &tenant_path)?;
            crate::leftover_disk::remove_dir_all_on_device_under_pinned_parent_with_pre_unlink(
                &cache_root,
                &tenant_path,
                root_identity.device,
                &root_identity,
                &tenants_identity,
                &tenants_directory,
                &tenant_identity,
                &pinned_tenant,
                &|_| Ok(()),
            )
            .context("evict idle GHA cache tenant namespace through quarantine")?;
            return Ok(1);
        }
        Ok(0)
    }

    fn refresh_fallback_activity_marker(&self, namespace: &str) -> Result<()> {
        if namespace.len() != 64 || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        if read_session(&self.root, namespace).is_some() {
            return Ok(());
        }
        let sessions = self.root.join("sessions");
        std::fs::create_dir_all(&sessions).context("create gha-cache sessions dir")?;
        let conflict = sessions.join(format!("{namespace}.conflict"));
        let isolated = sessions.join(format!("{namespace}.isolated"));
        let marker = if conflict.exists() {
            conflict
        } else {
            isolated
        };
        let first = std::fs::symlink_metadata(&marker).is_err();
        let mut marker_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&marker)
            .with_context(|| format!("refresh fallback cache marker {}", marker.display()))?;
        // Updating file mtime under the registry lock makes fallback tenant
        // retention a bounded idle TTL, without persisting the bearer token.
        marker_file
            .write_all(now_unix_millis()?.to_string().as_bytes())
            .with_context(|| format!("refresh fallback cache marker {}", marker.display()))?;
        marker_file
            .sync_all()
            .context("sync fallback cache marker")?;
        sync_directory(&sessions).context("sync fallback cache marker")?;
        if first {
            let message = format!(
                "gha-cache isolated fallback: no job session for token hash {namespace}; serving per-token namespace"
            );
            match &self.forensic_log_dir {
                Some(dir) => crate::slot_log::append_log_line(
                    dir,
                    crate::slot_log::DAEMON_LOG,
                    &format!("gha-cache pid={}", std::process::id()),
                    &message,
                ),
                None => eprintln!("Warning: {message}"),
            }
        }
        Ok(())
    }

    fn refresh_registered_session_activity_locked(&self, token: &str) -> Result<()> {
        let token_hash = cache_namespace(token);
        let Some(session) = read_session(&self.root, &token_hash) else {
            return Ok(());
        };
        replace_json_atomically(
            &self
                .root
                .join("sessions")
                .join(format!("{token_hash}.json")),
            &session.as_json(),
        )?;
        Ok(())
    }

    #[cfg(test)]
    async fn try_admit_upload(
        &self,
        namespace: Option<&str>,
        id: &str,
        attempt: &str,
        maximum_bytes: u64,
        reserved_bytes: Option<u64>,
        active_files: usize,
    ) -> Result<UploadAdmission> {
        self.try_admit_upload_with_scratch(
            namespace,
            id,
            attempt,
            maximum_bytes,
            reserved_bytes,
            0,
            active_files,
        )
        .await
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn try_admit_upload_with_scratch(
        &self,
        namespace: Option<&str>,
        id: &str,
        attempt: &str,
        maximum_bytes: u64,
        reserved_bytes: Option<u64>,
        scratch_bytes: u64,
        active_files: usize,
    ) -> Result<UploadAdmission> {
        let namespace_lock = lock_cache_namespace(self, namespace).await?;
        self.try_admit_upload_locked(
            namespace,
            id,
            attempt,
            maximum_bytes,
            reserved_bytes,
            scratch_bytes,
            active_files,
            &namespace_lock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn try_admit_upload_locked(
        &self,
        namespace: Option<&str>,
        id: &str,
        attempt: &str,
        maximum_bytes: u64,
        reserved_bytes: Option<u64>,
        scratch_bytes: u64,
        active_files: usize,
        _namespace_lock: &CacheNamespaceLock,
    ) -> Result<UploadAdmission> {
        let slot = self.upload_slots.clone().try_acquire_owned().map_err(|_| {
            anyhow::Error::new(CacheLockBusy).context("cache upload slots are full")
        })?;
        // Unknown-length streams grow this durable reservation before each
        // frame is written. Reserving the whole per-entry maximum here rejects
        // every such upload in a full cache, even when the body is empty.
        let reserved_bytes = reserved_bytes.unwrap_or(0).min(maximum_bytes);
        if reserved_bytes > self.budget_bytes || scratch_bytes > self.budget_bytes {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        validate_cache_id(id)?;
        if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("invalid cache admission attempt id");
        }
        let _budget_lock = lock_cache_budget_blocking(self, namespace)?;
        self.reap_stale_upload_admissions_locked(namespace)?;
        self.cleanup_json_temporary_files_locked(namespace)?;

        // Reserve root-wide scratch before evicting cached entries. If another
        // namespace owns the physical headroom, a failed admission must not
        // discard this namespace's cache as a side effect.
        let assembly_scratch = reserve_global_assembly_scratch(
            &self.root,
            self.budget_bytes,
            namespace,
            id,
            attempt,
            scratch_bytes,
        )?;

        let record = json!({
            "namespace": namespace.unwrap_or_default(),
            "id": id,
            "attempt": attempt,
            "reservedBytes": reserved_bytes,
            "scratchBytes": scratch_bytes,
            "reservedFiles": active_files,
        });
        let record_bytes = record.to_string().into_bytes();
        let lease_dir = cache_namespace_storage_dir(&self.root, namespace);
        let additional_files = active_files
            .saturating_add(1)
            .saturating_add(usize::from(!lease_dir.is_dir()));
        let (projected_bytes, projected_files) = self.evict_lru_for_capacity_locked(
            namespace,
            Some(id),
            reserved_bytes.saturating_add(record_bytes.len() as u64),
            additional_files,
            _namespace_lock,
        )?;
        if projected_bytes > self.budget_bytes {
            return Err(
                anyhow::Error::new(CacheLockBusy).context("cache namespace byte budget is full")
            );
        }
        if projected_files > self.namespace_file_limit {
            return Err(anyhow::Error::new(CacheNamespaceLimit));
        }

        std::fs::create_dir_all(&lease_dir).context("create cache admission lease directory")?;
        let lease_path = lease_dir.join(format!("{attempt}.json"));
        let mut lease_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease_path)
            .with_context(|| format!("create cache admission lease {}", lease_path.display()))?;
        if let Err(error) = rustix::fs::flock(
            &lease_file,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        ) {
            let _ = std::fs::remove_file(&lease_path);
            return Err(anyhow::Error::new(error).context("lock cache admission lease"));
        }
        if let Err(error) = lease_file
            .write_all(&record_bytes)
            .and_then(|()| lease_file.sync_all())
        {
            let _ = std::fs::remove_file(&lease_path);
            return Err(error).context("persist cache admission lease");
        }
        sync_directory(&lease_dir).context("sync cache admission lease directory")?;
        Ok(UploadAdmission {
            _slot: slot,
            cache_root: self.root.clone(),
            namespace: namespace.map(ToOwned::to_owned),
            id: id.to_owned(),
            attempt: attempt.to_owned(),
            namespace_lock_path: cache_namespace_lock_path(&self.root, namespace),
            lease_path,
            lease_file: Some(lease_file),
            maximum_bytes: maximum_bytes.min(self.budget_bytes),
            reserved_bytes,
            reserved_files: active_files,
            scratch_bytes,
            assembly_scratch,
        })
    }

    fn namespace_disk_usage(&self, namespace: Option<&str>) -> Result<(u64, usize)> {
        let tenant_root = self.tenant_root(namespace);
        let mut total_bytes = 0u64;
        let mut total_files = 0usize;
        if std::fs::symlink_metadata(&tenant_root).is_ok() {
            total_files = total_files.saturating_add(1);
        }
        for directory in ["blobs", "entries", "reservations", "uploads"] {
            scan_cache_files(
                &tenant_root.join(directory),
                directory == "blobs",
                &mut total_bytes,
                &mut total_files,
            )?;
        }
        Ok((total_bytes, total_files))
    }

    fn upload_admission_usage(&self, namespace: Option<&str>) -> Result<(u64, usize)> {
        let directory = cache_namespace_storage_dir(&self.root, namespace);
        let mut total_bytes = 0u64;
        let mut reserved_files = 1usize; // existing per-namespace lease directory
        let leases = match std::fs::read_dir(&directory) {
            Ok(leases) => leases,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
            Err(error) => return Err(error).context("scan cache admission leases"),
        };
        for lease in leases {
            let lease = lease.context("read cache admission lease")?;
            if lease.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(lease.path()) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache admission lease"),
            };
            let raw = std::fs::read(lease.path()).context("read cache admission lease")?;
            let record: Value =
                serde_json::from_slice(&raw).context("parse active cache admission lease")?;
            let Some(bytes) = record["reservedBytes"].as_u64() else {
                anyhow::bail!("active cache admission has no reserved byte count");
            };
            let Some(files) = record["reservedFiles"].as_u64() else {
                anyhow::bail!("active cache admission has no reserved file count");
            };
            total_bytes = total_bytes
                .saturating_add(bytes)
                .saturating_add(metadata.len());
            // Count both the durable admission record inode and the
            // in-flight data inode(s) it reserves.
            reserved_files = reserved_files.saturating_add(1).saturating_add(
                usize::try_from(files).context("cache admission file count exceeds usize")?,
            );
        }
        Ok((total_bytes, reserved_files))
    }

    fn reap_stale_upload_admissions_locked(&self, namespace: Option<&str>) -> Result<usize> {
        let directory = cache_namespace_storage_dir(&self.root, namespace);
        let leases = match std::fs::read_dir(&directory) {
            Ok(leases) => leases,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error).context("scan stale cache admission leases"),
        };
        let mut removed = 0;
        for lease in leases {
            let lease = lease.context("read stale cache admission lease")?;
            if lease.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lease.path())
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("open stale cache admission lease"),
            };
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Err(rustix::io::Errno::WOULDBLOCK) => continue,
                Err(error) => return Err(anyhow::Error::new(error).context("lock stale admission")),
                Ok(()) => {}
            }
            let record = std::fs::read(lease.path())
                .ok()
                .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok());
            if let Some((id, attempt)) = record.as_ref().and_then(|record| {
                let id = record["id"].as_str()?;
                let attempt = record["attempt"].as_str()?;
                (validate_cache_id(id).is_ok()
                    && attempt.len() == 32
                    && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then_some((id.to_owned(), attempt.to_owned()))
            }) {
                let temp = self
                    .tenant_root(namespace)
                    .join("blobs")
                    .join(format!(".{id}.{attempt}.tmp"));
                match std::fs::symlink_metadata(&temp) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        let Some(_temp_lock) = try_lock_upload_temp(&temp)? else {
                            // Keep the byte/count claim if a separate live
                            // temp lease still owns this exact inode.
                            continue;
                        };
                        match std::fs::remove_file(&temp) {
                            Ok(()) => removed += 1,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => {
                                return Err(error).context("remove abandoned admitted temp")
                            }
                        }
                    }
                    Ok(_) => continue,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).context("stat admitted temp during recovery"),
                }
            }
            match std::fs::remove_file(lease.path()) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove stale cache admission lease"),
            }
        }
        if removed > 0 {
            sync_directory(&directory).context("sync cache admission lease cleanup")?;
        }
        Ok(removed)
    }

    fn cleanup_json_temporary_files_locked(&self, namespace: Option<&str>) -> Result<usize> {
        let root = self.tenant_root(namespace);
        let mut removed = 0;
        for directory in ["entries", "reservations"] {
            removed += remove_json_temporary_files(&root.join(directory))?;
        }
        Ok(removed)
    }

    fn ensure_namespace_capacity_locked(
        &self,
        namespace: Option<&str>,
        new_files: usize,
        transition_headroom: usize,
        new_bytes: u64,
        namespace_lock: &CacheNamespaceLock,
    ) -> Result<()> {
        let _budget_lock = lock_cache_budget_blocking(self, namespace)?;
        self.reap_stale_upload_admissions_locked(namespace)?;
        self.cleanup_json_temporary_files_locked(namespace)?;
        let (projected_bytes, projected_files) = self.evict_lru_for_capacity_locked(
            namespace,
            None,
            new_bytes,
            new_files.saturating_add(transition_headroom),
            namespace_lock,
        )?;
        if projected_files > self.namespace_file_limit || projected_bytes > self.budget_bytes {
            return Err(anyhow::Error::new(CacheNamespaceLimit));
        }
        Ok(())
    }

    /// Caller serializes mutations for this namespace with its namespace lock
    /// and serializes capacity snapshots with the budget lock.
    fn evict_lru_for_capacity_locked(
        &self,
        namespace: Option<&str>,
        exclude_id: Option<&str>,
        additional_bytes: u64,
        additional_files: usize,
        _namespace_lock: &CacheNamespaceLock,
    ) -> Result<(u64, usize)> {
        let (disk_bytes, disk_files) = self.namespace_disk_usage(namespace)?;
        let (active_bytes, active_files) = self.upload_admission_usage(namespace)?;
        let mut projected_bytes = disk_bytes
            .saturating_add(active_bytes)
            .saturating_add(additional_bytes);
        let mut projected_files = disk_files
            .saturating_add(active_files)
            .saturating_add(additional_files);
        if projected_bytes <= self.budget_bytes && projected_files <= self.namespace_file_limit {
            return Ok((projected_bytes, projected_files));
        }

        let entries_dir = self.tenant_root(namespace).join("entries");
        let mut candidates: Vec<(u64, String, PathBuf)> = Vec::new();
        for file in std::fs::read_dir(&entries_dir).context("scan cache entries for admission")? {
            let file = file.context("read cache entry admission candidate")?;
            let path = file.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_cache_id(id).is_err() || exclude_id == Some(id) {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache entry admission candidate"),
            };
            let raw = match std::fs::read(&path) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("read cache entry admission candidate"),
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let (Some(key), Some(version)) = (entry["key"].as_str(), entry["version"].as_str())
            else {
                continue;
            };
            if entry["blob"].as_str() != Some(id) || entry_hash(key, version) != id {
                continue;
            }
            let accessed = cache_entry_access_ns(&entry, &metadata);
            candidates.push((accessed, id.to_owned(), path));
        }
        candidates.sort_by_key(|(accessed, id, _)| (*accessed, id.clone()));

        for (observed_access, id, entry_path) in candidates {
            if projected_bytes <= self.budget_bytes && projected_files <= self.namespace_file_limit
            {
                break;
            }
            // The caller already owns this namespace's exclusive lock. Lock
            // only the candidate shard here; reacquiring the namespace lock
            // would make every candidate look busy to its own admission.
            let Some(_entry_lock) = try_lock_cache_entry_file_at(&self.root, namespace, &id)?
            else {
                continue;
            };
            let entry_metadata = match std::fs::symlink_metadata(&entry_path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("restat cache admission candidate"),
            };
            let reservation_path = self.reservation_path(&id, namespace);
            match std::fs::symlink_metadata(&reservation_path) {
                Ok(_) => continue, // Preserve every live or unknown cache claim.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("check cache admission reservation"),
            }
            if namespace_has_admission_lease_for_id(&self.root, namespace, &id)? {
                continue;
            }
            let raw = match std::fs::read(&entry_path) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("reread cache admission candidate"),
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let (Some(key), Some(version)) = (entry["key"].as_str(), entry["version"].as_str())
            else {
                continue;
            };
            if entry["blob"].as_str() != Some(id.as_str())
                || entry_hash(key, version) != id
                || cache_entry_access_ns(&entry, &entry_metadata) != observed_access
            {
                continue; // A concurrent restore made this entry newer.
            }
            let blob_path = self.tenant_root(namespace).join("blobs").join(&id);
            let blob_metadata = match std::fs::symlink_metadata(&blob_path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache admission blob candidate"),
            };
            match std::fs::remove_file(&entry_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => continue,
            }
            let mut freed_bytes = entry_metadata.len();
            let mut freed_files = 1usize;
            match std::fs::remove_file(&blob_path) {
                Ok(()) => {
                    freed_bytes = freed_bytes.saturating_add(blob_metadata.len());
                    freed_files = freed_files.saturating_add(1);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    freed_files = freed_files.saturating_add(1);
                }
                Err(error) => return Err(error).context("remove evicted cache blob for admission"),
            }
            projected_bytes = projected_bytes.saturating_sub(freed_bytes);
            projected_files = projected_files.saturating_sub(freed_files);
        }
        Ok((projected_bytes, projected_files))
    }

    #[cfg(test)]
    fn staged_upload_usage(
        &self,
        namespace: Option<&str>,
        exclude_id: Option<&str>,
    ) -> Result<(u64, usize)> {
        let uploads = self.tenant_root(namespace).join("uploads");
        let mut total = 0u64;
        let mut count = 0usize;
        match std::fs::read_dir(&uploads) {
            Ok(directories) => {
                for directory in directories {
                    let directory = directory.context("read staged cache upload directory")?;
                    let Some(id) = directory.file_name().to_str().map(ToOwned::to_owned) else {
                        continue;
                    };
                    if validate_cache_id(&id).is_err() || exclude_id == Some(id.as_str()) {
                        continue;
                    }
                    let metadata = match std::fs::symlink_metadata(directory.path()) {
                        Ok(metadata) if metadata.file_type().is_dir() => metadata,
                        Ok(_) => continue,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error).context("stat staged cache upload directory");
                        }
                    };
                    let _ = metadata;
                    for file in std::fs::read_dir(directory.path())
                        .context("scan staged cache upload parts")?
                    {
                        let file = file.context("read staged cache upload part")?;
                        if upload_part_attempt(&file.file_name()).is_none() {
                            continue;
                        }
                        let metadata = match std::fs::symlink_metadata(file.path()) {
                            Ok(metadata) if metadata.file_type().is_file() => metadata,
                            Ok(_) => continue,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => {
                                return Err(error).context("stat staged cache upload part");
                            }
                        };
                        count = count.saturating_add(1);
                        total = total.saturating_add(metadata.len());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("scan staged cache upload directories"),
        }

        let blobs = self.tenant_root(namespace).join("blobs");
        match std::fs::read_dir(&blobs) {
            Ok(files) => {
                for file in files {
                    let file = file.context("read orphan cache blob for upload budget")?;
                    let file_name = file.file_name();
                    let Some(id) = cache_blob_id(&file_name) else {
                        continue;
                    };
                    if exclude_id == Some(id) {
                        continue;
                    }
                    let metadata = match std::fs::symlink_metadata(file.path()) {
                        Ok(metadata) if metadata.file_type().is_file() => metadata,
                        Ok(_) => continue,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error).context("stat orphan cache blob"),
                    };
                    let committed = self.read_entry(id, namespace).is_some_and(|entry| {
                        let (Some(key), Some(version)) =
                            (entry["key"].as_str(), entry["version"].as_str())
                        else {
                            return false;
                        };
                        entry["blob"].as_str() == Some(id) && entry_hash(key, version) == id
                    });
                    if !committed {
                        count = count.saturating_add(1);
                        total = total.saturating_add(metadata.len());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("scan orphan cache blobs for upload budget"),
        }
        Ok((total, count))
    }

    fn entry_path(&self, hash: &str, namespace: Option<&str>) -> PathBuf {
        self.tenant_root(namespace)
            .join("entries")
            .join(format!("{hash}.json"))
    }

    fn reservation_path(&self, id: &str, namespace: Option<&str>) -> PathBuf {
        self.tenant_root(namespace)
            .join("reservations")
            .join(format!("{id}.json"))
    }

    fn cache_id_index_path(&self, cache_id: u64, namespace: Option<&str>) -> PathBuf {
        self.tenant_root(namespace)
            .join("reservations")
            .join("by-id")
            .join(format!("{cache_id}.json"))
    }

    fn cache_entry_lock_path(&self, id: &str, namespace: Option<&str>) -> PathBuf {
        cache_entry_lock_path(&self.root, namespace, id)
    }

    fn read_entry(&self, hash: &str, namespace: Option<&str>) -> Option<Value> {
        let raw = std::fs::read(self.entry_path(hash, namespace)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Checkpoint a cache hit while holding the same namespace/entry locks as
    /// eviction. Persisting at most once per interval keeps the durable LRU
    /// signal useful across processes without syncing the manifest per read.
    fn mark_entry_accessed(&self, id: &str, namespace: Option<&str>) -> Result<bool> {
        let _lock = lock_cache_entry_blocking(self, id, namespace)?;
        self.mark_entry_accessed_at_locked(id, namespace, now_unix_nanos())
    }

    #[cfg(test)]
    fn mark_entry_accessed_at(&self, id: &str, namespace: Option<&str>, now: u64) -> Result<bool> {
        let _lock = lock_cache_entry_blocking(self, id, namespace)?;
        self.mark_entry_accessed_at_locked(id, namespace, now)
    }

    fn mark_entry_accessed_at_locked(
        &self,
        id: &str,
        namespace: Option<&str>,
        now: u64,
    ) -> Result<bool> {
        let path = self.entry_path(id, namespace);
        let Some(mut entry) = self.read_entry(id, namespace) else {
            return Ok(false);
        };
        let (Some(key), Some(version)) = (entry["key"].as_str(), entry["version"].as_str()) else {
            return Ok(false);
        };
        if entry["blob"].as_str() != Some(id) || entry_hash(key, version) != id {
            return Ok(false);
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("stat cache entry before access update"),
        };
        let previous = cache_entry_access_ns(&entry, &metadata);
        if previous <= now && now - previous < CACHE_ACCESS_CHECKPOINT_INTERVAL_NS {
            return Ok(true);
        }
        entry["last_accessed_ns"] = json!(now);
        replace_json_atomically(&path, &entry).context("persist cache entry access time")?;
        Ok(true)
    }

    /// Exact match first, then each restore key as a newest-wins prefix scan.
    fn lookup(
        &self,
        keys: &[&str],
        version: &str,
        namespace: Option<&str>,
    ) -> Result<Option<CacheHit>> {
        for (index, key) in keys.iter().enumerate() {
            let hash = entry_hash(key, version);
            let candidate = if self.read_entry(&hash, namespace).is_some() {
                Some(CacheHit {
                    hash,
                    key: (*key).to_owned(),
                })
            } else if index > 0 {
                self.prefix_scan(key, version, namespace)
            } else {
                None // the primary key is exact-only; only restore keys prefix-scan
            };
            if let Some(hit) = candidate
                && self.mark_entry_accessed(&hit.hash, namespace)?
            {
                return Ok(Some(hit));
            }
        }
        Ok(None)
    }

    fn prefix_scan(&self, key: &str, version: &str, namespace: Option<&str>) -> Option<CacheHit> {
        let mut best: Option<((u64, usize, String), CacheHit)> = None;
        let entries = std::fs::read_dir(self.tenant_root(namespace).join("entries")).ok()?;
        for file in entries.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = std::fs::read(&path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            if entry["version"].as_str() != Some(version) {
                continue;
            }
            let entry_key = entry["key"].as_str().unwrap_or_default();
            if !entry_key.starts_with(key) {
                continue;
            }
            let hash = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            if entry["blob"].as_str() != Some(hash.as_str())
                || entry_hash(entry_key, version) != hash
            {
                continue;
            }
            let created = entry["created_ms"].as_u64().unwrap_or(0);
            // Newest wins; ties broken by longest key (GitHub favors the most
            // specific restore key on equal timestamps in practice), then by
            // hash for determinism. Track the max directly: ranking through a
            // map keyed by a (created_ms, key_len) string let equal-rank
            // entries overwrite each other, so an arbitrary loser could win
            // depending on directory iteration order.
            let rank = (created, entry_key.len(), hash.clone());
            let hit = CacheHit {
                hash,
                key: entry_key.to_owned(),
            };
            match &best {
                Some((best_rank, _)) if *best_rank >= rank => {}
                _ => best = Some((rank, hit)),
            }
        }
        best.map(|(_, hit)| hit)
    }

    async fn enforce_budget(&self, namespace: Option<&str>) -> Result<()> {
        self.enforce_budget_after_scan(namespace, std::future::ready(()))
            .await
    }

    async fn enforce_budget_after_scan<F>(
        &self,
        namespace: Option<&str>,
        after_scan: F,
    ) -> Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        // Serialize each namespace's full snapshot/eviction pass. Without this,
        // concurrent enforcers can both act on the same stale byte total and
        // evict more entries than the configured budget requires.
        let _budget_lock = lock_cache_budget(self, namespace).await?;
        // Share the root-wide scheduler with request/startup cleanup. Budget
        // enforcement already walks blobs and entries, so a full reservation
        // and temp sweep here must not run once per completed upload.
        self.cleanup_stale_upload_temps(false)?;

        let blobs_dir = self.tenant_root(namespace).join("blobs");
        let mut total = 0u64;
        for file in std::fs::read_dir(&blobs_dir).context("scan cache blobs for budget")? {
            let file = file.context("read cache blob budget entry")?;
            let file_name = file.file_name();
            if cache_blob_id(&file_name).is_none() {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(file.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache blob for budget"),
            };
            if metadata.file_type().is_file() {
                total = total.saturating_add(metadata.len());
            }
        }

        let mut entries: Vec<(u64, String, PathBuf)> = Vec::new();
        for file in std::fs::read_dir(self.tenant_root(namespace).join("entries"))
            .context("scan entries")?
        {
            let file = file.context("read cache entry budget candidate")?;
            let path = file.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_cache_id(id).is_err() {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache entry budget candidate"),
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            let Ok(raw) = std::fs::read(&path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let (Some(key), Some(version)) = (entry["key"].as_str(), entry["version"].as_str())
            else {
                continue;
            };
            if entry["blob"].as_str() != Some(id) || entry_hash(key, version) != id {
                continue;
            }
            entries.push((
                cache_entry_access_ns(&entry, &metadata),
                id.to_owned(),
                path,
            ));
        }
        after_scan.await;
        if total <= self.budget_bytes {
            return Ok(());
        }
        entries.sort_by_key(|(last_accessed, id, _)| (*last_accessed, id.clone()));
        for (observed_access, id, entry_path) in entries {
            if total <= self.budget_bytes {
                break;
            }
            let Some(_lock) = try_lock_cache_entry_at(&self.root, namespace, &id)? else {
                self.schedule_upload_temp_retry()?;
                continue;
            };
            let entry_metadata = match std::fs::symlink_metadata(&entry_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("restat cache entry budget candidate"),
            };
            if !entry_metadata.file_type().is_file() {
                continue;
            }
            let reservation_path = self.reservation_path(&id, namespace);
            match std::fs::symlink_metadata(&reservation_path) {
                Ok(_) => continue, // Preserve entries with an unknown live claim.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("restat cache reservation budget guard"),
            }
            if namespace_has_admission_lease_for_id(&self.root, namespace, &id)? {
                continue;
            }
            let Ok(raw) = std::fs::read(&entry_path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let blob_path = self.tenant_root(namespace).join("blobs").join(&id);
            if entry["blob"].as_str() != Some(id.as_str())
                || cache_entry_access_ns(&entry, &entry_metadata) != observed_access
            {
                continue;
            }
            let blob_metadata = match std::fs::symlink_metadata(&blob_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => return Err(error).context("restat cache blob budget candidate"),
            };
            if !blob_metadata.file_type().is_file() {
                continue;
            }
            match std::fs::remove_file(&entry_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => continue,
            }
            match std::fs::remove_file(&blob_path) {
                Ok(()) => total = total.saturating_sub(blob_metadata.len()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    total = total.saturating_sub(blob_metadata.len());
                }
                Err(_) => {}
            }
        }
        Ok(())
    }

    fn schedule_upload_temp_retry(&self) -> Result<()> {
        let retry_at =
            std::time::Instant::now() + std::time::Duration::from_secs(UPLOAD_TEMP_BUSY_RETRY_SECS);
        let mut next_sweep = self
            .upload_temp_cleanup_schedule
            .lock()
            .map_err(|_| anyhow::anyhow!("cache temp cleanup schedule is poisoned"))?;
        *next_sweep = Some(
            next_sweep
                .as_ref()
                .map_or(retry_at, |scheduled| (*scheduled).min(retry_at)),
        );
        Ok(())
    }

    /// Remove upload hard links and abandoned canonical blobs left by a crash.
    /// Bodies hold per-attempt temp flocks; reservation/publication transitions
    /// hold the matching entry stripe. Reaping preserves committed entries and
    /// fresh reservations.
    fn cleanup_stale_upload_temps(&self, force: bool) -> Result<usize> {
        let now = std::time::Instant::now();
        {
            let mut next_sweep = self
                .upload_temp_cleanup_schedule
                .lock()
                .map_err(|_| anyhow::anyhow!("cache temp cleanup schedule is poisoned"))?;
            if !force && next_sweep.as_ref().is_some_and(|deadline| now < *deadline) {
                return Ok(0);
            }
            *next_sweep =
                Some(now + std::time::Duration::from_secs(UPLOAD_TEMP_SWEEP_INTERVAL_SECS));
        }

        // Remove stale assembly temps and source parts before reclaiming their
        // root-wide lease. If cleanup cannot prove removal, the lease remains
        // charged and future admissions fail closed.
        let cleanup = self
            .cleanup_all_upload_temps()
            .and_then(|outcome| cleanup_stale_global_assembly_scratch(&self.root).map(|_| outcome));
        match cleanup {
            Ok(outcome) => {
                if outcome.skipped_busy {
                    self.schedule_upload_temp_retry()?;
                }
                Ok(outcome.removed)
            }
            Err(error) => {
                let mut next_sweep = self
                    .upload_temp_cleanup_schedule
                    .lock()
                    .map_err(|_| anyhow::anyhow!("cache temp cleanup schedule is poisoned"))?;
                *next_sweep = None;
                Err(error)
            }
        }
    }

    fn cleanup_all_upload_temps(&self) -> Result<UploadTempCleanup> {
        let mut outcome = self.cleanup_upload_temps_at(&self.root, None)?;
        let tenants = self.root.join("tenants");
        for entry in std::fs::read_dir(&tenants).context("scan gha-cache tenants")? {
            let entry = entry.context("read gha-cache tenant entry")?;
            if entry
                .file_type()
                .context("stat gha-cache tenant entry")?
                .is_dir()
            {
                let namespace = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("gha-cache tenant name is not UTF-8"))?;
                outcome.merge(self.cleanup_upload_temps_at(&entry.path(), Some(&namespace))?);
            }
        }
        Ok(outcome)
    }

    fn cleanup_upload_temps_at(
        &self,
        tenant_root: &Path,
        namespace: Option<&str>,
    ) -> Result<UploadTempCleanup> {
        let blobs = tenant_root.join("blobs");
        let mut outcome = UploadTempCleanup::default();
        let blobs_exist = match std::fs::symlink_metadata(&blobs) {
            Ok(metadata) if metadata.file_type().is_dir() => true,
            Ok(_) => false, // Never follow a tenant-controlled link out of the cache root.
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("stat cache blobs for abandoned uploads"),
        };
        if blobs_exist {
            for entry in
                std::fs::read_dir(&blobs).context("scan cache blobs for abandoned uploads")?
            {
                let entry = entry.context("read cache blob directory entry")?;
                let path = entry.path();
                let file_name = entry.file_name();
                let (id, is_temp, attempt) =
                    if let Some((id, attempt)) = upload_temp_parts(&file_name) {
                        (id, true, Some(attempt))
                    } else if let Some(id) = cache_blob_id(&file_name) {
                        (id, false, None)
                    } else {
                        continue;
                    };
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error).context("stat possible abandoned cache upload");
                    }
                };
                if !metadata.file_type().is_file() {
                    continue;
                }
                let Some(_lock) = try_lock_cache_entry_at(&self.root, namespace, id)? else {
                    outcome.skipped_busy = true;
                    continue;
                };
                // Recheck after taking the lock. A regular-file check also
                // keeps cleanup from following unexpected symlinks.
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error).context("restat abandoned cache upload"),
                };
                if !metadata.file_type().is_file() {
                    continue;
                }
                if is_temp {
                    let Some(_temp_lock) = try_lock_upload_temp(&path)? else {
                        outcome.skipped_busy = true;
                        continue;
                    };
                    let metadata = match std::fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error).context("restat abandoned cache upload temp");
                        }
                    };
                    if !metadata.file_type().is_file() {
                        continue;
                    }
                    match std::fs::remove_file(&path) {
                        Ok(()) => outcome.removed += 1,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error).context("remove abandoned cache upload temp");
                        }
                    }
                    if let Some(attempt) = attempt {
                        clear_upload_attempt_if_matches(self, id, namespace, attempt)?;
                    }
                } else {
                    self.cleanup_orphan_blob_at(namespace, id, &path, &mut outcome)?;
                }
            }
        }

        self.cleanup_stale_reservations_at(tenant_root, namespace, &mut outcome)?;
        self.cleanup_orphan_upload_parts_at(tenant_root, namespace, &mut outcome)?;
        self.cleanup_orphan_v1_indexes_at(tenant_root, namespace)?;
        if outcome.removed_blob {
            sync_directory(&blobs).context("sync cache blob directory after orphan cleanup")?;
        }
        Ok(outcome)
    }

    fn cleanup_orphan_blob_at(
        &self,
        namespace: Option<&str>,
        id: &str,
        blob: &Path,
        outcome: &mut UploadTempCleanup,
    ) -> Result<()> {
        let tenant_root = self.tenant_root(namespace);
        if has_live_upload_temp_for_id(&tenant_root, id)? {
            outcome.skipped_busy = true;
            return Ok(());
        }
        let entry = self.entry_path(id, namespace);
        match std::fs::symlink_metadata(&entry) {
            Ok(_) => return Ok(()), // Any entry record owns the canonical blob.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("stat possible cache entry for orphan blob"),
        }

        let now_ms = now_unix_millis()?;
        let reservation_path = self.reservation_path(id, namespace);
        let expired = match read_reservation_record(self, id, namespace) {
            Ok(Some((record, modified_ms))) => {
                reservation_record_is_expired(&record, id, modified_ms, now_ms)
            }
            Ok(None) => true,
            Err(_) => reservation_file_is_expired(&reservation_path, now_ms)?,
        };
        if !expired {
            return Ok(()); // Fresh reservations and malformed records are preserved.
        }

        match std::fs::remove_file(&reservation_path) {
            Ok(()) => {
                if let Some(parent) = reservation_path.parent() {
                    sync_directory(parent)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove expired orphan reservation"),
        }
        match std::fs::remove_file(blob) {
            Ok(()) => {
                outcome.removed += 1;
                outcome.removed_blob = true;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove orphan cache blob"),
        }
        Ok(())
    }

    fn cleanup_stale_reservations_at(
        &self,
        tenant_root: &Path,
        namespace: Option<&str>,
        outcome: &mut UploadTempCleanup,
    ) -> Result<()> {
        let reservations = tenant_root.join("reservations");
        let files = match std::fs::read_dir(&reservations) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("scan cache reservations for cleanup"),
        };
        for file in files {
            let file = file.context("read cache reservation cleanup entry")?;
            let path = file.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_cache_id(id).is_err() {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat reservation cleanup entry"),
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            let Some(_lock) = try_lock_cache_entry_at(&self.root, namespace, id)? else {
                outcome.skipped_busy = true;
                continue;
            };
            if reconcile_inactive_upload_markers(self, id, namespace)? {
                outcome.skipped_busy = true;
                continue;
            }
            let entry = self.entry_path(id, namespace);
            let entry_exists = match std::fs::symlink_metadata(&entry) {
                Ok(_) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error).context("stat reservation entry reference"),
            };
            let now_ms = now_unix_millis()?;
            let (expired, record_is_valid) = match read_reservation_record(self, id, namespace) {
                Ok(Some((record, modified_ms))) => {
                    let valid = match record.get("protocol").and_then(Value::as_str) {
                        Some("v2") => parse_v2_reservation(record.clone(), id, modified_ms).is_ok(),
                        None => parse_v1_reservation(record.clone(), id).is_ok(),
                        Some(_) => false,
                    };
                    (
                        reservation_record_is_expired(&record, id, modified_ms, now_ms),
                        valid,
                    )
                }
                Ok(None) => continue,
                Err(_) => (reservation_file_is_expired(&path, now_ms)?, false),
            };
            if !expired && (!entry_exists || !record_is_valid) {
                continue;
            }
            if has_live_upload_temp_for_id(tenant_root, id)? {
                outcome.skipped_busy = true;
                continue;
            }

            let durable_scratch_attempt = read_reservation_record(self, id, namespace)
                .ok()
                .flatten()
                .and_then(|(record, modified_ms)| {
                    (record.get("protocol").and_then(Value::as_str) == Some("v2"))
                        .then(|| parse_v2_reservation(record, id, modified_ms).ok())
                        .flatten()
                })
                .and_then(|reservation| {
                    reservation
                        .assembly_scratch_attempt
                        .or(reservation.upload_attempt)
                });

            if let Some((record, _)) = read_reservation_record(self, id, namespace)?
                && record.get("protocol").is_none()
                && let Ok(reservation) = parse_v1_reservation(record, id)
                && let Some(namespace) = namespace
            {
                remove_v1_cache_id_index(self, namespace, reservation.cache_id, id)?;
            }

            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        sync_directory(parent)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("remove stale cache reservation"),
            }
            if entry_exists {
                remove_upload_parts_directory(tenant_root, id, outcome)?;
                if let Some(attempt) = durable_scratch_attempt {
                    release_global_assembly_scratch(&self.root, namespace, id, &attempt)?;
                }
                continue; // The committed entry keeps its blob.
            }
            remove_upload_parts_directory(tenant_root, id, outcome)?;
            if let Some(attempt) = durable_scratch_attempt {
                release_global_assembly_scratch(&self.root, namespace, id, &attempt)?;
            }
            let blob = tenant_root.join("blobs").join(id);
            match std::fs::symlink_metadata(&blob) {
                Ok(metadata) if metadata.file_type().is_file() => match std::fs::remove_file(&blob)
                {
                    Ok(()) => {
                        outcome.removed += 1;
                        outcome.removed_blob = true;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).context("remove stale reservation blob"),
                },
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("stat stale reservation blob"),
            }
        }
        Ok(())
    }

    /// Remove completed range/block parts after their claim disappeared or
    /// expired. Fresh claims keep their parts for the later commit request;
    /// live body streams keep both their claim and parts through the per-temp
    /// flock checked while holding the stable entry shard.
    fn cleanup_orphan_upload_parts_at(
        &self,
        tenant_root: &Path,
        namespace: Option<&str>,
        outcome: &mut UploadTempCleanup,
    ) -> Result<()> {
        let uploads = tenant_root.join("uploads");
        let directories = match std::fs::read_dir(&uploads) {
            Ok(directories) => directories,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("scan cache upload parts"),
        };
        for directory in directories {
            let directory = directory.context("read cache upload parts directory")?;
            let Some(id) = directory.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if validate_cache_id(&id).is_err() {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(directory.path()) {
                Ok(metadata) if metadata.file_type().is_dir() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat cache upload parts directory"),
            };
            let _ = metadata;
            let Some(_lock) = try_lock_cache_entry_at(&self.root, namespace, &id)? else {
                outcome.skipped_busy = true;
                continue;
            };
            if has_live_upload_temp_for_id(tenant_root, &id)? {
                outcome.skipped_busy = true;
                continue;
            }
            let entry = self.entry_path(&id, namespace);
            let entry_exists = match std::fs::symlink_metadata(&entry) {
                Ok(_) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error).context("stat cache entry for upload parts"),
            };
            let now_ms = now_unix_millis()?;
            let reservation = match read_reservation_record(self, &id, namespace) {
                Ok(Some((record, modified_ms))) => {
                    let fresh = !reservation_record_is_expired(&record, &id, modified_ms, now_ms);
                    let referenced = if fresh {
                        match record.get("protocol").and_then(Value::as_str) {
                            Some("v2") => parse_v2_reservation(record, &id, modified_ms).ok().map(
                                |reservation| {
                                    reservation
                                        .upload_blocks
                                        .into_iter()
                                        .map(|block| (true, block.attempt))
                                        .collect::<HashSet<_>>()
                                },
                            ),
                            None => parse_v1_reservation(record, &id).ok().map(|reservation| {
                                reservation
                                    .chunks
                                    .into_iter()
                                    .map(|chunk| (false, chunk.attempt))
                                    .collect::<HashSet<_>>()
                            }),
                            Some(_) => None,
                        }
                    } else {
                        None
                    };
                    (fresh, referenced)
                }
                Ok(None) => (false, None),
                Err(_) => (
                    !reservation_file_is_expired(&self.reservation_path(&id, namespace), now_ms)?,
                    None,
                ),
            };
            let (fresh_claim, referenced_parts) = reservation;
            if fresh_claim && !entry_exists {
                let Some(referenced_parts) = referenced_parts else {
                    continue; // Preserve parts when a fresh claim cannot be parsed.
                };
                for part in std::fs::read_dir(directory.path())
                    .context("scan unreferenced cache upload parts")?
                {
                    let part = part.context("read unreferenced cache upload part")?;
                    let part_name = part.file_name();
                    let Some((v2, attempt)) = upload_part_attempt(&part_name) else {
                        continue;
                    };
                    if referenced_parts.contains(&(v2, attempt.to_owned())) {
                        continue;
                    }
                    let part_path = part.path();
                    let metadata = match std::fs::symlink_metadata(&part_path) {
                        Ok(metadata) if metadata.file_type().is_file() => metadata,
                        Ok(_) => continue,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error).context("stat unreferenced upload part"),
                    };
                    let _ = metadata;
                    match std::fs::remove_file(&part_path) {
                        Ok(()) => outcome.removed += 1,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error).context("remove unreferenced upload part"),
                    }
                }
                continue;
            }
            remove_upload_parts_directory(tenant_root, &id, outcome)?;
        }
        Ok(())
    }

    /// A process can die after allocating a numeric v1 id but before
    /// publishing its reservation. Reap that side-index entry only after
    /// taking the exact entry shard and confirming the reservation is absent.
    fn cleanup_orphan_v1_indexes_at(
        &self,
        tenant_root: &Path,
        namespace: Option<&str>,
    ) -> Result<()> {
        let indexes = tenant_root.join("reservations").join("by-id");
        let files = match std::fs::read_dir(&indexes) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("scan v1 cache-id indexes"),
        };
        for file in files {
            let file = file.context("read v1 cache-id index")?;
            if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(file.path()) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("stat v1 cache-id index"),
            };
            let _ = metadata;
            let raw = std::fs::read(file.path()).context("read v1 cache-id index")?;
            let value: Value = match serde_json::from_slice(&raw) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Some(id) = value["entryHash"].as_str() else {
                continue;
            };
            if validate_cache_id(id).is_err() {
                continue;
            }
            let Some(_lock) = try_lock_cache_entry_at(&self.root, namespace, id)? else {
                continue;
            };
            match std::fs::symlink_metadata(self.reservation_path(id, namespace)) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("recheck reservation for v1 cache id"),
            }
            match std::fs::remove_file(file.path()) {
                Ok(()) => {
                    if let Some(parent) = file.path().parent() {
                        sync_directory(parent)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove orphan v1 cache-id index"),
            }
        }
        Ok(())
    }

    fn blob_path_for(&self, entry: &Value, namespace: Option<&str>) -> PathBuf {
        self.tenant_root(namespace)
            .join("blobs")
            .join(entry["blob"].as_str().unwrap_or_default())
    }
}

struct Ctx {
    service: Arc<CacheService>,
    public_base: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct V1Reservation {
    key: String,
    version: String,
    cache_id: u64,
    expected_size: Option<u64>,
    chunks: Vec<V1UploadChunk>,
    commit_attempt: Option<String>,
    active_uploads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct V1UploadChunk {
    start: u64,
    end: u64,
    attempt: String,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct V2Reservation {
    key: String,
    version: String,
    upload_nonce: String,
    block_id_bytes: Option<usize>,
    upload_attempt: Option<String>,
    upload_blocks: Vec<V2UploadBlock>,
    active_uploads: Vec<String>,
    assembly_scratch_attempt: Option<String>,
    assembly_scratch_bytes: u64,
    updated_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct V2UploadBlock {
    block_id: String,
    attempt: String,
    size: u64,
    sha256: String,
}

impl V2Reservation {
    fn as_json(&self) -> Value {
        json!({
            "protocol": "v2",
            "key": self.key,
            "version": self.version,
            "uploadNonce": self.upload_nonce,
            "blockIdBytes": self.block_id_bytes,
            "uploadAttempt": self.upload_attempt,
            "uploadBlocks": self.upload_blocks,
            "activeUploads": self.active_uploads,
            "assemblyScratchAttempt": self.assembly_scratch_attempt,
            "assemblyScratchBytes": self.assembly_scratch_bytes,
            "updatedMs": self.updated_ms,
        })
    }

    fn persist(&self, service: &CacheService, id: &str, namespace: &str) -> Result<()> {
        replace_json_atomically(
            &service.reservation_path(id, Some(namespace)),
            &self.as_json(),
        )
    }
}

#[derive(Debug, Default)]
struct UploadTempCleanup {
    removed: usize,
    removed_blob: bool,
    skipped_busy: bool,
}

struct UploadAdmission {
    _slot: tokio::sync::OwnedSemaphorePermit,
    cache_root: PathBuf,
    namespace: Option<String>,
    id: String,
    attempt: String,
    namespace_lock_path: PathBuf,
    lease_path: PathBuf,
    lease_file: Option<std::fs::File>,
    maximum_bytes: u64,
    reserved_bytes: u64,
    reserved_files: usize,
    scratch_bytes: u64,
    assembly_scratch: Option<CacheAssemblyScratchLease>,
}

impl UploadAdmission {
    fn retain_assembly_scratch(&mut self) -> Result<()> {
        if let Some(mut scratch) = self.assembly_scratch.take() {
            scratch.retain_durable()?;
        }
        Ok(())
    }

    /// The namespace guard serializes this active-to-durable handoff with all
    /// other admission snapshots, including requests in other daemon processes.
    fn release_under(&mut self, _namespace: &CacheNamespaceLock) -> Result<()> {
        match std::fs::remove_file(&self.lease_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove durable cache admission lease"),
        }
        if let Some(mut scratch) = self.assembly_scratch.take() {
            scratch.release()?;
        }
        drop(self.lease_file.take());
        Ok(())
    }

    fn reserve_at_least(&mut self, service: &CacheService, requested_bytes: u64) -> Result<()> {
        if requested_bytes <= self.reserved_bytes {
            return Ok(());
        }
        if requested_bytes > self.maximum_bytes {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        let namespace = self.namespace.as_deref();
        let _namespace_lock = lock_cache_namespace_blocking(&self.cache_root, namespace)?;
        let _budget_lock = lock_cache_budget_blocking(service, namespace)?;
        service.reap_stale_upload_admissions_locked(namespace)?;
        service.cleanup_json_temporary_files_locked(namespace)?;

        let current_file_len = self
            .lease_file
            .as_ref()
            .context("cache upload admission lease was released")?
            .metadata()
            .context("stat active cache admission lease")?
            .len();
        let record = json!({
            "namespace": namespace.unwrap_or_default(),
            "id": self.id.as_str(),
            "attempt": self.attempt.as_str(),
            "reservedBytes": requested_bytes,
            "scratchBytes": self.scratch_bytes,
            "reservedFiles": self.reserved_files,
        });
        let record_bytes = record.to_string().into_bytes();
        let record_growth = (record_bytes.len() as u64).saturating_sub(current_file_len);
        let projected_delta = requested_bytes
            .saturating_sub(self.reserved_bytes)
            .saturating_add(record_growth);
        let (projected_bytes, projected_files) = service.evict_lru_for_capacity_locked(
            namespace,
            Some(&self.id),
            projected_delta,
            0,
            &_namespace_lock,
        )?;
        if projected_bytes > service.budget_bytes {
            return Err(
                anyhow::Error::new(CacheLockBusy).context("cache namespace byte budget is full")
            );
        }
        if projected_files > service.namespace_file_limit {
            return Err(anyhow::Error::new(CacheNamespaceLimit));
        }

        let lease_file = self
            .lease_file
            .as_mut()
            .context("cache upload admission lease was released")?;
        lease_file
            .set_len(0)
            .context("truncate cache admission lease")?;
        lease_file
            .seek(std::io::SeekFrom::Start(0))
            .context("seek cache admission lease")?;
        lease_file
            .write_all(&record_bytes)
            .and_then(|()| lease_file.sync_all())
            .context("grow cache admission lease")?;
        self.reserved_bytes = requested_bytes;
        Ok(())
    }
}

impl Drop for UploadAdmission {
    fn drop(&mut self) {
        // Cancellation is serialized when possible. If the caller still owns
        // this namespace lock, leave the lease for the next locked reaper.
        if let Ok(file) = open_cache_entry_lock(&self.namespace_lock_path)
            && rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_ok()
        {
            let _ = std::fs::remove_file(&self.lease_path);
        }
        drop(self.assembly_scratch.take());
        drop(self.lease_file.take());
    }
}

struct CacheAssemblyScratchLease {
    cache_root: PathBuf,
    path: PathBuf,
    namespace: Option<String>,
    id: String,
    attempt: String,
    file: Option<std::fs::File>,
}

impl CacheAssemblyScratchLease {
    fn retain_durable(&mut self) -> Result<()> {
        let _budget_lock = lock_cache_assembly_scratch_blocking(&self.cache_root)?;
        let file = self
            .file
            .as_mut()
            .context("assembly scratch lease is closed")?;
        // The lease record was synced before the temp could be created. Its
        // identity and byte count remain valid after the flock closes, so the
        // active-to-durable handoff needs no crash-vulnerable rewrite.
        file.sync_all()
            .context("sync durable assembly scratch lease")?;
        if let Some(parent) = self.path.parent() {
            sync_directory(parent).context("sync durable assembly scratch lease")?;
        }
        drop(self.file.take());
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        let _budget_lock = lock_cache_assembly_scratch_blocking(&self.cache_root)?;
        let artifacts = assembly_scratch_artifacts(
            &self.cache_root,
            self.namespace.as_deref(),
            &self.id,
            &self.attempt,
        )?;
        if artifacts.temporary_assembly || artifacts.canonical_with_sources {
            anyhow::bail!("cannot release assembly scratch while duplicate bytes remain");
        }
        sync_assembly_scratch_artifact_directories(
            &self.cache_root,
            self.namespace.as_deref(),
            &self.id,
        )?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove assembly scratch lease"),
        }
        if let Some(parent) = self.path.parent() {
            sync_directory(parent).context("sync assembly scratch lease removal")?;
        }
        drop(self.file.take());
        Ok(())
    }
}

impl Drop for CacheAssemblyScratchLease {
    fn drop(&mut self) {
        // Cancellation cannot synchronously prove deletion durability. Leave
        // the durable claim for the next locked sweep, which syncs artifact
        // parent directories before reclaiming its bytes.
        drop(self.file.take());
    }
}

impl UploadTempCleanup {
    fn merge(&mut self, other: Self) {
        self.removed += other.removed;
        self.removed_blob |= other.removed_blob;
        self.skipped_busy |= other.skipped_busy;
    }
}

/// Stable sharded per-entry lock. The inode is intentionally retained after
/// use so reservation recovery, upload, finalize, and temp cleanup always
/// contend on the same flock even when reservation JSON changes. There are
/// exactly [`CACHE_ENTRY_LOCK_SHARDS`] possible lock files for the cache root.
struct CacheEntryLock {
    // Fields drop in declaration order: release the entry shard before its
    // namespace lock, preserving the reverse of namespace -> entry acquire.
    _file: std::fs::File,
    _namespace: CacheNamespaceLock,
}

struct CacheNamespaceLock {
    _file: std::fs::File,
}

struct CacheActivityLock {
    _file: std::fs::File,
}

struct CacheTenantGcLocks {
    // Release in reverse acquisition order: namespace, then activity.
    _namespace: CacheNamespaceLock,
    _activity: CacheActivityLock,
}

struct PinnedCacheAdmissionDirectory {
    parent: std::fs::File,
    parent_path: PathBuf,
    parent_identity: crate::leftover_disk::FilesystemDirectoryIdentity,
    name: std::ffi::OsString,
    path: PathBuf,
    directory: std::fs::File,
    identity: crate::leftover_disk::FilesystemDirectoryIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleTenantGcStage {
    BeforeAdmissionReap,
    BeforeTenantQuarantine,
}

/// Keeps a GHA tenant unavailable to requests and tenant transitions while
/// generic cache GC removes its pinned directory tree.
#[must_use = "hold the tenant GC guard through candidate deletion and sync"]
pub(crate) struct GhaTenantGcGuard {
    // Fields drop in declaration order. Release the namespace lock before the
    // activity lock, reversing the acquisition order used by tenant GC.
    _namespace: CacheNamespaceLock,
    _activity: CacheActivityLock,
    _tenant_directory: std::fs::File,
}

impl CacheEntryLock {
    fn namespace_guard(&self) -> &CacheNamespaceLock {
        &self._namespace
    }
}

struct CacheBudgetLock {
    _file: std::fs::File,
}

#[derive(Debug)]
struct CacheLockBusy;

impl std::fmt::Display for CacheLockBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache lock remained busy until its deadline")
    }
}

impl std::error::Error for CacheLockBusy {}

#[derive(Debug)]
struct CacheEntryTooLarge;

impl std::fmt::Display for CacheEntryTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("actual cache upload size exceeds the namespace byte budget")
    }
}

impl std::error::Error for CacheEntryTooLarge {}

#[derive(Debug)]
struct CacheProtocolConflict;

impl std::fmt::Display for CacheProtocolConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache reservation belongs to another protocol")
    }
}

impl std::error::Error for CacheProtocolConflict {}

#[derive(Debug)]
struct CacheKeyConflict;

impl std::fmt::Display for CacheKeyConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache key is already committed or reserved")
    }
}

impl std::error::Error for CacheKeyConflict {}

#[derive(Debug)]
struct CacheBadRequest;

impl std::fmt::Display for CacheBadRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid cache request")
    }
}

impl std::error::Error for CacheBadRequest {}

#[derive(Debug)]
struct CacheEntryNotRetained;

impl std::fmt::Display for CacheEntryNotRetained {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache entry was evicted to satisfy the namespace budget")
    }
}

impl std::error::Error for CacheEntryNotRetained {}

#[derive(Debug)]
struct CacheNamespaceLimit;

impl std::fmt::Display for CacheNamespaceLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache namespace reached its bounded metadata/file limit")
    }
}

impl std::error::Error for CacheNamespaceLimit {}

#[derive(Debug)]
struct CacheBlobNotFound;

impl std::fmt::Display for CacheBlobNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache blob was evicted or does not exist")
    }
}

impl std::error::Error for CacheBlobNotFound {}

#[derive(Debug)]
struct CacheRangeNotSatisfiable {
    total_size: u64,
}

impl std::fmt::Display for CacheRangeNotSatisfiable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("requested cache byte range is not satisfiable")
    }
}

impl std::error::Error for CacheRangeNotSatisfiable {}

fn scan_cache_files(
    directory: &Path,
    skip_upload_temps: bool,
    total_bytes: &mut u64,
    total_files: &mut usize,
) -> Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("scan {}", directory.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            *total_files = total_files.saturating_add(1);
            scan_cache_files(&path, skip_upload_temps, total_bytes, total_files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if skip_upload_temps && upload_temp_parts(&entry.file_name()).is_some() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("stat cache file {}", path.display()))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        *total_bytes = total_bytes.saturating_add(metadata.len());
        *total_files = total_files.saturating_add(1);
    }
    Ok(())
}

fn remove_json_temporary_files(directory: &Path) -> Result<usize> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("scan {}", directory.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            removed += remove_json_temporary_files(&path)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Some(candidate) = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        let Some((_stem, attempt)) = candidate.rsplit_once('.') else {
            continue;
        };
        if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
        }
    }
    if removed > 0 {
        sync_directory(directory)?;
    }
    Ok(removed)
}

fn cache_entry_lock_path(cache_root: &Path, namespace: Option<&str>, id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-entry-lock-v1\0");
    match namespace {
        Some(namespace) => {
            hasher.update([1u8]);
            hasher.update((namespace.len() as u64).to_be_bytes());
            hasher.update(namespace.as_bytes());
        }
        None => hasher.update([0u8]),
    }
    hasher.update(id.to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    let shard =
        (u16::from_be_bytes([digest[0], digest[1]]) as usize) & (CACHE_ENTRY_LOCK_SHARDS - 1);
    cache_root
        .join("entry-locks")
        .join(format!("{shard:03x}.lock"))
}

fn cache_namespace_lock_path(cache_root: &Path, namespace: Option<&str>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-namespace-lock-v1\0");
    match namespace {
        Some(namespace) => {
            hasher.update([1u8]);
            hasher.update((namespace.len() as u64).to_be_bytes());
            hasher.update(namespace.as_bytes());
        }
        None => hasher.update([0u8]),
    }
    let digest = hasher.finalize();
    let shard = usize::from(digest[0]) & (CACHE_NAMESPACE_LOCK_SHARDS - 1);
    cache_root
        .join("namespace-locks")
        .join(format!("{shard:02x}.lock"))
}

fn cache_activity_lock_path(cache_root: &Path, namespace: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-activity-lock-v1\0");
    hasher.update((namespace.len() as u64).to_be_bytes());
    hasher.update(namespace.as_bytes());
    let digest = hasher.finalize();
    let shard =
        (u16::from_be_bytes([digest[0], digest[1]]) as usize) & (CACHE_ACTIVITY_LOCK_SHARDS - 1);
    cache_root
        .join("activity-locks")
        .join(format!("{shard:03x}.lock"))
}

fn lock_cache_tenant_registry(cache_root: &Path) -> Result<std::fs::File> {
    let file = open_cache_entry_lock(&cache_root.join("tenant-locks/registry.lock"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("timed out waiting for cache tenant registry lock"));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock cache tenant registry"))
            }
        }
    }
}

fn cache_namespace_storage_dir(cache_root: &Path, namespace: Option<&str>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-namespace-storage-v1\0");
    match namespace {
        Some(namespace) => {
            hasher.update([1u8]);
            hasher.update((namespace.len() as u64).to_be_bytes());
            hasher.update(namespace.as_bytes());
        }
        None => hasher.update([0u8]),
    }
    cache_root
        .join("admission-leases")
        .join(hex(&hasher.finalize()))
}

fn count_tenant_directories(tenants: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in std::fs::read_dir(tenants).context("scan gha-cache tenant namespaces")? {
        let entry = entry.context("read gha-cache tenant namespace")?;
        if entry
            .file_type()
            .context("stat gha-cache tenant namespace")?
            .is_dir()
        {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

/// A valid token binding retains each namespace in its restore chain. A
/// fallback marker retains its per-token tenant until the marker's idle TTL.
fn cache_namespace_is_retained(
    cache_root: &Path,
    namespace: &str,
    now: std::time::SystemTime,
) -> Result<bool> {
    let sessions = cache_root.join("sessions");
    let files = match std::fs::read_dir(&sessions) {
        Ok(files) => files,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("scan cache session registry for tenant GC"),
    };
    for file in files {
        let file = file.context("read cache session registry entry")?;
        let path = file.path();
        let Some(token_hash) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if token_hash.len() != 64 || !token_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if read_session(cache_root, token_hash).is_some_and(|session| {
            session
                .identity
                .namespaces()
                .iter()
                .any(|value| value == namespace)
        }) {
            return Ok(true);
        }
    }
    if namespace.len() != 64 || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(false);
    }
    for suffix in ["isolated", "conflict"] {
        let path = sessions.join(format!("{namespace}.{suffix}"));
        let recent = std::fs::symlink_metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() < SESSION_TTL_SECS);
        if recent {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Identify committed entries that still have a live or unknown upload claim.
/// Callers hold the namespace lock and have reaped unlocked leases, so every
/// remaining JSON lease must be treated as active until the owner releases it.
fn namespace_has_admission_lease_for_id(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
) -> Result<bool> {
    let directory = cache_namespace_storage_dir(cache_root, namespace);
    for entry in match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("scan cache admission claims for entry"),
    } {
        let entry = entry.context("read cache admission claim for entry")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("stat cache admission claim for entry"),
        }
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read cache admission claim for entry"),
        };
        let record: Value = match serde_json::from_slice(&raw) {
            Ok(record) => record,
            Err(_) => return Ok(true),
        };
        let Some(claimed_id) = record["id"].as_str() else {
            return Ok(true);
        };
        if claimed_id == id {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cache_budget_lock_path(cache_root: &Path, namespace: Option<&str>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-budget-lock-v1\0");
    match namespace {
        Some(namespace) => {
            hasher.update([1u8]);
            hasher.update((namespace.len() as u64).to_be_bytes());
            hasher.update(namespace.as_bytes());
        }
        None => hasher.update([0u8]),
    }
    let digest = hasher.finalize();
    let shard = usize::from(digest[0]) & (CACHE_BUDGET_LOCK_SHARDS - 1);
    cache_root
        .join("budget-locks")
        .join(format!("{shard:02x}.lock"))
}

fn open_cache_entry_lock(path: &Path) -> Result<std::fs::File> {
    let parent = path.parent().context("cache entry lock has no parent")?;
    std::fs::create_dir_all(parent).context("create cache entry lock directory")?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open cache entry lock {}", path.display()))
}

async fn lock_cache_entry(
    service: &CacheService,
    id: &str,
    namespace: &str,
) -> Result<CacheEntryLock> {
    validate_cache_id(id)?;
    let namespace_guard = lock_cache_namespace(service, Some(namespace)).await?;
    let path = service.cache_entry_lock_path(id, Some(namespace));
    let file = open_cache_entry_lock(&path)?;
    let file = lock_file_with_deadline(
        file,
        std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS),
        "cache entry",
    )
    .await?;
    Ok(CacheEntryLock {
        _namespace: namespace_guard,
        _file: file,
    })
}

async fn lock_cache_namespace(
    service: &CacheService,
    namespace: Option<&str>,
) -> Result<CacheNamespaceLock> {
    let path = cache_namespace_lock_path(&service.root, namespace);
    let file = open_cache_entry_lock(&path)?;
    let file = lock_file_with_deadline(
        file,
        std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS),
        "cache namespace",
    )
    .await?;
    Ok(CacheNamespaceLock { _file: file })
}

async fn lock_cache_activity(cache_root: &Path, namespace: &str) -> Result<CacheActivityLock> {
    let file = open_cache_entry_lock(&cache_activity_lock_path(cache_root, namespace))?;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared) {
            Ok(()) => return Ok(CacheActivityLock { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(anyhow::Error::new(CacheLockBusy)
                        .context("timed out waiting for cache activity lock"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => return Err(anyhow::Error::new(error).context("lock cache activity")),
        }
    }
}

fn try_lock_cache_activity_at(
    cache_root: &Path,
    namespace: &str,
) -> Result<Option<CacheActivityLock>> {
    let file = open_cache_entry_lock(&cache_activity_lock_path(cache_root, namespace))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(CacheActivityLock { _file: file })),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache activity")),
    }
}

fn try_lock_cache_activity_shared_at(
    cache_root: &Path,
    namespace: &str,
) -> Result<Option<CacheActivityLock>> {
    let file = open_cache_entry_lock(&cache_activity_lock_path(cache_root, namespace))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared) {
        Ok(()) => Ok(Some(CacheActivityLock { _file: file })),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache activity shared")),
    }
}

fn try_lock_tenant_gc_locks_with(
    cache_root: &Path,
    namespace: &str,
    after_activity_lock: impl FnOnce() -> Result<()>,
) -> Result<Option<CacheTenantGcLocks>> {
    if !valid_capability_namespace(namespace) {
        anyhow::bail!("invalid GHA cache namespace for tenant GC");
    }
    let Some(activity) = try_lock_cache_activity_at(cache_root, namespace)? else {
        return Ok(None);
    };
    after_activity_lock()?;
    let Some(namespace_lock) = try_lock_cache_namespace_at(cache_root, Some(namespace))? else {
        return Ok(None);
    };
    Ok(Some(CacheTenantGcLocks {
        _namespace: namespace_lock,
        _activity: activity,
    }))
}

fn try_lock_tenant_gc_locks(
    cache_root: &Path,
    namespace: &str,
) -> Result<Option<CacheTenantGcLocks>> {
    try_lock_tenant_gc_locks_with(cache_root, namespace, || Ok(()))
}

fn verify_pinned_tenant_identity(
    pinned_tenant: &std::fs::File,
    opened_tenant: &std::fs::File,
) -> Result<()> {
    verify_pinned_cache_directory_identity(pinned_tenant, opened_tenant, "GHA tenant candidate")
}

fn verify_pinned_cache_directory_identity(
    pinned: &std::fs::File,
    opened: &std::fs::File,
    label: &str,
) -> Result<()> {
    if !pinned
        .metadata()
        .with_context(|| format!("inspect pinned {label}"))?
        .is_dir()
    {
        anyhow::bail!("pinned {label} is not a directory");
    }
    let pinned_identity = crate::leftover_disk::filesystem_object_identity(pinned)
        .with_context(|| format!("identify pinned {label}"))?;
    let opened_identity = crate::leftover_disk::filesystem_object_identity(opened)
        .with_context(|| format!("identify currently named {label}"))?;
    if pinned_identity != opened_identity {
        anyhow::bail!("GHA path no longer names pinned {label}");
    }
    Ok(())
}

fn verify_cache_mount_identity(
    root: &crate::leftover_disk::FilesystemDirectoryIdentity,
    descendant: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<()> {
    if root.device != descendant.device || root.mount != descendant.mount {
        anyhow::bail!("{label} crossed the GHA cache root filesystem or mount");
    }
    Ok(())
}

fn verify_cache_descendant_mount(
    root: &crate::leftover_disk::FilesystemDirectoryIdentity,
    descendant: &std::fs::File,
    label: &str,
) -> Result<crate::leftover_disk::FilesystemDirectoryIdentity> {
    let identity = crate::leftover_disk::filesystem_object_identity(descendant)
        .with_context(|| format!("identify pinned {label}"))?;
    verify_cache_mount_identity(root, &identity, label)?;
    Ok(identity)
}

fn open_pinned_tenant_directory(
    cache_root: &Path,
    namespace: &str,
    tenant_path: &Path,
) -> Result<std::fs::File> {
    if !valid_capability_namespace(namespace) {
        anyhow::bail!("invalid GHA cache namespace for tenant GC");
    }
    let tenants_path = cache_root.join("tenants");
    let expected_tenant_path = tenants_path.join(namespace);
    if tenant_path != expected_tenant_path {
        anyhow::bail!("GHA tenant candidate path does not match its namespace");
    }
    let cache_root_directory = open_cache_directory(cache_root, "GHA cache root")?;
    let root_identity = crate::leftover_disk::filesystem_object_identity(&cache_root_directory)
        .context("identify GHA cache root while opening tenant")?;
    let tenants_directory = open_cache_directory_child(
        &cache_root_directory,
        std::ffi::OsStr::new("tenants"),
        &tenants_path,
        "GHA cache tenants directory",
    )?
    .context("GHA cache tenants directory is missing")?;
    verify_cache_descendant_mount(
        &root_identity,
        &tenants_directory,
        "GHA cache tenants directory",
    )?;
    let tenant = open_cache_directory_child(
        &tenants_directory,
        std::ffi::OsStr::new(namespace),
        tenant_path,
        "GHA tenant candidate",
    )?
    .context("GHA tenant candidate is missing")?;
    verify_cache_descendant_mount(&root_identity, &tenant, "GHA tenant candidate")?;
    Ok(tenant)
}

fn open_pinned_cache_admission_directory(
    cache_root_directory: &std::fs::File,
    cache_root_path: &Path,
    namespace: &str,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<Option<PinnedCacheAdmissionDirectory>> {
    let parent_path = cache_root_path.join("admission-leases");
    let Some(parent) = open_cache_directory_child(
        cache_root_directory,
        std::ffi::OsStr::new("admission-leases"),
        &parent_path,
        "cache admission root",
    )?
    else {
        return Ok(None);
    };
    verify_cache_descendant_mount(root_identity, &parent, "cache admission root")?;
    let path = cache_namespace_storage_dir(cache_root_path, Some(namespace));
    let name = path
        .file_name()
        .context("cache admission directory has no name")?
        .to_os_string();
    let Some(directory) =
        open_cache_directory_child(&parent, &name, &path, "cache admission namespace directory")?
    else {
        return Ok(None);
    };
    verify_cache_descendant_mount(
        root_identity,
        &directory,
        "cache admission namespace directory",
    )?;
    Ok(Some(PinnedCacheAdmissionDirectory {
        parent_identity: crate::leftover_disk::filesystem_object_identity(&parent)
            .context("identify pinned cache admission root")?,
        identity: crate::leftover_disk::filesystem_object_identity(&directory)
            .context("identify pinned cache admission namespace directory")?,
        parent,
        parent_path,
        name,
        path,
        directory,
    }))
}

fn namespace_has_active_admission_lease_at_pinned(
    admission_directory: &PinnedCacheAdmissionDirectory,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<bool> {
    verify_cache_descendant_mount(
        root_identity,
        &admission_directory.directory,
        "cache admission namespace directory",
    )?;
    if cache_gc_quarantine_directory_is_busy(
        &admission_directory.directory,
        &admission_directory.path,
        root_identity,
        "cache admission leases",
    )
    .unwrap_or(true)
    {
        return Ok(true);
    }
    Ok(cache_directory_entry_names(
        &admission_directory.directory,
        "cache admission leases",
        root_identity,
    )?
    .iter()
    .any(|name| Path::new(name).extension().and_then(|value| value.to_str()) == Some("json")))
}

#[cfg(unix)]
fn cache_gc_quarantine_directory_is_busy(
    directory: &std::fs::File,
    directory_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    verify_cache_descendant_mount(root_identity, directory, label)?;
    for name in cache_directory_entry_names(directory, label, root_identity)? {
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with(CACHE_GC_QUARANTINE_PREFIX))
        {
            continue;
        }
        let quarantine_path = directory_path.join(&name);
        let Some(quarantine) =
            open_cache_directory_child(directory, &name, &quarantine_path, "cache-GC quarantine")?
        else {
            return Ok(true);
        };
        verify_cache_descendant_mount(root_identity, &quarantine, "cache-GC quarantine")?;
        let metadata = quarantine
            .metadata()
            .context("inspect cache-GC quarantine during tenant liveness check")?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Ok(true);
        }

        let entries =
            cache_directory_entry_names(&quarantine, "cache-GC quarantine", root_identity)?;
        if entries
            .iter()
            .any(|entry| entry == CACHE_GC_QUARANTINE_PRESERVED)
        {
            return Ok(true);
        }
        let has_entry = entries
            .iter()
            .any(|entry| entry == CACHE_GC_QUARANTINE_ENTRY);
        if !has_entry {
            if entries
                .iter()
                .any(|entry| entry != CACHE_GC_QUARANTINE_IDENTITY)
            {
                return Ok(true);
            }
            if entries
                .iter()
                .any(|entry| entry == CACHE_GC_QUARANTINE_IDENTITY)
                && read_cache_gc_quarantine_identity(&quarantine, root_identity, label)
                    .ok()
                    .flatten()
                    .is_none()
            {
                return Ok(true);
            }
            continue;
        }
        if entries.iter().any(|entry| {
            entry != CACHE_GC_QUARANTINE_ENTRY && entry != CACHE_GC_QUARANTINE_IDENTITY
        }) {
            return Ok(true);
        }
        let Some(expected_identity) =
            read_cache_gc_quarantine_identity(&quarantine, root_identity, label)
                .ok()
                .flatten()
        else {
            return Ok(true);
        };
        let entry_name = std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY);
        let Some(file) = open_cache_file_child(
            &quarantine,
            entry_name,
            &quarantine_path.join(entry_name),
            "cache-GC quarantine entry",
        )
        .ok()
        .flatten() else {
            return Ok(true);
        };
        let file_identity = match verify_cache_descendant_mount(
            root_identity,
            &file,
            "cache-GC quarantine entry",
        ) {
            Ok(identity) => identity,
            Err(_) => return Ok(true),
        };
        if !file
            .metadata()
            .context("inspect cache-GC quarantine entry during tenant liveness check")?
            .is_file()
            || !expected_identity.matches(&file_identity)
            || !try_lock_cache_file_exclusive(&file, "cache-GC quarantine entry")?
        {
            return Ok(true);
        }
        let opened = rustix::fs::fstat(&file)
            .map_err(io::Error::from)
            .context("stat cache-GC quarantine entry during tenant liveness check")?;
        let named = match rustix::fs::statat(
            &quarantine,
            entry_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        {
            Ok(named) => named,
            Err(_) => return Ok(true),
        };
        if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
            || opened.st_dev != named.st_dev
            || opened.st_ino != named.st_ino
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
fn cache_gc_quarantine_directory_is_busy(
    directory: &std::fs::File,
    directory_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<bool> {
    let _ = (directory, directory_path, root_identity, label);
    Ok(true)
}

fn tenant_has_unproven_cache_gc_quarantine(
    cache_root: &Path,
    namespace: &str,
    tenant_directory: &std::fs::File,
) -> Result<bool> {
    let root_directory = open_cache_directory(cache_root, "GHA cache root")?;
    let root_identity = crate::leftover_disk::filesystem_object_identity(&root_directory)
        .context("identify GHA cache root during generic GC")?;
    verify_cache_descendant_mount(&root_identity, tenant_directory, "GHA tenant")?;

    let tenant_path = cache_root.join("tenants").join(namespace);
    let blobs_path = tenant_path.join("blobs");
    if let Some(blobs) = open_cache_directory_child(
        tenant_directory,
        std::ffi::OsStr::new("blobs"),
        &blobs_path,
        "GHA tenant blobs directory",
    )? && cache_gc_quarantine_directory_is_busy(
        &blobs,
        &blobs_path,
        &root_identity,
        "GHA tenant blobs",
    )
    .unwrap_or(true)
    {
        return Ok(true);
    }

    let admission = open_pinned_cache_admission_directory(
        &root_directory,
        cache_root,
        namespace,
        &root_identity,
    )?;
    if let Some(admission) = admission
        && cache_gc_quarantine_directory_is_busy(
            &admission.directory,
            &admission.path,
            &root_identity,
            "cache admission leases",
        )
        .unwrap_or(true)
    {
        return Ok(true);
    }
    Ok(false)
}

fn namespace_has_reservations_at_pinned(
    tenant_directory: &std::fs::File,
    tenant_root_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<bool> {
    let reservations_path = tenant_root_path.join("reservations");
    let Some(reservations) = open_cache_directory_child(
        tenant_directory,
        std::ffi::OsStr::new("reservations"),
        &reservations_path,
        "GHA tenant reservations directory",
    )?
    else {
        return Ok(false);
    };
    verify_cache_descendant_mount(root_identity, &reservations, "GHA tenant reservations")?;
    Ok(
        cache_directory_entry_names(&reservations, "GHA tenant reservations", root_identity)?
            .iter()
            .any(|name| {
                Path::new(name).extension().and_then(|value| value.to_str()) == Some("json")
            }),
    )
}

fn namespace_has_live_upload_temp_at_pinned(
    tenant_directory: &std::fs::File,
    tenant_root_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<bool> {
    let blobs_path = tenant_root_path.join("blobs");
    let Some(blobs) = open_cache_directory_child(
        tenant_directory,
        std::ffi::OsStr::new("blobs"),
        &blobs_path,
        "GHA tenant blobs directory",
    )?
    else {
        return Ok(false);
    };
    verify_cache_descendant_mount(root_identity, &blobs, "GHA tenant blobs")?;
    if cache_gc_quarantine_directory_is_busy(&blobs, &blobs_path, root_identity, "GHA tenant blobs")
        .unwrap_or(true)
    {
        return Ok(true);
    }
    for name in cache_directory_entry_names(&blobs, "GHA tenant blobs", root_identity)? {
        if upload_temp_parts(&name).is_none() {
            continue;
        }
        let Some(file) =
            open_cache_file_child(&blobs, &name, &blobs_path.join(&name), "GHA upload temp")?
        else {
            continue;
        };
        verify_cache_descendant_mount(root_identity, &file, "GHA upload temp")?;
        if !file
            .metadata()
            .context("inspect pinned GHA upload temp")?
            .is_file()
            || !try_lock_cache_file_exclusive(&file, "GHA upload temp")?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Try to reserve one pinned tenant for generic cache GC. The returned guard
/// must stay live through quarantine deletion and its final parent sync.
/// Contention returns `Ok(None)` so the generic collector can skip this entry.
pub(crate) fn try_lock_tenant_for_cache_gc(
    cache_root: &Path,
    namespace: &str,
    tenant_path: &Path,
    pinned_tenant: &std::fs::File,
) -> Result<Option<GhaTenantGcGuard>> {
    let opened_tenant = open_pinned_tenant_directory(cache_root, namespace, tenant_path)?;
    verify_pinned_tenant_identity(pinned_tenant, &opened_tenant)?;
    let pinned_tenant = pinned_tenant
        .try_clone()
        .context("retain pinned GHA tenant candidate for GC")?;

    let Some(locks) = try_lock_tenant_gc_locks(cache_root, namespace)? else {
        return Ok(None);
    };
    let current_tenant = open_pinned_tenant_directory(cache_root, namespace, tenant_path)?;
    verify_pinned_tenant_identity(&pinned_tenant, &current_tenant)?;
    if tenant_has_unproven_cache_gc_quarantine(cache_root, namespace, &pinned_tenant)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    // Reverse declaration/drop order is namespace, then activity.
    Ok(Some(GhaTenantGcGuard {
        _namespace: locks._namespace,
        _activity: locks._activity,
        _tenant_directory: pinned_tenant,
    }))
}

async fn lock_cache_budget(
    service: &CacheService,
    namespace: Option<&str>,
) -> Result<CacheBudgetLock> {
    let path = cache_budget_lock_path(&service.root, namespace);
    let file = open_cache_entry_lock(&path)?;
    let file = lock_file_with_deadline(
        file,
        std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS),
        "cache budget",
    )
    .await?;
    Ok(CacheBudgetLock { _file: file })
}

fn lock_cache_budget_blocking(
    service: &CacheService,
    namespace: Option<&str>,
) -> Result<CacheBudgetLock> {
    let file = open_cache_entry_lock(&cache_budget_lock_path(&service.root, namespace))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(CacheBudgetLock { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("timed out waiting for cache budget lock"));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock cache budget"));
            }
        }
    }
}

fn lock_cache_assembly_scratch_blocking(cache_root: &Path) -> Result<CacheBudgetLock> {
    let file = open_cache_entry_lock(&cache_root.join(CACHE_ASSEMBLY_SCRATCH_LOCK))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(CacheBudgetLock { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("timed out waiting for assembly scratch budget lock"));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock assembly scratch budget"));
            }
        }
    }
}

fn assembly_scratch_lease_path(
    cache_root: &Path,
    namespace: Option<&str>,
    attempt: &str,
) -> PathBuf {
    let namespace_storage_dir = cache_namespace_storage_dir(cache_root, namespace);
    let namespace_hash = namespace_storage_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("root");
    cache_root
        .join(CACHE_ASSEMBLY_SCRATCH_LEASE_DIR)
        .join(format!("{namespace_hash}-{attempt}.json"))
}

fn ensure_global_assembly_scratch_lease_directory<F>(
    cache_root: &Path,
    sync_parent: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let directory = cache_root.join(CACHE_ASSEMBLY_SCRATCH_LEASE_DIR);
    std::fs::create_dir_all(&directory).context("create assembly scratch lease directory")?;
    existing_real_directory(&directory, "assembly scratch lease directory")?;
    // The child sync below persists its contents, while this parent sync
    // persists the lease directory's name itself before scratch artifacts can
    // be created.
    sync_parent(cache_root).context("sync cache root for assembly scratch lease directory")?;
    Ok(directory)
}

fn reserve_global_assembly_scratch(
    cache_root: &Path,
    maximum_bytes: u64,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
    scratch_bytes: u64,
) -> Result<Option<CacheAssemblyScratchLease>> {
    reserve_global_assembly_scratch_with_sync(
        cache_root,
        maximum_bytes,
        namespace,
        id,
        attempt,
        scratch_bytes,
        sync_directory,
    )
}

fn reserve_global_assembly_scratch_with_sync<F>(
    cache_root: &Path,
    maximum_bytes: u64,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
    scratch_bytes: u64,
    sync_parent: F,
) -> Result<Option<CacheAssemblyScratchLease>>
where
    F: FnOnce(&Path) -> Result<()>,
{
    if scratch_bytes == 0 {
        return Ok(None);
    }
    let _budget_lock = lock_cache_assembly_scratch_blocking(cache_root)?;
    let directory = ensure_global_assembly_scratch_lease_directory(cache_root, sync_parent)?;
    let (active_bytes, _) = scan_global_assembly_scratch_locked(cache_root)?;
    if active_bytes
        .checked_add(scratch_bytes)
        .is_none_or(|projected| projected > maximum_bytes)
    {
        return Err(anyhow::Error::new(CacheLockBusy)
            .context("global assembly scratch byte budget is full"));
    }

    let path = assembly_scratch_lease_path(cache_root, namespace, attempt);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("assembly scratch lease has no file name")?;
    let temporary_path = directory.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let cleanup = TemporaryUpload::new(temporary_path.clone());
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    let mut file = options.open(&temporary_path).with_context(|| {
        format!(
            "create temporary assembly scratch lease {}",
            temporary_path.display()
        )
    })?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .context("lock temporary assembly scratch lease")?;
    let record = json!({
        "namespace": namespace.unwrap_or_default(),
        "id": id,
        "attempt": attempt,
        "scratchBytes": scratch_bytes,
    });
    file.write_all(record.to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .context("persist temporary assembly scratch lease")?;

    let directory_file = std::fs::File::open(&directory)
        .context("open assembly scratch lease directory for publication")?;
    rustix::fs::renameat_with(
        &directory_file,
        temporary_path
            .file_name()
            .context("temporary assembly scratch lease has no file name")?,
        &directory_file,
        path.file_name()
            .context("assembly scratch lease has no file name")?,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
    .context("publish assembly scratch lease without replacement")?;
    drop(cleanup);
    sync_directory(&directory).context("sync assembly scratch lease directory")?;
    Ok(Some(CacheAssemblyScratchLease {
        cache_root: cache_root.to_path_buf(),
        path,
        namespace: namespace.map(ToOwned::to_owned),
        id: id.to_owned(),
        attempt: attempt.to_owned(),
        file: Some(file),
    }))
}

fn cleanup_assembly_scratch_lease_temps(directory: &Path) -> Result<usize> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("scan temporary assembly scratch leases"),
    };
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.context("read temporary assembly scratch lease")?;
        if !is_assembly_scratch_lease_temp(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => anyhow::bail!("temporary assembly scratch lease is not a regular file"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("stat temporary assembly scratch lease"),
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove temporary assembly scratch lease"),
        }
    }
    if removed > 0 {
        sync_directory(directory).context("sync temporary assembly scratch lease cleanup")?;
    }
    Ok(removed)
}

fn is_assembly_scratch_lease_temp(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str().and_then(|name| name.strip_prefix('.')) else {
        return false;
    };
    let Some(name) = name.strip_suffix(".tmp") else {
        return false;
    };
    let Some((lease_name, nonce)) = name.rsplit_once('.') else {
        return false;
    };
    let Some(lease_stem) = lease_name.strip_suffix(".json") else {
        return false;
    };
    let Some((_namespace_hash, attempt)) = lease_stem.rsplit_once('-') else {
        return false;
    };
    !lease_stem.is_empty()
        && attempt.len() == 32
        && attempt.bytes().all(|byte| byte.is_ascii_hexdigit())
        && nonce.len() == 32
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cleanup_stale_global_assembly_scratch(cache_root: &Path) -> Result<usize> {
    let _budget_lock = lock_cache_assembly_scratch_blocking(cache_root)?;
    let (_, removed) = scan_global_assembly_scratch_locked(cache_root)?;
    Ok(removed)
}

fn release_global_assembly_scratch(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
) -> Result<()> {
    let _budget_lock = lock_cache_assembly_scratch_blocking(cache_root)?;
    remove_global_assembly_scratch_lease_locked(cache_root, namespace, id, attempt)?;
    Ok(())
}

fn remove_global_assembly_scratch_lease_locked(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
) -> Result<bool> {
    remove_global_assembly_scratch_lease_locked_with_sync(
        cache_root,
        namespace,
        id,
        attempt,
        sync_directory,
    )
}

fn remove_global_assembly_scratch_lease_locked_with_sync<F>(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
    mut sync: F,
) -> Result<bool>
where
    F: FnMut(&Path) -> Result<()>,
{
    let path = assembly_scratch_lease_path(cache_root, namespace, attempt);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("open durable assembly scratch lease"),
    };
    if let Some(file) = file.as_ref() {
        match rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("assembly scratch lease is still active"));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock assembly scratch lease"));
            }
        }
        let metadata = std::fs::symlink_metadata(&path)
            .context("stat durable assembly scratch lease before release")?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("durable assembly scratch lease is not a regular file");
        }
    }
    if file.is_some() {
        let raw = std::fs::read(&path).context("read durable assembly scratch lease")?;
        let record: Value =
            serde_json::from_slice(&raw).context("parse durable assembly scratch lease")?;
        let (recorded_namespace, recorded_id, recorded_attempt, _) =
            parse_assembly_scratch_record(cache_root, &path, &record)?;
        if recorded_namespace != namespace || recorded_id != id || recorded_attempt != attempt {
            anyhow::bail!("durable assembly scratch lease identity changed");
        }
    }
    let artifacts = assembly_scratch_artifacts(cache_root, namespace, id, attempt)?;
    if artifacts.temporary_assembly || artifacts.canonical_with_sources {
        anyhow::bail!("cannot release assembly scratch while duplicate bytes remain");
    }
    sync_assembly_scratch_artifact_directories_with(cache_root, namespace, id, &mut sync)?;
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("remove durable assembly scratch lease"),
    }
    drop(file);
    if let Some(parent) = path.parent() {
        sync(parent).context("sync durable assembly scratch lease removal")?;
    }
    Ok(true)
}

/// Sum live leases and remove unlocked remnants while the root-wide budget
/// lock prevents a concurrent admission from observing a partial sweep.
fn scan_global_assembly_scratch_locked(cache_root: &Path) -> Result<(u64, usize)> {
    let directory = cache_root.join(CACHE_ASSEMBLY_SCRATCH_LEASE_DIR);
    if !existing_real_directory(&directory, "assembly scratch lease directory")? {
        return Ok((0, 0));
    }
    cleanup_assembly_scratch_lease_temps(&directory)?;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error).context("scan assembly scratch leases"),
    };
    let mut active_bytes = 0u64;
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.context("read assembly scratch lease")?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            anyhow::bail!("unexpected file in assembly scratch lease directory");
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => anyhow::bail!("assembly scratch lease is not a regular file"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("stat assembly scratch lease"),
        };
        let _ = metadata;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("open assembly scratch lease"),
        };
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Err(rustix::io::Errno::WOULDBLOCK) => {
                let raw = std::fs::read(&path).context("read active assembly scratch lease")?;
                let record: Value =
                    serde_json::from_slice(&raw).context("parse active assembly scratch lease")?;
                let (_, _, _, bytes) = parse_assembly_scratch_record(cache_root, &path, &record)?;
                active_bytes = active_bytes
                    .checked_add(bytes)
                    .context("assembly scratch lease byte sum overflow")?;
            }
            Ok(()) => {
                let raw = std::fs::read(&path).context("read idle assembly scratch lease")?;
                let record: Value =
                    serde_json::from_slice(&raw).context("parse idle assembly scratch lease")?;
                let (namespace, id, attempt, bytes) =
                    parse_assembly_scratch_record(cache_root, &path, &record)?;
                let artifacts = assembly_scratch_artifacts(cache_root, namespace, id, attempt)?;
                if artifacts.temporary_assembly || artifacts.canonical_with_sources {
                    active_bytes = active_bytes
                        .checked_add(bytes)
                        .context("assembly scratch lease byte sum overflow")?;
                    drop(file);
                    continue;
                }
                drop(file);
                if remove_global_assembly_scratch_lease_locked(cache_root, namespace, id, attempt)?
                {
                    removed = removed.saturating_add(1);
                }
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock assembly scratch lease"));
            }
        }
    }
    if removed > 0 {
        sync_directory(&directory).context("sync stale assembly scratch lease cleanup")?;
    }
    Ok((active_bytes, removed))
}

struct AssemblyScratchArtifacts {
    temporary_assembly: bool,
    canonical_with_sources: bool,
}

/// Persist absence of assembly artifacts before dropping their durable charge.
/// Cancellation cleanup may unlink a temp or part directory without being
/// able to report a parent-directory sync failure. The locked recovery sweep
/// performs that sync before removing the lease, so power-loss recovery cannot
/// resurrect an uncharged name.
fn sync_assembly_scratch_artifact_directories(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
) -> Result<()> {
    sync_assembly_scratch_artifact_directories_with(cache_root, namespace, id, &mut sync_directory)
}

fn sync_assembly_scratch_artifact_directories_with<F>(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
    sync: &mut F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let tenant_root = match namespace {
        None => cache_root.to_path_buf(),
        Some(namespace) => {
            let tenants = cache_root.join("tenants");
            if !existing_real_directory(&tenants, "assembly scratch tenants directory")? {
                sync(cache_root).context("sync cache root without tenants directory")?;
                return Ok(());
            }
            let tenant = tenants.join(namespace);
            if !existing_real_directory(&tenant, "assembly scratch tenant directory")? {
                sync(&tenants).context("sync assembly scratch tenants directory")?;
                return Ok(());
            }
            tenant
        }
    };

    let blobs = tenant_root.join("blobs");
    if existing_real_directory(&blobs, "assembly scratch blobs directory")? {
        sync(&blobs).context("sync assembly scratch blob directory")?;
    } else {
        sync(&tenant_root).context("sync assembly scratch tenant directory")?;
    }

    let uploads = tenant_root.join("uploads");
    if existing_real_directory(&uploads, "assembly scratch uploads directory")? {
        let parts = uploads.join(id);
        if existing_real_directory(&parts, "assembly scratch parts directory")? {
            sync(&parts).context("sync assembly scratch parts directory")?;
        }
        sync(&uploads).context("sync assembly scratch uploads directory")?;
    } else {
        sync(&tenant_root).context("sync assembly scratch tenant directory")?;
    }
    Ok(())
}

fn parse_assembly_scratch_record<'a>(
    cache_root: &Path,
    path: &Path,
    record: &'a Value,
) -> Result<(Option<&'a str>, &'a str, &'a str, u64)> {
    let namespace = record["namespace"]
        .as_str()
        .context("assembly scratch lease has no namespace")?;
    let namespace = if namespace.is_empty() {
        None
    } else if valid_capability_namespace(namespace) {
        Some(namespace)
    } else {
        anyhow::bail!("assembly scratch lease has invalid namespace");
    };
    let id = record["id"]
        .as_str()
        .context("assembly scratch lease has no cache id")?;
    validate_cache_id(id).context("assembly scratch lease has invalid cache id")?;
    let attempt = record["attempt"]
        .as_str()
        .context("assembly scratch lease has no attempt id")?;
    if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("assembly scratch lease has invalid attempt id");
    }
    let bytes = record["scratchBytes"]
        .as_u64()
        .filter(|bytes| *bytes > 0)
        .context("assembly scratch lease has no positive byte count")?;
    let expected = assembly_scratch_lease_path(cache_root, namespace, attempt);
    if path.file_name() != expected.file_name() {
        anyhow::bail!("assembly scratch lease identity does not match its path");
    }
    Ok((namespace, id, attempt, bytes))
}

/// A live assembly temp owns scratch by itself. Once publication hard-links it
/// into the canonical blob, scratch remains charged only while source block
/// files still duplicate those bytes. This probe uses no namespace lock so it
/// is safe beneath the root-wide scratch lock and during startup recovery.
fn assembly_scratch_artifacts(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
    attempt: &str,
) -> Result<AssemblyScratchArtifacts> {
    let tenant_root = match namespace {
        None => cache_root.to_path_buf(),
        Some(namespace) => {
            let tenants = cache_root.join("tenants");
            if !existing_real_directory(&tenants, "assembly scratch tenants directory")? {
                return Ok(AssemblyScratchArtifacts {
                    temporary_assembly: false,
                    canonical_with_sources: false,
                });
            }
            let tenant = tenants.join(namespace);
            if !existing_real_directory(&tenant, "assembly scratch tenant directory")? {
                return Ok(AssemblyScratchArtifacts {
                    temporary_assembly: false,
                    canonical_with_sources: false,
                });
            }
            tenant
        }
    };
    let blobs = tenant_root.join("blobs");
    let blobs_exist = existing_real_directory(&blobs, "assembly scratch blobs directory")?;
    let temporary_assembly = if blobs_exist {
        artifact_is_regular_file(
            &blobs.join(format!(".{id}.{attempt}.tmp")),
            "assembly scratch temporary file",
        )?
    } else {
        false
    };
    let canonical = if blobs_exist {
        artifact_is_regular_file(&blobs.join(id), "assembly scratch canonical blob")?
    } else {
        false
    };

    let uploads = tenant_root.join("uploads");
    let uploads_exist = existing_real_directory(&uploads, "assembly scratch uploads directory")?;
    let parts = if uploads_exist {
        let path = uploads.join(id);
        if existing_real_directory(&path, "assembly scratch parts directory")? {
            let mut has_sources = false;
            for entry in std::fs::read_dir(&path).context("scan assembly scratch source parts")? {
                let entry = entry.context("read assembly scratch source part")?;
                artifact_is_regular_file(&entry.path(), "assembly scratch source part")?;
                has_sources = true;
            }
            has_sources
        } else {
            false
        }
    } else {
        false
    };

    Ok(AssemblyScratchArtifacts {
        temporary_assembly,
        canonical_with_sources: canonical && parts,
    })
}

fn existing_real_directory(path: &Path, label: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => anyhow::bail!("{label} is not a real directory"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat {label}")),
    }
}

fn artifact_is_regular_file(path: &Path, label: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => anyhow::bail!("{label} is not a regular file"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat {label}")),
    }
}

fn lock_cache_namespace_blocking(
    cache_root: &Path,
    namespace: Option<&str>,
) -> Result<CacheNamespaceLock> {
    let file = open_cache_entry_lock(&cache_namespace_lock_path(cache_root, namespace))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(CacheNamespaceLock { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("timed out waiting for cache namespace lock"));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("lock cache namespace"));
            }
        }
    }
}

fn lock_cache_entry_blocking(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
) -> Result<CacheEntryLock> {
    validate_cache_id(id)?;
    let namespace_guard = lock_cache_namespace_blocking(&service.root, namespace)?;
    let file = open_cache_entry_lock(&service.cache_entry_lock_path(id, namespace))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(CACHE_LOCK_WAIT_SECS);
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                return Ok(CacheEntryLock {
                    _namespace: namespace_guard,
                    _file: file,
                });
            }
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("timed out waiting for cache entry lock"));
            }
            Err(error) => return Err(anyhow::Error::new(error).context("lock cache entry")),
        }
    }
}

async fn lock_file_with_deadline(
    file: std::fs::File,
    timeout: std::time::Duration,
    lock_name: &str,
) -> Result<std::fs::File> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(anyhow::Error::new(CacheLockBusy)
                        .context(format!("timed out waiting for {lock_name} lock")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!("lock {lock_name}")));
            }
        }
    }
}

/// Try the same stable entry flock without waiting. Cleanup skips an entry
/// whenever an upload, reservation recovery, or finalize currently owns it.
fn try_lock_cache_entry_at(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
) -> Result<Option<CacheEntryLock>> {
    validate_cache_id(id)?;
    let Some(namespace_lock) = try_lock_cache_namespace_at(cache_root, namespace)? else {
        return Ok(None);
    };
    let path = cache_entry_lock_path(cache_root, namespace, id);
    let file = open_cache_entry_lock(&path)?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(CacheEntryLock {
            _namespace: namespace_lock,
            _file: file,
        })),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache entry")),
    }
}

/// Try only the entry shard for callers that already own the namespace lock.
fn try_lock_cache_entry_file_at(
    cache_root: &Path,
    namespace: Option<&str>,
    id: &str,
) -> Result<Option<std::fs::File>> {
    validate_cache_id(id)?;
    let file = open_cache_entry_lock(&cache_entry_lock_path(cache_root, namespace, id))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache entry shard")),
    }
}

fn try_lock_cache_namespace_at(
    cache_root: &Path,
    namespace: Option<&str>,
) -> Result<Option<CacheNamespaceLock>> {
    let path = cache_namespace_lock_path(cache_root, namespace);
    let file = open_cache_entry_lock(&path)?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(CacheNamespaceLock { _file: file })),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache namespace")),
    }
}

/// Try the per-attempt temp-file flock. Active body streams keep this inode
/// locked while the shared entry stripe is released; process death releases
/// the flock so the next sweep can remove the exact temp link.
fn try_lock_upload_temp(path: &Path) -> Result<Option<std::fs::File>> {
    let file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("open cache upload temp for reaping"),
    };
    if !file
        .metadata()
        .context("stat cache upload temp for reaping")?
        .is_file()
    {
        return Ok(None);
    }
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error).context("try-lock cache upload temp")),
    }
}

/// Extract an ID only from the exact `.{id}.{32 hex chars}.tmp` attempt shape.
/// Loose prefix matches could delete unrelated blobs.
#[cfg(test)]
fn upload_temp_cache_id(name: &std::ffi::OsStr) -> Option<&str> {
    upload_temp_parts(name).map(|(id, _attempt)| id)
}

fn upload_temp_parts(name: &std::ffi::OsStr) -> Option<(&str, &str)> {
    let name = name.to_str()?.strip_prefix('.')?.strip_suffix(".tmp")?;
    let (id, nonce) = name.split_once('.')?;
    if name.matches('.').count() != 1
        || id.len() != 64
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some((id, nonce))
}

/// Identify V1 range (`{attempt}.part`) and V2 block (`v2-{attempt}.part`)
/// files by their exact generated names.
fn upload_part_attempt(name: &std::ffi::OsStr) -> Option<(bool, &str)> {
    let name = name.to_str()?.strip_suffix(".part")?;
    let (v2, attempt) = match name.strip_prefix("v2-") {
        Some(attempt) => (true, attempt),
        None => (false, name),
    };
    (attempt.len() == 32 && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some((v2, attempt))
}

fn cache_blob_id(name: &std::ffi::OsStr) -> Option<&str> {
    let id = name.to_str()?;
    (id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(id)
}

/// Return true if a matching upload temp still has its per-attempt flock.
/// This check is always performed while holding the entry shard before stale
/// reservation or multipart cleanup, so a body stream cannot lose its lease.
fn has_live_upload_temp_for_id(tenant_root: &Path, id: &str) -> Result<bool> {
    let blobs = tenant_root.join("blobs");
    let files = match std::fs::read_dir(&blobs) {
        Ok(files) => files,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("scan active cache upload leases"),
    };
    for file in files {
        let file = file.context("read active cache upload lease")?;
        let file_name = file.file_name();
        let Some((temp_id, _)) = upload_temp_parts(&file_name) else {
            continue;
        };
        if temp_id != id {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(file.path()) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("stat active cache upload lease"),
        };
        let _ = metadata;
        if try_lock_upload_temp(&file.path())?.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove the per-entry v1 range/v2 block staging directory without following
/// a tenant-controlled symlink. The caller holds the entry shard and has
/// already established that no live upload lease references it.
fn remove_upload_parts_directory(
    tenant_root: &Path,
    id: &str,
    outcome: &mut UploadTempCleanup,
) -> Result<()> {
    let directory = tenant_root.join("uploads").join(id);
    let metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_dir() => metadata,
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("stat stale cache upload parts"),
    };
    let _ = metadata;
    std::fs::remove_dir_all(&directory).context("remove stale cache upload parts")?;
    if let Some(parent) = directory.parent() {
        sync_directory(parent)?;
    }
    outcome.removed += 1;
    Ok(())
}

fn reservation_record_is_expired(value: &Value, id: &str, modified_ms: u64, now_ms: u64) -> bool {
    let file_age_expired = now_ms.saturating_sub(modified_ms) > V1_RESERVATION_TTL_MS;
    match value.get("protocol").and_then(Value::as_str) {
        Some("v2") => parse_v2_reservation(value.clone(), id, modified_ms)
            .map_or(file_age_expired, |reservation| {
                v2_reservation_is_expired(&reservation, now_ms)
            }),
        None => {
            let record_updated_ms = if parse_v1_reservation(value.clone(), id).is_ok() {
                value["updatedMs"].as_u64().unwrap_or(modified_ms)
            } else {
                modified_ms
            };
            now_ms.saturating_sub(record_updated_ms) > V1_RESERVATION_TTL_MS
        }
        Some(_) => file_age_expired,
    }
}

fn reservation_file_is_expired(path: &Path, now_ms: u64) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error).context("stat malformed cache reservation"),
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let modified_ms = system_time_unix_millis(
        metadata
            .modified()
            .context("read malformed cache reservation modification time")?,
    )?;
    Ok(now_ms.saturating_sub(modified_ms) > V1_RESERVATION_TTL_MS)
}

fn system_time_unix_millis(time: std::time::SystemTime) -> Result<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .context("system clock predates Unix epoch")
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).context("system clock milliseconds overflow")
        })
}

fn now_unix_millis() -> Result<u64> {
    system_time_unix_millis(std::time::SystemTime::now())
}

fn system_time_unix_nanos(time: std::time::SystemTime) -> u64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

fn now_unix_nanos() -> u64 {
    system_time_unix_nanos(std::time::SystemTime::now())
}

fn cache_entry_access_ns(entry: &Value, metadata: &std::fs::Metadata) -> u64 {
    entry["last_accessed_ns"]
        .as_u64()
        .unwrap_or_else(|| metadata.modified().map(system_time_unix_nanos).unwrap_or(0))
}

fn replace_json_atomically(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("JSON replacement has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("JSON replacement has no file name")?;
    let tmp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let cleanup = TemporaryUpload::new(tmp.clone());
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .context("create temporary JSON replacement")?;
        file.write_all(value.to_string().as_bytes())
            .context("write temporary JSON replacement")?;
        file.sync_all().context("sync temporary JSON replacement")?;
    }
    std::fs::rename(&tmp, path).context("atomically replace JSON record")?;
    drop(cleanup);
    sync_directory(parent)?;
    Ok(())
}

impl V1Reservation {
    fn as_json(&self) -> Value {
        json!({
            "key": self.key,
            "version": self.version,
            "cacheId": self.cache_id,
            "cacheSize": self.expected_size,
            "uploadChunks": self.chunks.iter().map(|chunk| json!({
                "start": chunk.start,
                "end": chunk.end,
                "attempt": chunk.attempt,
                "sha256": chunk.sha256,
            })).collect::<Vec<_>>(),
            "commitAttempt": self.commit_attempt,
            "activeUploads": self.active_uploads,
        })
    }

    fn as_json_with_generation(&self, generation: &str, updated_ms: u64) -> Value {
        let mut value = self.as_json();
        value["uploadGeneration"] = json!(generation);
        value["uploadAttempt"] = Value::Null;
        value["updatedMs"] = json!(updated_ms);
        value
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("open directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))?;
    Ok(())
}

/// Atomically publish JSON only when the destination does not already exist.
/// The temporary file is flushed before its hard link becomes visible, so a
/// restart cannot observe a partial reservation or entry record.
fn atomically_create_json(path: &Path, value: &Value) -> Result<bool> {
    let parent = path.parent().context("JSON publication has no parent")?;
    std::fs::create_dir_all(parent).context("create JSON publication directory")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("JSON publication has no file name")?;
    let tmp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let cleanup = TemporaryUpload::new(tmp.clone());
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .context("create temporary JSON publication")?;
        file.write_all(value.to_string().as_bytes())
            .context("write temporary JSON publication")?;
        file.sync_all().context("sync temporary JSON publication")?;
    }

    match std::fs::hard_link(&tmp, path) {
        Ok(()) => {
            drop(cleanup);
            sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            drop(cleanup);
            Ok(false)
        }
        Err(error) => Err(error).context("publish JSON without replacing existing record"),
    }
}

/// Reserve a JavaScript-safe numeric V1 cache ID. The side index is created
/// with a no-replace link so concurrent reservations cannot alias an ID.
fn allocate_v1_cache_id(service: &CacheService, namespace: &str, hash: &str) -> Result<u64> {
    let directory = service
        .tenant_root(Some(namespace))
        .join("reservations")
        .join("by-id");
    std::fs::create_dir_all(&directory).context("create v1 cache-id index")?;
    for _ in 0..32 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
        let cache_id = (u64::from_be_bytes(bytes) & MAX_SAFE_CACHE_ID).max(1);
        let path = service.cache_id_index_path(cache_id, Some(namespace));
        if atomically_create_json(&path, &json!({"entryHash": hash}))? {
            return Ok(cache_id);
        }
        let existing = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok());
        if existing
            .as_ref()
            .and_then(|value| value["entryHash"].as_str())
            == Some(hash)
        {
            return Ok(cache_id);
        }
    }
    anyhow::bail!("could not allocate a unique v1 cacheId")
}

fn remove_v1_cache_id_index(
    service: &CacheService,
    namespace: &str,
    cache_id: u64,
    hash: &str,
) -> Result<()> {
    let path = service.cache_id_index_path(cache_id, Some(namespace));
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read v1 cache-id index before removal"),
    };
    let value: Value = serde_json::from_slice(&raw).context("parse v1 cache-id index")?;
    if value["entryHash"].as_str() != Some(hash) {
        anyhow::bail!("refusing to remove a v1 cache-id index for another entry");
    }
    std::fs::remove_file(&path).context("remove v1 cache-id index")?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn resolve_v1_cache_id(service: &CacheService, namespace: &str, raw_id: &str) -> Result<String> {
    if raw_id.is_empty() || !raw_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(anyhow::Error::new(CacheBadRequest).context("invalid v1 numeric cacheId"));
    }
    let cache_id: u64 = raw_id
        .parse()
        .map_err(|error| anyhow::Error::new(CacheBadRequest).context(error))?;
    if cache_id == 0 || cache_id > MAX_SAFE_CACHE_ID {
        return Err(anyhow::Error::new(CacheBadRequest)
            .context("v1 cacheId is outside the safe integer range"));
    }
    let path = service.cache_id_index_path(cache_id, Some(namespace));
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow::Error::new(CacheProtocolConflict)
                .context("v1 cacheId has no active reservation"));
        }
        Err(error) => return Err(error).context("find v1 cacheId"),
    };
    if !metadata.file_type().is_file() {
        return Err(anyhow::Error::new(CacheProtocolConflict)
            .context("v1 cacheId index is not a regular file"));
    }
    let raw = std::fs::read(&path).context("read v1 cacheId index")?;
    let index: Value = serde_json::from_slice(&raw)
        .map_err(|error| anyhow::Error::new(CacheProtocolConflict).context(error))?;
    let Some(hash) = index["entryHash"].as_str() else {
        return Err(
            anyhow::Error::new(CacheProtocolConflict).context("v1 cacheId index has no entry hash")
        );
    };
    if validate_cache_id(hash).is_err() {
        return Err(anyhow::Error::new(CacheProtocolConflict)
            .context("v1 cacheId index has an invalid entry hash"));
    }
    let Some((record, _)) = read_reservation_record(service, hash, Some(namespace))? else {
        return Err(
            anyhow::Error::new(CacheProtocolConflict).context("v1 cache reservation is missing")
        );
    };
    if record.get("protocol").is_some() {
        return Err(anyhow::Error::new(CacheProtocolConflict)
            .context("v1 cacheId points at a foreign protocol reservation"));
    }
    let reservation = parse_v1_reservation(record, hash)
        .map_err(|error| anyhow::Error::new(CacheProtocolConflict).context(error))?;
    if reservation.cache_id != cache_id {
        return Err(anyhow::Error::new(CacheProtocolConflict)
            .context("v1 cacheId index does not match its reservation"));
    }
    Ok(hash.to_owned())
}

/// Recheck the side index only after the caller acquires the resolved entry's
/// stable shard. The preliminary resolver read can race reservation expiry and
/// numeric-ID reuse; a stale index must never authorize writes to a new claim.
fn verify_v1_cache_id_locked(
    service: &CacheService,
    namespace: &str,
    raw_id: &str,
    expected_hash: &str,
) -> Result<()> {
    let cache_id: u64 = raw_id
        .parse()
        .map_err(|error| anyhow::Error::new(CacheBadRequest).context(error))?;
    let path = service.cache_id_index_path(cache_id, Some(namespace));
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow::Error::new(CacheProtocolConflict)
                .context("v1 cacheId reservation expired before upload"));
        }
        Err(error) => return Err(error).context("recheck v1 cacheId index"),
    };
    if !metadata.file_type().is_file() {
        return Err(anyhow::Error::new(CacheProtocolConflict)
            .context("v1 cacheId index is not a regular file"));
    }
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow::Error::new(CacheProtocolConflict)
                .context("v1 cacheId reservation expired before upload"));
        }
        Err(error) => return Err(error).context("recheck v1 cacheId index"),
    };
    let index: Value = serde_json::from_slice(&raw)
        .map_err(|error| anyhow::Error::new(CacheProtocolConflict).context(error))?;
    if index["entryHash"].as_str() != Some(expected_hash) {
        return Err(
            anyhow::Error::new(CacheProtocolConflict).context("v1 cacheId changed before upload")
        );
    }
    Ok(())
}

fn parse_v1_reservation(value: Value, id: &str) -> Result<V1Reservation> {
    if value.get("protocol").is_some() {
        anyhow::bail!("cache reservation belongs to another protocol");
    }
    let key = value["key"]
        .as_str()
        .context("v1 cache reservation has no key")?;
    let version = value["version"]
        .as_str()
        .context("v1 cache reservation has no version")?;
    let cache_id = value["cacheId"]
        .as_u64()
        .context("v1 cache reservation has no valid cacheId")?;
    if cache_id == 0 || cache_id > MAX_SAFE_CACHE_ID {
        anyhow::bail!("v1 cache reservation has an unsafe cacheId");
    }
    let expected_size = match value.get("cacheSize") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .context("v1 cache reservation has an invalid cacheSize")?,
        ),
    };
    if expected_size.is_some_and(|size| size > MAX_BODY) {
        anyhow::bail!("v1 cache reservation exceeds {MAX_BODY} bytes");
    }
    let mut chunks = Vec::new();
    if let Some(records) = value.get("uploadChunks").and_then(Value::as_array) {
        if records.len() > MAX_V1_UPLOAD_CHUNKS {
            anyhow::bail!("v1 cache reservation exceeds the upload chunk limit");
        }
        for record in records {
            let start = record["start"].as_u64().context("v1 chunk has no start")?;
            let end = record["end"].as_u64().context("v1 chunk has no end")?;
            let attempt = record["attempt"]
                .as_str()
                .context("v1 chunk has no attempt")?;
            let sha256 = record["sha256"]
                .as_str()
                .context("v1 chunk has no digest")?;
            if start > end
                || attempt.len() != 32
                || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit())
                || sha256.len() != 64
                || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                anyhow::bail!("v1 cache reservation contains an invalid upload chunk");
            }
            chunks.push(V1UploadChunk {
                start,
                end,
                attempt: attempt.to_owned(),
                sha256: sha256.to_owned(),
            });
        }
    } else if value
        .get("uploadChunks")
        .is_some_and(|value| !value.is_null())
    {
        anyhow::bail!("v1 cache reservation has an invalid uploadChunks list");
    }
    let commit_attempt = match value.get("commitAttempt") {
        None | Some(Value::Null) => None,
        Some(Value::String(attempt))
            if attempt.len() == 32 && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(attempt.clone())
        }
        Some(_) => anyhow::bail!("v1 cache reservation has an invalid commit attempt"),
    };
    let active_uploads = parse_attempt_list(&value, "activeUploads")?;
    if entry_hash(key, version) != id {
        anyhow::bail!("v1 cache reservation does not match cache id");
    }
    Ok(V1Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        cache_id,
        expected_size,
        chunks,
        commit_attempt,
        active_uploads,
    })
}

fn parse_attempt_list(value: &Value, field: &str) -> Result<Vec<String>> {
    let attempts = match value.get(field) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(attempts)) => attempts,
        Some(_) => anyhow::bail!("invalid cache upload attempt list"),
    };
    let mut parsed = Vec::with_capacity(attempts.len());
    for attempt in attempts {
        let attempt = attempt
            .as_str()
            .context("invalid cache upload attempt list")?;
        if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("invalid cache upload attempt list");
        }
        parsed.push(attempt.to_owned());
    }
    Ok(parsed)
}

fn read_v1_reservation(
    service: &CacheService,
    id: &str,
    namespace: &str,
) -> Result<Option<V1Reservation>> {
    let Some((value, _)) = read_reservation_record(service, id, Some(namespace))? else {
        return Ok(None);
    };
    parse_v1_reservation(value, id).map(Some)
}

fn reservation_upload_generation(record: &Value) -> Result<&str> {
    let generation = record["uploadGeneration"]
        .as_str()
        .context("v1 cache reservation has no upload generation")?;
    if generation.len() != 32 || !generation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("v1 cache reservation has an invalid upload generation");
    }
    Ok(generation)
}

/// Caller holds this entry's shard lock. Refreshing the mtime starts the v1
/// upload lease; the random generation rejects a body that outlives or loses
/// its reservation while the shard is released for streaming.
fn refresh_v1_reservation_claim(
    service: &CacheService,
    id: &str,
    namespace: &str,
    expected: &V1Reservation,
) -> Result<String> {
    let path = service.reservation_path(id, Some(namespace));
    let Some((mut record, _)) = read_reservation_record(service, id, Some(namespace))? else {
        anyhow::bail!("v1 cache reservation is missing");
    };
    if parse_v1_reservation(record.clone(), id)? != *expected {
        anyhow::bail!("v1 cache reservation changed before upload");
    }
    let generation = match record.get("uploadGeneration") {
        None => uuid::Uuid::new_v4().simple().to_string(),
        Some(Value::String(value)) => {
            reservation_upload_generation(&record)?;
            value.clone()
        }
        Some(_) => anyhow::bail!("v1 cache reservation has an invalid upload generation"),
    };
    record["uploadGeneration"] = json!(generation);
    record["updatedMs"] = json!(now_unix_millis()?);
    replace_json_atomically(&path, &record)?;
    Ok(generation)
}

#[cfg(test)]
fn begin_v1_upload_attempt(
    service: &CacheService,
    id: &str,
    namespace: &str,
    expected: &V1Reservation,
    attempt: &str,
) -> Result<(String, String)> {
    let generation = refresh_v1_reservation_claim(service, id, namespace, expected)?;
    let path = service.reservation_path(id, Some(namespace));
    let Some((mut record, _)) = read_reservation_record(service, id, Some(namespace))? else {
        anyhow::bail!("v1 cache reservation disappeared while starting upload");
    };
    record["uploadAttempt"] = json!(attempt);
    record["updatedMs"] = json!(now_unix_millis()?);
    replace_json_atomically(&path, &record)?;
    Ok((generation, attempt.to_owned()))
}

#[cfg(test)]
fn validate_v1_reservation_attempt(
    service: &CacheService,
    id: &str,
    namespace: &str,
    expected: &V1Reservation,
    generation: &str,
    attempt: &str,
) -> Result<()> {
    let Some((record, _)) = read_reservation_record(service, id, Some(namespace))? else {
        anyhow::bail!("v1 cache reservation expired or was replaced during upload");
    };
    if parse_v1_reservation(record.clone(), id)? != *expected
        || reservation_upload_generation(&record)? != generation
        || record["uploadAttempt"].as_str() != Some(attempt)
    {
        anyhow::bail!("v1 cache reservation expired or was replaced during upload");
    }
    Ok(())
}

fn read_reservation_record(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
) -> Result<Option<(Value, u64)>> {
    validate_cache_id(id)?;
    let path = service.reservation_path(id, namespace);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("stat cache reservation"),
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!("cache reservation is not a regular file");
    }
    if metadata.len() > MAX_RESERVATION_BYTES {
        anyhow::bail!("cache reservation exceeds its byte limit");
    }
    let modified_ms = system_time_unix_millis(
        metadata
            .modified()
            .context("read cache reservation modification time")?,
    )?;
    let raw = std::fs::read(&path).context("read cache reservation")?;
    let value = serde_json::from_slice(&raw).context("parse cache reservation")?;
    Ok(Some((value, modified_ms)))
}

#[cfg(test)]
fn load_v1_reservation(service: &CacheService, id: &str, namespace: &str) -> Result<V1Reservation> {
    read_v1_reservation(service, id, namespace)?.context("v1 cache reservation is missing")
}

fn persist_v1_reservation(
    service: &CacheService,
    id: &str,
    reservation: &V1Reservation,
    namespace: &str,
) -> Result<()> {
    let path = service.reservation_path(id, Some(namespace));
    if let Some(existing) = read_v1_reservation(service, id, namespace)? {
        if existing == *reservation {
            refresh_v1_reservation_claim(service, id, namespace, reservation)?;
            return Ok(());
        }
        anyhow::bail!("v1 cache reservation does not match existing reservation");
    }

    let generation = uuid::Uuid::new_v4().simple().to_string();
    if atomically_create_json(
        &path,
        &reservation.as_json_with_generation(&generation, now_unix_millis()?),
    )? {
        return Ok(());
    }

    let existing = read_v1_reservation(service, id, namespace)?
        .context("v1 cache reservation disappeared during creation")?;
    if existing == *reservation {
        refresh_v1_reservation_claim(service, id, namespace, reservation)?;
        Ok(())
    } else {
        anyhow::bail!("v1 cache reservation does not match concurrent reservation")
    }
}

/// Replace V1 reservation data without dropping its generation, active
/// request-attempt lease, or update timestamp.
fn update_v1_reservation(
    service: &CacheService,
    id: &str,
    namespace: &str,
    reservation: &V1Reservation,
) -> Result<()> {
    let path = service.reservation_path(id, Some(namespace));
    let Some((mut current, _)) = read_reservation_record(service, id, Some(namespace))? else {
        anyhow::bail!("v1 cache reservation disappeared during update");
    };
    if parse_v1_reservation(current.clone(), id)?.cache_id != reservation.cache_id {
        anyhow::bail!("v1 cache reservation changed during update");
    }
    let replacement = reservation.as_json();
    for name in [
        "key",
        "version",
        "cacheId",
        "cacheSize",
        "uploadChunks",
        "commitAttempt",
        "activeUploads",
    ] {
        current[name] = replacement[name].clone();
    }
    current["updatedMs"] = json!(now_unix_millis()?);
    replace_json_atomically(&path, &current)
}

fn clear_v1_reservation(service: &CacheService, id: &str, namespace: &str) -> Result<()> {
    let path = service.reservation_path(id, Some(namespace));
    if has_live_upload_temp_for_id(&service.tenant_root(Some(namespace)), id)? {
        return Err(
            anyhow::Error::new(CacheLockBusy).context("v1 cache upload body is still active")
        );
    }
    if let Some((record, _)) = read_reservation_record(service, id, Some(namespace))? {
        if record.get("protocol").is_some() {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        let reservation = parse_v1_reservation(record, id)?;
        remove_v1_cache_id_index(service, namespace, reservation.cache_id, id)?;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove published v1 cache reservation {id}"));
        }
    }
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    let mut outcome = UploadTempCleanup::default();
    remove_upload_parts_directory(&service.tenant_root(Some(namespace)), id, &mut outcome)?;
    Ok(())
}

fn parse_v2_reservation(value: Value, id: &str, file_mtime_ms: u64) -> Result<V2Reservation> {
    if value["protocol"].as_str() != Some("v2") {
        anyhow::bail!("cache reservation is not a v2 claim");
    }
    let key = value["key"]
        .as_str()
        .context("v2 cache reservation has no key")?;
    let version = value["version"]
        .as_str()
        .context("v2 cache reservation has no version")?;
    let upload_nonce = value["uploadNonce"]
        .as_str()
        .context("v2 cache reservation has no upload nonce")?;
    let updated_ms = match value.get("updatedMs") {
        Some(updated_ms) => updated_ms
            .as_u64()
            .context("v2 cache reservation has an invalid update time")?,
        // Claims written before expiry metadata was added use their atomic
        // publication time as the recovery age, so one abandoned claim cannot
        // pin a key forever across a service upgrade.
        None => file_mtime_ms,
    };
    let block_id_bytes = match value.get("blockIdBytes") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            usize::try_from(
                value
                    .as_u64()
                    .context("v2 reservation has invalid block ID length")?,
            )
            .context("v2 block ID length overflow")?,
        ),
    };
    if entry_hash(key, version) != id {
        anyhow::bail!("v2 cache reservation does not match cache id");
    }
    if upload_nonce.len() != 32 || !upload_nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("v2 cache reservation has an invalid upload nonce");
    }
    let upload_attempt = match value.get("uploadAttempt") {
        None | Some(Value::Null) => None,
        Some(Value::String(attempt))
            if attempt.len() == 32 && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(attempt.clone())
        }
        Some(_) => anyhow::bail!("v2 cache reservation has an invalid upload attempt"),
    };
    let assembly_scratch_attempt = match value.get("assemblyScratchAttempt") {
        None | Some(Value::Null) => None,
        Some(Value::String(attempt))
            if attempt.len() == 32 && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(attempt.clone())
        }
        Some(_) => anyhow::bail!("v2 cache reservation has an invalid assembly scratch attempt"),
    };
    let assembly_scratch_bytes = match value.get("assemblyScratchBytes") {
        None => 0,
        Some(bytes) => bytes
            .as_u64()
            .context("v2 cache reservation has an invalid assembly scratch byte count")?,
    };
    if assembly_scratch_attempt.is_some() != (assembly_scratch_bytes > 0) {
        anyhow::bail!("v2 cache reservation has inconsistent assembly scratch metadata");
    }
    let mut upload_blocks = Vec::new();
    if let Some(blocks) = value.get("uploadBlocks").and_then(Value::as_array) {
        if blocks.len() > MAX_V2_UNCOMMITTED_BLOCKS {
            anyhow::bail!("v2 cache reservation exceeds Azure's uncommitted block limit");
        }
        for block in blocks {
            let block_id = block["blockId"]
                .as_str()
                .context("v2 upload block has no id")?;
            let attempt = block["attempt"]
                .as_str()
                .context("v2 upload block has no attempt")?;
            let size = block["size"]
                .as_u64()
                .context("v2 upload block has no size")?;
            let sha256 = block["sha256"]
                .as_str()
                .context("v2 upload block has no digest")?;
            if block_id.is_empty()
                || block_id.len() > 128
                || attempt.len() != 32
                || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit())
                || sha256.len() != 64
                || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                anyhow::bail!("v2 cache reservation contains an invalid upload block");
            }
            upload_blocks.push(V2UploadBlock {
                block_id: block_id.to_owned(),
                attempt: attempt.to_owned(),
                size,
                sha256: sha256.to_owned(),
            });
        }
    } else if value
        .get("uploadBlocks")
        .is_some_and(|value| !value.is_null())
    {
        anyhow::bail!("v2 cache reservation has an invalid uploadBlocks list");
    }
    if block_id_bytes.is_some_and(|length| !(1..=64).contains(&length)) {
        anyhow::bail!("v2 cache reservation has an invalid block ID length");
    }
    let first_block_id_bytes = upload_blocks
        .first()
        .map(|block| {
            base64::engine::general_purpose::STANDARD
                .decode(&block.block_id)
                .map(|decoded| decoded.len())
        })
        .transpose()
        .context("v2 cache reservation has an invalid block ID")?;
    let block_id_bytes = block_id_bytes.or(first_block_id_bytes);
    if block_id_bytes.is_some_and(|length| {
        upload_blocks.iter().any(|block| {
            base64::engine::general_purpose::STANDARD
                .decode(&block.block_id)
                .map_or(true, |decoded| decoded.len() != length)
        })
    }) {
        anyhow::bail!("v2 cache reservation mixes Azure block ID lengths");
    }
    Ok(V2Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        upload_nonce: upload_nonce.to_owned(),
        block_id_bytes,
        upload_attempt,
        upload_blocks,
        active_uploads: parse_attempt_list(&value, "activeUploads")?,
        assembly_scratch_attempt,
        assembly_scratch_bytes,
        updated_ms,
    })
}

fn read_v2_reservation(
    service: &CacheService,
    id: &str,
    namespace: &str,
) -> Result<Option<V2Reservation>> {
    let Some((value, file_mtime_ms)) = read_reservation_record(service, id, Some(namespace))?
    else {
        return Ok(None);
    };
    parse_v2_reservation(value, id, file_mtime_ms).map(Some)
}

fn clear_v2_reservation(
    service: &CacheService,
    id: &str,
    namespace: &str,
    upload_nonce: &str,
) -> Result<()> {
    let Some(current) = read_v2_reservation(service, id, namespace)? else {
        return Ok(());
    };
    if current.upload_nonce != upload_nonce {
        anyhow::bail!("refusing to clear a different v2 cache reservation");
    }
    if has_live_upload_temp_for_id(&service.tenant_root(Some(namespace)), id)? {
        return Err(
            anyhow::Error::new(CacheLockBusy).context("v2 cache upload body is still active")
        );
    }
    let path = service.reservation_path(id, Some(namespace));
    let assembly_scratch_attempt = current
        .assembly_scratch_attempt
        .as_deref()
        .or(current.upload_attempt.as_deref());
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("remove published v2 cache reservation"),
    }
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    let mut outcome = UploadTempCleanup::default();
    remove_upload_parts_directory(&service.tenant_root(Some(namespace)), id, &mut outcome)?;
    if let Some(attempt) = assembly_scratch_attempt {
        release_global_assembly_scratch(&service.root, Some(namespace), id, attempt)?;
    }
    Ok(())
}

fn v2_reservation_is_expired(reservation: &V2Reservation, now_ms: u64) -> bool {
    now_ms.saturating_sub(reservation.updated_ms) > V2_RESERVATION_TTL_MS
}

fn remove_unpublished_cache_blob(service: &CacheService, id: &str, namespace: &str) -> Result<()> {
    // A committed entry owns its blob permanently. Recovery only removes a
    // canonical blob whose entry was never published.
    let entry = service.entry_path(id, Some(namespace));
    match std::fs::symlink_metadata(&entry) {
        Ok(_) => anyhow::bail!("refusing to remove a blob for a published v2 cache entry"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("stat possible published v2 cache entry"),
    }
    let blob = service.tenant_root(Some(namespace)).join("blobs").join(id);
    let metadata = match std::fs::symlink_metadata(&blob) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("stat abandoned v2 cache blob"),
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!("abandoned v2 cache blob is not a regular file");
    }
    std::fs::remove_file(&blob).context("remove abandoned cache blob")?;
    if let Some(parent) = blob.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn begin_v2_upload_attempt(
    service: &CacheService,
    id: &str,
    namespace: &str,
    upload_nonce: &str,
    attempt: &str,
) -> Result<(V2Reservation, String)> {
    let Some(current) = read_v2_reservation(service, id, namespace)? else {
        anyhow::bail!("v2 cache reservation is missing or already finalized");
    };
    if current.upload_nonce != upload_nonce {
        anyhow::bail!("v2 cache reservation has a stale claim nonce");
    }
    if service.entry_path(id, Some(namespace)).exists() {
        anyhow::bail!("v2 cache entry already exists");
    }
    let updated = V2Reservation {
        upload_attempt: Some(attempt.to_owned()),
        updated_ms: now_unix_millis()?,
        ..current
    };
    replace_json_atomically(
        &service.reservation_path(id, Some(namespace)),
        &updated.as_json(),
    )?;
    Ok((updated, attempt.to_owned()))
}

fn complete_v2_upload_attempt(
    service: &CacheService,
    id: &str,
    namespace: &str,
    upload_nonce: &str,
    attempt: &str,
) -> Result<()> {
    let Some(current) = read_v2_reservation(service, id, namespace)? else {
        anyhow::bail!("v2 cache reservation disappeared during upload");
    };
    if current.upload_nonce != upload_nonce || current.upload_attempt.as_deref() != Some(attempt) {
        anyhow::bail!("v2 cache reservation changed during upload");
    }
    let updated = V2Reservation {
        upload_attempt: None,
        updated_ms: now_unix_millis()?,
        ..current
    };
    replace_json_atomically(
        &service.reservation_path(id, Some(namespace)),
        &updated.as_json(),
    )?;
    Ok(())
}

fn clear_upload_attempt_if_matches(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
    attempt: &str,
) -> Result<()> {
    clear_upload_attempt_if_matches_with_sync(service, id, namespace, attempt, sync_directory)
}

fn clear_upload_attempt_if_matches_with_sync<F>(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
    attempt: &str,
    mut sync: F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let Some((mut record, modified_ms)) = read_reservation_record(service, id, namespace)? else {
        return Ok(());
    };
    match record.get("protocol").and_then(Value::as_str) {
        Some("v2") => {
            let mut reservation = parse_v2_reservation(record, id, modified_ms)?;
            let matched = reservation.upload_attempt.as_deref() == Some(attempt)
                || reservation
                    .active_uploads
                    .iter()
                    .any(|value| value == attempt);
            if !matched {
                return Ok(());
            }
            // A crash can land after publication but before the reservation
            // records the scratch attempt. Reconcile that cut while the root
            // scratch lock prevents another admission from missing its charge.
            let _scratch_budget_lock = lock_cache_assembly_scratch_blocking(&service.root)?;
            let scratch_path = assembly_scratch_lease_path(&service.root, namespace, attempt);
            let scratch_record = match std::fs::read(&scratch_path) {
                Ok(raw) => Some(
                    serde_json::from_slice::<Value>(&raw)
                        .context("parse interrupted assembly scratch lease")?,
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("read interrupted assembly scratch lease"),
            };
            if let Some(scratch_record) = scratch_record {
                let (recorded_namespace, recorded_id, recorded_attempt, scratch_bytes) =
                    parse_assembly_scratch_record(&service.root, &scratch_path, &scratch_record)?;
                if recorded_namespace != namespace
                    || recorded_id != id
                    || recorded_attempt != attempt
                {
                    anyhow::bail!("assembly scratch lease identity changed");
                }
                let artifacts = assembly_scratch_artifacts(&service.root, namespace, id, attempt)?;
                if artifacts.canonical_with_sources {
                    reservation.assembly_scratch_attempt = Some(attempt.to_owned());
                    reservation.assembly_scratch_bytes = scratch_bytes;
                } else if !artifacts.temporary_assembly {
                    remove_global_assembly_scratch_lease_locked_with_sync(
                        &service.root,
                        namespace,
                        id,
                        attempt,
                        &mut sync,
                    )?;
                    if reservation.assembly_scratch_attempt.as_deref() == Some(attempt) {
                        reservation.assembly_scratch_attempt = None;
                        reservation.assembly_scratch_bytes = 0;
                    }
                }
            }
            if reservation.upload_attempt.as_deref() == Some(attempt) {
                reservation.upload_attempt = None;
            }
            reservation.active_uploads.retain(|value| value != attempt);
            if reservation.upload_blocks.is_empty() && reservation.active_uploads.is_empty() {
                reservation.block_id_bytes = None;
            }
            reservation.updated_ms = now_unix_millis()?;
            let path = service.reservation_path(id, namespace);
            replace_json_atomically(&path, &reservation.as_json())?;
        }
        None => {
            let mut reservation = parse_v1_reservation(record.clone(), id)?;
            let matched = record["uploadAttempt"].as_str() == Some(attempt)
                || record["commitAttempt"].as_str() == Some(attempt)
                || reservation
                    .active_uploads
                    .iter()
                    .any(|value| value == attempt);
            if !matched {
                return Ok(());
            }
            if reservation.commit_attempt.as_deref() == Some(attempt) {
                reservation.commit_attempt = None;
            }
            if record["uploadAttempt"].as_str() == Some(attempt) {
                record["uploadAttempt"] = Value::Null;
            }
            record["commitAttempt"] = json!(reservation.commit_attempt);
            reservation.active_uploads.retain(|value| value != attempt);
            let replacement = reservation.as_json();
            record["activeUploads"] = replacement["activeUploads"].clone();
            record["updatedMs"] = json!(now_unix_millis()?);
            replace_json_atomically(&service.reservation_path(id, namespace), &record)?;
        }
        Some(_) => {}
    }
    Ok(())
}

/// Reconcile reservation markers against their per-attempt temp leases while
/// the caller holds the shared entry shard. Missing or unlocked temps are
/// crash debris; a held flock proves that the corresponding body/assembly is
/// still active and keeps its marker intact.
fn reconcile_inactive_upload_markers(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
) -> Result<bool> {
    reconcile_inactive_upload_markers_with_sync(service, id, namespace, sync_directory)
}

fn reconcile_inactive_upload_markers_with_sync<F>(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
    mut sync: F,
) -> Result<bool>
where
    F: FnMut(&Path) -> Result<()>,
{
    let Some((record, modified_ms)) = read_reservation_record(service, id, namespace)? else {
        return Ok(false);
    };
    let mut attempts = Vec::new();
    match record.get("protocol").and_then(Value::as_str) {
        Some("v2") => {
            let reservation = parse_v2_reservation(record.clone(), id, modified_ms)?;
            attempts.extend(reservation.active_uploads);
            attempts.extend(reservation.upload_attempt);
        }
        None => {
            let reservation = parse_v1_reservation(record.clone(), id)?;
            attempts.extend(reservation.active_uploads);
            attempts.extend(record["uploadAttempt"].as_str().map(ToOwned::to_owned));
            attempts.extend(reservation.commit_attempt);
        }
        Some(_) => return Ok(false),
    }
    attempts.sort_unstable();
    attempts.dedup();

    let blobs = service.tenant_root(namespace).join("blobs");
    let mut busy = false;
    for attempt in attempts {
        if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let temp = blobs.join(format!(".{id}.{attempt}.tmp"));
        let metadata = match std::fs::symlink_metadata(&temp) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => {
                busy = true; // Do not clear a lease through an unexpected path type.
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                clear_upload_attempt_if_matches_with_sync(
                    service, id, namespace, &attempt, &mut sync,
                )?;
                continue;
            }
            Err(error) => return Err(error).context("stat cache upload attempt lease"),
        };
        let _ = metadata;
        let Some(temp_lock) = try_lock_upload_temp(&temp)? else {
            busy = true;
            continue;
        };
        match std::fs::remove_file(&temp) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove inactive cache upload lease"),
        }
        drop(temp_lock);
        clear_upload_attempt_if_matches_with_sync(service, id, namespace, &attempt, &mut sync)?;
    }
    Ok(busy)
}

/// Caller holds the entry stripe. Retire an interrupted attempt before a retry
/// starts; a held temp flock means its body is still active, so leave the claim
/// intact and return retryable busy.
#[cfg(test)]
fn retire_inactive_upload_attempt(service: &CacheService, id: &str, namespace: &str) -> Result<()> {
    let Some((record, modified_ms)) = read_reservation_record(service, id, Some(namespace))? else {
        return Ok(());
    };
    let attempt = match record.get("protocol").and_then(Value::as_str) {
        Some("v2") => parse_v2_reservation(record, id, modified_ms)?.upload_attempt,
        None => record
            .get("uploadAttempt")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        Some(_) => None,
    };
    let Some(attempt) = attempt else {
        return Ok(());
    };
    let temp = service
        .tenant_root(Some(namespace))
        .join("blobs")
        .join(format!(".{id}.{attempt}.tmp"));
    match std::fs::symlink_metadata(&temp) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            clear_upload_attempt_if_matches(service, id, Some(namespace), &attempt)?;
        }
        Err(error) => return Err(error).context("stat previous cache upload attempt"),
        Ok(metadata) if !metadata.file_type().is_file() => {
            anyhow::bail!("previous cache upload temp is not a regular file");
        }
        Ok(_) => {
            let Some(_temp_lock) = try_lock_upload_temp(&temp)? else {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("previous cache upload body is still active"));
            };
            match std::fs::remove_file(&temp) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove inactive cache upload temp"),
            }
            clear_upload_attempt_if_matches(service, id, Some(namespace), &attempt)?;
        }
    }
    Ok(())
}

fn cache_namespace(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-runner-cache-tenant\0");
    hasher.update(token.as_bytes());
    hex(&hasher.finalize())
}

/// Repository identity a job's cache requests are scoped to: the canonical
/// server-origin/repository-ID key, the full ref it runs on, and — for pull requests —
/// the full base ref it may restore from — plus the job's trust class, which
/// decides whether the job writes the base namespaces or an isolated fork
/// one. Validation failures never fail the job; the caller treats them as "no
/// usable identity" and the job's requests fall back to an isolated per-token
/// namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheIdentity {
    repository_key: String,
    git_ref: String,
    base_ref: Option<String>,
    trust: TrustClass,
}

impl CacheIdentity {
    pub(crate) fn for_repository(
        server_url: &str,
        repository_id: &str,
        git_ref: &str,
        base_ref: Option<&str>,
        trust: TrustClass,
    ) -> Result<Self> {
        let repository_key = crate::store_catalog::repository_store_key(server_url, repository_id)
            .context("cache identity has no canonical server and repository ID")?;
        Self::from_repository_key(&repository_key, git_ref, base_ref, trust)
    }

    fn from_repository_key(
        repository_key: &str,
        git_ref: &str,
        base_ref: Option<&str>,
        trust: TrustClass,
    ) -> Result<Self> {
        let digest = repository_key
            .strip_prefix(REPOSITORY_KEY_PREFIX)
            .unwrap_or_default();
        if digest.len() != REPOSITORY_KEY_HEX_LEN
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            anyhow::bail!("cache identity repository key is not usable");
        }
        let git_ref = git_ref.trim().to_owned();
        if git_ref.is_empty()
            || git_ref.len() > MAX_REF_LEN
            || git_ref.chars().any(char::is_control)
        {
            anyhow::bail!("cache identity ref is not usable");
        }
        // `github.base_ref` is a short branch name (`main`); refs in jobs are
        // full (`refs/heads/main`, `refs/pull/123/merge`). Normalize the base
        // the same way so both hash stably, and drop it when it adds no scope.
        let base_ref = base_ref
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .map(|base| {
                if base.starts_with("refs/") {
                    base.to_owned()
                } else {
                    format!("refs/heads/{base}")
                }
            })
            .filter(|base| *base != git_ref);
        if let Some(base) = &base_ref
            && (base.len() > MAX_REF_LEN || base.chars().any(char::is_control))
        {
            anyhow::bail!("cache identity base ref is not usable");
        }
        Ok(Self {
            repository_key: repository_key.to_ascii_lowercase(),
            git_ref,
            base_ref,
            trust,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_identity(
        repository_label: &str,
        git_ref: &str,
        base_ref: Option<&str>,
        trust: TrustClass,
    ) -> Result<Self> {
        let mut hasher = Sha256::new();
        hasher.update(b"velnor-runner-cache-test-repository\0");
        hasher.update(repository_label.trim().to_ascii_lowercase().as_bytes());
        let digest = hasher.finalize();
        let digest_bytes: [u8; 8] = digest[..8].try_into().context("fixed digest length")?;
        let id = u64::from_be_bytes(digest_bytes) | 1;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", &id.to_string())
                .context("test repository identity is valid")?;
        Self::from_repository_key(&repository_key, git_ref, base_ref, trust)
    }

    /// Lookup chain in scope order. A trusted job searches its own ref first,
    /// then its base. A fork-PR or unknown job searches its isolated fork
    /// namespace first, then the same base namespaces for read-through — and
    /// since every write path lands in the chain head, its writes can only
    /// ever reach the fork namespace. The `repo-`/`fork-` prefixes keep repo
    /// and fork scopes disjoint from each other and from the bare-hex
    /// isolated per-token namespaces by construction.
    fn namespaces(&self) -> Vec<String> {
        let mut chain = Vec::new();
        if !self.trust.is_trusted() {
            chain.push(fork_namespace(&self.repository_key, &self.git_ref));
        }
        chain.push(repo_namespace(&self.repository_key, &self.git_ref));
        if let Some(base) = &self.base_ref {
            chain.push(repo_namespace(&self.repository_key, base));
        }
        chain
    }
}

fn repo_namespace(repository_key: &str, git_ref: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-runner-cache-repo\0");
    hasher.update(repository_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(git_ref.as_bytes());
    format!("repo-{}", hex(&hasher.finalize()))
}

/// Isolated write scope for fork-PR and unknown jobs. Same inputs as
/// [`repo_namespace`] but a distinct hash domain and prefix, so a fork
/// namespace can never collide with a base namespace even for the same
/// repository and ref — including a fork-controlled run whose ref *is* a base
/// branch. Trusted jobs never resolve here (see [`CacheIdentity::namespaces`]).
fn fork_namespace(repository_key: &str, git_ref: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-runner-cache-fork\0");
    hasher.update(repository_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(git_ref.as_bytes());
    format!("fork-{}", hex(&hasher.finalize()))
}

/// Parse a session `trust` label back to its class. The labels are
/// [`TrustClass::as_str`]; anything else fails the session closed — the
/// token falls back to its isolated per-token namespace like a corrupt
/// session, it never inherits a trust it did not parse to.
fn parse_trust(label: &str) -> Result<TrustClass> {
    match label {
        "trusted" => Ok(TrustClass::Trusted),
        "fork-pr" => Ok(TrustClass::ForkPR),
        "unknown" => Ok(TrustClass::Unknown),
        _ => anyhow::bail!("cache session trust label is not usable"),
    }
}

/// Durable token-to-identity binding written by
/// [`register_job_cache_session`]. The token itself never touches disk; the
/// file name is its hash, which doubles as the isolated-namespace name when no
/// binding exists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheSession {
    identity: CacheIdentity,
    registered_ms: u64,
}

impl CacheSession {
    fn as_json(&self) -> Value {
        json!({
            "repositoryKey": self.identity.repository_key,
            "ref": self.identity.git_ref,
            "baseRef": self.identity.base_ref,
            "trust": self.identity.trust.as_str(),
            "registeredMs": self.registered_ms,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let repository_key = value["repositoryKey"]
            .as_str()
            .context("cache session has no canonical repository key")?;
        let git_ref = value["ref"].as_str().context("cache session has no ref")?;
        let base_ref = value["baseRef"].as_str();
        // No default: a session written before trust existed (or with a
        // label this build does not know) must not inherit trust. Failing
        // the parse sends the token to its isolated per-token namespace —
        // the same fail-closed path as a corrupt session — and the next
        // registration cannot rewrite the immutable binding and will instead
        // poison it when its identity differs or is unreadable.
        let trust = value["trust"]
            .as_str()
            .context("cache session has no trust")?;
        let registered_ms = value["registeredMs"].as_u64().unwrap_or(0);
        Ok(Self {
            identity: CacheIdentity::from_repository_key(
                repository_key,
                git_ref,
                base_ref,
                parse_trust(trust)?,
            )?,
            registered_ms,
        })
    }
}

/// Bind a job's runtime token to its repository identity so the job's cache
/// requests resolve to the shared repo namespaces instead of an isolated
/// per-token one. The slot process calls this before any job process can issue
/// a cache request; the file registry (not memory) carries the binding across
/// to the daemon process hosting the HTTP service, and across restarts.
/// Bindings are immutable. Re-registering the same identity is idempotent;
/// attempting to bind a token to another identity poisons the token to its
/// isolated fallback namespace.
///
/// Pruning rides along: every registration sweeps session and marker files
/// older than [`SESSION_TTL`], so one tiny file per job token stays bounded
/// without a timer. The sweep is best-effort; only the binding write itself
/// can fail this call.
pub(crate) fn register_job_cache_session(
    root: &Path,
    token: &str,
    identity: &CacheIdentity,
) -> Result<()> {
    if token.is_empty() {
        anyhow::bail!("cannot register a cache session without a token");
    }
    let _registry = lock_cache_tenant_registry(root)?;
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&sessions).context("create gha-cache sessions dir")?;
    let now = std::time::SystemTime::now();
    prune_stale_sessions(&sessions, now);
    let token_hash = cache_namespace(token);
    let session = CacheSession {
        identity: identity.clone(),
        registered_ms: now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    let path = sessions.join(format!("{token_hash}.json"));
    let conflict_path = sessions.join(format!("{token_hash}.conflict"));
    let isolated_path = sessions.join(format!("{token_hash}.isolated"));
    for (marker, description) in [
        (&conflict_path, "conflict"),
        (&isolated_path, "isolated fallback"),
    ] {
        match std::fs::symlink_metadata(marker) {
            Ok(_) => {
                if description == "isolated fallback" {
                    poison_session_binding(&sessions, &token_hash)
                        .context("poison previously isolated cache session")?;
                }
                anyhow::bail!("cache session token binding is {description}-poisoned");
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect cache session fallback marker"),
        }
    }
    match atomically_create_json(&path, &session.as_json()) {
        Ok(true) => {
            prune_stale_sessions(&sessions, std::time::SystemTime::now());
            return Ok(());
        }
        Ok(false) => {}
        Err(error) => {
            let _ = poison_session_binding(&sessions, &token_hash);
            return Err(error).context("publish immutable cache session binding");
        }
    }

    if let Some(existing) = read_session(root, &token_hash)
        && existing.identity == session.identity
    {
        prune_stale_sessions(&sessions, std::time::SystemTime::now());
        return Ok(());
    }

    // Publish poison before removing the old session. Readers always inspect
    // the marker first, so no request can revive a conflicting binding.
    poison_session_binding(&sessions, &token_hash)
        .context("poison conflicting cache session binding")?;
    anyhow::bail!("cache session token was already bound to another identity")
}

/// Invalidate any existing binding after registration cannot establish the
/// identity for this admitted token. This prevents a reused token from
/// inheriting a previous job's repository while all readers fall back to the
/// token-hash namespace.
pub(crate) fn invalidate_job_cache_session(root: &Path, token: &str) -> Result<()> {
    if token.is_empty() {
        anyhow::bail!("cannot invalidate a cache session without a token");
    }
    let _registry = lock_cache_tenant_registry(root)?;
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&sessions).context("create gha-cache sessions dir")?;
    let now = std::time::SystemTime::now();
    prune_stale_sessions(&sessions, now);
    let token_hash = cache_namespace(token);
    poison_session_binding(&sessions, &token_hash).context("invalidate cache session binding")
}

fn poison_session_binding(sessions: &Path, token_hash: &str) -> Result<()> {
    let session_path = sessions.join(format!("{token_hash}.json"));
    let conflict_path = sessions.join(format!("{token_hash}.conflict"));
    let conflict_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    if let Err(error) =
        atomically_create_json(&conflict_path, &json!({ "conflictMs": conflict_ms }))
    {
        // Even if the poison record cannot be published, remove the old
        // binding so subsequent readers fall back to the token namespace.
        let _ = std::fs::remove_file(&session_path);
        let _ = sync_directory(sessions);
        return Err(error);
    }
    let _ = std::fs::remove_file(session_path);
    sync_directory(sessions).context("sync cache sessions dir after conflict")?;
    Ok(())
}

pub(crate) fn job_cache_fallback_namespace(token: &str) -> String {
    cache_namespace(token)
}

fn read_session(root: &Path, token_hash: &str) -> Option<CacheSession> {
    let sessions = root.join("sessions");
    for suffix in ["conflict", "isolated"] {
        match std::fs::symlink_metadata(sessions.join(format!("{token_hash}.{suffix}"))) {
            Ok(_) => return None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    let raw = std::fs::read(sessions.join(format!("{token_hash}.json"))).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    CacheSession::parse(value).ok()
}

/// Resolve the cache lookup chain for a job before publishing namespace
/// leases. The daemon service uses this same path so GC protects every scope
/// it can read, including the isolated token namespace when a session is
/// missing or invalid.
pub(crate) fn resolve_job_cache_namespaces(root: &Path, token: &str) -> Vec<String> {
    let fallback = job_cache_fallback_namespace(token);
    read_session(root, fallback.as_str())
        .map(|session| session.identity.namespaces())
        .unwrap_or_else(|| vec![fallback])
}

/// Delete session bindings and fallback markers older than [`SESSION_TTL_SECS`].
/// Best-effort and conservative: only `<64-hex>.json`/`.isolated`/`.conflict` files are
/// candidates, and anything without a readable past age is kept. `now` is a
/// parameter so tests age files without touching mtimes.
fn prune_stale_sessions(sessions: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return;
    };
    for file in entries.flatten() {
        let path = file.path();
        let is_candidate = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let stem = name
                    .strip_suffix(".json")
                    .or_else(|| name.strip_suffix(".isolated"))
                    .or_else(|| name.strip_suffix(".conflict"));
                stem.is_some_and(|stem| {
                    stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit())
                })
            });
        if !is_candidate {
            continue;
        }
        let stale = file
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age.as_secs() >= SESSION_TTL_SECS);
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl CacheService {
    /// Resolve a request token without creating fallback marker or tenant
    /// state. A valid write later refreshes its fallback marker only after
    /// request syntax has been checked; isolated reads and misses stay inert.
    fn resolve_namespaces(&self, token: &str) -> Vec<String> {
        resolve_job_cache_namespaces(&self.root, token)
    }
}

async fn serve_with_public_base(
    listener: tokio::net::TcpListener,
    service: CacheService,
    public_base: String,
) -> Result<()> {
    let service = Arc::new(service);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let ctx = Arc::new(Ctx {
            service: Arc::clone(&service),
            public_base: public_base.clone(),
        });
        tokio::task::spawn(async move {
            let handler = service_fn(move |req| {
                let ctx = Arc::clone(&ctx);
                async move {
                    let mut ctx = Ctx {
                        service: Arc::clone(&ctx.service),
                        public_base: ctx.public_base.clone(),
                    };
                    route(req, &mut ctx).await
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, handler)
                .await;
        });
    }
}

async fn route<B>(req: Request<B>, ctx: &mut Ctx) -> Result<Response<ResponseBody>, hyper::Error>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let respond = |status: StatusCode, body: Value| {
        #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(full_body(body.to_string()))
            .unwrap()
    };
    if !cache_route_supported(&method, &path) {
        return Ok(respond(
            StatusCode::NOT_FOUND,
            json!({"message": "not found"}),
        ));
    }
    // Azure clients use the signed URL directly and do not forward the
    // Actions Runtime bearer. Read URLs carry an HMAC capability scoped to
    // one entry, namespace, method set, and expiry. Upload URLs carry the
    // random v2 reservation nonce plus its namespace; the claim is rechecked
    // again under the entry shard before any body is accepted.
    let capability_scope = if let Some(id) = v2_download_id(&path) {
        query_param(&req, "sig")
            .and_then(|capability| ctx.service.verify_read_capability(&capability, id, &method))
    } else if method == hyper::Method::PUT {
        v2_upload_id(&path).and_then(|id| {
            let namespace = query_param(&req, "scope")?;
            let claim = query_param(&req, "sig")?;
            verify_v2_upload_capability(&ctx.service, id, &namespace, &claim, &method)
        })
    } else {
        None
    };
    // Bearer remains mandatory for every ordinary route. Only the exact
    // capability resource/method combinations above may bypass it.
    let (scope, activity_guards) = if let Some(namespace) = capability_scope {
        let activity = match lock_cache_activity(&ctx.service.root, &namespace).await {
            Ok(activity) => activity,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some() => {
                return Ok(respond(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"message": "cache is busy; retry"}),
                ));
            }
            Err(error) => {
                eprintln!("Warning: gha cache activity lock: {error:#}");
                return Ok(respond(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"message": "internal error"}),
                ));
            }
        };
        (vec![namespace], vec![activity])
    } else {
        let token = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned);
        let Some(token) = token else {
            return Ok(respond_unauthorized());
        };
        let registry = match lock_cache_tenant_registry(&ctx.service.root) {
            Ok(lock) => lock,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some() => {
                return Ok(respond(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"message": "cache is busy; retry"}),
                ));
            }
            Err(error) => {
                eprintln!("Warning: gha cache registry lock: {error:#}");
                return Ok(respond(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"message": "internal error"}),
                ));
            }
        };
        let scope = ctx.service.resolve_namespaces(&token);
        let mut activity = Vec::with_capacity(scope.len());
        for namespace in &scope {
            match try_lock_cache_activity_shared_at(&ctx.service.root, namespace) {
                Ok(Some(guard)) => activity.push(guard),
                Ok(None) => {
                    return Ok(respond(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({"message": "cache is busy; retry"}),
                    ));
                }
                Err(error) => {
                    eprintln!("Warning: gha cache activity lock: {error:#}");
                    return Ok(respond(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"message": "internal error"}),
                    ));
                }
            }
        }
        if let Err(error) = ctx
            .service
            .refresh_registered_session_activity_locked(&token)
        {
            eprintln!("Warning: gha cache session activity refresh: {error:#}");
            return Ok(respond(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"message": "internal error"}),
            ));
        }
        drop(registry);
        (scope, activity)
    };
    let chain: Vec<&str> = scope.iter().map(String::as_str).collect();
    // Writes always land in the job's own scope head (its ref namespace, its
    // fork namespace when the job is fork-PR or unknown, or its isolated
    // namespace); only reads walk the chain into the base scope.
    let primary = chain[0];
    if let Err(error) = ctx.service.cleanup_stale_upload_temps(false) {
        eprintln!("Warning: gha cache request cleanup: {error:#}");
        return Ok(hold_cache_activity(
            respond(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"message": "internal error"}),
            ),
            activity_guards,
        ));
    }
    let internal_error = || {
        respond(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"message": "internal error"}),
        )
    };

    let result = match (method, path.as_str()) {
        (hyper::Method::POST, p) if p.ends_with("/_apis/artifactcache/caches") => {
            reserve(req, ctx, primary).await.map(|v| {
                respond(
                    StatusCode::OK,
                    json!({"cacheId": v["cacheID"], "cacheID": v["cacheID"]}),
                )
            })
        }
        (hyper::Method::PATCH, p) if v1_cache_resource_id(p).is_some() => {
            let cache_id = v1_cache_resource_id(p).unwrap_or_default().to_owned();
            upload_v1_chunk(req, ctx, primary, &cache_id)
                .await
                .map(|_| empty_response(StatusCode::NO_CONTENT))
        }
        (hyper::Method::POST, p) if v1_cache_resource_id(p).is_some() => {
            let cache_id = v1_cache_resource_id(p).unwrap_or_default().to_owned();
            commit_v1(req, ctx, primary, &cache_id)
                .await
                .map(|_| respond(StatusCode::OK, json!({})))
        }
        (hyper::Method::PUT, p) if v2_upload_id(p).is_some() => {
            let id = v2_upload_id(p).unwrap_or_default().to_owned();
            let upload_nonce = query_param(&req, "sig");
            let component = query_param(&req, "comp");
            match component.as_deref() {
                Some("block") => stage_v2_block(req, ctx, primary, &id, upload_nonce.as_deref())
                    .await
                    .map(|_| azure_response(StatusCode::CREATED)),
                Some("blocklist") => {
                    commit_v2_block_list(req, ctx, primary, &id, upload_nonce.as_deref())
                        .await
                        .map(|_| azure_response(StatusCode::CREATED))
                }
                None => upload_v2_blob(req, ctx, &id, primary, upload_nonce.as_deref())
                    .await
                    .map(|_| azure_response(StatusCode::CREATED)),
                Some(_) => Ok(respond(
                    StatusCode::BAD_REQUEST,
                    json!({"message":"unsupported blob operation"}),
                )),
            }
        }
        (hyper::Method::GET, p) if p.ends_with("/_apis/artifactcache/cache") => {
            lookup_v1(&req, ctx, &chain).map(|entry| match entry {
                Some(entry) => respond(StatusCode::OK, entry),
                // Proof: `body()` fails only on an invalid status/header;
                // the status is a const and no headers are set.
                #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
                None => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(full_body(Bytes::new()))
                    .unwrap(),
            })
        }
        (hyper::Method::GET, p) if v2_download_id(p).is_some() => {
            let id = v2_download_id(p).unwrap_or_default();
            let range = req
                .headers()
                .get("x-ms-range")
                .or_else(|| req.headers().get(RANGE))
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            download_range_chain(&ctx.service, id, &chain, range.as_deref())
                .await
                .map(download_response)
        }
        (hyper::Method::HEAD, p) if v2_download_id(p).is_some() => {
            let id = v2_download_id(p).unwrap_or_default();
            download_size_chain(&ctx.service, id, &chain)
                .await
                .map(azure_head_response)
        }
        (hyper::Method::POST, p) if v2_twirp_path(p, "CreateCacheEntry") => {
            reserve_v2(req, ctx, primary)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if v2_twirp_path(p, "FinalizeCacheEntryUpload") => {
            finalize_v2(req, ctx, primary)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if v2_twirp_path(p, "GetCacheEntryDownloadURL") => {
            lookup_v2(req, ctx, &chain)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        _ => Ok(respond(
            StatusCode::NOT_FOUND,
            json!({"message": "not found"}),
        )),
    };
    let response = match result {
        Ok(response) => response,
        Err(error) if error.downcast_ref::<CacheLockBusy>().is_some() => {
            eprintln!("Warning: gha cache service is busy: {error:#}");
            respond(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"message": "cache is busy; retry"}),
            )
        }
        Err(error) if error.downcast_ref::<CacheNamespaceLimit>().is_some() => respond(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"message": "cache namespace capacity is full; retry later"}),
        ),
        Err(error) if error.downcast_ref::<CacheEntryTooLarge>().is_some() => respond(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"message": "cache entry exceeds the namespace byte budget"}),
        ),
        Err(error) if error.downcast_ref::<CacheBadRequest>().is_some() => respond(
            StatusCode::BAD_REQUEST,
            json!({"message": "invalid cache request"}),
        ),
        Err(error) if error.downcast_ref::<CacheRangeNotSatisfiable>().is_some() => {
            let total_size = error
                .downcast_ref::<CacheRangeNotSatisfiable>()
                .map_or(0, |range| range.total_size);
            // Proof: `body()` fails only on an invalid status/header; the
            // status is a const and the header values are static or u64
            // digits, which are always valid.
            #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
            Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{total_size}"))
                .header("content-type", "application/json")
                .body(full_body(
                    json!({"message": "requested cache byte range is not satisfiable"}).to_string(),
                ))
                .unwrap()
        }
        Err(error) if error.downcast_ref::<CacheBlobNotFound>().is_some() => {
            azure_blob_not_found_response()
        }
        Err(error)
            if error.downcast_ref::<CacheKeyConflict>().is_some()
                || error.downcast_ref::<CacheProtocolConflict>().is_some() =>
        {
            respond(
                StatusCode::CONFLICT,
                json!({
                    "message": "Cache already exists.",
                    "typeName": "ArtifactCacheItemAlreadyExistsException",
                    "typeKey": "ArtifactCacheItemAlreadyExistsException",
                    "errorCode": 409,
                    "code": "already_exists",
                }),
            )
        }
        Err(error) if error.downcast_ref::<CacheEntryNotRetained>().is_some() => respond(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"message": "cache entry could not be retained; retry"}),
        ),
        Err(error) => {
            eprintln!("Warning: gha cache service: {error:#}");
            internal_error()
        }
    };
    Ok(hold_cache_activity(response, activity_guards))
}

fn cache_route_supported(method: &hyper::Method, path: &str) -> bool {
    match *method {
        hyper::Method::GET => {
            path.ends_with("/_apis/artifactcache/cache") || v2_download_id(path).is_some()
        }
        hyper::Method::HEAD => v2_download_id(path).is_some(),
        hyper::Method::POST => {
            path.ends_with("/_apis/artifactcache/caches")
                || v1_cache_resource_id(path).is_some()
                || v2_twirp_path(path, "CreateCacheEntry")
                || v2_twirp_path(path, "FinalizeCacheEntryUpload")
                || v2_twirp_path(path, "GetCacheEntryDownloadURL")
        }
        hyper::Method::PATCH => v1_cache_resource_id(path).is_some(),
        hyper::Method::PUT => v2_upload_id(path).is_some(),
        _ => false,
    }
}

fn hold_cache_activity(
    response: Response<ResponseBody>,
    activity: Vec<CacheActivityLock>,
) -> Response<ResponseBody> {
    let (parts, body) = response.into_parts();
    let body = body
        .map_frame(move |frame| {
            let _activity = &activity;
            frame
        })
        .boxed_unsync();
    Response::from_parts(parts, body)
}

fn empty_response(status: StatusCode) -> Response<ResponseBody> {
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a valid `StatusCode` and there are no headers.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    Response::builder()
        .status(status)
        .body(full_body(Bytes::new()))
        .unwrap()
}

fn azure_response(status: StatusCode) -> Response<ResponseBody> {
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a valid `StatusCode` and the header values are static strings or a
    // UUID, which are always valid.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    Response::builder()
        .status(status)
        .header("x-ms-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-ms-version", "2021-12-02")
        .header("etag", "\"velnor-cache\"")
        .header("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT")
        .body(full_body(Bytes::new()))
        .unwrap()
}

fn azure_head_response(size: u64) -> Response<ResponseBody> {
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a const and the header values are static strings, a UUID, or u64
    // digits, which are always valid.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    Response::builder()
        .status(StatusCode::OK)
        .header("x-ms-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-ms-version", "2021-12-02")
        .header("etag", "\"velnor-cache\"")
        .header("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT")
        .header("content-length", size)
        .header("content-type", "application/octet-stream")
        .header("x-ms-blob-type", "BlockBlob")
        .body(full_body(Bytes::new()))
        .unwrap()
}

fn azure_blob_not_found_response() -> Response<ResponseBody> {
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a const and the header values are static strings or a UUID, which are
    // always valid.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("x-ms-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-ms-version", "2021-12-02")
        .header("x-ms-error-code", "BlobNotFound")
        .header("content-type", "application/xml")
        .body(full_body(
            "<Error><Code>BlobNotFound</Code><Message>The specified blob does not exist.</Message></Error>",
        ))
        .unwrap()
}

fn v2_upload_id(path: &str) -> Option<&str> {
    if !path.starts_with('/') {
        return None;
    }
    let mut segments = path.rsplit('/');
    let id = segments.next()?;
    if segments.next() != Some("upload") || segments.next() != Some("_results") {
        return None;
    }
    validate_cache_id(id).ok()?;
    Some(id)
}

fn verify_v2_upload_capability(
    service: &CacheService,
    id: &str,
    namespace: &str,
    claim: &str,
    method: &hyper::Method,
) -> Option<String> {
    if *method != hyper::Method::PUT
        || validate_cache_id(id).is_err()
        || !valid_capability_namespace(namespace)
        || claim.len() != 32
        || !claim.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let reservation = read_v2_reservation(service, id, namespace).ok()??;
    if reservation.upload_nonce != claim
        || v2_reservation_is_expired(&reservation, now_unix_millis().ok()?)
        || service.entry_path(id, Some(namespace)).exists()
    {
        return None;
    }
    Some(namespace.to_owned())
}

fn v2_twirp_path(path: &str, rpc: &str) -> bool {
    path.ends_with(&format!(
        "/twirp/github.actions.results.api.v1.CacheService/{rpc}"
    ))
}

fn v2_download_id(path: &str) -> Option<&str> {
    let mut segments = path.rsplit('/');
    let id = segments.next()?;
    if segments.next() != Some("download") || segments.next() != Some("_results") {
        return None;
    }
    validate_cache_id(id).ok()?;
    Some(id)
}

fn v1_cache_resource_id(path: &str) -> Option<&str> {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 4
        || segments[segments.len() - 4] != "_apis"
        || segments[segments.len() - 3] != "artifactcache"
        || segments[segments.len() - 2] != "caches"
    {
        return None;
    }
    let id = *segments.last()?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(id)
}

fn respond_unauthorized() -> Response<ResponseBody> {
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a const and the header name/value are static valid strings.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("content-type", "application/json")
        .body(full_body(json!({"message": "bad token"}).to_string()))
        .unwrap()
}

fn full_body(body: impl Into<Bytes>) -> ResponseBody {
    Full::new(body.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

async fn body_json<B>(req: Request<B>) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    collect_json(req.into_body(), MAX_JSON_BODY)
        .await
        .map_err(|error| anyhow::Error::new(CacheBadRequest).context(error))
}

async fn collect_json<B>(body: B, max_bytes: usize) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let bytes = Limited::new(body, max_bytes)
        .collect()
        .await
        .map_err(anyhow::Error::from_boxed)?
        .to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

/// Lookup keys in request order: primary key first, then restore keys.
///
/// `actions/toolkit` sends them as one comma-joined, percent-encoded parameter
/// (`cache?keys=${encodeURIComponent(keys.join(','))}&version=…`), so the value
/// must be decoded before it is split — the separators arrive as `%2C`.
fn keys_from_query<B>(req: &Request<B>) -> Vec<String> {
    query_pairs(req)
        .into_iter()
        .filter(|(name, _)| name == "keys" || name == "restoreKeys")
        .flat_map(|(_, value)| {
            value
                .split(',')
                .map(ToOwned::to_owned)
                .collect::<Vec<String>>()
        })
        .filter(|key| !key.is_empty())
        .collect()
}

async fn reserve<B>(req: Request<B>, ctx: &Ctx, namespace: &str) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let size = match body.get("cacheSize") {
        None | Some(Value::Null) => None,
        Some(Value::Number(value)) => value.as_u64(),
        Some(_) => None,
    };
    if body.get("cacheSize").is_some_and(|value| !value.is_null()) && size.is_none() {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    if size.is_some_and(|size| size > MAX_BODY) {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    if size.is_some_and(|size| size > ctx.service.budget_bytes) {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let hash = entry_hash(key, version);
    ctx.service.ensure_tenant(namespace)?;
    let _lock = lock_cache_entry(&ctx.service, &hash, namespace).await?;
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        return Err(anyhow::Error::new(CacheKeyConflict));
    }
    if let Some((record, _)) = read_reservation_record(&ctx.service, &hash, Some(namespace))? {
        if record.get("protocol").is_some() {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        let _reservation = parse_v1_reservation(record, &hash)?;
        // The V1 client treats an existing reservation as a cache conflict.
        // Reissuing its numeric ID would let a second save race the first.
        return Err(anyhow::Error::new(CacheKeyConflict));
    }
    let index_directory = ctx
        .service
        .tenant_root(Some(namespace))
        .join("reservations/by-id");
    let new_files = 2 + usize::from(!index_directory.is_dir());
    let metadata_bytes = u64::try_from(key.len().saturating_add(version.len()).saturating_add(512))
        .context("v1 reservation metadata size overflow")?;
    ctx.service.ensure_namespace_capacity_locked(
        Some(namespace),
        new_files,
        CACHE_ADMISSION_TRANSITION_HEADROOM,
        metadata_bytes,
        _lock.namespace_guard(),
    )?;
    let cache_id = allocate_v1_cache_id(&ctx.service, namespace, &hash)?;
    let reservation = V1Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        cache_id,
        expected_size: size,
        chunks: Vec::new(),
        commit_attempt: None,
        active_uploads: Vec::new(),
    };
    persist_v1_reservation(&ctx.service, &hash, &reservation, namespace)?;
    Ok(json!({"cacheId": hash, "cacheID": cache_id}))
}

async fn reserve_v2<B>(req: Request<B>, ctx: &Ctx, namespace: &str) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let hash = entry_hash(key, version);
    ctx.service.ensure_tenant(namespace)?;
    let _lock = lock_cache_entry(&ctx.service, &hash, namespace).await?;
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        if let Some((record, modified_ms)) =
            read_reservation_record(&ctx.service, &hash, Some(namespace))?
        {
            match record.get("protocol").and_then(Value::as_str) {
                Some("v2") => {
                    let reservation = parse_v2_reservation(record, &hash, modified_ms)?;
                    clear_v2_reservation(
                        &ctx.service,
                        &hash,
                        namespace,
                        &reservation.upload_nonce,
                    )?;
                }
                None => {
                    parse_v1_reservation(record, &hash)?;
                    clear_v1_reservation(&ctx.service, &hash, namespace)?;
                }
                Some(_) => {}
            }
        }
        return Ok(json!({"ok": false}));
    }

    if let Some((record, _)) = read_reservation_record(&ctx.service, &hash, Some(namespace))?
        && record.get("protocol").and_then(Value::as_str) != Some("v2")
    {
        return Ok(json!({"ok": false}));
    }
    if let Some(reservation) = read_v2_reservation(&ctx.service, &hash, namespace)? {
        if !v2_reservation_is_expired(&reservation, now_unix_millis()?) {
            return Ok(json!({"ok": false}));
        }
        remove_unpublished_cache_blob(&ctx.service, &hash, namespace)?;
        clear_v2_reservation(&ctx.service, &hash, namespace, &reservation.upload_nonce)?;
    }

    let reservation = V2Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        upload_nonce: uuid::Uuid::new_v4().simple().to_string(),
        block_id_bytes: None,
        upload_attempt: None,
        upload_blocks: Vec::new(),
        active_uploads: Vec::new(),
        assembly_scratch_attempt: None,
        assembly_scratch_bytes: 0,
        updated_ms: now_unix_millis()?,
    };
    let reservation_record = reservation.as_json();
    let reservation_bytes = reservation_record.to_string().len() as u64;
    ctx.service.ensure_namespace_capacity_locked(
        Some(namespace),
        1,
        CACHE_ADMISSION_TRANSITION_HEADROOM,
        reservation_bytes,
        _lock.namespace_guard(),
    )?;
    if !atomically_create_json(
        &ctx.service.reservation_path(&hash, Some(namespace)),
        &reservation_record,
    )? {
        return Ok(json!({"ok": false}));
    }
    // A v1 publisher may have won after the first entry check but before this
    // immutable claim was linked. Do not issue an upload URL when it did.
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        clear_v2_reservation(&ctx.service, &hash, namespace, &reservation.upload_nonce)?;
        return Ok(json!({"ok": false}));
    }
    let signed_upload_url = format!(
        "{}/_results/upload/{hash}?sig={}&scope={namespace}",
        ctx.public_base, reservation.upload_nonce
    );
    Ok(json!({
        "ok": true,
        "signed_upload_url": signed_upload_url,
        "signedUploadUrl": signed_upload_url,
    }))
}

#[derive(Debug, Clone, Copy)]
struct UploadLimits {
    declared_size: Option<u64>,
    expected_size: Option<u64>,
    max_bytes: u64,
    body_idle_timeout: Option<std::time::Duration>,
}

fn validate_existing_v1_entry(
    service: &CacheService,
    id: &str,
    reservation: &V1Reservation,
    namespace: &str,
) -> Result<()> {
    let entry_path = service.entry_path(id, Some(namespace));
    let entry_metadata = std::fs::symlink_metadata(&entry_path)
        .with_context(|| format!("stat existing v1 cache entry {id}"))?;
    if !entry_metadata.file_type().is_file() {
        anyhow::bail!("existing v1 cache entry is not a regular file");
    }
    let raw =
        std::fs::read(&entry_path).with_context(|| format!("read existing v1 cache entry {id}"))?;
    let entry: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("parse existing v1 cache entry {id}"))?;
    if entry["key"].as_str() != Some(reservation.key.as_str())
        || entry["version"].as_str() != Some(reservation.version.as_str())
        || entry["blob"].as_str() != Some(id)
        || reservation
            .expected_size
            .is_some_and(|size| entry["size"].as_u64() != Some(size))
    {
        anyhow::bail!("existing v1 cache entry does not match reservation");
    }

    let blob_path = service.tenant_root(Some(namespace)).join("blobs").join(id);
    let blob_metadata = std::fs::symlink_metadata(&blob_path)
        .with_context(|| format!("stat existing v1 cache blob {id}"))?;
    if !blob_metadata.file_type().is_file() {
        anyhow::bail!("existing v1 cache blob is not a regular file");
    }
    if reservation
        .expected_size
        .is_some_and(|size| blob_metadata.len() != size)
    {
        anyhow::bail!(
            "existing v1 cache blob size mismatch: reservation records {:?}, file has {}",
            reservation.expected_size,
            blob_metadata.len()
        );
    }
    let expected_digest = entry["contentSha256"]
        .as_str()
        .context("existing cache entry has no content digest")?;
    if sha256_file(&blob_path)? != expected_digest {
        anyhow::bail!("existing cache blob does not match its committed digest");
    }
    Ok(())
}

#[cfg(test)]
async fn upload<B>(
    req: Request<B>,
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    v1: bool,
    upload_nonce: Option<&str>,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let declared_size = request_content_length(&req)?;
    let body = req.into_body();
    if v1 {
        upload_v1_body(body, ctx, id, namespace, declared_size).await
    } else {
        upload_v2_body(
            body,
            ctx,
            id,
            namespace,
            declared_size,
            upload_nonce.context("v2 upload URL has no claim nonce")?,
        )
        .await
    }
}

fn parse_content_range<B>(req: &Request<B>) -> Result<(u64, u64, Option<u64>)> {
    let value = req
        .headers()
        .get(CONTENT_RANGE)
        .ok_or_else(|| anyhow::Error::new(CacheBadRequest))?
        .to_str()
        .map_err(|_| anyhow::Error::new(CacheBadRequest))?;
    let Some(value) = value.strip_prefix("bytes ") else {
        return Err(anyhow::Error::new(CacheBadRequest));
    };
    let Some((range, total)) = value.split_once('/') else {
        return Err(anyhow::Error::new(CacheBadRequest));
    };
    let Some((start, end)) = range.split_once('-') else {
        return Err(anyhow::Error::new(CacheBadRequest));
    };
    let start: u64 = start
        .parse()
        .map_err(|_| anyhow::Error::new(CacheBadRequest))?;
    let end: u64 = end
        .parse()
        .map_err(|_| anyhow::Error::new(CacheBadRequest))?;
    if start > end || end >= MAX_BODY {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let total = if total == "*" {
        None
    } else {
        let total: u64 = total
            .parse()
            .map_err(|_| anyhow::Error::new(CacheBadRequest))?;
        if total == 0 || end >= total {
            return Err(anyhow::Error::new(CacheBadRequest));
        }
        Some(total)
    };
    Ok((start, end, total))
}

fn v1_upload_part_path(
    service: &CacheService,
    namespace: &str,
    id: &str,
    attempt: &str,
) -> PathBuf {
    service
        .tenant_root(Some(namespace))
        .join("uploads")
        .join(id)
        .join(format!("{attempt}.part"))
}

fn v1_chunk_file_path(service: &CacheService, namespace: &str, id: &str, attempt: &str) -> PathBuf {
    v1_upload_part_path(service, namespace, id, attempt)
}

fn sync_multipart_path(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("stat multipart upload path {} for sync", path.display()))?;
    if metadata.file_type().is_dir() {
        sync_directory(path)
    } else if metadata.file_type().is_file() {
        std::fs::File::open(path)
            .with_context(|| format!("open multipart upload file {} for sync", path.display()))?
            .sync_all()
            .with_context(|| format!("sync multipart upload file {}", path.display()))?;
        Ok(())
    } else {
        anyhow::bail!(
            "multipart upload path {} is not a regular file or directory",
            path.display()
        )
    }
}

#[cfg(unix)]
fn open_cache_directory(path: &Path, label: &str) -> Result<std::fs::File> {
    let directory = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)
    .with_context(|| format!("open {label} {} without following links", path.display()))?;
    let directory: std::fs::File = directory.into();
    if !directory
        .metadata()
        .with_context(|| format!("stat opened {label} {}", path.display()))?
        .file_type()
        .is_dir()
    {
        anyhow::bail!("{label} {} is not a real directory", path.display());
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_cache_directory(path: &Path, label: &str) -> Result<std::fs::File> {
    let _ = (path, label);
    anyhow::bail!("descriptor-relative cache directory access is unavailable")
}

#[cfg(unix)]
fn open_cache_directory_child(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    label: &str,
) -> Result<Option<std::fs::File>> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        anyhow::bail!("{label} name is not one path component");
    }
    match rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => Ok(Some(directory.into())),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io::Error::from(error))
            .with_context(|| format!("open {label} {} without following links", path.display())),
    }
}

#[cfg(not(unix))]
fn open_cache_directory_child(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    label: &str,
) -> Result<Option<std::fs::File>> {
    let _ = (parent, name, path, label);
    anyhow::bail!("descriptor-relative cache directory access is unavailable")
}

#[cfg(unix)]
fn open_cache_file_child(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    label: &str,
) -> Result<Option<std::fs::File>> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        anyhow::bail!("{label} name is not one path component");
    }
    match rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOCTTY
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(file) => Ok(Some(file.into())),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io::Error::from(error))
            .with_context(|| format!("open {label} {} without following links", path.display())),
    }
}

#[cfg(not(unix))]
fn open_cache_file_child(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    label: &str,
) -> Result<Option<std::fs::File>> {
    let _ = (parent, name, path, label);
    anyhow::bail!("descriptor-relative cache file access is unavailable")
}

#[cfg(unix)]
fn cache_directory_entry_names(
    directory: &std::fs::File,
    label: &str,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;

    let scan_directory = rustix::fs::openat(
        directory,
        ".",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)
    .with_context(|| format!("pin {label} for scan"))?;
    let scan_directory: std::fs::File = scan_directory.into();
    verify_cache_descendant_mount(root_identity, &scan_directory, label)?;
    let entries = rustix::fs::Dir::read_from(&scan_directory)
        .map_err(io::Error::from)
        .with_context(|| format!("read {label}"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(io::Error::from)
            .with_context(|| format!("read {label} entry"))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(not(unix))]
fn cache_directory_entry_names(
    directory: &std::fs::File,
    label: &str,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<Vec<std::ffi::OsString>> {
    let _ = (directory, label, root_identity);
    anyhow::bail!("descriptor-relative cache directory scans are unavailable")
}

#[cfg(unix)]
fn try_lock_cache_file_exclusive(file: &std::fs::File, label: &str) -> Result<bool> {
    match rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(error) => Err(anyhow::Error::new(error).context(format!("try-lock {label}"))),
    }
}

#[cfg(not(unix))]
fn try_lock_cache_file_exclusive(_file: &std::fs::File, _label: &str) -> Result<bool> {
    anyhow::bail!("descriptor-relative cache file locks are unavailable")
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheGcQuarantineIdentity {
    version: u8,
    device: u64,
    inode: u64,
    mount: String,
    file_type: String,
}

#[cfg(unix)]
impl CacheGcQuarantineIdentity {
    fn from_identity(identity: &crate::leftover_disk::FilesystemEntryIdentity) -> Self {
        Self {
            version: 1,
            device: identity.device,
            inode: identity.inode,
            mount: format!("{:?}", identity.mount),
            file_type: "regular-file".to_owned(),
        }
    }

    fn matches(&self, identity: &crate::leftover_disk::FilesystemEntryIdentity) -> bool {
        self == &Self::from_identity(identity)
    }
}

#[cfg(unix)]
fn write_cache_gc_quarantine_identity(
    quarantine: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    identity: &crate::leftover_disk::FilesystemEntryIdentity,
    label: &str,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let mut journal: std::fs::File = rustix::fs::openat(
        quarantine,
        std::ffi::OsStr::new(CACHE_GC_QUARANTINE_IDENTITY),
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(io::Error::from)
    .context("create cache-GC quarantine identity journal")?
    .into();
    verify_cache_descendant_mount(root_identity, &journal, "cache-GC identity journal")?;
    let metadata = journal
        .metadata()
        .context("inspect cache-GC identity journal")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        anyhow::bail!("cache-GC identity journal is not a private regular file");
    }
    serde_json::to_writer(
        &mut journal,
        &CacheGcQuarantineIdentity::from_identity(identity),
    )
    .context("write cache-GC quarantine identity journal")?;
    journal
        .sync_all()
        .with_context(|| format!("sync {label} quarantine identity journal"))?;
    quarantine
        .sync_all()
        .with_context(|| format!("persist {label} quarantine identity journal"))
}

#[cfg(unix)]
fn read_cache_gc_quarantine_identity(
    quarantine: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<Option<CacheGcQuarantineIdentity>> {
    use std::os::unix::fs::MetadataExt as _;

    let Some(mut journal) = open_cache_file_child(
        quarantine,
        std::ffi::OsStr::new(CACHE_GC_QUARANTINE_IDENTITY),
        Path::new(CACHE_GC_QUARANTINE_IDENTITY),
        "cache-GC identity journal",
    )?
    else {
        return Ok(None);
    };
    verify_cache_descendant_mount(root_identity, &journal, "cache-GC identity journal")?;
    let metadata = journal
        .metadata()
        .context("inspect cache-GC identity journal")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        anyhow::bail!("cache-GC identity journal is not a private regular file");
    }
    let mut raw = Vec::new();
    std::io::Read::take(&mut journal, 1025)
        .read_to_end(&mut raw)
        .with_context(|| format!("read {label} quarantine identity journal"))?;
    if raw.len() > 1024 {
        anyhow::bail!("cache-GC identity journal exceeds its size limit");
    }
    let identity: CacheGcQuarantineIdentity =
        serde_json::from_slice(&raw).context("parse cache-GC quarantine identity journal")?;
    if identity.version != 1 || identity.file_type != "regular-file" {
        anyhow::bail!("cache-GC identity journal has an unsupported file identity");
    }
    Ok(Some(identity))
}

#[cfg(unix)]
fn mark_cache_gc_quarantine_preserved(
    quarantine: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let marker = std::ffi::OsStr::new(CACHE_GC_QUARANTINE_PRESERVED);
    match rustix::fs::openat(
        quarantine,
        marker,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    ) {
        Ok(file) => {
            let mut file: std::fs::File = file.into();
            verify_cache_descendant_mount(root_identity, &file, "cache-GC preserve marker")?;
            let metadata = file
                .metadata()
                .context("inspect cache-GC preserve marker")?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                anyhow::bail!("cache-GC preserve marker is not a private regular file");
            }
            file.write_all(b"preserve-v1\n")
                .context("write cache-GC preserve marker")?;
            file.sync_all().context("sync cache-GC preserve marker")?;
            quarantine
                .sync_all()
                .with_context(|| format!("persist preserved {label} quarantine"))
        }
        Err(rustix::io::Errno::EXIST) => {
            let Some(file) = open_cache_file_child(
                quarantine,
                marker,
                Path::new(CACHE_GC_QUARANTINE_PRESERVED),
                "cache-GC preserve marker",
            )?
            else {
                anyhow::bail!("cache-GC preserve marker disappeared");
            };
            verify_cache_descendant_mount(root_identity, &file, "cache-GC preserve marker")?;
            let metadata = file
                .metadata()
                .context("inspect existing cache-GC preserve marker")?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                anyhow::bail!("cache-GC preserve marker is not a private regular file");
            }
            Ok(())
        }
        Err(error) => Err(io::Error::from(error)).context("mark cache-GC quarantine preserved"),
    }
}

#[cfg(unix)]
fn unlink_cache_file_if_same(
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    opened_file: &std::fs::File,
    expected_file: &crate::leftover_disk::FilesystemEntryIdentity,
    label: &str,
) -> Result<bool> {
    unlink_cache_file_if_same_with_hook(
        root_identity,
        parent,
        name,
        opened_file,
        expected_file,
        label,
        |_, _| Ok(()),
    )
}

#[cfg(unix)]
fn unlink_cache_file_if_same_with_hook(
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    opened_file: &std::fs::File,
    expected_file: &crate::leftover_disk::FilesystemEntryIdentity,
    label: &str,
    after_move: impl FnOnce(&std::fs::File, &std::ffi::OsStr) -> Result<()>,
) -> Result<bool> {
    verify_cache_descendant_mount(root_identity, parent, "cache-GC parent")?;
    let opened_identity = verify_cache_descendant_mount(root_identity, opened_file, label)?;
    if &opened_identity != expected_file {
        anyhow::bail!("pinned {label} changed identity before quarantine");
    }
    if !opened_file
        .metadata()
        .with_context(|| format!("inspect pinned {label}"))?
        .is_file()
    {
        anyhow::bail!("pinned {label} is not a regular file");
    }

    let (quarantine, quarantine_name, quarantine_identity) =
        create_cache_gc_quarantine(parent, root_identity, label)?;
    write_cache_gc_quarantine_identity(&quarantine, root_identity, &opened_identity, label)?;
    let recorded_identity = read_cache_gc_quarantine_identity(&quarantine, root_identity, label)?
        .context("cache-GC quarantine identity journal disappeared")?;
    if !recorded_identity.matches(&opened_identity) {
        anyhow::bail!("cache-GC quarantine identity journal did not persist the pinned {label}");
    }
    let quarantine_entry = std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY);
    match rustix::fs::renameat_with(
        parent,
        name,
        &quarantine,
        quarantine_entry,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity)?;
            remove_cache_gc_quarantine(
                parent,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
            )?;
            return Ok(false);
        }
        Err(error) => {
            let cleanup = cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity)
                .and_then(|()| {
                    remove_cache_gc_quarantine(
                        parent,
                        &quarantine,
                        &quarantine_name,
                        &quarantine_identity,
                    )
                });
            return Err(error).context(format!("quarantine {label} (cleanup: {cleanup:?})"));
        }
    }

    if let Err(error) = quarantine
        .sync_all()
        .and_then(|()| parent.sync_all())
        .context("persist cache-GC quarantine move")
    {
        if let Err(preserve_error) =
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)
        {
            return Err(error).context(format!(
                "preserve {label} after move sync failure (marker: {preserve_error:#})"
            ));
        }
        let restore = restore_cache_gc_quarantine_entry(&quarantine, parent, name);
        let cleanup = if restore.is_ok() {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity).and_then(|()| {
                remove_cache_gc_quarantine(
                    parent,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                )
            })
        } else {
            Err(anyhow::anyhow!(
                "preserved cache-GC quarantine after restore failure"
            ))
        };
        return Err(error).context(format!(
            "persist {label} quarantine (restore: {restore:?}, cleanup: {cleanup:?})"
        ));
    }

    if let Err(error) = after_move(&quarantine, quarantine_entry) {
        if let Err(preserve_error) =
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)
        {
            return Err(error).context(format!(
                "preserve {label} after quarantine hook failure (marker: {preserve_error:#})"
            ));
        }
        let restore = restore_cache_gc_quarantine_entry(&quarantine, parent, name);
        let cleanup = if restore.is_ok() {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity).and_then(|()| {
                remove_cache_gc_quarantine(
                    parent,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                )
            })
        } else {
            Err(anyhow::anyhow!(
                "preserved cache-GC quarantine after restore failure"
            ))
        };
        return Err(error).context(format!(
            "restore {label} after quarantine hook (restore: {restore:?}, cleanup: {cleanup:?})"
        ));
    }
    let quarantined = open_cache_file_child(
        &quarantine,
        quarantine_entry,
        &PathBuf::from(CACHE_GC_QUARANTINE_ENTRY),
        label,
    )?
    .context("cache-GC quarantine entry disappeared after move")?;
    let quarantined_identity = verify_cache_descendant_mount(root_identity, &quarantined, label)?;
    if quarantined_identity != opened_identity
        || !quarantined
            .metadata()
            .with_context(|| format!("inspect quarantined {label}"))?
            .is_file()
    {
        mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
        let restore = restore_cache_gc_quarantine_entry(&quarantine, parent, name);
        let cleanup = if restore.is_ok() {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity).and_then(|()| {
                remove_cache_gc_quarantine(
                    parent,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                )
            })
        } else {
            Err(anyhow::anyhow!(
                "preserved cache-GC quarantine after restore failure"
            ))
        };
        return Err(anyhow::anyhow!(
            "{label} changed identity during quarantine move"
        ))
        .context(format!(
            "preserve mismatched {label} (restore: {restore:?}, cleanup: {cleanup:?})"
        ));
    }

    let names = cache_directory_entry_names(&quarantine, "cache-GC quarantine", root_identity)?;
    if names.len() != 2
        || !names.iter().any(|entry| entry == quarantine_entry)
        || !names
            .iter()
            .any(|entry| entry == CACHE_GC_QUARANTINE_IDENTITY)
    {
        mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
        let restore = restore_cache_gc_quarantine_entry(&quarantine, parent, name);
        let cleanup = if restore.is_ok() {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity).and_then(|()| {
                remove_cache_gc_quarantine(
                    parent,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                )
            })
        } else {
            Err(anyhow::anyhow!(
                "preserved cache-GC quarantine after restore failure"
            ))
        };
        return Err(anyhow::anyhow!(
            "cache-GC quarantine changed during {label} removal"
        ))
        .context(format!(
            "preserve unexpected quarantine contents (restore: {restore:?}, cleanup: {cleanup:?})"
        ));
    }
    let named = rustix::fs::statat(
        &quarantine,
        quarantine_entry,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)
    .with_context(|| format!("recheck quarantined {label}"))?;
    // st_dev is u32 on macOS, u64 on Linux; the conversion is load-bearing on macOS.
    #[allow(clippy::useless_conversion)]
    let named_device =
        u64::try_from(named.st_dev).context("convert quarantined cache-GC entry device ID")?;
    if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
        || named_device != quarantined_identity.device
        || named.st_ino != quarantined_identity.inode
    {
        mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
        let restore = restore_cache_gc_quarantine_entry(&quarantine, parent, name);
        let cleanup = if restore.is_ok() {
            cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity).and_then(|()| {
                remove_cache_gc_quarantine(
                    parent,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                )
            })
        } else {
            Err(anyhow::anyhow!(
                "preserved cache-GC quarantine after restore failure"
            ))
        };
        return Err(anyhow::anyhow!("quarantined {label} changed before unlink")).context(format!(
            "preserve mismatched {label} (restore: {restore:?}, cleanup: {cleanup:?})"
        ));
    }
    rustix::fs::unlinkat(&quarantine, quarantine_entry, rustix::fs::AtFlags::empty())
        .map_err(io::Error::from)
        .with_context(|| format!("remove quarantined {label}"))?;
    quarantine
        .sync_all()
        .with_context(|| format!("sync quarantine after removing {label}"))?;
    cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity)?;
    remove_cache_gc_quarantine(parent, &quarantine, &quarantine_name, &quarantine_identity)?;
    Ok(true)
}

#[cfg(unix)]
fn create_cache_gc_quarantine(
    parent: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<(
    std::fs::File,
    std::ffi::OsString,
    crate::leftover_disk::FilesystemDirectoryIdentity,
)> {
    use std::os::unix::fs::MetadataExt as _;

    for _ in 0..16 {
        let name = std::ffi::OsString::from(format!(
            "{CACHE_GC_QUARANTINE_PREFIX}{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        match rustix::fs::mkdirat(parent, &name, rustix::fs::Mode::from_raw_mode(0o700)) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => {
                return Err(io::Error::from(error))
                    .with_context(|| format!("create private cache-GC quarantine for {label}"));
            }
        }
        let path = PathBuf::from(&name);
        let Some(quarantine) = open_cache_directory_child(parent, &name, &path, label)? else {
            anyhow::bail!("new cache-GC quarantine disappeared");
        };
        let identity = verify_cache_descendant_mount(root_identity, &quarantine, label)?;
        let metadata = quarantine
            .metadata()
            .context("inspect private cache-GC quarantine")?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("cache-GC quarantine is not owned by the daemon account");
        }
        rustix::fs::fchmod(&quarantine, rustix::fs::Mode::from_raw_mode(0o700))
            .map_err(io::Error::from)
            .context("make cache-GC quarantine private")?;
        let metadata = quarantine
            .metadata()
            .context("inspect private cache-GC quarantine after chmod")?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            anyhow::bail!("cache-GC quarantine is not private to the daemon account");
        }
        parent
            .sync_all()
            .context("persist created cache-GC quarantine")?;
        return Ok((quarantine, name, identity));
    }
    anyhow::bail!("could not allocate a unique cache-GC quarantine for {label}")
}

#[cfg(unix)]
fn restore_cache_gc_quarantine_entry(
    quarantine: &std::fs::File,
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<()> {
    rustix::fs::renameat_with(
        quarantine,
        std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
        parent,
        name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
    .context("restore preserved cache-GC quarantine entry without replacement")?;
    quarantine
        .sync_all()
        .context("sync cache-GC quarantine after restore")?;
    parent
        .sync_all()
        .context("sync cache-GC parent after restore")
}

#[cfg(unix)]
fn remove_cache_gc_quarantine(
    parent: &std::fs::File,
    quarantine: &std::fs::File,
    name: &std::ffi::OsStr,
    expected_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<()> {
    if verify_cache_descendant_mount(expected_identity, quarantine, "cache-GC quarantine")?
        != *expected_identity
    {
        anyhow::bail!("cache-GC quarantine identity changed before cleanup");
    }
    if !cache_directory_entry_names(quarantine, "cache-GC quarantine", expected_identity)?
        .is_empty()
    {
        anyhow::bail!("refusing to remove a non-empty cache-GC quarantine");
    }
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(io::Error::from)
        .context("remove private cache-GC quarantine")?;
    parent
        .sync_all()
        .context("persist cache-GC quarantine removal")
}

#[cfg(unix)]
fn cleanup_cache_gc_quarantine_metadata(
    quarantine: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<()> {
    let entries = cache_directory_entry_names(quarantine, "cache-GC quarantine", root_identity)?;
    if entries
        .iter()
        .any(|name| name != CACHE_GC_QUARANTINE_IDENTITY && name != CACHE_GC_QUARANTINE_PRESERVED)
    {
        anyhow::bail!("refusing to clear cache-GC quarantine metadata with unexpected entries");
    }
    for metadata_name in [CACHE_GC_QUARANTINE_PRESERVED, CACHE_GC_QUARANTINE_IDENTITY] {
        if entries.iter().any(|entry| entry == metadata_name) {
            rustix::fs::unlinkat(
                quarantine,
                std::ffi::OsStr::new(metadata_name),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(io::Error::from)
            .with_context(|| format!("remove cache-GC quarantine metadata {metadata_name}"))?;
        }
    }
    quarantine
        .sync_all()
        .context("sync cache-GC quarantine metadata removal")
}

#[cfg(unix)]
fn recover_cache_gc_quarantines(
    parent: &std::fs::File,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    label: &str,
) -> Result<usize> {
    use std::os::unix::fs::MetadataExt as _;

    verify_cache_descendant_mount(root_identity, parent, label)?;
    let mut recovered = 0usize;
    for name in cache_directory_entry_names(parent, label, root_identity)? {
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with(CACHE_GC_QUARANTINE_PREFIX))
        {
            continue;
        }
        let path = PathBuf::from(&name);
        let quarantine = match open_cache_directory_child(parent, &name, &path, label) {
            Ok(Some(quarantine)) => quarantine,
            Ok(None) | Err(_) => continue,
        };
        let identity = match verify_cache_descendant_mount(root_identity, &quarantine, label) {
            Ok(identity) => identity,
            Err(_) => continue,
        };
        let metadata = match quarantine.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            continue;
        }

        let entries = match cache_directory_entry_names(&quarantine, label, root_identity) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        if entries.is_empty() {
            remove_cache_gc_quarantine(parent, &quarantine, &name, &identity)?;
            recovered += 1;
            continue;
        }

        let entry_name = std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY);
        let has_entry = entries
            .iter()
            .any(|entry| entry == CACHE_GC_QUARANTINE_ENTRY);
        if !has_entry {
            if entries.iter().all(|entry| {
                entry == CACHE_GC_QUARANTINE_IDENTITY || entry == CACHE_GC_QUARANTINE_PRESERVED
            }) {
                cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity)?;
                remove_cache_gc_quarantine(parent, &quarantine, &name, &identity)?;
                recovered += 1;
            }
            continue;
        }

        if entries
            .iter()
            .any(|entry| entry == CACHE_GC_QUARANTINE_PRESERVED)
        {
            continue;
        }
        if entries.iter().any(|entry| {
            entry != CACHE_GC_QUARANTINE_ENTRY && entry != CACHE_GC_QUARANTINE_IDENTITY
        }) {
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
            continue;
        }
        let expected_identity =
            match read_cache_gc_quarantine_identity(&quarantine, root_identity, label) {
                Ok(Some(identity)) => identity,
                Ok(None) | Err(_) => {
                    mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                    continue;
                }
            };
        let file =
            match open_cache_file_child(&quarantine, entry_name, &path.join(entry_name), label) {
                Ok(Some(file)) => file,
                Ok(None) | Err(_) => {
                    mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                    continue;
                }
            };
        if verify_cache_descendant_mount(root_identity, &file, label).is_err()
            || !file
                .metadata()
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        {
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
            continue;
        }
        match try_lock_cache_file_exclusive(&file, label) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => {
                mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                continue;
            }
        }
        let opened_identity = match verify_cache_descendant_mount(root_identity, &file, label) {
            Ok(identity) => identity,
            Err(_) => {
                mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                continue;
            }
        };
        if !expected_identity.matches(&opened_identity)
            || !file
                .metadata()
                .with_context(|| format!("inspect interrupted {label}"))?
                .is_file()
        {
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
            continue;
        }
        let opened = match rustix::fs::fstat(&file).map_err(io::Error::from) {
            Ok(opened) => opened,
            Err(_) => {
                mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                continue;
            }
        };
        let named = match rustix::fs::statat(
            &quarantine,
            entry_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        {
            Ok(named) => named,
            Err(_) => {
                mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
                continue;
            }
        };
        if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
            || opened.st_dev != named.st_dev
            || opened.st_ino != named.st_ino
        {
            mark_cache_gc_quarantine_preserved(&quarantine, root_identity, label)?;
            continue;
        }
        rustix::fs::unlinkat(&quarantine, entry_name, rustix::fs::AtFlags::empty())
            .map_err(io::Error::from)
            .with_context(|| format!("finish interrupted {label} removal"))?;
        quarantine
            .sync_all()
            .with_context(|| format!("sync recovered {label} quarantine"))?;
        cleanup_cache_gc_quarantine_metadata(&quarantine, root_identity)?;
        remove_cache_gc_quarantine(parent, &quarantine, &name, &identity)?;
        recovered += 1;
    }
    Ok(recovered)
}

#[cfg(not(unix))]
fn unlink_cache_file_if_same(
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    opened_file: &std::fs::File,
    expected_file: &crate::leftover_disk::FilesystemEntryIdentity,
    label: &str,
) -> Result<bool> {
    let _ = (
        root_identity,
        parent,
        name,
        opened_file,
        expected_file,
        label,
    );
    anyhow::bail!("descriptor-relative cache file removal is unavailable")
}

#[cfg(unix)]
fn reap_stale_upload_admissions_at_pinned(
    lease_directory: &std::fs::File,
    tenant_directory: &std::fs::File,
    tenant_root_path: &Path,
    lease_directory_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<usize> {
    verify_cache_descendant_mount(root_identity, lease_directory, "cache admission leases")?;
    verify_cache_descendant_mount(root_identity, tenant_directory, "GHA tenant")?;
    let blobs_path = tenant_root_path.join("blobs");
    let blobs = open_cache_directory_child(
        tenant_directory,
        std::ffi::OsStr::new("blobs"),
        &blobs_path,
        "GHA tenant blobs directory",
    )?;
    if let Some(blobs) = blobs.as_ref() {
        verify_cache_descendant_mount(root_identity, blobs, "GHA tenant blobs")?;
    }
    recover_cache_gc_quarantines(lease_directory, root_identity, "cache admission leases")?;
    if let Some(blobs) = blobs.as_ref() {
        recover_cache_gc_quarantines(blobs, root_identity, "GHA tenant blobs")?;
    }
    let mut removed_leases = 0usize;
    let mut removed_temps = 0usize;
    for lease_name in
        cache_directory_entry_names(lease_directory, "cache admission leases", root_identity)?
    {
        if Path::new(&lease_name)
            .extension()
            .and_then(|value| value.to_str())
            != Some("json")
        {
            continue;
        }
        let lease_path = lease_directory_path.join(lease_name.clone());
        let Some(lease) = open_cache_file_child(
            lease_directory,
            &lease_name,
            &lease_path,
            "cache admission lease",
        )?
        else {
            continue;
        };
        let lease_identity =
            verify_cache_descendant_mount(root_identity, &lease, "cache admission lease")?;
        if !lease
            .metadata()
            .context("inspect pinned cache admission lease")?
            .is_file()
        {
            continue;
        }
        if !try_lock_cache_file_exclusive(&lease, "cache admission lease")? {
            continue;
        }
        let record = lease.try_clone().ok().and_then(|mut reader| {
            let mut raw = Vec::new();
            reader.read_to_end(&mut raw).ok()?;
            serde_json::from_slice::<Value>(&raw).ok()
        });
        if let Some((id, attempt)) = record.as_ref().and_then(|record| {
            let id = record["id"].as_str()?;
            let attempt = record["attempt"].as_str()?;
            (validate_cache_id(id).is_ok()
                && attempt.len() == 32
                && attempt.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some((id.to_owned(), attempt.to_owned()))
        }) && let Some(blobs) = blobs.as_ref()
        {
            let temp_name = std::ffi::OsString::from(format!(".{id}.{attempt}.tmp"));
            let temp_path = blobs_path.join(&temp_name);
            if let Some(temp) =
                open_cache_file_child(blobs, &temp_name, &temp_path, "admitted upload temp")?
            {
                let temp_identity =
                    verify_cache_descendant_mount(root_identity, &temp, "admitted upload temp")?;
                if !temp
                    .metadata()
                    .context("inspect pinned admitted upload temp")?
                    .is_file()
                {
                    continue;
                }
                if !try_lock_cache_file_exclusive(&temp, "admitted upload temp")? {
                    continue;
                }
                if unlink_cache_file_if_same(
                    root_identity,
                    blobs,
                    &temp_name,
                    &temp,
                    &temp_identity,
                    "admitted upload temp",
                )? {
                    removed_temps += 1;
                }
            }
        }
        if unlink_cache_file_if_same(
            root_identity,
            lease_directory,
            &lease_name,
            &lease,
            &lease_identity,
            "cache admission lease",
        )? {
            removed_leases += 1;
        }
    }
    if removed_temps > 0 {
        blobs
            .as_ref()
            .context("removed admitted temps without a pinned blobs directory")?
            .sync_all()
            .context("sync pinned tenant blobs after admission recovery")?;
    }
    if removed_leases > 0 {
        lease_directory
            .sync_all()
            .context("sync pinned cache admission directory after recovery")?;
    }
    Ok(removed_leases.saturating_add(removed_temps))
}

#[cfg(not(unix))]
fn reap_stale_upload_admissions_at_pinned(
    lease_directory: &std::fs::File,
    tenant_directory: &std::fs::File,
    tenant_root_path: &Path,
    lease_directory_path: &Path,
    root_identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Result<usize> {
    let _ = (
        lease_directory,
        tenant_directory,
        tenant_root_path,
        lease_directory_path,
        root_identity,
    );
    anyhow::bail!("descriptor-relative cache admission recovery is unavailable")
}

#[cfg(unix)]
fn open_or_create_cache_directory_child_with_sync<F>(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    parent_path: &Path,
    sync: &mut F,
    label: &str,
) -> Result<std::fs::File>
where
    F: FnMut(&Path) -> Result<()>,
{
    if let Some(directory) = open_cache_directory_child(parent, name, path, label)? {
        sync(parent_path).with_context(|| format!("sync parent of {label}"))?;
        parent
            .sync_all()
            .with_context(|| format!("sync pinned parent of {label}"))?;
        return Ok(directory);
    }

    match rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => {
            return Err(io::Error::from(error))
                .with_context(|| format!("create {label} {}", path.display()));
        }
    }
    let directory = open_cache_directory_child(parent, name, path, label)?
        .with_context(|| format!("new {label} {} disappeared", path.display()))?;
    sync(parent_path).with_context(|| format!("sync parent of new {label}"))?;
    parent
        .sync_all()
        .with_context(|| format!("sync pinned parent of new {label}"))?;
    Ok(directory)
}

#[cfg(not(unix))]
fn open_or_create_cache_directory_child_with_sync<F>(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    parent_path: &Path,
    sync: &mut F,
    label: &str,
) -> Result<std::fs::File>
where
    F: FnMut(&Path) -> Result<()>,
{
    let _ = (parent, name, path, parent_path, sync, label);
    anyhow::bail!("descriptor-relative cache directory access is unavailable")
}

/// Open/create the upload hierarchy relative to pinned directory descriptors.
/// Path-based `create_dir` could follow a replaced `uploads` symlink before
/// the later validation noticed it.
fn create_multipart_part_directory_with_sync<F>(directory: &Path, sync: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let uploads_directory = directory
        .parent()
        .context("multipart upload parts directory has no uploads parent")?;
    if uploads_directory.file_name().and_then(|name| name.to_str()) != Some("uploads") {
        anyhow::bail!("multipart upload parts directory is outside the uploads hierarchy");
    }
    let tenant_root = uploads_directory
        .parent()
        .context("multipart uploads directory has no tenant parent")?;
    let tenants_directory = tenant_root
        .parent()
        .context("multipart upload tenant directory has no tenants parent")?;
    if tenants_directory.file_name().and_then(|name| name.to_str()) != Some("tenants") {
        anyhow::bail!("multipart upload tenant directory is outside the tenants hierarchy");
    }
    let cache_root = tenants_directory
        .parent()
        .context("multipart upload tenants directory has no cache root")?;
    let namespace = tenant_root
        .file_name()
        .context("multipart upload tenant directory has no namespace")?;
    let upload_id = directory
        .file_name()
        .context("multipart upload parts directory has no upload ID")?;

    let cache_root_directory = open_cache_directory(cache_root, "cache root")?;
    let tenants_directory_handle = open_cache_directory_child(
        &cache_root_directory,
        std::ffi::OsStr::new("tenants"),
        tenants_directory,
        "cache tenants directory",
    )?
    .context("cache tenants directory is missing")?;
    let tenant_directory_handle = open_cache_directory_child(
        &tenants_directory_handle,
        namespace,
        tenant_root,
        "multipart upload tenant directory",
    )?
    .context("multipart upload tenant directory is missing")?;

    let uploads_handle = open_or_create_cache_directory_child_with_sync(
        &tenant_directory_handle,
        std::ffi::OsStr::new("uploads"),
        uploads_directory,
        tenant_root,
        sync,
        "multipart uploads directory",
    )?;
    open_or_create_cache_directory_child_with_sync(
        &uploads_handle,
        upload_id,
        directory,
        uploads_directory,
        sync,
        "multipart upload parts directory",
    )?;
    Ok(())
}

/// Make the part's contents, directory hierarchy, and both sides of its move
/// durable before a reservation manifest may name it as complete.
fn publish_multipart_part_with_sync<F>(
    staged_path: &Path,
    part_path: &Path,
    sync: &mut F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let destination_directory = part_path
        .parent()
        .context("multipart upload part has no parent")?;
    create_multipart_part_directory_with_sync(destination_directory, sync)?;

    let staged_metadata =
        std::fs::symlink_metadata(staged_path).context("stat staged multipart upload part")?;
    if !staged_metadata.file_type().is_file() {
        anyhow::bail!("staged multipart upload part is not a regular file");
    }
    match std::fs::symlink_metadata(part_path) {
        Ok(_) => anyhow::bail!("multipart upload part destination already exists"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("stat multipart upload part destination"),
    }
    sync(staged_path).context("sync staged multipart upload part contents")?;
    let source_directory = staged_path
        .parent()
        .context("staged multipart upload part has no parent")?;
    std::fs::rename(staged_path, part_path).context("publish multipart upload part")?;
    if source_directory != destination_directory {
        sync(source_directory).context("sync multipart upload source directory after rename")?;
    }
    sync(destination_directory).context("sync multipart upload destination directory")?;
    Ok(())
}

fn publish_multipart_part_then_persist_with_sync<F, P>(
    staged_path: &Path,
    part_path: &Path,
    persist_manifest: P,
    sync: &mut F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
    P: FnOnce() -> Result<()>,
{
    publish_multipart_part_with_sync(staged_path, part_path, sync)?;
    persist_manifest()
}

fn publish_multipart_part_then_persist<P>(
    staged_path: &Path,
    part_path: &Path,
    persist_manifest: P,
) -> Result<()>
where
    P: FnOnce() -> Result<()>,
{
    publish_multipart_part_then_persist_with_sync(
        staged_path,
        part_path,
        persist_manifest,
        &mut sync_multipart_path,
    )
}

fn replace_multipart_part_then_persist_with_sync<F, P>(
    staged_path: &Path,
    part_path: &Path,
    old_part_path: &Path,
    persist_manifest: P,
    sync: &mut F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
    P: FnOnce() -> Result<()>,
{
    publish_multipart_part_then_persist_with_sync(staged_path, part_path, persist_manifest, sync)?;
    let old_directory = old_part_path
        .parent()
        .context("replaced multipart upload part has no parent")?;
    match std::fs::remove_file(old_part_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("remove replaced multipart upload part"),
    }
    sync(old_directory).context("sync multipart upload directory after replacement cleanup")?;
    Ok(())
}

fn replace_multipart_part_then_persist<P>(
    staged_path: &Path,
    part_path: &Path,
    old_part_path: &Path,
    persist_manifest: P,
) -> Result<()>
where
    P: FnOnce() -> Result<()>,
{
    replace_multipart_part_then_persist_with_sync(
        staged_path,
        part_path,
        old_part_path,
        persist_manifest,
        &mut sync_multipart_path,
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_v1_multipart_part_with_sync<F>(
    service: &CacheService,
    id: &str,
    namespace: &str,
    reservation: &mut V1Reservation,
    chunk: V1UploadChunk,
    staged_path: &Path,
    part_path: &Path,
    sync: &mut F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let attempt = chunk.attempt.clone();
    let already_stored = reservation
        .chunks
        .iter()
        .any(|existing| existing.start == chunk.start && existing.end == chunk.end);
    if !already_stored {
        if let Err(error) = publish_multipart_part_with_sync(staged_path, part_path, sync) {
            reservation
                .active_uploads
                .retain(|active| active != &attempt);
            update_v1_reservation(service, id, namespace, reservation)?;
            return Err(error).context("persist v1 cache range chunk");
        }
        reservation.chunks.push(chunk);
    }
    reservation
        .active_uploads
        .retain(|active| active != &attempt);
    update_v1_reservation(service, id, namespace, reservation)
}

fn commit_v1_multipart_part(
    service: &CacheService,
    id: &str,
    namespace: &str,
    reservation: &mut V1Reservation,
    chunk: V1UploadChunk,
    staged_path: &Path,
    part_path: &Path,
) -> Result<()> {
    commit_v1_multipart_part_with_sync(
        service,
        id,
        namespace,
        reservation,
        chunk,
        staged_path,
        part_path,
        &mut sync_multipart_path,
    )
}

fn set_v1_active_upload(
    service: &CacheService,
    id: &str,
    namespace: &str,
    attempt: &str,
    active: bool,
) -> Result<V1Reservation> {
    let Some((record, _)) = read_reservation_record(service, id, Some(namespace))? else {
        anyhow::bail!("v1 cache reservation disappeared during upload");
    };
    let mut reservation = parse_v1_reservation(record, id)?;
    if active {
        if reservation.commit_attempt.is_some() {
            return Err(anyhow::Error::new(CacheLockBusy).context("v1 cache entry is committing"));
        }
        if !reservation
            .active_uploads
            .iter()
            .any(|value| value == attempt)
        {
            reservation.active_uploads.push(attempt.to_owned());
        }
    } else {
        reservation.active_uploads.retain(|value| value != attempt);
    }
    update_v1_reservation(service, id, namespace, &reservation)?;
    Ok(reservation)
}

async fn upload_v1_chunk<B>(
    req: Request<B>,
    ctx: &Ctx,
    namespace: &str,
    numeric_id: &str,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let id = resolve_v1_cache_id(&ctx.service, namespace, numeric_id)?;
    let (start, end, total) = parse_content_range(&req)?;
    let chunk_size = end - start + 1;
    let declared_size = request_content_length(&req)?;
    if declared_size.is_some_and(|size| size != chunk_size) {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let max_bytes = MAX_BODY.min(ctx.service.budget_bytes);
    if chunk_size > max_bytes || end >= max_bytes {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let body = req.into_body();
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (temp, mut admission) = {
        let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
        verify_v1_cache_id_locked(&ctx.service, namespace, numeric_id, &id)?;
        let _ = reconcile_inactive_upload_markers(&ctx.service, &id, Some(namespace))?;
        let Some(mut reservation) = read_v1_reservation(&ctx.service, &id, namespace)? else {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        };
        if reservation.cache_id.to_string() != numeric_id {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        if reservation
            .expected_size
            .is_some_and(|size| total.is_some_and(|range_total| range_total != size) || end >= size)
        {
            return Err(anyhow::Error::new(CacheBadRequest));
        }
        if let Some(total) = total {
            if total > max_bytes {
                return Err(anyhow::Error::new(CacheBadRequest));
            }
            if reservation.expected_size.is_none() {
                reservation.expected_size = Some(total);
                update_v1_reservation(&ctx.service, &id, namespace, &reservation)?;
            }
        }
        if reservation.chunks.iter().any(|chunk| {
            start <= chunk.end && chunk.start <= end && !(start == chunk.start && end == chunk.end)
        }) {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("v1 cache chunk overlaps a different range"));
        }
        let already_stored = reservation
            .chunks
            .iter()
            .any(|chunk| start == chunk.start && end == chunk.end);
        if !already_stored
            && reservation
                .chunks
                .len()
                .saturating_add(reservation.active_uploads.len())
                >= MAX_V1_UPLOAD_CHUNKS
        {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("v1 cache reservation reached its range count limit"));
        }
        let sum = reservation
            .chunks
            .iter()
            .try_fold(if already_stored { 0 } else { chunk_size }, |sum, chunk| {
                sum.checked_add(chunk.end - chunk.start + 1)
            })
            .context("v1 cache chunk total overflow")?;
        if sum > max_bytes {
            return Err(anyhow::Error::new(CacheBadRequest));
        }
        let admission = ctx.service.try_admit_upload_locked(
            Some(namespace),
            &id,
            &attempt,
            max_bytes,
            Some(chunk_size),
            0,
            1,
            _lock.namespace_guard(),
        )?;
        let temp = create_upload_temp(&ctx.service, &id, Some(namespace), &attempt)?;
        set_v1_active_upload(&ctx.service, &id, namespace, &attempt, true)?;
        (temp, admission)
    };

    let staged = match stage_upload_with_deadline(
        body,
        temp,
        UploadLimits {
            declared_size,
            expected_size: Some(chunk_size),
            max_bytes: chunk_size,
            body_idle_timeout: Some(std::time::Duration::from_secs(V2_UPLOAD_IDLE_TIMEOUT_SECS)),
        },
        "v1",
        &ctx.service,
        &mut admission,
    )
    .await
    {
        Ok(staged) => staged,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
            let _ = set_v1_active_upload(&ctx.service, &id, namespace, &attempt, false);
            return Err(error);
        }
    };
    let digest = match sha256_file(&staged.path) {
        Ok(digest) => digest,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
            let _ = set_v1_active_upload(&ctx.service, &id, namespace, &attempt, false);
            return Err(error);
        }
    };
    let part_path = v1_chunk_file_path(&ctx.service, namespace, &id, &attempt);
    let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
    let Some(mut reservation) = read_v1_reservation(&ctx.service, &id, namespace)? else {
        clear_upload_attempt_if_matches(&ctx.service, &id, Some(namespace), &attempt)?;
        anyhow::bail!("v1 cache reservation expired during chunk upload");
    };
    if reservation.cache_id.to_string() != numeric_id || reservation.commit_attempt.is_some() {
        clear_upload_attempt_if_matches(&ctx.service, &id, Some(namespace), &attempt)?;
        anyhow::bail!("v1 cache reservation changed during chunk upload");
    }
    if reservation.chunks.iter().any(|chunk| {
        start <= chunk.end && chunk.start <= end && !(start == chunk.start && end == chunk.end)
    }) {
        reservation.active_uploads.retain(|value| value != &attempt);
        update_v1_reservation(&ctx.service, &id, namespace, &reservation)?;
        return Err(anyhow::Error::new(CacheBadRequest)
            .context("v1 cache chunk overlaps a concurrently committed range"));
    }
    if reservation
        .chunks
        .iter()
        .find(|chunk| chunk.start == start && chunk.end == end)
        .is_some_and(|chunk| chunk.sha256 != digest)
    {
        reservation.active_uploads.retain(|value| value != &attempt);
        update_v1_reservation(&ctx.service, &id, namespace, &reservation)?;
        return Err(anyhow::Error::new(CacheBadRequest)
            .context("v1 retried chunk bytes differ from original range"));
    }
    if !reservation
        .chunks
        .iter()
        .any(|chunk| chunk.start == start && chunk.end == end)
        && reservation.chunks.len() >= MAX_V1_UPLOAD_CHUNKS
    {
        reservation.active_uploads.retain(|value| value != &attempt);
        update_v1_reservation(&ctx.service, &id, namespace, &reservation)?;
        return Err(anyhow::Error::new(CacheBadRequest)
            .context("v1 cache reservation reached its range count limit"));
    }
    commit_v1_multipart_part(
        &ctx.service,
        &id,
        namespace,
        &mut reservation,
        V1UploadChunk {
            start,
            end,
            attempt: attempt.clone(),
            sha256: digest,
        },
        &staged.path,
        &part_path,
    )?;
    admission.release_under(_lock.namespace_guard())?;
    drop(staged);
    drop(admission);
    Ok(())
}

async fn assemble_v1_chunks(
    service: &CacheService,
    namespace: &str,
    id: &str,
    chunks: &[V1UploadChunk],
    expected_size: u64,
    upload: UploadTempWriter,
) -> Result<StagedUpload> {
    let UploadTempWriter {
        path,
        cleanup,
        _active_lock,
        mut writer,
    } = upload;
    let mut actual = 0u64;
    for chunk in chunks {
        let part = v1_chunk_file_path(service, namespace, id, &chunk.attempt);
        let metadata = std::fs::symlink_metadata(&part).context("stat v1 range part")?;
        if !metadata.file_type().is_file()
            || metadata.len() != chunk.end - chunk.start + 1
            || sha256_file(&part)? != chunk.sha256
        {
            anyhow::bail!("v1 cache range part failed integrity validation");
        }
        let mut reader = tokio::fs::File::open(&part).await?;
        actual = actual
            .checked_add(tokio::io::copy(&mut reader, &mut writer).await?)
            .context("v1 assembled cache size overflow")?;
    }
    if actual != expected_size {
        anyhow::bail!("v1 cache assembled size does not match commit size");
    }
    writer.flush().await?;
    writer.sync_all().await?;
    drop(writer);
    Ok(StagedUpload {
        path,
        _cleanup: cleanup,
        _active_lock,
        actual_size: actual,
    })
}

async fn commit_v1<B>(req: Request<B>, ctx: &Ctx, namespace: &str, numeric_id: &str) -> Result<()>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = body_json(req).await?;
    let size = body["size"]
        .as_u64()
        .ok_or_else(|| anyhow::Error::new(CacheBadRequest))?;
    if size > MAX_BODY || size > ctx.service.budget_bytes {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let id = resolve_v1_cache_id(&ctx.service, namespace, numeric_id)?;
    ctx.service.ensure_tenant(namespace)?;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (reservation, chunks, assembly_temp, mut admission) =
        {
            let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
            verify_v1_cache_id_locked(&ctx.service, namespace, numeric_id, &id)?;
            if reconcile_inactive_upload_markers(&ctx.service, &id, Some(namespace))? {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("v1 cache chunks are still uploading"));
            }
            let Some(mut reservation) = read_v1_reservation(&ctx.service, &id, namespace)? else {
                return Err(anyhow::Error::new(CacheProtocolConflict));
            };
            if reservation.cache_id.to_string() != numeric_id {
                return Err(anyhow::Error::new(CacheProtocolConflict));
            }
            if reservation
                .expected_size
                .is_some_and(|expected| expected != size)
            {
                return Err(anyhow::Error::new(CacheBadRequest));
            }
            if !reservation.active_uploads.is_empty() {
                return Err(anyhow::Error::new(CacheLockBusy)
                    .context("v1 cache chunks are still uploading"));
            }
            reservation.chunks.sort_by_key(|chunk| chunk.start);
            let mut next = 0u64;
            for chunk in &reservation.chunks {
                if chunk.start != next {
                    return Err(anyhow::Error::new(CacheBadRequest)
                        .context("v1 cache upload has a missing range"));
                }
                next = chunk.end + 1;
            }
            if next != size {
                return Err(anyhow::Error::new(CacheBadRequest)
                    .context("v1 cache upload ranges do not cover committed size"));
            }
            // A prior process may have published the canonical blob and
            // crashed before its entry JSON. Remove that exact unpublished
            // same-ID blob under the namespace+entry locks before admission,
            // so a retry is charged by physical peak use rather than twice.
            remove_unpublished_cache_blob(&ctx.service, &id, namespace)?;
            let admission = ctx.service.try_admit_upload_locked(
                Some(namespace),
                &id,
                &attempt,
                MAX_BODY.min(ctx.service.budget_bytes),
                Some(0),
                size,
                2,
                _lock.namespace_guard(),
            )?;
            reservation.commit_attempt = Some(attempt.clone());
            reservation.expected_size = Some(size);
            let assembly_temp = create_upload_temp(&ctx.service, &id, Some(namespace), &attempt)?;
            update_v1_reservation(&ctx.service, &id, namespace, &reservation)?;
            (
                reservation.clone(),
                reservation.chunks.clone(),
                assembly_temp,
                admission,
            )
        };

    let staged = match assemble_v1_chunks(
        &ctx.service,
        namespace,
        &id,
        &chunks,
        size,
        assembly_temp,
    )
    .await
    {
        Ok(staged) => staged,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
            if let Some(mut current) = read_v1_reservation(&ctx.service, &id, namespace)?
                && current.cache_id == reservation.cache_id
                && current.commit_attempt.as_deref() == Some(attempt.as_str())
            {
                current.commit_attempt = None;
                update_v1_reservation(&ctx.service, &id, namespace, &current)?;
            }
            return Err(error);
        }
    };

    let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
    let Some(current) = read_v1_reservation(&ctx.service, &id, namespace)? else {
        anyhow::bail!("v1 cache reservation disappeared during commit");
    };
    if current.cache_id != reservation.cache_id
        || current.commit_attempt.as_deref() != Some(attempt.as_str())
        || !current.active_uploads.is_empty()
    {
        anyhow::bail!("v1 cache reservation changed during commit");
    }
    let actual_size = publish_staged_upload(&ctx.service, &id, Some(namespace), staged)?;
    if actual_size != size {
        anyhow::bail!("v1 committed cache size does not match uploaded ranges");
    }
    let published = commit_entry_without_overwrite(
        &ctx.service,
        &current.key,
        &current.version,
        actual_size,
        Some(namespace),
    )?;
    if !published {
        validate_existing_v1_entry(&ctx.service, &id, &current, namespace)?;
    }
    clear_v1_reservation(&ctx.service, &id, namespace)?;
    let chunks_dir = ctx
        .service
        .tenant_root(Some(namespace))
        .join("uploads")
        .join(&id);
    match std::fs::remove_dir_all(&chunks_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove committed v1 upload chunks"),
    }
    admission.release_under(_lock.namespace_guard())?;
    drop(_lock);
    drop(admission);
    ctx.service.enforce_budget(Some(namespace)).await?;
    if !ctx.service.entry_path(&id, Some(namespace)).exists() {
        return Err(anyhow::Error::new(CacheEntryNotRetained));
    }
    Ok(())
}

#[cfg(test)]
async fn upload_v1_body<B>(
    body: B,
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    declared_size: Option<u64>,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_cache_id(id)?;
    let max_bytes = MAX_BODY.min(ctx.service.budget_bytes);
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (reservation, generation, temp, mut admission) = {
        let lock = lock_cache_entry(&ctx.service, id, namespace).await?;
        let Some((record, _)) = read_reservation_record(&ctx.service, id, Some(namespace))? else {
            anyhow::bail!("v1 cache reservation is missing");
        };
        if record.get("protocol").is_some() {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        let reservation = parse_v1_reservation(record, id)?;
        if let (Some(expected_size), Some(declared_size)) =
            (reservation.expected_size, declared_size)
            && declared_size != expected_size
        {
            anyhow::bail!(
                "cache upload size mismatch: reserved {expected_size}, declared {declared_size}"
            );
        }
        if reservation
            .expected_size
            .is_some_and(|size| size > max_bytes)
        {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        retire_inactive_upload_attempt(&ctx.service, id, namespace)?;
        let admission = ctx.service.try_admit_upload_locked(
            Some(namespace),
            id,
            &attempt,
            max_bytes,
            declared_size,
            0,
            2,
            lock.namespace_guard(),
        )?;
        let (generation, attempt) =
            begin_v1_upload_attempt(&ctx.service, id, namespace, &reservation, &attempt)?;
        let temp = create_upload_temp(&ctx.service, id, Some(namespace), &attempt)?;
        drop(lock);
        (reservation, generation, temp, admission)
    };

    let limits = UploadLimits {
        declared_size,
        expected_size: reservation.expected_size,
        max_bytes,
        body_idle_timeout: Some(std::time::Duration::from_secs(V2_UPLOAD_IDLE_TIMEOUT_SECS)),
    };
    let staged =
        match stage_upload_with_deadline(body, temp, limits, "v1", &ctx.service, &mut admission)
            .await
        {
            Ok(staged) => staged,
            Err(error) => {
                clear_failed_v1_upload_attempt(
                    ctx,
                    id,
                    namespace,
                    &reservation,
                    &generation,
                    &attempt,
                )
                .await?;
                return Err(error);
            }
        };

    let lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    validate_v1_reservation_attempt(
        &ctx.service,
        id,
        namespace,
        &reservation,
        &generation,
        &attempt,
    )?;
    let actual_size = publish_staged_upload(&ctx.service, id, Some(namespace), staged)?;
    if actual_size > ctx.service.budget_bytes {
        remove_unpublished_cache_blob(&ctx.service, id, namespace)?;
        clear_v1_reservation(&ctx.service, id, namespace)?;
        return Err(anyhow::Error::new(CacheEntryTooLarge));
    }
    if !commit_entry_without_overwrite(
        &ctx.service,
        &reservation.key,
        &reservation.version,
        actual_size,
        Some(namespace),
    )? {
        validate_existing_v1_entry(&ctx.service, id, &reservation, namespace)?;
    }
    clear_v1_reservation(&ctx.service, id, namespace)?;
    admission.release_under(lock.namespace_guard())?;
    drop(lock);
    drop(admission);

    ctx.service.enforce_budget(Some(namespace)).await?;
    if !ctx.service.entry_path(id, Some(namespace)).exists() {
        return Err(anyhow::Error::new(CacheEntryNotRetained));
    }
    Ok(())
}

async fn upload_v2_body<B>(
    body: B,
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    declared_size: Option<u64>,
    nonce: &str,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_cache_id(id)?;
    let max_bytes = MAX_BODY.min(ctx.service.budget_bytes);
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (temp, mut admission) = {
        let lock = lock_cache_entry(&ctx.service, id, namespace).await?;
        if reconcile_inactive_upload_markers(&ctx.service, id, Some(namespace))? {
            return Err(
                anyhow::Error::new(CacheLockBusy).context("previous v2 upload is still active")
            );
        }
        let Some((record, _)) = read_reservation_record(&ctx.service, id, Some(namespace))? else {
            anyhow::bail!("v2 cache reservation is missing or already finalized");
        };
        if record.get("protocol").and_then(Value::as_str) != Some("v2") {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        let Some(mut reservation) = read_v2_reservation(&ctx.service, id, namespace)? else {
            anyhow::bail!("v2 cache reservation is missing or already finalized");
        };
        if reservation.upload_nonce != nonce
            || v2_reservation_is_expired(&reservation, now_unix_millis()?)
        {
            anyhow::bail!("v2 upload URL has a stale claim nonce");
        }
        if reservation.upload_attempt.is_some()
            || !reservation.active_uploads.is_empty()
            || !reservation.upload_blocks.is_empty()
        {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        if ctx.service.entry_path(id, Some(namespace)).exists() {
            anyhow::bail!("v2 cache entry already exists");
        }
        remove_unpublished_cache_blob(&ctx.service, id, namespace)?;
        if let Some(previous_attempt) = reservation.assembly_scratch_attempt.take() {
            release_global_assembly_scratch(
                &ctx.service.root,
                Some(namespace),
                id,
                &previous_attempt,
            )?;
            reservation.assembly_scratch_bytes = 0;
            reservation.persist(&ctx.service, id, namespace)?;
        }
        let admission = ctx.service.try_admit_upload_locked(
            Some(namespace),
            id,
            &attempt,
            max_bytes,
            declared_size,
            0,
            1,
            lock.namespace_guard(),
        )?;
        let temp = create_upload_temp(&ctx.service, id, Some(namespace), &attempt)?;
        let _ = begin_v2_upload_attempt(&ctx.service, id, namespace, nonce, &attempt)?;
        drop(lock);
        (temp, admission)
    };

    let limits = UploadLimits {
        declared_size,
        expected_size: None,
        max_bytes,
        body_idle_timeout: Some(std::time::Duration::from_secs(V2_UPLOAD_IDLE_TIMEOUT_SECS)),
    };
    let staged =
        match stage_upload_with_deadline(body, temp, limits, "v2", &ctx.service, &mut admission)
            .await
        {
            Ok(staged) => staged,
            Err(error) => {
                let discard_claim = error.downcast_ref::<CacheEntryTooLarge>().is_some();
                clear_failed_v2_upload_attempt(ctx, id, namespace, nonce, &attempt, discard_claim)
                    .await?;
                return Err(error);
            }
        };

    let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    let Some(reservation) = read_v2_reservation(&ctx.service, id, namespace)? else {
        anyhow::bail!("v2 cache reservation disappeared during upload");
    };
    if reservation.upload_nonce != nonce
        || reservation.upload_attempt.as_deref() != Some(attempt.as_str())
    {
        anyhow::bail!("v2 cache reservation changed during upload");
    }
    if ctx.service.entry_path(id, Some(namespace)).exists() {
        anyhow::bail!("v2 cache entry was finalized during upload");
    }
    publish_staged_upload(&ctx.service, id, Some(namespace), staged)?;
    complete_v2_upload_attempt(&ctx.service, id, namespace, nonce, &attempt)?;
    admission.release_under(_lock.namespace_guard())?;
    drop(_lock);
    drop(admission);
    Ok(())
}

fn v2_upload_part_path(
    service: &CacheService,
    namespace: &str,
    id: &str,
    attempt: &str,
) -> PathBuf {
    service
        .tenant_root(Some(namespace))
        .join("uploads")
        .join(id)
        .join(format!("v2-{attempt}.part"))
}

fn decode_azure_block_id(value: &str) -> Result<String> {
    azure_block_id_decoded_len(value)?;
    Ok(value.to_owned())
}

fn azure_block_id_decoded_len(value: &str) -> Result<usize> {
    if value.is_empty() || value.len() > 128 {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| anyhow::Error::new(CacheBadRequest))?;
    if decoded.is_empty() || decoded.len() > 64 {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    Ok(decoded.len())
}

fn v2_block_slot_available(staged: usize, active: usize, replaces_existing: bool) -> bool {
    replaces_existing || staged.saturating_add(active) < MAX_V2_UNCOMMITTED_BLOCKS
}

async fn stage_v2_block<B>(
    req: Request<B>,
    ctx: &Ctx,
    namespace: &str,
    id: &str,
    nonce: Option<&str>,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_cache_id(id)?;
    let nonce = nonce.ok_or_else(|| anyhow::Error::new(CacheBadRequest))?;
    let block_id = decode_azure_block_id(
        &query_param(&req, "blockid").ok_or_else(|| anyhow::Error::new(CacheBadRequest))?,
    )?;
    let block_id_bytes = azure_block_id_decoded_len(&block_id)?;
    let declared_size = request_content_length(&req)?;
    let max_bytes = MAX_BODY.min(ctx.service.budget_bytes);
    if declared_size.is_some_and(|size| size > max_bytes) {
        return Err(anyhow::Error::new(CacheEntryTooLarge));
    }
    let body = req.into_body();
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (temp, mut admission) = {
        let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
        let _ = reconcile_inactive_upload_markers(&ctx.service, id, Some(namespace))?;
        let Some(mut reservation) = read_v2_reservation(&ctx.service, id, namespace)? else {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        };
        if reservation.upload_nonce != nonce
            || v2_reservation_is_expired(&reservation, now_unix_millis()?)
            || reservation.upload_attempt.is_some()
        {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        if ctx.service.entry_path(id, Some(namespace)).exists() {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        if reservation
            .block_id_bytes
            .is_some_and(|expected| expected != block_id_bytes)
        {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("Azure block IDs for one blob must have equal decoded lengths"));
        }
        let replaces_existing = reservation
            .upload_blocks
            .iter()
            .any(|block| block.block_id == block_id);
        if !v2_block_slot_available(
            reservation.upload_blocks.len(),
            reservation.active_uploads.len(),
            replaces_existing,
        ) {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("Azure cache blob reached its uncommitted block limit"));
        }
        let existing_bytes = reservation
            .upload_blocks
            .iter()
            .filter(|block| block.block_id != block_id)
            .try_fold(0u64, |sum, block| sum.checked_add(block.size))
            .context("v2 upload block byte count overflow")?;
        if existing_bytes.saturating_add(declared_size.unwrap_or(0)) > max_bytes {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        let admission = ctx.service.try_admit_upload_locked(
            Some(namespace),
            id,
            &attempt,
            max_bytes,
            declared_size,
            0,
            1,
            _lock.namespace_guard(),
        )?;
        let temp = create_upload_temp(&ctx.service, id, Some(namespace), &attempt)?;
        if !reservation.active_uploads.contains(&attempt) {
            reservation.active_uploads.push(attempt.clone());
        }
        reservation.block_id_bytes = Some(block_id_bytes);
        reservation.updated_ms = now_unix_millis()?;
        reservation.persist(&ctx.service, id, namespace)?;
        (temp, admission)
    };

    let staged = match stage_upload_with_deadline(
        body,
        temp,
        UploadLimits {
            declared_size,
            expected_size: None,
            max_bytes,
            body_idle_timeout: Some(std::time::Duration::from_secs(V2_UPLOAD_IDLE_TIMEOUT_SECS)),
        },
        "v2 Azure block",
        &ctx.service,
        &mut admission,
    )
    .await
    {
        Ok(staged) => staged,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
            clear_upload_attempt_if_matches(&ctx.service, id, Some(namespace), &attempt)?;
            return Err(error);
        }
    };
    let digest = match sha256_file(&staged.path) {
        Ok(digest) => digest,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
            clear_upload_attempt_if_matches(&ctx.service, id, Some(namespace), &attempt)?;
            return Err(error);
        }
    };
    let part = v2_upload_part_path(&ctx.service, namespace, id, &attempt);
    let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    let Some(mut reservation) = read_v2_reservation(&ctx.service, id, namespace)? else {
        anyhow::bail!("v2 cache claim expired during Azure block upload");
    };
    if reservation.upload_nonce != nonce
        || reservation.block_id_bytes != Some(block_id_bytes)
        || !reservation.active_uploads.contains(&attempt)
    {
        anyhow::bail!("v2 cache claim changed during Azure block upload");
    }
    let mut publish_part = false;
    let mut replaced_part = None;
    if let Some(existing_index) = reservation
        .upload_blocks
        .iter()
        .position(|block| block.block_id == block_id)
    {
        let existing = reservation.upload_blocks[existing_index].clone();
        if existing.size == staged.actual_size && existing.sha256 == digest {
            // The manifest already names a durable identical part. This retry
            // only needs to clear its active marker below.
        } else {
            // Azure Put Block replaces the uncommitted block for this ID.
            // Make a new per-attempt path durable before publishing its
            // manifest reference. Keep the old part until that manifest is
            // durable, so every crash cut leaves the recorded block present.
            let old_part = v2_upload_part_path(&ctx.service, namespace, id, &existing.attempt);
            reservation.upload_blocks[existing_index] = V2UploadBlock {
                block_id,
                attempt: attempt.clone(),
                size: staged.actual_size,
                sha256: digest,
            };
            publish_part = true;
            replaced_part = Some(old_part);
        }
    } else {
        if reservation.upload_blocks.len() >= MAX_V2_UNCOMMITTED_BLOCKS {
            reservation.active_uploads.retain(|value| value != &attempt);
            reservation.updated_ms = now_unix_millis()?;
            reservation.persist(&ctx.service, id, namespace)?;
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("Azure cache blob reached its uncommitted block limit"));
        }
        let existing_bytes = reservation
            .upload_blocks
            .iter()
            .filter(|block| block.block_id != block_id)
            .try_fold(staged.actual_size, |sum, block| sum.checked_add(block.size))
            .context("v2 upload block byte count overflow")?;
        if existing_bytes > max_bytes {
            reservation.active_uploads.retain(|value| value != &attempt);
            reservation.updated_ms = now_unix_millis()?;
            reservation.persist(&ctx.service, id, namespace)?;
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        reservation.upload_blocks.push(V2UploadBlock {
            block_id,
            attempt: attempt.clone(),
            size: staged.actual_size,
            sha256: digest,
        });
        publish_part = true;
    }
    reservation.active_uploads.retain(|value| value != &attempt);
    reservation.updated_ms = now_unix_millis()?;
    if let Some(old_part) = replaced_part {
        replace_multipart_part_then_persist(&staged.path, &part, &old_part, || {
            reservation.persist(&ctx.service, id, namespace)
        })
        .context("replace Azure cache block")?;
    } else if publish_part {
        publish_multipart_part_then_persist(&staged.path, &part, || {
            reservation.persist(&ctx.service, id, namespace)
        })
        .context("persist Azure cache block")?;
    } else {
        reservation.persist(&ctx.service, id, namespace)?;
    }
    admission.release_under(_lock.namespace_guard())?;
    drop(staged);
    drop(admission);
    Ok(())
}

fn parse_azure_block_list(raw: &[u8]) -> Result<Vec<String>> {
    if raw.len() > 1024 * 1024 {
        return Err(anyhow::Error::new(CacheBadRequest));
    }
    let mut remaining =
        std::str::from_utf8(raw).map_err(|_| anyhow::Error::new(CacheBadRequest))?;
    if let Some(declaration) = remaining.strip_prefix("<?xml") {
        let Some(end) = declaration.find("?>") else {
            return Err(anyhow::Error::new(CacheBadRequest));
        };
        remaining = &declaration[end + 2..];
    }
    remaining = remaining.trim();
    let Some(mut remaining) = remaining.strip_prefix("<BlockList>") else {
        return Err(anyhow::Error::new(CacheBadRequest));
    };
    let mut block_ids = Vec::new();
    loop {
        remaining = remaining.trim_start();
        if let Some(after_close) = remaining.strip_prefix("</BlockList>") {
            if !after_close.trim().is_empty() || block_ids.is_empty() {
                return Err(anyhow::Error::new(CacheBadRequest));
            }
            return Ok(block_ids);
        }
        let Some(content) = remaining.strip_prefix("<Latest>") else {
            return Err(anyhow::Error::new(CacheBadRequest));
        };
        let Some(close) = content.find("</Latest>") else {
            return Err(anyhow::Error::new(CacheBadRequest));
        };
        let value = content[..close].trim();
        if value.is_empty() || value.contains(['<', '>', '&']) {
            return Err(anyhow::Error::new(CacheBadRequest));
        }
        block_ids.push(decode_azure_block_id(value)?);
        remaining = &content[close + "</Latest>".len()..];
        if block_ids.len() > MAX_V2_COMMITTED_BLOCKS {
            return Err(anyhow::Error::new(CacheBadRequest));
        }
    }
}

async fn assemble_v2_blocks(
    service: &CacheService,
    namespace: &str,
    id: &str,
    blocks: &[V2UploadBlock],
    expected_size: u64,
    upload: UploadTempWriter,
) -> Result<StagedUpload> {
    let UploadTempWriter {
        path,
        cleanup,
        _active_lock,
        mut writer,
    } = upload;
    let mut actual = 0u64;
    for block in blocks {
        let part = v2_upload_part_path(service, namespace, id, &block.attempt);
        let metadata = std::fs::symlink_metadata(&part).context("stat Azure cache block")?;
        if !metadata.file_type().is_file()
            || metadata.len() != block.size
            || sha256_file(&part)? != block.sha256
        {
            anyhow::bail!("Azure cache block failed integrity validation");
        }
        let mut reader = tokio::fs::File::open(part).await?;
        actual = actual
            .checked_add(tokio::io::copy(&mut reader, &mut writer).await?)
            .context("Azure block list size overflow")?;
    }
    if actual != expected_size {
        anyhow::bail!("Azure block list size differs from finalize size");
    }
    writer.flush().await?;
    writer.sync_all().await?;
    drop(writer);
    Ok(StagedUpload {
        path,
        _cleanup: cleanup,
        _active_lock,
        actual_size: actual,
    })
}

async fn commit_v2_block_list<B>(
    req: Request<B>,
    ctx: &Ctx,
    namespace: &str,
    id: &str,
    nonce: Option<&str>,
) -> Result<()>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_cache_id(id)?;
    let nonce = nonce.ok_or_else(|| anyhow::Error::new(CacheBadRequest))?;
    let bytes = Limited::new(req.into_body(), 1024 * 1024)
        .collect()
        .await
        .map_err(anyhow::Error::from_boxed)?
        .to_bytes();
    let ordered_ids = parse_azure_block_list(&bytes)?;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let (ordered_blocks, size, mut admission, assembly_temp) = {
        let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
        if reconcile_inactive_upload_markers(&ctx.service, id, Some(namespace))? {
            return Err(
                anyhow::Error::new(CacheLockBusy).context("v2 cache blocks are still uploading")
            );
        }
        let Some(mut reservation) = read_v2_reservation(&ctx.service, id, namespace)? else {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        };
        if reservation.upload_nonce != nonce
            || v2_reservation_is_expired(&reservation, now_unix_millis()?)
            || reservation.upload_attempt.is_some()
            || !reservation.active_uploads.is_empty()
        {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        if ctx.service.entry_path(id, Some(namespace)).exists() {
            return Err(anyhow::Error::new(CacheProtocolConflict));
        }
        let mut ordered_blocks = Vec::with_capacity(ordered_ids.len());
        let mut size = 0u64;
        for block_id in ordered_ids {
            let block = reservation
                .upload_blocks
                .iter()
                .find(|block| block.block_id == block_id)
                .ok_or_else(|| anyhow::Error::new(CacheBadRequest))?;
            size = size
                .checked_add(block.size)
                .context("Azure block list size overflow")?;
            ordered_blocks.push(block.clone());
        }
        if size > MAX_BODY.min(ctx.service.budget_bytes) {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        remove_unpublished_cache_blob(&ctx.service, id, namespace)?;
        let admission = ctx.service.try_admit_upload_locked(
            Some(namespace),
            id,
            &attempt,
            MAX_BODY.min(ctx.service.budget_bytes),
            Some(0),
            size,
            2,
            _lock.namespace_guard(),
        )?;
        let assembly_temp = create_upload_temp(&ctx.service, id, Some(namespace), &attempt)?;
        reservation.upload_attempt = Some(attempt.clone());
        reservation.updated_ms = now_unix_millis()?;
        reservation.persist(&ctx.service, id, namespace)?;
        (ordered_blocks, size, admission, assembly_temp)
    };

    let staged = match assemble_v2_blocks(
        &ctx.service,
        namespace,
        id,
        &ordered_blocks,
        size,
        assembly_temp,
    )
    .await
    {
        Ok(staged) => staged,
        Err(error) => {
            let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
            if let Some(current) = read_v2_reservation(&ctx.service, id, namespace)?
                && current.upload_nonce == nonce
                && current.upload_attempt.as_deref() == Some(attempt.as_str())
            {
                let cleared = V2Reservation {
                    upload_attempt: None,
                    updated_ms: now_unix_millis()?,
                    ..current
                };
                cleared.persist(&ctx.service, id, namespace)?;
            }
            return Err(error);
        }
    };
    let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    let Some(current) = read_v2_reservation(&ctx.service, id, namespace)? else {
        anyhow::bail!("v2 cache claim disappeared during Azure block commit");
    };
    if current.upload_nonce != nonce || current.upload_attempt.as_deref() != Some(&attempt) {
        anyhow::bail!("v2 cache claim changed during Azure block commit");
    }
    // Keep the root-wide scratch charge after this request finishes. The
    // persisted lease survives closing its active flock, and recovery checks
    // the temp/canonical/source paths across publication crash cuts.
    admission.retain_assembly_scratch()?;
    publish_staged_upload(&ctx.service, id, Some(namespace), staged)?;
    let completed = V2Reservation {
        upload_attempt: None,
        assembly_scratch_attempt: (size > 0).then(|| attempt.clone()),
        assembly_scratch_bytes: size,
        updated_ms: now_unix_millis()?,
        ..current
    };
    completed.persist(&ctx.service, id, namespace)?;
    admission.release_under(_lock.namespace_guard())?;
    drop(_lock);
    drop(admission);
    Ok(())
}

async fn upload_v2_blob<B>(
    req: Request<B>,
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    nonce: Option<&str>,
) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if req
        .headers()
        .get("x-ms-blob-type")
        .and_then(|value| value.to_str().ok())
        != Some("BlockBlob")
    {
        return Err(anyhow::Error::new(CacheBadRequest)
            .context("Azure Put Blob requires x-ms-blob-type: BlockBlob"));
    }
    let declared_size = request_content_length(&req)?;
    upload_v2_body(
        req.into_body(),
        ctx,
        id,
        namespace,
        declared_size,
        nonce.ok_or_else(|| anyhow::Error::new(CacheBadRequest))?,
    )
    .await
}

async fn stage_upload_with_deadline<B>(
    body: B,
    temp: UploadTempWriter,
    limits: UploadLimits,
    protocol: &str,
    service: &CacheService,
    admission: &mut UploadAdmission,
) -> Result<StagedUpload>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(
        std::time::Duration::from_secs(V2_UPLOAD_TIMEOUT_SECS),
        stage_upload_body(body, temp, limits, Some((service, admission))),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => anyhow::bail!("{protocol} cache upload exceeded its six-hour deadline"),
    }
}

#[cfg(test)]
async fn clear_failed_v1_upload_attempt(
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    expected: &V1Reservation,
    generation: &str,
    attempt: &str,
) -> Result<()> {
    let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    let Some((record, _)) = read_reservation_record(&ctx.service, id, Some(namespace))? else {
        return Ok(());
    };
    if parse_v1_reservation(record.clone(), id)? == *expected
        && reservation_upload_generation(&record)? == generation
        && record["uploadAttempt"].as_str() == Some(attempt)
    {
        clear_upload_attempt_if_matches(&ctx.service, id, Some(namespace), attempt)?;
    }
    Ok(())
}

async fn clear_failed_v2_upload_attempt(
    ctx: &Ctx,
    id: &str,
    namespace: &str,
    nonce: &str,
    attempt: &str,
    discard_claim: bool,
) -> Result<()> {
    let _lock = lock_cache_entry(&ctx.service, id, namespace).await?;
    let Some(current) = read_v2_reservation(&ctx.service, id, namespace)? else {
        return Ok(());
    };
    if current.upload_nonce != nonce || current.upload_attempt.as_deref() != Some(attempt) {
        return Ok(());
    }
    if discard_claim {
        remove_unpublished_cache_blob(&ctx.service, id, namespace)?;
        clear_v2_reservation(&ctx.service, id, namespace, nonce)?;
    } else {
        clear_upload_attempt_if_matches(&ctx.service, id, Some(namespace), attempt)?;
    }
    Ok(())
}

fn request_content_length<B>(req: &Request<B>) -> Result<Option<u64>> {
    req.headers()
        .get(CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| anyhow::Error::new(CacheBadRequest))?
                .parse()
                .map_err(|_| anyhow::Error::new(CacheBadRequest))
        })
        .transpose()
}

struct UploadTempWriter {
    path: PathBuf,
    cleanup: TemporaryUpload,
    _active_lock: std::fs::File,
    writer: tokio::fs::File,
}

struct StagedUpload {
    path: PathBuf,
    _cleanup: TemporaryUpload,
    _active_lock: std::fs::File,
    actual_size: u64,
}

fn validate_upload_limits(limits: UploadLimits) -> Result<()> {
    let UploadLimits {
        declared_size,
        expected_size,
        max_bytes,
        ..
    } = limits;
    if declared_size.is_some_and(|size| size > max_bytes) {
        return Err(anyhow::Error::new(CacheEntryTooLarge));
    }
    if let Some(expected_size) = expected_size {
        if expected_size > max_bytes {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        if let Some(declared_size) = declared_size
            && declared_size != expected_size
        {
            anyhow::bail!(
                "cache upload size mismatch: expected {expected_size}, declared {declared_size}"
            );
        }
    }
    Ok(())
}

/// Create and lock the unique temp inode while the caller still holds the
/// entry stripe. The reaper then uses this per-attempt flock while streaming,
/// so unrelated keys can use the stripe without making a live temp look stale.
fn create_upload_temp(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
    attempt: &str,
) -> Result<UploadTempWriter> {
    validate_cache_id(id)?;
    if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid cache upload attempt id");
    }
    let blob_dir = service.tenant_root(namespace).join("blobs");
    std::fs::create_dir_all(&blob_dir).context("create cache blob directory")?;
    let path = blob_dir.join(format!(".{id}.{attempt}.tmp"));
    let cleanup = TemporaryUpload::new(path.clone());
    let active_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .context("create temporary cache upload")?;
    rustix::fs::flock(
        &active_lock,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .context("lock active cache upload temp")?;
    let writer = tokio::fs::File::from_std(
        active_lock
            .try_clone()
            .context("clone active cache upload temp handle")?,
    );
    Ok(UploadTempWriter {
        path,
        cleanup,
        _active_lock: active_lock,
        writer,
    })
}

async fn stage_upload_body<B>(
    mut body: B,
    temp: UploadTempWriter,
    limits: UploadLimits,
    mut admission: Option<(&CacheService, &mut UploadAdmission)>,
) -> Result<StagedUpload>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_upload_limits(limits)?;
    let UploadLimits {
        declared_size,
        expected_size,
        max_bytes,
        body_idle_timeout,
    } = limits;
    let UploadTempWriter {
        path,
        cleanup,
        _active_lock,
        mut writer,
    } = temp;
    let mut actual_size = 0u64;

    loop {
        let next_frame = match body_idle_timeout {
            Some(timeout) => tokio::time::timeout(timeout, body.frame())
                .await
                .map_err(|_| anyhow::anyhow!("cache upload body stalled beyond {timeout:?}"))?,
            None => body.frame().await,
        };
        let Some(frame) = next_frame else {
            break;
        };
        let frame = frame.map_err(anyhow::Error::new)?;
        let bytes = frame
            .into_data()
            .map_err(|_| anyhow::anyhow!("cache upload frame carried trailers instead of data"))?;
        let frame_size = u64::try_from(bytes.len()).context("cache upload frame size overflow")?;
        actual_size = actual_size
            .checked_add(frame_size)
            .context("cache upload size overflow")?;
        if declared_size.is_some_and(|size| actual_size > size) {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("actual cache upload size exceeds declared Content-Length"));
        }
        if expected_size.is_some_and(|size| actual_size > size) {
            return Err(anyhow::Error::new(CacheBadRequest)
                .context("actual cache upload size exceeds expected cache size"));
        }
        if actual_size > max_bytes {
            return Err(anyhow::Error::new(CacheEntryTooLarge));
        }
        if let Some((service, upload_admission)) = admission.as_mut() {
            upload_admission.reserve_at_least(service, actual_size)?;
        }
        for chunk in bytes.chunks(TRANSFER_CHUNK_BYTES) {
            writer.write_all(chunk).await?;
        }
    }

    if let Some(declared_size) = declared_size
        && declared_size != actual_size
    {
        return Err(anyhow::Error::new(CacheBadRequest).context(format!(
            "cache upload size mismatch: declared {declared_size}, received {actual_size}"
        )));
    }
    if let Some(expected_size) = expected_size
        && expected_size != actual_size
    {
        return Err(anyhow::Error::new(CacheBadRequest).context(format!(
            "cache upload size mismatch: reserved {expected_size}, received {actual_size}"
        )));
    }
    writer.flush().await?;
    writer.sync_all().await?;
    drop(writer);

    Ok(StagedUpload {
        path,
        _cleanup: cleanup,
        _active_lock,
        actual_size,
    })
}

fn publish_staged_upload(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
    staged: StagedUpload,
) -> Result<u64> {
    let blob_dir = service.tenant_root(namespace).join("blobs");
    let dst = blob_dir.join(id);
    match std::fs::hard_link(&staged.path, &dst) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !files_are_identical(&staged.path, &dst)? {
                anyhow::bail!("refusing to overwrite existing cache blob with different bytes");
            }
        }
        Err(error) => {
            return Err(error).context("publish cache upload without replacing existing blob");
        }
    }
    sync_directory(&blob_dir).context("sync cache blob directory after publication")?;
    let actual_size = staged.actual_size;
    drop(staged);
    Ok(actual_size)
}

#[cfg(test)]
async fn store_upload<B>(
    body: B,
    service: &CacheService,
    id: &str,
    limits: UploadLimits,
    namespace: Option<&str>,
) -> Result<u64>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_upload_limits(limits)?;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let temp = create_upload_temp(service, id, namespace, &attempt)?;
    let staged = stage_upload_body(body, temp, limits, None).await?;
    publish_staged_upload(service, id, namespace, staged)
}

fn files_are_identical(left: &Path, right: &Path) -> Result<bool> {
    let left_metadata = std::fs::symlink_metadata(left).context("stat temporary cache upload")?;
    let right_metadata = std::fs::symlink_metadata(right).context("stat existing cache blob")?;
    if !left_metadata.file_type().is_file() || !right_metadata.file_type().is_file() {
        return Ok(false);
    }
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }

    let mut left_file = std::fs::File::open(left).context("open temporary cache upload")?;
    let mut right_file = std::fs::File::open(right).context("open existing cache blob")?;
    let mut left_buffer = [0u8; TRANSFER_CHUNK_BYTES];
    let mut right_buffer = [0u8; TRANSFER_CHUNK_BYTES];
    loop {
        let left_read = left_file
            .read(&mut left_buffer)
            .context("read temporary cache upload")?;
        let right_read = right_file
            .read(&mut right_buffer)
            .context("read existing cache blob")?;
        if left_read != right_read {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
        if left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open cache blob for digest {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; TRANSFER_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read cache blob for digest {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

struct TemporaryUpload {
    path: PathBuf,
}

impl TemporaryUpload {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TemporaryUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn validate_cache_id(id: &str) -> Result<()> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid cache id");
    }
    Ok(())
}

async fn finalize_v2<B>(req: Request<B>, ctx: &Ctx, namespace: &str) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let size = declared_cache_size(&body)?;
    let id = entry_hash(key, version);
    let _lock = lock_cache_entry(&ctx.service, &id, namespace).await?;
    if reconcile_inactive_upload_markers(&ctx.service, &id, Some(namespace))? {
        return Ok(json!({
            "ok": false,
            "message": "cache upload is still in progress",
        }));
    }

    if ctx.service.entry_path(&id, Some(namespace)).exists() {
        validate_existing_v1_entry(
            &ctx.service,
            &id,
            &V1Reservation {
                key: key.to_owned(),
                version: version.to_owned(),
                cache_id: 1,
                expected_size: Some(size),
                chunks: Vec::new(),
                commit_attempt: None,
                active_uploads: Vec::new(),
            },
            namespace,
        )?;
        if let Some(reservation) = read_v2_reservation(&ctx.service, &id, namespace)? {
            if reservation.key != key || reservation.version != version {
                anyhow::bail!("v2 cache finalization conflicts with a different reservation");
            }
            clear_v2_reservation(&ctx.service, &id, namespace, &reservation.upload_nonce)?;
        }
        drop(_lock);
        ctx.service.enforce_budget(Some(namespace)).await?;
        if !ctx.service.entry_path(&id, Some(namespace)).exists() {
            return Ok(json!({
                "ok": false,
                "message": "cache entry could not be retained within the namespace budget",
            }));
        }
        return Ok(v2_finalize_success(&v2_entry_id(
            &ctx.service,
            &id,
            namespace,
        )?));
    }

    if let Some((record, _)) = read_reservation_record(&ctx.service, &id, Some(namespace))?
        && record.get("protocol").and_then(Value::as_str) != Some("v2")
    {
        return Ok(json!({
            "ok": false,
            "message": "cache reservation belongs to another protocol",
        }));
    }
    let Some(reservation) = read_v2_reservation(&ctx.service, &id, namespace)? else {
        return Ok(json!({
            "ok": false,
            "message": "cache reservation is missing or already finalized",
        }));
    };
    if reservation.key != key || reservation.version != version {
        anyhow::bail!("v2 cache finalization does not match its reservation");
    }
    if reservation.upload_attempt.is_some() || !reservation.active_uploads.is_empty() {
        return Ok(json!({
            "ok": false,
            "message": "cache upload is still in progress",
        }));
    }
    if size > ctx.service.budget_bytes || size > MAX_BODY {
        remove_unpublished_cache_blob(&ctx.service, &id, namespace)?;
        clear_v2_reservation(&ctx.service, &id, namespace, &reservation.upload_nonce)?;
        return Ok(json!({
            "ok": false,
            "message": "cache entry exceeds the namespace byte budget",
        }));
    }
    let blob = ctx
        .service
        .tenant_root(Some(namespace))
        .join("blobs")
        .join(&id);
    let blob_metadata = match std::fs::symlink_metadata(&blob) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Ok(json!({
                "ok": false,
                "message": "uploaded cache blob is not a regular file",
            }));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(json!({
                "ok": false,
                "message": "uploaded cache blob is missing",
            }));
        }
        Err(error) => return Err(error).context("stat v2 blob before finalize"),
    };
    if blob_metadata.len() != size {
        remove_unpublished_cache_blob(&ctx.service, &id, namespace)?;
        return Ok(json!({
            "ok": false,
            "message": "uploaded cache size does not match finalize size",
        }));
    }
    let entry_id = stable_numeric_entry_id(&id);
    let published = commit_v2_entry_without_overwrite(
        &ctx.service,
        key,
        version,
        size,
        Some(namespace),
        &entry_id,
    )?;
    if !published {
        validate_existing_v1_entry(
            &ctx.service,
            &id,
            &V1Reservation {
                key: key.to_owned(),
                version: version.to_owned(),
                cache_id: 1,
                expected_size: Some(size),
                chunks: Vec::new(),
                commit_attempt: None,
                active_uploads: Vec::new(),
            },
            namespace,
        )?;
    }
    clear_v2_reservation(&ctx.service, &id, namespace, &reservation.upload_nonce)?;
    drop(_lock);
    ctx.service.enforce_budget(Some(namespace)).await?;
    if !ctx.service.entry_path(&id, Some(namespace)).exists() {
        return Ok(json!({
            "ok": false,
            "message": "cache entry could not be retained within the namespace budget",
        }));
    }
    Ok(v2_finalize_success(&v2_entry_id(
        &ctx.service,
        &id,
        namespace,
    )?))
}

fn stable_numeric_entry_id(id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-v2-entry-id-v1\0");
    hasher.update(id.as_bytes());
    let digest = hasher.finalize();
    let raw = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0u8; 8]));
    (raw & i64::MAX as u64).max(1).to_string()
}

fn v2_entry_id(service: &CacheService, id: &str, namespace: &str) -> Result<String> {
    let entry = service
        .read_entry(id, Some(namespace))
        .context("read committed cache entry id")?;
    Ok(entry["entryId"]
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| stable_numeric_entry_id(id)))
}

fn v2_finalize_success(entry_id: &str) -> Value {
    json!({
        "ok": true,
        "entryId": entry_id,
        "entry_id": entry_id,
        "state": "succeeded",
    })
}

fn declared_cache_size(body: &Value) -> Result<u64> {
    for field in ["size_bytes", "sizeBytes", "size"] {
        let Some(value) = body.get(field) else {
            continue;
        };
        if let Some(size) = value.as_u64() {
            return Ok(size);
        }
        if let Some(size) = value.as_str() {
            return size.parse().map_err(|_| {
                anyhow::Error::new(CacheBadRequest).context(format!("invalid {field}"))
            });
        }
        return Err(anyhow::Error::new(CacheBadRequest).context(format!("invalid {field}")));
    }
    Err(anyhow::Error::new(CacheBadRequest).context("missing cache upload size"))
}

#[cfg(test)]
fn commit_entry(
    ctx: &CacheService,
    key: &str,
    version: &str,
    size: u64,
    namespace: Option<&str>,
) -> Result<()> {
    if !commit_entry_without_overwrite(ctx, key, version, size, namespace)? {
        anyhow::bail!("cache entry already exists");
    }
    Ok(())
}

fn commit_entry_without_overwrite(
    ctx: &CacheService,
    key: &str,
    version: &str,
    size: u64,
    namespace: Option<&str>,
) -> Result<bool> {
    let (hash, entry) = validated_entry(ctx, key, version, size, namespace)?;
    std::fs::create_dir_all(ctx.tenant_root(namespace).join("entries"))?;
    if !atomically_create_json(&ctx.entry_path(&hash, namespace), &entry)? {
        return Ok(false);
    }
    Ok(true)
}

fn commit_v2_entry_without_overwrite(
    service: &CacheService,
    key: &str,
    version: &str,
    size: u64,
    namespace: Option<&str>,
    entry_id: &str,
) -> Result<bool> {
    let (hash, mut entry) = validated_entry(service, key, version, size, namespace)?;
    entry["entryId"] = json!(entry_id);
    std::fs::create_dir_all(service.tenant_root(namespace).join("entries"))?;
    if !atomically_create_json(&service.entry_path(&hash, namespace), &entry)? {
        return Ok(false);
    }
    Ok(true)
}

fn validated_entry(
    ctx: &CacheService,
    key: &str,
    version: &str,
    size: u64,
    namespace: Option<&str>,
) -> Result<(String, Value)> {
    let hash = entry_hash(key, version);
    let blob = ctx.tenant_root(namespace).join("blobs").join(&hash);
    let metadata = std::fs::metadata(&blob).context("stat uploaded cache blob")?;
    if !metadata.is_file() {
        anyhow::bail!("uploaded cache blob is not a regular file");
    }
    let actual = metadata.len();
    if actual > MAX_BODY {
        anyhow::bail!("actual cache upload size exceeds {MAX_BODY} bytes");
    }
    if actual != size {
        anyhow::bail!("cache upload size mismatch: declared {size}, received {actual}");
    }
    let content_sha256 = sha256_file(&blob)?;
    let entry = json!({
        "key": key,
        "version": version,
        "blob": hash,
        "size": actual,
        "contentSha256": content_sha256,
        "last_accessed_ns": now_unix_nanos(),
        "created_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0),
    });
    Ok((hash, entry))
}

/// v1 `GET _apis/artifactcache/cache?keys=…&version=…`.
///
/// The response body is `actions/toolkit`'s `ArtifactCacheEntry`
/// (`packages/cache/src/internal/contracts.ts`): the client reads
/// `archiveLocation` for the download and `cacheKey` for the matched key
/// (`packages/cache/src/internal/cacheHttpClient.ts:115`,
/// `packages/cache/src/cache.ts`). A miss is HTTP 204 with no body —
/// `getCacheEntry` returns `null` only on 204, and treats a 200 without
/// `archiveLocation` as a hard error.
fn lookup_v1<B>(req: &Request<B>, ctx: &Ctx, namespaces: &[&str]) -> Result<Option<Value>> {
    let keys = keys_from_query(req);
    let version = query_param(req, "version").unwrap_or_default();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    // Scope order is the outer loop: every key step runs on the job's own ref
    // before anything is tried on the base, exactly the order the GitHub cache
    // scope rules describe (current branch fully, then the fallback branch).
    for namespace in namespaces {
        if let Some(hit) = ctx.service.lookup(&keys, &version, Some(namespace))? {
            let capability = ctx
                .service
                .create_read_capability(namespace, &hit.hash, 1)?;
            return Ok(Some(json!({
                "archiveLocation": format!("{}/_results/download/{}?sig={capability}", ctx.public_base, hit.hash),
                "cacheKey": hit.key,
                "cacheVersion": version,
            })));
        }
    }
    Ok(None)
}

async fn lookup_v2<B>(req: Request<B>, ctx: &mut Ctx, namespaces: &[&str]) -> Result<Value>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = collect_json(req.into_body(), MAX_JSON_BODY).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let mut keys: Vec<String> = vec![key.to_owned()];
    let restore_keys = body.get("restore_keys").or_else(|| body.get("restoreKeys"));
    if let Some(restores) = restore_keys.and_then(Value::as_array) {
        keys.extend(
            restores
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned)),
        );
    }
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    // Scope order is the outer loop, like v1: the whole key sequence runs on
    // the job's own ref before the base scope is consulted.
    for namespace in namespaces {
        if let Some(hit) = ctx.service.lookup(&keys, version, Some(namespace))? {
            let capability = ctx
                .service
                .create_read_capability(namespace, &hit.hash, 3)?;
            // `matched_key` is field 3 of
            // `github.actions.results.api.v1.GetCacheEntryDownloadURLResponse`
            // (actions/toolkit `packages/cache/src/generated/results/api/v1/cache.ts`).
            // `restoreCache` compares it against the requested primary key to
            // decide exact hit vs restore-key hit and returns it as the cache key,
            // so omitting it makes every hit look like a restore-key hit and the
            // entry is re-saved on the next run.
            return Ok(json!({
                "ok": true,
                "signed_download_url": format!("{}/_results/download/{}?sig={capability}", ctx.public_base, hit.hash),
                "signedDownloadUrl": format!("{}/_results/download/{}?sig={capability}", ctx.public_base, hit.hash),
                "matched_key": hit.key,
                "matchedKey": hit.key,
            }));
        }
    }
    Ok(json!({"ok": false}))
}

/// Split a query string into decoded `(name, value)` pairs, preserving order
/// and repeats. Values are percent-decoded because `actions/toolkit` builds the
/// v1 lookup URL with `encodeURIComponent(keys.join(','))`, which escapes the
/// separators — including `,` as `%2C`.
///
/// `+` is left literal: `encodeURIComponent` emits `%20` for a space and never
/// `+`, so decoding `+` as a space would corrupt any cache key containing one.
fn query_pairs<B>(req: &Request<B>) -> Vec<(String, String)> {
    req.uri()
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (percent_decode(name), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// First value for `name`, or `None` when the query carries no such parameter.
fn query_param<B>(req: &Request<B>, name: &str) -> Option<String> {
    query_pairs(req)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Some(high) = (bytes[index + 1] as char).to_digit(16)
            && let Some(low) = (bytes[index + 2] as char).to_digit(16)
        {
            out.push((high * 16 + low) as u8);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn required_str<'a>(body: &'a Value, field: &str) -> Result<&'a str> {
    body[field]
        .as_str()
        .ok_or_else(|| anyhow::Error::new(CacheBadRequest).context(format!("missing {field}")))
}

#[cfg_attr(not(test), allow(dead_code))]
async fn download(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
) -> Result<(ResponseBody, u64)> {
    validate_cache_id(id)?;
    let entry = service
        .read_entry(id, namespace)
        .context("download before finalize (no entry)")?;
    if !service.mark_entry_accessed(id, namespace)? {
        return Err(anyhow::Error::new(CacheBlobNotFound));
    }
    let expected_size = entry["size"].as_u64().context("cache entry has no size")?;
    if expected_size > MAX_BODY {
        anyhow::bail!("cache entry size exceeds {MAX_BODY} bytes");
    }

    let blob_path = service.blob_path_for(&entry, namespace);
    let file = tokio::fs::File::open(&blob_path)
        .await
        .context("open cached blob")?;
    let metadata = file.metadata().await.context("stat cached blob")?;
    if !metadata.is_file() {
        anyhow::bail!("cached blob is not a regular file");
    }
    let actual_size = metadata.len();
    if actual_size > MAX_BODY {
        anyhow::bail!("actual cached blob size exceeds {MAX_BODY} bytes");
    }
    if actual_size != expected_size {
        anyhow::bail!(
            "cached blob size mismatch: entry records {expected_size}, file has {actual_size}"
        );
    }

    let (sender, body) = Channel::<Bytes, io::Error>::new(DOWNLOAD_BUFFERED_CHUNKS);
    tokio::spawn(stream_download(file, sender, actual_size, true));
    Ok((body.boxed_unsync(), actual_size))
}

/// Serve a download from the first scope holding the entry, in chain order.
/// The lookup that issued the URL searched the same order, so this serves what
/// the lookup reported; a scope whose entry vanished (budget eviction between
/// the two calls) simply yields to the next one.
#[cfg_attr(not(test), allow(dead_code))]
async fn download_chain(
    service: &CacheService,
    id: &str,
    namespaces: &[&str],
) -> Result<(ResponseBody, u64)> {
    let mut error = anyhow::anyhow!("no cache namespace served the download");
    for namespace in namespaces {
        match download(service, id, Some(namespace)).await {
            Ok(served) => return Ok(served),
            Err(failed) => error = failed,
        }
    }
    Err(error)
}

struct DownloadedCacheBlob {
    body: ResponseBody,
    total_size: u64,
    range: Option<(u64, u64)>,
}

fn parse_download_range(value: Option<&str>, total_size: u64) -> Result<Option<(u64, u64)>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.strip_prefix("bytes=") else {
        return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
    };
    if value.contains(',') {
        return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
    }
    let Some((start, end)) = value.split_once('-') else {
        return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
    };
    if total_size == 0 {
        return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
    }
    let (start, end) = if start.is_empty() {
        let suffix: u64 = end
            .parse()
            .map_err(|_| anyhow::Error::new(CacheRangeNotSatisfiable { total_size }))?;
        if suffix == 0 {
            return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
        }
        (total_size.saturating_sub(suffix), total_size - 1)
    } else {
        let start: u64 = start
            .parse()
            .map_err(|_| anyhow::Error::new(CacheRangeNotSatisfiable { total_size }))?;
        let end = if end.is_empty() {
            total_size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| anyhow::Error::new(CacheRangeNotSatisfiable { total_size }))?
                .min(total_size - 1)
        };
        (start, end)
    };
    if start >= total_size || start > end {
        return Err(anyhow::Error::new(CacheRangeNotSatisfiable { total_size }));
    }
    Ok(Some((start, end)))
}

async fn download_range(
    service: &CacheService,
    id: &str,
    namespace: &str,
    range_header: Option<&str>,
) -> Result<DownloadedCacheBlob> {
    validate_cache_id(id)?;
    let entry = service
        .read_entry(id, Some(namespace))
        .ok_or_else(|| anyhow::Error::new(CacheBlobNotFound))?;
    if !service.mark_entry_accessed(id, Some(namespace))? {
        return Err(anyhow::Error::new(CacheBlobNotFound));
    }
    let expected_size = entry["size"].as_u64().context("cache entry has no size")?;
    if expected_size > MAX_BODY {
        anyhow::bail!("cache entry size exceeds {MAX_BODY} bytes");
    }
    let blob_path = service.blob_path_for(&entry, Some(namespace));
    let mut file = match tokio::fs::File::open(&blob_path).await {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow::Error::new(CacheBlobNotFound));
        }
        Err(error) => return Err(error).context("open cached blob"),
    };
    let metadata = file.metadata().await.context("stat cached blob")?;
    if !metadata.is_file() {
        anyhow::bail!("cached blob is not a regular file");
    }
    let total_size = metadata.len();
    if total_size > MAX_BODY {
        anyhow::bail!("actual cached blob size exceeds {MAX_BODY} bytes");
    }
    if total_size != expected_size {
        anyhow::bail!(
            "cached blob size mismatch: entry records {expected_size}, file has {total_size}"
        );
    }
    let range = parse_download_range(range_header, total_size)?;
    let body_size = if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start)).await?;
        end - start + 1
    } else {
        total_size
    };
    let (sender, body) = Channel::<Bytes, io::Error>::new(DOWNLOAD_BUFFERED_CHUNKS);
    tokio::spawn(stream_download(file, sender, body_size, range.is_none()));
    Ok(DownloadedCacheBlob {
        body: body.boxed_unsync(),
        total_size,
        range,
    })
}

async fn download_range_chain(
    service: &CacheService,
    id: &str,
    namespaces: &[&str],
    range_header: Option<&str>,
) -> Result<DownloadedCacheBlob> {
    for namespace in namespaces {
        if service.read_entry(id, Some(namespace)).is_none() {
            continue;
        }
        match download_range(service, id, namespace, range_header).await {
            Ok(downloaded) => return Ok(downloaded),
            Err(_) if service.read_entry(id, Some(namespace)).is_none() => continue,
            Err(error) => return Err(error),
        }
    }
    Err(anyhow::Error::new(CacheBlobNotFound))
}

async fn download_size_chain(service: &CacheService, id: &str, namespaces: &[&str]) -> Result<u64> {
    for namespace in namespaces {
        let Some(entry) = service.read_entry(id, Some(namespace)) else {
            continue;
        };
        if !service.mark_entry_accessed(id, Some(namespace))? {
            continue;
        }
        validate_cache_id(id)?;
        let expected_size = entry["size"].as_u64().context("cache entry has no size")?;
        let blob_path = service.blob_path_for(&entry, Some(namespace));
        let metadata = match tokio::fs::symlink_metadata(blob_path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(anyhow::Error::new(CacheBlobNotFound));
            }
            Err(error) => return Err(error).context("stat cached blob for Azure HEAD"),
        };
        if !metadata.is_file() || metadata.len() != expected_size || expected_size > MAX_BODY {
            anyhow::bail!("cached blob metadata does not match its entry");
        }
        return Ok(expected_size);
    }
    Err(anyhow::Error::new(CacheBlobNotFound))
}

fn download_response(downloaded: DownloadedCacheBlob) -> Response<ResponseBody> {
    let body_size = downloaded
        .range
        .map_or(downloaded.total_size, |(start, end)| end - start + 1);
    let status = if downloaded.range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "application/octet-stream")
        .header(CONTENT_LENGTH, body_size)
        .header("accept-ranges", "bytes")
        .header("x-ms-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-ms-version", "2021-12-02")
        .header("etag", "\"velnor-cache\"")
        .header("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT")
        .header("x-ms-blob-type", "BlockBlob");
    if let Some((start, end)) = downloaded.range {
        response = response.header(
            CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", downloaded.total_size),
        );
    }
    // Proof: `body()` fails only on an invalid status/header; the status is
    // a const and the header values are static strings, a UUID, or u64
    // digits, which are always valid.
    #[allow(clippy::unwrap_used, reason = "response parts are static and valid")]
    response.body(downloaded.body).unwrap()
}

async fn stream_download(
    mut file: tokio::fs::File,
    mut sender: http_body_util::channel::Sender<Bytes, io::Error>,
    expected_size: u64,
    verify_eof: bool,
) {
    let mut buffer = vec![0u8; TRANSFER_CHUNK_BYTES];
    let mut sent = 0u64;

    loop {
        if sent == expected_size {
            if !verify_eof {
                return;
            }
            let mut extra = [0u8; 1];
            match file.read(&mut extra).await {
                Ok(0) => return,
                Ok(_) => {
                    sender.abort(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("cached blob grew beyond its {expected_size}-byte entry"),
                    ));
                    return;
                }
                Err(error) => {
                    sender.abort(error);
                    return;
                }
            }
        }
        let remaining = expected_size - sent;
        let read_limit = buffer.len().min(remaining as usize);
        let read = match file.read(&mut buffer[..read_limit]).await {
            Ok(0) if sent == expected_size => return,
            Ok(0) => {
                sender.abort(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("cached blob ended after {sent} of {expected_size} bytes"),
                ));
                return;
            }
            Ok(read) => read,
            Err(error) => {
                sender.abort(error);
                return;
            }
        };
        let read = match u64::try_from(read) {
            Ok(read) => read,
            Err(error) => {
                sender.abort(io::Error::other(error));
                return;
            }
        };
        let Some(next_sent) = sent.checked_add(read) else {
            sender.abort(io::Error::other("cached blob byte count overflow"));
            return;
        };
        let read = match usize::try_from(read) {
            Ok(read) => read,
            Err(error) => {
                sender.abort(io::Error::other(error));
                return;
            }
        };
        if sender
            .send_data(Bytes::copy_from_slice(&buffer[..read]))
            .await
            .is_err()
        {
            return;
        }
        sent = next_sent;
    }
}

/// Operator enablement contract: the cache URL must be present and non-empty.
/// Job-scoped `ACTIONS_RUNTIME_TOKEN` credentials authenticate requests; the
/// operator environment never supplies a shared job credential.
#[must_use]
pub(crate) fn enabled_from_env() -> Option<String> {
    let url = std::env::var("VELNOR_ACTIONS_CACHE_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

const ACTIONS_CACHE_URL_ENV: &str = "VELNOR_ACTIONS_CACHE_URL";

fn configured_public_base() -> Result<String> {
    let raw = std::env::var(ACTIONS_CACHE_URL_ENV)
        .context("read VELNOR_ACTIONS_CACHE_URL for GHA cache public base")?;
    normalize_public_base(&raw)
}

pub(crate) fn normalize_public_base(raw: &str) -> Result<String> {
    if raw.is_empty() {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must not be empty");
    }
    if raw.trim() != raw {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must not have surrounding whitespace");
    }

    let authority = raw
        .split_once("://")
        .map(|(_, authority)| authority)
        .context("VELNOR_ACTIONS_CACHE_URL must include an authority")?;
    if authority.starts_with('/') {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must include a non-empty authority");
    }

    let url = url::Url::parse(raw).context("parse VELNOR_ACTIONS_CACHE_URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must use http or https");
    }
    if url.host_str().is_none() {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must not include credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("VELNOR_ACTIONS_CACHE_URL must not include a query or fragment");
    }

    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Default listen address. Operators override with `VELNOR_ACTIONS_CACHE_BIND`
/// (e.g. `0.0.0.0:17933`) so job containers reach the service through their
/// docker bridge gateway (`host.docker.internal`, mapped by the job's
/// `--add-host`).
pub(crate) const DEFAULT_CACHE_BIND: &str = "127.0.0.1:17933";

/// Bind the configured address, spawn the accept loop, return the bound addr.
pub(crate) async fn bind_configured(service: CacheService) -> Result<SocketAddr> {
    let public_base = configured_public_base()?;
    let raw = std::env::var("VELNOR_ACTIONS_CACHE_BIND")
        .unwrap_or_else(|_| DEFAULT_CACHE_BIND.to_owned());
    let addr: SocketAddr = raw.parse().context("parse VELNOR_ACTIONS_CACHE_BIND")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(error) = serve_with_public_base(listener, service, public_base).await {
            eprintln!("Warning: gha cache service stopped: {error:#}");
        }
    });
    Ok(bound)
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
    use futures_util::stream;
    use http_body_util::StreamBody;
    use hyper::body::Frame;
    use std::path::Path;

    fn test_repository_key(label: &str) -> String {
        CacheIdentity::test_identity(label, "refs/heads/main", None, TrustClass::Trusted)
            .expect("valid test repository label")
            .repository_key
    }

    fn test_service(dir: &Path) -> CacheService {
        let mut service = CacheService::open(dir.to_path_buf()).expect("open");
        service.budget_bytes = 1024;
        service
    }

    #[cfg(unix)]
    fn create_test_preserved_cache_gc_quarantine(
        cache_root: &Path,
        parent_path: &Path,
        label: &str,
    ) -> (PathBuf, PathBuf) {
        let root = open_cache_directory(cache_root, "test cache root").unwrap();
        let root_identity = crate::leftover_disk::filesystem_object_identity(&root).unwrap();
        let parent = open_cache_directory(parent_path, "test quarantine parent").unwrap();
        let source_name = std::ffi::OsString::from(format!("{label}-expected"));
        let source_path = parent_path.join(&source_name);
        std::fs::write(&source_path, b"expected original").unwrap();
        let source = open_cache_file_child(&parent, &source_name, &source_path, "test source")
            .unwrap()
            .unwrap();
        let expected_identity =
            verify_cache_descendant_mount(&root_identity, &source, "test source").unwrap();
        let (quarantine, quarantine_name, _) =
            create_cache_gc_quarantine(&parent, &root_identity, "test quarantine").unwrap();
        write_cache_gc_quarantine_identity(
            &quarantine,
            &root_identity,
            &expected_identity,
            "test quarantine",
        )
        .unwrap();
        rustix::fs::renameat_with(
            &parent,
            &source_name,
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .unwrap();
        rustix::fs::renameat_with(
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            &quarantine,
            std::ffi::OsStr::new("retained-original"),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .unwrap();
        let mut replacement: std::fs::File = rustix::fs::openat(
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap()
        .into();
        replacement.write_all(b"frozen replacement bytes").unwrap();
        replacement.sync_all().unwrap();
        mark_cache_gc_quarantine_preserved(&quarantine, &root_identity, "test quarantine").unwrap();
        quarantine.sync_all().unwrap();
        parent.sync_all().unwrap();

        let quarantine_path = parent_path.join(quarantine_name);
        let replacement_path = quarantine_path.join(CACHE_GC_QUARANTINE_ENTRY);
        (quarantine_path, replacement_path)
    }

    #[cfg(unix)]
    fn create_test_malformed_cache_gc_quarantine(
        cache_root: &Path,
        parent_path: &Path,
    ) -> (PathBuf, PathBuf) {
        let root = open_cache_directory(cache_root, "test cache root").unwrap();
        let root_identity = crate::leftover_disk::filesystem_object_identity(&root).unwrap();
        let parent = open_cache_directory(parent_path, "test quarantine parent").unwrap();
        let (quarantine, quarantine_name, _) =
            create_cache_gc_quarantine(&parent, &root_identity, "test malformed quarantine")
                .unwrap();
        let mut identity: std::fs::File = rustix::fs::openat(
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_IDENTITY),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap()
        .into();
        identity.write_all(b"{ malformed identity").unwrap();
        identity.sync_all().unwrap();
        let mut entry: std::fs::File = rustix::fs::openat(
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap()
        .into();
        entry.write_all(b"unproven replacement bytes").unwrap();
        entry.sync_all().unwrap();
        quarantine.sync_all().unwrap();
        parent.sync_all().unwrap();

        let quarantine_path = parent_path.join(quarantine_name);
        let entry_path = quarantine_path.join(CACHE_GC_QUARANTINE_ENTRY);
        (quarantine_path, entry_path)
    }

    fn v2_upload_nonce(reservation: &Value) -> String {
        url::Url::parse(reservation["signedUploadUrl"].as_str().expect("upload URL"))
            .expect("valid upload URL")
            .query_pairs()
            .find_map(|(name, value)| (name == "sig").then(|| value.into_owned()))
            .expect("upload URL claim nonce")
    }

    fn write_assembly_scratch_claim(
        service: &CacheService,
        namespace: &str,
        id: &str,
        attempt: &str,
        scratch_bytes: u64,
    ) -> PathBuf {
        let path = assembly_scratch_lease_path(&service.root, Some(namespace), attempt);
        std::fs::create_dir_all(path.parent().expect("scratch lease parent")).unwrap();
        std::fs::write(
            &path,
            json!({
                "namespace": namespace,
                "id": id,
                "attempt": attempt,
                "scratchBytes": scratch_bytes,
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    fn write_blob(service: &CacheService, key: &str, version: &str, contents: &[u8]) {
        let hash = entry_hash(key, version);
        let blobs = service.root.join("blobs");
        std::fs::create_dir_all(&blobs).expect("create blobs");
        std::fs::write(blobs.join(hash), contents).expect("write blob");
    }

    fn commit_blob(service: &CacheService, key: &str, version: &str, contents: &[u8]) {
        write_blob(service, key, version, contents);
        commit_entry(
            service,
            key,
            version,
            u64::try_from(contents.len()).expect("blob length fits u64"),
            None,
        )
        .expect("commit blob");
    }

    fn commit_blob_at(
        service: &CacheService,
        key: &str,
        version: &str,
        contents: &[u8],
        created_ms: u64,
    ) {
        write_blob(service, key, version, contents);
        let hash = entry_hash(key, version);
        let entries = service.tenant_root(None).join("entries");
        std::fs::create_dir_all(&entries).expect("create entries");
        std::fs::write(
            entries.join(format!("{hash}.json")),
            json!({
                "key": key,
                "version": version,
                "blob": hash,
                "size": contents.len(),
                "contentSha256": hex(&Sha256::digest(contents)),
                "created_ms": created_ms,
            })
            .to_string(),
        )
        .expect("write entry");
    }

    #[test]
    fn entry_hash_is_deterministic_and_version_sensitive() {
        assert_eq!(entry_hash("a", "v"), entry_hash("a", "v"));
        assert_ne!(entry_hash("a", "v"), entry_hash("a", "v2"));
        assert_ne!(entry_hash("ab", "v"), entry_hash("a", "bv"));
        assert_ne!(
            entry_hash("a\0b", "c"),
            entry_hash("a", "b\0c"),
            "length framing keeps embedded delimiter bytes from aliasing"
        );
    }

    #[test]
    fn read_capabilities_bind_resource_method_and_expiry() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("signed-read", "v1");
        let get_only = service.create_read_capability("tenant-a", &id, 1).unwrap();
        assert_eq!(
            service.verify_read_capability(&get_only, &id, &hyper::Method::GET),
            Some("tenant-a".to_owned())
        );
        assert!(service
            .verify_read_capability(&get_only, &id, &hyper::Method::HEAD)
            .is_none());
        assert!(service
            .verify_read_capability(
                &get_only,
                &entry_hash("other-read", "v1"),
                &hyper::Method::GET
            )
            .is_none());

        let expired = service
            .create_read_capability_until(
                "tenant-a",
                &id,
                3,
                now_unix_millis().unwrap().saturating_sub(1),
            )
            .unwrap();
        assert!(service
            .verify_read_capability(&expired, &id, &hyper::Method::GET)
            .is_none());

        let reopened = CacheService::open(service.root.clone()).unwrap();
        assert_eq!(
            reopened.verify_read_capability(&get_only, &id, &hyper::Method::GET),
            Some("tenant-a".to_owned()),
            "read-capability secret survives process restart"
        );
    }

    #[test]
    fn azure_block_limits_allow_replacement_but_reject_the_first_excess_id() {
        assert!(v2_block_slot_available(
            MAX_V2_UNCOMMITTED_BLOCKS - 1,
            0,
            false
        ));
        assert!(!v2_block_slot_available(
            MAX_V2_UNCOMMITTED_BLOCKS,
            0,
            false
        ));
        assert!(!v2_block_slot_available(
            MAX_V2_UNCOMMITTED_BLOCKS - 1,
            1,
            false
        ));
        assert!(v2_block_slot_available(MAX_V2_UNCOMMITTED_BLOCKS, 0, true));
        assert_eq!(MAX_V2_COMMITTED_BLOCKS, 50_000);
    }

    #[tokio::test]
    async fn staged_parts_and_active_upload_bytes_share_a_per_namespace_budget() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.ensure_tenant("tenant-a").unwrap();
        let tenant_uploads = service.tenant_root(Some("tenant-a")).join("uploads");
        let first = tenant_uploads.join(entry_hash("staged-a", "v1"));
        let second = tenant_uploads.join(entry_hash("staged-b", "v1"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(
            first.join("0123456789abcdef0123456789abcdef.part"),
            vec![b'a'; 600],
        )
        .unwrap();
        std::fs::write(
            second.join("v2-fedcba9876543210fedcba9876543210.part"),
            vec![b'b'; 300],
        )
        .unwrap();
        assert_eq!(
            service.staged_upload_usage(Some("tenant-a"), None).unwrap(),
            (900, 2)
        );

        let first_id = entry_hash("admission-a", "v1");
        let first_attempt = "00112233445566778899aabbccddeeff";
        let record_bytes = json!({
            "namespace": "tenant-a",
            "id": first_id.as_str(),
            "attempt": first_attempt,
            "reservedBytes": 124,
            "scratchBytes": 0,
            "reservedFiles": 1,
        })
        .to_string()
        .len() as u64;
        service.budget_bytes = 900 + 124 + record_bytes;
        let too_large = service
            .try_admit_upload(
                Some("tenant-a"),
                first_id.as_str(),
                first_attempt,
                service.budget_bytes,
                Some(125),
                1,
            )
            .await;
        assert!(matches!(too_large, Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()));
        let admitted = service
            .try_admit_upload(
                Some("tenant-a"),
                first_id.as_str(),
                first_attempt,
                service.budget_bytes,
                Some(124),
                1,
            )
            .await
            .unwrap();
        let overbooked = service
            .try_admit_upload(
                Some("tenant-a"),
                &entry_hash("admission-b", "v1"),
                "112233445566778899aabbccddeeff00",
                service.budget_bytes,
                Some(1),
                1,
            )
            .await;
        assert!(
            matches!(overbooked, Err(error) if error.downcast_ref::<CacheLockBusy>().is_some())
        );
        assert!(service
            .try_admit_upload(
                Some("tenant-b"),
                &entry_hash("admission-c", "v1"),
                "2233445566778899aabbccddeeff0011",
                service.budget_bytes,
                Some(500),
                1,
            )
            .await
            .is_ok());
        drop(admitted);
    }

    #[tokio::test]
    async fn assembly_scratch_budget_is_shared_across_namespaces() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 1024;

        let first = service
            .try_admit_upload_with_scratch(
                Some("tenant-a"),
                &entry_hash("scratch-a", "v1"),
                "00112233445566778899aabbccddeeff",
                service.budget_bytes,
                Some(0),
                600,
                2,
            )
            .await
            .unwrap();
        let second_args = (
            Some("tenant-b"),
            entry_hash("scratch-b", "v1"),
            "112233445566778899aabbccddeeff00",
        );
        let blocked = service
            .try_admit_upload_with_scratch(
                second_args.0,
                &second_args.1,
                second_args.2,
                service.budget_bytes,
                Some(0),
                425,
                2,
            )
            .await;
        assert!(matches!(
            blocked,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()
        ));

        drop(first);
        let admitted = service
            .try_admit_upload_with_scratch(
                second_args.0,
                &second_args.1,
                second_args.2,
                service.budget_bytes,
                Some(0),
                425,
                2,
            )
            .await
            .unwrap();
        drop(admitted);
    }

    #[test]
    fn assembly_scratch_claim_syncs_its_parent_before_claim_publication() {
        let dir = tempfile_dir();
        let cache_root = dir.path();
        let lease_dir = cache_root.join(CACHE_ASSEMBLY_SCRATCH_LEASE_DIR);
        let attempt = "00112233445566778899aabbccddeeff";
        let id = entry_hash("create-sync", "v1");
        let mut parent_synced = false;
        let lease = reserve_global_assembly_scratch_with_sync(
            cache_root,
            1024,
            Some("tenant-create-sync"),
            &id,
            attempt,
            600,
            |parent| {
                assert_eq!(parent, cache_root);
                assert!(lease_dir.is_dir(), "lease directory exists before sync");
                assert!(
                    std::fs::read_dir(&lease_dir)?.next().is_none(),
                    "claim files are published only after parent sync"
                );
                parent_synced = true;
                sync_directory(parent)
            },
        )
        .unwrap()
        .expect("positive scratch reservation creates a lease");
        assert!(parent_synced);
        let claim = assembly_scratch_lease_path(cache_root, Some("tenant-create-sync"), attempt);
        drop(lease);
        assert!(claim.exists(), "Drop preserves the scratch budget claim");
        let _budget_lock = lock_cache_assembly_scratch_blocking(cache_root).unwrap();
        let mut synced = Vec::new();
        assert!(remove_global_assembly_scratch_lease_locked_with_sync(
            cache_root,
            Some("tenant-create-sync"),
            &id,
            attempt,
            |path| {
                if path == cache_root {
                    assert!(claim.exists(), "root sync persists missing tenants");
                } else if path == lease_dir.as_path() {
                    assert!(!claim.exists(), "lease unlink precedes lease-dir sync");
                }
                synced.push(path.to_path_buf());
                sync_directory(path)
            },
        )
        .unwrap());
        assert_eq!(synced, vec![cache_root.to_path_buf(), lease_dir.clone()]);
        assert!(!claim.exists());

        let failing_root = dir.path().join("sync-failure");
        std::fs::create_dir(&failing_root).unwrap();
        let failing_lease_dir = failing_root.join(CACHE_ASSEMBLY_SCRATCH_LEASE_DIR);
        let result = reserve_global_assembly_scratch_with_sync(
            &failing_root,
            1024,
            Some("tenant-create-sync-failure"),
            &entry_hash("create-sync-failure", "v1"),
            "112233445566778899aabbccddeeff00",
            600,
            |_| anyhow::bail!("injected cache-root fsync failure"),
        );
        assert!(result.is_err());
        assert!(failing_lease_dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&failing_lease_dir).unwrap().count(),
            0,
            "failed parent sync prevents lease publication"
        );
    }

    #[test]
    fn assembly_scratch_lease_removal_syncs_artifacts_first_and_keeps_claim_on_failure() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-release-sync";
        service.ensure_tenant(namespace).unwrap();
        std::fs::create_dir(service.tenant_root(Some(namespace)).join("uploads")).unwrap();
        let _budget_lock = lock_cache_assembly_scratch_blocking(&service.root).unwrap();
        let id = entry_hash("release-sync", "v1");
        let attempt = "2233445566778899aabbccddeeff0011";
        let claim = write_assembly_scratch_claim(&service, namespace, &id, attempt, 600);
        let tenant = service.tenant_root(Some(namespace));
        let blobs = tenant.join("blobs");
        let uploads = tenant.join("uploads");
        let lease_dir = claim.parent().unwrap().to_path_buf();

        let mut synced = Vec::new();
        let released = remove_global_assembly_scratch_lease_locked_with_sync(
            &service.root,
            Some(namespace),
            &id,
            attempt,
            |path| {
                if path == blobs.as_path() || path == uploads.as_path() {
                    assert!(claim.exists(), "artifact parents sync before unlink");
                } else if path == lease_dir.as_path() {
                    assert!(!claim.exists(), "lease directory sync follows unlink");
                }
                synced.push(path.to_path_buf());
                sync_directory(path)
            },
        )
        .unwrap();
        assert!(released);
        assert_eq!(
            synced,
            vec![blobs.clone(), uploads.clone(), lease_dir.clone()]
        );
        assert!(!claim.exists());

        let failed_attempt = "33445566778899aabbccddeeff001122";
        let failed_claim =
            write_assembly_scratch_claim(&service, namespace, &id, failed_attempt, 600);
        let result = remove_global_assembly_scratch_lease_locked_with_sync(
            &service.root,
            Some(namespace),
            &id,
            failed_attempt,
            |path| {
                if path == blobs.as_path() {
                    anyhow::bail!("injected artifact-parent fsync failure");
                }
                sync_directory(path)
            },
        );
        assert!(result.is_err());
        assert!(failed_claim.exists(), "sync failure must retain the claim");
    }

    #[test]
    fn inactive_v2_marker_reconciliation_keeps_scratch_claim_on_parent_sync_failure() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-reconcile-sync";
        service.ensure_tenant(namespace).unwrap();
        let key = "reconcile-sync";
        let version = "v1";
        let id = entry_hash(key, version);
        let attempt = "445566778899aabbccddeeff00112233";
        let temp = service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(format!(".{id}.{attempt}.tmp"));
        std::fs::write(&temp, vec![b't'; 600]).unwrap();
        let claim = write_assembly_scratch_claim(&service, namespace, &id, attempt, 600);
        let reservation = V2Reservation {
            key: key.to_owned(),
            version: version.to_owned(),
            upload_nonce: "5566778899aabbccddeeff0011223344".to_owned(),
            block_id_bytes: None,
            upload_attempt: Some(attempt.to_owned()),
            upload_blocks: Vec::new(),
            active_uploads: Vec::new(),
            assembly_scratch_attempt: None,
            assembly_scratch_bytes: 0,
            updated_ms: now_unix_millis().unwrap(),
        };
        reservation.persist(&service, &id, namespace).unwrap();
        let blobs = service.tenant_root(Some(namespace)).join("blobs");

        let result =
            reconcile_inactive_upload_markers_with_sync(&service, &id, Some(namespace), |path| {
                if path == blobs.as_path() {
                    assert!(claim.exists(), "claim remains during parent fsync");
                    anyhow::bail!("injected blob-parent fsync failure");
                }
                sync_directory(path)
            });
        assert!(result.is_err());
        assert!(!temp.exists(), "reconciliation removed the inactive temp");
        assert!(
            claim.exists(),
            "failed parent sync preserves scratch charge"
        );
        assert_eq!(
            read_v2_reservation(&service, &id, namespace)
                .unwrap()
                .unwrap()
                .upload_attempt
                .as_deref(),
            Some(attempt),
            "failed reconciliation leaves its reservation marker retryable"
        );

        assert!(!reconcile_inactive_upload_markers(&service, &id, Some(namespace)).unwrap());
        assert!(
            !claim.exists(),
            "retry syncs parents before releasing claim"
        );
        assert_eq!(
            read_v2_reservation(&service, &id, namespace)
                .unwrap()
                .unwrap()
                .upload_attempt,
            None
        );
    }

    #[tokio::test]
    async fn partial_assembly_scratch_lease_publication_does_not_poison_restart() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 1024;
        let namespace = "tenant-atomic-scratch";
        service.ensure_tenant(namespace).unwrap();
        let id = entry_hash("atomic-scratch-publication", "v1");
        let attempt = "00112233445566778899aabbccddeeff";
        let bytes = vec![b'p'; 600];
        let mut lease = reserve_global_assembly_scratch(
            &service.root,
            1024,
            Some(namespace),
            &id,
            attempt,
            bytes.len() as u64,
        )
        .unwrap()
        .expect("positive scratch reservation creates a lease");
        lease.retain_durable().unwrap();

        let tenant_root = service.tenant_root(Some(namespace));
        std::fs::write(tenant_root.join("blobs").join(&id), &bytes).unwrap();
        let part_attempt = "ffeeddccbbaa99887766554433221100";
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");
        let uploads = tenant_root.join("uploads").join(&id);
        std::fs::create_dir_all(&uploads).unwrap();
        std::fs::write(uploads.join(format!("v2-{part_attempt}.part")), &bytes).unwrap();
        let reservation = V2Reservation {
            key: "atomic-scratch-publication".to_owned(),
            version: "v1".to_owned(),
            upload_nonce: "11223344556677889900aabbccddeeff".to_owned(),
            block_id_bytes: Some(10),
            upload_attempt: None,
            upload_blocks: vec![V2UploadBlock {
                block_id,
                attempt: part_attempt.to_owned(),
                size: bytes.len() as u64,
                sha256: hex(&Sha256::digest(&bytes)),
            }],
            active_uploads: Vec::new(),
            assembly_scratch_attempt: Some(attempt.to_owned()),
            assembly_scratch_bytes: bytes.len() as u64,
            updated_ms: now_unix_millis().unwrap(),
        };
        atomically_create_json(
            &service.reservation_path(&id, Some(namespace)),
            &reservation.as_json(),
        )
        .unwrap();

        // Crash cut before atomic rename: only a partial temp exists beside a
        // complete prior lease. Startup must discard the temp and retain the
        // valid root-wide charge while canonical and source bytes coexist.
        let lease_path = assembly_scratch_lease_path(&service.root, Some(namespace), attempt);
        let lease_name = lease_path.file_name().unwrap().to_str().unwrap();
        let partial_temp = lease_path.parent().unwrap().join(format!(
            ".{lease_name}.0123456789abcdef0123456789abcdef.tmp"
        ));
        let mut partial_file = std::fs::File::create(&partial_temp).unwrap();
        partial_file.write_all(b"{\"namespace\":").unwrap();
        partial_file.sync_all().unwrap();
        drop(partial_file);
        drop(service);

        let mut reopened = CacheService::open(dir.path().to_path_buf()).unwrap();
        reopened.budget_bytes = 1024;
        assert!(
            !partial_temp.exists(),
            "startup removes the incomplete temp"
        );
        assert!(lease_path.exists(), "the complete durable lease survives");
        let record: Value = serde_json::from_slice(&std::fs::read(&lease_path).unwrap()).unwrap();
        assert_eq!(record["scratchBytes"], bytes.len() as u64);
        assert_eq!(
            read_v2_reservation(&reopened, &id, namespace)
                .unwrap()
                .unwrap()
                .assembly_scratch_attempt
                .as_deref(),
            Some(attempt)
        );

        let blocked = reopened
            .try_admit_upload_with_scratch(
                Some("tenant-after-restart"),
                &entry_hash("after-atomic-scratch-restart", "v1"),
                "2233445566778899aabbccddeeff0011",
                1024,
                Some(0),
                425,
                1,
            )
            .await;
        assert!(matches!(
            blocked,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()
        ));
    }

    #[tokio::test]
    async fn crash_orphan_assembly_temp_stays_charged_until_cleanup_removes_it() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 1024;
        let namespace = "tenant-crash";
        service.ensure_tenant(namespace).unwrap();
        let id = entry_hash("crashed-assembly", "v1");
        let attempt = "00112233445566778899aabbccddeeff";
        let temp = service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(format!(".{id}.{attempt}.tmp"));
        std::fs::write(&temp, vec![b't'; 600]).unwrap();
        let claim = write_assembly_scratch_claim(&service, namespace, &id, attempt, 600);

        let blocked = service
            .try_admit_upload_with_scratch(
                Some("tenant-next"),
                &entry_hash("next-after-crash", "v1"),
                "112233445566778899aabbccddeeff00",
                1024,
                Some(0),
                500,
                1,
            )
            .await;
        assert!(matches!(
            blocked,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()
        ));

        service.cleanup_stale_upload_temps(true).unwrap();
        assert!(!temp.exists(), "recovery removes the orphan temp first");
        assert!(!claim.exists(), "recovery releases its lease after removal");
        let admitted = service
            .try_admit_upload_with_scratch(
                Some("tenant-next"),
                &entry_hash("next-after-crash", "v1"),
                "2233445566778899aabbccddeeff0011",
                1024,
                Some(0),
                500,
                1,
            )
            .await
            .expect("cleanup makes the bounded headroom reusable");
        drop(admitted);
    }

    #[tokio::test]
    async fn unsafe_crash_orphan_fails_closed_without_releasing_its_scratch_claim() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 1024;
        let namespace = "tenant-unsafe-crash";
        service.ensure_tenant(namespace).unwrap();
        let id = entry_hash("unsafe-crashed-assembly", "v1");
        let attempt = "33445566778899aabbccddeeff001122";
        let temp = service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(format!(".{id}.{attempt}.tmp"));
        std::fs::create_dir(&temp).unwrap();
        let claim = write_assembly_scratch_claim(&service, namespace, &id, attempt, 600);

        assert!(service.cleanup_stale_upload_temps(true).is_err());
        assert!(claim.exists(), "failed recovery preserves the budget claim");
        let blocked = service
            .try_admit_upload_with_scratch(
                Some("tenant-next"),
                &entry_hash("next-after-unsafe-crash", "v1"),
                "445566778899aabbccddeeff00112233",
                1024,
                Some(0),
                500,
                1,
            )
            .await;
        assert!(
            blocked.is_err(),
            "unsafe artifacts cannot free root headroom"
        );
        assert!(claim.exists());
    }

    #[test]
    fn cache_access_checkpoint_refreshes_stale_timestamp() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-stale-access-checkpoint";
        service.ensure_tenant(namespace).unwrap();
        let key = "stale-access-checkpoint";
        let version = "v1";
        let id = entry_hash(key, version);
        std::fs::write(
            service.tenant_root(Some(namespace)).join("blobs").join(&id),
            b"blob",
        )
        .unwrap();
        commit_entry(&service, key, version, 4, Some(namespace)).unwrap();

        let stale = 10_000_000_000u64;
        let touched_at = stale + CACHE_ACCESS_CHECKPOINT_INTERVAL_NS;
        let path = service.entry_path(&id, Some(namespace));
        let mut entry = service.read_entry(&id, Some(namespace)).unwrap();
        entry["last_accessed_ns"] = json!(stale);
        std::fs::write(&path, entry.to_string()).unwrap();

        assert!(service
            .mark_entry_accessed_at(&id, Some(namespace), touched_at)
            .unwrap());
        let refreshed = service.read_entry(&id, Some(namespace)).unwrap()["last_accessed_ns"]
            .as_u64()
            .unwrap();
        assert_eq!(
            refreshed, touched_at,
            "a checkpoint at the interval boundary must refresh to the hit time"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_access_checkpoint_skips_fresh_manifest_rewrite() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-fresh-access-checkpoint";
        service.ensure_tenant(namespace).unwrap();
        let key = "fresh-access-checkpoint";
        let version = "v1";
        let id = entry_hash(key, version);
        std::fs::write(
            service.tenant_root(Some(namespace)).join("blobs").join(&id),
            b"blob",
        )
        .unwrap();
        commit_entry(&service, key, version, 4, Some(namespace)).unwrap();

        let checkpoint = 10_000_000_000u64;
        let touched_at = checkpoint + CACHE_ACCESS_CHECKPOINT_INTERVAL_NS - 1;
        let path = service.entry_path(&id, Some(namespace));
        let mut entry = service.read_entry(&id, Some(namespace)).unwrap();
        entry["last_accessed_ns"] = json!(checkpoint);
        std::fs::write(&path, entry.to_string()).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();

        assert!(service
            .mark_entry_accessed_at(&id, Some(namespace), touched_at)
            .unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            inode,
            "a fresh checkpoint must not atomically replace the manifest"
        );
        assert_eq!(
            service.read_entry(&id, Some(namespace)).unwrap()["last_accessed_ns"],
            json!(checkpoint)
        );
    }

    #[test]
    fn cache_access_checkpoint_recovers_future_timestamps() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-future-access-checkpoint";
        service.ensure_tenant(namespace).unwrap();
        let key = "future-access-checkpoint";
        let version = "v1";
        let id = entry_hash(key, version);
        std::fs::write(
            service.tenant_root(Some(namespace)).join("blobs").join(&id),
            b"blob",
        )
        .unwrap();
        commit_entry(&service, key, version, 4, Some(namespace)).unwrap();

        let now = 10_000_000_000u64;
        let path = service.entry_path(&id, Some(namespace));
        for future in [now + CACHE_ACCESS_CHECKPOINT_INTERVAL_NS, u64::MAX] {
            let mut entry = service.read_entry(&id, Some(namespace)).unwrap();
            entry["last_accessed_ns"] = json!(future);
            std::fs::write(&path, entry.to_string()).unwrap();

            assert!(service
                .mark_entry_accessed_at(&id, Some(namespace), now)
                .unwrap());
            assert_eq!(
                service.read_entry(&id, Some(namespace)).unwrap()["last_accessed_ns"],
                json!(now),
                "future persisted timestamps must reset to the current clock"
            );
        }
    }

    #[tokio::test]
    async fn eviction_ranks_entries_by_persisted_access_checkpoint() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        let namespace = "tenant-persisted-access-order";
        service.ensure_tenant(namespace).unwrap();
        service.budget_bytes = 2500;

        // Make the older entry sort later by ID so this test proves eviction
        // follows the persisted access checkpoint rather than an ID tie-break.
        let key_a = "persisted-access-a";
        let key_b = "persisted-access-b";
        let id_a = entry_hash(key_a, "v1");
        let id_b = entry_hash(key_b, "v1");
        let (older_key, older_id, newer_key, newer_id) = if id_a > id_b {
            (key_a, id_a, key_b, id_b)
        } else {
            (key_b, id_b, key_a, id_a)
        };

        for (key, id, access_ns) in [(older_key, &older_id, 1u64), (newer_key, &newer_id, 2u64)] {
            std::fs::write(
                service.tenant_root(Some(namespace)).join("blobs").join(id),
                vec![b'x'; 2000],
            )
            .unwrap();
            commit_entry(&service, key, "v1", 2000, Some(namespace)).unwrap();
            let path = service.entry_path(id, Some(namespace));
            let mut entry = service.read_entry(id, Some(namespace)).unwrap();
            entry["last_accessed_ns"] = json!(access_ns);
            std::fs::write(path, entry.to_string()).unwrap();
        }

        service.enforce_budget(Some(namespace)).await.unwrap();

        assert!(!service.entry_path(&older_id, Some(namespace)).exists());
        assert!(!service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(&older_id)
            .exists());
        assert!(service.entry_path(&newer_id, Some(namespace)).exists());
        assert!(service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(&newer_id)
            .exists());
    }

    #[tokio::test]
    async fn admission_evicts_lru_entry_before_rejecting_full_namespace_budget() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 4096;
        service.ensure_tenant("tenant-a").unwrap();
        let tenant = service.tenant_root(Some("tenant-a"));
        let old_key = "full-budget-old";
        let old_version = "v1";
        let old_id = entry_hash(old_key, old_version);
        let old_contents = vec![b'o'; 3500];
        std::fs::write(tenant.join("blobs").join(&old_id), &old_contents).unwrap();
        commit_entry(
            &service,
            old_key,
            old_version,
            old_contents.len() as u64,
            Some("tenant-a"),
        )
        .unwrap();

        let new_id = entry_hash("full-budget-new", "v1");
        let admission = service
            .try_admit_upload(
                Some("tenant-a"),
                &new_id,
                "2233445566778899aabbccddeeff0011",
                service.budget_bytes,
                Some(1000),
                1,
            )
            .await
            .unwrap();

        assert!(!service.entry_path(&old_id, Some("tenant-a")).exists());
        assert!(!tenant.join("blobs").join(old_id).exists());
        drop(admission);
    }

    #[test]
    fn cache_entry_lock_shards_are_globally_bounded() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());

        let logical_id = entry_hash("fixed-shard", "v1");
        let same_entry = cache_entry_lock_path(&service.root, Some("tenant-a"), &logical_id);
        assert_eq!(
            same_entry,
            cache_entry_lock_path(&service.root, Some("tenant-a"), &logical_id)
        );

        for index in 0..8192u64 {
            let namespace = (index % 5 != 0).then(|| format!("namespace-{}", index % 1021));
            let id = hex(&Sha256::digest(index.to_be_bytes()));
            let lock = try_lock_cache_entry_at(&service.root, namespace.as_deref(), &id)
                .expect("open cache entry lock")
                .expect("lock shard is free");
            drop(lock);
        }

        let locks = service.root.join("entry-locks");
        let files: Vec<_> = std::fs::read_dir(&locks)
            .expect("read lock directory")
            .map(|entry| entry.expect("read lock entry").file_name())
            .collect();
        assert!(files.len() <= CACHE_ENTRY_LOCK_SHARDS);
        assert!(files.iter().all(|name| {
            let Some(name) = name.to_str() else {
                return false;
            };
            name.len() == 8 && name.ends_with(".lock")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn tenant_tree_removal_cannot_unlink_or_replace_entry_lock_shard() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let id = entry_hash("stable-lock", "v1");
        let path = service.cache_entry_lock_path(&id, Some("tenant"));
        let lock = try_lock_cache_entry_at(&service.root, Some("tenant"), &id)
            .unwrap()
            .expect("lock shard is free");
        let inode = std::fs::metadata(&path).unwrap().ino();
        drop(lock);

        std::fs::remove_dir_all(service.tenant_root(Some("tenant"))).unwrap();
        assert!(
            path.exists(),
            "tenant cleanup must leave the root lock shard"
        );

        let reopened = CacheService::open(dir.path().to_path_buf()).unwrap();
        let reopened_path = reopened.cache_entry_lock_path(&id, Some("tenant"));
        assert_eq!(path, reopened_path);
        assert_eq!(std::fs::metadata(&reopened_path).unwrap().ino(), inode);
        let lock = try_lock_cache_entry_at(&reopened.root, Some("tenant"), &id)
            .unwrap()
            .expect("reopened shard is lockable");
        drop(lock);
    }

    #[tokio::test]
    async fn entry_lock_wait_deadline_returns_retryable_busy() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("held-lock", "v1");
        let path = service.cache_entry_lock_path(&id, Some("tenant"));
        let holder = open_cache_entry_lock(&path).unwrap();
        rustix::fs::flock(
            &holder,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();

        let waiter = open_cache_entry_lock(&path).unwrap();
        let error =
            lock_file_with_deadline(waiter, std::time::Duration::from_millis(5), "cache entry")
                .await
                .unwrap_err();
        assert!(error.downcast_ref::<CacheLockBusy>().is_some());
        drop(holder);
    }

    #[tokio::test]
    async fn colliding_entry_state_operation_completes_while_v2_body_streams() {
        use futures_util::StreamExt;

        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 1024;
        let ctx = test_ctx(service.clone());
        let id = entry_hash("slow-upload", "v1");
        let claim = reserve_v2(
            post_json(json!({"key": "slow-upload", "version": "v1"})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let nonce = read_v2_reservation(&ctx.service, &id, "tenant")
            .unwrap()
            .unwrap()
            .upload_nonce;
        assert_eq!(claim["ok"], json!(true));

        let other_key = (0..100_000)
            .map(|index| format!("colliding-{index}"))
            .find(|key| {
                cache_entry_lock_path(&ctx.service.root, Some("tenant"), &entry_hash(key, "v1"))
                    == cache_entry_lock_path(&ctx.service.root, Some("tenant"), &id)
            })
            .expect("find a key in the same finite lock shard");
        let other_id = entry_hash(&other_key, "v1");

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let frames = stream::once(async move {
            let _ = started_tx.send(());
            Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"first")))
        })
        .chain(stream::once(async move {
            let _ = release_rx.await;
            Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"part")))
        }));
        let request = Request::builder()
            .method(hyper::Method::PUT)
            .uri(format!(
                "http://cache.test/_results/upload/{id}?claim={nonce}"
            ))
            .body(StreamBody::new(Box::pin(frames)))
            .unwrap();
        let upload_ctx = test_ctx(service.clone());
        let upload_nonce = nonce.clone();
        let upload_id = id.clone();
        let upload_task = tokio::spawn(async move {
            upload(
                request,
                &upload_ctx,
                &upload_id,
                "tenant",
                false,
                Some(&upload_nonce),
            )
            .await
        });
        started_rx.await.unwrap();

        let second_claim = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            reserve_v2(
                post_json(json!({"key": other_key, "version": "v1"})),
                &ctx,
                "tenant",
            ),
        )
        .await
        .expect("colliding reservation must not wait for the body stream")
        .unwrap();
        assert_eq!(second_claim["ok"], json!(true));

        let tenant_root = ctx.service.tenant_root(Some("tenant"));
        let temp_path = std::fs::read_dir(tenant_root.join("blobs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| upload_temp_cache_id(path.file_name().unwrap()) == Some(id.as_str()))
            .expect("active temp upload exists");
        let cleanup = ctx
            .service
            .cleanup_upload_temps_at(&tenant_root, Some("tenant"))
            .unwrap();
        assert!(cleanup.skipped_busy);
        assert!(temp_path.exists(), "sweep must retain the live temp flock");

        release_tx.send(()).unwrap();
        upload_task.await.unwrap().unwrap();
        assert!(tenant_root.join("blobs").join(id).exists());
        assert!(ctx
            .service
            .reservation_path(&other_id, Some("tenant"))
            .exists());
    }

    #[tokio::test]
    async fn upload_temp_sweeps_are_throttled_and_retry_busy_locks() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        *service.upload_temp_cleanup_schedule.lock().unwrap() = None;
        service.ensure_tenant("tenant").unwrap();
        let blobs = service.tenant_root(Some("tenant")).join("blobs");
        let nonce = "0123456789abcdef0123456789abcdef";

        let first_id = entry_hash("first-orphan", "v1");
        let first = blobs.join(format!(".{first_id}.{nonce}.tmp"));
        std::fs::write(&first, b"orphan").unwrap();
        assert_eq!(service.cleanup_stale_upload_temps(false).unwrap(), 1);
        assert!(!first.exists());

        let second_id = entry_hash("second-orphan", "v1");
        let second = blobs.join(format!(".{second_id}.{nonce}.tmp"));
        std::fs::write(&second, b"orphan").unwrap();
        let live_id = entry_hash("busy-upload", "v1");
        let live = blobs.join(format!(".{live_id}.{nonce}.tmp"));
        std::fs::write(&live, b"active").unwrap();
        let live_lock = try_lock_cache_entry_at(&service.root, Some("tenant"), &live_id)
            .unwrap()
            .expect("hold live upload lock");

        assert_eq!(service.cleanup_stale_upload_temps(false).unwrap(), 0);
        assert!(
            second.exists(),
            "scheduled sweep should skip repeated scans"
        );
        assert!(live.exists());

        *service.upload_temp_cleanup_schedule.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert_eq!(service.cleanup_stale_upload_temps(false).unwrap(), 1);
        assert!(!second.exists());
        assert!(live.exists(), "busy lock must preserve its temp");
        let retry_at = service
            .upload_temp_cleanup_schedule
            .lock()
            .unwrap()
            .as_ref()
            .copied()
            .expect("busy sweep schedules retry");
        assert!(
            retry_at
                <= std::time::Instant::now()
                    + std::time::Duration::from_secs(UPLOAD_TEMP_BUSY_RETRY_SECS)
        );

        drop(live_lock);
        assert_eq!(service.cleanup_stale_upload_temps(false).unwrap(), 0);
        assert!(live.exists(), "busy lock retry should respect its deadline");
        *service.upload_temp_cleanup_schedule.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert_eq!(service.cleanup_stale_upload_temps(false).unwrap(), 1);
        assert!(!live.exists());

        let first_budget_id = entry_hash("budget-path-sweep-first", "v1");
        let first_budget_blob = blobs.join(first_budget_id);
        std::fs::write(&first_budget_blob, b"orphan").unwrap();
        *service.upload_temp_cleanup_schedule.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        service.enforce_budget(Some("tenant")).await.unwrap();
        assert!(!first_budget_blob.exists());

        let second_budget_id = entry_hash("budget-path-sweep-second", "v1");
        let second_budget_blob = blobs.join(second_budget_id);
        std::fs::write(&second_budget_blob, b"orphan").unwrap();
        service.enforce_budget(Some("tenant")).await.unwrap();
        assert!(
            second_budget_blob.exists(),
            "budget path must respect the root-wide sweep throttle"
        );
        *service.upload_temp_cleanup_schedule.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        service.enforce_budget(Some("tenant")).await.unwrap();
        assert!(!second_budget_blob.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn generic_cache_gc_tenant_guard_pins_and_excludes_namespace() {
        use std::os::unix::fs::symlink;

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-generic-gc-guard";
        service.ensure_tenant(namespace).unwrap();
        let tenant_path = service.tenant_root(Some(namespace));
        let pinned_tenant = open_cache_directory(&tenant_path, "test GHA tenant").unwrap();

        let mut checked_order = false;
        let locks = try_lock_tenant_gc_locks_with(&service.root, namespace, || {
            assert!(
                try_lock_cache_activity_shared_at(&service.root, namespace)?.is_none(),
                "GC must own the exclusive activity lock before the namespace lock"
            );
            let namespace_observer = try_lock_cache_namespace_at(&service.root, Some(namespace))?;
            assert!(
                namespace_observer.is_some(),
                "activity lock must be acquired before the namespace lock"
            );
            drop(namespace_observer);
            checked_order = true;
            Ok(())
        })
        .unwrap()
        .expect("both GHA tenant locks are free");
        assert!(checked_order);
        drop(locks);

        let active_route = try_lock_cache_activity_shared_at(&service.root, namespace)
            .unwrap()
            .expect("acquire simulated bearer-route activity lock");
        assert!(
            try_lock_tenant_for_cache_gc(&service.root, namespace, &tenant_path, &pinned_tenant,)
                .unwrap()
                .is_none(),
            "GC must skip a tenant with an active fallback-bearer request"
        );
        drop(active_route);

        let active_namespace = try_lock_cache_namespace_at(&service.root, Some(namespace))
            .unwrap()
            .expect("acquire simulated namespace transition lock");
        assert!(
            try_lock_tenant_for_cache_gc(&service.root, namespace, &tenant_path, &pinned_tenant,)
                .unwrap()
                .is_none(),
            "GC must skip a tenant with an active namespace transition"
        );
        assert!(
            try_lock_cache_activity_shared_at(&service.root, namespace)
                .unwrap()
                .is_some(),
            "failed namespace acquisition must release the activity lock"
        );
        drop(active_namespace);

        let guard =
            try_lock_tenant_for_cache_gc(&service.root, namespace, &tenant_path, &pinned_tenant)
                .unwrap()
                .expect("acquire tenant GC guard");
        assert!(
            try_lock_cache_activity_shared_at(&service.root, namespace)
                .unwrap()
                .is_none(),
            "guard retains the exclusive activity lock through deletion"
        );
        assert!(
            try_lock_cache_namespace_at(&service.root, Some(namespace))
                .unwrap()
                .is_none(),
            "guard retains the exclusive namespace lock through deletion"
        );
        drop(guard);

        let moved_tenant = dir.path().join("saved-tenant");
        std::fs::rename(&tenant_path, &moved_tenant).unwrap();
        let outside = dir.path().join("outside-tenant");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, &tenant_path).unwrap();
        assert!(
            try_lock_tenant_for_cache_gc(&service.root, namespace, &tenant_path, &pinned_tenant,)
                .is_err(),
            "GC must reject a tenant path replaced by a symlink"
        );
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cache_gc_rejects_descendants_on_another_device_or_mount() {
        use crate::leftover_disk::{FilesystemDirectoryIdentity, FilesystemMountIdentity};

        let root = FilesystemDirectoryIdentity {
            device: 12,
            inode: 34,
            mount: FilesystemMountIdentity::LinuxMountId(56),
        };
        let other_device = FilesystemDirectoryIdentity {
            device: 13,
            ..root.clone()
        };
        let other_mount = FilesystemDirectoryIdentity {
            mount: FilesystemMountIdentity::LinuxMountId(57),
            ..root.clone()
        };
        for descendant in ["cache admission namespace directory", "GHA tenant blobs"] {
            assert!(verify_cache_mount_identity(&root, &root, descendant).is_ok());
            assert!(verify_cache_mount_identity(&root, &other_device, descendant).is_err());
            assert!(verify_cache_mount_identity(&root, &other_mount, descendant).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cache_gc_quarantine_journal_binds_device_inode_mount_and_type() {
        use crate::leftover_disk::{FilesystemDirectoryIdentity, FilesystemMountIdentity};

        let identity = FilesystemDirectoryIdentity {
            device: 12,
            inode: 34,
            mount: FilesystemMountIdentity::LinuxMountId(56),
        };
        let journal = CacheGcQuarantineIdentity::from_identity(&identity);
        assert!(journal.matches(&identity));
        assert_eq!(journal.file_type, "regular-file");

        let other_device = FilesystemDirectoryIdentity {
            device: 13,
            ..identity.clone()
        };
        let other_inode = FilesystemDirectoryIdentity {
            inode: 35,
            ..identity.clone()
        };
        let other_mount = FilesystemDirectoryIdentity {
            mount: FilesystemMountIdentity::LinuxMountId(57),
            ..identity.clone()
        };
        assert!(!journal.matches(&other_device));
        assert!(!journal.matches(&other_inode));
        assert!(!journal.matches(&other_mount));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn generic_size_gc_skips_fallback_bearer_activity_without_scope_lease() {
        let dir = tempfile_dir();
        let layout = crate::storage::StorageLayout::from_prefix(&dir.path().join("storage"));
        let service = test_service(&crate::store_catalog::gha_cache_root(&layout));
        let token = "active-fallback-bearer-gc-token";
        let namespace = job_cache_fallback_namespace(token);
        service.ensure_tenant(&namespace).unwrap();
        let tenant_path = service.tenant_root(Some(&namespace));
        let blob = tenant_path.join("blobs").join(entry_hash("gc-key", "v1"));
        std::fs::write(&blob, vec![0; 4096]).unwrap();

        assert!(crate::capacity::active_scopes(
            &layout.run_root,
            std::time::Duration::from_secs(86_400)
        )
        .unwrap()
        .is_empty());

        let mut request_context = test_ctx(service);
        let active_request = route_fixture(
            &mut request_context,
            Request::builder()
                .method(hyper::Method::GET)
                .uri("http://cache.test/_apis/artifactcache/cache?keys=gc-missing&version=v1")
                .header("authorization", format!("Bearer {token}"))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await;
        assert_eq!(active_request.status(), StatusCode::NO_CONTENT);

        let scope = crate::cache::StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: crate::trust_scope::TRUSTED.to_owned(),
            daemon_environment: None,
        };
        let run_size_gc = || {
            crate::cache::run_gc_with(
                &layout.lib_root.join("work"),
                crate::args::CacheGcArgs {
                    dry_run: false,
                    yes: true,
                    force_no_lease_check: false,
                    keep_newest_targets: 0,
                    max_age_days: 365_000,
                    max_size_bytes: Some(0),
                },
                std::collections::BTreeMap::new(),
                &scope,
                |_, _| Ok(crate::leftover_disk::LeftoverReclaimReport::default()),
            )
        };

        run_size_gc().unwrap();
        assert!(tenant_path.is_dir(), "busy tenant must survive generic GC");
        assert!(blob.is_file(), "busy tenant data must survive generic GC");
        let history = std::fs::read_to_string(layout.log_root.join("gc-history.jsonl")).unwrap();
        assert!(
            history.lines().any(|line| {
                line.contains(namespace.as_str()) && line.contains("\"outcome\":\"skipped\"")
            }),
            "busy tenant must be recorded as skipped: {history}"
        );

        drop(active_request);

        let blobs_path = tenant_path.join("blobs");
        let (preserved_quarantine, replacement_path) = create_test_preserved_cache_gc_quarantine(
            &request_context.service.root,
            &blobs_path,
            "generic-gc-preserved",
        );
        run_size_gc().unwrap();
        assert!(
            tenant_path.is_dir(),
            "generic GC must keep a tenant with preserved quarantine data"
        );
        assert_eq!(
            std::fs::read(&replacement_path).unwrap(),
            b"frozen replacement bytes",
            "generic GC must retain frozen replacement bytes"
        );

        std::fs::remove_dir_all(&preserved_quarantine).unwrap();
        let (malformed_quarantine, unproven_entry) =
            create_test_malformed_cache_gc_quarantine(&request_context.service.root, &blobs_path);
        run_size_gc().unwrap();
        assert!(
            tenant_path.is_dir(),
            "generic GC must keep a tenant with an unproven quarantine"
        );
        assert_eq!(
            std::fs::read(&unproven_entry).unwrap(),
            b"unproven replacement bytes",
            "generic GC must retain entries with malformed identity journals"
        );

        std::fs::remove_dir_all(&malformed_quarantine).unwrap();
        run_size_gc().unwrap();
        assert!(
            !tenant_path.exists(),
            "idle tenant should be deleted on retry"
        );
        let history = std::fs::read_to_string(layout.log_root.join("gc-history.jsonl")).unwrap();
        assert!(
            history.lines().any(|line| {
                line.contains(namespace.as_str()) && line.contains("\"outcome\":\"deleted\"")
            }),
            "released tenant must be deleted on the next pass: {history}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn idle_tenant_gc_keeps_tenant_with_preserved_quarantine() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-idle-preserved-quarantine";
        service.ensure_tenant(namespace).unwrap();
        let tenant_path = service.tenant_root(Some(namespace));
        let (quarantine_path, replacement_path) = create_test_preserved_cache_gc_quarantine(
            &service.root,
            &tenant_path.join("blobs"),
            "idle-gc-preserved",
        );

        assert_eq!(
            service.reclaim_idle_tenant_namespaces("keep").unwrap(),
            0,
            "idle GC must skip a tenant with preserved quarantine data"
        );
        assert!(tenant_path.is_dir(), "tenant must remain available");
        assert!(quarantine_path.is_dir(), "frozen quarantine must remain");
        assert_eq!(
            std::fs::read(replacement_path).unwrap(),
            b"frozen replacement bytes",
            "idle GC must retain frozen replacement bytes"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn generic_cache_gc_tenant_guard_rejects_symlinked_cache_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile_dir();
        let real_root = dir.path().join("real-cache-root");
        let service = test_service(&real_root);
        let namespace = "tenant-generic-gc-root-symlink";
        service.ensure_tenant(namespace).unwrap();
        let real_tenant = service.tenant_root(Some(namespace));
        let pinned_tenant = open_cache_directory(&real_tenant, "test GHA tenant").unwrap();

        let cache_root_link = dir.path().join("cache-root-link");
        symlink(&real_root, &cache_root_link).unwrap();
        let linked_tenant = cache_root_link.join("tenants").join(namespace);
        assert!(
            try_lock_tenant_for_cache_gc(
                &cache_root_link,
                namespace,
                &linked_tenant,
                &pinned_tenant,
            )
            .is_err(),
            "GC must not reopen a cache root through a symlink"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn idle_tenant_gc_rejects_tenant_parent_replacement_before_quarantine() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-idle-gc-parent-swap";
        service.ensure_tenant(namespace).unwrap();
        let tenant_path = service.tenant_root(Some(namespace));
        let original_sentinel = tenant_path.join("blobs/original-sentinel");
        std::fs::write(&original_sentinel, b"original").unwrap();

        let tenants_path = service.root.join("tenants");
        let moved_tenants = dir.path().join("saved-tenants");
        let mut injected = false;
        let result =
            service.reclaim_idle_tenant_namespaces_with_hook("keep", |stage, candidate| {
                assert_eq!(stage, IdleTenantGcStage::BeforeTenantQuarantine);
                assert_eq!(candidate, tenant_path.as_path());
                injected = true;
                std::fs::rename(&tenants_path, &moved_tenants)?;
                std::fs::create_dir(&tenants_path)?;
                std::fs::rename(moved_tenants.join(namespace), &tenant_path)?;
                Ok(())
            });

        assert!(injected, "tenant ancestor swap hook ran under GC locks");
        assert!(
            result.is_err(),
            "GC must reject the replaced tenant parent inode"
        );
        assert_eq!(std::fs::read(&original_sentinel).unwrap(), b"original");
        assert!(tenant_path.is_dir(), "the moved tenant must remain intact");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn idle_tenant_gc_reaps_only_from_pinned_admission_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-idle-gc-admission-parent-swap";
        service.ensure_tenant(namespace).unwrap();
        let tenant_path = service.tenant_root(Some(namespace));
        let id = entry_hash("orphaned-admission", "v1");
        let attempt = "112233445566778899aabbccddeeff00";
        let admission_parent = service.root.join("admission-leases");
        let admission_path = cache_namespace_storage_dir(&service.root, Some(namespace));
        let lease_name = format!("{attempt}.json");
        let lease_path = admission_path.join(&lease_name);
        std::fs::create_dir_all(&admission_path).unwrap();
        std::fs::write(
            &lease_path,
            json!({"id": &id, "attempt": attempt}).to_string(),
        )
        .unwrap();
        let temp_path = tenant_path
            .join("blobs")
            .join(format!(".{id}.{attempt}.tmp"));
        std::fs::write(&temp_path, b"orphaned upload").unwrap();

        let moved_admission_parent = dir.path().join("saved-admission-leases");
        let outside_admission_parent = dir.path().join("replacement-admission-leases");
        let moved_lease = moved_admission_parent
            .join(admission_path.file_name().unwrap())
            .join(&lease_name);
        let outside_lease = outside_admission_parent
            .join(admission_path.file_name().unwrap())
            .join(&lease_name);
        let mut injected = false;
        let result = service.reclaim_idle_tenant_namespaces_with_hook("keep", |stage, _| {
            if stage == IdleTenantGcStage::BeforeAdmissionReap {
                injected = true;
                std::fs::rename(&admission_parent, &moved_admission_parent)?;
                std::fs::create_dir_all(outside_lease.parent().unwrap())?;
                std::fs::write(
                    &outside_lease,
                    json!({"id": &id, "attempt": attempt}).to_string(),
                )?;
                symlink(&outside_admission_parent, &admission_parent)?;
            }
            Ok(())
        });

        assert!(injected, "admission ancestor swap hook ran under GC locks");
        assert!(
            result.is_err(),
            "GC must reject the replaced admission parent"
        );
        assert!(
            !moved_lease.exists(),
            "stale lease reaped through pinned fd"
        );
        assert!(
            !temp_path.exists(),
            "stale temp reaped through pinned tenant fd"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_lease).unwrap(),
            json!({"id": &id, "attempt": attempt}).to_string()
        );
        assert!(tenant_path.is_dir(), "tenant survives admission path swap");
    }

    #[cfg(unix)]
    #[test]
    fn stale_file_quarantine_preserves_inode_replacement() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let root_directory = open_cache_directory(&service.root, "test cache root").unwrap();
        let root_identity =
            crate::leftover_disk::filesystem_object_identity(&root_directory).unwrap();
        let parent_path = service.root.join("stale-files");
        std::fs::create_dir(&parent_path).unwrap();
        let parent = open_cache_directory(&parent_path, "test stale-file parent").unwrap();
        let name = std::ffi::OsStr::new("lease.json");
        let path = parent_path.join(name);
        std::fs::write(&path, b"original").unwrap();
        let opened = open_cache_file_child(&parent, name, &path, "test stale lease")
            .unwrap()
            .unwrap();
        let expected =
            verify_cache_descendant_mount(&root_identity, &opened, "test stale lease").unwrap();
        let mut pinned_quarantine = None;

        let result = unlink_cache_file_if_same_with_hook(
            &root_identity,
            &parent,
            name,
            &opened,
            &expected,
            "test stale lease",
            |quarantine, entry| {
                pinned_quarantine = Some(quarantine.try_clone()?);
                let retained = std::ffi::OsStr::new("retained-original");
                rustix::fs::renameat_with(
                    quarantine,
                    entry,
                    quarantine,
                    retained,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                let mut replacement: std::fs::File = rustix::fs::openat(
                    quarantine,
                    entry,
                    rustix::fs::OFlags::RDWR
                        | rustix::fs::OFlags::CREATE
                        | rustix::fs::OFlags::EXCL
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::from_raw_mode(0o600),
                )
                .map_err(io::Error::from)?
                .into();
                replacement.write_all(b"replacement")?;
                Ok(())
            },
        );

        assert!(result.is_err(), "replacement inode must fail closed");
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        let quarantine = pinned_quarantine.expect("quarantine descriptor captured");
        let retained_path = PathBuf::from("retained-original");
        let retained = open_cache_file_child(
            &quarantine,
            std::ffi::OsStr::new("retained-original"),
            &retained_path,
            "retained stale lease",
        )
        .unwrap()
        .unwrap();
        let mut contents = Vec::new();
        let mut retained = retained;
        retained.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"original");
        assert_eq!(
            recover_cache_gc_quarantines(&parent, &root_identity, "test stale-file parent")
                .unwrap(),
            0,
            "recovery must keep a deliberately preserved mismatch frozen"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_file_recovery_preserves_replacement_captured_before_crash() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let root_directory = open_cache_directory(&service.root, "test cache root").unwrap();
        let root_identity =
            crate::leftover_disk::filesystem_object_identity(&root_directory).unwrap();
        let parent_path = service.root.join("stale-files");
        std::fs::create_dir(&parent_path).unwrap();
        let parent = open_cache_directory(&parent_path, "test stale-file parent").unwrap();
        let name = std::ffi::OsStr::new("lease.json");
        let path = parent_path.join(name);
        let retained_path = parent_path.join("retained-original");
        std::fs::write(&path, b"expected original").unwrap();
        let expected_file = open_cache_file_child(&parent, name, &path, "expected stale lease")
            .unwrap()
            .unwrap();
        let expected_identity =
            verify_cache_descendant_mount(&root_identity, &expected_file, "expected stale lease")
                .unwrap();
        let quarantine_name =
            std::ffi::OsString::from(format!("{CACHE_GC_QUARANTINE_PREFIX}replacement-test"));
        rustix::fs::mkdirat(
            &parent,
            &quarantine_name,
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .unwrap();
        let quarantine_path = parent_path.join(&quarantine_name);
        let quarantine = open_cache_directory_child(
            &parent,
            &quarantine_name,
            &quarantine_path,
            "test stale-file quarantine",
        )
        .unwrap()
        .unwrap();
        write_cache_gc_quarantine_identity(
            &quarantine,
            &root_identity,
            &expected_identity,
            "test stale lease",
        )
        .unwrap();

        std::fs::rename(&path, &retained_path).unwrap();
        std::fs::write(&path, b"replacement captured by rename").unwrap();
        rustix::fs::renameat_with(
            &parent,
            name,
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .unwrap();
        quarantine.sync_all().unwrap();
        parent.sync_all().unwrap();

        assert_eq!(
            recover_cache_gc_quarantines(&parent, &root_identity, "test stale-file parent")
                .unwrap(),
            0,
            "mismatched inode must be preserved, not counted as recovered"
        );
        assert_eq!(std::fs::read(&retained_path).unwrap(), b"expected original");
        assert_eq!(
            std::fs::read(quarantine_path.join(CACHE_GC_QUARANTINE_ENTRY)).unwrap(),
            b"replacement captured by rename"
        );
        assert!(quarantine_path
            .join(CACHE_GC_QUARANTINE_PRESERVED)
            .is_file());
        assert_eq!(
            recover_cache_gc_quarantines(&parent, &root_identity, "test stale-file parent")
                .unwrap(),
            0,
            "preserved mismatch must remain ineligible on later recovery"
        );
        assert!(quarantine_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_file_restore_failure_quarantine_stays_frozen() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let root_directory = open_cache_directory(&service.root, "test cache root").unwrap();
        let root_identity =
            crate::leftover_disk::filesystem_object_identity(&root_directory).unwrap();
        let parent_path = service.root.join("stale-files");
        std::fs::create_dir(&parent_path).unwrap();
        let parent = open_cache_directory(&parent_path, "test stale-file parent").unwrap();
        let name = std::ffi::OsStr::new("lease.json");
        let path = parent_path.join(name);
        std::fs::write(&path, b"quarantined original").unwrap();
        let opened = open_cache_file_child(&parent, name, &path, "test stale lease")
            .unwrap()
            .unwrap();
        let expected =
            verify_cache_descendant_mount(&root_identity, &opened, "test stale lease").unwrap();

        let result = unlink_cache_file_if_same_with_hook(
            &root_identity,
            &parent,
            name,
            &opened,
            &expected,
            "test stale lease",
            |_, _| {
                std::fs::write(&path, b"source collision").context("inject restore collision")?;
                anyhow::bail!("injected post-move failure")
            },
        );
        assert!(result.is_err(), "restore collision must fail closed");
        assert_eq!(std::fs::read(&path).unwrap(), b"source collision");

        let quarantine_name =
            cache_directory_entry_names(&parent, "test stale-file parent", &root_identity)
                .unwrap()
                .into_iter()
                .find(|name| {
                    name.to_str()
                        .is_some_and(|name| name.starts_with(CACHE_GC_QUARANTINE_PREFIX))
                })
                .expect("failed restore quarantine remains present");
        let quarantine_path = parent_path.join(&quarantine_name);
        let quarantine = open_cache_directory_child(
            &parent,
            &quarantine_name,
            &quarantine_path,
            "test stale-file quarantine",
        )
        .unwrap()
        .unwrap();
        assert!(quarantine_path
            .join(CACHE_GC_QUARANTINE_PRESERVED)
            .is_file());
        assert_eq!(
            recover_cache_gc_quarantines(&parent, &root_identity, "test stale-file parent")
                .unwrap(),
            0,
            "restore-failure quarantine must never become eligible for cleanup"
        );
        let quarantined = open_cache_file_child(
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            Path::new(CACHE_GC_QUARANTINE_ENTRY),
            "preserved stale lease",
        )
        .unwrap()
        .unwrap();
        let mut contents = Vec::new();
        let mut quarantined = quarantined;
        quarantined.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"quarantined original");
        assert!(quarantine_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_file_gc_recovers_interrupted_private_quarantine() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let root_directory = open_cache_directory(&service.root, "test cache root").unwrap();
        let root_identity =
            crate::leftover_disk::filesystem_object_identity(&root_directory).unwrap();
        let parent_path = service.root.join("stale-files");
        std::fs::create_dir(&parent_path).unwrap();
        let parent = open_cache_directory(&parent_path, "test stale-file parent").unwrap();
        let quarantine_name = std::ffi::OsString::from(format!("{CACHE_GC_QUARANTINE_PREFIX}test"));
        rustix::fs::mkdirat(
            &parent,
            &quarantine_name,
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .unwrap();
        let quarantine_path = parent_path.join(&quarantine_name);
        let quarantine = open_cache_directory_child(
            &parent,
            &quarantine_name,
            &quarantine_path,
            "test stale-file quarantine",
        )
        .unwrap()
        .unwrap();
        rustix::fs::fchmod(&quarantine, rustix::fs::Mode::from_raw_mode(0o700)).unwrap();
        let source_name = std::ffi::OsStr::new("lease.json");
        let source_path = parent_path.join(source_name);
        std::fs::write(&source_path, b"interrupted stale lease").unwrap();
        let stale = open_cache_file_child(&parent, source_name, &source_path, "test stale lease")
            .unwrap()
            .unwrap();
        let stale_identity =
            verify_cache_descendant_mount(&root_identity, &stale, "test stale lease").unwrap();
        write_cache_gc_quarantine_identity(
            &quarantine,
            &root_identity,
            &stale_identity,
            "test stale lease",
        )
        .unwrap();
        rustix::fs::renameat_with(
            &parent,
            source_name,
            &quarantine,
            std::ffi::OsStr::new(CACHE_GC_QUARANTINE_ENTRY),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .unwrap();
        quarantine.sync_all().unwrap();
        parent.sync_all().unwrap();

        assert_eq!(
            recover_cache_gc_quarantines(&parent, &root_identity, "test stale-file parent")
                .unwrap(),
            1
        );
        assert!(!quarantine_path.exists());
    }

    #[test]
    fn v1_range_manifest_waits_for_durable_part_and_recovery_reaps_failed_publication() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-v1-part-durability";
        service.ensure_tenant(namespace).unwrap();

        let key = "durable-v1-part";
        let version = "v1";
        let id = entry_hash(key, version);
        let attempt = "112233445566778899aabbccddeeff00";
        let tenant = service.tenant_root(Some(namespace));
        let staged = tenant.join("blobs").join(format!(".{id}.{attempt}.tmp"));
        std::fs::write(&staged, b"part").unwrap();
        let mut reservation = V1Reservation {
            key: key.to_owned(),
            version: version.to_owned(),
            cache_id: 123,
            expected_size: Some(4),
            chunks: Vec::new(),
            commit_attempt: None,
            active_uploads: vec![attempt.to_owned()],
        };
        persist_v1_reservation(&service, &id, &reservation, namespace).unwrap();
        let part = v1_chunk_file_path(&service, namespace, &id, attempt);
        let uploads = tenant.join("uploads");
        let parts = uploads.join(&id);
        assert!(!uploads.exists(), "fresh tenant has no uploads directory");
        assert!(!parts.exists(), "fresh tenant has no upload ID directory");
        let mut syncs = Vec::new();

        commit_v1_multipart_part_with_sync(
            &service,
            &id,
            namespace,
            &mut reservation,
            V1UploadChunk {
                start: 0,
                end: 3,
                attempt: attempt.to_owned(),
                sha256: hex(&Sha256::digest(b"part")),
            },
            &staged,
            &part,
            &mut |path| {
                let current = read_v1_reservation(&service, &id, namespace)
                    .unwrap()
                    .unwrap();
                assert!(
                    current.chunks.is_empty(),
                    "manifest cannot name a part before all part syncs finish"
                );
                syncs.push(path.to_path_buf());
                sync_multipart_path(path)
            },
        )
        .unwrap();

        assert_eq!(
            syncs,
            vec![
                tenant.clone(),
                uploads.clone(),
                staged.clone(),
                tenant.join("blobs"),
                parts.clone(),
            ],
            "sync new ancestor entries, part contents, rename source, and destination before manifest"
        );
        assert!(part.exists());
        assert!(uploads.is_dir(), "fresh uploads directory was created");
        assert!(parts.is_dir(), "fresh upload ID directory was created");
        let committed = read_v1_reservation(&service, &id, namespace)
            .unwrap()
            .unwrap();
        assert_eq!(committed.chunks[0].attempt, attempt);
        assert!(committed.active_uploads.is_empty());

        let failed_key = "failed-v1-part";
        let failed_id = entry_hash(failed_key, version);
        let failed_attempt = "2233445566778899aabbccddeeff0011";
        let failed_staged = tenant
            .join("blobs")
            .join(format!(".{failed_id}.{failed_attempt}.tmp"));
        std::fs::write(&failed_staged, b"bad!").unwrap();
        let mut failed_reservation = V1Reservation {
            key: failed_key.to_owned(),
            version: version.to_owned(),
            cache_id: 124,
            expected_size: Some(4),
            chunks: Vec::new(),
            commit_attempt: None,
            active_uploads: vec![failed_attempt.to_owned()],
        };
        persist_v1_reservation(&service, &failed_id, &failed_reservation, namespace).unwrap();
        let failed_part = v1_chunk_file_path(&service, namespace, &failed_id, failed_attempt);
        let failed_parts = failed_part.parent().unwrap().to_path_buf();
        let result = commit_v1_multipart_part_with_sync(
            &service,
            &failed_id,
            namespace,
            &mut failed_reservation,
            V1UploadChunk {
                start: 0,
                end: 3,
                attempt: failed_attempt.to_owned(),
                sha256: hex(&Sha256::digest(b"bad!")),
            },
            &failed_staged,
            &failed_part,
            &mut |path| {
                if path == failed_parts.as_path() {
                    anyhow::bail!("injected upload-parts directory fsync failure");
                }
                sync_multipart_path(path)
            },
        );
        assert!(result.is_err());
        assert!(
            failed_part.exists(),
            "rename preceded the injected sync failure"
        );
        let uncommitted = read_v1_reservation(&service, &failed_id, namespace)
            .unwrap()
            .unwrap();
        assert!(uncommitted.chunks.is_empty());
        assert!(uncommitted.active_uploads.is_empty());

        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant, Some(namespace), &mut outcome)
            .unwrap();
        assert!(
            !failed_part.exists(),
            "recovery reaps the unreferenced failed part"
        );
        assert!(
            part.exists(),
            "recovery retains the manifest-referenced part"
        );
    }

    #[cfg(unix)]
    #[test]
    fn multipart_upload_directory_creation_rejects_uploads_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-upload-symlink";
        service.ensure_tenant(namespace).unwrap();

        let tenant = service.tenant_root(Some(namespace));
        let outside = dir.path().join("outside-upload-tree");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, tenant.join("uploads")).unwrap();
        let parts = tenant.join("uploads").join("fresh-upload-id");

        let result = create_multipart_part_directory_with_sync(&parts, &mut sync_multipart_path);
        assert!(result.is_err(), "uploads symlink must be rejected");
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "rejected publication must not create an upload directory outside the tenant"
        );
    }

    #[test]
    fn v2_replacement_keeps_a_manifested_part_across_publication_crash_cuts() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let namespace = "tenant-v2-part-replacement";
        service.ensure_tenant(namespace).unwrap();

        let key = "durable-v2-replacement";
        let version = "v1";
        let id = entry_hash(key, version);
        let old_attempt = "33445566778899aabbccddeeff001122";
        let first_attempt = "445566778899aabbccddeeff00112233";
        let second_attempt = "5566778899aabbccddeeff0011223344";
        let tenant = service.tenant_root(Some(namespace));
        let old_part = v2_upload_part_path(&service, namespace, &id, old_attempt);
        std::fs::create_dir_all(old_part.parent().unwrap()).unwrap();
        std::fs::write(&old_part, b"old").unwrap();
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");
        let reservation = V2Reservation {
            key: key.to_owned(),
            version: version.to_owned(),
            upload_nonce: "66778899aabbccddeeff001122334455".to_owned(),
            block_id_bytes: Some(10),
            upload_attempt: None,
            upload_blocks: vec![V2UploadBlock {
                block_id: block_id.clone(),
                attempt: old_attempt.to_owned(),
                size: 3,
                sha256: hex(&Sha256::digest(b"old")),
            }],
            active_uploads: Vec::new(),
            assembly_scratch_attempt: None,
            assembly_scratch_bytes: 0,
            updated_ms: now_unix_millis().unwrap(),
        };
        reservation.persist(&service, &id, namespace).unwrap();

        let first_staged = tenant
            .join("blobs")
            .join(format!(".{id}.{first_attempt}.tmp"));
        std::fs::write(&first_staged, b"new").unwrap();
        let first_part = v2_upload_part_path(&service, namespace, &id, first_attempt);
        let failed = replace_multipart_part_then_persist_with_sync(
            &first_staged,
            &first_part,
            &old_part,
            || anyhow::bail!("injected manifest persistence failure"),
            &mut sync_multipart_path,
        );
        assert!(failed.is_err());
        assert!(
            old_part.exists(),
            "old manifest target remains on persistence failure"
        );
        assert!(
            first_part.exists(),
            "new durable part is left as recoverable debris"
        );
        assert_eq!(
            read_v2_reservation(&service, &id, namespace)
                .unwrap()
                .unwrap()
                .upload_blocks[0]
                .attempt,
            old_attempt
        );

        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant, Some(namespace), &mut outcome)
            .unwrap();
        assert!(
            !first_part.exists(),
            "recovery removes the unreferenced replacement"
        );
        assert!(
            old_part.exists(),
            "recovery retains the referenced old block"
        );

        let second_staged = tenant
            .join("blobs")
            .join(format!(".{id}.{second_attempt}.tmp"));
        std::fs::write(&second_staged, b"new").unwrap();
        let second_part = v2_upload_part_path(&service, namespace, &id, second_attempt);
        let mut second_update = reservation.clone();
        second_update.upload_blocks[0] = V2UploadBlock {
            block_id,
            attempt: second_attempt.to_owned(),
            size: 3,
            sha256: hex(&Sha256::digest(b"new")),
        };
        let manifest_then_crash = replace_multipart_part_then_persist_with_sync(
            &second_staged,
            &second_part,
            &old_part,
            || {
                second_update.persist(&service, &id, namespace)?;
                anyhow::bail!("injected crash after durable manifest publication")
            },
            &mut sync_multipart_path,
        );
        assert!(manifest_then_crash.is_err());
        assert!(
            old_part.exists(),
            "old part remains until persist reports success"
        );
        assert!(second_part.exists());
        assert_eq!(
            read_v2_reservation(&service, &id, namespace)
                .unwrap()
                .unwrap()
                .upload_blocks[0]
                .attempt,
            second_attempt,
            "durable replacement manifest names the new part"
        );

        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant, Some(namespace), &mut outcome)
            .unwrap();
        assert!(
            !old_part.exists(),
            "recovery reaps old unreferenced block data"
        );
        assert!(
            second_part.exists(),
            "recovery retains the newly referenced block"
        );
        let mut repeated_outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant, Some(namespace), &mut repeated_outcome)
            .unwrap();
        assert!(second_part.exists(), "repeated recovery is idempotent");
    }

    #[test]
    fn orphan_multipart_parts_and_unpublished_v1_indexes_are_reaped_safely() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let tenant_root = service.tenant_root(Some("tenant"));
        let uploads = tenant_root.join("uploads");

        let orphan_id = entry_hash("orphan-parts", "v1");
        let orphan_parts = uploads.join(&orphan_id);
        std::fs::create_dir_all(&orphan_parts).unwrap();
        std::fs::write(orphan_parts.join("range.part"), b"abandoned range").unwrap();

        let live_id = entry_hash("live-parts", "v1");
        let live_parts = uploads.join(&live_id);
        std::fs::create_dir_all(&live_parts).unwrap();
        std::fs::write(live_parts.join("block.part"), b"staged block").unwrap();
        let attempt = uuid::Uuid::new_v4().simple().to_string();
        let live_temp = create_upload_temp(&service, &live_id, Some("tenant"), &attempt).unwrap();

        let orphan_index_id = entry_hash("crashed-v1-id-allocation", "v1");
        let index = service.cache_id_index_path(123_456, Some("tenant"));
        atomically_create_json(&index, &json!({"entryHash": orphan_index_id})).unwrap();

        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant_root, Some("tenant"), &mut outcome)
            .unwrap();
        service
            .cleanup_orphan_v1_indexes_at(&tenant_root, Some("tenant"))
            .unwrap();
        assert!(
            !orphan_parts.exists(),
            "orphan multipart data must be reaped"
        );
        assert!(
            live_parts.exists(),
            "a live temp lease protects multipart data"
        );
        assert!(outcome.skipped_busy);
        assert!(
            !index.exists(),
            "a numeric id without its reservation is an orphan index"
        );

        drop(live_temp);
        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant_root, Some("tenant"), &mut outcome)
            .unwrap();
        assert!(!live_parts.exists(), "dead upload parts must be reclaimed");

        let fresh_id = entry_hash("fresh-claim-part-reap", "v1");
        let referenced_attempt = "11111111111111111111111111111111";
        let orphan_attempt = "22222222222222222222222222222222";
        let fresh_parts = uploads.join(&fresh_id);
        std::fs::create_dir_all(&fresh_parts).unwrap();
        let referenced_part = fresh_parts.join(format!("v2-{referenced_attempt}.part"));
        let orphan_part = fresh_parts.join(format!("v2-{orphan_attempt}.part"));
        std::fs::write(&referenced_part, b"referenced").unwrap();
        std::fs::write(&orphan_part, b"orphan").unwrap();
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");
        let claim = V2Reservation {
            key: "fresh-claim-part-reap".to_owned(),
            version: "v1".to_owned(),
            upload_nonce: "33333333333333333333333333333333".to_owned(),
            block_id_bytes: Some(10),
            upload_attempt: None,
            upload_blocks: vec![V2UploadBlock {
                block_id,
                attempt: referenced_attempt.to_owned(),
                size: 10,
                sha256: hex(&Sha256::digest(b"referenced")),
            }],
            active_uploads: Vec::new(),
            assembly_scratch_attempt: None,
            assembly_scratch_bytes: 0,
            updated_ms: now_unix_millis().unwrap(),
        };
        atomically_create_json(
            &service.reservation_path(&fresh_id, Some("tenant")),
            &claim.as_json(),
        )
        .unwrap();
        let mut outcome = UploadTempCleanup::default();
        service
            .cleanup_orphan_upload_parts_at(&tenant_root, Some("tenant"), &mut outcome)
            .unwrap();
        assert!(referenced_part.exists(), "fresh claim keeps recorded parts");
        assert!(
            !orphan_part.exists(),
            "fresh claim does not keep crash debris"
        );
    }

    #[test]
    fn abandoned_upload_temps_are_removed_without_touching_live_or_committed_data() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let tenant_root = service.tenant_root(Some("tenant"));
        let blobs = tenant_root.join("blobs");
        let nonce = "0123456789abcdef0123456789abcdef";

        let committed_id = entry_hash("committed-upload", "v1");
        let committed_blob = blobs.join(&committed_id);
        std::fs::write(&committed_blob, b"committed bytes").unwrap();
        let committed_entry_path = service.entry_path(&committed_id, Some("tenant"));
        commit_entry(
            &service,
            "committed-upload",
            "v1",
            b"committed bytes".len() as u64,
            Some("tenant"),
        )
        .unwrap();
        let committed_entry = std::fs::read(&committed_entry_path).unwrap();

        let orphan_id = entry_hash("orphan-upload", "v1");
        let orphan = blobs.join(format!(".{orphan_id}.{nonce}.tmp"));
        std::fs::write(&orphan, b"partial upload").unwrap();

        let live_id = entry_hash("live-upload", "v1");
        let live = blobs.join(format!(".{live_id}.{nonce}.tmp"));
        std::fs::write(&live, b"active upload").unwrap();
        let live_blob = blobs.join(&live_id);
        std::fs::write(&live_blob, b"published before finalize").unwrap();
        let live_lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&live)
            .unwrap();
        rustix::fs::flock(
            &live_lease,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();

        let committed_temp = blobs.join(format!(".{committed_id}.{nonce}.tmp"));
        std::fs::hard_link(&committed_blob, &committed_temp).unwrap();

        let malformed = blobs.join(format!(".{orphan_id}.not-a-nonce.tmp"));
        std::fs::write(&malformed, b"unrecognized filename").unwrap();

        assert_eq!(service.cleanup_all_upload_temps().unwrap().removed, 2);
        assert!(!orphan.exists());
        assert!(live.exists(), "cleanup must retain a temp with a live lock");
        assert!(
            live_blob.exists(),
            "cleanup must retain a canonical blob with a live lock"
        );
        assert!(
            !committed_temp.exists(),
            "cleanup may remove only the temp link"
        );
        assert_eq!(std::fs::read(&committed_blob).unwrap(), b"committed bytes");
        assert_eq!(
            std::fs::read(&committed_entry_path).unwrap(),
            committed_entry
        );
        assert!(
            malformed.exists(),
            "cleanup must ignore non-exact temp names"
        );

        drop(live_lease); // A crashed process releases the lease but leaves its temp link.
        assert_eq!(service.cleanup_all_upload_temps().unwrap().removed, 2);
        assert!(!live.exists());
        assert!(!live_blob.exists());
    }

    #[tokio::test]
    async fn crashed_v1_and_v2_upload_blobs_are_reaped_after_claim_expiry() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let tenant_root = ctx.service.tenant_root(Some("tenant"));
        let blobs = tenant_root.join("blobs");

        // Crash cut: the v1 upload published its canonical blob, but the
        // process died before the entry record and reservation cleanup.
        let v1_claim = reserve(
            post_json(json!({
                "key": "crashed-v1-upload",
                "version": "v1",
                "cacheSize": 7,
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let v1_id = v1_claim["cacheId"].as_str().unwrap();
        let v1_blob = blobs.join(v1_id);
        std::fs::write(&v1_blob, b"v1 blob").unwrap();
        assert_eq!(ctx.service.cleanup_all_upload_temps().unwrap().removed, 0);
        assert!(v1_blob.exists(), "fresh v1 claim protects upload blob");

        let old_time = std::time::SystemTime::now()
            - std::time::Duration::from_millis(V1_RESERVATION_TTL_MS + 1);
        std::fs::OpenOptions::new()
            .write(true)
            .open(ctx.service.reservation_path(v1_id, Some("tenant")))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        assert_eq!(ctx.service.cleanup_all_upload_temps().unwrap().removed, 1);
        assert!(!v1_blob.exists());
        assert!(!ctx.service.reservation_path(v1_id, Some("tenant")).exists());

        // Crash cut: v2 completed upload publication but finalize never
        // committed the entry. The stale claim and canonical blob are both
        // reclaimable under the same entry shard.
        let v2_claim = reserve_v2(
            post_json(json!({"key": "crashed-v2-upload", "version": "v1"})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let v2_id = entry_hash("crashed-v2-upload", "v1");
        let v2_blob = blobs.join(&v2_id);
        std::fs::write(&v2_blob, b"v2 blob").unwrap();
        let mut stale_v2 = read_reservation_record(&ctx.service, &v2_id, Some("tenant"))
            .unwrap()
            .unwrap()
            .0;
        stale_v2["updatedMs"] = json!(0);
        replace_json_atomically(
            &ctx.service.reservation_path(&v2_id, Some("tenant")),
            &stale_v2,
        )
        .unwrap();
        assert_eq!(ctx.service.cleanup_all_upload_temps().unwrap().removed, 1);
        assert!(!v2_blob.exists());
        assert!(!ctx
            .service
            .reservation_path(&v2_id, Some("tenant"))
            .exists());
        assert_eq!(v2_claim["ok"], json!(true));
    }

    #[tokio::test]
    async fn budget_counts_protected_orphan_blobs_and_evicts_committed_entries_first() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 3;
        service.ensure_tenant("tenant").unwrap();
        let tenant_root = service.tenant_root(Some("tenant"));
        let orphan_id = entry_hash("budget-orphan", "v1");
        let orphan_blob = tenant_root.join("blobs").join(&orphan_id);
        std::fs::write(&orphan_blob, b"orphan").unwrap();
        let claim = V2Reservation {
            key: "budget-orphan".to_owned(),
            version: "v1".to_owned(),
            upload_nonce: "0123456789abcdef0123456789abcdef".to_owned(),
            block_id_bytes: None,
            upload_attempt: None,
            upload_blocks: Vec::new(),
            active_uploads: Vec::new(),
            assembly_scratch_attempt: None,
            assembly_scratch_bytes: 0,
            updated_ms: now_unix_millis().unwrap(),
        };
        atomically_create_json(
            &service.reservation_path(&orphan_id, Some("tenant")),
            &claim.as_json(),
        )
        .unwrap();

        let committed_id = entry_hash("budget-committed", "v1");
        std::fs::write(tenant_root.join("blobs").join(&committed_id), b"kept?").unwrap();
        commit_entry(&service, "budget-committed", "v1", 5, Some("tenant")).unwrap();

        service.enforce_budget(Some("tenant")).await.unwrap();

        assert!(orphan_blob.exists(), "fresh claim must preserve its blob");
        assert!(service
            .reservation_path(&orphan_id, Some("tenant"))
            .exists());
        assert!(
            !service.entry_path(&committed_id, Some("tenant")).exists(),
            "canonical orphan bytes count toward budget, so committed entries are evicted first"
        );
        assert!(!tenant_root.join("blobs").join(&committed_id).exists());
    }

    #[test]
    fn cache_namespace_is_deterministic_token_specific_and_redacted() {
        let first = cache_namespace("job-token-a");
        assert_eq!(first, cache_namespace("job-token-a"));
        assert_ne!(first, cache_namespace("job-token-b"));
        assert_eq!(first.len(), 64);
        assert!(!first.contains("job-token-a"));
    }

    #[test]
    fn tenant_lookup_does_not_cross_token_namespaces() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        for namespace in ["tenant-a", "tenant-b"] {
            service.ensure_tenant(namespace).unwrap();
        }
        let hash = entry_hash("shared-key", "v1");
        let tenant_a_blob = service
            .tenant_root(Some("tenant-a"))
            .join("blobs")
            .join(&hash);
        std::fs::write(&tenant_a_blob, b"private").unwrap();
        commit_entry(&service, "shared-key", "v1", 7, Some("tenant-a")).unwrap();

        assert!(service
            .lookup(&["shared-key"], "v1", Some("tenant-a"))
            .unwrap()
            .is_some());
        assert!(service
            .lookup(&["shared-key"], "v1", Some("tenant-b"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn lookup_prefers_exact_over_prefix_and_newest_wins() {
        let dir = tempfile_dir();
        let svc = test_service(dir.path());
        commit_blob(&svc, "linux-rust-2026", "v1", b"old");
        std::thread::sleep(std::time::Duration::from_millis(5));
        commit_blob(&svc, "linux-rust", "v1", b"newer");

        // Exact beats newer prefix hit.
        let hit = svc
            .lookup(&["linux-rust-2026", "linux"], "v1", None)
            .unwrap()
            .unwrap();
        assert_eq!(hit.hash, entry_hash("linux-rust-2026", "v1"));
        assert_eq!(hit.key, "linux-rust-2026");

        // Prefix falls back to newest, and reports the stored key it matched.
        let hit = svc
            .lookup(&["linux-other", "linux"], "v1", None)
            .unwrap()
            .unwrap();
        assert_eq!(hit.hash, entry_hash("linux-rust", "v1"));
        assert_eq!(hit.key, "linux-rust");

        // Version mismatch misses.
        assert!(svc.lookup(&["linux-rust"], "v9", None).unwrap().is_none());
    }

    #[test]
    fn prefix_scan_equal_rank_breaks_ties_by_hash() {
        // Entries sharing created_ms and key length used to collide on one
        // rank-string map key, so the loser could overwrite the winner
        // depending on directory iteration order. The tie-break is now the
        // greater entry hash, independent of iteration order. Each pair shares
        // one restore prefix, so every pair independently exercises the tie.
        let dir = tempfile_dir();
        let svc = test_service(dir.path());
        let created_ms = 1_757_836_800_000;
        let mut pairs = Vec::new();
        for index in 0..16 {
            let first = format!("pair{index:02}-aa");
            let second = format!("pair{index:02}-bb");
            commit_blob_at(&svc, &first, "v1", b"a-body", created_ms);
            commit_blob_at(&svc, &second, "v1", b"b-body", created_ms);
            assert_eq!(first.len(), second.len());
            assert_ne!(entry_hash(&first, "v1"), entry_hash(&second, "v1"));
            pairs.push((first, second));
        }

        for (first, second) in &pairs {
            let winner = if entry_hash(first, "v1") > entry_hash(second, "v1") {
                first
            } else {
                second
            };
            let restore = format!("{}-", &winner[..6]);
            let primary = format!("{restore}zz");
            let hit = svc
                .lookup(&[primary.as_str(), restore.as_str()], "v1", None)
                .unwrap();
            let hit = hit.unwrap_or_else(|| panic!("restore {restore} missed"));
            assert_eq!(hit.hash, entry_hash(winner, "v1"), "restore {restore}");
            assert_eq!(hit.key, *winner, "restore {restore}");
        }
    }

    fn test_ctx(service: CacheService) -> Ctx {
        Ctx {
            service: Arc::new(service),
            public_base: "http://cache.test".to_owned(),
        }
    }

    fn authenticated_request(
        method: hyper::Method,
        path: &str,
        body: impl Into<Bytes>,
    ) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(format!("http://cache.test{path}"))
            .header("authorization", "Bearer client-fixture-token")
            .body(Full::new(body.into()))
            .expect("build client-shaped request")
    }

    fn capability_request(
        method: hyper::Method,
        path: &str,
        body: impl Into<Bytes>,
    ) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(format!("http://cache.test{path}"))
            .body(Full::new(body.into()))
            .expect("build signed Azure request")
    }

    async fn route_fixture(ctx: &mut Ctx, request: Request<Full<Bytes>>) -> Response<ResponseBody> {
        route(request, ctx).await.expect("route response")
    }

    async fn response_bytes(
        response: Response<ResponseBody>,
    ) -> (StatusCode, hyper::HeaderMap, Bytes) {
        let (parts, body) = response.into_parts();
        let bytes = body
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        (parts.status, parts.headers, bytes)
    }

    fn fixture_json_request(
        method: hyper::Method,
        path: &str,
        value: Value,
    ) -> Request<Full<Bytes>> {
        let mut request = authenticated_request(method, path, Bytes::from(value.to_string()));
        request.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            "application/json".parse().expect("valid content type"),
        );
        request
    }

    #[tokio::test]
    async fn official_v1_routes_reserve_parallel_ranges_commit_lookup_and_range_download() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));
        let key = "official-v1-route";
        let version = "0123456789abcdef";

        let reserve = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        assert_eq!(reserve.status(), StatusCode::OK);
        let (status, _, body) = response_bytes(reserve).await;
        assert_eq!(status, StatusCode::OK);
        let reserved: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reserved["cacheID"], reserved["cacheId"]);
        let cache_id = reserved["cacheId"].as_u64().unwrap().to_string();

        let duplicate = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        let (status, _, body) = response_bytes(duplicate).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["typeKey"],
            "ArtifactCacheItemAlreadyExistsException"
        );

        // BuildKit's client PATCHes inclusive ranges in parallel and commits
        // the final total size in a separate POST.
        let mut first_range = authenticated_request(
            hyper::Method::PATCH,
            &format!("/_apis/artifactcache/caches/{cache_id}"),
            Bytes::from_static(b"he"),
        );
        first_range.headers_mut().insert(
            CONTENT_RANGE,
            "bytes 0-1/*".parse().expect("valid Content-Range"),
        );
        first_range
            .headers_mut()
            .insert(CONTENT_LENGTH, "2".parse().expect("valid Content-Length"));
        let mut second_range = authenticated_request(
            hyper::Method::PATCH,
            &format!("/_apis/artifactcache/caches/{cache_id}"),
            Bytes::from_static(b"llo"),
        );
        second_range.headers_mut().insert(
            CONTENT_RANGE,
            "bytes 2-4/*".parse().expect("valid Content-Range"),
        );
        second_range
            .headers_mut()
            .insert(CONTENT_LENGTH, "3".parse().expect("valid Content-Length"));
        let mut first_ctx = test_ctx((*ctx.service).clone());
        let mut second_ctx = test_ctx((*ctx.service).clone());
        let (first_upload, second_upload) = tokio::join!(
            route_fixture(&mut first_ctx, first_range),
            route_fixture(&mut second_ctx, second_range),
        );
        assert_eq!(first_upload.status(), StatusCode::NO_CONTENT);
        assert_eq!(second_upload.status(), StatusCode::NO_CONTENT);

        let commit = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                &format!("/_apis/artifactcache/caches/{cache_id}"),
                json!({"size": 5}),
            ),
        )
        .await;
        assert_eq!(commit.status(), StatusCode::OK);

        let lookup = route_fixture(
            &mut ctx,
            authenticated_request(
                hyper::Method::GET,
                &format!("/_apis/artifactcache/cache?keys={key}&version={version}"),
                Bytes::new(),
            ),
        )
        .await;
        assert_eq!(lookup.status(), StatusCode::OK);
        let (status, _, body) = response_bytes(lookup).await;
        assert_eq!(status, StatusCode::OK);
        let hit: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(hit["cacheKey"], json!(key));
        let download_url = url::Url::parse(hit["archiveLocation"].as_str().unwrap()).unwrap();
        let download_path = format!(
            "{}?{}",
            download_url.path(),
            download_url.query().expect("signed archive URL query")
        );
        let mut range_request =
            capability_request(hyper::Method::GET, &download_path, Bytes::new());
        range_request
            .headers_mut()
            .insert(RANGE, "bytes=1-3".parse().expect("valid Range"));
        let (status, headers, body) =
            response_bytes(route_fixture(&mut ctx, range_request).await).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers[CONTENT_RANGE], "bytes 1-3/5");
        assert_eq!(headers[CONTENT_LENGTH], "3");
        assert_eq!(body, Bytes::from_static(b"ell"));

        let duplicate = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        let (status, _, body) = response_bytes(duplicate).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["typeKey"], "ArtifactCacheItemAlreadyExistsException");

        let stale_commit = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                &format!("/_apis/artifactcache/caches/{cache_id}"),
                json!({"size": 5}),
            ),
        )
        .await;
        assert_eq!(
            stale_commit.status(),
            StatusCode::CONFLICT,
            "expired or foreign numeric IDs must not turn into internal errors"
        );
    }

    #[tokio::test]
    async fn v1_multipart_commit_above_half_budget_uses_global_assembly_scratch() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 4096;
        let mut ctx = test_ctx(service);
        let key = "multipart-over-half-budget";
        let version = "v1";
        let payload = [vec![b'a'; 1500], vec![b'b'; 1500]].concat();

        let reserve = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        assert_eq!(reserve.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(reserve).await;
        let cache_id = serde_json::from_slice::<Value>(&body).unwrap()["cacheId"]
            .as_u64()
            .unwrap()
            .to_string();

        for (start, end, contents) in [
            (0u64, 1499u64, vec![b'a'; 1500]),
            (1500u64, 2999u64, vec![b'b'; 1500]),
        ] {
            let mut range = authenticated_request(
                hyper::Method::PATCH,
                &format!("/_apis/artifactcache/caches/{cache_id}"),
                Bytes::from(contents),
            );
            range.headers_mut().insert(
                CONTENT_RANGE,
                format!("bytes {start}-{end}/3000")
                    .parse()
                    .expect("valid Content-Range"),
            );
            range.headers_mut().insert(
                CONTENT_LENGTH,
                "1500".parse().expect("valid Content-Length"),
            );
            let response = route_fixture(&mut ctx, range).await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }

        let commit = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                &format!("/_apis/artifactcache/caches/{cache_id}"),
                json!({"size": payload.len()}),
            ),
        )
        .await;
        assert_eq!(commit.status(), StatusCode::OK);

        let namespace = ctx
            .service
            .resolve_namespaces("client-fixture-token")
            .remove(0);
        let blob = ctx
            .service
            .tenant_root(Some(&namespace))
            .join("blobs")
            .join(entry_hash(key, version));
        assert_eq!(std::fs::read(blob).unwrap(), payload);
    }

    #[tokio::test]
    async fn v2_blocklist_commit_above_half_budget_uses_global_assembly_scratch() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 4096;
        let mut ctx = test_ctx(service);
        let key = "v2-blocklist-over-half-budget";
        let version = "v1";
        let payload = vec![b'x'; 3000];

        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        assert_eq!(create.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        let upload_url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");

        let mut block_url = upload_url.clone();
        block_url
            .query_pairs_mut()
            .append_pair("comp", "block")
            .append_pair("blockid", &block_id);
        let mut put_block = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", block_url.path(), block_url.query().unwrap()),
            Bytes::from(payload.clone()),
        );
        put_block.headers_mut().insert(
            CONTENT_LENGTH,
            "3000".parse().expect("valid Content-Length"),
        );
        assert_eq!(
            route_fixture(&mut ctx, put_block).await.status(),
            StatusCode::CREATED
        );

        let mut block_list_url = upload_url.clone();
        block_list_url
            .query_pairs_mut()
            .append_pair("comp", "blocklist");
        let block_list = format!("<BlockList><Latest>{block_id}</Latest></BlockList>");
        let put_list = capability_request(
            hyper::Method::PUT,
            &format!(
                "{}?{}",
                block_list_url.path(),
                block_list_url.query().unwrap()
            ),
            Bytes::from(block_list),
        );
        assert_eq!(
            route_fixture(&mut ctx, put_list).await.status(),
            StatusCode::CREATED
        );

        let namespace = ctx
            .service
            .resolve_namespaces("client-fixture-token")
            .remove(0);
        let id = entry_hash(key, version);
        let claim = read_v2_reservation(&ctx.service, &id, &namespace)
            .unwrap()
            .expect("blocklist claim remains until finalize");
        assert_eq!(claim.assembly_scratch_bytes, payload.len() as u64);
        let scratch_attempt = claim
            .assembly_scratch_attempt
            .as_deref()
            .expect("blocklist keeps its root-wide scratch charge");
        let scratch_path =
            assembly_scratch_lease_path(&ctx.service.root, Some(&namespace), scratch_attempt);
        assert!(scratch_path.exists());

        // Startup recovery must retain the durable claim while the duplicate
        // source parts are still staged, and another tenant cannot spend that
        // root-wide headroom before finalize removes the duplicate.
        let mut reopened = CacheService::open(ctx.service.root.clone()).unwrap();
        reopened.budget_bytes = 4096;
        let blocked = reopened
            .try_admit_upload_with_scratch(
                Some("tenant-blocked"),
                &entry_hash("other-tenant-blocklist", "v1"),
                "11112222333344445555666677778888",
                4096,
                Some(0),
                payload.len() as u64,
                2,
            )
            .await;
        assert!(matches!(
            blocked,
            Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()
        ));

        let delayed_attempt = uuid::Uuid::new_v4().simple().to_string();
        let delayed = reopened
            .try_admit_upload_with_scratch(
                Some("tenant-blocked"),
                &entry_hash("delayed-finalize-probe", "v1"),
                &delayed_attempt,
                4096,
                Some(0),
                1100,
                2,
            )
            .await;
        assert!(
            matches!(
                delayed,
                Err(error) if error.downcast_ref::<CacheLockBusy>().is_some()
            ),
            "the published blob remains charged while source blocks await finalize"
        );

        let failed_finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": key, "version": version, "size_bytes": payload.len() - 1}),
            ),
        )
        .await;
        assert_eq!(failed_finalize.status(), StatusCode::OK);
        let (_, _, failed_body) = response_bytes(failed_finalize).await;
        assert_eq!(
            serde_json::from_slice::<Value>(&failed_body).unwrap()["ok"],
            false,
            "a failed finalize removes the canonical duplicate but retains retryable blocks"
        );
        assert!(
            ctx.service
                .tenant_root(Some(&namespace))
                .join("uploads")
                .join(&id)
                .exists(),
            "failed finalize keeps the source blocks for retry"
        );
        assert!(
            !ctx.service
                .tenant_root(Some(&namespace))
                .join("blobs")
                .join(&id)
                .exists(),
            "failed finalize removes the duplicate canonical blob"
        );
        let failed_attempt = uuid::Uuid::new_v4().simple().to_string();
        let after_failed_finalize = reopened
            .try_admit_upload_with_scratch(
                Some("tenant-blocked"),
                &entry_hash("retry-after-failed-finalize", "v1"),
                &failed_attempt,
                4096,
                Some(0),
                1100,
                2,
            )
            .await
            .expect("failed finalize removed the canonical duplicate copy");
        drop(after_failed_finalize);

        let retry_block_list = capability_request(
            hyper::Method::PUT,
            &format!(
                "{}?{}",
                block_list_url.path(),
                block_list_url.query().unwrap()
            ),
            Bytes::from(format!(
                "<BlockList><Latest>{block_id}</Latest></BlockList>"
            )),
        );
        assert_eq!(
            route_fixture(&mut ctx, retry_block_list).await.status(),
            StatusCode::CREATED,
            "retry can assemble from retained source blocks after failed finalize"
        );
        let retry_claim = read_v2_reservation(&ctx.service, &id, &namespace)
            .unwrap()
            .expect("retried blocklist claim remains until finalize");
        let retry_scratch_attempt = retry_claim
            .assembly_scratch_attempt
            .as_deref()
            .expect("retry records its scratch attempt");
        let retry_scratch_path =
            assembly_scratch_lease_path(&ctx.service.root, Some(&namespace), retry_scratch_attempt);
        assert!(retry_scratch_path.exists());

        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": key, "version": version, "size_bytes": payload.len()}),
            ),
        )
        .await;
        assert_eq!(finalize.status(), StatusCode::OK);
        let (_, _, finalize_body) = response_bytes(finalize).await;
        assert_eq!(
            serde_json::from_slice::<Value>(&finalize_body).unwrap()["ok"],
            true
        );
        assert!(scratch_path != retry_scratch_path);
        assert!(!scratch_path.exists());
        assert!(!retry_scratch_path.exists());
        assert!(
            !ctx.service
                .tenant_root(Some(&namespace))
                .join("uploads")
                .join(&id)
                .exists(),
            "finalize removes source parts before releasing their scratch charge"
        );
        let admitted = reopened
            .try_admit_upload_with_scratch(
                Some("tenant-blocked"),
                &entry_hash("other-tenant-blocklist", "v1"),
                "99992222333344445555666677778888",
                4096,
                Some(0),
                payload.len() as u64,
                2,
            )
            .await
            .expect("finalize frees root-wide duplicate-copy headroom");
        drop(admitted);
        let blob = ctx
            .service
            .tenant_root(Some(&namespace))
            .join("blobs")
            .join(entry_hash(key, version));
        assert_eq!(std::fs::read(blob).unwrap(), payload);
    }

    #[tokio::test]
    async fn official_v1_over_budget_uploads_use_bad_request_status() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        service.budget_bytes = 2;
        let mut ctx = test_ctx(service);
        let reserve = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": "v1-over-budget", "version": "v1"}),
            ),
        )
        .await;
        assert_eq!(reserve.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(reserve).await;
        let cache_id = serde_json::from_slice::<Value>(&body).unwrap()["cacheId"]
            .as_u64()
            .unwrap()
            .to_string();

        let mut patch = authenticated_request(
            hyper::Method::PATCH,
            &format!("/_apis/artifactcache/caches/{cache_id}"),
            Bytes::from_static(b"abc"),
        );
        patch.headers_mut().insert(
            CONTENT_RANGE,
            "bytes 0-2/*".parse().expect("valid Content-Range"),
        );
        patch
            .headers_mut()
            .insert(CONTENT_LENGTH, "3".parse().expect("valid Content-Length"));
        assert_eq!(
            route_fixture(&mut ctx, patch).await.status(),
            StatusCode::BAD_REQUEST
        );

        let commit = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                &format!("/_apis/artifactcache/caches/{cache_id}"),
                json!({"size": 3}),
            ),
        )
        .await;
        assert_eq!(commit.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn concurrent_overlapping_v1_ranges_reject_the_loser_at_completion() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));
        let key = "concurrent-overlap";
        let reserve = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/_apis/artifactcache/caches",
                json!({"key": key, "version": "v1"}),
            ),
        )
        .await;
        let (_, _, reserve_body) = response_bytes(reserve).await;
        let cache_id = serde_json::from_slice::<Value>(&reserve_body).unwrap()["cacheID"]
            .as_u64()
            .unwrap()
            .to_string();

        let (first_ready_tx, mut first_ready_rx) = tokio::sync::oneshot::channel();
        let (second_ready_tx, mut second_ready_rx) = tokio::sync::oneshot::channel();
        let (first_release_tx, first_release_rx) = tokio::sync::oneshot::channel();
        let (second_release_tx, second_release_rx) = tokio::sync::oneshot::channel();
        let first_body = StreamBody::new(Box::pin(stream::once(async move {
            first_ready_tx.send(()).unwrap();
            first_release_rx.await.unwrap();
            Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"abc")))
        })));
        let second_body = StreamBody::new(Box::pin(stream::once(async move {
            second_ready_tx.send(()).unwrap();
            second_release_rx.await.unwrap();
            Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"XYZ")))
        })));
        fn make_request<B>(cache_id: &str, range: &str, body: B) -> Request<B> {
            let mut request = Request::builder()
                .method(hyper::Method::PATCH)
                .uri(format!(
                    "http://cache.test/_apis/artifactcache/caches/{cache_id}"
                ))
                .header("authorization", "Bearer client-fixture-token")
                .body(body)
                .unwrap();
            request
                .headers_mut()
                .insert(CONTENT_RANGE, range.parse().expect("valid Content-Range"));
            request
                .headers_mut()
                .insert(CONTENT_LENGTH, "3".parse().expect("valid Content-Length"));
            request
        }
        let first_request = make_request(&cache_id, "bytes 0-2/*", first_body);
        let second_request = make_request(&cache_id, "bytes 1-3/*", second_body);
        let mut first_ctx = test_ctx((*ctx.service).clone());
        let mut second_ctx = test_ctx((*ctx.service).clone());
        let first_route = route(first_request, &mut first_ctx);
        let second_route = route(second_request, &mut second_ctx);
        tokio::pin!(first_route, second_route);

        let mut first_ready = false;
        let mut second_ready = false;
        while !first_ready || !second_ready {
            tokio::select! {
                _ = &mut first_ready_rx, if !first_ready => first_ready = true,
                _ = &mut second_ready_rx, if !second_ready => second_ready = true,
                _ = &mut first_route => panic!("first range completed before body release"),
                _ = &mut second_route => panic!("second range completed before body release"),
            }
        }
        first_release_tx.send(()).unwrap();
        second_release_tx.send(()).unwrap();
        let (first, second) = tokio::join!(first_route, second_route);
        let statuses = [
            first.expect("first route response").status(),
            second.expect("second route response").status(),
        ];
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::NO_CONTENT)
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::BAD_REQUEST)
                .count(),
            1
        );

        let id = entry_hash(key, "v1");
        let reservation =
            read_v1_reservation(&ctx.service, &id, &cache_namespace("client-fixture-token"))
                .unwrap()
                .unwrap();
        assert_eq!(reservation.chunks.len(), 1);
        assert!(reservation.active_uploads.is_empty());
    }

    #[tokio::test]
    async fn official_v2_twirp_and_azure_routes_handle_single_and_block_blob_uploads() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));

        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": "official-v2-single", "version": "v1"}),
            ),
        )
        .await;
        assert_eq!(create.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created["ok"], true);
        let upload_url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
        let upload_query: Vec<_> = upload_url.query_pairs().map(|(name, _)| name).collect();
        assert!(upload_query.iter().any(|name| name == "sig"));
        assert!(!upload_query
            .iter()
            .any(|name| name == "claim" || name == "cap"));
        let mut put = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", upload_url.path(), upload_url.query().unwrap()),
            Bytes::from_static(b"single"),
        );
        put.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            "6".parse().expect("valid Content-Length"),
        );
        put.headers_mut().insert(
            "x-ms-blob-type",
            "BlockBlob".parse().expect("valid blob type"),
        );
        assert_eq!(
            route_fixture(&mut ctx, put).await.status(),
            StatusCode::CREATED
        );

        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": "official-v2-single", "version": "v1", "size_bytes": 6}),
            ),
        )
        .await;
        assert_eq!(finalize.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(finalize).await;
        let finalized: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(finalized["ok"], true);
        let entry_id = finalized["entryId"].as_str().unwrap();
        assert!(
            entry_id.parse::<i64>().is_ok(),
            "entryId is proto int64 JSON"
        );
        assert_eq!(finalized["entry_id"], entry_id);
        let namespace = ctx.service.resolve_namespaces("client-fixture-token")[0].clone();
        assert_eq!(
            ctx.service
                .read_entry(&entry_hash("official-v2-single", "v1"), Some(&namespace))
                .unwrap()["entryId"],
            entry_id,
            "numeric ID persists in the committed manifest"
        );

        let duplicate_finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": "official-v2-single", "version": "v1", "size_bytes": 6}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(duplicate_finalize).await;
        let duplicate_finalize: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(duplicate_finalize["entryId"], entry_id);

        let lookup = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/GetCacheEntryDownloadURL",
                json!({
                    "key": "official-v2-single",
                    "version": "v1",
                    "restore_keys": [],
                }),
            ),
        )
        .await;
        assert_eq!(lookup.status(), StatusCode::OK);
        let (_, _, body) = response_bytes(lookup).await;
        let lookup: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(lookup["matched_key"], "official-v2-single");
        let download_url =
            url::Url::parse(lookup["signed_download_url"].as_str().unwrap()).unwrap();
        assert!(download_url.query_pairs().any(|(name, _)| name == "sig"));
        let signed_download_path = format!(
            "{}?{}",
            download_url.path(),
            download_url.query().expect("signed download URL query")
        );
        let mut get = capability_request(hyper::Method::GET, &signed_download_path, Bytes::new());
        get.headers_mut()
            .insert("x-ms-range", "bytes=1-3".parse().expect("valid x-ms-range"));
        let (status, headers, body) = response_bytes(route_fixture(&mut ctx, get).await).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers[CONTENT_RANGE], "bytes 1-3/6");
        assert_eq!(body, Bytes::from_static(b"ing"));

        let head = capability_request(hyper::Method::HEAD, &signed_download_path, Bytes::new());
        let (status, headers, body) = response_bytes(route_fixture(&mut ctx, head).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_LENGTH], "6");
        assert!(body.is_empty());

        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": "official-v2-blocks", "version": "v1"}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        let upload_url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");
        let mut block_url = upload_url.clone();
        block_url
            .query_pairs_mut()
            .append_pair("comp", "block")
            .append_pair("blockid", &block_id);
        let mut put_block = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", block_url.path(), block_url.query().unwrap()),
            Bytes::from_static(b"block"),
        );
        put_block.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            "5".parse().expect("valid Content-Length"),
        );
        assert_eq!(
            route_fixture(&mut ctx, put_block).await.status(),
            StatusCode::CREATED
        );

        let mismatched_block_id = base64::engine::general_purpose::STANDARD.encode(b"short");
        let mut mismatched_block_url = upload_url.clone();
        mismatched_block_url
            .query_pairs_mut()
            .append_pair("comp", "block")
            .append_pair("blockid", &mismatched_block_id);
        let mut mismatched_block = capability_request(
            hyper::Method::PUT,
            &format!(
                "{}?{}",
                mismatched_block_url.path(),
                mismatched_block_url.query().unwrap()
            ),
            Bytes::from_static(b"bad"),
        );
        mismatched_block.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            "3".parse().expect("valid Content-Length"),
        );
        assert_eq!(
            route_fixture(&mut ctx, mismatched_block).await.status(),
            StatusCode::BAD_REQUEST,
            "Azure block IDs for a blob must retain the first decoded length"
        );

        let mut replace_block = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", block_url.path(), block_url.query().unwrap()),
            Bytes::from_static(b"replaced"),
        );
        replace_block.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            "8".parse().expect("valid Content-Length"),
        );
        let replaced = route_fixture(&mut ctx, replace_block).await;
        assert_eq!(
            replaced.status(),
            StatusCode::CREATED,
            "Put Block replaces bytes for an existing block ID"
        );
        assert!(replaced
            .headers()
            .get("x-ms-request-server-encrypted")
            .is_none());

        let mut block_list_url = upload_url.clone();
        block_list_url
            .query_pairs_mut()
            .append_pair("comp", "blocklist");
        let block_list = format!("<BlockList><Latest>{block_id}</Latest></BlockList>");
        let put_list = capability_request(
            hyper::Method::PUT,
            &format!(
                "{}?{}",
                block_list_url.path(),
                block_list_url.query().unwrap()
            ),
            Bytes::from(block_list),
        );
        assert_eq!(
            route_fixture(&mut ctx, put_list).await.status(),
            StatusCode::CREATED
        );
        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": "official-v2-blocks", "version": "v1", "size_bytes": 8}),
            ),
        )
        .await;
        let (status, _, body) = response_bytes(finalize).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["ok"], true);
        let download = route_fixture(
            &mut ctx,
            authenticated_request(
                hyper::Method::GET,
                &format!(
                    "/_results/download/{}",
                    entry_hash("official-v2-blocks", "v1")
                ),
                Bytes::new(),
            ),
        )
        .await;
        let (status, _, body) = response_bytes(download).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, Bytes::from_static(b"replaced"));
    }

    #[tokio::test]
    async fn unfinalized_v2_blobs_consume_budget_until_their_entries_commit() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));
        let namespace = cache_namespace("client-fixture-token");
        let entries = [
            ("unfinalized-budget-a", 400usize),
            ("unfinalized-budget-b", 400usize),
            ("unfinalized-budget-c", 300usize),
        ];
        let mut uploads = Vec::new();

        for (key, size) in entries {
            let create = route_fixture(
                &mut ctx,
                fixture_json_request(
                    hyper::Method::POST,
                    "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                    json!({"key": key, "version": "v1"}),
                ),
            )
            .await;
            assert_eq!(create.status(), StatusCode::OK);
            let (_, _, body) = response_bytes(create).await;
            let created: Value = serde_json::from_slice(&body).unwrap();
            let url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
            uploads.push((key, size, entry_hash(key, "v1"), url));
        }

        for (key, size, id, url) in &uploads[..2] {
            let mut put = capability_request(
                hyper::Method::PUT,
                &format!("{}?{}", url.path(), url.query().unwrap()),
                Bytes::from(vec![b'x'; *size]),
            );
            put.headers_mut().insert(
                hyper::header::CONTENT_LENGTH,
                size.to_string().parse().unwrap(),
            );
            put.headers_mut().insert(
                "x-ms-blob-type",
                "BlockBlob".parse().expect("valid blob type"),
            );
            assert_eq!(
                route_fixture(&mut ctx, put).await.status(),
                StatusCode::CREATED,
                "unfinalized upload {key} is admitted below the aggregate budget"
            );
            assert!(ctx
                .service
                .tenant_root(Some(&namespace))
                .join("blobs")
                .join(id)
                .exists());
        }
        assert_eq!(
            ctx.service
                .staged_upload_usage(Some(&namespace), None)
                .unwrap(),
            (800, 2),
            "canonical blobs without entries remain budgeted"
        );

        let (blocked_key, blocked_size, blocked_id, blocked_url) = &uploads[2];
        let mut blocked_put = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", blocked_url.path(), blocked_url.query().unwrap()),
            Bytes::from(vec![b'y'; *blocked_size]),
        );
        blocked_put.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            blocked_size.to_string().parse().unwrap(),
        );
        blocked_put.headers_mut().insert(
            "x-ms-blob-type",
            "BlockBlob".parse().expect("valid blob type"),
        );
        assert_eq!(
            route_fixture(&mut ctx, blocked_put).await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "unique unfinalized keys cannot bypass aggregate staged-byte admission"
        );
        assert!(!ctx
            .service
            .tenant_root(Some(&namespace))
            .join("blobs")
            .join(blocked_id)
            .exists());
        let _ = blocked_key;

        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": uploads[0].0, "version": "v1", "size_bytes": uploads[0].1}),
            ),
        )
        .await;
        assert_eq!(finalize.status(), StatusCode::OK);
        let (_, _, finalize_body) = response_bytes(finalize).await;
        assert_eq!(
            serde_json::from_slice::<Value>(&finalize_body).unwrap()["ok"],
            true
        );
        assert_eq!(
            ctx.service
                .staged_upload_usage(Some(&namespace), None)
                .unwrap(),
            (400, 1),
            "a committed blob leaves staged accounting exactly once"
        );

        let mut retry = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", blocked_url.path(), blocked_url.query().unwrap()),
            Bytes::from(vec![b'y'; *blocked_size]),
        );
        retry.headers_mut().insert(
            hyper::header::CONTENT_LENGTH,
            blocked_size.to_string().parse().unwrap(),
        );
        retry.headers_mut().insert(
            "x-ms-blob-type",
            "BlockBlob".parse().expect("valid blob type"),
        );
        assert_eq!(
            route_fixture(&mut ctx, retry).await.status(),
            StatusCode::CREATED
        );
        assert!(ctx
            .service
            .tenant_root(Some(&namespace))
            .join("blobs")
            .join(blocked_id)
            .exists());

        for (key, size, _, _) in [&uploads[1], &uploads[2]] {
            let finalize = route_fixture(
                &mut ctx,
                fixture_json_request(
                    hyper::Method::POST,
                    "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                    json!({"key": key, "version": "v1", "size_bytes": size}),
                ),
            )
            .await;
            assert_eq!(finalize.status(), StatusCode::OK);
            let (_, _, body) = response_bytes(finalize).await;
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["ok"], true);
        }
        assert_eq!(
            ctx.service
                .staged_upload_usage(Some(&namespace), None)
                .unwrap(),
            (0, 0),
            "all finalized blobs are excluded from staged accounting"
        );
    }

    #[tokio::test]
    async fn signed_download_reports_azure_blob_not_found_after_eviction() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));
        let key = "signed-download-evicted";
        let version = "v1";
        let id = entry_hash(key, version);
        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        let upload_url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
        let contents = Bytes::from_static(b"available before eviction");
        let mut put = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", upload_url.path(), upload_url.query().unwrap()),
            contents.clone(),
        );
        put.headers_mut()
            .insert(CONTENT_LENGTH, contents.len().to_string().parse().unwrap());
        put.headers_mut().insert(
            "x-ms-blob-type",
            "BlockBlob".parse().expect("valid blob type"),
        );
        assert_eq!(
            route_fixture(&mut ctx, put).await.status(),
            StatusCode::CREATED
        );

        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": key, "version": version, "size_bytes": contents.len()}),
            ),
        )
        .await;
        assert_eq!(finalize.status(), StatusCode::OK);

        let lookup = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/GetCacheEntryDownloadURL",
                json!({"key": key, "version": version, "restore_keys": []}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(lookup).await;
        let lookup: Value = serde_json::from_slice(&body).unwrap();
        let download_url =
            url::Url::parse(lookup["signed_download_url"].as_str().unwrap()).unwrap();
        let signed_path = format!(
            "{}?{}",
            download_url.path(),
            download_url.query().expect("signed download URL query")
        );

        let namespace = cache_namespace("client-fixture-token");
        std::fs::remove_file(ctx.service.entry_path(&id, Some(&namespace))).unwrap();
        std::fs::remove_file(
            ctx.service
                .tenant_root(Some(&namespace))
                .join("blobs")
                .join(&id),
        )
        .unwrap();

        for method in [hyper::Method::GET, hyper::Method::HEAD] {
            let request = capability_request(method, &signed_path, Bytes::new());
            let (status, headers, _) = response_bytes(route_fixture(&mut ctx, request).await).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(headers.get("x-ms-error-code").unwrap(), "BlobNotFound");
        }
    }

    #[tokio::test]
    async fn capability_routes_reject_missing_wrong_method_resource_and_expired_tokens() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("capability-auth", "v1");
        let get_sig = service.create_read_capability("tenant", &id, 1).unwrap();
        let mut ctx = test_ctx(service);

        let missing = capability_request(
            hyper::Method::GET,
            &format!("/_results/download/{id}"),
            Bytes::new(),
        );
        assert_eq!(
            route_fixture(&mut ctx, missing).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong_method = capability_request(
            hyper::Method::HEAD,
            &format!("/_results/download/{id}?sig={get_sig}"),
            Bytes::new(),
        );
        assert_eq!(
            route_fixture(&mut ctx, wrong_method).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong_resource = capability_request(
            hyper::Method::GET,
            &format!(
                "/_results/download/{}?sig={get_sig}",
                entry_hash("different-resource", "v1")
            ),
            Bytes::new(),
        );
        assert_eq!(
            route_fixture(&mut ctx, wrong_resource).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let expired_sig = ctx
            .service
            .create_read_capability_until(
                "tenant",
                &id,
                1,
                now_unix_millis().unwrap().saturating_sub(1),
            )
            .unwrap();
        let expired = capability_request(
            hyper::Method::GET,
            &format!("/_results/download/{id}?sig={expired_sig}"),
            Bytes::new(),
        );
        assert_eq!(
            route_fixture(&mut ctx, expired).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let ordinary_route = capability_request(
            hyper::Method::POST,
            "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
            Bytes::from_static(br#"{"key":"no-bearer","version":"v1"}"#),
        );
        assert_eq!(
            route_fixture(&mut ctx, ordinary_route).await.status(),
            StatusCode::UNAUTHORIZED,
            "only signed transfer URLs bypass Runtime bearer authentication"
        );

        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": "expired-upload-capability", "version": "v1"}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        let upload_url = url::Url::parse(created["signed_upload_url"].as_str().unwrap()).unwrap();
        let upload_id = v2_upload_id(upload_url.path()).unwrap();
        let namespace = upload_url
            .query_pairs()
            .find_map(|(name, value)| (name == "scope").then(|| value.into_owned()))
            .unwrap();
        let mut reservation = read_reservation_record(&ctx.service, upload_id, Some(&namespace))
            .unwrap()
            .unwrap()
            .0;
        reservation["updatedMs"] = json!(0);
        replace_json_atomically(
            &ctx.service.reservation_path(upload_id, Some(&namespace)),
            &reservation,
        )
        .unwrap();
        let expired_upload = capability_request(
            hyper::Method::PUT,
            &format!("{}?{}", upload_url.path(), upload_url.query().unwrap()),
            Bytes::from_static(b"expired"),
        );
        assert_eq!(
            route_fixture(&mut ctx, expired_upload).await.status(),
            StatusCode::UNAUTHORIZED,
            "V2 upload claims expire with their reservation"
        );
    }

    #[tokio::test]
    async fn v2_finalize_preserves_a_live_upload_lease_and_reconciles_a_crash_cut() {
        let dir = tempfile_dir();
        let mut ctx = test_ctx(test_service(dir.path()));
        let key = "v2-live-finalize";
        let version = "v1";
        let create = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry",
                json!({"key": key, "version": version}),
            ),
        )
        .await;
        let (_, _, body) = response_bytes(create).await;
        let created: Value = serde_json::from_slice(&body).unwrap();
        let nonce = created["signed_upload_url"]
            .as_str()
            .and_then(|raw| url::Url::parse(raw).ok())
            .and_then(|url| {
                url.query_pairs()
                    .find(|(name, _)| name == "sig")
                    .map(|(_, value)| value.into_owned())
            })
            .unwrap();
        let id = entry_hash(key, version);
        let attempt = uuid::Uuid::new_v4().simple().to_string();
        let mut temp = {
            let _lock = lock_cache_entry(&ctx.service, &id, "tenant").await.unwrap();
            let temp = create_upload_temp(&ctx.service, &id, Some("tenant"), &attempt).unwrap();
            begin_v2_upload_attempt(&ctx.service, &id, "tenant", &nonce, &attempt).unwrap();
            temp
        };
        let temp_path = temp.path.clone();
        temp.writer.write_all(b"abc").await.unwrap();
        temp.writer.flush().await.unwrap();
        temp.writer.sync_all().await.unwrap();
        let blob_path = ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(&id);
        std::fs::hard_link(&temp_path, &blob_path).unwrap();

        let finalize = route_fixture(
            &mut ctx,
            fixture_json_request(
                hyper::Method::POST,
                "/twirp/github.actions.results.api.v1.CacheService/FinalizeCacheEntryUpload",
                json!({"key": key, "version": version, "size_bytes": 3}),
            ),
        )
        .await;
        let (status, _, body) = response_bytes(finalize).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["ok"], false);
        assert!(
            temp_path.exists(),
            "a live temp lease must remain untouched"
        );
        assert!(ctx.service.reservation_path(&id, Some("tenant")).exists());

        // Model a crash after canonical blob publication: the lease unlocks,
        // while both hard links and the active marker remain on disk.
        let UploadTempWriter {
            path: abandoned_path,
            cleanup,
            _active_lock,
            writer,
        } = temp;
        std::mem::forget(cleanup); // A crashed process cannot run temp cleanup.
        drop(writer);
        drop(_active_lock);
        assert!(abandoned_path.exists(), "crash cut leaves its temp link");
        let retry = finalize_v2(
            post_v2(json!({"key": key, "version": version, "size_bytes": 3})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(retry["ok"], true);
        assert!(
            !abandoned_path.exists(),
            "retry reaps the unlocked attempt temp"
        );
        assert!(!ctx.service.reservation_path(&id, Some("tenant")).exists());
        assert_eq!(std::fs::read(blob_path).unwrap(), b"abc");
    }

    #[test]
    fn azure_block_list_rejects_noncanonical_or_unbounded_xml() {
        let block_id = base64::engine::general_purpose::STANDARD.encode(b"block-0001");
        assert!(parse_azure_block_list(
            format!("<BlockList><Latest>{block_id}</Latest></BlockList>").as_bytes()
        )
        .is_ok());
        for malformed in [
            "<BlockList></BlockList>",
            "<BlockList><Uncommitted>eA==</Uncommitted></BlockList>",
            "<BlockList><Latest>&amp;</Latest></BlockList>",
            "<BlockList><Latest>not-base64!</Latest></BlockList>",
            "<BlockList><Latest>eA==</Latest></BlockList><BlockList>",
        ] {
            assert!(
                parse_azure_block_list(malformed.as_bytes()).is_err(),
                "accepted malformed Azure block list: {malformed}"
            );
        }
    }

    fn post_json(body: Value) -> Request<Full<Bytes>> {
        Request::builder()
            .method(hyper::Method::POST)
            .uri("http://cache.test/_apis/artifactcache/cache/reserve")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("build request")
    }

    fn put_body(body: &'static [u8]) -> Request<Full<Bytes>> {
        Request::builder()
            .method(hyper::Method::PUT)
            .uri("http://cache.test/_apis/artifactcache/cache/upload")
            .body(Full::new(Bytes::from_static(body)))
            .expect("build request")
    }

    fn get_at_host(host: &str, query: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .uri(format!("http://{host}/_apis/artifactcache/cache?{query}"))
            .body(Full::new(Bytes::new()))
            .expect("build request")
    }

    fn get(query: &str) -> Request<Full<Bytes>> {
        get_at_host("cache.test", query)
    }

    #[test]
    fn public_base_is_normalized_and_request_host_is_ignored() {
        assert_eq!(
            normalize_public_base("https://cache.test///").unwrap(),
            "https://cache.test"
        );

        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let blob = service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(entry_hash("linux-rust-2026", "v1"));
        std::fs::write(&blob, b"abc").unwrap();
        commit_entry(&service, "linux-rust-2026", "v1", 3, Some("tenant")).unwrap();
        let ctx = Ctx {
            service: Arc::new(service),
            public_base: normalize_public_base("https://cache.test///").unwrap(),
        };

        let hit = lookup_v1(
            &get_at_host("attacker.test", "keys=linux-rust-2026&version=v1"),
            &ctx,
            &["tenant"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            hit["archiveLocation"],
            json!(format!(
                "https://cache.test/_results/download/{}",
                entry_hash("linux-rust-2026", "v1")
            ))
        );
    }

    #[test]
    fn public_base_rejects_ambiguous_or_unsafe_values() {
        for raw in [
            "",
            " ",
            "cache.test:17933",
            "ftp://cache.test",
            "http:///cache",
            "http://user:password@cache.test",
            "http://cache.test?tenant=untrusted",
            "http://cache.test#fragment",
        ] {
            assert!(normalize_public_base(raw).is_err(), "accepted {raw:?}");
        }
    }

    #[test]
    fn v2_upload_route_requires_exact_suffix_and_cache_id() {
        let id = entry_hash("route", "v1");
        assert_eq!(
            v2_upload_id(&format!("/_results/upload/{id}")),
            Some(id.as_str())
        );
        assert_eq!(
            v2_upload_id(&format!("/cache/_results/upload/{id}")),
            Some(id.as_str())
        );
        assert!(v2_upload_id("/_results/upload/not-a-cache-id").is_none());
        assert!(v2_upload_id(&format!("/_results/upload/{id}/extra")).is_none());
        assert!(v2_upload_id(&format!("/_results/not-upload/{id}")).is_none());
        assert!(v2_upload_id(&format!("/not-results/upload/{id}")).is_none());
    }

    #[test]
    fn query_param_reads_every_parameter_not_only_the_first() {
        // `?keys=…&version=…` is exactly the shape actions/toolkit sends
        // (cacheHttpClient.ts `getCacheEntry`). Reading only the first pair
        // returned an empty version, and version participates in the entry
        // hash, so every v1 lookup missed.
        let req = get("keys=linux-rust&version=abc123");
        assert_eq!(query_param(&req, "keys").as_deref(), Some("linux-rust"));
        assert_eq!(query_param(&req, "version").as_deref(), Some("abc123"));
        assert_eq!(query_param(&req, "absent"), None);
    }

    #[test]
    fn query_param_handles_repeats_valueless_pairs_and_encoding() {
        let req = get("flag&version=a%2Fb%20c&version=second&plus=a+b");
        // First occurrence wins for a repeated name.
        assert_eq!(query_param(&req, "version").as_deref(), Some("a/b c"));
        assert_eq!(query_param(&req, "flag").as_deref(), Some(""));
        // encodeURIComponent never emits `+` for a space, so `+` stays literal.
        assert_eq!(query_param(&req, "plus").as_deref(), Some("a+b"));
        // A stray `%` that is not a valid escape is preserved verbatim.
        assert_eq!(
            query_param(&get("version=100%25%zz"), "version").as_deref(),
            Some("100%%zz")
        );
    }

    #[test]
    fn keys_from_query_splits_the_percent_encoded_comma_joined_list() {
        // encodeURIComponent(keys.join(',')) escapes the separators as %2C, so
        // the value must be decoded before it is split.
        let req = get("keys=primary%2Crestore-one%2Crestore-two&version=v1");
        assert_eq!(
            keys_from_query(&req),
            vec!["primary", "restore-one", "restore-two"]
        );
    }

    #[tokio::test]
    async fn v1_reserve_put_publishes_lookup_hit_and_clears_reservation() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "v1-rust-cache";
        let version = "v1";
        let contents = b"abc";

        let reserved = reserve(
            post_json(json!({
                "key": key,
                "version": version,
                "cacheSize": contents.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap();
        assert!(ctx.service.reservation_path(id, Some("tenant")).exists());

        // A fresh service instance must recover the durable reservation.
        let reopened = test_ctx(test_service(dir.path()));
        assert_eq!(
            load_v1_reservation(&reopened.service, id, "tenant").unwrap(),
            V1Reservation {
                key: key.to_owned(),
                version: version.to_owned(),
                cache_id: reserved["cacheID"].as_u64().unwrap(),
                expected_size: Some(contents.len() as u64),
                chunks: Vec::new(),
                commit_attempt: None,
                active_uploads: Vec::new(),
            }
        );

        upload(put_body(contents), &reopened, id, "tenant", true, None)
            .await
            .unwrap();

        assert!(!reopened
            .service
            .reservation_path(id, Some("tenant"))
            .exists());
        let hit = lookup_v1(
            &get("keys=v1-rust-cache&version=v1"),
            &reopened,
            &["tenant"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit["cacheKey"], json!(key));
        assert_eq!(hit["cacheVersion"], json!(version));
    }

    #[tokio::test]
    async fn v1_duplicate_distinct_uploads_do_not_overwrite_the_winner() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "v1-race";
        let version = "v1";
        let first_body = b"first-body";
        let second_body = b"other-body";
        let reserved = reserve(
            post_json(json!({
                "key": key,
                "version": version,
                "cacheSize": first_body.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap();

        let first_upload = upload(put_body(first_body), &ctx, id, "tenant", true, None);
        let second_upload = upload(put_body(second_body), &ctx, id, "tenant", true, None);
        let (first_result, second_result) = tokio::join!(first_upload, second_upload);

        assert_ne!(first_result.is_ok(), second_result.is_ok());
        let winner = if first_result.is_ok() {
            first_body
        } else {
            second_body
        };
        let blob = ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(id);
        assert_eq!(std::fs::read(blob).unwrap(), winner);
        assert!(ctx.service.entry_path(id, Some("tenant")).exists());
    }

    #[tokio::test]
    async fn v1_retry_clears_matching_stale_reservation_after_entry_publication() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "v1-retry";
        let version = "v1";
        let contents = b"retry-body";
        let reserved = reserve(
            post_json(json!({
                "key": key,
                "version": version,
                "cacheSize": contents.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap();
        let reservation = V1Reservation {
            key: key.to_owned(),
            version: version.to_owned(),
            cache_id: reserved["cacheID"].as_u64().unwrap(),
            expected_size: Some(contents.len() as u64),
            chunks: Vec::new(),
            commit_attempt: None,
            active_uploads: Vec::new(),
        };

        upload(put_body(contents), &ctx, id, "tenant", true, None)
            .await
            .unwrap();
        persist_v1_reservation(&ctx.service, id, &reservation, "tenant").unwrap();
        assert!(ctx.service.reservation_path(id, Some("tenant")).exists());

        upload(put_body(contents), &ctx, id, "tenant", true, None)
            .await
            .unwrap();

        assert!(!ctx.service.reservation_path(id, Some("tenant")).exists());
        assert!(ctx.service.entry_path(id, Some("tenant")).exists());
    }

    #[tokio::test]
    async fn v1_reserve_waits_for_v2_finalize_and_rechecks_the_committed_entry() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "cross-protocol-finalize-race";
        let version = "v1";
        let v2_claim = reserve_v2(
            post_json(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(v2_claim["ok"], json!(true));
        let id = entry_hash(key, version);
        let nonce = v2_upload_nonce(&v2_claim);

        // This lock represents the v2 finalizer's exclusive critical section.
        // The v1 reserve must wait, then observe the entry it publishes.
        let finalizer_lock = try_lock_cache_entry_at(&ctx.service.root, Some("tenant"), &id)
            .unwrap()
            .expect("entry shard is free");
        let reserve_ctx = Ctx {
            service: Arc::clone(&ctx.service),
            public_base: ctx.public_base.clone(),
        };
        let mut v1_reserve = tokio::spawn(async move {
            reserve(
                post_json(json!({
                    "key": key,
                    "version": version,
                    "cacheSize": 8,
                })),
                &reserve_ctx,
                "tenant",
            )
            .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut v1_reserve)
                .await
                .is_err(),
            "v1 reservation must wait on the v2 entry shard"
        );

        let blob = ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(&id);
        std::fs::write(&blob, b"v2 bytes").unwrap();
        assert!(
            commit_entry_without_overwrite(&ctx.service, key, version, 8, Some("tenant"),).unwrap()
        );
        clear_v2_reservation(&ctx.service, &id, "tenant", &nonce).unwrap();
        drop(finalizer_lock);

        let result = v1_reserve.await.unwrap().unwrap_err();
        assert!(result.downcast_ref::<CacheKeyConflict>().is_some());
        assert!(!ctx.service.reservation_path(&id, Some("tenant")).exists());
        assert!(ctx.service.entry_path(&id, Some("tenant")).exists());
    }

    #[tokio::test]
    async fn reserve_endpoints_reject_a_foreign_protocol_claim_without_overwriting_it() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);

        let v2_claim = reserve_v2(
            post_json(json!({"key": "v2-claimed", "version": "v1"})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let v2_id = entry_hash("v2-claimed", "v1");
        let v1_conflict = reserve(
            post_json(json!({
                "key": "v2-claimed",
                "version": "v1",
                "cacheSize": 4,
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap_err();
        assert!(v1_conflict
            .downcast_ref::<CacheProtocolConflict>()
            .is_some());
        assert_eq!(
            read_v2_reservation(&ctx.service, &v2_id, "tenant")
                .unwrap()
                .unwrap()
                .upload_nonce,
            v2_upload_nonce(&v2_claim)
        );

        let v1_claim = reserve(
            post_json(json!({
                "key": "v1-claimed",
                "version": "v1",
                "cacheSize": 4,
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let v1_id = v1_claim["cacheId"].as_str().unwrap();
        let v2_conflict = reserve_v2(
            post_json(json!({"key": "v1-claimed", "version": "v1"})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(v2_conflict["ok"], json!(false));
        assert!(read_v1_reservation(&ctx.service, v1_id, "tenant")
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn v1_upload_rejects_missing_reservation() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let id = entry_hash("missing-reservation", "v1");

        let error = upload(put_body(b"abc"), &ctx, &id, "tenant", true, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("reservation is missing"));
        assert!(!ctx.service.entry_path(&id, Some("tenant")).exists());
        assert!(!ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(id)
            .exists());
    }

    #[tokio::test]
    async fn v1_upload_size_mismatch_leaves_no_entry_and_keeps_reservation() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "size-mismatch";
        let version = "v1";
        let reserved = reserve(
            post_json(json!({
                "key": key,
                "version": version,
                "cacheSize": 4,
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap();

        let error = upload(put_body(b"abc"), &ctx, id, "tenant", true, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("reserved 4, received 3"));
        assert!(!ctx.service.entry_path(id, Some("tenant")).exists());
        assert!(!ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(id)
            .exists());
        assert!(ctx.service.reservation_path(id, Some("tenant")).exists());
    }

    #[tokio::test]
    async fn v1_upload_rejects_mismatched_reservation() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let id = entry_hash("original", "v1");
        let reservation_path = ctx.service.reservation_path(&id, Some("tenant"));
        std::fs::write(
            reservation_path,
            json!({
                "key": "different",
                "version": "v1",
                "cacheSize": 3,
            })
            .to_string(),
        )
        .unwrap();

        let error = upload(put_body(b"abc"), &ctx, &id, "tenant", true, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("does not match cache id"));
        assert!(!ctx.service.entry_path(&id, Some("tenant")).exists());
        assert!(!ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(id)
            .exists());
    }

    #[test]
    fn v1_lookup_returns_the_toolkit_artifact_cache_entry_shape() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let blob = service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(entry_hash("linux-rust-2026", "v1"));
        std::fs::write(&blob, b"abc").unwrap();
        commit_entry(&service, "linux-rust-2026", "v1", 3, Some("tenant")).unwrap();
        let ctx = test_ctx(service);

        let hit = lookup_v1(
            &get("keys=linux-rust-2026%2Clinux-rust&version=v1"),
            &ctx,
            &["tenant"],
        )
        .expect("lookup")
        .expect("hit");
        // actions/toolkit packages/cache/src/internal/contracts.ts
        // `ArtifactCacheEntry`; cacheHttpClient.ts reads `archiveLocation`.
        assert_eq!(
            hit["archiveLocation"],
            json!(format!(
                "http://cache.test/_results/download/{}",
                entry_hash("linux-rust-2026", "v1")
            ))
        );
        assert_eq!(hit["cacheKey"], json!("linux-rust-2026"));
        assert_eq!(hit["cacheVersion"], json!("v1"));

        // A restore-key hit reports the stored key that matched.
        let hit = lookup_v1(
            &get("keys=linux-rust-2027%2Clinux&version=v1"),
            &ctx,
            &["tenant"],
        )
        .expect("lookup")
        .expect("hit");
        assert_eq!(hit["cacheKey"], json!("linux-rust-2026"));

        // A miss carries no entry; the route turns that into HTTP 204, which is
        // the only status `getCacheEntry` treats as "no cache".
        assert!(lookup_v1(&get("keys=absent&version=v1"), &ctx, &["tenant"])
            .expect("lookup")
            .is_none());
    }

    #[tokio::test]
    async fn v2_lookup_reports_the_matched_key() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let blob = service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(entry_hash("linux-rust-2026", "v1"));
        std::fs::write(&blob, b"abc").unwrap();
        commit_entry(&service, "linux-rust-2026", "v1", 3, Some("tenant")).unwrap();
        let mut ctx = test_ctx(service);

        let request = |body: Value| {
            Request::builder()
                .method(hyper::Method::POST)
                .uri("http://cache.test/twirp/github.actions.results.api.v1.CacheService/GetCacheEntryDownloadURL")
                .body(Full::new(Bytes::from(body.to_string())))
                .expect("build request")
        };

        // Exact hit: matched_key equals the requested primary key, so
        // actions/cache reports a cache hit rather than a restore-key hit.
        let exact = lookup_v2(
            request(json!({"key": "linux-rust-2026", "version": "v1"})),
            &mut ctx,
            &["tenant"],
        )
        .await
        .expect("lookup");
        assert_eq!(exact["ok"], json!(true));
        assert_eq!(exact["matchedKey"], json!("linux-rust-2026"));

        // Restore-key hit: matched_key is the stored key, not the primary.
        let restored = lookup_v2(
            request(json!({"key": "linux-rust-2027", "version": "v1", "restoreKeys": ["linux"]})),
            &mut ctx,
            &["tenant"],
        )
        .await
        .expect("lookup");
        assert_eq!(restored["matchedKey"], json!("linux-rust-2026"));

        let miss = lookup_v2(
            request(json!({"key": "absent", "version": "v1"})),
            &mut ctx,
            &["tenant"],
        )
        .await
        .expect("lookup");
        assert_eq!(miss["ok"], json!(false));
    }

    #[tokio::test]
    async fn budget_evicts_oldest_first() {
        let dir = tempfile_dir();
        let mut svc = CacheService::open(dir.path().to_path_buf()).expect("open");
        svc.budget_bytes = 10;
        commit_blob(&svc, "old", "v", b"123456");
        std::thread::sleep(std::time::Duration::from_millis(5));
        commit_blob(&svc, "new", "v", b"123456");
        svc.enforce_budget(None).await.unwrap();
        assert!(
            svc.lookup(&["old"], "v", None).unwrap().is_none(),
            "oldest evicted"
        );
        assert!(
            svc.lookup(&["new"], "v", None).unwrap().is_some(),
            "newest retained"
        );
    }

    #[tokio::test]
    async fn upload_rejects_actual_bytes_over_limit_and_removes_temporary_file() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("oversize", "v");

        let error = store_upload(
            Full::new(Bytes::from_static(b"12345")),
            &service,
            &id,
            UploadLimits {
                declared_size: None,
                expected_size: None,
                max_bytes: 4,
                body_idle_timeout: None,
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("actual cache upload size exceeds"));
        assert_eq!(
            std::fs::read_dir(service.root.join("blobs"))
                .expect("read blobs")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn upload_removes_temporary_file_when_request_body_fails() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("interrupted", "v");
        let frames: Vec<Result<Frame<Bytes>, io::Error>> = vec![
            Ok(Frame::data(Bytes::from_static(b"partial"))),
            Err(io::Error::other("injected body failure")),
        ];

        let error = store_upload(
            StreamBody::new(stream::iter(frames)),
            &service,
            &id,
            UploadLimits {
                declared_size: None,
                expected_size: None,
                max_bytes: MAX_BODY,
                body_idle_timeout: None,
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("injected body failure"));
        assert_eq!(
            std::fs::read_dir(service.root.join("blobs"))
                .expect("read blobs")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn upload_idle_timeout_removes_temporary_file() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("stalled-upload", "v");
        let body = StreamBody::new(stream::pending::<Result<Frame<Bytes>, io::Error>>());

        let error = store_upload(
            body,
            &service,
            &id,
            UploadLimits {
                declared_size: None,
                expected_size: None,
                max_bytes: MAX_BODY,
                body_idle_timeout: Some(std::time::Duration::from_millis(1)),
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("body stalled"));
        assert_eq!(
            std::fs::read_dir(service.root.join("blobs"))
                .expect("read blobs")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn upload_rejects_declared_size_mismatch() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("declared", "v");

        let error = store_upload(
            Full::new(Bytes::from_static(b"12345")),
            &service,
            &id,
            UploadLimits {
                declared_size: Some(4),
                expected_size: None,
                max_bytes: MAX_BODY,
                body_idle_timeout: None,
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("declared 4, received 5"));
        assert_eq!(
            std::fs::read_dir(service.root.join("blobs"))
                .expect("read blobs")
                .count(),
            0
        );
    }

    #[test]
    fn finalize_requires_an_uploaded_blob() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());

        let error = commit_entry(&service, "missing", "v", 0, None).unwrap_err();

        assert!(error.to_string().contains("stat uploaded cache blob"));
    }

    #[test]
    fn finalize_rejects_size_that_differs_from_uploaded_blob() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        write_blob(&service, "mismatch", "v", b"12345");

        let error = commit_entry(&service, "mismatch", "v", 4, None).unwrap_err();

        assert!(error.to_string().contains("declared 4, received 5"));
        assert!(!service
            .entry_path(&entry_hash("mismatch", "v"), None)
            .exists());
    }

    #[test]
    fn declared_size_accepts_v2_and_existing_field_names() {
        assert_eq!(declared_cache_size(&json!({"size_bytes": 7})).unwrap(), 7);
        assert_eq!(declared_cache_size(&json!({"sizeBytes": "8"})).unwrap(), 8);
        assert_eq!(declared_cache_size(&json!({"size": 9})).unwrap(), 9);
    }

    #[tokio::test]
    async fn download_streams_file_in_bounded_chunks() {
        let dir = tempfile_dir();
        let contents = vec![0x5a; TRANSFER_CHUNK_BYTES * 2 + 17];
        let mut service = test_service(dir.path());
        service.budget_bytes = u64::try_from(contents.len() * 2).unwrap();
        commit_blob(&service, "download", "v", &contents);
        let hash = entry_hash("download", "v");

        let (mut body, size) = download(&service, &hash, None).await.unwrap();
        let mut received = 0usize;
        while let Some(frame) = body.frame().await {
            let bytes = frame.unwrap().into_data().unwrap();
            assert!(bytes.len() <= TRANSFER_CHUNK_BYTES);
            assert!(bytes.iter().all(|byte| *byte == 0x5a));
            received += bytes.len();
        }

        assert_eq!(size, u64::try_from(contents.len()).unwrap());
        assert_eq!(received, contents.len());
    }

    #[tokio::test]
    async fn json_control_body_limit_counts_actual_bytes() {
        let error = collect_json(Full::new(Bytes::from_static(br#"{"key":"value"}"#)), 4)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("length limit exceeded"));
    }

    #[test]
    fn repo_namespace_is_stable_scoped_and_redacted() {
        let first = repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main");
        assert_eq!(
            first,
            repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main")
        );
        assert_ne!(
            first,
            repo_namespace(&test_repository_key("acme/other"), "refs/heads/main")
        );
        assert_ne!(
            first,
            repo_namespace(&test_repository_key("acme/repo"), "refs/heads/feature")
        );
        assert!(first.starts_with("repo-"));
        assert_eq!(first.len(), "repo-".len() + 64);
        assert!(!first.contains("acme"));
        assert!(!first.contains("main"));
    }

    #[test]
    fn identity_validation_normalizes_refs_and_rejects_invalid_repository_keys() {
        let identity = CacheIdentity::test_identity(
            "Acme/Repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        assert_eq!(identity.repository_key, test_repository_key("acme/repo"));
        assert_eq!(identity.git_ref, "refs/pull/7/merge");
        // A short base branch name normalizes to the full ref form.
        assert_eq!(identity.base_ref.as_deref(), Some("refs/heads/main"));

        // A base that adds no scope beyond the ref is dropped.
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/main",
            Some("refs/heads/main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        assert_eq!(identity.base_ref, None);

        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            None,
            TrustClass::Unknown,
        )
        .unwrap();
        assert_eq!(identity.base_ref, None);

        for repository_key in ["", "acme/repo", "repo-key-v1-not-hex"] {
            assert!(
                CacheIdentity::from_repository_key(
                    repository_key,
                    "refs/heads/main",
                    None,
                    TrustClass::Trusted,
                )
                .is_err(),
                "accepted repository key {repository_key:?}"
            );
        }
        for (server_url, repository_id) in [
            ("https://github.com", ""),
            ("https://github.com", "0"),
            ("https://github.com", "not-a-number"),
            ("https://github.example.com/path", "12"),
            ("not a url", "12"),
        ] {
            assert!(
                CacheIdentity::for_repository(
                    server_url,
                    repository_id,
                    "refs/heads/main",
                    None,
                    TrustClass::Trusted,
                )
                .is_err(),
                "accepted server/repository ID {server_url:?}/{repository_id:?}"
            );
        }
        for git_ref in ["", "refs/heads/\rx", &"r".repeat(600)] {
            assert!(
                CacheIdentity::test_identity("acme/repo", git_ref, None, TrustClass::Trusted)
                    .is_err(),
                "accepted ref {git_ref:?}"
            );
        }
        assert!(CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/main",
            Some("ba\rse"),
            TrustClass::Trusted
        )
        .is_err());
    }

    #[test]
    fn canonical_repository_identity_uses_server_origin_and_numeric_id() {
        let github = CacheIdentity::for_repository(
            "https://github.com",
            "123",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        let renamed_or_case_changed = CacheIdentity::for_repository(
            "HTTPS://GITHUB.COM/",
            "000123",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        let ghes = CacheIdentity::for_repository(
            "https://github.example.com",
            "123",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        assert_eq!(github, renamed_or_case_changed);
        assert_ne!(github, ghes);
        assert_eq!(github.namespaces(), renamed_or_case_changed.namespaces());
        assert_ne!(github.namespaces(), ghes.namespaces());
    }

    #[test]
    fn register_resolve_round_trip_serves_ref_then_base() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "job-token", &identity).unwrap();
        // Re-registration is idempotent.
        register_job_cache_session(dir.path(), "job-token", &identity).unwrap();

        let chain = service.resolve_namespaces("job-token");
        assert_eq!(
            chain,
            vec![
                repo_namespace(&test_repository_key("acme/repo"), "refs/pull/7/merge"),
                repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main"),
            ]
        );
        assert_eq!(resolve_job_cache_namespaces(dir.path(), "job-token"), chain);
        // A different job token in the same repo resolves to the same shared
        // namespaces once registered: this is the cross-run hit.
        let other = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "other-token", &other).unwrap();
        assert_eq!(service.resolve_namespaces("other-token"), chain);
    }

    #[test]
    fn conflicting_token_rebind_poison_resolves_to_the_token_hash() {
        let dir = tempfile_dir();
        let first =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        let conflicting = CacheIdentity::test_identity(
            "acme/other",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "reused-token", &first).unwrap();
        let original_chain = resolve_job_cache_namespaces(dir.path(), "reused-token");
        assert_eq!(original_chain, first.namespaces());

        assert!(register_job_cache_session(dir.path(), "reused-token", &conflicting).is_err());
        let fallback = job_cache_fallback_namespace("reused-token");
        let sessions = dir.path().join("sessions");
        assert!(sessions.join(format!("{fallback}.conflict")).exists());
        assert!(!sessions.join(format!("{fallback}.json")).exists());
        assert_eq!(
            resolve_job_cache_namespaces(dir.path(), "reused-token"),
            vec![fallback.clone()]
        );
        assert!(register_job_cache_session(dir.path(), "reused-token", &first).is_err());
        assert_eq!(
            job_cache_fallback_namespace("reused-token"),
            cache_namespace("reused-token")
        );
    }

    #[test]
    fn invalidating_a_missing_identity_removes_the_previous_binding() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        register_job_cache_session(dir.path(), "reused-token", &identity).unwrap();
        assert_eq!(
            resolve_job_cache_namespaces(dir.path(), "reused-token"),
            identity.namespaces()
        );

        invalidate_job_cache_session(dir.path(), "reused-token").unwrap();
        let fallback = job_cache_fallback_namespace("reused-token");
        assert_eq!(
            resolve_job_cache_namespaces(dir.path(), "reused-token"),
            vec![fallback.clone()]
        );
        assert_eq!(service.resolve_namespaces("reused-token"), vec![fallback]);
        assert!(register_job_cache_session(dir.path(), "reused-token", &identity).is_err());
    }

    #[test]
    fn unregistered_token_falls_back_isolated_with_exactly_one_forensic_line() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        let logs = dir.path().join("logs");
        service.forensic_log_dir = Some(logs.clone());

        let isolated = cache_namespace("unknown-token");
        assert_eq!(
            resolve_job_cache_namespaces(dir.path(), "unknown-token"),
            vec![isolated.clone()]
        );
        assert_eq!(
            service.resolve_namespaces("unknown-token"),
            vec![isolated.clone()]
        );
        assert!(dir
            .path()
            .join("sessions")
            .join(format!("{isolated}.isolated"))
            .exists());

        // The second request reuses the marker instead of re-logging.
        assert_eq!(
            service.resolve_namespaces("unknown-token"),
            vec![isolated.clone()]
        );
        let log = std::fs::read_to_string(logs.join(crate::slot_log::DAEMON_LOG)).unwrap();
        assert_eq!(log.lines().count(), 1, "expected one forensic line: {log}");
        assert!(log.contains("isolated fallback"));
        assert!(log.contains(&isolated));
        assert!(!log.contains("unknown-token"));
    }

    #[test]
    fn corrupt_session_is_fail_closed_to_isolated() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let token_hash = cache_namespace("job-token");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join(format!("{token_hash}.json")), b"{not json").unwrap();
        assert_eq!(
            resolve_job_cache_namespaces(dir.path(), "job-token"),
            vec![token_hash.clone()]
        );
        assert_eq!(
            service.resolve_namespaces("job-token"),
            vec![token_hash.clone()]
        );
        assert!(sessions.join(format!("{token_hash}.isolated")).exists());
    }

    #[test]
    fn slug_keyed_legacy_session_is_not_read_as_a_canonical_binding() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let token_hash = cache_namespace("legacy-slug-token");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{token_hash}.json")),
            json!({
                "repository": "acme/repo",
                "ref": "refs/heads/main",
                "baseRef": Value::Null,
                "trust": "trusted",
                "registeredMs": 1,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            service.resolve_namespaces("legacy-slug-token"),
            vec![token_hash.clone()]
        );
        assert!(sessions.join(format!("{token_hash}.isolated")).exists());
    }

    #[test]
    fn lookup_searches_the_whole_ref_scope_before_the_base() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/feature",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        let chain = identity.namespaces();
        let (ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str());
        for namespace in [&ref_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        let commit_in = |namespace: &str, key: &str, contents: &[u8]| {
            let blob = service
                .tenant_root(Some(namespace))
                .join("blobs")
                .join(entry_hash(key, "v1"));
            std::fs::write(&blob, contents).unwrap();
            commit_entry(
                &service,
                key,
                "v1",
                u64::try_from(contents.len()).unwrap(),
                Some(namespace),
            )
            .unwrap();
        };
        // Same key in both scopes with different blobs: the ref scope wins.
        commit_in(ref_ns, "shared", b"ref-blob");
        commit_in(base_ns, "shared", b"base-blob");
        // Restore-only entries live in the base scope.
        commit_in(base_ns, "linux-rust-2026", b"base-only");
        // A restore-prefix hit in the ref scope beats an exact primary-key hit
        // in the base scope: scope order is the outer loop, per the GitHub
        // cache scope rules (current branch fully, then the fallback branch).
        commit_in(ref_ns, "prefix-entry", b"ref-prefix");
        commit_in(base_ns, "exact-in-base", b"base-exact");

        let ctx = test_ctx(service);
        let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();

        let hit = lookup_v1(&get("keys=shared&version=v1"), &ctx, &chain_refs)
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("shared"));

        let hit = lookup_v1(
            &get("keys=linux-other%2Clinux&version=v1"),
            &ctx,
            &chain_refs,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit["cacheKey"], json!("linux-rust-2026"));

        let hit = lookup_v1(
            &get("keys=exact-in-base%2Cprefix&version=v1"),
            &ctx,
            &chain_refs,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit["cacheKey"], json!("prefix-entry"));
    }

    #[tokio::test]
    async fn download_serves_the_scope_the_lookup_reported() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/feature",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        let chain = identity.namespaces();
        let (ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str());
        for namespace in [&ref_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
            let blob = service
                .tenant_root(Some(namespace))
                .join("blobs")
                .join(entry_hash("shared", "v1"));
            std::fs::write(&blob, format!("{namespace}-blob").as_bytes()).unwrap();
            commit_entry(
                &service,
                "shared",
                "v1",
                u64::try_from(format!("{namespace}-blob").len()).unwrap(),
                Some(namespace),
            )
            .unwrap();
        }
        let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();
        let (mut body, _) = download_chain(&service, &entry_hash("shared", "v1"), &chain_refs)
            .await
            .unwrap();
        let mut received = Vec::new();
        while let Some(frame) = body.frame().await {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(received, format!("{ref_ns}-blob").as_bytes());

        // An entry evicted from the ref scope still downloads from the base.
        std::fs::remove_file(service.entry_path(&entry_hash("shared", "v1"), Some(ref_ns)))
            .unwrap();
        let (mut body, _) = download_chain(&service, &entry_hash("shared", "v1"), &chain_refs)
            .await
            .unwrap();
        let mut received = Vec::new();
        while let Some(frame) = body.frame().await {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(received, format!("{base_ns}-blob").as_bytes());
    }

    #[tokio::test]
    async fn writes_land_in_the_ref_namespace_only() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        let chain = identity.namespaces();
        let (ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str());
        service.ensure_tenant(ref_ns).unwrap();
        service.ensure_tenant(base_ns).unwrap();
        let ctx = test_ctx(service);
        let contents = b"pr-blob";

        let reserved = reserve(
            post_json(json!({
                "key": "pr-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            ref_ns,
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, ref_ns, true, None)
            .await
            .unwrap();

        assert!(ctx.service.entry_path(&id, Some(ref_ns)).exists());
        assert!(!ctx.service.entry_path(&id, Some(base_ns)).exists());
        // The PR run restores its own write through the chain, and the base
        // scope it read from is untouched by the write.
        let hit = lookup_v1(&get("keys=pr-key&version=v1"), &ctx, &[ref_ns, base_ns])
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("pr-key"));
    }

    #[test]
    fn prune_removes_only_stale_session_files() {
        let dir = tempfile_dir();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let stale_hash = "a".repeat(64);
        std::fs::write(sessions.join(format!("{stale_hash}.json")), b"{}").unwrap();
        std::fs::write(sessions.join(format!("{stale_hash}.isolated")), b"").unwrap();
        std::fs::write(sessions.join(format!("{stale_hash}.conflict")), b"{}").unwrap();
        std::fs::write(sessions.join(".abc.tmp"), b"tmp").unwrap();
        std::fs::write(sessions.join("notes.txt"), b"notes").unwrap();
        std::fs::write(sessions.join("zz.json"), b"{}").unwrap();

        // A `now` before every mtime keeps everything.
        prune_stale_sessions(&sessions, std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 6);

        // A `now` past the TTL removes only the session-shaped files.
        let aged =
            std::time::SystemTime::now() + std::time::Duration::from_secs(SESSION_TTL_SECS + 60);
        prune_stale_sessions(&sessions, aged);
        let remaining: Vec<String> = std::fs::read_dir(&sessions)
            .unwrap()
            .flatten()
            .map(|file| file.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.contains(&".abc.tmp".to_owned()));
        assert!(remaining.contains(&"notes.txt".to_owned()));
        assert!(remaining.contains(&"zz.json".to_owned()));
    }

    #[test]
    fn register_rejects_an_empty_token() {
        let dir = tempfile_dir();
        let identity =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        assert!(register_job_cache_session(dir.path(), "", &identity).is_err());
    }

    fn post_v2(body: Value) -> Request<Full<Bytes>> {
        Request::builder()
            .method(hyper::Method::POST)
            .uri("http://cache.test/twirp/github.actions.results.api.v1.CacheService/GetCacheEntryDownloadURL")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("build request")
    }

    fn commit_in(service: &CacheService, namespace: &str, key: &str, contents: &[u8]) {
        let blob = service
            .tenant_root(Some(namespace))
            .join("blobs")
            .join(entry_hash(key, "v1"));
        std::fs::write(&blob, contents).unwrap();
        commit_entry(
            service,
            key,
            "v1",
            u64::try_from(contents.len()).unwrap(),
            Some(namespace),
        )
        .unwrap();
    }

    #[test]
    fn fork_isolation_conformance_chains_split_by_trust() {
        let trusted = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        assert_eq!(
            trusted.namespaces(),
            vec![
                repo_namespace(&test_repository_key("acme/repo"), "refs/pull/7/merge"),
                repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main"),
            ]
        );

        // ForkPR and Unknown resolve fork-headed: the isolated fork namespace
        // first, then the same base namespaces for read-through.
        for trust in [TrustClass::ForkPR, TrustClass::Unknown] {
            let untrusted =
                CacheIdentity::test_identity("acme/repo", "refs/pull/7/merge", Some("main"), trust)
                    .unwrap();
            assert_eq!(
                untrusted.namespaces(),
                vec![
                    fork_namespace(&test_repository_key("acme/repo"), "refs/pull/7/merge"),
                    repo_namespace(&test_repository_key("acme/repo"), "refs/pull/7/merge"),
                    repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main"),
                ],
                "wrong chain for {trust:?}"
            );
        }
    }

    #[test]
    fn fork_isolation_conformance_fork_namespace_is_stable_scoped_and_redacted() {
        let first = fork_namespace(&test_repository_key("acme/repo"), "refs/heads/main");
        assert_eq!(
            first,
            fork_namespace(&test_repository_key("acme/repo"), "refs/heads/main")
        );
        assert_ne!(
            first,
            fork_namespace(&test_repository_key("acme/other"), "refs/heads/main")
        );
        assert_ne!(
            first,
            fork_namespace(&test_repository_key("acme/repo"), "refs/heads/feature")
        );
        assert!(first.starts_with("fork-"));
        assert_eq!(first.len(), "fork-".len() + 64);
        assert!(!first.contains("acme"));
        assert!(!first.contains("main"));
        // Same inputs, distinct domains: a fork namespace can never name the
        // same tenant as the base namespace, not even by prefix confusion.
        assert_ne!(
            first,
            repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main")
        );
        assert!(
            !repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main")
                .starts_with("fork-"),
            "repo namespaces must never carry the fork prefix"
        );
    }

    #[test]
    fn fork_isolation_conformance_trust_label_round_trips_through_the_session() {
        for trust in [TrustClass::Trusted, TrustClass::ForkPR, TrustClass::Unknown] {
            assert_eq!(parse_trust(trust.as_str()).unwrap(), trust);
            let session = CacheSession {
                identity: CacheIdentity::test_identity(
                    "acme/repo",
                    "refs/pull/7/merge",
                    Some("main"),
                    trust,
                )
                .unwrap(),
                registered_ms: 0,
            };
            let encoded = session.as_json();
            assert_eq!(encoded["trust"], json!(trust.as_str()));
            assert_eq!(CacheSession::parse(encoded).unwrap(), session);
        }
    }

    #[tokio::test]
    async fn fork_isolation_conformance_fork_v1_writes_stay_in_the_fork_namespace() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "fork-token", &identity).unwrap();
        // Resolve through the durable session, exactly as the request path does.
        let chain = service.resolve_namespaces("fork-token");
        assert_eq!(chain.len(), 3);
        let (fork_ns, ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str(), chain[2].as_str());
        for namespace in [&fork_ns, &ref_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        let ctx = test_ctx(service);
        let contents = b"fork-blob";

        let reserved = reserve(
            post_json(json!({
                "key": "fork-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, fork_ns, true, None)
            .await
            .unwrap();

        // The write reached the fork namespace only: no entry and no blob in
        // either base scope.
        assert!(ctx.service.entry_path(&id, Some(fork_ns)).exists());
        for namespace in [ref_ns, base_ns] {
            assert!(
                !ctx.service.entry_path(&id, Some(namespace)).exists(),
                "fork write leaked an entry into {namespace}"
            );
            assert!(
                !ctx.service
                    .tenant_root(Some(namespace))
                    .join("blobs")
                    .join(&id)
                    .exists(),
                "fork write leaked a blob into {namespace}"
            );
        }
        // The fork run restores its own write through its chain.
        let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();
        let hit = lookup_v1(&get("keys=fork-key&version=v1"), &ctx, &chain_refs)
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("fork-key"));

        // A trusted chain over the same base scopes cannot see the fork entry.
        assert!(
            lookup_v1(&get("keys=fork-key&version=v1"), &ctx, &[ref_ns, base_ns])
                .unwrap()
                .is_none(),
            "trusted chain restored a fork-namespace entry"
        );
    }

    #[tokio::test]
    async fn fork_isolation_conformance_fork_v1_reads_through_to_base() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "fork-token", &identity).unwrap();
        let chain = service.resolve_namespaces("fork-token");
        let (fork_ns, ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str(), chain[2].as_str());
        for namespace in [&fork_ns, &ref_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        commit_in(&service, base_ns, "base-key", b"base-blob");
        let ctx = test_ctx(service);

        let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();
        let hit = lookup_v1(&get("keys=base-key&version=v1"), &ctx, &chain_refs)
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("base-key"));

        // The base entry downloads through the fork chain too.
        let (mut body, _) =
            download_chain(&ctx.service, &entry_hash("base-key", "v1"), &chain_refs)
                .await
                .unwrap();
        let mut received = Vec::new();
        while let Some(frame) = body.frame().await {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(received, b"base-blob");
    }

    #[tokio::test]
    async fn fork_isolation_conformance_fork_v2_reserve_through_finalize_stays_isolated() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "fork-token", &identity).unwrap();
        let chain = service.resolve_namespaces("fork-token");
        let (fork_ns, ref_ns, base_ns) = (chain[0].as_str(), chain[1].as_str(), chain[2].as_str());
        for namespace in [&fork_ns, &ref_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        let mut ctx = test_ctx(service);
        let key = "fork-v2-key";
        let version = "v1";
        let contents = b"fork-v2-blob";

        let reserved = reserve_v2(
            post_v2(json!({ "key": key, "version": version })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        assert_eq!(reserved["ok"], json!(true));
        let id = entry_hash(key, version);
        let upload_nonce = v2_upload_nonce(&reserved);
        upload(
            put_body(contents),
            &ctx,
            &id,
            fork_ns,
            false,
            Some(&upload_nonce),
        )
        .await
        .unwrap();
        let finalized = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": contents.len(),
            })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        assert_eq!(finalized["ok"], json!(true));

        assert!(ctx.service.entry_path(&id, Some(fork_ns)).exists());
        for namespace in [ref_ns, base_ns] {
            assert!(
                !ctx.service.entry_path(&id, Some(namespace)).exists(),
                "fork v2 write leaked an entry into {namespace}"
            );
            assert!(
                !ctx.service
                    .tenant_root(Some(namespace))
                    .join("blobs")
                    .join(&id)
                    .exists(),
                "fork v2 write leaked a blob into {namespace}"
            );
        }
        // A second reserve against the fork head now conflicts; the base
        // scopes stay reservable because nothing was written there.
        let again = reserve_v2(
            post_v2(json!({ "key": key, "version": version })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        assert_eq!(again["ok"], json!(false));
        for namespace in [ref_ns, base_ns] {
            let base_reserve = reserve_v2(
                post_v2(json!({ "key": key, "version": version })),
                &ctx,
                namespace,
            )
            .await
            .unwrap();
            assert_eq!(base_reserve["ok"], json!(true), "base scope {namespace}");
        }

        // The fork chain restores the entry over v2; the trusted chain misses.
        let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();
        let hit = lookup_v2(
            post_v2(json!({ "key": key, "version": version })),
            &mut ctx,
            &chain_refs,
        )
        .await
        .unwrap();
        assert_eq!(hit["ok"], json!(true));
        assert_eq!(hit["matchedKey"], json!(key));
        let miss = lookup_v2(
            post_v2(json!({ "key": key, "version": version })),
            &mut ctx,
            &[ref_ns, base_ns],
        )
        .await
        .unwrap();
        assert_eq!(miss, json!({"ok": false}));
    }

    #[tokio::test]
    async fn v2_claim_and_published_bytes_are_immutable_across_replay() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "immutable-v2-key";
        let version = "immutable-v2-version";
        let original = b"original-cache-bytes";
        let replay = b"replaced-cache-bytes";
        assert_eq!(original.len(), replay.len());
        let id = entry_hash(key, version);

        let reservation = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(reservation["ok"], json!(true));
        let nonce = v2_upload_nonce(&reservation);
        let retry = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(retry["ok"], json!(false), "the claim is single-writer");

        upload(put_body(original), &ctx, &id, "tenant", false, Some(&nonce))
            .await
            .unwrap();
        let changed_replay =
            upload(put_body(replay), &ctx, &id, "tenant", false, Some(&nonce)).await;
        assert!(
            changed_replay.is_err(),
            "same claim cannot replace its blob"
        );
        let blob = ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(&id);
        assert_eq!(std::fs::read(&blob).unwrap(), original);

        let finalized = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": original.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(finalized["ok"], json!(true));
        let entry = ctx.service.entry_path(&id, Some("tenant"));
        let published_entry = std::fs::read(&entry).unwrap();
        assert!(!ctx.service.reservation_path(&id, Some("tenant")).exists());

        let replay_reservation = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(replay_reservation["ok"], json!(false));
        let stale_url_upload =
            upload(put_body(replay), &ctx, &id, "tenant", false, Some(&nonce)).await;
        assert!(
            stale_url_upload.is_err(),
            "finalization invalidates old URLs"
        );
        let repeated_finalize = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": original.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(repeated_finalize["ok"], json!(true));

        let mismatched_finalize = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": original.len() + 1,
            })),
            &ctx,
            "tenant",
        )
        .await;
        assert!(
            mismatched_finalize.is_err(),
            "size mismatch must fail closed"
        );

        let (downloaded, size) = download_chain(&ctx.service, &id, &["tenant"])
            .await
            .unwrap();
        assert_eq!(size, original.len() as u64);
        let downloaded = downloaded.collect().await.unwrap().to_bytes();
        assert_eq!(
            &downloaded[..],
            original,
            "replay left published bytes intact"
        );
        assert_eq!(std::fs::read(blob).unwrap(), original);
        assert_eq!(std::fs::read(entry).unwrap(), published_entry);
    }

    #[tokio::test]
    async fn v2_abandoned_claim_recovery_rotates_nonce_and_discards_unpublished_blob() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "abandoned-v2-key";
        let version = "abandoned-v2-version";
        let id = entry_hash(key, version);

        let abandoned = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(abandoned["ok"], json!(true));
        let old_nonce = v2_upload_nonce(&abandoned);

        let claim_path = ctx.service.reservation_path(&id, Some("tenant"));
        let mut stale_claim = std::fs::read(&claim_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
            .expect("read durable reservation");
        stale_claim["updatedMs"] = json!(now_unix_millis().unwrap() - V2_RESERVATION_TTL_MS - 1);
        std::fs::write(&claim_path, stale_claim.to_string()).unwrap();

        let blob = ctx
            .service
            .tenant_root(Some("tenant"))
            .join("blobs")
            .join(&id);
        std::fs::write(&blob, b"abandoned upload").unwrap();

        let recovered = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(recovered["ok"], json!(true));
        let new_nonce = v2_upload_nonce(&recovered);
        assert_ne!(new_nonce, old_nonce, "recovery rotates upload capability");
        assert!(
            !blob.exists(),
            "unpublished bytes from abandoned owner are removed"
        );

        let stale_upload = upload(
            put_body(b"stale capability"),
            &ctx,
            &id,
            "tenant",
            false,
            Some(&old_nonce),
        )
        .await;
        assert!(stale_upload.is_err(), "recovered claim rejects stale URL");

        let replacement = b"replacement bytes";
        upload(
            put_body(replacement),
            &ctx,
            &id,
            "tenant",
            false,
            Some(&new_nonce),
        )
        .await
        .unwrap();
        let finalized = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": replacement.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(finalized["ok"], json!(true));
    }

    #[tokio::test]
    async fn v2_finalize_retry_recovers_entry_published_before_claim_cleanup() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let key = "finalize-retry-key";
        let version = "finalize-retry-version";
        let bytes = b"published before response";
        let id = entry_hash(key, version);

        let reservation = reserve_v2(
            post_v2(json!({"key": key, "version": version})),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        let nonce = v2_upload_nonce(&reservation);
        upload(put_body(bytes), &ctx, &id, "tenant", false, Some(&nonce))
            .await
            .unwrap();

        assert!(commit_entry_without_overwrite(
            &ctx.service,
            key,
            version,
            bytes.len() as u64,
            Some("tenant"),
        )
        .unwrap());
        assert!(ctx.service.reservation_path(&id, Some("tenant")).exists());

        let retry = finalize_v2(
            post_v2(json!({
                "key": key,
                "version": version,
                "size_bytes": bytes.len(),
            })),
            &ctx,
            "tenant",
        )
        .await
        .unwrap();
        assert_eq!(retry["ok"], json!(true));
        assert!(!ctx.service.reservation_path(&id, Some("tenant")).exists());
        assert_eq!(
            std::fs::read(
                ctx.service
                    .tenant_root(Some("tenant"))
                    .join("blobs")
                    .join(id)
            )
            .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn fork_isolation_conformance_trusted_never_reads_fork_namespaces() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let fork_ns = fork_namespace(&test_repository_key("acme/repo"), "refs/heads/main");
        let base_ns = repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main");
        for namespace in [&fork_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        // A fork entry committed directly to the fork namespace.
        commit_in(&service, &fork_ns, "sneaky", b"sneaky-blob");
        let mut ctx = test_ctx(service);

        // The trusted chain over the same repository and ref misses on both
        // wire generations, and the blob is unreachable through it: the
        // download fails rather than serving fork bytes.
        assert!(lookup_v1(&get("keys=sneaky&version=v1"), &ctx, &[&base_ns])
            .unwrap()
            .is_none());
        let miss = lookup_v2(
            post_v2(json!({ "key": "sneaky", "version": "v1" })),
            &mut ctx,
            &[&base_ns],
        )
        .await
        .unwrap();
        assert_eq!(miss, json!({"ok": false}));
        assert!(
            download_chain(&ctx.service, &entry_hash("sneaky", "v1"), &[&base_ns])
                .await
                .is_err()
        );

        // Sanity: the fork chain does restore it.
        let fork_hit = lookup_v1(&get("keys=sneaky&version=v1"), &ctx, &[&fork_ns, &base_ns])
            .unwrap()
            .unwrap();
        assert_eq!(fork_hit["cacheKey"], json!("sneaky"));
    }

    #[tokio::test]
    async fn fork_isolation_regression_fork_write_on_a_base_ref_cannot_poison_base() {
        // The `workflow_run`-from-a-fork shape: a fork-controlled run whose
        // ref *is* the base branch. Without trust in the chain its writes
        // would land in the base scope trusted jobs restore from.
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::ForkPR)
                .unwrap();
        register_job_cache_session(dir.path(), "fork-token", &identity).unwrap();
        let chain = service.resolve_namespaces("fork-token");
        assert_eq!(
            chain,
            vec![
                fork_namespace(&test_repository_key("acme/repo"), "refs/heads/main"),
                repo_namespace(&test_repository_key("acme/repo"), "refs/heads/main"),
            ]
        );
        let (fork_ns, base_ns) = (chain[0].as_str(), chain[1].as_str());
        for namespace in [&fork_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        let ctx = test_ctx(service);
        let contents = b"poison";

        let reserved = reserve(
            post_json(json!({
                "key": "base-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, fork_ns, true, None)
            .await
            .unwrap();

        assert!(ctx.service.entry_path(&id, Some(fork_ns)).exists());
        assert!(
            !ctx.service.entry_path(&id, Some(base_ns)).exists(),
            "fork write poisoned the base-branch scope"
        );
        assert!(
            !ctx.service
                .tenant_root(Some(base_ns))
                .join("blobs")
                .join(&id)
                .exists(),
            "fork write poisoned the base-branch blob"
        );
        // The base scope still restores nothing for the key.
        assert!(
            lookup_v1(&get("keys=base-key&version=v1"), &ctx, &[base_ns])
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn fork_isolation_regression_unknown_writes_like_fork() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Unknown)
                .unwrap();
        register_job_cache_session(dir.path(), "unknown-token", &identity).unwrap();
        let chain = service.resolve_namespaces("unknown-token");
        let (fork_ns, base_ns) = (chain[0].as_str(), chain[1].as_str());
        assert!(fork_ns.starts_with("fork-"));
        for namespace in [&fork_ns, &base_ns] {
            service.ensure_tenant(namespace).unwrap();
        }
        let ctx = test_ctx(service);
        let contents = b"unknown-blob";

        let reserved = reserve(
            post_json(json!({
                "key": "unknown-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            fork_ns,
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, fork_ns, true, None)
            .await
            .unwrap();

        assert!(ctx.service.entry_path(&id, Some(fork_ns)).exists());
        assert!(!ctx.service.entry_path(&id, Some(base_ns)).exists());
    }

    #[test]
    fn fork_isolation_regression_session_without_trust_falls_back_isolated() {
        // Sessions written before trust existed carry no `trust` field; they
        // must not inherit trust — the token falls back to its isolated
        // per-token namespace exactly like a corrupt session.
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let token_hash = cache_namespace("legacy-token");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{token_hash}.json")),
            json!({
                "repositoryKey": test_repository_key("acme/repo"),
                "ref": "refs/heads/main",
                "baseRef": Value::Null,
                "registeredMs": 0,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            service.resolve_namespaces("legacy-token"),
            vec![token_hash.clone()]
        );
        assert!(sessions.join(format!("{token_hash}.isolated")).exists());
    }

    #[test]
    fn fork_isolation_regression_session_with_unusable_trust_label_falls_back_isolated() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        for label in ["superuser", "", "TRUSTED", "fork_pr"] {
            let token = format!("token-{label}");
            let token_hash = cache_namespace(&token);
            let sessions = dir.path().join("sessions");
            std::fs::create_dir_all(&sessions).unwrap();
            std::fs::write(
                sessions.join(format!("{token_hash}.json")),
                json!({
                    "repositoryKey": test_repository_key("acme/repo"),
                    "ref": "refs/heads/main",
                    "baseRef": Value::Null,
                    "trust": label,
                    "registeredMs": 0,
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(
                service.resolve_namespaces(&token),
                vec![token_hash.clone()],
                "unusable trust label {label:?} must fail closed"
            );
            assert!(sessions.join(format!("{token_hash}.isolated")).exists());
        }
    }

    #[test]
    fn fork_isolation_benchmark_namespace_resolution_throughput() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        // `resolve_namespaces` runs on every cache request: one session-file
        // read plus a parse. Bound the hot path in the repo's
        // timing-gated-test style (no criterion/divan harness exists here).
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let trusted =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        let fork = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "trusted-token", &trusted).unwrap();
        register_job_cache_session(dir.path(), "fork-token", &fork).unwrap();
        let trusted_chain = service.resolve_namespaces("trusted-token");
        let fork_chain = service.resolve_namespaces("fork-token");
        assert_eq!(trusted_chain.len(), 1);
        assert_eq!(fork_chain.len(), 3);

        let started = Instant::now();
        for _ in 0..2_000 {
            assert_eq!(
                service.resolve_namespaces(black_box("trusted-token")),
                trusted_chain
            );
            assert_eq!(
                service.resolve_namespaces(black_box("fork-token")),
                fork_chain
            );
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(30),
            "4k session resolutions took {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn cache_trust_regression_cross_job_restore_hit_within_one_repo() {
        // Two jobs, two tokens, one repository: job A saves on the base
        // branch, job B restores from its feature branch through the base
        // scope — the GitHub "current branch, then base branch" rule,
        // end-to-end over the v1 save path and both lookup generations.
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let job_a =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        let job_b = CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/feature",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "job-a-token", &job_a).unwrap();
        register_job_cache_session(dir.path(), "job-b-token", &job_b).unwrap();
        let chain_a = service.resolve_namespaces("job-a-token");
        let chain_b = service.resolve_namespaces("job-b-token");
        assert_eq!(chain_a.len(), 1);
        assert_eq!(chain_b.len(), 2);
        assert_eq!(chain_b[1], chain_a[0]);
        for namespace in chain_a.iter().chain(chain_b.iter()) {
            service.ensure_tenant(namespace).unwrap();
        }
        let mut ctx = test_ctx(service);
        let contents = b"shared-blob";

        // Job A saves through the v1 reserve/upload path into its chain head.
        let reserved = reserve(
            post_json(json!({
                "key": "shared-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            &chain_a[0],
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, &chain_a[0], true, None)
            .await
            .unwrap();

        // Job B restores the entry exactly, over both wire generations, even
        // though its own ref namespace is empty.
        let chain_b_refs: Vec<&str> = chain_b.iter().map(String::as_str).collect();
        let hit = lookup_v1(&get("keys=shared-key&version=v1"), &ctx, &chain_b_refs)
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("shared-key"));
        let hit_v2 = lookup_v2(
            post_v2(json!({ "key": "shared-key", "version": "v1" })),
            &mut ctx,
            &chain_b_refs,
        )
        .await
        .unwrap();
        assert_eq!(hit_v2["ok"], json!(true));
        assert_eq!(hit_v2["matchedKey"], json!("shared-key"));

        // A restore-key prefix also reaches across the job boundary.
        let prefix_hit = lookup_v1(
            &get("keys=shared-key-zzz,shared-&version=v1"),
            &ctx,
            &chain_b_refs,
        )
        .unwrap()
        .unwrap();
        assert_eq!(prefix_hit["cacheKey"], json!("shared-key"));

        // The bytes download through B's chain, and the hit genuinely came
        // from A's scope: B's own ref namespace holds no entry.
        let (mut body, _) = download_chain(&ctx.service, &id, &chain_b_refs)
            .await
            .unwrap();
        let mut received = Vec::new();
        while let Some(frame) = body.frame().await {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(received, contents);
        assert!(
            !ctx.service.entry_path(&id, Some(&chain_b[0])).exists(),
            "job B must restore from the base scope, not its own ref"
        );
    }

    #[tokio::test]
    async fn cache_trust_regression_cross_repo_restore_misses() {
        // Same key, same version, same ref shape — different repositories.
        // Job B must miss on both lookup generations, and its own save of
        // the same key must succeed without seeing job A's entry.
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let job_a = CacheIdentity::test_identity(
            "acme/repo-a",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        let job_b = CacheIdentity::test_identity(
            "acme/repo-b",
            "refs/heads/main",
            None,
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "repo-a-token", &job_a).unwrap();
        register_job_cache_session(dir.path(), "repo-b-token", &job_b).unwrap();
        let chain_a = service.resolve_namespaces("repo-a-token");
        let chain_b = service.resolve_namespaces("repo-b-token");
        assert_ne!(chain_a, chain_b);
        for namespace in chain_a.iter().chain(chain_b.iter()) {
            service.ensure_tenant(namespace).unwrap();
        }
        let mut ctx = test_ctx(service);
        let contents = b"repo-a-blob";

        let reserved = reserve(
            post_json(json!({
                "key": "shared-key",
                "version": "v1",
                "cacheSize": contents.len(),
            })),
            &ctx,
            &chain_a[0],
        )
        .await
        .unwrap();
        let id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(put_body(contents), &ctx, &id, &chain_a[0], true, None)
            .await
            .unwrap();

        let chain_b_refs: Vec<&str> = chain_b.iter().map(String::as_str).collect();
        assert!(
            lookup_v1(&get("keys=shared-key&version=v1"), &ctx, &chain_b_refs)
                .unwrap()
                .is_none(),
            "cross-repo v1 lookup must miss"
        );
        let miss = lookup_v2(
            post_v2(json!({ "key": "shared-key", "version": "v1" })),
            &mut ctx,
            &chain_b_refs,
        )
        .await
        .unwrap();
        assert_eq!(miss, json!({"ok": false}));
        assert!(
            download_chain(&ctx.service, &id, &chain_b_refs)
                .await
                .is_err(),
            "cross-repo download must fail"
        );

        // Job B can save the same key into its own namespace: no conflict
        // with the invisible repo-A entry.
        let own = reserve_v2(
            post_v2(json!({ "key": "shared-key", "version": "v1" })),
            &ctx,
            &chain_b[0],
        )
        .await
        .unwrap();
        assert_eq!(own["ok"], json!(true));
    }

    #[tokio::test]
    async fn cache_trust_regression_fork_pr_read_hit_but_write_isolated() {
        // The full fork-PR exchange between two jobs: the trusted base job
        // saves, the fork job restores it (read-hit through the base scope)
        // and saves its own entry, and the trusted job can never see the
        // fork entry back — over the v1 save path and both lookups.
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let base_job =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        let fork_job = CacheIdentity::test_identity(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "base-token", &base_job).unwrap();
        register_job_cache_session(dir.path(), "fork-token", &fork_job).unwrap();
        let base_chain = service.resolve_namespaces("base-token");
        let fork_chain = service.resolve_namespaces("fork-token");
        assert_eq!(base_chain.len(), 1);
        assert_eq!(fork_chain.len(), 3);
        assert_eq!(fork_chain[2], base_chain[0]);
        for namespace in base_chain.iter().chain(fork_chain.iter()) {
            service.ensure_tenant(namespace).unwrap();
        }
        let mut ctx = test_ctx(service);

        // The trusted job saves the base entry.
        let base_contents = b"base-blob";
        let reserved = reserve(
            post_json(json!({
                "key": "base-key",
                "version": "v1",
                "cacheSize": base_contents.len(),
            })),
            &ctx,
            &base_chain[0],
        )
        .await
        .unwrap();
        let base_id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(
            put_body(base_contents),
            &ctx,
            &base_id,
            &base_chain[0],
            true,
            None,
        )
        .await
        .unwrap();

        // The fork job restores it through its chain and downloads the bytes.
        let fork_refs: Vec<&str> = fork_chain.iter().map(String::as_str).collect();
        let hit = lookup_v1(&get("keys=base-key&version=v1"), &ctx, &fork_refs)
            .unwrap()
            .unwrap();
        assert_eq!(hit["cacheKey"], json!("base-key"));
        let (mut body, _) = download_chain(&ctx.service, &base_id, &fork_refs)
            .await
            .unwrap();
        let mut received = Vec::new();
        while let Some(frame) = body.frame().await {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(received, base_contents);

        // The fork job saves its own entry into its chain head.
        let fork_contents = b"fork-blob";
        let reserved = reserve(
            post_json(json!({
                "key": "fork-key",
                "version": "v1",
                "cacheSize": fork_contents.len(),
            })),
            &ctx,
            &fork_chain[0],
        )
        .await
        .unwrap();
        let fork_id = reserved["cacheId"].as_str().unwrap().to_owned();
        upload(
            put_body(fork_contents),
            &ctx,
            &fork_id,
            &fork_chain[0],
            true,
            None,
        )
        .await
        .unwrap();
        assert!(ctx
            .service
            .entry_path(&fork_id, Some(&fork_chain[0]))
            .exists());

        // The trusted chain misses the fork entry on both generations and
        // cannot download it: the write stayed isolated.
        let base_refs: Vec<&str> = base_chain.iter().map(String::as_str).collect();
        assert!(
            lookup_v1(&get("keys=fork-key&version=v1"), &ctx, &base_refs)
                .unwrap()
                .is_none(),
            "trusted job restored a fork-namespace entry over v1"
        );
        let miss = lookup_v2(
            post_v2(json!({ "key": "fork-key", "version": "v1" })),
            &mut ctx,
            &base_refs,
        )
        .await
        .unwrap();
        assert_eq!(miss, json!({"ok": false}));
        assert!(
            download_chain(&ctx.service, &fork_id, &base_refs)
                .await
                .is_err(),
            "trusted job downloaded a fork-namespace blob"
        );
    }

    #[test]
    fn cache_trust_regression_benchmark_cross_job_restore_throughput() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        // The cross-job restore path — session resolution plus a two-scope
        // v1 lookup — runs on every cache restore. Bound it in the repo's
        // timing-gated-test style (no criterion/divan harness exists here).
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let job_a =
            CacheIdentity::test_identity("acme/repo", "refs/heads/main", None, TrustClass::Trusted)
                .unwrap();
        let job_b = CacheIdentity::test_identity(
            "acme/repo",
            "refs/heads/feature",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "job-a-token", &job_a).unwrap();
        register_job_cache_session(dir.path(), "job-b-token", &job_b).unwrap();
        let chain_a = service.resolve_namespaces("job-a-token");
        let chain_b = service.resolve_namespaces("job-b-token");
        assert_eq!(chain_a.len(), 1);
        assert_eq!(chain_b.len(), 2);
        assert_eq!(chain_a[0], chain_b[1]);
        for namespace in chain_a.iter().chain(chain_b.iter()) {
            service.ensure_tenant(namespace).unwrap();
        }
        commit_in(&service, &chain_a[0], "shared-key", b"shared-blob");
        let ctx = test_ctx(service);
        let chain_b_refs: Vec<&str> = chain_b.iter().map(String::as_str).collect();
        assert!(
            lookup_v1(&get("keys=shared-key&version=v1"), &ctx, &chain_b_refs)
                .unwrap()
                .is_some()
        );

        let started = Instant::now();
        for _ in 0..2_000 {
            let chain = ctx.service.resolve_namespaces(black_box("job-b-token"));
            let refs: Vec<&str> = chain.iter().map(String::as_str).collect();
            let hit = lookup_v1(
                &get("keys=shared-key&version=v1"),
                black_box(&ctx),
                black_box(&refs),
            )
            .unwrap();
            assert!(hit.is_some());
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(30),
            "2k cross-job restores took {elapsed:?}",
        );
    }

    fn tempfile_dir() -> TestDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let temp_root = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonicalize system temporary directory");
        TestDir(temp_root.join(format!(
            "velnor-gha-cache-test-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )))
    }

    struct TestDir(PathBuf);
    impl TestDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

//! Velnor-native GitHub Actions cache service (Plan P1).
//!
//! Self-hosted job messages never carry a `CacheServerUrl`, so BuildKit's
//! `type=gha` backend and `actions/cache@v4` silently no-op on Velnor while
//! working on GitHub-hosted runners. This module hosts the two cache service
//! generations on a small hyper server so the same YAML is warm on every lane:
//!
//! * v1 Twirp artifactcache (`actions/cache@v4`, older buildkit):
//!   `_apis/artifactcache/api/v1/cache/{reserve,{id},cache,{id}}`
//! * v2 Results CacheService (buildkit selects via
//!   `ACTIONS_CACHE_SERVICE_V2=True`): `CreateCacheEntryUpload`,
//!   `FinalizeCacheEntryUpload`, `GetCacheEntryDownloadURL`
//!
//! Storage is content-addressed beneath the durable cache root, namespaced by
//! repository and ref: `tenants/repo-<sha256(repository \0 ref)>/{blobs,entries,reservations}`
//! plus tiny JSON entry records keyed by `sha256(key \0 version)`. The runner
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
//! request path. Insertion enforces an LRU byte budget per namespace by
//! deleting oldest-hit entries.
//!
//! Known deviation, recorded for the follow-up: GitHub also retries the lookup
//! on the repository's default branch, which the job message does not carry,
//! so the fallback chain ends at the base ref. PR runs (whose base is usually
//! the default branch) are covered; direct pushes to a side branch cannot yet
//! restore default-branch entries.
//!
//! The service is OFF unless the operator exports `VELNOR_ACTIONS_CACHE_URL`
//! into the runner environment (strict capability contract: no behavior
//! change without explicit enablement). Requests must carry the job-scoped
//! `ACTIONS_RUNTIME_TOKEN`; the operator's enablement variable is never used
//! as a job credential.

use crate::trust_class::TrustClass;
use anyhow::{Context, Result};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Channel, Full, Limited};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::CONTENT_LENGTH;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Read as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_BUDGET_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_BODY: u64 = 16 * 1024 * 1024 * 1024;
const MAX_JSON_BODY: usize = 64 * 1024;
const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;
const DOWNLOAD_BUFFERED_CHUNKS: usize = 1;
/// Sanity caps for identity fields. Generous on purpose: rejection degrades a
/// job to its isolated namespace, so the bounds only exclude garbage.
const MAX_REPOSITORY_LEN: usize = 256;
const MAX_REF_LEN: usize = 512;
/// Session and fallback-marker retention. Job tokens live hours; a week keeps
/// the registry bounded while surviving weekends and daemon restarts.
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub(crate) fn entry_hash(key: &str, version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(b"\0");
    hasher.update(version.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    /// LRU byte budget per cache namespace (one ref scope or one isolated token).
    pub(crate) budget_bytes: u64,
    /// Directory for the isolated-fallback forensic line (`daemon.log`). `None`
    /// keeps the line on stderr; the daemon sets it to its log directory.
    pub(crate) forensic_log_dir: Option<PathBuf>,
}

impl CacheService {
    pub(crate) fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("tenants")).context("create gha-cache tenants dir")?;
        let budget = std::env::var("VELNOR_GHA_CACHE_BUDGET_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BUDGET_BYTES);
        Ok(Self {
            root,
            budget_bytes: budget,
            forensic_log_dir: None,
        })
    }

    fn tenant_root(&self, namespace: Option<&str>) -> PathBuf {
        namespace.map_or_else(
            || self.root.clone(),
            |namespace| self.root.join("tenants").join(namespace),
        )
    }

    fn ensure_tenant(&self, namespace: &str) -> Result<()> {
        let root = self.tenant_root(Some(namespace));
        std::fs::create_dir_all(root.join("blobs")).context("create gha-cache tenant blobs dir")?;
        std::fs::create_dir_all(root.join("entries"))
            .context("create gha-cache tenant entries dir")?;
        std::fs::create_dir_all(root.join("reservations"))
            .context("create gha-cache tenant reservations dir")?;
        Ok(())
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

    fn read_entry(&self, hash: &str, namespace: Option<&str>) -> Option<Value> {
        let raw = std::fs::read(self.entry_path(hash, namespace)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Exact match first, then each restore key as a newest-wins prefix scan.
    fn lookup(&self, keys: &[&str], version: &str, namespace: Option<&str>) -> Option<CacheHit> {
        for (index, key) in keys.iter().enumerate() {
            let hash = entry_hash(key, version);
            if self.read_entry(&hash, namespace).is_some() {
                return Some(CacheHit {
                    hash,
                    key: (*key).to_owned(),
                });
            }
            if index == 0 {
                continue; // the primary key is exact-only; only restore keys prefix-scan
            }
            if let Some(hit) = self.prefix_scan(key, version, namespace) {
                return Some(hit);
            }
        }
        None
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
            let created = entry["created_ms"].as_u64().unwrap_or(0);
            let hash = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
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

    fn enforce_budget(&self, namespace: Option<&str>) -> Result<()> {
        let mut entries: Vec<(std::time::SystemTime, u64, PathBuf, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for file in std::fs::read_dir(self.tenant_root(namespace).join("entries"))
            .context("scan entries")?
            .flatten()
        {
            let path = file.path();
            let Ok(raw) = std::fs::read(&path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let size = entry["size"].as_u64().unwrap_or(0);
            let modified = file
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            total += size;
            entries.push((modified, size, path, self.blob_path_for(&entry, namespace)));
        }
        if total <= self.budget_bytes {
            return Ok(());
        }
        entries.sort_by_key(|(modified, ..)| *modified);
        for (_, size, entry_path, blob_path) in entries {
            if total <= self.budget_bytes {
                break;
            }
            let _ = std::fs::remove_file(&entry_path);
            let _ = std::fs::remove_file(blob_path);
            total = total.saturating_sub(size);
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
    expected_size: u64,
}

impl V1Reservation {
    fn as_json(&self) -> Value {
        json!({
            "key": self.key,
            "version": self.version,
            "cacheSize": self.expected_size,
        })
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

fn parse_v1_reservation(value: Value, id: &str) -> Result<V1Reservation> {
    let key = value["key"]
        .as_str()
        .context("v1 cache reservation has no key")?;
    let version = value["version"]
        .as_str()
        .context("v1 cache reservation has no version")?;
    let expected_size = value["cacheSize"]
        .as_u64()
        .context("v1 cache reservation has no valid cacheSize")?;
    if expected_size > MAX_BODY {
        anyhow::bail!("v1 cache reservation exceeds {MAX_BODY} bytes");
    }
    if entry_hash(key, version) != id {
        anyhow::bail!("v1 cache reservation does not match cache id");
    }
    Ok(V1Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        expected_size,
    })
}

fn read_v1_reservation(
    service: &CacheService,
    id: &str,
    namespace: &str,
) -> Result<Option<V1Reservation>> {
    validate_cache_id(id)?;
    let path = service.reservation_path(id, Some(namespace));
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read v1 cache reservation {id}")),
    };
    let value: Value =
        serde_json::from_slice(&raw).with_context(|| format!("parse v1 cache reservation {id}"))?;
    parse_v1_reservation(value, id).map(Some)
}

fn load_v1_reservation(service: &CacheService, id: &str, namespace: &str) -> Result<V1Reservation> {
    read_v1_reservation(service, id, namespace)?.context("v1 cache reservation is missing")
}

fn persist_v1_reservation(
    service: &CacheService,
    id: &str,
    reservation: &V1Reservation,
    namespace: &str,
) -> Result<()> {
    service.ensure_tenant(namespace)?;
    let path = service.reservation_path(id, Some(namespace));
    if let Some(existing) = read_v1_reservation(service, id, namespace)? {
        if existing == *reservation {
            return Ok(());
        }
        anyhow::bail!("v1 cache reservation does not match existing reservation");
    }

    if atomically_create_json(&path, &reservation.as_json())? {
        return Ok(());
    }

    let existing = read_v1_reservation(service, id, namespace)?
        .context("v1 cache reservation disappeared during creation")?;
    if existing == *reservation {
        Ok(())
    } else {
        anyhow::bail!("v1 cache reservation does not match concurrent reservation")
    }
}

fn clear_v1_reservation(service: &CacheService, id: &str, namespace: &str) -> Result<()> {
    let path = service.reservation_path(id, Some(namespace));
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
    Ok(())
}

fn cache_namespace(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-tenant\0");
    hasher.update(token.as_bytes());
    hex(&hasher.finalize())
}

/// Repository identity a job's cache requests are scoped to: the `owner/name`
/// the workflow runs in, the full ref it runs on, and — for pull requests —
/// the full base ref it may restore from — plus the job's trust class, which
/// decides whether the job writes the base namespaces or an isolated fork
/// one. Validation failures never fail the job; the caller treats them as "no
/// usable identity" and the job's requests fall back to an isolated per-token
/// namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheIdentity {
    repository: String,
    git_ref: String,
    base_ref: Option<String>,
    trust: TrustClass,
}

impl CacheIdentity {
    pub(crate) fn new(
        repository: &str,
        git_ref: &str,
        base_ref: Option<&str>,
        trust: TrustClass,
    ) -> Result<Self> {
        let repository = repository.trim().to_ascii_lowercase();
        let (owner, name) = repository
            .split_once('/')
            .context("cache identity repository is not owner/name")?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            anyhow::bail!("cache identity repository is not owner/name");
        }
        if repository.len() > MAX_REPOSITORY_LEN || repository.chars().any(char::is_control) {
            anyhow::bail!("cache identity repository is not usable");
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
            repository,
            git_ref,
            base_ref,
            trust,
        })
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
            chain.push(fork_namespace(&self.repository, &self.git_ref));
        }
        chain.push(repo_namespace(&self.repository, &self.git_ref));
        if let Some(base) = &self.base_ref {
            chain.push(repo_namespace(&self.repository, base));
        }
        chain
    }
}

fn repo_namespace(repository: &str, git_ref: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-repo\0");
    hasher.update(repository.as_bytes());
    hasher.update(b"\0");
    hasher.update(git_ref.as_bytes());
    format!("repo-{}", hex(&hasher.finalize()))
}

/// Isolated write scope for fork-PR and unknown jobs. Same inputs as
/// [`repo_namespace`] but a distinct hash domain and prefix, so a fork
/// namespace can never collide with a base namespace even for the same
/// repository and ref — including a fork-controlled run whose ref *is* a base
/// branch. Trusted jobs never resolve here (see [`CacheIdentity::namespaces`]).
fn fork_namespace(repository: &str, git_ref: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-fork\0");
    hasher.update(repository.as_bytes());
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
            "repository": self.identity.repository,
            "ref": self.identity.git_ref,
            "baseRef": self.identity.base_ref,
            "trust": self.identity.trust.as_str(),
            "registeredMs": self.registered_ms,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let repository = value["repository"]
            .as_str()
            .context("cache session has no repository")?;
        let git_ref = value["ref"].as_str().context("cache session has no ref")?;
        let base_ref = value["baseRef"].as_str();
        // No default: a session written before trust existed (or with a
        // label this build does not know) must not inherit trust. Failing
        // the parse sends the token to its isolated per-token namespace —
        // the same fail-closed path as a corrupt session — and the next
        // registration rewrites the binding with trust.
        let trust = value["trust"]
            .as_str()
            .context("cache session has no trust")?;
        let registered_ms = value["registeredMs"].as_u64().unwrap_or(0);
        Ok(Self {
            identity: CacheIdentity::new(repository, git_ref, base_ref, parse_trust(trust)?)?,
            registered_ms,
        })
    }
}

/// Bind a job's runtime token to its repository identity so the job's cache
/// requests resolve to the shared repo namespaces instead of an isolated
/// per-token one. The slot process calls this before any job process can issue
/// a cache request; the file registry (not memory) carries the binding across
/// to the daemon process hosting the HTTP service, and across restarts.
/// Overwriting an existing binding is idempotent for the same identity.
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
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&sessions).context("create gha-cache sessions dir")?;
    let token_hash = cache_namespace(token);
    let session = CacheSession {
        identity: identity.clone(),
        registered_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    let path = sessions.join(format!("{token_hash}.json"));
    let tmp = sessions.join(format!(
        ".{token_hash}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let cleanup = TemporaryUpload::new(tmp.clone());
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .context("create temporary cache session")?;
        file.write_all(session.as_json().to_string().as_bytes())
            .context("write temporary cache session")?;
        file.sync_all().context("sync temporary cache session")?;
    }
    std::fs::rename(&tmp, &path).context("publish cache session")?;
    drop(cleanup);
    sync_directory(&sessions).context("sync cache sessions dir after publication")?;
    prune_stale_sessions(&sessions, std::time::SystemTime::now());
    Ok(())
}

fn read_session(root: &Path, token_hash: &str) -> Option<CacheSession> {
    let raw = std::fs::read(root.join("sessions").join(format!("{token_hash}.json"))).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    CacheSession::parse(value).ok()
}

/// Delete session bindings and fallback markers older than [`SESSION_TTL_SECS`].
/// Best-effort and conservative: only `<64-hex>.json`/`.isolated` files are
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
                    .or_else(|| name.strip_suffix(".isolated"));
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
    /// Resolve a request token to its lookup chain: the registered ref/base
    /// repo namespaces — fork-headed for fork-PR and unknown jobs — or, when
    /// no session exists, the isolated per-token namespace plus one forensic
    /// line. Never empty.
    fn resolve_namespaces(&self, token: &str) -> Vec<String> {
        let token_hash = cache_namespace(token);
        if let Some(session) = read_session(&self.root, &token_hash) {
            return session.identity.namespaces();
        }
        self.note_isolated_fallback(&token_hash);
        vec![token_hash]
    }

    /// Record that a token fell back to its isolated namespace. Exactly once
    /// per token: the marker file is the dedupe state, durable across
    /// restarts so a restarted daemon does not re-log live jobs. Strictly
    /// additive — it never fails the request — and the line names only the
    /// token hash, never the token.
    fn note_isolated_fallback(&self, token_hash: &str) {
        let sessions = self.root.join("sessions");
        if std::fs::create_dir_all(&sessions).is_err() {
            return;
        }
        let noted = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(sessions.join(format!("{token_hash}.isolated")))
            .is_ok();
        if !noted {
            return;
        }
        let message = format!(
            "gha-cache isolated fallback: no job session for token hash {token_hash}; serving per-token namespace"
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

async fn route(
    req: Request<Incoming>,
    ctx: &mut Ctx,
) -> Result<Response<ResponseBody>, hyper::Error> {
    // Auth: every route requires a non-empty job-scoped bearer capability.
    // The credential is never persisted or surfaced in errors; only its hash
    // leaves the request path, as the session-registry key (registered jobs
    // resolve to their ref/base repo namespaces) or as the isolated-namespace
    // name (unregistered tokens, with one forensic line).
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty());
    let Some(token) = token else {
        return Ok(respond_unauthorized());
    };
    let scope = ctx.service.resolve_namespaces(token);
    let chain: Vec<&str> = scope.iter().map(String::as_str).collect();
    // Writes always land in the job's own scope head (its ref namespace, its
    // fork namespace when the job is fork-PR or unknown, or its isolated
    // namespace); only reads walk the chain into the base scope.
    let primary = chain[0];
    if let Err(error) = ctx.service.ensure_tenant(primary) {
        eprintln!("Warning: gha cache tenant initialization: {error:#}");
        return Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/json")
            .body(full_body(json!({"message": "internal error"}).to_string()))
            .unwrap());
    }
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    let respond = |status: StatusCode, body: Value| {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(full_body(body.to_string()))
            .unwrap()
    };
    let internal_error = || {
        respond(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"message": "internal error"}),
        )
    };

    let result = match (method, path.as_str()) {
        (hyper::Method::POST, p) if p.ends_with("/cache/reserve") => reserve(req, ctx, primary)
            .await
            .map(|v| respond(StatusCode::OK, v)),
        (hyper::Method::PUT, p) => {
            let (id, v1) = match v2_upload_id(p) {
                Some(id) => (id.to_owned(), false),
                None => (p.rsplit('/').next().unwrap_or_default().to_owned(), true),
            };
            upload(req, ctx, &id, primary, v1)
                .await
                .map(|_| respond(StatusCode::OK, json!({"ok": true})))
        }
        (hyper::Method::GET, p) if p.ends_with("/cache") => {
            lookup_v1(&req, ctx, &chain).map(|entry| match entry {
                Some(entry) => respond(StatusCode::OK, entry),
                None => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(full_body(Bytes::new()))
                    .unwrap(),
            })
        }
        (hyper::Method::GET, p) => {
            if let Some(id) = p.rsplit('/').next() {
                download_chain(&ctx.service, id, &chain)
                    .await
                    .map(|(body, size)| {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/octet-stream")
                            .header(CONTENT_LENGTH, size)
                            .body(body)
                            .unwrap()
                    })
            } else {
                Ok(respond(
                    StatusCode::NOT_FOUND,
                    json!({"message": "not found"}),
                ))
            }
        }
        (hyper::Method::POST, p) if p.contains("CreateCacheEntryUpload") => {
            reserve_v2(req, ctx, primary)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if p.contains("FinalizeCacheEntryUpload") => {
            finalize_v2(req, ctx, primary)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if p.contains("GetCacheEntryDownloadURL") => {
            lookup_v2(req, ctx, &chain)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        _ => Ok(respond(
            StatusCode::NOT_FOUND,
            json!({"message": "not found"}),
        )),
    };
    match result {
        Ok(response) => Ok(response),
        Err(error) => {
            eprintln!("Warning: gha cache service: {error:#}");
            Ok(internal_error())
        }
    }
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

fn respond_unauthorized() -> Response<ResponseBody> {
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
    collect_json(req.into_body(), MAX_JSON_BODY).await
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
    let size = body["cacheSize"]
        .as_u64()
        .context("missing or invalid cacheSize")?;
    if size > MAX_BODY {
        return Ok(json!({"__typename": "BadRequestError"}));
    }
    let hash = entry_hash(key, version);
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        return Ok(json!({"__typename": "ConflictError", "message": "already exists"}));
    }
    let reservation = V1Reservation {
        key: key.to_owned(),
        version: version.to_owned(),
        expected_size: size,
    };
    persist_v1_reservation(&ctx.service, &hash, &reservation, namespace)?;
    Ok(json!({"cacheId": hash}))
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
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        return Ok(json!({"ok": false}));
    }
    Ok(json!({
        "ok": true,
        "signedUploadUrl": format!("{}/_results/upload/{hash}", ctx.public_base),
    }))
}

#[derive(Debug, Clone, Copy)]
enum BlobPublication {
    Replace,
    NoReplace,
}

#[derive(Debug, Clone, Copy)]
struct UploadLimits {
    declared_size: Option<u64>,
    expected_size: Option<u64>,
    max_bytes: u64,
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
        || entry["size"].as_u64() != Some(reservation.expected_size)
    {
        anyhow::bail!("existing v1 cache entry does not match reservation");
    }

    let blob_path = service.tenant_root(Some(namespace)).join("blobs").join(id);
    let blob_metadata = std::fs::symlink_metadata(&blob_path)
        .with_context(|| format!("stat existing v1 cache blob {id}"))?;
    if !blob_metadata.file_type().is_file() {
        anyhow::bail!("existing v1 cache blob is not a regular file");
    }
    if blob_metadata.len() != reservation.expected_size {
        anyhow::bail!(
            "existing v1 cache blob size mismatch: reservation records {}, file has {}",
            reservation.expected_size,
            blob_metadata.len()
        );
    }
    Ok(())
}

async fn upload<B>(req: Request<B>, ctx: &Ctx, id: &str, namespace: &str, v1: bool) -> Result<()>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let declared_size = request_content_length(&req)?;

    if !v1 {
        store_upload(
            req.into_body(),
            &ctx.service,
            id,
            UploadLimits {
                declared_size,
                expected_size: None,
                max_bytes: MAX_BODY,
            },
            Some(namespace),
            BlobPublication::Replace,
        )
        .await?;
        return Ok(());
    }

    let reservation = load_v1_reservation(&ctx.service, id, namespace)?;
    if let Some(declared_size) = declared_size
        && declared_size != reservation.expected_size
    {
        anyhow::bail!(
            "cache upload size mismatch: reserved {}, declared {declared_size}",
            reservation.expected_size
        );
    }
    if ctx.service.entry_path(id, Some(namespace)).exists() {
        validate_existing_v1_entry(&ctx.service, id, &reservation, namespace)?;
    }

    let actual_size = store_upload(
        req.into_body(),
        &ctx.service,
        id,
        UploadLimits {
            declared_size,
            expected_size: Some(reservation.expected_size),
            max_bytes: MAX_BODY,
        },
        Some(namespace),
        BlobPublication::NoReplace,
    )
    .await?;

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
    Ok(())
}

fn request_content_length<B>(req: &Request<B>) -> Result<Option<u64>> {
    req.headers()
        .get(CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .context("cache upload Content-Length is not ASCII")?
                .parse()
                .context("cache upload Content-Length is not an unsigned integer")
        })
        .transpose()
}

async fn store_upload<B>(
    mut body: B,
    service: &CacheService,
    id: &str,
    limits: UploadLimits,
    namespace: Option<&str>,
    publication: BlobPublication,
) -> Result<u64>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let UploadLimits {
        declared_size,
        expected_size,
        max_bytes,
    } = limits;
    validate_cache_id(id)?;
    if declared_size.is_some_and(|size| size > max_bytes) {
        anyhow::bail!("declared cache upload size exceeds {max_bytes} bytes");
    }
    if let Some(expected_size) = expected_size {
        if expected_size > max_bytes {
            anyhow::bail!("expected cache upload size exceeds {max_bytes} bytes");
        }
        if let Some(declared_size) = declared_size
            && declared_size != expected_size
        {
            anyhow::bail!(
                "cache upload size mismatch: expected {expected_size}, declared {declared_size}"
            );
        }
    }

    let blob_dir = service.tenant_root(namespace).join("blobs");
    std::fs::create_dir_all(&blob_dir).context("create cache blob directory")?;
    let tmp = blob_dir.join(format!(".{id}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let cleanup = TemporaryUpload::new(tmp.clone());
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .await
        .context("create temporary cache upload")?;
    let mut actual_size = 0u64;

    loop {
        let Some(frame) = body.frame().await else {
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
        if actual_size > max_bytes {
            anyhow::bail!("actual cache upload size exceeds {max_bytes} bytes");
        }
        if expected_size.is_some_and(|size| actual_size > size) {
            anyhow::bail!("actual cache upload size exceeds reserved cacheSize");
        }
        for chunk in bytes.chunks(TRANSFER_CHUNK_BYTES) {
            file.write_all(chunk).await?;
        }
    }

    if let Some(declared_size) = declared_size
        && declared_size != actual_size
    {
        anyhow::bail!(
            "cache upload size mismatch: declared {declared_size}, received {actual_size}"
        );
    }
    if let Some(expected_size) = expected_size
        && expected_size != actual_size
    {
        anyhow::bail!(
            "cache upload size mismatch: reserved {expected_size}, received {actual_size}"
        );
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    let dst = blob_dir.join(id);
    match publication {
        BlobPublication::Replace => {
            tokio::fs::rename(&tmp, &dst)
                .await
                .context("publish completed cache upload")?;
        }
        BlobPublication::NoReplace => match std::fs::hard_link(&tmp, &dst) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !files_are_identical(&tmp, &dst)? {
                    anyhow::bail!("refusing to overwrite existing cache blob with different bytes");
                }
            }
            Err(error) => {
                return Err(error).context("publish cache upload without replacing existing blob");
            }
        },
    }
    drop(cleanup);
    sync_directory(&blob_dir).context("sync cache blob directory after publication")?;
    Ok(actual_size)
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
    commit_entry(&ctx.service, key, version, size, Some(namespace))?;
    Ok(json!({"ok": true, "state": "succeeded"}))
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
            return size.parse().with_context(|| format!("invalid {field}"));
        }
        anyhow::bail!("invalid {field}");
    }
    anyhow::bail!("missing cache upload size")
}

fn commit_entry(
    ctx: &CacheService,
    key: &str,
    version: &str,
    size: u64,
    namespace: Option<&str>,
) -> Result<()> {
    let (hash, entry) = validated_entry(ctx, key, version, size, namespace)?;
    std::fs::create_dir_all(ctx.tenant_root(namespace).join("entries"))?;
    std::fs::write(ctx.entry_path(&hash, namespace), entry.to_string())?;
    ctx.enforce_budget(namespace)?;
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
    ctx.enforce_budget(namespace)?;
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
    let entry = json!({
        "key": key,
        "version": version,
        "blob": hash,
        "size": actual,
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
        if let Some(hit) = ctx.service.lookup(&keys, &version, Some(namespace)) {
            return Ok(Some(json!({
                "archiveLocation": format!("{}/_results/download/{}", ctx.public_base, hit.hash),
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
    if let Some(restores) = body["restoreKeys"].as_array() {
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
        if let Some(hit) = ctx.service.lookup(&keys, version, Some(namespace)) {
            // `matched_key` is field 3 of
            // `github.actions.results.api.v1.GetCacheEntryDownloadURLResponse`
            // (actions/toolkit `packages/cache/src/generated/results/api/v1/cache.ts`).
            // `restoreCache` compares it against the requested primary key to
            // decide exact hit vs restore-key hit and returns it as the cache key,
            // so omitting it makes every hit look like a restore-key hit and the
            // entry is re-saved on the next run.
            return Ok(json!({
                "ok": true,
                "signedDownloadUrl": format!("{}/_results/download/{}", ctx.public_base, hit.hash),
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
    body[field].as_str().context(format!("missing {field}"))
}

async fn download(
    service: &CacheService,
    id: &str,
    namespace: Option<&str>,
) -> Result<(ResponseBody, u64)> {
    validate_cache_id(id)?;
    let entry = service
        .read_entry(id, namespace)
        .context("download before finalize (no entry)")?;
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
    tokio::spawn(stream_download(file, sender, actual_size));
    Ok((body.boxed_unsync(), actual_size))
}

/// Serve a download from the first scope holding the entry, in chain order.
/// The lookup that issued the URL searched the same order, so this serves what
/// the lookup reported; a scope whose entry vanished (budget eviction between
/// the two calls) simply yields to the next one.
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

async fn stream_download(
    mut file: tokio::fs::File,
    mut sender: http_body_util::channel::Sender<Bytes, io::Error>,
    expected_size: u64,
) {
    let mut buffer = vec![0u8; TRANSFER_CHUNK_BYTES];
    let mut sent = 0u64;

    loop {
        let read = match file.read(&mut buffer).await {
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
        if next_sent > expected_size {
            sender.abort(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cached blob grew beyond its {expected_size}-byte entry"),
            ));
            return;
        }
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
mod tests {
    use super::*;
    use futures_util::stream;
    use http_body_util::StreamBody;
    use hyper::body::Frame;
    use std::path::Path;

    fn test_service(dir: &Path) -> CacheService {
        let mut service = CacheService::open(dir.to_path_buf()).expect("open");
        service.budget_bytes = 1024;
        service
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
            .is_some());
        assert!(service
            .lookup(&["shared-key"], "v1", Some("tenant-b"))
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
            .unwrap();
        assert_eq!(hit.hash, entry_hash("linux-rust-2026", "v1"));
        assert_eq!(hit.key, "linux-rust-2026");

        // Prefix falls back to newest, and reports the stored key it matched.
        let hit = svc.lookup(&["linux-other", "linux"], "v1", None).unwrap();
        assert_eq!(hit.hash, entry_hash("linux-rust", "v1"));
        assert_eq!(hit.key, "linux-rust");

        // Version mismatch misses.
        assert!(svc.lookup(&["linux-rust"], "v9", None).is_none());
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
            let hit = svc.lookup(&[primary.as_str(), restore.as_str()], "v1", None);
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
                expected_size: contents.len() as u64,
            }
        );

        upload(put_body(contents), &reopened, id, "tenant", true)
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

        let first_upload = upload(put_body(first_body), &ctx, id, "tenant", true);
        let second_upload = upload(put_body(second_body), &ctx, id, "tenant", true);
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
            expected_size: contents.len() as u64,
        };

        upload(put_body(contents), &ctx, id, "tenant", true)
            .await
            .unwrap();
        persist_v1_reservation(&ctx.service, id, &reservation, "tenant").unwrap();
        assert!(ctx.service.reservation_path(id, Some("tenant")).exists());

        upload(put_body(contents), &ctx, id, "tenant", true)
            .await
            .unwrap();

        assert!(!ctx.service.reservation_path(id, Some("tenant")).exists());
        assert!(ctx.service.entry_path(id, Some("tenant")).exists());
    }

    #[tokio::test]
    async fn v1_upload_rejects_missing_reservation() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        service.ensure_tenant("tenant").unwrap();
        let ctx = test_ctx(service);
        let id = entry_hash("missing-reservation", "v1");

        let error = upload(put_body(b"abc"), &ctx, &id, "tenant", true)
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

        let error = upload(put_body(b"abc"), &ctx, id, "tenant", true)
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

        let error = upload(put_body(b"abc"), &ctx, &id, "tenant", true)
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

    #[test]
    fn budget_evicts_oldest_first() {
        let dir = tempfile_dir();
        let mut svc = CacheService::open(dir.path().to_path_buf()).expect("open");
        svc.budget_bytes = 10;
        commit_blob(&svc, "old", "v", b"123456");
        std::thread::sleep(std::time::Duration::from_millis(5));
        commit_blob(&svc, "new", "v", b"123456");
        svc.enforce_budget(None).unwrap();
        assert!(svc.lookup(&["old"], "v", None).is_none(), "oldest evicted");
        assert!(svc.lookup(&["new"], "v", None).is_some(), "newest retained");
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
            },
            None,
            BlobPublication::Replace,
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
            },
            None,
            BlobPublication::Replace,
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
            },
            None,
            BlobPublication::Replace,
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
        let first = repo_namespace("acme/repo", "refs/heads/main");
        assert_eq!(first, repo_namespace("acme/repo", "refs/heads/main"));
        assert_ne!(first, repo_namespace("acme/other", "refs/heads/main"));
        assert_ne!(first, repo_namespace("acme/repo", "refs/heads/feature"));
        assert!(first.starts_with("repo-"));
        assert_eq!(first.len(), "repo-".len() + 64);
        assert!(!first.contains("acme"));
        assert!(!first.contains("main"));
    }

    #[test]
    fn identity_validation_normalizes_and_rejects_garbage() {
        let identity = CacheIdentity::new(
            "Acme/Repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        assert_eq!(identity.repository, "acme/repo");
        assert_eq!(identity.git_ref, "refs/pull/7/merge");
        // A short base branch name normalizes to the full ref form.
        assert_eq!(identity.base_ref.as_deref(), Some("refs/heads/main"));

        // A base that adds no scope beyond the ref is dropped.
        let identity = CacheIdentity::new(
            "acme/repo",
            "refs/heads/main",
            Some("refs/heads/main"),
            TrustClass::ForkPR,
        )
        .unwrap();
        assert_eq!(identity.base_ref, None);

        let identity =
            CacheIdentity::new("acme/repo", "refs/pull/7/merge", None, TrustClass::Unknown)
                .unwrap();
        assert_eq!(identity.base_ref, None);

        for repository in [
            "",
            "noslash",
            "/name",
            "owner/",
            "a/b/c",
            "a/b\rc",
            &format!("owner/{}", "n".repeat(300)),
        ] {
            assert!(
                CacheIdentity::new(repository, "refs/heads/main", None, TrustClass::Trusted)
                    .is_err(),
                "accepted repository {repository:?}"
            );
        }
        for git_ref in ["", "refs/heads/\rx", &"r".repeat(600)] {
            assert!(
                CacheIdentity::new("acme/repo", git_ref, None, TrustClass::Trusted).is_err(),
                "accepted ref {git_ref:?}"
            );
        }
        assert!(CacheIdentity::new(
            "acme/repo",
            "refs/heads/main",
            Some("ba\rse"),
            TrustClass::Trusted
        )
        .is_err());
    }

    #[test]
    fn register_resolve_round_trip_serves_ref_then_base() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::new(
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
                repo_namespace("acme/repo", "refs/pull/7/merge"),
                repo_namespace("acme/repo", "refs/heads/main"),
            ]
        );
        // A different job token in the same repo resolves to the same shared
        // namespaces once registered: this is the cross-run hit.
        let other = CacheIdentity::new(
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
    fn unregistered_token_falls_back_isolated_with_exactly_one_forensic_line() {
        let dir = tempfile_dir();
        let mut service = test_service(dir.path());
        let logs = dir.path().join("logs");
        service.forensic_log_dir = Some(logs.clone());

        let isolated = cache_namespace("unknown-token");
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
            service.resolve_namespaces("job-token"),
            vec![token_hash.clone()]
        );
        assert!(sessions.join(format!("{token_hash}.isolated")).exists());
    }

    #[test]
    fn lookup_searches_the_whole_ref_scope_before_the_base() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let identity = CacheIdentity::new(
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
        let identity = CacheIdentity::new(
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
        let identity = CacheIdentity::new(
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
        upload(put_body(contents), &ctx, &id, ref_ns, true)
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
        std::fs::write(sessions.join(".abc.tmp"), b"tmp").unwrap();
        std::fs::write(sessions.join("notes.txt"), b"notes").unwrap();
        std::fs::write(sessions.join("zz.json"), b"{}").unwrap();

        // A `now` before every mtime keeps everything.
        prune_stale_sessions(&sessions, std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 5);

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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Trusted).unwrap();
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
        let trusted = CacheIdentity::new(
            "acme/repo",
            "refs/pull/7/merge",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        assert_eq!(
            trusted.namespaces(),
            vec![
                repo_namespace("acme/repo", "refs/pull/7/merge"),
                repo_namespace("acme/repo", "refs/heads/main"),
            ]
        );

        // ForkPR and Unknown resolve fork-headed: the isolated fork namespace
        // first, then the same base namespaces for read-through.
        for trust in [TrustClass::ForkPR, TrustClass::Unknown] {
            let untrusted =
                CacheIdentity::new("acme/repo", "refs/pull/7/merge", Some("main"), trust).unwrap();
            assert_eq!(
                untrusted.namespaces(),
                vec![
                    fork_namespace("acme/repo", "refs/pull/7/merge"),
                    repo_namespace("acme/repo", "refs/pull/7/merge"),
                    repo_namespace("acme/repo", "refs/heads/main"),
                ],
                "wrong chain for {trust:?}"
            );
        }
    }

    #[test]
    fn fork_isolation_conformance_fork_namespace_is_stable_scoped_and_redacted() {
        let first = fork_namespace("acme/repo", "refs/heads/main");
        assert_eq!(first, fork_namespace("acme/repo", "refs/heads/main"));
        assert_ne!(first, fork_namespace("acme/other", "refs/heads/main"));
        assert_ne!(first, fork_namespace("acme/repo", "refs/heads/feature"));
        assert!(first.starts_with("fork-"));
        assert_eq!(first.len(), "fork-".len() + 64);
        assert!(!first.contains("acme"));
        assert!(!first.contains("main"));
        // Same inputs, distinct domains: a fork namespace can never name the
        // same tenant as the base namespace, not even by prefix confusion.
        assert_ne!(first, repo_namespace("acme/repo", "refs/heads/main"));
        assert!(
            !repo_namespace("acme/repo", "refs/heads/main").starts_with("fork-"),
            "repo namespaces must never carry the fork prefix"
        );
    }

    #[test]
    fn fork_isolation_conformance_trust_label_round_trips_through_the_session() {
        for trust in [TrustClass::Trusted, TrustClass::ForkPR, TrustClass::Unknown] {
            assert_eq!(parse_trust(trust.as_str()).unwrap(), trust);
            let session = CacheSession {
                identity: CacheIdentity::new("acme/repo", "refs/pull/7/merge", Some("main"), trust)
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
        let identity = CacheIdentity::new(
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
        upload(put_body(contents), &ctx, &id, fork_ns, true)
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
        let identity = CacheIdentity::new(
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
        let identity = CacheIdentity::new(
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
        upload(put_body(contents), &ctx, &id, fork_ns, false)
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
    async fn fork_isolation_conformance_trusted_never_reads_fork_namespaces() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let fork_ns = fork_namespace("acme/repo", "refs/heads/main");
        let base_ns = repo_namespace("acme/repo", "refs/heads/main");
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::ForkPR).unwrap();
        register_job_cache_session(dir.path(), "fork-token", &identity).unwrap();
        let chain = service.resolve_namespaces("fork-token");
        assert_eq!(
            chain,
            vec![
                fork_namespace("acme/repo", "refs/heads/main"),
                repo_namespace("acme/repo", "refs/heads/main"),
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
        upload(put_body(contents), &ctx, &id, fork_ns, true)
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Unknown).unwrap();
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
        upload(put_body(contents), &ctx, &id, fork_ns, true)
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
                "repository": "acme/repo",
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
                    "repository": "acme/repo",
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Trusted).unwrap();
        let fork = CacheIdentity::new(
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Trusted).unwrap();
        let job_b = CacheIdentity::new(
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
        upload(put_body(contents), &ctx, &id, &chain_a[0], true)
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
        let job_a = CacheIdentity::new("acme/repo-a", "refs/heads/main", None, TrustClass::Trusted)
            .unwrap();
        let job_b = CacheIdentity::new("acme/repo-b", "refs/heads/main", None, TrustClass::Trusted)
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
        upload(put_body(contents), &ctx, &id, &chain_a[0], true)
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Trusted).unwrap();
        let fork_job = CacheIdentity::new(
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
            CacheIdentity::new("acme/repo", "refs/heads/main", None, TrustClass::Trusted).unwrap();
        let job_b = CacheIdentity::new(
            "acme/repo",
            "refs/heads/feature",
            Some("main"),
            TrustClass::Trusted,
        )
        .unwrap();
        register_job_cache_session(dir.path(), "job-a-token", &job_a).unwrap();
        register_job_cache_session(dir.path(), "job-b-token", &job_b).unwrap();
        let chain_b = service.resolve_namespaces("job-b-token");
        for namespace in &chain_b {
            service.ensure_tenant(namespace).unwrap();
        }
        commit_in(&service, &chain_b[1], "shared-key", b"shared-blob");
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
        TestDir(std::env::temp_dir().join(format!(
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

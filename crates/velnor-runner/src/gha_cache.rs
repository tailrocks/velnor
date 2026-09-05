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
//! Storage is content-addressed under a bearer-token tenant beneath the
//! durable cache root: `tenants/<token-sha256>/blobs/<sha256>` plus tiny JSON
//! entry records keyed by `sha256(key \0 version)`. Key matching follows
//! GitHub semantics: exact `(key, version)` first, then restore-keys prefix
//! order, newest wins. The token is never persisted; its hash is only a
//! storage namespace, so one job cannot address another job's entries by key.
//! Insertion enforces an LRU byte budget by deleting oldest-hit entries.
//!
//! The service is OFF unless the operator exports `VELNOR_ACTIONS_CACHE_URL`
//! into the runner environment (strict capability contract: no behavior
//! change without explicit enablement). Requests must carry the job-scoped
//! `ACTIONS_RUNTIME_TOKEN`; the operator's enablement variable is never used
//! as a job credential.

use anyhow::{Context, Result};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Channel, Full, Limited};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::CONTENT_LENGTH;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub fn entry_hash(key: &str, version: &str) -> String {
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
pub struct CacheService {
    /// Durable storage root; tenant directories live beneath it.
    pub root: PathBuf,
    /// LRU byte budget per bearer-token tenant.
    pub budget_bytes: u64,
}

impl CacheService {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("tenants")).context("create gha-cache tenants dir")?;
        let budget = std::env::var("VELNOR_GHA_CACHE_BUDGET_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BUDGET_BYTES);
        Ok(Self {
            root,
            budget_bytes: budget,
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
                continue; // only the primary key participates in prefix scans
            }
            if let Some(hit) = self.prefix_scan(key, version, namespace) {
                return Some(hit);
            }
        }
        None
    }

    fn prefix_scan(&self, key: &str, version: &str, namespace: Option<&str>) -> Option<CacheHit> {
        let mut hits: BTreeMap<String, CacheHit> = BTreeMap::new();
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
            // specific restore key on equal timestamps in practice).
            let rank = format!("{created:020}/{:04}", entry_key.len());
            hits.insert(
                rank,
                CacheHit {
                    hash,
                    key: entry_key.to_owned(),
                },
            );
        }
        hits.into_values().next_back()
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

pub async fn serve(listener: tokio::net::TcpListener, service: CacheService) -> Result<()> {
    let public_base = configured_public_base()?;
    serve_with_public_base(listener, service, public_base).await
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
    // The credential is never persisted or surfaced in errors; its hash is
    // the only namespace input used by the storage layer.
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty());
    let Some(token) = token else {
        return Ok(respond_unauthorized());
    };
    let namespace = cache_namespace(token);
    if let Err(error) = ctx.service.ensure_tenant(&namespace) {
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
        (hyper::Method::POST, p) if p.ends_with("/cache/reserve") => reserve(req, ctx, &namespace)
            .await
            .map(|v| respond(StatusCode::OK, v)),
        (hyper::Method::PUT, p) => {
            let (id, v1) = match v2_upload_id(p) {
                Some(id) => (id.to_owned(), false),
                None => (p.rsplit('/').next().unwrap_or_default().to_owned(), true),
            };
            upload(req, ctx, &id, &namespace, v1)
                .await
                .map(|_| respond(StatusCode::OK, json!({"ok": true})))
        }
        (hyper::Method::GET, p) if p.ends_with("/cache") => {
            lookup_v1(&req, ctx, &namespace).map(|entry| match entry {
                Some(entry) => respond(StatusCode::OK, entry),
                None => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(full_body(Bytes::new()))
                    .unwrap(),
            })
        }
        (hyper::Method::GET, p) => {
            if let Some(id) = p.rsplit('/').next() {
                download(&ctx.service, id, Some(&namespace))
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
            reserve_v2(req, ctx, &namespace)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if p.contains("FinalizeCacheEntryUpload") => {
            finalize_v2(req, ctx, &namespace)
                .await
                .map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::POST, p) if p.contains("GetCacheEntryDownloadURL") => {
            lookup_v2(req, ctx, &namespace)
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

async fn reserve_v2(req: Request<Incoming>, ctx: &Ctx, namespace: &str) -> Result<Value> {
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
            declared_size,
            None,
            MAX_BODY,
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
        declared_size,
        Some(reservation.expected_size),
        MAX_BODY,
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
    declared_size: Option<u64>,
    expected_size: Option<u64>,
    max_bytes: u64,
    namespace: Option<&str>,
    publication: BlobPublication,
) -> Result<u64>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
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

async fn finalize_v2(req: Request<Incoming>, ctx: &Ctx, namespace: &str) -> Result<Value> {
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
fn lookup_v1<B>(req: &Request<B>, ctx: &Ctx, namespace: &str) -> Result<Option<Value>> {
    let keys = keys_from_query(req);
    let version = query_param(req, "version").unwrap_or_default();
    Ok(ctx
        .service
        .lookup(
            &keys.iter().map(String::as_str).collect::<Vec<_>>(),
            &version,
            Some(namespace),
        )
        .map(|hit| {
            json!({
                "archiveLocation": format!("{}/_results/download/{}", ctx.public_base, hit.hash),
                "cacheKey": hit.key,
                "cacheVersion": version,
            })
        }))
}

async fn lookup_v2<B>(req: Request<B>, ctx: &mut Ctx, namespace: &str) -> Result<Value>
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
    match ctx.service.lookup(
        &keys.iter().map(String::as_str).collect::<Vec<_>>(),
        version,
        Some(namespace),
    ) {
        // `matched_key` is field 3 of
        // `github.actions.results.api.v1.GetCacheEntryDownloadURLResponse`
        // (actions/toolkit `packages/cache/src/generated/results/api/v1/cache.ts`).
        // `restoreCache` compares it against the requested primary key to
        // decide exact hit vs restore-key hit and returns it as the cache key,
        // so omitting it makes every hit look like a restore-key hit and the
        // entry is re-saved on the next run.
        Some(hit) => Ok(json!({
            "ok": true,
            "signedDownloadUrl": format!("{}/_results/download/{}", ctx.public_base, hit.hash),
            "matchedKey": hit.key,
        })),
        None => Ok(json!({"ok": false})),
    }
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
pub fn enabled_from_env() -> Option<String> {
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
pub const DEFAULT_CACHE_BIND: &str = "127.0.0.1:17933";

/// Bind the configured address, spawn the accept loop, return the bound addr.
pub async fn bind_configured(service: CacheService) -> Result<SocketAddr> {
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
            "tenant",
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
        let hit = lookup_v1(&get("keys=v1-rust-cache&version=v1"), &reopened, "tenant")
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
            "tenant",
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
            "tenant",
        )
        .expect("lookup")
        .expect("hit");
        assert_eq!(hit["cacheKey"], json!("linux-rust-2026"));

        // A miss carries no entry; the route turns that into HTTP 204, which is
        // the only status `getCacheEntry` treats as "no cache".
        assert!(lookup_v1(&get("keys=absent&version=v1"), &ctx, "tenant")
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
            "tenant",
        )
        .await
        .expect("lookup");
        assert_eq!(exact["ok"], json!(true));
        assert_eq!(exact["matchedKey"], json!("linux-rust-2026"));

        // Restore-key hit: matched_key is the stored key, not the primary.
        let restored = lookup_v2(
            request(json!({"key": "linux-rust-2027", "version": "v1", "restoreKeys": ["linux"]})),
            &mut ctx,
            "tenant",
        )
        .await
        .expect("lookup");
        assert_eq!(restored["matchedKey"], json!("linux-rust-2026"));

        let miss = lookup_v2(
            request(json!({"key": "absent", "version": "v1"})),
            &mut ctx,
            "tenant",
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
            None,
            None,
            4,
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
            None,
            None,
            MAX_BODY,
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
            Some(4),
            None,
            MAX_BODY,
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

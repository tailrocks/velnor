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
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
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
        Ok(())
    }

    fn entry_path(&self, hash: &str, namespace: Option<&str>) -> PathBuf {
        self.tenant_root(namespace)
            .join("entries")
            .join(format!("{hash}.json"))
    }

    fn read_entry(&self, hash: &str, namespace: Option<&str>) -> Option<Value> {
        let raw = std::fs::read(self.entry_path(hash, namespace)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Exact match first, then each restore key as a newest-wins prefix scan.
    fn lookup(
        &self,
        keys: &[&str],
        version: &str,
        namespace: Option<&str>,
    ) -> Option<(String, u64)> {
        for (index, key) in keys.iter().enumerate() {
            let hash = entry_hash(key, version);
            if let Some(entry) = self.read_entry(&hash, namespace) {
                return Some((hash, entry["size"].as_u64().unwrap_or(0)));
            }
            if index == 0 {
                continue; // only the primary key participates in prefix scans
            }
            if let Some((hash, size)) = self.prefix_scan(key, version, namespace) {
                return Some((hash, size));
            }
        }
        None
    }

    fn prefix_scan(
        &self,
        key: &str,
        version: &str,
        namespace: Option<&str>,
    ) -> Option<(String, u64)> {
        let mut hits: BTreeMap<String, (String, u64)> = BTreeMap::new();
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
            hits.insert(rank, (hash, entry["size"].as_u64().unwrap_or(0)));
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

fn cache_namespace(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-actions-cache-tenant\0");
    hasher.update(token.as_bytes());
    hex(&hasher.finalize())
}

pub async fn serve(listener: tokio::net::TcpListener, service: CacheService) -> Result<()> {
    let service = Arc::new(service);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let ctx = Arc::new(Ctx {
            service: Arc::clone(&service),
            public_base: String::new(),
        });
        tokio::task::spawn(async move {
            let handler = service_fn(move |req| {
                let ctx = Arc::clone(&ctx);
                async move {
                    let mut ctx = Ctx {
                        service: Arc::clone(&ctx.service),
                        public_base: ctx.public_base.clone(),
                    };
                    if ctx.public_base.is_empty() {
                        let host = req
                            .headers()
                            .get("host")
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("localhost");
                        ctx.public_base = format!("http://{host}");
                    }
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
            let id = p.rsplit('/').next().unwrap_or_default().to_owned();
            upload(req, ctx, &id, &namespace)
                .await
                .map(|_| respond(StatusCode::OK, json!({"ok": true})))
        }
        (hyper::Method::GET, p) if p.ends_with("/cache") => {
            lookup_v1(&req, ctx, &namespace).map(|v| respond(StatusCode::OK, v))
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

async fn body_json(req: Request<Incoming>) -> Result<Value> {
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

fn keys_from_query(req: &Request<Incoming>) -> Vec<String> {
    req.uri()
        .query()
        .map(|q| q.to_owned())
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == "keys" || *k == "restoreKeys")
        .flat_map(|(_, v)| v.split(','))
        .map(|v| v.replace("%2C", ","))
        .filter(|v| !v.is_empty())
        .collect()
}

async fn reserve(req: Request<Incoming>, ctx: &Ctx, namespace: &str) -> Result<Value> {
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let size = body["cacheSize"].as_u64().unwrap_or(0);
    if size > MAX_BODY {
        return Ok(json!({"__typename": "BadRequestError"}));
    }
    let hash = entry_hash(key, version);
    if ctx.service.entry_path(&hash, Some(namespace)).exists() {
        return Ok(json!({"__typename": "ConflictError", "message": "already exists"}));
    }
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

async fn upload(req: Request<Incoming>, ctx: &Ctx, id: &str, namespace: &str) -> Result<()> {
    let declared_size = request_content_length(&req)?;
    store_upload(
        req.into_body(),
        &ctx.service,
        id,
        declared_size,
        MAX_BODY,
        Some(namespace),
    )
    .await?;
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
    max_bytes: u64,
    namespace: Option<&str>,
) -> Result<u64>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_cache_id(id)?;
    if declared_size.is_some_and(|size| size > max_bytes) {
        anyhow::bail!("declared cache upload size exceeds {max_bytes} bytes");
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
        for chunk in bytes.chunks(TRANSFER_CHUNK_BYTES) {
            file.write_all(chunk).await?;
        }
    }

    if let Some(declared_size) = declared_size {
        if declared_size != actual_size {
            anyhow::bail!(
                "cache upload size mismatch: declared {declared_size}, received {actual_size}"
            );
        }
    }
    file.flush().await?;
    drop(file);

    let dst = blob_dir.join(id);
    tokio::fs::rename(&tmp, &dst)
        .await
        .context("publish completed cache upload")?;
    drop(cleanup);
    Ok(actual_size)
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
    std::fs::create_dir_all(ctx.tenant_root(namespace).join("entries"))?;
    std::fs::write(ctx.entry_path(&hash, namespace), entry.to_string())?;
    ctx.enforce_budget(namespace)?;
    Ok(())
}

fn lookup_v1(req: &Request<Incoming>, ctx: &Ctx, namespace: &str) -> Result<Value> {
    let keys = keys_from_query(req);
    let version = query_param(req, "version").unwrap_or_default();
    match ctx.service.lookup(
        &keys.iter().map(String::as_str).collect::<Vec<_>>(),
        &version,
        Some(namespace),
    ) {
        Some((hash, size)) => Ok(json!({
            "cacheDownloadUrl": format!("{}/_results/download/{hash}", ctx.public_base),
            "cacheId": hash,
            "size": size,
        })),
        None => Ok(json!({"__typename": "NotFoundError"})),
    }
}

async fn lookup_v2(req: Request<Incoming>, ctx: &mut Ctx, namespace: &str) -> Result<Value> {
    let body = body_json(req).await?;
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
    let _key = keys[0].as_str();
    match ctx.service.lookup(
        &keys.iter().map(String::as_str).collect::<Vec<_>>(),
        version,
        Some(namespace),
    ) {
        Some((hash, _)) => Ok(json!({
            "ok": true,
            "signedDownloadUrl": format!("{}/_results/download/{hash}", ctx.public_base),
        })),
        None => Ok(json!({"ok": false})),
    }
}

fn query_param(req: &Request<Incoming>, name: &str) -> Option<String> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
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

/// Default listen address. Operators override with `VELNOR_ACTIONS_CACHE_BIND`
/// (e.g. `0.0.0.0:17933`) so job containers reach the service through their
/// docker bridge gateway (`host.docker.internal`, mapped by the job's
/// `--add-host`).
pub const DEFAULT_CACHE_BIND: &str = "127.0.0.1:17933";

/// Bind the configured address, spawn the accept loop, return the bound addr.
pub async fn bind_configured(service: CacheService) -> Result<SocketAddr> {
    let raw = std::env::var("VELNOR_ACTIONS_CACHE_BIND")
        .unwrap_or_else(|_| DEFAULT_CACHE_BIND.to_owned());
    let addr: SocketAddr = raw.parse().context("parse VELNOR_ACTIONS_CACHE_BIND")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(error) = serve(listener, service).await {
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
        let (hash, size) = svc
            .lookup(&["linux-rust-2026", "linux"], "v1", None)
            .unwrap();
        assert_eq!(size, 3);
        assert_eq!(hash, entry_hash("linux-rust-2026", "v1"));

        // Prefix falls back to newest.
        let (_, size) = svc.lookup(&["linux-other", "linux"], "v1", None).unwrap();
        assert_eq!(size, 5);

        // Version mismatch misses.
        assert!(svc.lookup(&["linux-rust"], "v9", None).is_none());
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
            4,
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
            None,
            MAX_BODY,
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
    async fn upload_rejects_declared_size_mismatch() {
        let dir = tempfile_dir();
        let service = test_service(dir.path());
        let id = entry_hash("declared", "v");

        let error = store_upload(
            Full::new(Bytes::from_static(b"12345")),
            &service,
            &id,
            Some(4),
            MAX_BODY,
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

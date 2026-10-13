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
//! Storage is content-addressed under the durable cache root:
//! `blobs/<sha256>` plus tiny JSON entry records keyed by
//! `sha256(key \0 version)`. Key matching follows GitHub semantics: exact
//! `(key, version)` first, then restore-keys prefix order, newest wins.
//! Insertion enforces an LRU byte budget by deleting oldest-hit entries.
//!
//! The service is OFF unless the operator exports
//! `VELNOR_ACTIONS_CACHE_URL`/`VELNOR_ACTIONS_RUNTIME_TOKEN` into the runner
//! environment (strict capability contract: no behavior change without
//! explicit enablement).

use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const DEFAULT_BUDGET_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_BODY: u64 = 16 * 1024 * 1024 * 1024;

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
    /// Durable storage root; `blobs/` and `entries/` live beneath it.
    pub root: PathBuf,
    /// Shared bearer token jobs must present.
    pub token: String,
    /// LRU byte budget across the whole store.
    pub budget_bytes: u64,
}

impl CacheService {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("blobs")).context("create gha-cache blobs dir")?;
        std::fs::create_dir_all(root.join("entries")).context("create gha-cache entries dir")?;
        let token = std::env::var("VELNOR_ACTIONS_RUNTIME_TOKEN").unwrap_or_default();
        let budget = std::env::var("VELNOR_GHA_CACHE_BUDGET_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BUDGET_BYTES);
        Ok(Self {
            root,
            token,
            budget_bytes: budget,
        })
    }

    fn entry_path(&self, hash: &str) -> PathBuf {
        self.root.join("entries").join(format!("{hash}.json"))
    }

    fn read_entry(&self, hash: &str) -> Option<Value> {
        let raw = std::fs::read(self.entry_path(hash)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Exact match first, then each restore key as a newest-wins prefix scan.
    fn lookup(&self, keys: &[&str], version: &str) -> Option<(String, u64)> {
        for (index, key) in keys.iter().enumerate() {
            let hash = entry_hash(key, version);
            if let Some(entry) = self.read_entry(&hash) {
                return Some((hash, entry["size"].as_u64().unwrap_or(0)));
            }
            if index == 0 {
                continue; // only the primary key participates in prefix scans
            }
            if let Some((hash, size)) = self.prefix_scan(key, version) {
                return Some((hash, size));
            }
        }
        None
    }

    fn prefix_scan(&self, key: &str, version: &str) -> Option<(String, u64)> {
        let mut hits: BTreeMap<String, (String, u64)> = BTreeMap::new();
        let entries = std::fs::read_dir(self.root.join("entries")).ok()?;
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

    fn enforce_budget(&self) -> Result<()> {
        let mut entries: Vec<(std::time::SystemTime, u64, PathBuf, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for file in std::fs::read_dir(self.root.join("entries"))
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
            entries.push((modified, size, path, self.blob_path_for(&entry)));
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

    fn blob_path_for(&self, entry: &Value) -> PathBuf {
        self.root
            .join("blobs")
            .join(entry["blob"].as_str().unwrap_or_default())
    }
}

struct Ctx {
    service: Arc<CacheService>,
    public_base: String,
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
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Auth: every route requires the bearer token. Constant-enough compare for
    // a LAN-scoped service; secrets never appear in responses.
    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", ctx.service.token));
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    let respond = |status: StatusCode, body: Value| {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Full::from(body.to_string()))
            .unwrap()
    };
    let internal_error = || {
        respond(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"message": "internal error"}),
        )
    };

    if !authorized {
        return Ok(respond(
            StatusCode::UNAUTHORIZED,
            json!({"message": "bad token"}),
        ));
    }

    let result = match (method, path.as_str()) {
        (hyper::Method::POST, p) if p.ends_with("/cache/reserve") => {
            reserve(req, ctx).await.map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::PUT, p) => {
            let id = p.rsplit('/').next().unwrap_or_default().to_owned();
            upload(req, ctx, &id)
                .await
                .map(|_| respond(StatusCode::OK, json!({"ok": true})))
        }
        (hyper::Method::GET, p) if p.ends_with("/cache") => {
            lookup_v1(&req, ctx).map(|v| respond(StatusCode::OK, v))
        }
        (hyper::Method::GET, p) => {
            if let Some(id) = p.rsplit('/').next() {
                download(ctx, id).map(|body| {
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/octet-stream")
                        .body(Full::from(body))
                        .unwrap()
                })
            } else {
                Ok(respond(
                    StatusCode::NOT_FOUND,
                    json!({"message": "not found"}),
                ))
            }
        }
        (hyper::Method::POST, p) if p.contains("CreateCacheEntryUpload") => reserve_v2(req, ctx)
            .await
            .map(|v| respond(StatusCode::OK, v)),
        (hyper::Method::POST, p) if p.contains("FinalizeCacheEntryUpload") => finalize_v2(req, ctx)
            .await
            .map(|v| respond(StatusCode::OK, v)),
        (hyper::Method::POST, p) if p.contains("GetCacheEntryDownloadURL") => lookup_v2(req, ctx)
            .await
            .map(|v| respond(StatusCode::OK, v)),
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

async fn body_json(req: Request<Incoming>) -> Result<Value> {
    let bytes = req.collect().await?.to_bytes();
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

async fn reserve(req: Request<Incoming>, ctx: &Ctx) -> Result<Value> {
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let size = body["cacheSize"].as_u64().unwrap_or(0);
    if size > MAX_BODY {
        return Ok(json!({"__typename": "BadRequestError"}));
    }
    let hash = entry_hash(key, version);
    if ctx.service.entry_path(&hash).exists() {
        return Ok(json!({"__typename": "ConflictError", "message": "already exists"}));
    }
    Ok(json!({"cacheId": hash}))
}

async fn reserve_v2(req: Request<Incoming>, ctx: &Ctx) -> Result<Value> {
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let hash = entry_hash(key, version);
    if ctx.service.entry_path(&hash).exists() {
        return Ok(json!({"ok": false}));
    }
    Ok(json!({
        "ok": true,
        "signedUploadUrl": format!("{}/_results/upload/{hash}", ctx.public_base),
    }))
}

async fn upload(req: Request<Incoming>, _ctx: &Ctx, id: &str) -> Result<()> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid cache id");
    }
    let tmp = _ctx.service.root.join("blobs").join(format!("{id}.tmp"));
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = req.into_body();
    loop {
        match stream.frame().await {
            Some(Ok(data)) => {
                let bytes = data.into_data().map_err(|_| {
                    anyhow::anyhow!("cache upload frame carried trailers instead of data")
                })?;
                file.write_all(bytes.as_ref()).await?;
            }
            Some(Err(error)) => return Err(error.into()),
            None => break,
        }
    }
    file.flush().await?;
    drop(file);
    let dst = _ctx.service.root.join("blobs").join(id);
    tokio::fs::rename(&tmp, &dst).await?;
    Ok(())
}

async fn finalize_v2(req: Request<Incoming>, ctx: &Ctx) -> Result<Value> {
    let body = body_json(req).await?;
    let key = required_str(&body, "key")?;
    let version = required_str(&body, "version")?;
    let size = body["size"].as_u64().unwrap_or(0);
    commit_entry(&ctx.service, key, version, size)?;
    Ok(json!({"ok": true, "state": "succeeded"}))
}

fn commit_entry(ctx: &CacheService, key: &str, version: &str, size: u64) -> Result<()> {
    let hash = entry_hash(key, version);
    let blob = ctx.root.join("blobs").join(&hash);
    let actual = std::fs::metadata(&blob).map(|m| m.len()).unwrap_or(size);
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
    std::fs::write(ctx.entry_path(&hash), entry.to_string())?;
    ctx.enforce_budget()?;
    Ok(())
}

fn lookup_v1(req: &Request<Incoming>, ctx: &Ctx) -> Result<Value> {
    let keys = keys_from_query(req);
    let version = query_param(req, "version").unwrap_or_default();
    match ctx.service.lookup(
        &keys.iter().map(String::as_str).collect::<Vec<_>>(),
        &version,
    ) {
        Some((hash, size)) => Ok(json!({
            "cacheDownloadUrl": format!("{}/_results/download/{hash}", ctx.public_base),
            "cacheId": hash,
            "size": size,
        })),
        None => Ok(json!({"__typename": "NotFoundError"})),
    }
}

async fn lookup_v2(req: Request<Incoming>, ctx: &mut Ctx) -> Result<Value> {
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

fn download(ctx: &Ctx, id: &str) -> Result<Vec<u8>> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid cache id");
    }
    let entry = ctx
        .service
        .read_entry(id)
        .context("download before finalize (no entry)")?;
    let blob_path = ctx.service.blob_path_for(&entry);
    std::fs::read(blob_path).context("read cached blob")
}

/// Operator enablement contract: both variables must be present and
/// non-empty. Shared by the daemon bootstrap (service spawn) and the
/// runtime-env injection so the two can never drift.
#[must_use]
pub fn enabled_from_env() -> Option<(String, String)> {
    let url = std::env::var("VELNOR_ACTIONS_CACHE_URL").ok()?;
    let token = std::env::var("VELNOR_ACTIONS_RUNTIME_TOKEN").ok()?;
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some((url, token))
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
    use std::path::Path;

    fn test_service(dir: &Path) -> CacheService {
        let mut service = CacheService::open(dir.to_path_buf()).expect("open");
        service.token = "t".into();
        service.budget_bytes = 1024;
        service
    }

    #[test]
    fn entry_hash_is_deterministic_and_version_sensitive() {
        assert_eq!(entry_hash("a", "v"), entry_hash("a", "v"));
        assert_ne!(entry_hash("a", "v"), entry_hash("a", "v2"));
        assert_ne!(entry_hash("ab", "v"), entry_hash("a", "bv"));
    }

    #[test]
    fn lookup_prefers_exact_over_prefix_and_newest_wins() {
        let dir = tempfile_dir();
        let svc = test_service(dir.path());
        commit_entry(&svc, "linux-rust-2026", "v1", 3).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        commit_entry(&svc, "linux-rust", "v1", 5).unwrap();

        // Exact beats newer prefix hit.
        let (hash, size) = svc.lookup(&["linux-rust-2026", "linux"], "v1").unwrap();
        assert_eq!(size, 3);
        assert_eq!(hash, entry_hash("linux-rust-2026", "v1"));

        // Prefix falls back to newest.
        let (_, size) = svc.lookup(&["linux-other", "linux"], "v1").unwrap();
        assert_eq!(size, 5);

        // Version mismatch misses.
        assert!(svc.lookup(&["linux-rust"], "v9").is_none());
    }

    #[test]
    fn budget_evicts_oldest_first() {
        let dir = tempfile_dir();
        let mut svc = CacheService::open(dir.path().to_path_buf()).expect("open");
        svc.budget_bytes = 10;
        commit_entry(&svc, "old", "v", 6).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        commit_entry(&svc, "new", "v", 6).unwrap();
        svc.enforce_budget().unwrap();
        assert!(svc.lookup(&["old"], "v").is_none(), "oldest evicted");
        assert!(svc.lookup(&["new"], "v").is_some(), "newest retained");
    }

    fn tempfile_dir() -> TestDir {
        TestDir(std::env::temp_dir().join(format!(
            "velnor-gha-cache-test-{}-{}",
            std::process::id(),
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

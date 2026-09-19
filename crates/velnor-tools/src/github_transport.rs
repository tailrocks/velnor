//! Concrete read-only GitHub transport and durable raw response store.
//!
//! The acquisition module owns the request/page state machine.  This module
//! supplies the production boundary it calls: one fixed GitHub API origin,
//! no redirects, bearer credentials held only in memory, bounded response
//! bodies, and an immutable content-addressed store with a provenance sidecar.

use super::{
    content_addressed_storage_ref, sha256_digest, AcquisitionFuture, AcquisitionRequest,
    AcquisitionTransport, AuthIdentity, HttpMethod, RawObject, RawObjectRef, RawObjectStore,
    RawStorageError, TransportFailure,
};
use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::redirect::Policy;
use serde_json::json;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const GITHUB_API_VERSION: &str = "2026-03-10";
const USER_AGENT_VALUE: &str = "velnor-tools-g0-live-collector";

/// Concrete authenticated read-only transport.
///
/// `token` never appears in `Debug`, request records, or errors.  The
/// transport rejects any request that is not a GET REST call or a POST to the
/// fixed GraphQL endpoint.  Redirects are disabled so a server cannot move an
/// auth-bearing request to another origin.
pub struct GithubHttpTransport {
    client: reqwest::Client,
    token: String,
    max_body_bytes: usize,
}

impl fmt::Debug for GithubHttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubHttpTransport")
            .field("max_body_bytes", &self.max_body_bytes)
            .finish_non_exhaustive()
    }
}

impl GithubHttpTransport {
    /// Construct the transport from a token already obtained by the CLI.
    /// Token material is copied into private memory and is never returned.
    pub fn new(token: impl Into<String>) -> Result<Self> {
        Self::with_limits(token, DEFAULT_TIMEOUT, DEFAULT_MAX_BODY_BYTES)
    }

    /// Environment constructor used by the live CLI.  The returned transport
    /// owns the token; callers must not place the returned env value in an
    /// evidence object or log it.
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .context("live GitHub collection requires GITHUB_TOKEN or GH_TOKEN")?;
        Self::new(token)
    }

    /// Register the private token with an acquisition identity so response
    /// bytes are masked before crossing the raw store boundary.
    pub fn bind_auth(&self, auth: &mut AuthIdentity) -> Result<()> {
        auth.register_credential(&self.token)
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    #[cfg(test)]
    fn with_limits(
        token: impl Into<String>,
        timeout: Duration,
        max_body_bytes: usize,
    ) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty() || token.as_bytes().contains(&0) {
            bail!("live GitHub transport requires a non-empty token");
        }
        if max_body_bytes == 0 {
            bail!("live GitHub transport requires a positive body limit");
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .context("build GitHub read-only HTTP client")?;
        Ok(Self {
            client,
            token,
            max_body_bytes,
        })
    }

    #[cfg(not(test))]
    fn with_limits(
        token: impl Into<String>,
        timeout: Duration,
        max_body_bytes: usize,
    ) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty() || token.as_bytes().contains(&0) {
            bail!("live GitHub transport requires a non-empty token");
        }
        if max_body_bytes == 0 {
            bail!("live GitHub transport requires a positive body limit");
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .context("build GitHub read-only HTTP client")?;
        Ok(Self {
            client,
            token,
            max_body_bytes,
        })
    }

    fn request_url(&self, request: &AcquisitionRequest) -> Result<Url, TransportFailure> {
        let endpoint = request
            .api_origin
            .bind(&request.endpoint_or_operation)
            .map_err(|_| TransportFailure::Other)?;
        let mut url = Url::parse(&endpoint).map_err(|_| TransportFailure::Other)?;
        for (key, value) in &request.query {
            url.query_pairs_mut().append_pair(key, value);
        }
        if request.api == super::ApiKind::GraphQl && url.path() != "/graphql" {
            return Err(TransportFailure::Other);
        }
        match (request.api, request.method) {
            (super::ApiKind::Rest, HttpMethod::Get)
            | (super::ApiKind::GraphQl, HttpMethod::Post) => Ok(url),
            _ => Err(TransportFailure::Other),
        }
    }

    fn request_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // HeaderValue construction can only fail for a token containing
        // invalid header bytes; the constructor rejects NUL and reqwest will
        // reject all other invalid values at send time without exposing them.
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            headers.insert(AUTHORIZATION, value);
        }
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        headers
    }
}

impl AcquisitionTransport for GithubHttpTransport {
    fn send<'a>(
        &'a self,
        request: AcquisitionRequest,
    ) -> AcquisitionFuture<'a, Result<super::TransportResponse, TransportFailure>> {
        Box::pin(async move {
            let url = self.request_url(&request)?;
            let mut builder = match request.method {
                HttpMethod::Get => self.client.get(url.clone()),
                HttpMethod::Post => self.client.post(url.clone()),
            };
            builder = builder.headers(self.request_headers());
            if let Some(body) = request.body {
                if body.len() > self.max_body_bytes {
                    return Err(TransportFailure::Other);
                }
                builder = builder
                    .header("content-type", "application/json")
                    .body(body);
            }
            let response = builder.send().await.map_err(classify_reqwest_error)?;
            let status = response.status().as_u16();
            // A redirect is not evidence.  Policy::none keeps the original
            // URL, and this explicit rejection prevents a redirect response
            // from being interpreted as a successful page.
            if (300..400).contains(&status) {
                return Err(TransportFailure::Other);
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.max_body_bytes as u64)
            {
                return Err(TransportFailure::Other);
            }
            let effective_endpoint = response.url().to_string();
            let headers = safe_response_headers(response.headers());
            let body = response.bytes().await.map_err(classify_reqwest_error)?;
            if body.len() > self.max_body_bytes {
                return Err(TransportFailure::Other);
            }
            Ok(super::TransportResponse {
                status,
                headers,
                body: body.to_vec(),
                effective_endpoint,
            })
        })
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> TransportFailure {
    if error.is_timeout() {
        TransportFailure::Timeout
    } else if error.is_connect() {
        TransportFailure::Connection
    } else {
        TransportFailure::Other
    }
}

fn safe_response_headers(headers: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    const ALLOWED: [&str; 10] = [
        "content-type",
        "link",
        "x-github-request-id",
        "x-request-id",
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
        "x-ratelimit-used",
        "x-ratelimit-reset",
        "retry-after",
        "etag",
    ];
    let mut safe = std::collections::BTreeMap::new();
    for name in ALLOWED {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            safe.insert(name.to_owned(), value.to_owned());
        }
    }
    safe
}

/// Durable local content-addressed store for response bytes.
///
/// Bytes live below `<root>/sha256/<hex>`.  A separate immutable provenance
/// sidecar below `<root>/refs/<raw-id>.json` binds the caller's request/raw
/// identity to that digest.  The store refuses path traversal, symlinks,
/// digest mismatches, and replacing an existing object with different bytes.
pub struct RawObjectFileStore {
    root: PathBuf,
}

impl fmt::Debug for RawObjectFileStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawObjectFileStore")
            .field("root", &self.root)
            .finish()
    }
}

impl RawObjectFileStore {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("sha256"))?;
        fs::create_dir_all(root.join("refs"))?;
        let metadata = fs::symlink_metadata(&root)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw store root is not a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, digest: &str) -> Result<PathBuf, RawStorageError> {
        let hex = digest
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(RawStorageError::Unbound)?;
        Ok(self.root.join("sha256").join(hex))
    }

    fn sidecar_path(&self, raw_id: &str) -> Result<PathBuf, RawStorageError> {
        if raw_id.is_empty()
            || raw_id.len() > 160
            || !raw_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(RawStorageError::Unbound);
        }
        Ok(self.root.join("refs").join(format!("{raw_id}.json")))
    }

    fn write_new(path: &Path, bytes: &[u8]) -> Result<(), RawStorageError> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(path);
                    return Err(io_to_storage(error));
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(io_to_storage(error)),
        }
    }

    fn existing_bytes(path: &Path) -> Result<Vec<u8>, RawStorageError> {
        let metadata = fs::symlink_metadata(path).map_err(io_to_storage)?;
        if !metadata.is_file() {
            return Err(RawStorageError::Unbound);
        }
        fs::read(path).map_err(io_to_storage)
    }
}

impl RawObjectStore for RawObjectFileStore {
    fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
        if !valid_digest(&object.original_sha256) {
            return Err(RawStorageError::Refused);
        }
        let digest = sha256_digest(&object.bytes);
        let object_path = self.object_path(&digest)?;
        if object_path.exists() {
            if Self::existing_bytes(&object_path)? != object.bytes {
                return Err(RawStorageError::Refused);
            }
        } else {
            Self::write_new(&object_path, &object.bytes)?;
            if Self::existing_bytes(&object_path)? != object.bytes {
                return Err(RawStorageError::Refused);
            }
        }
        let reference = RawObjectRef {
            raw_id: object.raw_id,
            request_id: object.request_id,
            object_kind: object.object_kind,
            canonicalization: object.canonicalization,
            sha256: digest.clone(),
            byte_length: object.bytes.len() as u64,
            original_sha256: object.original_sha256,
            original_byte_length: object.original_byte_length,
            media_type: object.media_type,
            storage_ref: content_addressed_storage_ref(&digest),
        };
        let sidecar = self.sidecar_path(&reference.raw_id)?;
        let sidecar_bytes = serde_json::to_vec(&json!({
            "raw_id": reference.raw_id,
            "request_id": reference.request_id,
            "object_kind": reference.object_kind,
            "canonicalization": reference.canonicalization,
            "sha256": reference.sha256,
            "byte_length": reference.byte_length,
            "original_sha256": reference.original_sha256,
            "original_byte_length": reference.original_byte_length,
            "media_type": reference.media_type,
            "storage_ref": reference.storage_ref,
        }))
        .map_err(|_| RawStorageError::Refused)?;
        if sidecar.exists() {
            if Self::existing_bytes(&sidecar)? != sidecar_bytes {
                return Err(RawStorageError::Refused);
            }
        } else {
            Self::write_new(&sidecar, &sidecar_bytes)?;
            if Self::existing_bytes(&sidecar)? != sidecar_bytes {
                return Err(RawStorageError::Refused);
            }
        }
        Ok(reference)
    }

    fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
        if !valid_digest(&reference.original_sha256) {
            return Err(RawStorageError::Unbound);
        }
        let object_path = self.object_path(&reference.sha256)?;
        let bytes = Self::existing_bytes(&object_path)?;
        if bytes.len() as u64 != reference.byte_length || sha256_digest(&bytes) != reference.sha256
        {
            return Err(RawStorageError::Unbound);
        }
        if reference.storage_ref != content_addressed_storage_ref(&reference.sha256) {
            return Err(RawStorageError::Unbound);
        }
        let sidecar = self.sidecar_path(&reference.raw_id)?;
        let sidecar_bytes = Self::existing_bytes(&sidecar)?;
        let expected = serde_json::to_vec(&json!({
            "raw_id": reference.raw_id,
            "request_id": reference.request_id,
            "object_kind": reference.object_kind,
            "canonicalization": reference.canonicalization,
            "sha256": reference.sha256,
            "byte_length": reference.byte_length,
            "original_sha256": reference.original_sha256,
            "original_byte_length": reference.original_byte_length,
            "media_type": reference.media_type,
            "storage_ref": reference.storage_ref,
        }))
        .map_err(|_| RawStorageError::Refused)?;
        if sidecar_bytes != expected {
            return Err(RawStorageError::Unbound);
        }
        Ok(())
    }
}

fn valid_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn io_to_storage(error: io::Error) -> RawStorageError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => RawStorageError::Refused,
        io::ErrorKind::NotFound => RawStorageError::Unavailable,
        _ => RawStorageError::Unavailable,
    }
}

/// Test-only unique directory helper.  Production callers choose the evidence
/// directory explicitly and never use this path.
#[cfg(test)]
fn test_store_root(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "velnor-g0-{label}-{}-{sequence}-{now}",
        std::process::id()
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::github_acquisition::RawObjectStore;

    #[test]
    fn transport_debug_does_not_render_token() {
        let token = "github_pat_0123456789abcdef";
        let transport = GithubHttpTransport::new(token).expect("transport");
        let debug = format!("{transport:?}");
        assert!(!debug.contains(token));
        assert!(!debug.contains("github_pat_"));
    }

    #[test]
    fn file_store_binds_bytes_and_provenance() {
        let root = test_store_root("binding");
        let mut store = RawObjectFileStore::new(&root).expect("store");
        let bytes = br#"{"id":1}"#.to_vec();
        let object = RawObject {
            raw_id: "request-0001-response".to_owned(),
            request_id: "request-0001".to_owned(),
            object_kind: "repository".to_owned(),
            canonicalization: "raw-bytes-v1".to_owned(),
            media_type: "application/json".to_owned(),
            original_sha256: sha256_digest(&bytes),
            original_byte_length: bytes.len() as u64,
            bytes,
        };
        let reference = store.store(object).expect("reference");
        store.verify(&reference).expect("verify");
        assert!(reference.storage_ref.starts_with("sha256://"));
        assert!(root.join("refs/request-0001-response.json").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn file_store_refuses_tampered_content_and_sidecar() {
        let root = test_store_root("tamper");
        let mut store = RawObjectFileStore::new(&root).expect("store");
        let bytes = b"original".to_vec();
        let object = RawObject {
            raw_id: "request-0002-response".to_owned(),
            request_id: "request-0002".to_owned(),
            object_kind: "repository".to_owned(),
            canonicalization: "raw-bytes-v1".to_owned(),
            media_type: "application/json".to_owned(),
            original_sha256: sha256_digest(&bytes),
            original_byte_length: bytes.len() as u64,
            bytes,
        };
        let reference = store.store(object).expect("reference");
        let object_path = root
            .join("sha256")
            .join(reference.sha256.trim_start_matches("sha256:"));
        let mut file = File::create(&object_path).expect("tamper object");
        file.write_all(b"tampered").expect("write tamper");
        assert_eq!(store.verify(&reference), Err(RawStorageError::Unbound));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn file_store_rejects_path_like_raw_id() {
        let root = test_store_root("raw-id");
        let mut store = RawObjectFileStore::new(&root).expect("store");
        let bytes = b"{}".to_vec();
        let result = store.store(RawObject {
            raw_id: "../escape".to_owned(),
            request_id: "request".to_owned(),
            object_kind: "repository".to_owned(),
            canonicalization: "raw-bytes-v1".to_owned(),
            media_type: "application/json".to_owned(),
            original_sha256: sha256_digest(&bytes),
            original_byte_length: bytes.len() as u64,
            bytes,
        });
        assert_eq!(result, Err(RawStorageError::Unbound));
        let _ = fs::remove_dir_all(root);
    }
}

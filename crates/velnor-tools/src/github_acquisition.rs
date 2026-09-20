//! Fail-closed, provenance-preserving GitHub API acquisition primitives.
//!
//! This module is intentionally independent from `evidence_live`.  It owns no
//! credentials, writes no files, and does not decide whether a workflow or
//! check is successful.  It records the API facts needed by a later collector:
//! every request, every page, the safe auth identity, and a verified reference
//! to the exact response bytes.

#![allow(
    dead_code,
    reason = "collector wiring is a follow-up integration scope"
)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use url::Url;

pub type AcquisitionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Production GitHub transport and content-addressed raw-byte store.  Kept as
/// a child module so the checker-owned binary module list remains untouched
/// while the live collector integration is reviewed independently.
#[path = "github_transport.rs"]
pub mod live_transport;

/// Descriptor-relative content-addressed store for safe response bytes.
#[path = "github_raw_store.rs"]
pub mod raw_store;

/// Current GitHub facts collector and typed observation mapping.  It remains
/// behind this acquisition namespace until the checker owner approves the
/// final `g0_contract` adapter seam.
#[path = "github_live_collector.rs"]
pub mod live_collector;

/// Append-only request progress and terminal status for a live capture.
#[path = "github_live_progress.rs"]
pub mod live_progress;

/// Adapter from the live observation ledger to the strict checker-owned G0
/// types.  It never fills missing provider identities with synthetic IDs.
#[path = "g0_live_mapping.rs"]
pub mod g0_mapping;

/// Producer for the non-GitHub binding inputs required by the strict G0
/// adapter.  It reads the model configuration through one observed local
/// file descriptor and derives the workload artifact from the exact live
/// workflow/source/scanner ledger; it never accepts a caller-created typed
/// claim as authoritative evidence.
#[path = "g0_binding_producer.rs"]
pub mod binding_producer;

/// Explicit CLI boundary for a read-only live capture.  It writes only to a
/// caller-selected local evidence directory and never dispatches or mutates
/// GitHub.
#[path = "github_live_cli.rs"]
pub mod live_cli;

/// API family used by a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ApiKind {
    Rest,
    GraphQl,
}

/// HTTP method sent by the acquisition transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

/// Normalized, fixed GitHub API origin.  The acquisition seam deliberately
/// does not accept arbitrary hosts: auth-bearing requests stay on the public
/// GitHub API origin until a separately designed enterprise policy exists.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GithubApiOrigin(String);

impl fmt::Debug for GithubApiOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("GithubApiOrigin")
            .field(&self.0)
            .finish()
    }
}

impl GithubApiOrigin {
    pub fn github() -> Self {
        Self("https://api.github.com".to_owned())
    }

    fn bind(&self, endpoint: &str) -> Result<String, AcquisitionError> {
        self.validate_origin()?;
        let origin = Url::parse(&self.0).map_err(|_| AcquisitionError::EndpointViolation)?;
        let candidate = match Url::parse(endpoint) {
            Ok(candidate) => candidate,
            Err(_) => origin
                .join(endpoint)
                .map_err(|_| AcquisitionError::EndpointViolation)?,
        };
        self.validate_candidate(&candidate)?;
        Ok(candidate.to_string())
    }

    fn validate_candidate(&self, candidate: &Url) -> Result<(), AcquisitionError> {
        self.validate_origin()?;
        let origin = Url::parse(&self.0).map_err(|_| AcquisitionError::EndpointViolation)?;
        if candidate.scheme() != "https"
            || candidate.host_str() != origin.host_str()
            || candidate.port() != origin.port()
            || !candidate.username().is_empty()
            || candidate.password().is_some()
            || candidate.fragment().is_some()
        {
            return Err(AcquisitionError::EndpointViolation);
        }
        for (key, value) in candidate.query_pairs() {
            if key_is_sensitive(&key) || looks_like_secret(&value) {
                return Err(AcquisitionError::EndpointViolation);
            }
        }
        Ok(())
    }

    fn validate_origin(&self) -> Result<(), AcquisitionError> {
        let origin = Url::parse(&self.0).map_err(|_| AcquisitionError::EndpointViolation)?;
        if origin.scheme() != "https"
            || origin.host_str() != Some("api.github.com")
            || origin.port().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(AcquisitionError::EndpointViolation);
        }
        Ok(())
    }
}

/// Credential handle carried only as a safe reference.  The actual token is
/// owned by the transport and cannot be represented by this type.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthHandle(String);

impl fmt::Debug for AuthHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthHandle([opaque])")
    }
}

impl AuthHandle {
    fn validate(&self) -> Result<(), AcquisitionError> {
        if self.0.is_empty()
            || self.0.len() > 128
            || !self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || looks_like_secret(&self.0)
        {
            return Err(AcquisitionError::SecretMetadata);
        }
        Ok(())
    }
}

/// Registered credential values are held in memory only and are masked before
/// response bytes cross the raw-object storage boundary.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CredentialRegistry {
    secrets: Vec<Vec<u8>>,
}

impl fmt::Debug for CredentialRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRegistry")
            .field("registered_count", &self.secrets.len())
            .finish()
    }
}

impl CredentialRegistry {
    pub fn register(&mut self, secret: impl AsRef<str>) -> Result<(), AcquisitionError> {
        let secret = secret.as_ref();
        if secret.len() < 8 || secret.as_bytes().contains(&0) {
            return Err(AcquisitionError::SecretMetadata);
        }
        if !self.secrets.iter().any(|value| value == secret.as_bytes()) {
            self.secrets.push(secret.as_bytes().to_vec());
        }
        Ok(())
    }
}

/// Safe identity for the credential used by a request.
///
/// Token values and auth headers are deliberately not represented.  The
/// `secret_excluded` invariant is checked before a collection starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthIdentity {
    #[serde(rename = "reference")]
    auth_handle: AuthHandle,
    pub provider: String,
    pub viewer_id: Option<String>,
    pub viewer_login: Option<String>,
    pub safe_scopes: BTreeSet<String>,
    pub secret_excluded: bool,
    #[serde(skip)]
    credentials: CredentialRegistry,
}

impl AuthIdentity {
    pub fn new(
        reference: impl Into<String>,
        provider: impl Into<String>,
        viewer_id: Option<String>,
        viewer_login: Option<String>,
        safe_scopes: BTreeSet<String>,
    ) -> Self {
        Self {
            auth_handle: AuthHandle(reference.into()),
            provider: provider.into(),
            viewer_id,
            viewer_login,
            safe_scopes,
            secret_excluded: true,
            credentials: CredentialRegistry::default(),
        }
    }

    pub fn reference(&self) -> &str {
        &self.auth_handle.0
    }

    pub fn register_credential(&mut self, secret: impl AsRef<str>) -> Result<(), AcquisitionError> {
        self.credentials.register(secret)
    }

    fn validate(&self) -> Result<(), AcquisitionError> {
        self.auth_handle.validate()?;
        if self.provider.trim().is_empty() {
            return Err(AcquisitionError::InvalidRequest);
        }
        if !self.secret_excluded
            || [
                Some(self.provider.as_str()),
                self.viewer_id.as_deref(),
                self.viewer_login.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(looks_like_secret)
            || self
                .safe_scopes
                .iter()
                .any(|scope| looks_like_secret(scope))
        {
            return Err(AcquisitionError::SecretMetadata);
        }
        Ok(())
    }
}

/// Request passed to the transport.  The body is never included in its
/// `Debug` representation because GraphQL variables may contain caller data.
#[derive(Clone)]
pub struct AcquisitionRequest {
    pub api: ApiKind,
    pub method: HttpMethod,
    pub api_origin: GithubApiOrigin,
    pub endpoint_or_operation: String,
    pub query: BTreeMap<String, String>,
    /// One of the fixed provider media types.  This is transport metadata,
    /// never caller-controlled header text.
    pub accept: Option<String>,
    pub body: Option<Vec<u8>>,
}

impl fmt::Debug for AcquisitionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquisitionRequest")
            .field("api", &self.api)
            .field("method", &self.method)
            .field("api_origin", &self.api_origin)
            .field(
                "endpoint_or_operation",
                &redacted_endpoint(&self.endpoint_or_operation),
            )
            .field("query", &redacted_query(&self.query))
            .field("body", &self.body.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Successful or failed HTTP response returned by an acquisition transport.
#[derive(Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    /// Final URL reported by a no-redirect transport.  The collector rejects
    /// a response whose effective origin is outside `request.api_origin`.
    pub effective_endpoint: String,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("headers", &redacted_headers(&self.headers))
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .field(
                "effective_endpoint",
                &redacted_endpoint(&self.effective_endpoint),
            )
            .finish()
    }
}

/// Transport failures have no free-form message, preventing accidental
/// persistence of URLs, headers, or credential-bearing client errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    Timeout,
    Connection,
    PermissionDenied,
    RateLimited,
    Other,
}

/// Read-only transport seam.  Implementations own auth headers and network
/// behavior; this module only receives safe response metadata and bytes.
pub trait AcquisitionTransport {
    fn send<'a>(
        &'a self,
        request: AcquisitionRequest,
    ) -> AcquisitionFuture<'a, Result<TransportResponse, TransportFailure>>;
}

/// Stored response bytes and their immutable provenance reference.
#[derive(Clone, PartialEq, Eq)]
pub struct RawObject {
    pub raw_id: String,
    pub request_id: String,
    pub object_kind: String,
    pub canonicalization: String,
    pub media_type: String,
    /// Exact bytes returned by the provider before masking/canonicalization.
    /// The storage boundary computes their digest and length; callers cannot
    /// supply either provenance claim separately.
    pub original_bytes: Vec<u8>,
    /// Safe bytes retained after credential masking. The store computes the
    /// safe digest and length from these bytes as well.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for RawObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawObject")
            .field("raw_id", &self.raw_id)
            .field("request_id", &self.request_id)
            .field("object_kind", &self.object_kind)
            .field("canonicalization", &self.canonicalization)
            .field("media_type", &self.media_type)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

/// Reference emitted into the collector snapshot.  The store must return a
/// reference matching the bytes offered to it; mismatches fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawObjectRef {
    pub raw_id: String,
    pub request_id: String,
    pub object_kind: String,
    pub canonicalization: String,
    pub sha256: String,
    pub byte_length: u64,
    /// Source-response digest pair. `sha256`/`byte_length` describe the
    /// immutable safe object in `storage_ref`; these fields describe the
    /// exact network response captured before redaction.
    pub original_sha256: String,
    pub original_byte_length: u64,
    /// Base64 of the safe/redacted bytes held at `storage_ref`.  This is the
    /// exact payload the typed checker can independently hash and verify.
    pub bytes_base64: String,
    pub media_type: String,
    pub storage_ref: String,
    pub original_storage_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawStorageError {
    Unavailable,
    Refused,
    Unbound,
}

/// Caller-owned raw-byte store.  A production implementation may place bytes
/// in an evidence object store; tests use an in-memory fixture store.
pub trait RawObjectStore {
    fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError>;

    /// Verify that the returned reference denotes an immutable object that is
    /// actually present in the external store.  A caller-asserted URI is not
    /// sufficient evidence.
    fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError>;
}

/// Pagination state recorded on each request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageState {
    pub number: u32,
    pub per_page: Option<u32>,
    pub link_next: Option<String>,
    pub cursor_in: Option<String>,
    pub cursor_out: Option<String>,
    pub has_next_page: Option<bool>,
    pub items_returned: usize,
}

/// Safe rate-limit facts from response headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitObservation {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub used: Option<u64>,
    pub reset_at: Option<String>,
    pub retry_after: Option<String>,
}

/// Every non-success path remains explicit.  No state maps to a successful
/// empty collection except a server-declared terminal page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionState {
    Complete,
    EmptyComplete,
    Forbidden,
    NotFound,
    RateLimited,
    TransportError,
    Malformed,
    GraphQlError,
    DuplicateCursor,
    Truncated,
    Unknown,
}

/// Per-request provenance record handed to the V2 collector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRecord {
    pub request_id: String,
    pub api: ApiKind,
    pub method: HttpMethod,
    pub endpoint_or_operation: String,
    pub query_base64: String,
    pub variables_base64: String,
    pub query_sha256: Option<String>,
    pub variables_sha256: Option<String>,
    pub redacted_variables: Option<Value>,
    pub auth_identity_ref: String,
    pub started_at_utc: String,
    pub completed_at_utc: String,
    pub http_status: Option<u16>,
    pub api_request_id: Option<String>,
    pub rate_limit: Option<RateLimitObservation>,
    pub safe_scopes: Option<Vec<String>>,
    pub page: PageState,
    pub response_raw_ref: Option<String>,
    pub error_raw_ref: Option<String>,
    pub state: AcquisitionState,
    pub complete: bool,
    pub truncation_reason: Option<String>,
}

/// Collected items plus the complete request/raw ledger.  `complete` is the
/// only authority for whether `items` can be consumed as a closed set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionResult {
    pub items: Vec<Value>,
    pub requests: Vec<RequestRecord>,
    pub raw_objects: Vec<RawObjectRef>,
    pub state: AcquisitionState,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionError {
    InvalidRequest,
    EndpointViolation,
    SecretMetadata,
    CredentialMaterialDetected,
    Serialization,
    StorageUnavailable,
    StorageRefused,
    StorageUnbound,
    RawReferenceMismatch,
    RawDigestMismatch,
}

impl fmt::Display for AcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRequest => "invalid acquisition request",
            Self::EndpointViolation => "endpoint is outside the fixed GitHub API origin",
            Self::SecretMetadata => "secret-bearing metadata is not allowed",
            Self::CredentialMaterialDetected => "credential material detected in response bytes",
            Self::Serialization => "request serialization failed",
            Self::StorageUnavailable => "raw object storage unavailable",
            Self::StorageRefused => "raw object storage refused object",
            Self::StorageUnbound => "raw object storage reference is not externally bound",
            Self::RawReferenceMismatch => "raw object reference metadata mismatch",
            Self::RawDigestMismatch => "raw object digest mismatch",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AcquisitionError {}

/// REST list acquisition request.  The item field is `None` for an array
/// response and otherwise names the array field in the JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestCollectionRequest {
    pub collection_id: String,
    pub api_origin: GithubApiOrigin,
    pub endpoint: String,
    pub query: BTreeMap<String, String>,
    pub accept: Option<String>,
    pub item_field: Option<String>,
    pub per_page: usize,
    pub object_kind: String,
    pub max_pages: usize,
}

impl RestCollectionRequest {
    pub fn new(
        collection_id: impl Into<String>,
        endpoint: impl Into<String>,
        item_field: Option<impl Into<String>>,
        object_kind: impl Into<String>,
    ) -> Self {
        Self {
            collection_id: collection_id.into(),
            api_origin: GithubApiOrigin::github(),
            endpoint: endpoint.into(),
            query: BTreeMap::new(),
            accept: None,
            item_field: item_field.map(Into::into),
            per_page: 100,
            object_kind: object_kind.into(),
            max_pages: 1_000,
        }
    }

    pub fn with_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.insert(name.into(), value.into());
        self
    }

    pub fn with_accept(mut self, accept: impl Into<String>) -> Self {
        self.accept = Some(accept.into());
        self
    }

    pub fn with_per_page(mut self, per_page: usize) -> Self {
        self.per_page = per_page;
        self
    }

    pub fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }
}

/// GraphQL connection acquisition request.  `node_path` points to the nodes
/// array, for example `data.repository.pullRequests.nodes`.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphQlCollectionRequest {
    pub collection_id: String,
    pub api_origin: GithubApiOrigin,
    pub endpoint: String,
    pub operation_name: String,
    pub query: String,
    pub variables: Value,
    pub cursor_variable: String,
    pub page_size_variable: String,
    pub page_size: usize,
    pub node_path: Vec<String>,
    pub object_kind: String,
    pub max_pages: usize,
}

impl GraphQlCollectionRequest {
    pub fn new(
        collection_id: impl Into<String>,
        endpoint: impl Into<String>,
        operation_name: impl Into<String>,
        query: impl Into<String>,
        variables: Value,
        node_path: Vec<String>,
        object_kind: impl Into<String>,
    ) -> Self {
        Self {
            collection_id: collection_id.into(),
            api_origin: GithubApiOrigin::github(),
            endpoint: endpoint.into(),
            operation_name: operation_name.into(),
            query: query.into(),
            variables,
            cursor_variable: "after".to_owned(),
            page_size_variable: "first".to_owned(),
            page_size: 100,
            node_path,
            object_kind: object_kind.into(),
            max_pages: 1_000,
        }
    }

    pub fn with_cursor_variable(mut self, variable: impl Into<String>) -> Self {
        self.cursor_variable = variable.into();
        self
    }

    pub fn with_page_size_variable(mut self, variable: impl Into<String>) -> Self {
        self.page_size_variable = variable.into();
        self
    }

    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size;
        self
    }

    pub fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }
}

/// Canonical check-run request.  `filter=all` is intentional: a latest-only
/// projection cannot prove attempt/run completeness.
pub fn github_check_runs_request(
    collection_id: impl Into<String>,
    repository: &str,
    source_sha: &str,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/commits/{source_sha}/check-runs"),
        Some("check_runs"),
        "check_runs",
    )
    .with_query("filter", "all")
    .with_query("per_page", "100")
}

pub fn github_check_suites_request(
    collection_id: impl Into<String>,
    repository: &str,
    source_sha: &str,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/commits/{source_sha}/check-suites"),
        Some("check_suites"),
        "check_suites",
    )
    .with_query("per_page", "100")
}

pub fn github_open_pull_requests_request(
    collection_id: impl Into<String>,
    repository: &str,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/pulls"),
        None::<String>,
        "pull_requests",
    )
    .with_query("state", "open")
    .with_query("per_page", "100")
}

pub fn github_workflow_runs_request(
    collection_id: impl Into<String>,
    repository: &str,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/actions/runs"),
        Some("workflow_runs"),
        "workflow_runs",
    )
    .with_query("per_page", "100")
}

pub fn github_workflow_jobs_request(
    collection_id: impl Into<String>,
    repository: &str,
    run_id: u64,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/actions/runs/{run_id}/jobs"),
        Some("jobs"),
        "workflow_jobs",
    )
    .with_query("per_page", "100")
    .with_query("filter", "all")
}

pub fn github_workflow_attempt_request(
    collection_id: impl Into<String>,
    repository: &str,
    run_id: u64,
    attempt: u64,
) -> RestCollectionRequest {
    github_single_object_request(
        collection_id,
        format!("/repos/{repository}/actions/runs/{run_id}/attempts/{attempt}"),
        "workflow_attempt",
    )
}

pub fn github_workflow_attempt_jobs_request(
    collection_id: impl Into<String>,
    repository: &str,
    run_id: u64,
    attempt: u64,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs"),
        Some("jobs"),
        "workflow_attempt_jobs",
    )
    .with_query("per_page", "100")
}

pub fn github_workflow_artifacts_request(
    collection_id: impl Into<String>,
    repository: &str,
    run_id: u64,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/actions/runs/{run_id}/artifacts"),
        Some("artifacts"),
        "workflow_artifacts",
    )
    .with_query("per_page", "100")
}

pub fn github_check_suite_runs_request(
    collection_id: impl Into<String>,
    repository: &str,
    suite_id: u64,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        format!("/repos/{repository}/check-suites/{suite_id}/check-runs"),
        Some("check_runs"),
        "check_suite_runs",
    )
    .with_query("filter", "all")
    .with_query("per_page", "100")
}

/// Acquire one JSON object while preserving the same request/raw provenance
/// contract as list acquisition.  This is used for `/user`, repository,
/// workflow, ruleset-detail, PR-detail, and commit objects.  The object is
/// represented as one item in `CollectionResult.items`; an empty/malformed
/// response is never converted into a successful empty collection.
pub fn github_single_object_request(
    collection_id: impl Into<String>,
    endpoint: impl Into<String>,
    object_kind: impl Into<String>,
) -> RestCollectionRequest {
    RestCollectionRequest::new(
        collection_id,
        endpoint,
        Some("$object".to_owned()),
        object_kind,
    )
    .with_per_page(1)
}

/// Acquire a REST list until GitHub's Link relation declares the terminal
/// page.  A full page without a next Link is deliberately incomplete.
pub async fn collect_rest<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    request: RestCollectionRequest,
) -> Result<CollectionResult, AcquisitionError>
where
    T: AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    auth.validate()?;
    validate_rest_request(&request)?;

    let mut result = CollectionResult {
        items: Vec::new(),
        requests: Vec::new(),
        raw_objects: Vec::new(),
        state: AcquisitionState::Unknown,
        complete: false,
    };
    let mut endpoint = request.api_origin.bind(&request.endpoint)?;
    let paginated = request.item_field.as_deref() != Some("$object");
    if paginated && request.query.contains_key("page") {
        return Err(AcquisitionError::InvalidRequest);
    }
    let mut query = request.query.clone();
    if !paginated
        && query
            .get("per_page")
            .is_some_and(|value| value != &request.per_page.to_string())
    {
        return Err(AcquisitionError::InvalidRequest);
    }
    if paginated {
        let mut endpoint_url =
            Url::parse(&endpoint).map_err(|_| AcquisitionError::EndpointViolation)?;
        for (key, value) in url_query_map(&endpoint_url)? {
            if query.insert(key, value).is_some() {
                return Err(AcquisitionError::EndpointViolation);
            }
        }
        endpoint_url.set_query(None);
        endpoint = endpoint_url.to_string();
        let expected_per_page = request.per_page.to_string();
        if query
            .insert("per_page".to_owned(), expected_per_page.clone())
            .is_some_and(|value| value != expected_per_page)
        {
            return Err(AcquisitionError::InvalidRequest);
        }
    }
    let mut seen_pages = BTreeSet::new();

    for page_number in 1..=request.max_pages {
        let page_number_u32 = page_number as u32;
        let mut page_query = query.clone();
        if paginated {
            page_query.insert("page".to_owned(), page_number.to_string());
        }
        let page_key = canonical_page_key(&endpoint, &page_query)?;
        if !seen_pages.insert(page_key) {
            result.state = AcquisitionState::DuplicateCursor;
            result.complete = false;
            break;
        }
        let acquisition_request = AcquisitionRequest {
            api: ApiKind::Rest,
            method: HttpMethod::Get,
            api_origin: request.api_origin.clone(),
            endpoint_or_operation: endpoint.clone(),
            query: page_query.clone(),
            accept: request.accept.clone(),
            body: None,
        };
        let request_id = format!("{}-{page_number_u32:04}", request.collection_id);
        let started_at_utc = utc_now();
        let response = transport.send(acquisition_request.clone()).await;

        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                let state = transport_failure_state(failure);
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    None,
                    None,
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.per_page as u32),
                        link_next: None,
                        cursor_in: None,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    None,
                    None,
                    state,
                    false,
                    Some("transport failure".to_owned()),
                    None,
                    None,
                ));
                result.state = state;
                result.complete = false;
                break;
            }
        };
        validate_effective_response(
            &request.api_origin,
            &acquisition_request,
            &response.effective_endpoint,
        )?;

        let rate_limit = rate_limit_from_headers(&response.headers);
        let media_type = header_value(&response.headers, "content-type")
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let raw_id = format!("{request_id}-response");
        let raw = retain_raw(
            store,
            &auth.credentials,
            RawObject {
                raw_id,
                request_id: request_id.clone(),
                object_kind: if response.status / 100 == 2 {
                    request.object_kind.clone()
                } else {
                    format!("{}.error", request.object_kind)
                },
                canonicalization: "raw-bytes-v1".to_owned(),
                media_type,
                original_bytes: response.body.clone(),
                bytes: response.body.clone(),
            },
        )?;
        let raw_id = raw.raw_id.clone();
        result.raw_objects.push(raw);
        let status = response.status;

        if status / 100 != 2 {
            let state = http_state(status, rate_limit.as_ref());
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.per_page as u32),
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: 0,
                },
                Some(raw_id.clone()),
                Some(raw_id),
                state,
                false,
                Some(format!("HTTP status {status}")),
                None,
                rate_limit,
            ));
            result.state = state;
            result.complete = false;
            break;
        }

        let body = match serde_json::from_slice::<Value>(&response.body) {
            Ok(body) => body,
            Err(_) => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.per_page as u32),
                        link_next: None,
                        cursor_in: None,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("invalid JSON response".to_owned()),
                    None,
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        let page_items = match rest_items(&body, request.item_field.as_deref()) {
            Ok(items) => items,
            Err(_) => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.per_page as u32),
                        link_next: None,
                        cursor_in: None,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("unexpected REST list shape".to_owned()),
                    None,
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        if page_items.len() > request.per_page {
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.per_page as u32),
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: page_items.len(),
                },
                Some(raw_id.clone()),
                Some(raw_id),
                AcquisitionState::Malformed,
                false,
                Some("page exceeded requested page size".to_owned()),
                None,
                rate_limit,
            ));
            result.state = AcquisitionState::Malformed;
            result.complete = false;
            break;
        }

        let link_header = header_value(&response.headers, "link");
        let next_link = match link_header.as_deref().map(parse_next_link).transpose() {
            Ok(next_link) => next_link.flatten(),
            Err(_) => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.per_page as u32),
                        link_next: link_header,
                        cursor_in: None,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: page_items.len(),
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("malformed Link pagination header".to_owned()),
                    None,
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        let page_items_len = page_items.len();
        result.items.extend(page_items);

        if let Some(next_link) = next_link {
            if !paginated {
                return Err(AcquisitionError::InvalidRequest);
            }
            let (next_endpoint, next_query, normalized_link) = next_page_request(
                &request.api_origin,
                &next_link,
                &endpoint,
                &query,
                page_number_u32.saturating_add(1),
            )?;
            let page = PageState {
                number: page_number_u32,
                per_page: Some(request.per_page as u32),
                link_next: Some(normalized_link),
                cursor_in: None,
                cursor_out: None,
                has_next_page: Some(true),
                items_returned: page_items_len,
            };
            if page_number >= request.max_pages {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    page,
                    Some(raw_id.clone()),
                    None,
                    AcquisitionState::Truncated,
                    false,
                    Some("collector page cap reached".to_owned()),
                    None,
                    rate_limit,
                ));
                result.state = AcquisitionState::Truncated;
                result.complete = false;
                break;
            }
            let next_page_key = canonical_page_key(&next_endpoint, &next_query)?;
            if seen_pages.contains(&next_page_key) {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    page,
                    Some(raw_id.clone()),
                    None,
                    AcquisitionState::DuplicateCursor,
                    false,
                    Some("duplicate REST next link".to_owned()),
                    None,
                    rate_limit,
                ));
                result.state = AcquisitionState::DuplicateCursor;
                result.complete = false;
                break;
            }
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                page,
                Some(raw_id),
                None,
                AcquisitionState::Complete,
                false,
                None,
                None,
                rate_limit,
            ));
            endpoint = next_endpoint;
            query = next_query;
            continue;
        }

        if page_items_len == request.per_page && request.item_field.as_deref() != Some("$object") {
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.per_page as u32),
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: Some(true),
                    items_returned: page_items_len,
                },
                Some(raw_id.clone()),
                None,
                AcquisitionState::Truncated,
                false,
                Some("full page omitted next Link".to_owned()),
                None,
                rate_limit,
            ));
            result.state = AcquisitionState::Truncated;
            result.complete = false;
            break;
        }

        let state = if result.items.is_empty() {
            AcquisitionState::EmptyComplete
        } else {
            AcquisitionState::Complete
        };
        result.requests.push(make_record(
            &request_id,
            &acquisition_request,
            auth,
            started_at_utc,
            utc_now(),
            Some(status),
            Some(&response.headers),
            PageState {
                number: page_number_u32,
                per_page: Some(request.per_page as u32),
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: Some(false),
                items_returned: page_items_len,
            },
            Some(raw_id),
            None,
            state,
            true,
            None,
            None,
            rate_limit,
        ));
        result.state = state;
        result.complete = true;
        break;
    }

    Ok(result)
}

/// Acquire a GraphQL connection until `pageInfo.hasNextPage` is false.
/// Acquire one binary response without attempting JSON parsing.  This is
/// used for immutable GitHub artifact archives: the exact archive bytes are
/// retained in the same request/raw ledger and never represented by a
/// caller-asserted digest.
pub async fn collect_binary<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    request: RestCollectionRequest,
) -> Result<CollectionResult, AcquisitionError>
where
    T: AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    auth.validate()?;
    validate_rest_request(&request)?;
    let endpoint = request.api_origin.bind(&request.endpoint)?;
    let request_id = format!("{}-0001", request.collection_id);
    let acquisition_request = AcquisitionRequest {
        api: ApiKind::Rest,
        method: HttpMethod::Get,
        api_origin: request.api_origin.clone(),
        endpoint_or_operation: endpoint,
        query: request.query.clone(),
        accept: request.accept.clone(),
        body: None,
    };
    let started_at_utc = utc_now();
    let response = match transport.send(acquisition_request.clone()).await {
        Ok(response) => response,
        Err(failure) => {
            let state = transport_failure_state(failure);
            return Ok(CollectionResult {
                items: Vec::new(),
                requests: vec![make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    None,
                    None,
                    PageState {
                        number: 1,
                        per_page: Some(request.per_page as u32),
                        link_next: None,
                        cursor_in: None,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    None,
                    None,
                    state,
                    false,
                    Some("transport failure".to_owned()),
                    None,
                    None,
                )],
                raw_objects: Vec::new(),
                state,
                complete: false,
            });
        }
    };
    validate_effective_response(
        &request.api_origin,
        &acquisition_request,
        &response.effective_endpoint,
    )?;
    let rate_limit = rate_limit_from_headers(&response.headers);
    let media_type = header_value(&response.headers, "content-type")
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    let raw_id = format!("{request_id}-response");
    let raw = retain_raw(
        store,
        &auth.credentials,
        RawObject {
            raw_id,
            request_id: request_id.clone(),
            object_kind: if response.status / 100 == 2 {
                request.object_kind.clone()
            } else {
                format!("{}.error", request.object_kind)
            },
            canonicalization: "raw-bytes-v1".to_owned(),
            media_type,
            original_bytes: response.body.clone(),
            bytes: response.body,
        },
    )?;
    let raw_id = raw.raw_id.clone();
    let status = response.status;
    let (state, complete, error_raw_ref, truncation_reason) = if status / 100 == 2 {
        (AcquisitionState::Complete, true, None, None)
    } else {
        let state = http_state(status, rate_limit.as_ref());
        (
            state,
            false,
            Some(raw_id.clone()),
            Some(format!("HTTP status {status}")),
        )
    };
    let request_record = make_record(
        &request_id,
        &acquisition_request,
        auth,
        started_at_utc,
        utc_now(),
        Some(status),
        Some(&response.headers),
        PageState {
            number: 1,
            per_page: Some(request.per_page as u32),
            link_next: None,
            cursor_in: None,
            cursor_out: None,
            has_next_page: Some(false),
            items_returned: 0,
        },
        Some(raw_id),
        error_raw_ref,
        state,
        complete,
        truncation_reason,
        None,
        rate_limit,
    );
    Ok(CollectionResult {
        items: Vec::new(),
        requests: vec![request_record],
        raw_objects: vec![raw],
        state,
        complete,
    })
}

pub async fn collect_graphql<T, S>(
    transport: &T,
    store: &mut S,
    auth: &AuthIdentity,
    request: GraphQlCollectionRequest,
) -> Result<CollectionResult, AcquisitionError>
where
    T: AcquisitionTransport + ?Sized,
    S: RawObjectStore,
{
    auth.validate()?;
    validate_graphql_request(&request)?;
    let mut result = CollectionResult {
        items: Vec::new(),
        requests: Vec::new(),
        raw_objects: Vec::new(),
        state: AcquisitionState::Unknown,
        complete: false,
    };
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();

    for page_number in 1..=request.max_pages {
        let page_number_u32 = page_number as u32;
        let variables = graphql_variables(&request, cursor.as_deref())?;
        let body = graphql_body(&request.operation_name, &request.query, &variables)?;
        let endpoint = request.api_origin.bind(&request.endpoint)?;
        let acquisition_request = AcquisitionRequest {
            api: ApiKind::GraphQl,
            method: HttpMethod::Post,
            api_origin: request.api_origin.clone(),
            endpoint_or_operation: endpoint,
            query: BTreeMap::new(),
            accept: None,
            body: Some(body),
        };
        let request_id = format!("{}-{page_number_u32:04}", request.collection_id);
        let started_at_utc = utc_now();
        let cursor_in = cursor.clone();
        let response = transport.send(acquisition_request.clone()).await;
        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                let state = transport_failure_state(failure);
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    None,
                    None,
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    None,
                    None,
                    state,
                    false,
                    Some("transport failure".to_owned()),
                    Some((&variables, &request.query)),
                    None,
                ));
                result.state = state;
                result.complete = false;
                break;
            }
        };
        validate_effective_response(
            &request.api_origin,
            &acquisition_request,
            &response.effective_endpoint,
        )?;
        let rate_limit = rate_limit_from_headers(&response.headers);
        let media_type = header_value(&response.headers, "content-type")
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let raw_id = format!("{request_id}-response");
        let raw = retain_raw(
            store,
            &auth.credentials,
            RawObject {
                raw_id,
                request_id: request_id.clone(),
                object_kind: if response.status / 100 == 2 {
                    request.object_kind.clone()
                } else {
                    format!("{}.error", request.object_kind)
                },
                canonicalization: "raw-bytes-v1".to_owned(),
                media_type,
                original_bytes: response.body.clone(),
                bytes: response.body.clone(),
            },
        )?;
        let raw_id = raw.raw_id.clone();
        result.raw_objects.push(raw);
        let status = response.status;
        if status / 100 != 2 {
            let state = http_state(status, rate_limit.as_ref());
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.page_size as u32),
                    link_next: None,
                    cursor_in,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: 0,
                },
                Some(raw_id.clone()),
                Some(raw_id),
                state,
                false,
                Some(format!("HTTP status {status}")),
                Some((&variables, &request.query)),
                rate_limit,
            ));
            result.state = state;
            result.complete = false;
            break;
        }
        let body_value = match serde_json::from_slice::<Value>(&response.body) {
            Ok(body_value) => body_value,
            Err(_) => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("invalid GraphQL JSON response".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        if body_value
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.page_size as u32),
                    link_next: None,
                    cursor_in,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: 0,
                },
                Some(raw_id.clone()),
                Some(raw_id),
                AcquisitionState::GraphQlError,
                false,
                Some("GraphQL errors returned".to_owned()),
                Some((&variables, &request.query)),
                rate_limit,
            ));
            result.state = AcquisitionState::GraphQlError;
            result.complete = false;
            break;
        }
        let page_items = match value_at_path(&body_value, &request.node_path)
            .and_then(Value::as_array)
            .cloned()
        {
            Some(items) => items,
            None => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: 0,
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("GraphQL nodes path missing or not an array".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        if page_items.len() > request.page_size {
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.page_size as u32),
                    link_next: None,
                    cursor_in,
                    cursor_out: None,
                    has_next_page: None,
                    items_returned: page_items.len(),
                },
                Some(raw_id.clone()),
                Some(raw_id),
                AcquisitionState::Malformed,
                false,
                Some("GraphQL page exceeded requested page size".to_owned()),
                Some((&variables, &request.query)),
                rate_limit,
            ));
            result.state = AcquisitionState::Malformed;
            result.complete = false;
            break;
        }
        let mut page_info_path = request.node_path.clone();
        let Some(last) = page_info_path.last_mut() else {
            return Err(AcquisitionError::InvalidRequest);
        };
        *last = "pageInfo".to_owned();
        let page_info = match value_at_path(&body_value, &page_info_path).and_then(Value::as_object)
        {
            Some(page_info) => page_info,
            None => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: page_items.len(),
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("GraphQL pageInfo path missing".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        let has_next_page = match page_info.get("hasNextPage").and_then(Value::as_bool) {
            Some(value) => value,
            None => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: None,
                        items_returned: page_items.len(),
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("GraphQL pageInfo.hasNextPage missing".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        let cursor_out = match page_info.get("endCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    PageState {
                        number: page_number_u32,
                        per_page: Some(request.page_size as u32),
                        link_next: None,
                        cursor_in,
                        cursor_out: None,
                        has_next_page: Some(has_next_page),
                        items_returned: page_items.len(),
                    },
                    Some(raw_id.clone()),
                    Some(raw_id),
                    AcquisitionState::Malformed,
                    false,
                    Some("GraphQL pageInfo.endCursor has invalid type".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Malformed;
                result.complete = false;
                break;
            }
        };
        let page_items_len = page_items.len();
        if has_next_page && cursor_out.is_none() {
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                PageState {
                    number: page_number_u32,
                    per_page: Some(request.page_size as u32),
                    link_next: None,
                    cursor_in,
                    cursor_out,
                    has_next_page: Some(true),
                    items_returned: page_items_len,
                },
                Some(raw_id.clone()),
                Some(raw_id),
                AcquisitionState::Truncated,
                false,
                Some("GraphQL next page omitted endCursor".to_owned()),
                Some((&variables, &request.query)),
                rate_limit,
            ));
            result.state = AcquisitionState::Truncated;
            result.complete = false;
            break;
        }
        result.items.extend(page_items);
        let page = PageState {
            number: page_number_u32,
            per_page: Some(request.page_size as u32),
            link_next: None,
            cursor_in,
            cursor_out: cursor_out.clone(),
            has_next_page: Some(has_next_page),
            items_returned: page_items_len,
        };
        if has_next_page {
            let Some(next_cursor) = cursor_out else {
                return Err(AcquisitionError::InvalidRequest);
            };
            if page_number >= request.max_pages {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    page,
                    Some(raw_id.clone()),
                    None,
                    AcquisitionState::Truncated,
                    false,
                    Some("collector page cap reached".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::Truncated;
                result.complete = false;
                break;
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                result.requests.push(make_record(
                    &request_id,
                    &acquisition_request,
                    auth,
                    started_at_utc,
                    utc_now(),
                    Some(status),
                    Some(&response.headers),
                    page,
                    Some(raw_id.clone()),
                    None,
                    AcquisitionState::DuplicateCursor,
                    false,
                    Some("duplicate GraphQL endCursor".to_owned()),
                    Some((&variables, &request.query)),
                    rate_limit,
                ));
                result.state = AcquisitionState::DuplicateCursor;
                result.complete = false;
                break;
            }
            result.requests.push(make_record(
                &request_id,
                &acquisition_request,
                auth,
                started_at_utc,
                utc_now(),
                Some(status),
                Some(&response.headers),
                page,
                Some(raw_id),
                None,
                AcquisitionState::Complete,
                false,
                None,
                Some((&variables, &request.query)),
                rate_limit,
            ));
            cursor = Some(next_cursor);
            continue;
        }
        let state = if result.items.is_empty() {
            AcquisitionState::EmptyComplete
        } else {
            AcquisitionState::Complete
        };
        result.requests.push(make_record(
            &request_id,
            &acquisition_request,
            auth,
            started_at_utc,
            utc_now(),
            Some(status),
            Some(&response.headers),
            page,
            Some(raw_id),
            None,
            state,
            true,
            None,
            Some((&variables, &request.query)),
            rate_limit,
        ));
        result.state = state;
        result.complete = true;
        break;
    }
    Ok(result)
}

/// Revision tuple retained before and after a live evidence set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionIdentity {
    pub repository: String,
    pub subject: String,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub tested_merge_sha: Option<String>,
    pub merge_group_sha: Option<String>,
}

impl RevisionIdentity {
    fn key(&self) -> String {
        format!("{}:{}", self.repository, self.subject)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityChangeKind {
    Added,
    Removed,
    Rebound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityChange {
    pub key: String,
    pub kind: IdentityChangeKind,
    pub opening: Option<RevisionIdentity>,
    pub closing: Option<RevisionIdentity>,
}

/// Both full identity sets are retained.  `stable=false` invalidates the
/// affected evidence independently of aggregate check/run conclusions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityReconciliation {
    pub opening: Vec<RevisionIdentity>,
    pub closing: Vec<RevisionIdentity>,
    pub changes: Vec<IdentityChange>,
    pub duplicate_keys: Vec<String>,
    pub stable: bool,
}

pub fn reconcile_identity_sets(
    opening: Vec<RevisionIdentity>,
    closing: Vec<RevisionIdentity>,
) -> IdentityReconciliation {
    let (opening_index, mut duplicate_keys) = index_identities(&opening);
    let (closing_index, closing_duplicates) = index_identities(&closing);
    duplicate_keys.extend(closing_duplicates);
    duplicate_keys.sort();
    duplicate_keys.dedup();
    let keys = opening_index
        .keys()
        .chain(closing_index.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let changes = keys
        .into_iter()
        .filter_map(|key| {
            let opening_value = opening_index.get(&key).cloned();
            let closing_value = closing_index.get(&key).cloned();
            let kind = match (&opening_value, &closing_value) {
                (None, Some(_)) => IdentityChangeKind::Added,
                (Some(_), None) => IdentityChangeKind::Removed,
                (Some(left), Some(right)) if left != right => IdentityChangeKind::Rebound,
                _ => return None,
            };
            Some(IdentityChange {
                key,
                kind,
                opening: opening_value,
                closing: closing_value,
            })
        })
        .collect::<Vec<_>>();
    IdentityReconciliation {
        opening,
        closing,
        stable: changes.is_empty() && duplicate_keys.is_empty(),
        changes,
        duplicate_keys,
    }
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn validate_rest_request(request: &RestCollectionRequest) -> Result<(), AcquisitionError> {
    if request.collection_id.trim().is_empty()
        || request.endpoint.trim().is_empty()
        || request.object_kind.trim().is_empty()
        || request.per_page == 0
        || request.max_pages == 0
        || request.per_page > u32::MAX as usize
        || request.max_pages > u32::MAX as usize
    {
        return Err(AcquisitionError::InvalidRequest);
    }
    if request
        .query
        .iter()
        .any(|(key, value)| key_is_sensitive(key) || looks_like_secret(value))
    {
        return Err(AcquisitionError::EndpointViolation);
    }
    if request.accept.as_deref().is_some_and(|accept| {
        !matches!(
            accept,
            "application/vnd.github+json" | "application/vnd.github.raw+json"
        )
    }) {
        return Err(AcquisitionError::InvalidRequest);
    }
    Ok(())
}

fn validate_graphql_request(request: &GraphQlCollectionRequest) -> Result<(), AcquisitionError> {
    if request.collection_id.trim().is_empty()
        || request.endpoint.trim().is_empty()
        || request.operation_name.trim().is_empty()
        || request.query.trim().is_empty()
        || request.object_kind.trim().is_empty()
        || request.page_size == 0
        || request.max_pages == 0
        || request.page_size > u32::MAX as usize
        || request.max_pages > u32::MAX as usize
        || request.node_path.is_empty()
        || request.cursor_variable.trim().is_empty()
        || request.page_size_variable.trim().is_empty()
        || !request.variables.is_object()
        || looks_like_secret(&request.query)
        || contains_sensitive_value(&request.variables)
    {
        return Err(
            if looks_like_secret(&request.query) || contains_sensitive_value(&request.variables) {
                AcquisitionError::SecretMetadata
            } else {
                AcquisitionError::InvalidRequest
            },
        );
    }
    Ok(())
}

fn graphql_variables(
    request: &GraphQlCollectionRequest,
    cursor: Option<&str>,
) -> Result<Value, AcquisitionError> {
    let mut variables = request.variables.clone();
    let Some(object) = variables.as_object_mut() else {
        return Err(AcquisitionError::InvalidRequest);
    };
    object.insert(
        request.cursor_variable.clone(),
        cursor.map_or(Value::Null, |value| Value::String(value.to_owned())),
    );
    let page_size =
        u64::try_from(request.page_size).map_err(|_| AcquisitionError::InvalidRequest)?;
    object.insert(
        request.page_size_variable.clone(),
        Value::Number(page_size.into()),
    );
    Ok(variables)
}

fn graphql_body(
    operation_name: &str,
    query: &str,
    variables: &Value,
) -> Result<Vec<u8>, AcquisitionError> {
    let mut body = Map::new();
    body.insert(
        "operationName".to_owned(),
        Value::String(operation_name.to_owned()),
    );
    body.insert("query".to_owned(), Value::String(query.to_owned()));
    body.insert("variables".to_owned(), variables.clone());
    canonical_json_bytes(&Value::Object(body))
}

fn rest_items(body: &Value, item_field: Option<&str>) -> Result<Vec<Value>, AcquisitionError> {
    if item_field == Some("$object") {
        return Ok(vec![body.clone()]);
    }
    let array = item_field.map_or(Some(body), |field| body.get(field));
    array
        .and_then(Value::as_array)
        .cloned()
        .ok_or(AcquisitionError::InvalidRequest)
}

fn value_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

struct MaskedBytes {
    bytes: Vec<u8>,
    changed: bool,
}

fn mask_response_bytes(
    credentials: &CredentialRegistry,
    bytes: &[u8],
) -> Result<MaskedBytes, AcquisitionError> {
    let mut masked = bytes.to_vec();
    let mut changed = false;
    for secret in &credentials.secrets {
        let (next, replaced) = replace_bytes(&masked, secret, b"[REDACTED]");
        masked = next;
        changed |= replaced;
    }
    let (next, known_replaced) = mask_known_credential_markers(&masked);
    masked = next;
    changed |= known_replaced;

    if let Ok(value) = serde_json::from_slice::<Value>(&masked) {
        let redacted = redact_value(&value);
        if redacted != value {
            masked = serde_json::to_vec(&redacted).map_err(|_| AcquisitionError::Serialization)?;
            changed = true;
        }
    }
    if contains_unmasked_credential(&masked) {
        return Err(AcquisitionError::CredentialMaterialDetected);
    }
    Ok(MaskedBytes {
        bytes: masked,
        changed,
    })
}

fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> (Vec<u8>, bool) {
    if needle.is_empty() || needle.len() > input.len() {
        return (input.to_vec(), false);
    }
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    let mut changed = false;
    while cursor < input.len() {
        let remaining = &input[cursor..];
        if remaining.starts_with(needle) {
            output.extend_from_slice(replacement);
            cursor += needle.len();
            changed = true;
        } else {
            output.push(input[cursor]);
            cursor += 1;
        }
    }
    (output, changed)
}

fn mask_known_credential_markers(input: &[u8]) -> (Vec<u8>, bool) {
    const AUTHORIZATION: &[u8] = b"authorization:";
    const GH_TOKEN_PREFIXES: [&[u8]; 6] =
        [b"ghp_", b"ghs_", b"gho_", b"ghu_", b"ghr_", b"github_pat_"];
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    let mut changed = false;
    while cursor < input.len() {
        if starts_case_insensitive(input, cursor, AUTHORIZATION) {
            output.extend_from_slice(&input[cursor..cursor + AUTHORIZATION.len()]);
            output.extend_from_slice(b" [REDACTED]");
            cursor += AUTHORIZATION.len();
            while cursor < input.len() && !matches!(input[cursor], b'\r' | b'\n') {
                cursor += 1;
            }
            changed = true;
            continue;
        }
        if let Some(prefix) = GH_TOKEN_PREFIXES
            .iter()
            .find(|prefix| input[cursor..].starts_with(prefix))
        {
            output.extend_from_slice(b"[REDACTED]");
            cursor += prefix.len();
            while cursor < input.len()
                && (input[cursor].is_ascii_alphanumeric() || matches!(input[cursor], b'_' | b'-'))
            {
                cursor += 1;
            }
            changed = true;
            continue;
        }
        output.push(input[cursor]);
        cursor += 1;
    }
    (output, changed)
}

fn starts_case_insensitive(input: &[u8], offset: usize, needle: &[u8]) -> bool {
    input
        .get(offset..offset.saturating_add(needle.len()))
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
}

fn contains_unmasked_credential(input: &[u8]) -> bool {
    const GH_TOKEN_PREFIXES: [&[u8]; 6] =
        [b"ghp_", b"ghs_", b"gho_", b"ghu_", b"ghr_", b"github_pat_"];
    if GH_TOKEN_PREFIXES.iter().any(|prefix| {
        input
            .windows(prefix.len())
            .any(|window| window.eq_ignore_ascii_case(prefix))
    }) {
        return true;
    }
    let marker = b"authorization:";
    let mut offset = 0;
    while let Some(relative) = input[offset..]
        .windows(marker.len())
        .position(|window| window.eq_ignore_ascii_case(marker))
    {
        let start = offset + relative + marker.len();
        let end = input[start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(input.len(), |length| start + length);
        let value = input[start..end]
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        if !value.is_empty() && value != b"[REDACTED]" {
            return true;
        }
        if end >= input.len() {
            break;
        }
        offset = end;
    }
    false
}

fn retain_raw<S: RawObjectStore>(
    store: &mut S,
    credentials: &CredentialRegistry,
    mut object: RawObject,
) -> Result<RawObjectRef, AcquisitionError> {
    let masked = mask_response_bytes(credentials, &object.bytes)?;
    object.bytes = masked.bytes;
    if masked.changed {
        object.canonicalization = "redacted-raw-bytes-v1".to_owned();
    }
    let expected_original_sha256 = sha256_digest(&object.original_bytes);
    let expected_original_length = object.original_bytes.len() as u64;
    let expected_sha256 = sha256_digest(&object.bytes);
    let expected_length = object.bytes.len() as u64;
    let expected_id = object.raw_id.clone();
    let expected_request = object.request_id.clone();
    let expected_kind = object.object_kind.clone();
    let expected_canonicalization = object.canonicalization.clone();
    let expected_media_type = object.media_type.clone();
    let expected_bytes_base64 = BASE64.encode(&object.bytes);
    let reference = store.store(object).map_err(|error| match error {
        RawStorageError::Unavailable => AcquisitionError::StorageUnavailable,
        RawStorageError::Refused => AcquisitionError::StorageRefused,
        RawStorageError::Unbound => AcquisitionError::StorageUnbound,
    })?;
    if reference.raw_id != expected_id
        || reference.request_id != expected_request
        || reference.object_kind != expected_kind
        || reference.canonicalization != expected_canonicalization
        || reference.media_type != expected_media_type
        || reference.byte_length != expected_length
        || reference.original_sha256 != expected_original_sha256
        || reference.original_byte_length != expected_original_length
        || reference.bytes_base64 != expected_bytes_base64
    {
        return Err(AcquisitionError::RawReferenceMismatch);
    }
    if reference.sha256 != expected_sha256 {
        return Err(AcquisitionError::RawDigestMismatch);
    }
    if reference.storage_ref != content_addressed_storage_ref(&expected_sha256) {
        return Err(AcquisitionError::StorageUnbound);
    }
    if reference.original_storage_ref != content_addressed_storage_ref(&reference.original_sha256)
        || !reference.original_sha256.starts_with("sha256:")
    {
        return Err(AcquisitionError::StorageUnbound);
    }
    store.verify(&reference).map_err(|error| match error {
        RawStorageError::Unavailable => AcquisitionError::StorageUnavailable,
        RawStorageError::Refused => AcquisitionError::StorageRefused,
        RawStorageError::Unbound => AcquisitionError::StorageUnbound,
    })?;
    Ok(reference)
}

#[allow(clippy::too_many_arguments)]
fn make_record(
    request_id: &str,
    request: &AcquisitionRequest,
    auth: &AuthIdentity,
    started_at_utc: String,
    completed_at_utc: String,
    http_status: Option<u16>,
    headers: Option<&BTreeMap<String, String>>,
    page: PageState,
    response_raw_ref: Option<String>,
    error_raw_ref: Option<String>,
    state: AcquisitionState,
    complete: bool,
    truncation_reason: Option<String>,
    graphql_inputs: Option<(&Value, &str)>,
    rate_limit: Option<RateLimitObservation>,
) -> RequestRecord {
    let (query_base64, variables_base64, query_sha256, variables_sha256, redacted_variables) =
        match graphql_inputs {
            Some((variables, query)) => {
                let variable_bytes = canonical_json_bytes(variables);
                (
                    BASE64.encode(query.as_bytes()),
                    variable_bytes
                        .as_ref()
                        .map_or_else(|_| String::new(), |bytes| BASE64.encode(bytes)),
                    Some(query_digest(query)),
                    Some(variable_bytes.map_or_else(
                        |_| "sha256:serialization-error".to_owned(),
                        |bytes| sha256_digest(&bytes),
                    )),
                    Some(redact_value(variables)),
                )
            }
            None => {
                let canonical_query = canonical_query(&request.query);
                (
                    BASE64.encode(canonical_query.as_bytes()),
                    String::new(),
                    Some(query_digest(&canonical_query)),
                    Some(sha256_digest(b"")),
                    None,
                )
            }
        };
    RequestRecord {
        request_id: request_id.to_owned(),
        api: request.api,
        method: request.method,
        endpoint_or_operation: request.endpoint_or_operation.clone(),
        query_base64,
        variables_base64,
        query_sha256,
        variables_sha256,
        redacted_variables,
        auth_identity_ref: auth.reference().to_owned(),
        started_at_utc,
        completed_at_utc,
        http_status,
        api_request_id: headers.and_then(|map| {
            header_value(map, "x-github-request-id").or_else(|| header_value(map, "x-request-id"))
        }),
        rate_limit,
        page,
        response_raw_ref,
        error_raw_ref,
        state,
        complete,
        truncation_reason,
        safe_scopes: headers.and_then(parse_safe_scopes),
    }
}

fn parse_safe_scopes(headers: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let value = header_value(headers, "x-oauth-scopes")?;
    let scopes = value
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty() && !looks_like_secret(scope))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!scopes.is_empty()).then_some(scopes)
}

fn canonical_query(query: &BTreeMap<String, String>) -> String {
    let mut canonical = String::new();
    for (key, value) in query {
        canonical.push_str(key);
        canonical.push('\0');
        canonical.push_str(value);
        canonical.push('\0');
    }
    canonical
}

fn canonical_page_key(
    endpoint: &str,
    query: &BTreeMap<String, String>,
) -> Result<String, AcquisitionError> {
    let parsed = Url::parse(endpoint).map_err(|_| AcquisitionError::EndpointViolation)?;
    let mut pairs = url_query_map(&parsed)?;
    for (key, value) in query {
        if pairs.insert(key.clone(), value.clone()).is_some() {
            return Err(AcquisitionError::EndpointViolation);
        }
    }
    let host = parsed
        .host_str()
        .ok_or(AcquisitionError::EndpointViolation)?;
    let mut canonical = format!("{}://{}{}", parsed.scheme(), host, parsed.path());
    if let Some(port) = parsed.port() {
        canonical.push(':');
        canonical.push_str(&port.to_string());
    }
    canonical.push('?');
    for (key, value) in pairs {
        canonical.push_str(&key);
        canonical.push('\0');
        canonical.push_str(&value);
        canonical.push('\0');
    }
    Ok(canonical)
}

fn query_digest(query_or_document: &str) -> String {
    sha256_digest(query_or_document.as_bytes())
}

fn url_query_map(url: &Url) -> Result<BTreeMap<String, String>, AcquisitionError> {
    let Some(raw_query) = url.query() else {
        return Ok(BTreeMap::new());
    };
    if raw_query.is_empty() || raw_query.contains('%') {
        return Err(AcquisitionError::EndpointViolation);
    }
    let mut pairs = BTreeMap::new();
    for part in raw_query.split('&') {
        let (key, value) = part
            .split_once('=')
            .ok_or(AcquisitionError::EndpointViolation)?;
        if key.is_empty()
            || key_is_sensitive(key)
            || looks_like_secret(value)
            || pairs.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return Err(AcquisitionError::EndpointViolation);
        }
    }
    Ok(pairs)
}

fn next_page_request(
    api_origin: &GithubApiOrigin,
    link: &str,
    current_endpoint: &str,
    current_query: &BTreeMap<String, String>,
    next_page: u32,
) -> Result<(String, BTreeMap<String, String>, String), AcquisitionError> {
    let mut next = Url::parse(link).map_err(|_| AcquisitionError::EndpointViolation)?;
    api_origin.validate_candidate(&next)?;
    let current = Url::parse(current_endpoint).map_err(|_| AcquisitionError::EndpointViolation)?;
    if next.path() != current.path() || next.query().is_none() || next.fragment().is_some() {
        return Err(AcquisitionError::EndpointViolation);
    }
    let mut expected = current_query.clone();
    expected.remove("page");
    expected.insert("page".to_owned(), next_page.to_string());
    let link_query = url_query_map(&next)?;
    if link_query != expected {
        return Err(AcquisitionError::EndpointViolation);
    }
    let normalized_link = next.to_string();
    next.set_query(None);
    Ok((next.to_string(), link_query, normalized_link))
}

fn validate_effective_response(
    api_origin: &GithubApiOrigin,
    request: &AcquisitionRequest,
    effective_endpoint: &str,
) -> Result<(), AcquisitionError> {
    let expected = api_origin.bind(&request.endpoint_or_operation)?;
    let expected = Url::parse(&expected).map_err(|_| AcquisitionError::EndpointViolation)?;
    let actual = Url::parse(effective_endpoint).map_err(|_| AcquisitionError::EndpointViolation)?;
    api_origin.validate_candidate(&actual)?;
    if actual.path() != expected.path() {
        return Err(AcquisitionError::EndpointViolation);
    }
    let mut expected_query = url_query_map(&expected)?;
    for (key, value) in &request.query {
        if expected_query.insert(key.clone(), value.clone()).is_some() {
            return Err(AcquisitionError::EndpointViolation);
        }
    }
    if url_query_map(&actual)? != expected_query {
        return Err(AcquisitionError::EndpointViolation);
    }
    Ok(())
}

/// Build the one canonical URI used by both the producer store and checker
/// contract.  The digest itself must already come from measured bytes.
pub(crate) fn content_addressed_storage_ref(digest: &str) -> String {
    let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
    format!("sha256://{digest}")
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, AcquisitionError> {
    let canonical = canonicalize_value(value);
    serde_json::to_vec(&canonical).map_err(|_| AcquisitionError::Serialization)
}

fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in object {
                sorted.insert(key.clone(), canonicalize_value(value));
            }
            let mut output = Map::new();
            for (key, value) in sorted {
                output.insert(key, value);
            }
            Value::Object(output)
        }
        Value::Array(array) => Value::Array(array.iter().map(canonicalize_value).collect()),
        other => other.clone(),
    }
}

fn parse_next_link(header: &str) -> Result<Option<String>, AcquisitionError> {
    let mut next = None;
    for segment in header.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(AcquisitionError::InvalidRequest);
        }
        let Some(start) = segment.find('<') else {
            return Err(AcquisitionError::InvalidRequest);
        };
        let Some(end_relative) = segment[start + 1..].find('>') else {
            return Err(AcquisitionError::InvalidRequest);
        };
        let end = start + 1 + end_relative;
        let target = &segment[start + 1..end];
        if target.trim().is_empty() {
            return Err(AcquisitionError::InvalidRequest);
        }
        let has_next_relation = segment[end + 1..].split(';').any(|attribute| {
            let attribute = attribute.trim();
            attribute == "rel=\"next\"" || attribute == "rel=next"
        });
        if has_next_relation && next.replace(target.to_owned()).is_some() {
            return Err(AcquisitionError::InvalidRequest);
        }
    }
    Ok(next)
}

fn header_value(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn rate_limit_from_headers(headers: &BTreeMap<String, String>) -> Option<RateLimitObservation> {
    let limit = header_value(headers, "x-ratelimit-limit").and_then(|value| value.parse().ok());
    let remaining =
        header_value(headers, "x-ratelimit-remaining").and_then(|value| value.parse().ok());
    let used = header_value(headers, "x-ratelimit-used").and_then(|value| value.parse().ok());
    let reset_at = header_value(headers, "x-ratelimit-reset");
    let retry_after = header_value(headers, "retry-after");
    if limit.is_none()
        && remaining.is_none()
        && used.is_none()
        && reset_at.is_none()
        && retry_after.is_none()
    {
        None
    } else {
        Some(RateLimitObservation {
            limit,
            remaining,
            used,
            reset_at,
            retry_after,
        })
    }
}

fn http_state(status: u16, rate_limit: Option<&RateLimitObservation>) -> AcquisitionState {
    if status == 401 || status == 403 {
        if rate_limit.is_some_and(|rate| rate.remaining == Some(0) || rate.retry_after.is_some()) {
            AcquisitionState::RateLimited
        } else {
            AcquisitionState::Forbidden
        }
    } else if status == 404 {
        AcquisitionState::NotFound
    } else if status == 429 {
        AcquisitionState::RateLimited
    } else if status >= 500 {
        AcquisitionState::TransportError
    } else {
        AcquisitionState::Unknown
    }
}

fn transport_failure_state(failure: TransportFailure) -> AcquisitionState {
    match failure {
        TransportFailure::PermissionDenied => AcquisitionState::Forbidden,
        TransportFailure::RateLimited => AcquisitionState::RateLimited,
        TransportFailure::Timeout | TransportFailure::Connection | TransportFailure::Other => {
            AcquisitionState::TransportError
        }
    }
}

fn utc_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut redacted = Map::new();
            for (key, value) in object {
                if key_is_sensitive(key) {
                    redacted.insert(key.clone(), Value::String("[REDACTED]".to_owned()));
                } else {
                    redacted.insert(key.clone(), redact_value(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(array) => Value::Array(array.iter().map(redact_value).collect()),
        Value::String(value) if looks_like_secret(value) => Value::String("[REDACTED]".to_owned()),
        other => other.clone(),
    }
}

fn contains_sensitive_value(value: &Value) -> bool {
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, value)| key_is_sensitive(key) || contains_sensitive_value(value)),
        Value::Array(array) => array.iter().any(contains_sensitive_value),
        Value::String(value) => looks_like_secret(value),
        _ => false,
    }
}

fn redacted_query(query: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    query
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if key_is_sensitive(key) || looks_like_secret(value) {
                    "[REDACTED]".to_owned()
                } else {
                    value.clone()
                },
            )
        })
        .collect()
}

fn redacted_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if key_is_sensitive(key) || looks_like_secret(value) {
                    "[REDACTED]".to_owned()
                } else {
                    value.clone()
                },
            )
        })
        .collect()
}

fn redacted_endpoint(endpoint: &str) -> String {
    if looks_like_secret(endpoint) {
        return "[REDACTED_ENDPOINT]".to_owned();
    }
    let Ok(mut url) = Url::parse(endpoint) else {
        return "[INVALID_ENDPOINT]".to_owned();
    };
    if !url.username().is_empty() || url.password().is_some() {
        return "[REDACTED_ENDPOINT]".to_owned();
    }
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let key = key.into_owned();
            let value = value.into_owned();
            (
                key.clone(),
                if key_is_sensitive(&key) {
                    "[REDACTED]".to_owned()
                } else {
                    value
                },
            )
        })
        .collect::<Vec<_>>();
    if pairs.iter().any(|(_, value)| looks_like_secret(value)) {
        return "[REDACTED_ENDPOINT]".to_owned();
    }
    url.set_query(None);
    if !pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }
    url.to_string()
}

fn key_is_sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn looks_like_secret(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    ["ghp_", "ghs_", "gho_", "ghu_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| value.contains(prefix))
        || value.contains("bearer ")
        || value.contains("authorization:")
        || value.contains("x-oauth-basic")
}

fn index_identities(
    identities: &[RevisionIdentity],
) -> (BTreeMap<String, RevisionIdentity>, Vec<String>) {
    let mut indexed = BTreeMap::new();
    let mut duplicates = Vec::new();
    for identity in identities {
        let key = identity.key();
        if indexed.insert(key.clone(), identity.clone()).is_some() {
            duplicates.push(key);
        }
    }
    (indexed, duplicates)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FixtureTransport {
        requests: Arc<Mutex<Vec<AcquisitionRequest>>>,
        responses: Arc<Mutex<VecDeque<Result<TransportResponse, TransportFailure>>>>,
    }

    impl FixtureTransport {
        fn new(responses: Vec<Result<TransportResponse, TransportFailure>>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            }
        }

        fn requests(&self) -> Vec<AcquisitionRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl AcquisitionTransport for FixtureTransport {
        fn send<'a>(
            &'a self,
            request: AcquisitionRequest,
        ) -> AcquisitionFuture<'a, Result<TransportResponse, TransportFailure>> {
            self.requests.lock().unwrap().push(request);
            let request = self.requests.lock().unwrap().last().cloned().unwrap();
            let mut response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportFailure::Other));
            if let Ok(response) = &mut response
                && response.effective_endpoint == "https://api.github.com"
            {
                let endpoint = request
                    .api_origin
                    .bind(&request.endpoint_or_operation)
                    .expect("fixture request endpoint");
                let mut url = Url::parse(&endpoint).expect("fixture request URL");
                for (key, value) in &request.query {
                    url.query_pairs_mut().append_pair(key, value);
                }
                response.effective_endpoint = url.to_string();
            }
            Box::pin(async move { response })
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        refs: Vec<RawObjectRef>,
        bytes: Vec<Vec<u8>>,
    }

    impl RawObjectStore for MemoryStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let bytes = object.bytes.clone();
            let original_digest = sha256_digest(&object.original_bytes);
            let reference = RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: sha256_digest(&bytes),
                byte_length: bytes.len() as u64,
                original_sha256: original_digest.clone(),
                original_byte_length: object.original_bytes.len() as u64,
                bytes_base64: BASE64.encode(&bytes),
                media_type: object.media_type,
                storage_ref: content_addressed_storage_ref(&sha256_digest(&bytes)),
                original_storage_ref: content_addressed_storage_ref(&original_digest),
            };
            self.bytes.push(bytes);
            self.refs.push(reference.clone());
            Ok(reference)
        }

        fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
            if self.refs.contains(reference) {
                Ok(())
            } else {
                Err(RawStorageError::Unbound)
            }
        }
    }

    struct TamperStore;

    impl RawObjectStore for TamperStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let original_digest = sha256_digest(&object.original_bytes);
            Ok(RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: "sha256:tampered".to_owned(),
                byte_length: object.bytes.len() as u64,
                original_sha256: original_digest.clone(),
                original_byte_length: object.original_bytes.len() as u64,
                bytes_base64: BASE64.encode(&object.bytes),
                media_type: object.media_type,
                storage_ref: "sha256://tampered".to_owned(),
                original_storage_ref: content_addressed_storage_ref(&original_digest),
            })
        }

        fn verify(&self, _reference: &RawObjectRef) -> Result<(), RawStorageError> {
            Ok(())
        }
    }

    struct UnboundStore;

    impl RawObjectStore for UnboundStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let digest = sha256_digest(&object.bytes);
            let original_digest = sha256_digest(&object.original_bytes);
            Ok(RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: digest,
                byte_length: object.bytes.len() as u64,
                original_sha256: original_digest.clone(),
                original_byte_length: object.original_bytes.len() as u64,
                bytes_base64: BASE64.encode(&object.bytes),
                media_type: object.media_type,
                storage_ref: "store://unbound/caller-asserted".to_owned(),
                original_storage_ref: content_addressed_storage_ref(&original_digest),
            })
        }

        fn verify(&self, _reference: &RawObjectRef) -> Result<(), RawStorageError> {
            Ok(())
        }
    }

    fn auth() -> AuthIdentity {
        AuthIdentity::new(
            "github-viewer",
            "github",
            Some("viewer-1".to_owned()),
            Some("fixture".to_owned()),
            ["metadata:read".to_owned(), "actions:read".to_owned()]
                .into_iter()
                .collect(),
        )
    }

    fn response(status: u16, headers: &[(&str, &str)], body: Value) -> TransportResponse {
        raw_response(
            status,
            headers,
            serde_json::to_vec(&body).unwrap(),
            "https://api.github.com",
        )
    }

    fn raw_response(
        status: u16,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        effective_endpoint: &str,
    ) -> TransportResponse {
        TransportResponse {
            status,
            headers: headers
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            body,
            effective_endpoint: effective_endpoint.to_owned(),
        }
    }

    fn rest_items(count: usize, start: usize) -> Value {
        Value::Object(Map::from_iter([(
            "items".to_owned(),
            Value::Array(
                (start..start + count)
                    .map(|value| Value::Number((value as u64).into()))
                    .collect(),
            ),
        )]))
    }

    fn graphql_page(count: usize, start: usize, has_next: bool, cursor: Option<&str>) -> Value {
        let mut page_info = Map::new();
        page_info.insert("hasNextPage".to_owned(), Value::Bool(has_next));
        page_info.insert(
            "endCursor".to_owned(),
            cursor.map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
        let nodes = Value::Array(
            (start..start + count)
                .map(|value| {
                    Value::Object(Map::from_iter([(
                        "number".to_owned(),
                        Value::Number((value as u64).into()),
                    )]))
                })
                .collect(),
        );
        let mut pull_requests = Map::new();
        pull_requests.insert("nodes".to_owned(), nodes);
        pull_requests.insert("pageInfo".to_owned(), Value::Object(page_info));
        let mut repository = Map::new();
        repository.insert("pullRequests".to_owned(), Value::Object(pull_requests));
        let mut data = Map::new();
        data.insert("repository".to_owned(), Value::Object(repository));
        Value::Object(Map::from_iter([("data".to_owned(), Value::Object(data))]))
    }

    fn graphql_request() -> GraphQlCollectionRequest {
        GraphQlCollectionRequest::new(
            "prs",
            "https://api.github.com/graphql",
            "PullRequests",
            "query PullRequests($first: Int!, $after: String) { repository { pullRequests(first: $first, after: $after) { nodes { number } pageInfo { hasNextPage endCursor } } } }",
            serde_json::json!({"first": 100, "after": null}),
            ["data", "repository", "pullRequests", "nodes"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            "pull_requests",
        )
    }

    #[tokio::test]
    async fn rest_link_pagination_retains_all_items_and_raw_refs() {
        let transport = FixtureTransport::new(vec![
            Ok(response(
                200,
                &[
                    ("content-type", "application/json"),
                    ("x-github-request-id", "r1"),
                    (
                        "link",
                        "<https://api.github.com/items?page=2&per_page=100>; rel=\"next\"",
                    ),
                ],
                rest_items(100, 0),
            )),
            Ok(response(
                200,
                &[("content-type", "application/json")],
                rest_items(1, 100),
            )),
        ]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items")
            .with_query("per_page", "100");
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(result.complete);
        assert_eq!(result.state, AcquisitionState::Complete);
        assert_eq!(result.items.len(), 101);
        assert_eq!(result.requests.len(), 2);
        assert_eq!(result.raw_objects.len(), 2);
        assert_eq!(result.requests[0].page.items_returned, 100);
        assert_eq!(
            result.requests[0].page.link_next.as_deref(),
            Some("https://api.github.com/items?page=2&per_page=100")
        );
        let first_request = &transport.requests()[0];
        assert_eq!(
            first_request.query.get("page").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            result.requests[0].query_sha256,
            Some(sha256_digest(b"page\01\0per_page\0100\0"))
        );
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn rest_page_two_permission_failure_is_not_empty_success() {
        let transport = FixtureTransport::new(vec![
            Ok(response(
                200,
                &[(
                    "link",
                    "<https://api.github.com/items?page=2&per_page=100>; rel=\"next\"",
                )],
                rest_items(100, 0),
            )),
            Ok(response(
                403,
                &[("x-ratelimit-remaining", "42")],
                serde_json::json!({"message":"forbidden"}),
            )),
        ]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.state, AcquisitionState::Forbidden);
        assert_eq!(result.items.len(), 100);
        assert_eq!(
            result.requests[1].error_raw_ref,
            result.requests[1].response_raw_ref
        );
    }

    #[tokio::test]
    async fn rest_full_page_without_next_link_is_truncated() {
        let transport = FixtureTransport::new(vec![Ok(response(200, &[], rest_items(100, 0)))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.state, AcquisitionState::Truncated);
    }

    #[tokio::test]
    async fn rest_rate_limit_is_typed_and_retains_error_body() {
        let transport = FixtureTransport::new(vec![Ok(response(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-limit", "5000"),
            ],
            serde_json::json!({"message":"rate limit"}),
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert_eq!(result.state, AcquisitionState::RateLimited);
        assert_eq!(
            result.requests[0]
                .rate_limit
                .as_ref()
                .and_then(|rate| rate.remaining),
            Some(0)
        );
        assert_eq!(result.raw_objects.len(), 1);
    }

    #[tokio::test]
    async fn evil_rest_next_link_is_rejected_before_following() {
        let transport = FixtureTransport::new(vec![Ok(response(
            200,
            &[(
                "link",
                "<https://evil.example/items?page=2&token=ghp_secret>; rel=\"next\"",
            )],
            rest_items(1, 0),
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn relative_rest_next_link_is_rejected_before_following() {
        let transport = FixtureTransport::new(vec![Ok(response(
            200,
            &[("link", "</items?page=2&per_page=100>; rel=\"next\"")],
            rest_items(1, 0),
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn effective_response_path_and_query_must_match_request() {
        for effective_endpoint in [
            "https://api.github.com/other?page=1&per_page=100",
            "https://api.github.com/items?page=2&per_page=100",
        ] {
            let transport = FixtureTransport::new(vec![Ok(raw_response(
                200,
                &[],
                serde_json::to_vec(&rest_items(1, 0)).unwrap(),
                effective_endpoint,
            ))]);
            let mut store = MemoryStore::default();
            let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
            let error = collect_rest(&transport, &mut store, &auth(), request)
                .await
                .unwrap_err();
            assert_eq!(error, AcquisitionError::EndpointViolation);
            assert!(store.refs.is_empty());
        }
    }

    #[tokio::test]
    async fn duplicate_rest_link_with_query_reordering_is_rejected() {
        let transport = FixtureTransport::new(vec![
            Ok(response(
                200,
                &[(
                    "link",
                    "<https://api.github.com/items?page=2&per_page=100&a=1&b=2>; rel=\"next\"",
                )],
                rest_items(1, 0),
            )),
            Ok(response(
                200,
                &[(
                    "link",
                    "<https://api.github.com/items?b=2&a=1&page=2&per_page=100>; rel=\"next\"",
                )],
                rest_items(1, 1),
            )),
        ]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items")
            .with_query("a", "1")
            .with_query("b", "2");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn evil_initial_graphql_endpoint_is_rejected_before_transport() {
        let transport = FixtureTransport::new(Vec::new());
        let mut store = MemoryStore::default();
        let mut request = graphql_request();
        request.endpoint = "https://evil.example/graphql".to_owned();
        let error = collect_graphql(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn credential_query_and_handle_are_rejected_before_transport() {
        let transport = FixtureTransport::new(Vec::new());
        let mut store = MemoryStore::default();
        let query_request = RestCollectionRequest::new("items", "/items", Some("items"), "items")
            .with_query("token", "ghp_secret");
        let query_error = collect_rest(&transport, &mut store, &auth(), query_request)
            .await
            .unwrap_err();
        assert_eq!(query_error, AcquisitionError::EndpointViolation);

        let mut store = MemoryStore::default();
        let secret_auth = AuthIdentity::new(
            "ghp_secret",
            "github",
            Some("viewer-1".to_owned()),
            Some("fixture".to_owned()),
            BTreeSet::new(),
        );
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let handle_error = collect_rest(&transport, &mut store, &secret_auth, request)
            .await
            .unwrap_err();
        assert_eq!(handle_error, AcquisitionError::SecretMetadata);
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn effective_redirect_origin_is_rejected() {
        let transport = FixtureTransport::new(vec![Ok(raw_response(
            200,
            &[],
            serde_json::to_vec(&rest_items(1, 0)).unwrap(),
            "https://evil.example/items",
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert!(store.refs.is_empty());
    }

    #[tokio::test]
    async fn credential_bytes_are_masked_before_raw_storage() {
        let body = serde_json::json!({
            "authorization": "ghp_secret",
            "token": "registered-secret-value",
            "items": [1]
        });
        let transport = FixtureTransport::new(vec![Ok(response(200, &[], body))]);
        let mut store = MemoryStore::default();
        let mut credentials = auth();
        credentials
            .register_credential("registered-secret-value")
            .unwrap();
        let request = RestCollectionRequest::new("items", "/items", None::<String>, "items");
        let result = collect_rest(&transport, &mut store, &credentials, request)
            .await
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.state, AcquisitionState::Malformed);
        assert_eq!(store.bytes.len(), 1);
        let stored = String::from_utf8(store.bytes[0].clone()).unwrap();
        assert!(!stored.contains("ghp_secret"));
        assert!(!stored.contains("registered-secret-value"));
        assert_eq!(
            result.raw_objects[0].canonicalization,
            "redacted-raw-bytes-v1"
        );
    }

    #[tokio::test]
    async fn unbound_storage_reference_is_rejected() {
        let transport = FixtureTransport::new(vec![Ok(response(200, &[], rest_items(1, 0)))]);
        let mut store = UnboundStore;
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::StorageUnbound);
    }

    #[tokio::test]
    async fn graphql_page_info_pagination_retains_cursor_provenance() {
        let transport = FixtureTransport::new(vec![
            Ok(response(
                200,
                &[],
                graphql_page(100, 0, true, Some("cursor-1")),
            )),
            Ok(response(200, &[], graphql_page(1, 100, false, None))),
        ]);
        let mut store = MemoryStore::default();
        let result = collect_graphql(&transport, &mut store, &auth(), graphql_request())
            .await
            .unwrap();
        assert!(result.complete);
        assert_eq!(result.items.len(), 101);
        assert_eq!(result.requests.len(), 2);
        assert_eq!(
            result.requests[0].page.cursor_out.as_deref(),
            Some("cursor-1")
        );
        assert_eq!(
            result.requests[1].page.cursor_in.as_deref(),
            Some("cursor-1")
        );
        assert_ne!(
            result.requests[0].variables_sha256,
            result.requests[1].variables_sha256
        );
    }

    #[tokio::test]
    async fn graphql_duplicate_cursor_fails_closed() {
        let transport = FixtureTransport::new(vec![
            Ok(response(200, &[], graphql_page(1, 0, true, Some("same")))),
            Ok(response(200, &[], graphql_page(1, 1, true, Some("same")))),
        ]);
        let mut store = MemoryStore::default();
        let result = collect_graphql(&transport, &mut store, &auth(), graphql_request())
            .await
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.state, AcquisitionState::DuplicateCursor);
        assert_eq!(result.requests.len(), 2);
    }

    #[tokio::test]
    async fn graphql_permission_and_rate_errors_remain_distinct() {
        let permission_transport = FixtureTransport::new(vec![Ok(response(
            403,
            &[("x-ratelimit-remaining", "5")],
            serde_json::json!({"message":"forbidden"}),
        ))]);
        let mut permission_store = MemoryStore::default();
        let permission = collect_graphql(
            &permission_transport,
            &mut permission_store,
            &auth(),
            graphql_request(),
        )
        .await
        .unwrap();
        assert_eq!(permission.state, AcquisitionState::Forbidden);

        let rate_transport = FixtureTransport::new(vec![Ok(response(
            429,
            &[("retry-after", "60")],
            serde_json::json!({"message":"rate limit"}),
        ))]);
        let mut rate_store = MemoryStore::default();
        let rate = collect_graphql(&rate_transport, &mut rate_store, &auth(), graphql_request())
            .await
            .unwrap();
        assert_eq!(rate.state, AcquisitionState::RateLimited);
    }

    #[test]
    fn changing_head_reconciliation_retains_both_identity_sets() {
        let opening = vec![RevisionIdentity {
            repository: "tailrocks/velnor".to_owned(),
            subject: "pr:954".to_owned(),
            head_sha: Some("a".repeat(40)),
            base_sha: Some("b".repeat(40)),
            tested_merge_sha: None,
            merge_group_sha: None,
        }];
        let mut closing_identity = opening[0].clone();
        closing_identity.head_sha = Some("c".repeat(40));
        let reconciliation = reconcile_identity_sets(opening.clone(), vec![closing_identity]);
        assert!(!reconciliation.stable);
        assert_eq!(reconciliation.opening, opening);
        assert_eq!(reconciliation.changes[0].kind, IdentityChangeKind::Rebound);
    }

    #[tokio::test]
    async fn raw_digest_tamper_is_rejected() {
        let transport = FixtureTransport::new(vec![Ok(response(200, &[], rest_items(1, 0)))]);
        let mut store = TamperStore;
        let request = RestCollectionRequest::new("items", "/items", Some("items"), "items");
        let error = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::RawDigestMismatch);
    }

    #[tokio::test]
    async fn binary_response_retains_archive_bytes_and_typed_kind() {
        let body = b"zip archive bytes".to_vec();
        let transport = FixtureTransport::new(vec![Ok(raw_response(
            200,
            &[("content-type", "application/zip")],
            body.clone(),
            "https://api.github.com/repos/acme/project/actions/artifacts/7/zip",
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new(
            "artifact-7",
            "/repos/acme/project/actions/artifacts/7/zip",
            None::<String>,
            "workflow_artifacts",
        )
        .with_per_page(1);
        let result = collect_binary(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(result.complete);
        assert_eq!(result.raw_objects.len(), 1);
        assert_eq!(result.raw_objects[0].object_kind, "workflow_artifacts");
        assert_eq!(result.raw_objects[0].sha256, sha256_digest(&body));
        assert_eq!(
            result.requests[0].response_raw_ref.as_deref(),
            Some("artifact-7-0001-response")
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn binary_transport_failure_is_typed_incomplete_not_empty_success() {
        let transport = FixtureTransport::new(vec![Err(TransportFailure::PermissionDenied)]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new(
            "artifact-7",
            "/repos/acme/project/actions/artifacts/7/zip",
            None::<String>,
            "workflow_artifacts",
        );
        let result = collect_binary(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.state, AcquisitionState::Forbidden);
        assert!(result.items.is_empty());
        assert!(result.raw_objects.is_empty());
        assert_eq!(result.requests[0].state, AcquisitionState::Forbidden);
    }

    #[tokio::test]
    async fn binary_effective_redirect_origin_is_rejected_before_storage() {
        let transport = FixtureTransport::new(vec![Ok(raw_response(
            200,
            &[],
            b"zip".to_vec(),
            "https://evil.example/repos/acme/project/actions/artifacts/7/zip",
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new(
            "artifact-7",
            "/repos/acme/project/actions/artifacts/7/zip",
            None::<String>,
            "workflow_artifacts",
        );
        let error = collect_binary(&transport, &mut store, &auth(), request)
            .await
            .unwrap_err();
        assert_eq!(error, AcquisitionError::EndpointViolation);
        assert!(store.refs.is_empty());
    }

    #[tokio::test]
    async fn check_runs_request_forces_all_attempts() {
        let transport = FixtureTransport::new(vec![Ok(response(
            200,
            &[],
            serde_json::json!({"check_runs": []}),
        ))]);
        let mut store = MemoryStore::default();
        let request = github_check_runs_request("checks", "tailrocks/velnor", &"a".repeat(40));
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(result.complete);
        let sent = transport.requests();
        assert_eq!(sent[0].query.get("filter").map(String::as_str), Some("all"));
    }

    #[test]
    fn request_debug_redacts_graphql_body_and_sensitive_query() {
        let request = AcquisitionRequest {
            api: ApiKind::GraphQl,
            method: HttpMethod::Post,
            api_origin: GithubApiOrigin::github(),
            endpoint_or_operation: "query".to_owned(),
            query: [("token".to_owned(), "ghp_secret".to_owned())]
                .into_iter()
                .collect(),
            accept: None,
            body: Some(b"github_pat_secret".to_vec()),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("github_pat_secret"));
        assert!(debug.contains("REDACTED"));
    }

    #[tokio::test]
    async fn unknown_github_token_marker_is_redacted_and_digest_pair_is_retained() {
        let body = br#"[{"token":"ghs_unregistered_secret"}]"#.to_vec();
        let transport = FixtureTransport::new(vec![Ok(raw_response(
            200,
            &[],
            body.clone(),
            "https://api.github.com/items?page=1&per_page=100",
        ))]);
        let mut store = MemoryStore::default();
        let request = RestCollectionRequest::new("items", "/items", None::<String>, "items");
        let result = collect_rest(&transport, &mut store, &auth(), request)
            .await
            .unwrap();
        assert!(result.complete);
        assert!(!store.bytes[0].windows(4).any(|window| window == b"ghs_"));
        assert_eq!(result.raw_objects[0].original_sha256, sha256_digest(&body));
        assert_eq!(
            result.raw_objects[0].original_byte_length,
            body.len() as u64
        );
        assert_ne!(
            result.raw_objects[0].sha256,
            result.raw_objects[0].original_sha256
        );
    }

    #[test]
    fn response_debug_redacts_credential_bearing_effective_url() {
        let response = raw_response(
            200,
            &[],
            b"{}".to_vec(),
            "https://api.github.com/items?token=ghs_hidden",
        );
        let debug = format!("{response:?}");
        assert!(!debug.contains("ghs_hidden"));
        assert!(debug.contains("REDACTED_ENDPOINT"));
    }
}

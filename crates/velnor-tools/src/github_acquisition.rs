//! Fail-closed, provenance-preserving GitHub API acquisition primitives.
//!
//! This module is intentionally independent from `evidence_live`.  It owns no
//! credentials, writes no files, and does not decide whether a workflow or
//! check is successful.  It records the API facts needed by a later collector:
//! every request, every page, the safe auth identity, and a verified reference
//! to the exact response bytes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub type AcquisitionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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

/// Safe identity for the credential used by a request.
///
/// Token values and auth headers are deliberately not represented.  The
/// `secret_excluded` invariant is checked before a collection starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthIdentity {
    pub reference: String,
    pub provider: String,
    pub viewer_id: Option<String>,
    pub viewer_login: Option<String>,
    pub safe_scopes: BTreeSet<String>,
    pub secret_excluded: bool,
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
            reference: reference.into(),
            provider: provider.into(),
            viewer_id,
            viewer_login,
            safe_scopes,
            secret_excluded: true,
        }
    }

    fn validate(&self) -> Result<(), AcquisitionError> {
        if self.reference.trim().is_empty() || self.provider.trim().is_empty() {
            return Err(AcquisitionError::InvalidRequest);
        }
        if !self.secret_excluded
            || [
                Some(self.reference.as_str()),
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
    pub endpoint_or_operation: String,
    pub query: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

impl fmt::Debug for AcquisitionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquisitionRequest")
            .field("api", &self.api)
            .field("method", &self.method)
            .field("endpoint_or_operation", &self.endpoint_or_operation)
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
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("headers", &redacted_headers(&self.headers))
            .field("body", &format_args!("<{} bytes>", self.body.len()))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObject {
    pub raw_id: String,
    pub request_id: String,
    pub object_kind: String,
    pub canonicalization: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
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
    pub media_type: String,
    pub storage_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawStorageError {
    Unavailable,
    Refused,
}

/// Caller-owned raw-byte store.  A production implementation may place bytes
/// in an evidence object store; tests use an in-memory fixture store.
pub trait RawObjectStore {
    fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError>;
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
    pub query_sha256: Option<String>,
    pub variables_sha256: Option<String>,
    pub redacted_variables: Option<Value>,
    pub auth_identity_ref: String,
    pub started_at_utc: String,
    pub completed_at_utc: String,
    pub http_status: Option<u16>,
    pub api_request_id: Option<String>,
    pub rate_limit: Option<RateLimitObservation>,
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
    SecretMetadata,
    Serialization,
    StorageUnavailable,
    StorageRefused,
    RawReferenceMismatch,
    RawDigestMismatch,
}

impl fmt::Display for AcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRequest => "invalid acquisition request",
            Self::SecretMetadata => "secret-bearing metadata is not allowed",
            Self::Serialization => "request serialization failed",
            Self::StorageUnavailable => "raw object storage unavailable",
            Self::StorageRefused => "raw object storage refused object",
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
    pub endpoint: String,
    pub query: BTreeMap<String, String>,
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
            endpoint: endpoint.into(),
            query: BTreeMap::new(),
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
    let mut endpoint = request.endpoint.clone();
    let mut query = request.query.clone();
    let mut seen_pages = BTreeSet::new();

    for page_number in 1..=request.max_pages {
        let page_number_u32 = page_number as u32;
        let page_key = format!("{}?{}", endpoint, canonical_query(&query));
        if !seen_pages.insert(page_key) {
            result.state = AcquisitionState::DuplicateCursor;
            result.complete = false;
            break;
        }

        let mut page_query = query.clone();
        if page_number == 1 {
            page_query
                .entry("per_page".to_owned())
                .or_insert_with(|| request.per_page.to_string());
        }
        let acquisition_request = AcquisitionRequest {
            api: ApiKind::Rest,
            method: HttpMethod::Get,
            endpoint_or_operation: endpoint.clone(),
            query: page_query.clone(),
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

        let rate_limit = rate_limit_from_headers(&response.headers);
        let media_type = header_value(&response.headers, "content-type")
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let raw_id = format!("{request_id}-response");
        let raw = retain_raw(
            store,
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
            let page = PageState {
                number: page_number_u32,
                per_page: Some(request.per_page as u32),
                link_next: Some(next_link.clone()),
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
            if seen_pages.contains(&format!("{next_link}?"))
                || seen_pages
                    .iter()
                    .any(|entry| entry.starts_with(&format!("{next_link}?")))
            {
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
            endpoint = next_link;
            query = BTreeMap::new();
            continue;
        }

        if page_items_len == request.per_page {
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
        let acquisition_request = AcquisitionRequest {
            api: ApiKind::GraphQl,
            method: HttpMethod::Post,
            endpoint_or_operation: request.endpoint.clone(),
            query: BTreeMap::new(),
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
        let rate_limit = rate_limit_from_headers(&response.headers);
        let media_type = header_value(&response.headers, "content-type")
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let raw_id = format!("{request_id}-response");
        let raw = retain_raw(
            store,
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
    {
        return Err(AcquisitionError::InvalidRequest);
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
    let array = item_field.map_or(Some(body), |field| body.get(field));
    array
        .and_then(Value::as_array)
        .cloned()
        .ok_or(AcquisitionError::InvalidRequest)
}

fn value_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

fn retain_raw<S: RawObjectStore>(
    store: &mut S,
    object: RawObject,
) -> Result<RawObjectRef, AcquisitionError> {
    let expected_sha256 = sha256_digest(&object.bytes);
    let expected_length = object.bytes.len() as u64;
    let expected_id = object.raw_id.clone();
    let expected_request = object.request_id.clone();
    let expected_kind = object.object_kind.clone();
    let expected_canonicalization = object.canonicalization.clone();
    let expected_media_type = object.media_type.clone();
    let reference = store.store(object).map_err(|error| match error {
        RawStorageError::Unavailable => AcquisitionError::StorageUnavailable,
        RawStorageError::Refused => AcquisitionError::StorageRefused,
    })?;
    if reference.raw_id != expected_id
        || reference.request_id != expected_request
        || reference.object_kind != expected_kind
        || reference.canonicalization != expected_canonicalization
        || reference.media_type != expected_media_type
        || reference.byte_length != expected_length
    {
        return Err(AcquisitionError::RawReferenceMismatch);
    }
    if reference.sha256 != expected_sha256 {
        return Err(AcquisitionError::RawDigestMismatch);
    }
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
    let (query_sha256, variables_sha256, redacted_variables) = match graphql_inputs {
        Some((variables, query)) => (
            Some(sha256_digest(query.as_bytes())),
            Some(canonical_json_bytes(variables).map_or_else(
                |_| "sha256:serialization-error".to_owned(),
                |bytes| sha256_digest(&bytes),
            )),
            Some(redact_value(variables)),
        ),
        None => (
            Some(sha256_digest(canonical_query(&request.query).as_bytes())),
            None,
            None,
        ),
    };
    RequestRecord {
        request_id: request_id.to_owned(),
        api: request.api,
        method: request.method,
        endpoint_or_operation: request.endpoint_or_operation.clone(),
        query_sha256,
        variables_sha256,
        redacted_variables,
        auth_identity_ref: auth.reference.clone(),
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
    }
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
    value.contains("ghp_")
        || value.contains("github_pat_")
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
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportFailure::Other));
            Box::pin(async move { response })
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        refs: Vec<RawObjectRef>,
    }

    impl RawObjectStore for MemoryStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            let reference = RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: sha256_digest(&object.bytes),
                byte_length: object.bytes.len() as u64,
                media_type: object.media_type,
                storage_ref: format!("memory://{}", self.refs.len() + 1),
            };
            self.refs.push(reference.clone());
            Ok(reference)
        }
    }

    struct TamperStore;

    impl RawObjectStore for TamperStore {
        fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
            Ok(RawObjectRef {
                raw_id: object.raw_id,
                request_id: object.request_id,
                object_kind: object.object_kind,
                canonicalization: object.canonicalization,
                sha256: "sha256:tampered".to_owned(),
                byte_length: object.bytes.len() as u64,
                media_type: object.media_type,
                storage_ref: "memory://tampered".to_owned(),
            })
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
        TransportResponse {
            status,
            headers: headers
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            body: serde_json::to_vec(&body).unwrap(),
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
                    ("link", "<https://api.test/items?page=2>; rel=\"next\""),
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
            Some("https://api.test/items?page=2")
        );
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn rest_page_two_permission_failure_is_not_empty_success() {
        let transport = FixtureTransport::new(vec![
            Ok(response(
                200,
                &[("link", "</items?page=2>; rel=\"next\"")],
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
            endpoint_or_operation: "query".to_owned(),
            query: [("token".to_owned(), "ghp_secret".to_owned())]
                .into_iter()
                .collect(),
            body: Some(b"github_pat_secret".to_vec()),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("github_pat_secret"));
        assert!(debug.contains("REDACTED"));
    }
}

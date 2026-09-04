//! Versioned Unix-socket client for the local control API.
//!
//! This module deliberately implements only the transport boundary. It knows
//! socket selection, HTTP framing, API negotiation, deadlines, and safe error
//! decoding; it does not depend on daemon application ports or Axum.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use velnor_model::{
    AnyResource, Event, ExitClass, MachineErrorEnvelope, TelemetryEnvelope, SCHEMA_VERSION,
};

use crate::unix::{EndpointError, SocketKind, UnixEndpoint, API_VERSION};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Client-side query filters accepted by the versioned API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceQuery {
    /// Include-only selector.
    pub selector: Option<String>,
    /// Equality selector.
    pub field_selector: Option<String>,
    /// Opaque continuation token.
    pub page_token: Option<String>,
    /// RFC 3339 lower bound.
    pub since: Option<String>,
    /// Maximum returned resources.
    pub limit: Option<u32>,
}

/// One decoded resource page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePage {
    /// Sanitized resources returned by the daemon.
    pub resources: Vec<AnyResource>,
    /// Opaque continuation token, when more data exists.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// One decoded observation item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchItem {
    /// Monotonic stream version.
    pub version: u64,
    /// Sanitized event.
    pub event: Event,
}

/// One decoded sanitized log item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogItem {
    /// Canonical job/run identity owning the record.
    pub subject: String,
    /// Cursor position after this record.
    pub cursor: String,
    /// Selected source.
    pub source: String,
    /// Monotonic record sequence.
    pub sequence: u64,
    /// Redacted message.
    pub message: String,
}

/// One decoded shared telemetry record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryItem {
    /// Opaque cursor returned by the daemon.
    pub cursor: String,
    /// Secret-safe performance observation.
    pub envelope: TelemetryEnvelope,
}

/// One decoded shared telemetry page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryPage {
    /// Records in emission order.
    pub records: Vec<TelemetryItem>,
    /// Cursor for the next page, when more records remain.
    pub next_cursor: Option<String>,
    /// Oldest cursor when the requested cursor crossed a generation.
    pub dropped_before: Option<String>,
}

/// Server API identity returned by `/v1/info`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// Negotiated path version.
    pub api_version: String,
    /// Shared DTO schema version.
    pub schema_version: u32,
    /// Whether this endpoint accepts mutations.
    pub mutations: bool,
}

/// Accepted mutation response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationResponse {
    /// Durable operation id.
    pub operation_id: String,
    /// Current operation phase.
    pub phase: String,
}

/// Typed local control client.
#[derive(Debug, Clone)]
pub struct UnixControlClient {
    endpoint: UnixEndpoint,
    timeout: Duration,
}

impl UnixControlClient {
    /// Create a client with a bounded ten-second request deadline.
    #[must_use]
    pub fn new(endpoint: UnixEndpoint) -> Self {
        Self {
            endpoint,
            timeout: Duration::from_secs(10),
        }
    }

    /// Override the per-request transport deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Endpoint selected by this client.
    #[must_use]
    pub fn endpoint(&self) -> &UnixEndpoint {
        &self.endpoint
    }

    /// Negotiate the API and schema version on the read socket.
    pub async fn info(&self) -> Result<Info, ClientError> {
        let info: Info = self
            .request_json(SocketKind::Control, "GET", "/v1/info", None)
            .await?;
        validate_info(&info, false)?;
        Ok(info)
    }

    /// Query one resource collection through the read socket.
    pub async fn get_resources(
        &self,
        resource: &str,
        query: &ResourceQuery,
    ) -> Result<ResourcePage, ClientError> {
        validate_segment(resource, "resource")?;
        self.info().await?;
        let path = format!("/v1/{resource}{}", query_string(query));
        self.request_json(SocketKind::Control, "GET", &path, None)
            .await
    }

    /// Read a bounded event segment through the read socket.
    pub async fn watch(
        &self,
        resource: Option<&str>,
        after_version: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<WatchItem>, ClientError> {
        if let Some(resource) = resource {
            validate_segment(resource, "resource")?;
        }
        self.info().await?;
        let mut query = Vec::new();
        if let Some(resource) = resource {
            query.push(("resourceKind", resource.to_owned()));
        }
        if let Some(version) = after_version {
            query.push(("afterVersion", version.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let path = format!("/v1/watch{}", encoded_query(&query));
        self.request_json(SocketKind::Control, "GET", &path, None)
            .await
    }

    /// Read one bounded log segment through the read socket.
    pub async fn logs(
        &self,
        subject: &str,
        source: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<LogItem>, ClientError> {
        validate_segment(subject, "subject")?;
        self.info().await?;
        let mut query = Vec::new();
        if let Some(source) = source {
            validate_segment(source, "source")?;
            query.push(("source", source.to_owned()));
        }
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let path = format!("/v1/logs/{subject}{}", encoded_query(&query));
        self.request_json(SocketKind::Control, "GET", &path, None)
            .await
    }

    /// Read one bounded shared telemetry page through the read socket.
    pub async fn telemetry(
        &self,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<TelemetryPage, ClientError> {
        self.info().await?;
        let mut query = Vec::new();
        if let Some(after) = after {
            validate_cursor(after)?;
            query.push(("after", after.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let path = format!("/v1/telemetry{}", encoded_query(&query));
        self.request_json(SocketKind::Control, "GET", &path, None)
            .await
    }

    /// Submit one lifecycle mutation through the admin socket only.
    pub async fn mutate_instance(
        &self,
        instance: &str,
        operation: &str,
        reason: &str,
        idempotency_key: &str,
        expected_version: Option<u64>,
        scale_to: Option<u32>,
    ) -> Result<MutationResponse, ClientError> {
        crate::unix::UnixEndpoint::from_instance(instance).map_err(ClientError::Endpoint)?;
        validate_segment(operation, "operation")?;
        if reason.trim().is_empty() {
            return Err(ClientError::Invalid {
                field: "reason".to_owned(),
                message: "reason must not be empty".to_owned(),
            });
        }
        if idempotency_key.trim().is_empty() {
            return Err(ClientError::Invalid {
                field: "idempotencyKey".to_owned(),
                message: "idempotency key must not be empty".to_owned(),
            });
        }
        let info: Info = self
            .request_json(SocketKind::Admin, "GET", "/v1/info", None)
            .await?;
        validate_info(&info, true)?;
        let body = MutationBody {
            operation,
            reason,
            idempotency_key,
            expected_version,
            slots: scale_to,
        };
        let path = format!("/v1/instances/{instance}/{operation}");
        self.request_json(
            SocketKind::Admin,
            "POST",
            &path,
            Some(
                serde_json::to_vec(&body).map_err(|error| ClientError::Protocol {
                    message: error.to_string(),
                })?,
            ),
        )
        .await
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        socket: SocketKind,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, ClientError> {
        let response = self.request(socket, method, path, body).await?;
        serde_json::from_slice(&response.body).map_err(|error| ClientError::Protocol {
            message: format!("invalid JSON response: {error}"),
        })
    }

    async fn request(
        &self,
        socket: SocketKind,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Response, ClientError> {
        if !path.starts_with("/v1/") {
            return Err(ClientError::Invalid {
                field: "path".to_owned(),
                message: "path must use the v1 API".to_owned(),
            });
        }
        let body = body.unwrap_or_default();
        let request_id = next_request_id();
        let socket_path = self.endpoint.socket_path(socket);
        tokio::time::timeout(
            self.timeout,
            exchange(&socket_path, method, path, &body, &request_id),
        )
        .await
        .map_err(|_| ClientError::Timeout)?
        .and_then(|response| {
            if (200..300).contains(&response.status) {
                Ok(response)
            } else {
                Err(response.into_error())
            }
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationBody<'a> {
    operation: &'a str,
    reason: &'a str,
    idempotency_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slots: Option<u32>,
}

#[derive(Debug)]
struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    fn into_error(self) -> ClientError {
        match serde_json::from_slice::<MachineErrorEnvelope>(&self.body) {
            Ok(envelope) => ClientError::Api {
                status: self.status,
                envelope,
            },
            Err(_) => ClientError::Protocol {
                message: format!(
                    "server returned HTTP {} without a safe error envelope",
                    self.status
                ),
            },
        }
    }
}

async fn exchange(
    socket_path: &std::path::Path,
    method: &str,
    path: &str,
    body: &[u8],
    request_id: &str,
) -> Result<Response, ClientError> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|error| ClientError::Io {
            message: "control socket is unavailable".to_owned(),
            detail: error.to_string(),
        })?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nConnection: close\r\nX-Request-Id: {request_id}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| ClientError::Io {
            message: "control request could not be sent".to_owned(),
            detail: error.to_string(),
        })?;
    stream
        .write_all(body)
        .await
        .map_err(|error| ClientError::Io {
            message: "control request could not be sent".to_owned(),
            detail: error.to_string(),
        })?;
    stream.shutdown().await.map_err(|error| ClientError::Io {
        message: "control request could not be finalized".to_owned(),
        detail: error.to_string(),
    })?;
    read_response(&mut stream).await
}

#[cfg(test)]
fn parse_response(bytes: &[u8]) -> Result<Response, ClientError> {
    let marker = b"\r\n\r\n";
    let header_end = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| ClientError::Protocol {
            message: "control response omitted HTTP headers".to_owned(),
        })?;
    let (status, declared_length) = parse_response_headers(&bytes[..header_end])?;
    let body = &bytes[header_end + marker.len()..];
    let body_len = declared_length.unwrap_or(body.len());
    if body_len > MAX_RESPONSE_BYTES || body.len() < body_len {
        return Err(ClientError::Protocol {
            message: "control response exceeded the bounded body size".to_owned(),
        });
    }
    Ok(Response {
        status,
        body: body[..body_len].to_vec(),
    })
}

async fn read_response(stream: &mut UnixStream) -> Result<Response, ClientError> {
    let marker = b"\r\n\r\n";
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = bytes
            .windows(marker.len())
            .position(|window| window == marker)
        {
            break position;
        }
        if bytes.len() > MAX_RESPONSE_HEADER_BYTES {
            return Err(ClientError::Protocol {
                message: "control response headers exceeded the bounded size".to_owned(),
            });
        }
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| ClientError::Io {
                message: "control response could not be read".to_owned(),
                detail: error.to_string(),
            })?;
        if read == 0 {
            return Err(ClientError::Protocol {
                message: "control response omitted HTTP headers".to_owned(),
            });
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let (status, declared_length) = parse_response_headers(&bytes[..header_end])?;
    let body_start = header_end + marker.len();
    let mut body = bytes.split_off(body_start);
    if let Some(body_len) = declared_length {
        if body_len > MAX_RESPONSE_BYTES {
            return Err(ClientError::Protocol {
                message: "control response exceeded the bounded body size".to_owned(),
            });
        }
        if body.len() > body_len {
            return Err(ClientError::Protocol {
                message: "control response contained bytes beyond Content-Length".to_owned(),
            });
        }
        body.reserve(body_len.saturating_sub(body.len()));
        let missing = body_len.saturating_sub(body.len());
        if missing > 0 {
            let original_len = body.len();
            body.resize(body_len, 0);
            stream
                .read_exact(&mut body[original_len..])
                .await
                .map_err(|error| ClientError::Io {
                    message: "control response could not be read".to_owned(),
                    detail: error.to_string(),
                })?;
        }
    } else {
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(ClientError::Protocol {
                message: "control response exceeded the bounded body size".to_owned(),
            });
        }
        let mut chunk = [0_u8; 8192];
        loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| ClientError::Io {
                    message: "control response could not be read".to_owned(),
                    detail: error.to_string(),
                })?;
            if read == 0 {
                break;
            }
            if body.len().saturating_add(read) > MAX_RESPONSE_BYTES {
                return Err(ClientError::Protocol {
                    message: "control response exceeded the bounded body size".to_owned(),
                });
            }
            body.extend_from_slice(&chunk[..read]);
        }
    }
    Ok(Response { status, body })
}

fn parse_response_headers(bytes: &[u8]) -> Result<(u16, Option<usize>), ClientError> {
    let header = std::str::from_utf8(bytes).map_err(|_| ClientError::Protocol {
        message: "control response headers were not UTF-8".to_owned(),
    })?;
    let mut lines = header.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| ClientError::Protocol {
            message: "control response had no valid HTTP status".to_owned(),
        })?;
    let mut declared_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| ClientError::Protocol {
                    message: "control response had an invalid Content-Length".to_owned(),
                })?;
            if declared_length.replace(length).is_some() {
                return Err(ClientError::Protocol {
                    message: "control response repeated Content-Length".to_owned(),
                });
            }
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            && !value.trim().eq_ignore_ascii_case("identity")
        {
            return Err(ClientError::Protocol {
                message: "chunked control responses are unsupported".to_owned(),
            });
        }
    }
    Ok((status, declared_length))
}

fn validate_info(info: &Info, mutations: bool) -> Result<(), ClientError> {
    if info.api_version != API_VERSION || info.schema_version != SCHEMA_VERSION {
        return Err(ClientError::UnsupportedApi {
            api_version: info.api_version.clone(),
            schema_version: info.schema_version,
        });
    }
    if mutations && !info.mutations {
        return Err(ClientError::Unsupported {
            operation: "lifecycle mutations".to_owned(),
        });
    }
    Ok(())
}

fn validate_segment(value: &str, field: &str) -> Result<(), ClientError> {
    if value.is_empty()
        || value.len() > 128
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ClientError::Invalid {
            field: field.to_owned(),
            message: "value contains an unsupported path character".to_owned(),
        });
    }
    Ok(())
}

fn validate_cursor(value: &str) -> Result<(), ClientError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(ClientError::Invalid {
            field: "after".to_owned(),
            message: "telemetry cursor is malformed".to_owned(),
        });
    }
    Ok(())
}

fn query_string(query: &ResourceQuery) -> String {
    let mut values = Vec::new();
    if let Some(value) = &query.selector {
        values.push(("selector", value.clone()));
    }
    if let Some(value) = &query.field_selector {
        values.push(("fieldSelector", value.clone()));
    }
    if let Some(value) = &query.page_token {
        values.push(("pageToken", value.clone()));
    }
    if let Some(value) = &query.since {
        values.push(("since", value.clone()));
    }
    if let Some(value) = query.limit {
        values.push(("limit", value.to_string()));
    }
    encoded_query(&values)
}

fn encoded_query(values: &[(&str, String)]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let mut output = String::from("?");
    for (index, (name, value)) in values.iter().enumerate() {
        if index > 0 {
            output.push('&');
        }
        output.push_str(name);
        output.push('=');
        output.push_str(&encode_component(value));
    }
    output
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(hex(byte >> 4));
            encoded.push(hex(byte & 0x0f));
        }
    }
    encoded
}

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

fn next_request_id() -> String {
    format!("local-{}", NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
}

/// Transport failure returned by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// Local endpoint syntax is invalid.
    Endpoint(EndpointError),
    /// Request input is invalid.
    Invalid { field: String, message: String },
    /// The daemon returned a safe versioned error envelope.
    Api {
        status: u16,
        envelope: MachineErrorEnvelope,
    },
    /// The daemon and client cannot negotiate one compatible version.
    UnsupportedApi {
        api_version: String,
        schema_version: u32,
    },
    /// The daemon knows the API but does not implement the requested operation.
    Unsupported { operation: String },
    /// Peer authorization rejected the operation.
    Authorization,
    /// Request deadline elapsed.
    Timeout,
    /// Socket I/O failed without exposing filesystem details to callers.
    Io { message: String, detail: String },
    /// Response framing or decoding failed.
    Protocol { message: String },
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => error.fmt(formatter),
            Self::Invalid { field, message } => write!(formatter, "invalid {field}: {message}"),
            Self::Api { envelope, .. } => {
                write!(formatter, "control API error: {}", envelope.reason)
            }
            Self::UnsupportedApi {
                api_version,
                schema_version,
            } => write!(
                formatter,
                "unsupported control API {api_version} schema {schema_version}"
            ),
            Self::Unsupported { operation } => {
                write!(formatter, "unsupported control API operation: {operation}")
            }
            Self::Authorization => formatter.write_str("control API authorization denied"),
            Self::Timeout => formatter.write_str("control API request timed out"),
            Self::Io { message, .. } => formatter.write_str(message),
            Self::Protocol { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<EndpointError> for ClientError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

impl ClientError {
    /// Map transport failure to the public process exit contract.
    #[must_use]
    pub fn exit_class(&self) -> ExitClass {
        match self {
            Self::Endpoint(_) | Self::Invalid { .. } | Self::Protocol { .. } => ExitClass::Usage,
            Self::Api { envelope, .. } => {
                ExitClass::from_code(envelope.code).unwrap_or(ExitClass::Transport)
            }
            Self::UnsupportedApi { .. } | Self::Io { .. } => ExitClass::Transport,
            Self::Unsupported { .. } => ExitClass::Operation,
            Self::Authorization => ExitClass::Authorization,
            Self::Timeout => ExitClass::Timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encoding_is_stable_and_escapes_selectors() {
        let encoded = query_string(&ResourceQuery {
            selector: Some("phase=busy idle".to_owned()),
            limit: Some(2),
            ..ResourceQuery::default()
        });
        assert_eq!(encoded, "?selector=phase%3Dbusy%20idle&limit=2");
    }

    #[test]
    fn response_parser_bounds_body_and_reads_status() {
        let response =
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn mutation_client_never_changes_socket_class() {
        let endpoint = UnixEndpoint::from_instance("primary").expect("endpoint");
        assert_ne!(
            endpoint.socket_path(SocketKind::Control),
            endpoint.socket_path(SocketKind::Admin)
        );
    }

    #[test]
    fn disabled_mutation_capability_is_not_reported_as_authorization() {
        let error = validate_info(
            &Info {
                api_version: API_VERSION.to_owned(),
                schema_version: SCHEMA_VERSION,
                mutations: false,
            },
            true,
        )
        .expect_err("disabled mutation capability must fail closed");
        assert!(matches!(error, ClientError::Unsupported { .. }));
        assert_eq!(error.exit_class(), ExitClass::Operation);
    }

    #[tokio::test]
    async fn exchange_round_trip_uses_bounded_http_framing() {
        let path = std::env::temp_dir().join(format!(
            "velnor-client-test-{}-{}.sock",
            std::process::id(),
            NEXT_REQUEST_ID.load(Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("bind test socket");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).await.expect("read");
            assert!(request.starts_with(b"GET /v1/info HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .expect("write");
        });
        let response = exchange(&path, "GET", "/v1/info", &[], "test-1")
            .await
            .expect("round trip");
        server.await.expect("server task");
        let _ = std::fs::remove_file(path);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
    }
}

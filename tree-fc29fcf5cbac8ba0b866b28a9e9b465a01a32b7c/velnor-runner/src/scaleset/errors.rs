//! Scale-set error taxonomy (`errors.go` at [`crate::scaleset::UPSTREAM_COMMIT`]).
//!
//! Mirrors `MessageQueueTokenExpiredError`, the runner/job sentinels, the
//! top-level HTTP-code errors, and `newRequestResponseError` (activity ID +
//! GitHub request ID capture, exception-name mapping). Response bodies are
//! parsed for the known `typeName`/`message` shape; unknown bodies surface
//! the raw text for `text/plain` exactly like upstream.

use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use thiserror::Error;

/// Well-known Actions exception names mapped by `newRequestResponseError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleSetFault {
    /// `MessageQueueTokenExpiredError`: refresh the session, retry once.
    MessageQueueTokenExpired,
    /// `RunnerExistsError` (`AgentExistsException`).
    RunnerExists,
    /// `RunnerNotFoundError` (`AgentNotFoundException`).
    RunnerNotFound,
    /// `JobStillRunningError` (`JobStillRunningException`).
    JobStillRunning,
    /// Top-level `BadRequestError` (400).
    BadRequest,
    /// Top-level `NotFoundError` (404).
    NotFound,
    /// Top-level `UnauthorizedError` (401).
    Unauthorized,
    /// Top-level `ConflictError` (409).
    Conflict,
}

impl ScaleSetFault {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MessageQueueTokenExpired => "message queue token expired",
            Self::RunnerExists => "runner exists",
            Self::RunnerNotFound => "runner not found",
            Self::JobStillRunning => "job still running",
            Self::BadRequest => "bad request",
            Self::NotFound => "not found",
            Self::Unauthorized => "unauthorized",
            Self::Conflict => "conflict",
        }
    }

    /// Mirror of `wrapResponseErrorType`.
    #[must_use]
    pub fn for_status(status: StatusCode) -> Option<Self> {
        match status {
            StatusCode::BAD_REQUEST => Some(Self::BadRequest),
            StatusCode::UNAUTHORIZED => Some(Self::Unauthorized),
            StatusCode::NOT_FOUND => Some(Self::NotFound),
            StatusCode::CONFLICT => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// Failed-request detail; boxed inside [`ScaleSetError`] to keep the
/// `Err` variant small.
#[derive(Debug)]
pub struct RequestFailure {
    pub method: String,
    pub url: String,
    pub status: String,
    pub activity: String,
    pub request_id: String,
    pub message: String,
    /// Classified fault when the status/exception mapped to one.
    pub fault: Option<ScaleSetFault>,
}

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "request {} {} failed(status={}{}{}): {}",
            self.method, self.url, self.status, self.activity, self.request_id, self.message
        )
    }
}

/// Scale-set protocol error.
#[derive(Debug, Error)]
pub enum ScaleSetError {
    /// A request failed; mirrors `newRequestResponseError` output.
    #[error("{0}")]
    RequestFailed(Box<RequestFailure>),
    /// Transport failure (mirrors `sendRequest` failure wrapping).
    #[error("failed to send request: {0}")]
    Transport(String),
    /// Local request-construction failure.
    #[error("{0}")]
    Local(String),
}

impl ScaleSetError {
    /// True for the 401 queue-token signal that triggers session refresh.
    #[must_use]
    pub fn is_token_expired(&self) -> bool {
        matches!(
            self,
            Self::RequestFailed(failure)
                if failure.fault == Some(ScaleSetFault::MessageQueueTokenExpired)
        )
    }

    #[must_use]
    pub fn fault(&self) -> Option<ScaleSetFault> {
        match self {
            Self::RequestFailed(failure) => failure.fault,
            Self::Transport(_) | Self::Local(_) => None,
        }
    }
}

/// Mirror of `newRequestResponseError(req, resp, err)`.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors upstream error builder arity"
)]
pub fn request_response_error(
    method: &str,
    url: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    fault: Option<ScaleSetFault>,
    detail: &str,
) -> ScaleSetError {
    let mut activity = String::new();
    if let Some(value) = headers.get("ActivityId").and_then(|v| v.to_str().ok()) {
        activity = format!(", activity_id=\"{value}\"");
    }
    let mut request_id = String::new();
    if let Some(value) = headers
        .get("X-GitHub-Request-Id")
        .and_then(|v| v.to_str().ok())
    {
        request_id = format!(", github_request_id=\"{value}\"");
    }

    let message = error_message(headers, body, fault, detail);
    // Mirror `wrapResponseErrorType`: the stored fault prefers the explicit
    // sentinel and falls back to the status-code mapping.
    let fault = fault.or_else(|| ScaleSetFault::for_status(status));
    ScaleSetError::RequestFailed(Box::new(RequestFailure {
        method: method.to_string(),
        url: url.to_string(),
        status: format!("\"{}\"", status.as_str()),
        activity,
        request_id,
        message,
        fault,
    }))
}

fn error_message(
    headers: &HeaderMap,
    body: &[u8],
    fault: Option<ScaleSetFault>,
    detail: &str,
) -> String {
    if body.is_empty() {
        return format!("{detail}: unknown error");
    }
    // A classified sentinel short-circuits body parsing (upstream
    // `errors.As(err, &scalesetErr)` branch).
    if let Some(fault) = fault {
        return format!("{}: {}: {}", detail, fault.as_str(), lossy(body));
    }
    if let Some(content_type) = headers.get(reqwest::header::CONTENT_TYPE)
        && let Ok(value) = content_type.to_str()
        && value.contains("text/plain")
    {
        return format!("{detail}: {}", lossy(body));
    }
    let exception: ActionsException = match serde_json::from_slice(body) {
        Ok(exception) => exception,
        Err(_) => {
            return format!(
                "{detail}: failed to unmarshal error response body: {:?}",
                lossy(body)
            );
        }
    };
    if exception.type_name.contains("AgentExistsException") {
        return format!(
            "{detail}: {}: {}",
            ScaleSetFault::RunnerExists.as_str(),
            exception.message
        );
    }
    if exception.type_name.contains("AgentNotFoundException") {
        return format!(
            "{detail}: {}: {}",
            ScaleSetFault::RunnerNotFound.as_str(),
            exception.message
        );
    }
    if exception.type_name.contains("JobStillRunningException") {
        return format!(
            "{detail}: {}: {}",
            ScaleSetFault::JobStillRunning.as_str(),
            exception.message
        );
    }
    format!("{detail}: {}: {}", exception.type_name, exception.message)
}

fn lossy(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[derive(Debug, serde::Deserialize)]
struct ActionsException {
    #[serde(rename = "typeName", default)]
    type_name: String,
    #[serde(default)]
    message: String,
}

/// Strip a UTF-8 BOM exactly like `trimByteOrderMark` (`sendRequest` applies
/// this to every response body before decoding).
#[must_use]
pub fn trim_byte_order_mark(body: &[u8]) -> &[u8] {
    body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body)
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

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("ActivityId", "act-1".parse().unwrap());
        headers.insert("X-GitHub-Request-Id", "gh-1".parse().unwrap());
        headers
    }

    #[test]
    fn token_expired_is_detected() {
        let error = request_response_error(
            "GET",
            "https://q.example/queue",
            StatusCode::UNAUTHORIZED,
            &headers(),
            b"{}",
            Some(ScaleSetFault::MessageQueueTokenExpired),
            "unexpected status code",
        );
        assert!(error.is_token_expired());
        let text = error.to_string();
        assert!(text.contains("activity_id=\"act-1\""), "{text}");
        assert!(text.contains("github_request_id=\"gh-1\""), "{text}");
        assert!(text.contains("message queue token expired"), "{text}");
    }

    #[test]
    fn agent_not_found_exception_maps() {
        let error = request_response_error(
            "GET",
            "https://a.example/agents/9",
            StatusCode::NOT_FOUND,
            &HeaderMap::new(),
            br#"{"typeName":"AgentNotFoundException","message":"no such runner"}"#,
            None,
            "unexpected status code",
        );
        assert_eq!(error.fault(), Some(ScaleSetFault::NotFound));
        assert!(error.to_string().contains("runner not found"), "{}", error);
    }

    #[test]
    fn agent_exists_and_job_running_exceptions_map() {
        // Upstream `newRequestResponseError` maps these `typeName`s to the
        // runner/job sentinels while the status still tags the fault.
        let exists = request_response_error(
            "POST",
            "https://a.example/scalesets",
            StatusCode::CONFLICT,
            &HeaderMap::new(),
            br#"{"typeName":"AgentExistsException","message":"already registered"}"#,
            None,
            "unexpected status code",
        );
        assert_eq!(exists.fault(), Some(ScaleSetFault::Conflict));
        assert!(exists.to_string().contains("runner exists"), "{exists}");
        let running = request_response_error(
            "DELETE",
            "https://a.example/agents/11",
            StatusCode::CONFLICT,
            &HeaderMap::new(),
            br#"{"typeName":"JobStillRunningException","message":"job 9 running"}"#,
            None,
            "unexpected status code",
        );
        assert_eq!(running.fault(), Some(ScaleSetFault::Conflict));
        assert!(
            running.to_string().contains("job still running"),
            "{running}"
        );
    }

    #[test]
    fn empty_body_is_unknown_error() {
        let error = request_response_error(
            "DELETE",
            "https://q.example/queue/3",
            StatusCode::INTERNAL_SERVER_ERROR,
            &HeaderMap::new(),
            b"",
            None,
            "unexpected status code",
        );
        assert!(error.to_string().contains("unknown error"), "{error}");
    }

    #[test]
    fn bom_is_stripped() {
        assert_eq!(trim_byte_order_mark(b"\xef\xbb\xbf{}"), b"{}");
        assert_eq!(trim_byte_order_mark(b"{}"), b"{}");
    }
}

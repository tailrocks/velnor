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

/// Parsed Actions exception carried alongside the HTTP-status fault. Go's
/// `%w` wrapping preserves both errors, so callers can inspect them separately.
#[derive(Clone, PartialEq, Eq)]
pub struct ActionsApiException {
    pub type_name: String,
    pub message: String,
}

impl std::fmt::Debug for ActionsApiException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionsApiException")
            .field("type_name", &self.type_name)
            .field("message", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Display for ActionsApiException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.type_name, self.message)
    }
}

/// Failed-request detail; boxed inside [`ScaleSetError`] to keep the
/// `Err` variant small.
pub struct RequestFailure {
    pub method: String,
    pub url: String,
    pub status: String,
    pub activity: String,
    pub request_id: String,
    pub message: String,
    /// Actions API sentinel (or explicit caller-supplied sentinel).
    pub fault: Option<ScaleSetFault>,
    /// HTTP status sentinel from `wrapResponseErrorType`.
    pub status_fault: Option<ScaleSetFault>,
    /// Parsed exception preserved from the response body.
    pub api_exception: Option<ActionsApiException>,
}

impl std::fmt::Debug for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFailure")
            .field("method", &self.method)
            .field("url", &redact_url(&self.url))
            .field("status", &self.status)
            .field("activity", &self.activity)
            .field("request_id", &self.request_id)
            .field("message", &"<redacted>")
            .field("fault", &self.fault)
            .field("status_fault", &self.status_fault)
            .field("api_exception", &self.api_exception)
            .finish()
    }
}

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(status_fault) = self.status_fault {
            write!(f, "{}: ", status_fault.as_str())?;
        }
        write!(
            f,
            "request {} {} failed(status={:?}{}{}): {}",
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
    /// Context wrapper that preserves typed faults through `%w`-style layers.
    #[error("{context}: {source}")]
    Context {
        context: String,
        #[source]
        source: Box<ScaleSetError>,
    },
}

impl ScaleSetError {
    /// True for the 401 queue-token signal that triggers session refresh.
    #[must_use]
    pub fn is_token_expired(&self) -> bool {
        self.fault() == Some(ScaleSetFault::MessageQueueTokenExpired)
    }

    #[must_use]
    pub fn fault(&self) -> Option<ScaleSetFault> {
        match self {
            Self::RequestFailed(failure) => failure.fault.or(failure.status_fault),
            Self::Context { source, .. } => source.fault(),
            Self::Transport(_) | Self::Local(_) => None,
        }
    }

    /// HTTP-only classification, retained if the body also maps to an
    /// Actions exception such as `AgentNotFoundException`.
    #[must_use]
    pub fn status_fault(&self) -> Option<ScaleSetFault> {
        match self {
            Self::RequestFailed(failure) => failure.status_fault,
            Self::Context { source, .. } => source.status_fault(),
            Self::Transport(_) | Self::Local(_) => None,
        }
    }

    /// Parsed Actions exception, preserved through context wrappers.
    #[must_use]
    pub fn api_exception(&self) -> Option<&ActionsApiException> {
        match self {
            Self::RequestFailed(failure) => failure.api_exception.as_ref(),
            Self::Context { source, .. } => source.api_exception(),
            Self::Transport(_) | Self::Local(_) => None,
        }
    }

    /// Add display context while retaining the typed source error.
    #[must_use]
    pub fn context(self, context: impl Into<String>) -> Self {
        Self::Context {
            context: context.into(),
            source: Box::new(self),
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

    let details = error_message(headers, body, fault, detail);
    ScaleSetError::RequestFailed(Box::new(RequestFailure {
        method: method.to_string(),
        url: url.to_string(),
        status: format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default()
        ),
        activity,
        request_id,
        message: details.message,
        fault: details.fault,
        status_fault: ScaleSetFault::for_status(status),
        api_exception: details.api_exception,
    }))
}

struct ErrorDetails {
    message: String,
    fault: Option<ScaleSetFault>,
    api_exception: Option<ActionsApiException>,
}

fn error_message(
    headers: &HeaderMap,
    body: &[u8],
    fault: Option<ScaleSetFault>,
    detail: &str,
) -> ErrorDetails {
    if body.is_empty() {
        return ErrorDetails {
            message: match fault {
                Some(fault) => format!("{detail}: {}: unknown error", fault.as_str()),
                None => format!("{detail}: unknown error"),
            },
            fault,
            api_exception: None,
        };
    }
    // A classified sentinel short-circuits body parsing (upstream
    // `errors.As(err, &scalesetErr)` branch).
    if let Some(fault) = fault {
        return ErrorDetails {
            message: format!("{detail}: {}: {}", fault.as_str(), lossy(body)),
            fault: Some(fault),
            api_exception: None,
        };
    }
    if let Some(content_type) = headers.get(reqwest::header::CONTENT_TYPE)
        && let Ok(value) = content_type.to_str()
        && value.contains("text/plain")
    {
        return ErrorDetails {
            message: format!("{detail}: {}", lossy(body)),
            fault: None,
            api_exception: None,
        };
    }
    let exception: ActionsException = match serde_json::from_slice(body) {
        Ok(exception) => exception,
        Err(_) => {
            return ErrorDetails {
                message: format!(
                    "{detail}: failed to unmarshal error response body: {:?}",
                    lossy(body)
                ),
                fault: None,
                api_exception: None,
            };
        }
    };
    let api_exception = ActionsApiException {
        type_name: exception.type_name,
        message: exception.message,
    };
    let fault = if api_exception.type_name.contains("AgentExistsException") {
        Some(ScaleSetFault::RunnerExists)
    } else if api_exception.type_name.contains("AgentNotFoundException") {
        Some(ScaleSetFault::RunnerNotFound)
    } else if api_exception.type_name.contains("JobStillRunningException") {
        Some(ScaleSetFault::JobStillRunning)
    } else {
        None
    };
    let message = match fault {
        Some(fault) => format!("{detail}: {}: {}", fault.as_str(), api_exception.message),
        None => format!("{detail}: {api_exception}"),
    };
    ErrorDetails {
        message,
        fault,
        api_exception: Some(api_exception),
    }
}

fn lossy(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[derive(serde::Deserialize)]
struct ActionsException {
    #[serde(rename = "typeName", default)]
    type_name: String,
    #[serde(default)]
    message: String,
}

fn redact_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "<invalid URL>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
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
        assert!(text.contains("status=\"401 Unauthorized\""), "{text}");
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
        assert_eq!(error.fault(), Some(ScaleSetFault::RunnerNotFound));
        assert_eq!(error.status_fault(), Some(ScaleSetFault::NotFound));
        let exception = error.api_exception().unwrap();
        assert_eq!(exception.type_name, "AgentNotFoundException");
        assert_eq!(exception.message, "no such runner");
        assert!(error.to_string().contains("runner not found"), "{}", error);
        assert!(error.to_string().starts_with("not found: request GET"));
        let wrapped = error.context("lookup runner");
        assert_eq!(wrapped.fault(), Some(ScaleSetFault::RunnerNotFound));
        assert_eq!(wrapped.status_fault(), Some(ScaleSetFault::NotFound));
        assert_eq!(
            wrapped
                .api_exception()
                .map(|exception| exception.type_name.as_str()),
            Some("AgentNotFoundException")
        );
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
        assert_eq!(exists.fault(), Some(ScaleSetFault::RunnerExists));
        assert_eq!(exists.status_fault(), Some(ScaleSetFault::Conflict));
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
        assert_eq!(running.fault(), Some(ScaleSetFault::JobStillRunning));
        assert_eq!(running.status_fault(), Some(ScaleSetFault::Conflict));
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
    fn unknown_api_exception_is_preserved_as_typed_data() {
        let error = request_response_error(
            "POST",
            "https://a.example/scalesets",
            StatusCode::UNPROCESSABLE_ENTITY,
            &HeaderMap::new(),
            br#"{"typeName":"FutureActionsException","message":"details"}"#,
            None,
            "unexpected status code",
        );
        assert_eq!(error.fault(), None);
        assert_eq!(error.status_fault(), None);
        let exception = error.api_exception().unwrap();
        assert_eq!(exception.type_name, "FutureActionsException");
        assert_eq!(exception.message, "details");
        assert!(error
            .to_string()
            .contains("FutureActionsException: details"));
    }

    #[test]
    fn debug_redacts_signed_queue_url_and_response_body() {
        let error = request_response_error(
            "GET",
            "https://queue.example/messages?sig=url-secret",
            StatusCode::INTERNAL_SERVER_ERROR,
            &HeaderMap::new(),
            b"body-secret",
            None,
            "unexpected status code",
        );
        let rendered = format!("{error:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("url-secret"), "{rendered}");
        assert!(!rendered.contains("body-secret"), "{rendered}");
    }

    #[test]
    fn bom_is_stripped() {
        assert_eq!(trim_byte_order_mark(b"\xef\xbb\xbf{}"), b"{}");
        assert_eq!(trim_byte_order_mark(b"{}"), b"{}");
    }
}

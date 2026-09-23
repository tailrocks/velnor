//! Scale-set error taxonomy (`errors.go` at [`crate::scaleset::UPSTREAM_COMMIT`]).
//!
//! Mirrors `MessageQueueTokenExpiredError`, the runner/job sentinels, the
//! top-level HTTP-code errors, and `newRequestResponseError` (activity ID +
//! GitHub request ID capture, exception-name mapping). Response bodies are
//! parsed for the known `typeName`/`message` shape; unknown bodies surface
//! the raw text for `text/plain` exactly like upstream.

use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use std::fmt;

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
            .field("type_name", &redact_text(&self.type_name))
            .field("message", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Display for ActionsApiException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: <redacted>", redact_text(&self.type_name))
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
            .field("method", &redact_text(&self.method))
            .field("url", &redact_url(&self.url))
            .field("status", &redact_text(&self.status))
            .field("activity", &redact_text(&self.activity))
            .field("request_id", &redact_text(&self.request_id))
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
            redact_text(&self.method),
            redact_url(&self.url),
            redact_text(&self.status),
            redact_text(&self.activity),
            redact_text(&self.request_id),
            redact_text(&self.message)
        )
    }
}

/// Scale-set protocol error.
pub enum ScaleSetError {
    /// A request failed; mirrors `newRequestResponseError` output.
    RequestFailed(Box<RequestFailure>),
    /// Transport failure (mirrors `sendRequest` failure wrapping).
    Transport(String),
    /// Local request-construction failure.
    Local(String),
    /// Context wrapper that preserves typed faults through `%w`-style layers.
    Context {
        context: String,
        source: Box<ScaleSetError>,
    },
}

impl fmt::Debug for ScaleSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestFailed(failure) => f.debug_tuple("RequestFailed").field(failure).finish(),
            Self::Transport(message) => f
                .debug_struct("ScaleSetError::Transport")
                .field("message", &redact_text(message))
                .finish(),
            Self::Local(message) => f
                .debug_struct("ScaleSetError::Local")
                .field("message", &redact_text(message))
                .finish(),
            Self::Context { context, source } => f
                .debug_struct("ScaleSetError::Context")
                .field("context", &redact_text(context))
                .field("source", source)
                .finish(),
        }
    }
}

impl fmt::Display for ScaleSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestFailed(failure) => failure.fmt(f),
            Self::Transport(message) => {
                write!(f, "failed to send request: {}", redact_text(message))
            }
            Self::Local(message) => f.write_str(&redact_text(message)),
            Self::Context { context, source } => {
                write!(f, "{}: {source}", redact_text(context))
            }
        }
    }
}

impl std::error::Error for ScaleSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Context { source, .. } => Some(source.as_ref()),
            Self::RequestFailed(_) | Self::Transport(_) | Self::Local(_) => None,
        }
    }
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
    let detail = redact_text(detail);
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
            message: format!("{detail}: {}: response body redacted", fault.as_str()),
            fault: Some(fault),
            api_exception: None,
        };
    }
    if let Some(content_type) = headers.get(reqwest::header::CONTENT_TYPE)
        && let Ok(value) = content_type.to_str()
        && value.contains("text/plain")
    {
        return ErrorDetails {
            message: format!("{detail}: response body redacted"),
            fault: None,
            api_exception: None,
        };
    }
    let exception: ActionsException = match serde_json::from_slice(body) {
        Ok(exception) => exception,
        Err(_) => {
            return ErrorDetails {
                message: format!("{detail}: failed to unmarshal error response body"),
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
        Some(fault) => format!("{detail}: {}: response details redacted", fault.as_str()),
        None => format!("{detail}: {api_exception}"),
    };
    ErrorDetails {
        message,
        fault,
        api_exception: Some(api_exception),
    }
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

/// Redact credentials and authenticated URLs embedded in a diagnostic string.
/// Response bodies are never passed through this function: callers use a
/// fixed redacted description for body-bearing error paths instead.
pub(crate) fn redact_text(raw: &str) -> String {
    let with_urls = redact_embedded_urls(raw);
    redact_sensitive_values(&with_urls)
}

fn redact_embedded_urls(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    while cursor < raw.len() {
        let Some(relative_start) = find_url_start(&raw[cursor..]) else {
            output.push_str(&raw[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&raw[cursor..start]);
        let end = raw[start..]
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | ',')
            })
            .map_or(raw.len(), |offset| start + offset);
        output.push_str(&redact_url(&raw[start..end]));
        cursor = end;
    }
    output
}

fn find_url_start(raw: &str) -> Option<usize> {
    ["https://", "http://"]
        .iter()
        .filter_map(|prefix| raw.find(prefix))
        .min()
}

fn redact_sensitive_values(raw: &str) -> String {
    const SCHEMES: [&str; 3] = ["Bearer ", "RemoteAuth ", "Basic "];
    const KEYS: [&str; 13] = [
        "token",
        "access_token",
        "refresh_token",
        "authorization",
        "credential",
        "credentials",
        "sig",
        "signature",
        "client_secret",
        "private_key",
        "password",
        "secret",
        "body",
    ];

    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    while cursor < raw.len() {
        if let Some((scheme, value_start)) = SCHEMES.iter().find_map(|scheme| {
            raw[cursor..]
                .get(..scheme.len())
                .filter(|value| value.eq_ignore_ascii_case(scheme))
                .map(|_| (*scheme, cursor + scheme.len()))
        }) {
            output.push_str(scheme);
            output.push_str("<redacted>");
            cursor = skip_secret_value(raw, value_start);
            continue;
        }

        let Some((key_end, value_start)) = KEYS.iter().find_map(|key| {
            if !raw[cursor..]
                .get(..key.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(key))
                || !is_value_key_boundary(raw, cursor)
            {
                return None;
            }
            let mut separator = cursor + key.len();
            while raw
                .as_bytes()
                .get(separator)
                .is_some_and(u8::is_ascii_whitespace)
            {
                separator += 1;
            }
            let separator_byte = *raw.as_bytes().get(separator)?;
            if separator_byte != b'=' && separator_byte != b':' {
                return None;
            }
            let mut value_start = separator + 1;
            while raw
                .as_bytes()
                .get(value_start)
                .is_some_and(u8::is_ascii_whitespace)
            {
                value_start += 1;
            }
            Some((separator + 1, value_start))
        }) else {
            let character = raw[cursor..].chars().next().unwrap_or_default();
            output.push(character);
            cursor += character.len_utf8();
            continue;
        };

        output.push_str(&raw[cursor..key_end]);
        output.push_str("<redacted>");
        cursor = skip_secret_value(raw, value_start);
    }
    output
}

fn is_value_key_boundary(raw: &str, start: usize) -> bool {
    raw[..start].chars().next_back().is_none_or(|character| {
        !character.is_ascii_alphanumeric() && character != '_' && character != '-'
    })
}

fn skip_secret_value(raw: &str, start: usize) -> usize {
    let Some(first) = raw[start..].chars().next() else {
        return start;
    };
    if first == '\'' || first == '"' {
        return raw[start + first.len_utf8()..]
            .find(first)
            .map_or(raw.len(), |offset| {
                start + first.len_utf8() + offset + first.len_utf8()
            });
    }
    raw[start..]
        .find(|character: char| {
            character.is_whitespace()
                || matches!(character, '&' | ',' | ';' | '}' | ']' | '"' | '\'')
        })
        .map_or(raw.len(), |offset| start + offset)
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
            .contains("FutureActionsException: <redacted>"));
        assert!(!error.to_string().contains("details"));
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
        let rendered = error.to_string();
        assert!(!rendered.contains("url-secret"), "{rendered}");
        assert!(!rendered.contains("body-secret"), "{rendered}");
    }

    #[test]
    fn display_and_debug_redact_embedded_transport_credentials() {
        let error = ScaleSetError::Transport(
            "curl https://queue.example/messages?sig=url-secret Bearer token-secret body=body-secret"
                .into(),
        );
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("url-secret"), "{rendered}");
            assert!(!rendered.contains("token-secret"), "{rendered}");
            assert!(!rendered.contains("body-secret"), "{rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
        }
    }

    #[test]
    fn bom_is_stripped() {
        assert_eq!(trim_byte_order_mark(b"\xef\xbb\xbf{}"), b"{}");
        assert_eq!(trim_byte_order_mark(b"{}"), b"{}");
    }
}

//! Narrow application ports used by control-plane adapters.
//!
//! These contracts deliberately contain no transport types. HTTP, Unix
//! sockets, and future remote adapters translate into these requests before
//! reaching application code.

use std::fmt;

use serde::{Deserialize, Serialize};
use velnor_model::{AnyResource, Event};

/// Safe failure returned by an application port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// The caller lacks the required trusted identity or permission.
    Authorization { operation: String },
    /// The requested use case is not implemented by the selected adapter.
    Unsupported { operation: String },
    /// The request failed domain validation.
    Invalid { field: String, message: String },
    /// The requested resource or observation is unavailable.
    Unavailable { resource: String },
    /// An opaque cursor or expected version is stale.
    Conflict { operation: String },
    /// The operation itself failed after validation.
    Operation { operation: String },
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authorization { operation } => {
                write!(formatter, "authorization denied for {operation}")
            }
            Self::Unsupported { operation } => {
                write!(formatter, "unsupported operation {operation}")
            }
            Self::Invalid { field, message } => write!(formatter, "invalid {field}: {message}"),
            Self::Unavailable { resource } => write!(formatter, "resource unavailable: {resource}"),
            Self::Conflict { operation } => write!(formatter, "conflict: {operation}"),
            Self::Operation { operation } => write!(formatter, "operation failed: {operation}"),
        }
    }
}

impl std::error::Error for PortError {}

/// Bounded read request shared by resource query services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRequest {
    /// Resource noun, for example `jobs` or `events`.
    pub resource_kind: String,
    /// Include-only selector expression.
    pub selector: Option<String>,
    /// Equality selector expression.
    pub field_selector: Option<String>,
    /// Opaque continuation token from an earlier page.
    pub page_token: Option<String>,
    /// Maximum number of resources requested.
    pub limit: u32,
    /// Lower observation-time bound, expressed as an RFC 3339 string.
    pub since: Option<String>,
}

impl Default for QueryRequest {
    fn default() -> Self {
        Self {
            resource_kind: String::new(),
            selector: None,
            field_selector: None,
            page_token: None,
            limit: 100,
            since: None,
        }
    }
}

/// One bounded query page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPage {
    /// Resources in stable order.
    pub resources: Vec<AnyResource>,
    /// Opaque token for the next page, when more data exists.
    pub next_page_token: Option<String>,
}

/// Request for an ordered event watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRequest {
    /// Resource or subject filter.
    pub resource_kind: Option<String>,
    /// Resume after this resource version.
    pub after_version: Option<u64>,
    /// Maximum buffered items for this observation.
    pub limit: u32,
}

/// One watch item with a monotonic stream version.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WatchItem {
    /// Monotonic version used for reconnect and gap detection.
    pub version: u64,
    /// Sanitized event payload.
    pub event: Event,
}

/// Request for one bounded log page or stream segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRequest {
    /// Canonical job/run identity, when the source needs one.
    pub subject: String,
    /// Optional source selector such as `active` or `github`.
    pub source: Option<String>,
    /// Opaque cursor for reconnect.
    pub cursor: Option<String>,
    /// Maximum records returned.
    pub limit: u32,
}

/// One sanitized log record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LogItem {
    /// Canonical job/run identity owning the record.
    pub subject: String,
    /// Cursor position after this record.
    pub cursor: String,
    /// Source selected by the service.
    pub source: String,
    /// Monotonic record sequence.
    pub sequence: u64,
    /// Redacted log text.
    pub message: String,
}

/// Supported lifecycle mutation intents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationKind {
    /// Stop accepting new work for an instance.
    Cordon,
    /// Resume admission for an instance.
    Uncordon,
    /// Drain an instance to a terminal state.
    Drain,
    /// Recycle one stable slot or ephemeral runner.
    Recycle,
    /// Resume a drained instance.
    Resume,
    /// Restart an instance after a graceful drain.
    Restart,
    /// Change desired stable slot count.
    Scale,
    /// Reconcile one exact target.
    Reconcile,
}

/// Explicit, idempotent mutation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRequest {
    /// Operation kind.
    pub kind: MutationKind,
    /// Canonical target identity.
    pub target: String,
    /// Human reason recorded in the audit event.
    pub reason: String,
    /// Caller-supplied retry key.
    pub idempotency_key: String,
    /// Expected target version, when the caller reviewed one.
    pub expected_version: Option<u64>,
    /// Desired stable-slot count for `Scale`; absent for other operations.
    pub scale_to: Option<u32>,
}

/// Result of an accepted mutation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationResult {
    /// Durable operation identity.
    pub operation_id: String,
    /// Current operation phase.
    pub phase: String,
    /// Resulting resource, when immediately available.
    pub resource: Option<AnyResource>,
}

/// Read-only resource query port.
pub trait QueryPort: Send + Sync {
    /// Query one bounded page of sanitized resources.
    fn query(&self, request: QueryRequest) -> Result<QueryPage, PortError>;
}

/// Ordered event observation port.
pub trait WatchPort: Send + Sync {
    /// Read one bounded segment after an optional cursor.
    fn watch(&self, request: WatchRequest) -> Result<Vec<WatchItem>, PortError>;
}

/// Sanitized log access port.
pub trait LogPort: Send + Sync {
    /// Read one bounded segment or stream page.
    fn logs(&self, request: LogRequest) -> Result<Vec<LogItem>, PortError>;
}

/// Explicit lifecycle/reconciliation mutation port.
pub trait MutationPort: Send + Sync {
    /// Apply one reviewed operation; unsupported operations fail closed.
    fn mutate(&self, request: MutationRequest) -> Result<MutationResult, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_defaults_to_bounded_page() {
        assert_eq!(QueryRequest::default().limit, 100);
    }

    #[test]
    fn unsupported_port_error_is_actionable_without_internal_details() {
        let error = PortError::Unsupported {
            operation: "drain".to_owned(),
        };
        assert_eq!(error.to_string(), "unsupported operation drain");
    }
}

//! Bounded, cursor-based sanitized log service.

use std::fmt::Write as _;
use std::sync::{Arc, RwLock};

use crate::ports::{LogItem, LogPort, LogRequest, PortError};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use velnor_model::redaction::SecretMasker;

const MAX_BUFFER: usize = 16_384;
const MAX_RECORD_BYTES: usize = 256 * 1024;
const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const MAX_SUBJECT_BYTES: usize = 512;
const MAX_SECRET_COUNT: usize = 64;
const MAX_SECRET_BYTES: usize = 16 * 1024;
#[derive(Clone, Default)]
struct LogState {
    generation: u64,
    next_sequence: u64,
    bytes: usize,
    records: std::collections::VecDeque<LogItem>,
}

/// One bounded log stream. Raw input is redacted before it enters state.
#[derive(Clone)]
pub struct LogService {
    state: Arc<RwLock<LogState>>,
    supported: bool,
}

impl Default for LogService {
    fn default() -> Self {
        Self::new()
    }
}

impl LogService {
    /// Create an empty log service.
    #[must_use]
    pub fn new() -> Self {
        Self::with_support(true)
    }

    /// Create the production placeholder until durable log storage is wired.
    /// It fails closed rather than reporting an empty stream.
    #[must_use]
    pub(crate) fn unsupported() -> Self {
        Self::with_support(false)
    }

    fn with_support(supported: bool) -> Self {
        Self {
            state: Arc::new(RwLock::new(LogState::default())),
            supported,
        }
    }

    /// Append a line after replacing known secrets and path-like internals.
    pub fn append(
        &self,
        subject: &str,
        source: &str,
        message: &str,
        secrets: &[&str],
    ) -> Result<String, PortError> {
        if !self.supported {
            return Err(unsupported());
        }
        if !valid_identity(subject) || !valid_identity(source) {
            return Err(PortError::Invalid {
                field: "subject/source".to_owned(),
                message: format!(
                    "subject and source are required, control-free, and at most {MAX_SUBJECT_BYTES} bytes"
                ),
            });
        }
        let sanitized = redact_message(message, secrets)?;
        if sanitized.len() > MAX_RECORD_BYTES {
            return Err(PortError::Invalid {
                field: "message".to_owned(),
                message: format!(
                    "message must be at most {MAX_RECORD_BYTES} bytes after redaction"
                ),
            });
        }
        if contains_unredacted_secret(subject, secrets)
            || contains_unredacted_secret(source, secrets)
        {
            return Err(PortError::Invalid {
                field: "subject/source".to_owned(),
                message: "subject and source must not contain secrets".to_owned(),
            });
        }
        let mut state = self.state.write().map_err(|_| unavailable())?;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let cursor = format!(
            "v2:{}:{sequence}:{}",
            state.generation,
            cursor_fingerprint(subject, source)
        );
        let record_size = subject
            .len()
            .saturating_add(source.len())
            .saturating_add(cursor.len())
            .saturating_add(sanitized.len());
        if record_size > MAX_BUFFER_BYTES {
            return Err(PortError::Invalid {
                field: "message".to_owned(),
                message: format!("log record must be at most {MAX_BUFFER_BYTES} bytes"),
            });
        }
        state.records.push_back(LogItem {
            subject: subject.to_owned(),
            cursor: cursor.clone(),
            source: source.to_owned(),
            sequence,
            message: sanitized,
        });
        state.bytes = state.bytes.saturating_add(record_size);
        while state.records.len() > MAX_BUFFER || state.bytes > MAX_BUFFER_BYTES {
            if let Some(record) = state.records.pop_front() {
                state.bytes = state.bytes.saturating_sub(record_bytes(&record));
            }
            state.generation = state.generation.saturating_add(1);
        }
        Ok(cursor)
    }
}

impl LogPort for LogService {
    fn logs(&self, request: LogRequest) -> Result<Vec<LogItem>, PortError> {
        if !self.supported {
            return Err(unsupported());
        }
        if !valid_identity(&request.subject)
            || request
                .source
                .as_deref()
                .is_some_and(|source| !valid_identity(source))
            || request.limit == 0
            || request.limit as usize > MAX_BUFFER
        {
            return Err(PortError::Invalid {
                field: "subject/limit".to_owned(),
                message: format!(
                    "subject/source must be nonempty, control-free, and at most {MAX_SUBJECT_BYTES} bytes; limit must be 1..{MAX_BUFFER}"
                ),
            });
        }
        let state = self.state.read().map_err(|_| unavailable())?;
        let after = request
            .cursor
            .as_deref()
            .map(|cursor| {
                parse_cursor(
                    cursor,
                    &request.subject,
                    request.source.as_deref().unwrap_or_default(),
                )
            })
            .transpose()?;
        if let Some((generation, sequence)) = after {
            if sequence >= state.next_sequence {
                return Err(PortError::Invalid {
                    field: "cursor".to_owned(),
                    message: "log cursor is ahead of the stream".to_owned(),
                });
            }
            if generation != state.generation
                && state
                    .records
                    .front()
                    .is_some_and(|record| sequence < record.sequence)
            {
                return Err(PortError::Conflict {
                    operation: "log cursor expired; resnapshot required".to_owned(),
                });
            }
        }
        let mut result = Vec::new();
        let mut response_bytes = 0_usize;
        for record in state
            .records
            .iter()
            .filter(|record| {
                record.subject == request.subject
                    && request
                        .source
                        .as_deref()
                        .is_none_or(|source| source == record.source)
            })
            .filter(|record| after.is_none_or(|(_, sequence)| record.sequence > sequence))
        {
            let bytes = record_bytes(record);
            if !result.is_empty() && response_bytes.saturating_add(bytes) > MAX_BUFFER_BYTES {
                break;
            }
            response_bytes = response_bytes.saturating_add(bytes);
            result.push(record.clone());
            if result.len() == request.limit as usize {
                break;
            }
        }
        Ok(result)
    }
}

fn record_bytes(record: &LogItem) -> usize {
    record
        .subject
        .len()
        .saturating_add(record.cursor.len())
        .saturating_add(record.source.len())
        .saturating_add(record.message.len())
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_SUBJECT_BYTES
        && !value.chars().any(char::is_control)
}

fn redact_message(message: &str, secrets: &[&str]) -> Result<String, PortError> {
    if message.len() > MAX_RECORD_BYTES {
        return Err(PortError::Invalid {
            field: "message".to_owned(),
            message: format!("message must be at most {MAX_RECORD_BYTES} bytes"),
        });
    }
    let secret_bytes = secrets
        .iter()
        .fold(0_usize, |total, secret| total.saturating_add(secret.len()));
    if secrets.len() > MAX_SECRET_COUNT || secret_bytes > MAX_SECRET_BYTES {
        return Err(PortError::Invalid {
            field: "secrets".to_owned(),
            message: "secret registry exceeds bounded limits".to_owned(),
        });
    }
    let output = replace_literal_secrets(message, secrets)?;
    if contains_unredacted_secret(&output, secrets) {
        return Err(PortError::Invalid {
            field: "message".to_owned(),
            message: "message contains an encoded secret and was rejected".to_owned(),
        });
    }
    Ok(output)
}

fn replace_literal_secrets(message: &str, secrets: &[&str]) -> Result<String, PortError> {
    // One masker for the whole system: same sentinel, same encoded-variant
    // and multi-line rules as the runner and the durable store validator.
    let masker = SecretMasker::new(secrets.iter().copied());
    if masker.is_empty() {
        return Ok(message.to_owned());
    }
    let output = masker.mask(message);
    if output.len() > MAX_RECORD_BYTES {
        return Err(PortError::Invalid {
            field: "message".to_owned(),
            message: format!("message must be at most {MAX_RECORD_BYTES} bytes after redaction"),
        });
    }
    Ok(output)
}

/// Detect a supplied secret in canonical Unicode form. Any escape/entity
/// syntax is rejected whenever a mask registry is active: decoding every
/// possible shell, URL, JSON, and Unicode dialect safely is not feasible, so
/// this boundary fails closed instead of persisting an ambiguous payload.
fn contains_unredacted_secret(message: &str, secrets: &[&str]) -> bool {
    if secrets.iter().all(|secret| secret.is_empty()) {
        return false;
    }
    let normalized_message: String = message.nfkc().collect();
    if secrets
        .iter()
        .copied()
        .filter(|secret| !secret.is_empty())
        .any(|secret| {
            let normalized_secret: String = secret.nfkc().collect();
            normalized_message.contains(&normalized_secret)
        })
    {
        return true;
    }
    !secrets.is_empty() && has_encoded_syntax(message)
}

fn has_encoded_syntax(input: &str) -> bool {
    let bytes = input.as_bytes();
    bytes.contains(&b'%')
        || bytes.contains(&b'\\')
        || bytes.windows(2).any(|window| window == b"&#")
        || bytes
            .windows(2)
            .any(|window| window[0] == b'&' && window[1].is_ascii_alphabetic())
}

fn parse_cursor(raw: &str, subject: &str, source: &str) -> Result<(u64, u64), PortError> {
    let Some(raw) = raw.strip_prefix("v2:") else {
        return Err(PortError::Invalid {
            field: "cursor".to_owned(),
            message: "malformed log cursor".to_owned(),
        });
    };
    let mut fields = raw.split(':');
    let generation = fields.next().ok_or_else(|| PortError::Invalid {
        field: "cursor".to_owned(),
        message: "malformed log cursor".to_owned(),
    })?;
    let sequence = fields.next().ok_or_else(|| PortError::Invalid {
        field: "cursor".to_owned(),
        message: "malformed log cursor".to_owned(),
    })?;
    let fingerprint = fields.next().ok_or_else(|| PortError::Invalid {
        field: "cursor".to_owned(),
        message: "malformed log cursor".to_owned(),
    })?;
    if fields.next().is_some() || fingerprint != cursor_fingerprint(subject, source) {
        return Err(PortError::Invalid {
            field: "cursor".to_owned(),
            message: "log cursor does not belong to this stream".to_owned(),
        });
    }
    Ok((
        generation.parse().map_err(|_| PortError::Invalid {
            field: "cursor".to_owned(),
            message: "malformed log cursor".to_owned(),
        })?,
        sequence.parse().map_err(|_| PortError::Invalid {
            field: "cursor".to_owned(),
            message: "malformed log cursor".to_owned(),
        })?,
    ))
}

fn cursor_fingerprint(subject: &str, source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(subject.as_bytes());
    hasher.update([0]);
    hasher.update(source.as_bytes());
    let digest = hasher.finalize();
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(fingerprint, "{byte:02x}");
    }
    fingerprint
}

fn unavailable() -> PortError {
    PortError::Unavailable {
        resource: "log stream".to_owned(),
    }
}

fn unsupported() -> PortError {
    PortError::Unsupported {
        operation: "read logs".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_before_log_storage() {
        let service = LogService::new();
        let cursor = service
            .append("job-1", "active", "token=secret", &["secret"])
            .expect("append");
        let records = service
            .logs(LogRequest {
                subject: "job-1".to_owned(),
                source: Some("active".to_owned()),
                cursor: None,
                limit: 10,
            })
            .expect("read");
        assert!(!records[0].message.contains("secret"));
        assert!(cursor.starts_with("v2:"));
    }

    #[test]
    fn encoded_secrets_fail_closed_before_storage() {
        let service = LogService::new();
        for message in [r"tok\u0065n=\u0073ecret", "token=%73%65%63%72%65%74"] {
            assert!(service
                .append("job-1", "active", message, &["secret"])
                .is_err());
        }
    }

    #[test]
    fn nested_backslash_encoded_secrets_fail_closed() {
        let service = LogService::new();
        assert!(service
            .append("job-1", "active", r"token=\\u0073ecret", &["secret"])
            .is_err());
    }

    #[test]
    fn mixed_literal_and_encoded_secret_is_rejected() {
        let service = LogService::new();
        assert!(service
            .append(
                "job-1",
                "active",
                r"literal=secret encoded=\u0073ecret",
                &["secret"],
            )
            .is_err());
    }

    #[test]
    fn subject_and_source_are_bounded() {
        let service = LogService::new();
        assert!(service
            .append(&"s".repeat(MAX_SUBJECT_BYTES + 1), "active", "ok", &[])
            .is_err());
        assert!(service
            .append("job-1", &"s".repeat(MAX_SUBJECT_BYTES + 1), "ok", &[])
            .is_err());
    }

    #[test]
    fn secret_registry_is_bounded() {
        let service = LogService::new();
        let secrets = vec!["secret"; MAX_SECRET_COUNT + 1];
        assert!(service.append("job-1", "active", "ok", &secrets).is_err());
    }

    #[test]
    fn identity_fields_reject_literal_and_encoded_secrets() {
        let service = LogService::new();
        assert!(service
            .append("secret", "active", "ok", &["secret"])
            .is_err());
        assert!(service
            .append(r"job-\u0073ecret", "active", "ok", &["secret"])
            .is_err());
        assert!(service
            .append("job-1", "secret", "ok", &["secret"])
            .is_err());
    }

    #[test]
    fn unicode_equivalent_and_unknown_escaped_secrets_fail_closed() {
        let service = LogService::new();
        assert!(service
            .append("job-1", "active", "e\u{301}", &["é"])
            .is_err());
        assert!(service
            .append("job-1", "active", r"token=\U00000073ecret", &["secret"])
            .is_err());
    }

    #[test]
    fn log_cursor_is_bound_to_stream_identity_and_position() {
        let service = LogService::new();
        let cursor = service
            .append("job-1", "active", "ok", &[])
            .expect("append");
        assert!(service
            .logs(LogRequest {
                subject: "job-2".to_owned(),
                source: Some("active".to_owned()),
                cursor: Some(cursor.clone()),
                limit: 1,
            })
            .is_err());
        assert!(service
            .logs(LogRequest {
                subject: "job-1".to_owned(),
                source: Some("other".to_owned()),
                cursor: Some(cursor),
                limit: 1,
            })
            .is_err());
        let future = format!("v2:0:99:{}", cursor_fingerprint("job-1", "active"));
        assert!(service
            .logs(LogRequest {
                subject: "job-1".to_owned(),
                source: Some("active".to_owned()),
                cursor: Some(future),
                limit: 1,
            })
            .is_err());
    }
}

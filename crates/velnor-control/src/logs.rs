//! Bounded, cursor-based sanitized log service.

use std::sync::{Arc, RwLock};

use crate::ports::{LogItem, LogPort, LogRequest, PortError};

const MAX_BUFFER: usize = 16_384;
const MAX_RECORD_BYTES: usize = 256 * 1024;
const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Default)]
struct LogState {
    generation: u64,
    next_sequence: u64,
    bytes: usize,
    records: std::collections::VecDeque<LogItem>,
}

/// One bounded log stream. Raw input is redacted before it enters state.
#[derive(Clone, Default)]
pub struct LogService {
    state: Arc<RwLock<LogState>>,
}

impl LogService {
    /// Create an empty log service.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a line after replacing known secrets and path-like internals.
    pub fn append(
        &self,
        subject: &str,
        source: &str,
        message: &str,
        secrets: &[&str],
    ) -> Result<String, PortError> {
        if subject.trim().is_empty() || source.trim().is_empty() {
            return Err(PortError::Invalid {
                field: "subject/source".to_owned(),
                message: "log subject and source are required".to_owned(),
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
        let mut state = self.state.write().map_err(|_| unavailable())?;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let cursor = format!("v1:{}:{sequence}", state.generation);
        let record_bytes = sanitized.len();
        state.records.push_back(LogItem {
            subject: subject.to_owned(),
            cursor: cursor.clone(),
            source: source.to_owned(),
            sequence,
            message: sanitized,
        });
        state.bytes = state.bytes.saturating_add(record_bytes);
        while state.records.len() > MAX_BUFFER || state.bytes > MAX_BUFFER_BYTES {
            if let Some(record) = state.records.pop_front() {
                state.bytes = state.bytes.saturating_sub(record.message.len());
            }
            state.generation = state.generation.saturating_add(1);
        }
        Ok(cursor)
    }
}

impl LogPort for LogService {
    fn logs(&self, request: LogRequest) -> Result<Vec<LogItem>, PortError> {
        if request.subject.trim().is_empty()
            || request.limit == 0
            || request.limit as usize > MAX_BUFFER
        {
            return Err(PortError::Invalid {
                field: "subject/limit".to_owned(),
                message: format!("subject is required and limit must be 1..{MAX_BUFFER}"),
            });
        }
        let state = self.state.read().map_err(|_| unavailable())?;
        let after = request.cursor.as_deref().map(parse_cursor).transpose()?;
        if let Some((generation, sequence)) = after {
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
        Ok(state
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
            .take(request.limit as usize)
            .cloned()
            .collect())
    }
}

fn redact_message(message: &str, secrets: &[&str]) -> Result<String, PortError> {
    if message.len() > MAX_RECORD_BYTES {
        return Err(PortError::Invalid {
            field: "message".to_owned(),
            message: format!("message must be at most {MAX_RECORD_BYTES} bytes"),
        });
    }
    let replacement = "[REDACTED]";
    let mut output = String::with_capacity(message.len());
    let mut index = 0;
    while index < message.len() {
        let remaining = &message[index..];
        let secret = secrets
            .iter()
            .copied()
            .filter(|secret| !secret.is_empty() && remaining.starts_with(secret))
            .max_by_key(|secret| secret.len());
        if let Some(secret) = secret {
            if output.len().saturating_add(replacement.len()) > MAX_RECORD_BYTES {
                return Err(PortError::Invalid {
                    field: "message".to_owned(),
                    message: format!(
                        "message must be at most {MAX_RECORD_BYTES} bytes after redaction"
                    ),
                });
            }
            output.push_str(replacement);
            index += secret.len();
        } else {
            let character = remaining
                .chars()
                .next()
                .ok_or_else(|| PortError::Operation {
                    operation: "log redaction encountered invalid UTF-8 boundary".to_owned(),
                })?;
            output.push(character);
            index += character.len_utf8();
        }
    }
    Ok(output)
}

fn parse_cursor(raw: &str) -> Result<(u64, u64), PortError> {
    let Some(raw) = raw.strip_prefix("v1:") else {
        return Err(PortError::Invalid {
            field: "cursor".to_owned(),
            message: "malformed log cursor".to_owned(),
        });
    };
    let (generation, sequence) = raw.split_once(':').ok_or_else(|| PortError::Invalid {
        field: "cursor".to_owned(),
        message: "malformed log cursor".to_owned(),
    })?;
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

fn unavailable() -> PortError {
    PortError::Unavailable {
        resource: "log stream".to_owned(),
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
        assert!(cursor.starts_with("v1:"));
    }
}

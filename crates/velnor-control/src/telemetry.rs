//! Shared-file telemetry application service.
//!
//! Runner processes append best-effort envelopes to the instance telemetry
//! file. The control process reconstructs the bounded retained window through
//! the model reader, so observation does not depend on process-local memory.

use std::path::{Path, PathBuf};

use velnor_model::{TelemetryFileError, TelemetryFileReader, DEFAULT_TELEMETRY_FILE_BYTES};

use crate::ports::{PortError, TelemetryItem, TelemetryPage, TelemetryPort, TelemetryRequest};

/// Maximum page size accepted by the local telemetry API.
pub const MAX_TELEMETRY_LIMIT: u32 = 1024;

/// Derive the instance-scoped telemetry path from the authoritative state DB.
///
/// Instance identity is part of the filename because one host may supervise
/// several daemon instances while sharing the parent state directory.
#[must_use]
pub fn path_for_instance(state_path: &Path, instance: &str) -> PathBuf {
    let stem = state_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("state");
    let instance = instance_slug(instance);
    state_path.with_file_name(format!("{stem}.{instance}.telemetry.jsonl"))
}

fn instance_slug(raw: &str) -> String {
    let mut value = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if value.is_empty() || value == "." || value == ".." {
        value = "default".to_owned();
    }
    value
}

/// Control-plane telemetry reader for one daemon instance.
#[derive(Clone, Debug)]
pub struct TelemetryService {
    reader: TelemetryFileReader,
}

impl TelemetryService {
    /// Open a reader for one instance-owned NDJSON path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            reader: TelemetryFileReader::new(path, DEFAULT_TELEMETRY_FILE_BYTES)
                .expect("fixed telemetry file bound is valid"),
        }
    }

    /// Construct a reader that is intentionally empty for isolated tests.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Path::new("/nonexistent/velnor/telemetry.jsonl"))
    }
}

impl TelemetryPort for TelemetryService {
    fn telemetry(&self, request: TelemetryRequest) -> Result<TelemetryPage, PortError> {
        if request.limit == 0 || request.limit > MAX_TELEMETRY_LIMIT {
            return Err(PortError::Invalid {
                field: "limit".to_owned(),
                message: format!("must be between 1 and {MAX_TELEMETRY_LIMIT}"),
            });
        }

        let page = self
            .reader
            .read(request.after, request.limit as usize)
            .map_err(map_reader_error)?;
        Ok(TelemetryPage {
            records: page
                .records()
                .iter()
                .map(|record| TelemetryItem {
                    cursor: record.cursor(),
                    envelope: record.envelope().clone(),
                })
                .collect(),
            next_cursor: page.next_cursor(),
            dropped_before: page.dropped_before(),
        })
    }
}

fn map_reader_error(error: TelemetryFileError) -> PortError {
    match error {
        TelemetryFileError::CursorAhead => PortError::Conflict {
            operation: error.to_string(),
        },
        _ => PortError::Operation {
            operation: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_limit_is_bounded() {
        let service = TelemetryService::empty();
        let error = service
            .telemetry(TelemetryRequest {
                after: None,
                limit: MAX_TELEMETRY_LIMIT + 1,
            })
            .expect_err("oversized page must fail closed");
        assert!(matches!(error, PortError::Invalid { field, .. } if field == "limit"));
    }
}

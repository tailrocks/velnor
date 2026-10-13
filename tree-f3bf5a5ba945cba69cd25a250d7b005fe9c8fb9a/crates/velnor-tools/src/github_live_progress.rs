//! Append-only progress for a live GitHub capture.
//!
//! The progress file is deliberately smaller and less sensitive than the
//! final collection: it records safe request metadata only.  A capture is
//! complete only after the terminal `complete` event is appended; a process
//! that stops mid-collection therefore leaves an explicitly non-complete
//! ledger.

use super::live_collector::LiveProgressSink;
use super::{AcquisitionState, RequestRecord};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use url::Url;

pub const REQUEST_PROGRESS_FILE: &str = "request-progress.ndjson";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProgressTerminal {
    Complete,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum ProgressEvent {
    Started {
        schema_version: u32,
        started_at_utc: String,
    },
    Request {
        schema_version: u32,
        request_id: String,
        endpoint: String,
        state: AcquisitionState,
        complete: bool,
        http_status: Option<u16>,
        page: u32,
        started_at_utc: String,
        completed_at_utc: String,
    },
    Terminal {
        schema_version: u32,
        status: ProgressTerminal,
        completed_at_utc: String,
    },
}

/// Owns one immutable capture's append-only request progress file.
pub struct LiveProgressFile {
    path: PathBuf,
    file: File,
    terminal: Option<ProgressTerminal>,
}

impl LiveProgressFile {
    pub fn create(evidence_dir: &Path) -> Result<Self> {
        let path = evidence_dir.join(REQUEST_PROGRESS_FILE);
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create append-only progress file {}", path.display()))?;
        let mut progress = Self {
            path,
            file,
            terminal: None,
        };
        progress.append(ProgressEvent::Started {
            schema_version: 1,
            started_at_utc: utc_now(),
        })?;
        Ok(progress)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_complete(&self) -> bool {
        self.terminal == Some(ProgressTerminal::Complete)
    }

    pub fn mark_complete(&mut self) -> Result<()> {
        self.mark_terminal(ProgressTerminal::Complete)
    }

    pub fn mark_failed(&mut self) -> Result<()> {
        self.mark_terminal(ProgressTerminal::Failed)
    }

    /// Validate a persisted progress stream before treating its terminal
    /// state as authoritative.  A final partial JSON line, a request after a
    /// terminal event, or a second terminal event is never accepted.
    pub fn validate(path: &Path) -> Result<bool> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read request progress {}", path.display()))?;
        if bytes.is_empty() || !bytes.ends_with(b"\n") {
            bail!("request progress has a truncated final line");
        }
        let mut started = false;
        let mut terminal = None;
        let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        if !lines.last().is_some_and(|line| line.is_empty()) {
            bail!("request progress has no final line terminator");
        }
        for line in &lines[..lines.len() - 1] {
            if line.is_empty() {
                bail!("request progress contains an empty event line");
            }
            let event: ProgressEvent =
                serde_json::from_slice(line).context("parse request progress event")?;
            match event {
                ProgressEvent::Started { .. } if !started && terminal.is_none() => {
                    started = true;
                }
                ProgressEvent::Request { .. } if started && terminal.is_none() => {}
                ProgressEvent::Terminal { status, .. } if started && terminal.is_none() => {
                    terminal = Some(status);
                }
                _ => bail!("request progress event order is invalid"),
            }
        }
        if !started {
            bail!("request progress has no started event");
        }
        Ok(terminal == Some(ProgressTerminal::Complete))
    }

    fn mark_terminal(&mut self, status: ProgressTerminal) -> Result<()> {
        if self.terminal.is_some() {
            bail!("progress file already has a terminal event");
        }
        self.append(ProgressEvent::Terminal {
            schema_version: 1,
            status,
            completed_at_utc: utc_now(),
        })?;
        self.terminal = Some(status);
        Ok(())
    }

    fn append(&mut self, event: ProgressEvent) -> Result<()> {
        let mut bytes = serde_json::to_vec(&event).context("serialize request progress event")?;
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .and_then(|_| self.file.sync_data())
            .with_context(|| format!("append request progress {}", self.path.display()))
    }
}

impl LiveProgressSink for LiveProgressFile {
    fn record_request(&mut self, request: &RequestRecord) -> Result<()> {
        if self.terminal.is_some() {
            bail!("cannot append request after progress terminal event");
        }
        self.append(ProgressEvent::Request {
            schema_version: 1,
            request_id: request.request_id.clone(),
            endpoint: safe_endpoint(&request.endpoint_or_operation),
            state: request.state,
            complete: request.complete,
            http_status: request.http_status,
            page: request.page.number,
            started_at_utc: request.started_at_utc.clone(),
            completed_at_utc: request.completed_at_utc.clone(),
        })
    }
}

fn safe_endpoint(endpoint: &str) -> String {
    let Ok(url) = Url::parse(endpoint) else {
        return "operation".to_owned();
    };
    let Some(host) = url.host_str() else {
        return "operation".to_owned();
    };
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    format!("{}://{}{}{}", url.scheme(), host, port, path)
}

fn utc_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::super::{ApiKind, HttpMethod, PageState};
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn request() -> RequestRecord {
        RequestRecord {
            request_id: "request-0001".to_owned(),
            api: ApiKind::Rest,
            method: HttpMethod::Get,
            endpoint_or_operation: "https://api.github.com/repos/example/project?token=secret"
                .to_owned(),
            query_base64: "raw-query-secret".to_owned(),
            variables_base64: "raw-variables-secret".to_owned(),
            query_sha256: None,
            variables_sha256: None,
            redacted_variables: Some(serde_json::json!({"token": "secret"})),
            auth_identity_ref: "github-token".to_owned(),
            started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            http_status: Some(200),
            api_request_id: None,
            rate_limit: None,
            safe_scopes: None,
            page: PageState {
                number: 1,
                per_page: Some(100),
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: None,
                items_returned: 1,
            },
            response_raw_ref: Some("raw-secret".to_owned()),
            error_raw_ref: None,
            state: AcquisitionState::Complete,
            complete: true,
            truncation_reason: None,
        }
    }

    fn temp_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "velnor-live-progress-{}-{}",
            std::process::id(),
            TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(path)
    }

    #[test]
    fn progress_excludes_raw_and_credential_fields() -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let mut progress = LiveProgressFile::create(&directory)?;
        progress.record_request(&request())?;
        let text = fs::read_to_string(progress.path())?;
        assert!(text.contains("/repos/example/project"));
        assert!(!text.contains("raw-query-secret"));
        assert!(!text.contains("raw-variables-secret"));
        assert!(!text.contains("raw-secret"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("bytes_base64"));
        assert!(!text.contains("Authorization"));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn interrupted_or_failed_progress_never_reports_complete(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let mut progress = LiveProgressFile::create(&directory)?;
        progress.record_request(&request())?;
        assert!(!progress.is_complete());
        let interrupted = fs::read_to_string(progress.path())?;
        assert!(!interrupted.contains("\"status\":\"complete\""));
        progress.mark_failed()?;
        assert!(!progress.is_complete());
        let failed = fs::read_to_string(progress.path())?;
        assert!(failed.contains("\"status\":\"failed\""));
        assert!(failed.starts_with(&interrupted));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn complete_is_one_append_only_terminal_event() -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let mut progress = LiveProgressFile::create(&directory)?;
        progress.record_request(&request())?;
        progress.mark_complete()?;
        assert!(progress.is_complete());
        let text = fs::read_to_string(progress.path())?;
        assert_eq!(text.matches("\"event\":\"terminal\"").count(), 1);
        assert!(text.contains("\"status\":\"complete\""));
        assert!(LiveProgressFile::validate(progress.path())?);
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn truncated_final_progress_line_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let mut progress = LiveProgressFile::create(&directory)?;
        progress.record_request(&request())?;
        progress.mark_complete()?;
        let mut bytes = fs::read(progress.path())?;
        bytes.pop();
        fs::write(progress.path(), bytes)?;
        assert!(LiveProgressFile::validate(progress.path()).is_err());
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn progress_creation_refuses_reuse_of_an_existing_capture(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let _progress = LiveProgressFile::create(&directory)?;
        assert!(LiveProgressFile::create(&directory).is_err());
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn safe_endpoint_drops_query_and_rejects_non_url_operations() {
        assert_eq!(
            safe_endpoint("https://api.github.com/repos/a/b?token=secret"),
            "https://api.github.com/repos/a/b"
        );
        assert_eq!(safe_endpoint("graphql operation secret"), "operation");
    }

    #[test]
    fn progress_event_is_json_lines() -> Result<(), Box<dyn std::error::Error>> {
        let directory = temp_dir()?;
        let mut progress = LiveProgressFile::create(&directory)?;
        progress.record_request(&request())?;
        for line in fs::read_to_string(progress.path())?.lines() {
            let _: BTreeMap<String, serde_json::Value> = serde_json::from_str(line)?;
        }
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}

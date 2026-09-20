//! Fail-closed analysis of saved GitHub Actions run and job responses.
//!
//! This module deliberately does not contact GitHub. A transport-side
//! collector can save every REST response, then replay those responses here.
//! Raw API timestamps are the timing source. Human log markers and generated
//! reports are not consulted by this parser.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const UNKNOWN: &str = "unknown";

/// Options that affect only explicit classification, never timestamp parsing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowCollectOptions {
    /// Exact job names whose completion is reported as a separate required
    /// gate. Without a selector, gate timing stays unknown.
    pub required_job_names: Vec<String>,
}

/// A referenced reusable workflow. Its SHA identifies workflow configuration,
/// not a pull-request merge commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencedWorkflow {
    /// Referenced workflow path, when supplied by GitHub.
    pub path: Option<String>,
    /// Referenced workflow revision, when supplied by GitHub.
    pub sha: Option<String>,
    /// Referenced workflow ref, when supplied by GitHub.
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
}

/// Evidence retained when a `referenced_workflows` member cannot be decoded
/// as a workflow object. The raw member is retained so a later review can
/// distinguish malformed input from an absent field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencedWorkflowEvidenceIssue {
    /// Zero-based member index in the raw `referenced_workflows` array.
    pub member_index: usize,
    /// Why the member was not accepted as a workflow record.
    pub message: String,
    /// Compact JSON for the malformed member or field value.
    pub raw_json: String,
}

/// One raw run record normalized from a GitHub response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowRunInput {
    /// GitHub workflow run ID.
    pub run_id: Option<u64>,
    /// Workflow run number.
    pub run_number: Option<u64>,
    /// GitHub rerun attempt.
    pub run_attempt: Option<u64>,
    /// Workflow display name.
    pub name: Option<String>,
    /// Event that created the run.
    pub event: Option<String>,
    /// API lifecycle status.
    pub status: Option<String>,
    /// API conclusion.
    pub conclusion: Option<String>,
    /// Branch shown by the API.
    pub head_branch: Option<String>,
    /// Ref supplied by the raw response, when present.
    pub ref_name: Option<String>,
    /// Raw `head_sha` returned by the run API. This is run-record metadata,
    /// not proof of the effective checkout commit.
    pub head_sha: Option<String>,
    /// Immutable pull-request source SHA, only when the run event/ref proves it.
    pub source_sha: Option<String>,
    /// Evidence used for the source SHA.
    pub source_sha_basis: Option<String>,
    /// The first pull request head SHA in the current API projection. This is
    /// retained as evidence only; it is never used as historical identity.
    pub observed_pull_request_head_sha: Option<String>,
    /// Checkout-proven merge SHA. The run API alone cannot populate this.
    pub merge_sha: Option<String>,
    /// Pull-request merge ref observed in the run response.
    pub merge_ref: Option<String>,
    /// Evidence used for a checkout-proven merge SHA.
    pub merge_sha_basis: Option<String>,
    /// Workflow configuration revisions from `referenced_workflows`.
    pub referenced_workflows: Vec<ReferencedWorkflow>,
    /// Malformed referenced-workflow members retained as explicit evidence.
    pub referenced_workflow_evidence_issues: Vec<ReferencedWorkflowEvidenceIssue>,
    /// Workflow ID.
    pub workflow_id: Option<u64>,
    /// Workflow path.
    pub workflow_path: Option<String>,
    /// Run creation timestamp, preserved byte-for-byte when present.
    pub created_at: Option<String>,
    /// Run start timestamp, preserved byte-for-byte when present.
    pub run_started_at: Option<String>,
    /// API update timestamp, retained as metadata and never used as end time.
    pub updated_at: Option<String>,
    /// Raw API completion timestamp, when a source supplies one. It is never
    /// substituted for the derived job completion timestamp.
    pub api_completed_at: Option<String>,
}

/// One raw step record from a workflow job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStepRecord {
    /// Step number when supplied.
    pub number: Option<u64>,
    /// Step display name.
    pub name: Option<String>,
    /// Step lifecycle status.
    pub status: Option<String>,
    /// Step conclusion.
    pub conclusion: Option<String>,
    /// Raw start timestamp.
    pub started_at: Option<String>,
    /// Raw completion timestamp.
    pub completed_at: Option<String>,
    /// Duration from raw step timestamps, or null when unavailable/invalid.
    pub duration_ms: Option<u64>,
    /// Why duration is absent or excluded.
    pub duration_state: String,
}

/// One raw workflow job record normalized from a GitHub response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowJobInput {
    /// GitHub job ID.
    pub job_id: Option<u64>,
    /// Parent workflow run ID.
    pub run_id: Option<u64>,
    /// GitHub rerun attempt.
    pub run_attempt: Option<u64>,
    /// Job display name.
    pub name: Option<String>,
    /// Job lifecycle status.
    pub status: Option<String>,
    /// Job conclusion.
    pub conclusion: Option<String>,
    /// Raw start timestamp.
    pub started_at: Option<String>,
    /// Raw completion timestamp.
    pub completed_at: Option<String>,
    /// Runner name.
    pub runner_name: Option<String>,
    /// Runner group name.
    pub runner_group_name: Option<String>,
    /// Runner ID.
    pub runner_id: Option<u64>,
    /// Raw step records.
    pub steps: Vec<WorkflowStepRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct JobPage {
    /// Jobs in this response page.
    pub jobs: Vec<WorkflowJobInput>,
    /// API total count, when present.
    pub total_count: Option<usize>,
    /// Page number supplied by a transport wrapper, when present.
    pub page: Option<usize>,
    /// Page size supplied by a transport wrapper, when present.
    pub per_page: Option<usize>,
    /// Run ID supplied by a transport wrapper, when jobs are empty.
    pub run_id_hint: Option<u64>,
    /// Count of malformed or identity-incomplete entries retained by the page.
    pub malformed_entries: usize,
}

#[derive(Debug, Clone, Default)]
struct PageStats {
    expected_count: Option<usize>,
    conflict: bool,
    pages: BTreeSet<usize>,
    per_page: Option<usize>,
    page_metadata_missing: bool,
    page_count: usize,
    malformed_entries: usize,
}

type WorkflowJobGroupKey = (Option<u64>, Option<u64>);
type WorkflowJobGroups = BTreeMap<WorkflowJobGroupKey, Vec<WorkflowJobInput>>;
type WorkflowJobConflicts = BTreeSet<WorkflowJobGroupKey>;

/// A flat JSONL/CSV observation. One row is emitted per job; a run with no
/// jobs emits one row with `job_id = null` so incomplete collection is visible.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowJobRecord {
    /// `job` or `run_without_jobs`.
    pub record_type: String,
    /// Run identity and metadata.
    pub run_id: Option<u64>,
    pub run_number: Option<u64>,
    pub run_attempt: Option<u64>,
    pub event: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_id: Option<u64>,
    pub workflow_path: Option<String>,
    pub run_status: Option<String>,
    pub run_conclusion: Option<String>,
    pub run_terminal: Option<bool>,
    pub head_branch: Option<String>,
    pub ref_name: Option<String>,
    pub head_sha: Option<String>,
    pub source_sha: Option<String>,
    pub source_sha_basis: Option<String>,
    pub observed_pull_request_head_sha: Option<String>,
    pub merge_sha: Option<String>,
    pub merge_ref: Option<String>,
    pub merge_sha_basis: Option<String>,
    pub referenced_workflows: Vec<ReferencedWorkflow>,
    pub referenced_workflow_evidence_issues: Vec<ReferencedWorkflowEvidenceIssue>,
    pub run_created_at: Option<String>,
    pub run_started_at: Option<String>,
    pub run_updated_at: Option<String>,
    pub run_api_completed_at: Option<String>,
    /// Job collection verification.
    pub jobs_expected: Option<usize>,
    pub jobs_observed: usize,
    pub jobs_complete: Option<bool>,
    pub malformed_job_entries: usize,
    pub attempt_match: bool,
    pub executed_timing_complete: Option<bool>,
    /// Freshness is independent from collection completeness. A rerun may
    /// expose complete job records from an earlier attempt.
    pub attempt_freshness_state: String,
    pub completion_state: String,
    /// Maximum observed non-skipped completion, including stale records when
    /// the attempt freshness state is mixed.
    pub all_jobs_completed_at: Option<String>,
    /// Original run creation to completion. This is a trigger-to-final metric
    /// only for the initial attempt; reruns retain the original created_at.
    pub run_lifetime_ms: Option<u64>,
    pub run_lifetime_state: String,
    /// Full attempt wall time is emitted only when every executed record is
    /// fresh and its timing is valid.
    pub attempt_wall_ms: Option<u64>,
    /// Wall time for the fresh, observed subset. This is explicitly partial
    /// when `attempt_freshness_state` is mixed or stale.
    pub fresh_attempt_wall_ms: Option<u64>,
    /// Execution sum excludes skipped jobs and is null when an executed job
    /// is stale, missing, invalid, or otherwise incomplete.
    pub execution_sum_ms: Option<u64>,
    pub execution_partial_sum_ms: u64,
    pub fresh_execution_partial_sum_ms: Option<u64>,
    pub stale_execution_partial_sum_ms: Option<u64>,
    pub fresh_execution_unknown_jobs: usize,
    pub stale_execution_unknown_jobs: usize,
    pub fresh_executed_jobs: usize,
    pub stale_executed_jobs: usize,
    pub freshness_unknown_jobs: usize,
    pub execution_unknown_jobs: usize,
    pub execution_unobserved_jobs: Option<usize>,
    pub skipped_jobs: usize,
    /// This is an unclassified pre-start interval, never queue time.
    pub pre_start_ms: Option<u64>,
    pub pre_start_state: String,
    /// Maximum individual job duration; it is not a critical path.
    pub max_job_duration_ms: Option<u64>,
    pub max_job_name: Option<String>,
    pub critical_path_ms: Option<u64>,
    pub critical_path_state: String,
    /// Required-gate timing is unknown unless explicit selectors were passed.
    pub required_gate_completed_at: Option<String>,
    pub required_gate_state: String,
    /// Job identity and raw timing.
    pub job_id: Option<u64>,
    pub job_attempt: Option<u64>,
    pub job_name: Option<String>,
    pub job_status: Option<String>,
    pub job_conclusion: Option<String>,
    pub runner_name: Option<String>,
    pub runner_group_name: Option<String>,
    pub runner_id: Option<u64>,
    pub job_started_at: Option<String>,
    pub job_completed_at: Option<String>,
    pub job_duration_ms: Option<u64>,
    pub job_duration_state: String,
    pub job_freshness_state: String,
    pub steps: Vec<WorkflowStepRecord>,
}

/// Errors raised while decoding raw saved API documents.
#[derive(Debug, Error)]
pub enum WorkflowInputError {
    /// A document was empty.
    #[error("workflow input document {source_name} is empty")]
    Empty { source_name: String },
    /// A JSON document or JSONL line was malformed.
    #[error("invalid workflow JSON in {source_name}: {message}")]
    InvalidJson {
        source_name: String,
        message: String,
    },
    /// A typed field had an unsupported shape.
    #[error("invalid workflow field in {source_name}: {message}")]
    InvalidField {
        source_name: String,
        message: String,
    },
}

/// Parse run response documents, accepting direct REST objects, collections,
/// JSONL, and connector wrappers containing JSON text.
pub fn parse_run_documents(
    documents: &[(String, String)],
) -> Result<Vec<WorkflowRunInput>, WorkflowInputError> {
    let mut runs = Vec::new();
    for (source_name, document) in documents {
        for value in parse_json_document(source_name, document)? {
            extract_runs(&value, &mut runs, source_name)?;
        }
    }
    Ok(runs)
}

/// Parse jobs response documents into normalized pages. Page metadata is
/// retained internally so completion can be censored when count verification
/// is impossible.
pub fn parse_job_documents(
    documents: &[(String, String)],
) -> Result<Vec<JobPage>, WorkflowInputError> {
    let mut pages = Vec::new();
    for (source_name, document) in documents {
        for value in parse_json_document(source_name, document)? {
            extract_job_pages(&value, &mut pages, source_name)?;
        }
    }
    Ok(pages)
}

/// Collect flat job observations from saved run and job response documents.
pub fn collect_workflow(
    run_documents: &[(String, String)],
    job_documents: &[(String, String)],
    options: &WorkflowCollectOptions,
) -> Result<Vec<WorkflowJobRecord>, WorkflowInputError> {
    let runs = parse_run_documents(run_documents)?;
    let pages = parse_job_documents(job_documents)?;
    Ok(build_records(runs, pages, options))
}

/// Write flat observations as JSONL.
pub fn write_workflow_jsonl<W: Write>(
    mut writer: W,
    records: &[WorkflowJobRecord],
) -> Result<(), serde_json::Error> {
    for record in records {
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n").map_err(serde_json::Error::io)?;
    }
    Ok(())
}

/// Write a deterministic flat CSV view. Unknown values are the literal
/// `unknown`; empty strings are never used to imply a measured zero.
pub fn write_workflow_csv<W: Write>(
    mut writer: W,
    records: &[WorkflowJobRecord],
) -> io::Result<()> {
    const HEADERS: &[&str] = &[
        "record_type",
        "run_id",
        "run_number",
        "run_attempt",
        "event",
        "workflow_name",
        "workflow_id",
        "workflow_path",
        "run_status",
        "run_conclusion",
        "run_terminal",
        "head_branch",
        "ref_name",
        "head_sha",
        "source_sha",
        "source_sha_basis",
        "observed_pull_request_head_sha",
        "merge_sha",
        "merge_ref",
        "merge_sha_basis",
        "referenced_workflows_json",
        "referenced_workflow_evidence_issues_json",
        "run_created_at",
        "run_started_at",
        "run_updated_at",
        "run_api_completed_at",
        "jobs_expected",
        "jobs_observed",
        "jobs_complete",
        "malformed_job_entries",
        "attempt_match",
        "executed_timing_complete",
        "attempt_freshness_state",
        "completion_state",
        "all_jobs_completed_at",
        "run_lifetime_ms",
        "run_lifetime_state",
        "attempt_wall_ms",
        "fresh_attempt_wall_ms",
        "execution_sum_ms",
        "execution_partial_sum_ms",
        "fresh_execution_partial_sum_ms",
        "stale_execution_partial_sum_ms",
        "fresh_execution_unknown_jobs",
        "stale_execution_unknown_jobs",
        "fresh_executed_jobs",
        "stale_executed_jobs",
        "freshness_unknown_jobs",
        "execution_unknown_jobs",
        "execution_unobserved_jobs",
        "skipped_jobs",
        "pre_start_ms",
        "pre_start_state",
        "max_job_duration_ms",
        "max_job_name",
        "critical_path_ms",
        "critical_path_state",
        "required_gate_completed_at",
        "required_gate_state",
        "job_id",
        "job_attempt",
        "job_name",
        "job_status",
        "job_conclusion",
        "runner_name",
        "runner_group_name",
        "runner_id",
        "job_started_at",
        "job_completed_at",
        "job_duration_ms",
        "job_duration_state",
        "job_freshness_state",
        "step_count",
        "timed_step_count",
        "steps_json",
    ];
    write_csv_row(&mut writer, HEADERS)?;
    for record in records {
        let referenced_workflows_json =
            serde_json::to_string(&record.referenced_workflows).map_err(io::Error::other)?;
        let referenced_workflow_evidence_issues_json =
            serde_json::to_string(&record.referenced_workflow_evidence_issues)
                .map_err(io::Error::other)?;
        let steps_json = serde_json::to_string(&record.steps).map_err(io::Error::other)?;
        let timed_steps = record
            .steps
            .iter()
            .filter(|step| step.duration_ms.is_some())
            .count();
        let fields = vec![
            record.record_type.clone(),
            display_opt(&record.run_id),
            display_opt(&record.run_number),
            display_opt(&record.run_attempt),
            display_opt(&record.event),
            display_opt(&record.workflow_name),
            display_opt(&record.workflow_id),
            display_opt(&record.workflow_path),
            display_opt(&record.run_status),
            display_opt(&record.run_conclusion),
            display_opt(&record.run_terminal),
            display_opt(&record.head_branch),
            display_opt(&record.ref_name),
            display_opt(&record.head_sha),
            display_opt(&record.source_sha),
            display_opt(&record.source_sha_basis),
            display_opt(&record.observed_pull_request_head_sha),
            display_opt(&record.merge_sha),
            display_opt(&record.merge_ref),
            display_opt(&record.merge_sha_basis),
            referenced_workflows_json,
            referenced_workflow_evidence_issues_json,
            display_opt(&record.run_created_at),
            display_opt(&record.run_started_at),
            display_opt(&record.run_updated_at),
            display_opt(&record.run_api_completed_at),
            display_opt(&record.jobs_expected),
            record.jobs_observed.to_string(),
            display_opt(&record.jobs_complete),
            record.malformed_job_entries.to_string(),
            record.attempt_match.to_string(),
            display_opt(&record.executed_timing_complete),
            record.attempt_freshness_state.clone(),
            record.completion_state.clone(),
            display_opt(&record.all_jobs_completed_at),
            display_opt(&record.run_lifetime_ms),
            record.run_lifetime_state.clone(),
            display_opt(&record.attempt_wall_ms),
            display_opt(&record.fresh_attempt_wall_ms),
            display_opt(&record.execution_sum_ms),
            record.execution_partial_sum_ms.to_string(),
            display_opt(&record.fresh_execution_partial_sum_ms),
            display_opt(&record.stale_execution_partial_sum_ms),
            record.fresh_execution_unknown_jobs.to_string(),
            record.stale_execution_unknown_jobs.to_string(),
            record.fresh_executed_jobs.to_string(),
            record.stale_executed_jobs.to_string(),
            record.freshness_unknown_jobs.to_string(),
            record.execution_unknown_jobs.to_string(),
            display_opt(&record.execution_unobserved_jobs),
            record.skipped_jobs.to_string(),
            display_opt(&record.pre_start_ms),
            record.pre_start_state.clone(),
            display_opt(&record.max_job_duration_ms),
            display_opt(&record.max_job_name),
            display_opt(&record.critical_path_ms),
            record.critical_path_state.clone(),
            display_opt(&record.required_gate_completed_at),
            record.required_gate_state.clone(),
            display_opt(&record.job_id),
            display_opt(&record.job_attempt),
            display_opt(&record.job_name),
            display_opt(&record.job_status),
            display_opt(&record.job_conclusion),
            display_opt(&record.runner_name),
            display_opt(&record.runner_group_name),
            display_opt(&record.runner_id),
            display_opt(&record.job_started_at),
            display_opt(&record.job_completed_at),
            display_opt(&record.job_duration_ms),
            record.job_duration_state.clone(),
            record.job_freshness_state.clone(),
            record.steps.len().to_string(),
            timed_steps.to_string(),
            steps_json,
        ];
        write_csv_row(&mut writer, &fields)?;
    }
    Ok(())
}

/// Render a ranked summary without pretending that the longest job is a
/// critical path.
pub fn render_workflow_summary(records: &[WorkflowJobRecord]) -> String {
    use std::fmt::Write as _;

    let mut ranked: Vec<&WorkflowJobRecord> = records
        .iter()
        .filter(|record| record.job_id.is_some())
        .filter(|record| record.job_duration_ms.is_some())
        .collect();
    ranked.sort_by(|left, right| {
        right
            .job_duration_ms
            .cmp(&left.job_duration_ms)
            .then_with(|| left.job_name.cmp(&right.job_name))
            .then_with(|| left.job_id.cmp(&right.job_id))
    });

    let mut run_states = BTreeMap::new();
    let mut unknown_job_durations = 0_usize;
    for record in records {
        let key = (record.run_id, record.run_attempt);
        run_states
            .entry(key)
            .and_modify(|state: &mut String| {
                if state == "verified" && record.completion_state != "verified" {
                    *state = record.completion_state.clone();
                }
            })
            .or_insert_with(|| record.completion_state.clone());
        if record.job_id.is_some() && record.job_duration_ms.is_none() {
            unknown_job_durations += 1;
        }
    }
    let verified = run_states
        .values()
        .filter(|state| state.as_str() == "verified")
        .count();
    let censored = run_states.len().saturating_sub(verified);

    let mut summary = String::new();
    let _ = writeln!(summary, "# Workflow timing summary");
    let _ = writeln!(summary);
    let _ = writeln!(
        summary,
        "Observed run/attempt groups: {}.",
        run_states.len()
    );
    let _ = writeln!(summary, "Verified run completion rows: {verified}.");
    let _ = writeln!(summary, "Censored or unknown completion rows: {censored}.");
    let _ = writeln!(
        summary,
        "Jobs with unknown duration: {unknown_job_durations}."
    );
    let _ = writeln!(
        summary,
        "Full execution and attempt-wall sums require a complete fresh attempt; observed fresh/stale partial sums and unknown counters remain separate."
    );
    let _ = writeln!(
        summary,
        "Pre-start intervals are unclassified; rerun intervals are not queue time, and no queue attribution or critical path is inferred without a dependency graph."
    );
    let _ = writeln!(
        summary,
        "The raw duration table includes stale records for evidence; rows with freshness other than fresh are not current-attempt bottlenecks."
    );
    let _ = writeln!(summary);
    let _ = writeln!(summary, "## Ranked jobs by raw API duration");
    let _ = writeln!(summary);
    let _ = writeln!(
        summary,
        "| Rank | Run | Attempt | Job | Duration ms | Conclusion | Runner | Job freshness | Completion state |"
    );
    let _ = writeln!(
        summary,
        "| ---: | ---: | ---: | --- | ---: | --- | --- | --- | --- |"
    );
    for (index, record) in ranked.into_iter().take(25).enumerate() {
        let _ = writeln!(
            summary,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            index + 1,
            display_opt(&record.run_id),
            display_opt(&record.run_attempt),
            markdown_cell(record.job_name.as_deref().unwrap_or(UNKNOWN)),
            display_opt(&record.job_duration_ms),
            markdown_cell(record.job_conclusion.as_deref().unwrap_or(UNKNOWN)),
            markdown_cell(record.runner_name.as_deref().unwrap_or(UNKNOWN)),
            record.job_freshness_state,
            record.completion_state,
        );
    }
    summary
}

fn write_csv_row<W: Write, S: AsRef<str>>(writer: &mut W, fields: &[S]) -> io::Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            writer.write_all(b",")?;
        }
        writer.write_all(b"\"")?;
        for byte in field.as_ref().as_bytes() {
            if *byte == b'"' {
                writer.write_all(b"\"\"")?;
            } else {
                writer.write_all(std::slice::from_ref(byte))?;
            }
        }
        writer.write_all(b"\"")?;
    }
    writer.write_all(b"\n")
}

fn display_opt<T: ToString>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn parse_json_document(
    source_name: &str,
    document: &str,
) -> Result<Vec<Value>, WorkflowInputError> {
    if document.trim().is_empty() {
        return Err(WorkflowInputError::Empty {
            source_name: source_name.to_owned(),
        });
    }

    if let Ok(value) = serde_json::from_str::<Value>(document) {
        let mut values = Vec::new();
        unwrap_response(value, &mut values);
        return Ok(values);
    }

    let mut values = Vec::new();
    for (line_index, line) in document.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).map_err(|error| {
            WorkflowInputError::InvalidJson {
                source_name: format!("{source_name}:{}", line_index + 1),
                message: error.to_string(),
            }
        })?;
        unwrap_response(value, &mut values);
    }
    if values.is_empty() {
        return Err(WorkflowInputError::InvalidJson {
            source_name: source_name.to_owned(),
            message: "no JSON values found".to_owned(),
        });
    }
    Ok(values)
}

fn unwrap_response(value: Value, output: &mut Vec<Value>) {
    match value {
        Value::String(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                unwrap_response(parsed, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                unwrap_response(value, output);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = object.get("text").and_then(Value::as_str)
            {
                unwrap_response(Value::String(text.to_owned()), output);
                return;
            }
            let mut wrapped = false;
            for key in ["structuredContent", "content", "data", "response"] {
                if let Some(value) = object.get(key) {
                    wrapped = true;
                    let value = if key == "response" {
                        annotate_page_metadata(value.clone(), object.get("source_url"))
                    } else {
                        value.clone()
                    };
                    unwrap_response(value, output);
                }
            }
            if !wrapped {
                output.push(Value::Object(object));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => output.push(value),
    }
}

fn annotate_page_metadata(value: Value, source_url: Option<&Value>) -> Value {
    let Some(source_url) = source_url.and_then(Value::as_str) else {
        return value;
    };
    let value = if let Some(text) = value.as_str() {
        serde_json::from_str(text).unwrap_or(value)
    } else {
        value
    };
    let Some(mut object) = value.as_object().cloned() else {
        return value;
    };
    for key in ["page", "per_page"] {
        if object.contains_key(key) {
            continue;
        }
        if let Some(value) = query_number(source_url, key) {
            object.insert(key.to_owned(), Value::from(value));
        }
    }
    Value::Object(object)
}

fn query_number(url: &str, key: &str) -> Option<u64> {
    url.split_once('?')?.1.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then(|| value.parse().ok()).flatten()
    })
}

fn extract_runs(
    value: &Value,
    output: &mut Vec<WorkflowRunInput>,
    source_name: &str,
) -> Result<(), WorkflowInputError> {
    match value {
        Value::Array(values) => {
            for value in values {
                extract_runs(value, output, source_name)?;
            }
        }
        Value::Object(object) => {
            if let Some(values) = object.get("workflow_runs").and_then(Value::as_array) {
                for value in values {
                    if let Some(run) = parse_run(value, source_name)? {
                        output.push(run);
                    }
                }
            } else if let Some(value) = object.get("workflow_run") {
                if let Some(run) = parse_run(value, source_name)? {
                    output.push(run);
                }
            } else if looks_like_run(object)
                && let Some(run) = parse_run(value, source_name)?
            {
                output.push(run);
            }
        }
        _ => {}
    }
    Ok(())
}

fn extract_job_pages(
    value: &Value,
    output: &mut Vec<JobPage>,
    source_name: &str,
) -> Result<(), WorkflowInputError> {
    match value {
        Value::Array(values) => {
            let mut jobs = Vec::new();
            let mut malformed_entries = 0_usize;
            for value in values {
                if let Some(job) = parse_job(value, source_name)? {
                    if job.job_id.is_none() || job.run_id.is_none() {
                        malformed_entries = malformed_entries.saturating_add(1);
                    }
                    jobs.push(job);
                } else if value.as_object().is_some_and(|object| {
                    object.contains_key("jobs") || object.contains_key("workflow_jobs")
                }) {
                    extract_job_pages(value, output, source_name)?;
                } else {
                    malformed_entries = malformed_entries.saturating_add(1);
                }
            }
            if !jobs.is_empty() || malformed_entries != 0 {
                output.push(JobPage {
                    jobs,
                    malformed_entries,
                    ..JobPage::default()
                });
            }
        }
        Value::Object(object) => {
            if let Some(values) = object.get("jobs").and_then(Value::as_array) {
                let mut jobs = Vec::new();
                let mut malformed_entries = 0_usize;
                let hint = number(object.get("run_id"));
                for value in values {
                    if let Some(mut job) = parse_job(value, source_name)? {
                        if job.run_id.is_none() {
                            job.run_id = hint;
                        }
                        if job.job_id.is_none() || job.run_id.is_none() {
                            malformed_entries = malformed_entries.saturating_add(1);
                        }
                        jobs.push(job);
                    } else {
                        malformed_entries = malformed_entries.saturating_add(1);
                    }
                }
                output.push(JobPage {
                    jobs,
                    total_count: number(object.get("total_count")).and_then(as_usize),
                    page: number(object.get("page")).and_then(as_usize),
                    per_page: number(object.get("per_page")).and_then(as_usize),
                    run_id_hint: hint,
                    malformed_entries,
                });
            } else if let Some(value) = object.get("workflow_jobs") {
                extract_job_pages(value, output, source_name)?;
            } else if let Some(job) = parse_job(value, source_name)? {
                let malformed_entries = usize::from(job.job_id.is_none() || job.run_id.is_none());
                output.push(JobPage {
                    jobs: vec![job],
                    malformed_entries,
                    ..JobPage::default()
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn looks_like_run(object: &Map<String, Value>) -> bool {
    object.contains_key("workflow_id")
        || object.contains_key("run_number")
        || (object.contains_key("created_at") && !object.contains_key("run_id"))
        || (object.contains_key("head_sha") && object.contains_key("event"))
}

fn parse_run(
    value: &Value,
    source_name: &str,
) -> Result<Option<WorkflowRunInput>, WorkflowInputError> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if !looks_like_run(object) {
        return Ok(None);
    }

    let pull = object
        .get("pull_requests")
        .and_then(Value::as_array)
        .and_then(|pulls| pulls.first())
        .and_then(Value::as_object);
    let source_sha_from_pull = pull
        .and_then(|pull| pull.get("head"))
        .and_then(Value::as_object)
        .and_then(|head| string(head.get("sha")));
    let pull_number = object
        .get("pull_requests")
        .and_then(Value::as_array)
        .and_then(|pulls| pulls.first())
        .and_then(Value::as_object)
        .and_then(|pull| number(pull.get("number")));
    let ref_name = string(object.get("ref"));
    let head_sha = string(object.get("head_sha"));
    let event = string(object.get("event"));
    let (referenced_workflows, referenced_workflow_evidence_issues) =
        parse_referenced_workflows(object, source_name)?;
    let merge_ref = direct_merge_ref(ref_name.as_deref(), pull_number);
    let source_ref = direct_pull_head_ref(ref_name.as_deref(), pull_number);
    let valid_head_sha = valid_sha(head_sha.as_deref());
    let (source_sha, source_sha_basis) = match event.as_deref() {
        // A pull-request run normally executes a synthetic merge ref. Only an
        // explicit refs/pull/N/head ref proves that run.head_sha is the PR
        // source. The embedded PR object is a live projection and cannot
        // establish historical identity.
        Some("pull_request") if source_ref.is_some() => {
            let basis = if valid_head_sha.is_some() {
                "run.ref_pull_head_and_run.head_sha"
            } else {
                "unknown.invalid_run_head_sha"
            };
            (valid_head_sha.clone(), Some(basis.to_owned()))
        }
        Some("pull_request") => (
            None,
            Some("unknown.pull_request_source_not_proven".to_owned()),
        ),
        // pull_request_target executes the base repository context; its run
        // SHA is not the untrusted PR source SHA.
        Some("pull_request_target") => (
            None,
            Some("unknown.pull_request_target_source_not_proven".to_owned()),
        ),
        Some(event) if matches!(event, "push" | "workflow_dispatch" | "schedule") => {
            let basis = if valid_head_sha.is_some() {
                format!("run.head_sha.event={event}")
            } else {
                "unknown.invalid_run_head_sha".to_owned()
            };
            (valid_head_sha.clone(), Some(basis))
        }
        Some(event) => (None, Some(format!("unknown.event_semantics:{event}"))),
        None => (None, Some("unknown.event_missing".to_owned())),
    };
    // A PR merge ref identifies the ref requested by the run, but the REST
    // run head is not checkout evidence. Keep the merge SHA unknown until a
    // caller supplies immutable checkout/event evidence.
    let merge_sha = None;
    let merge_sha_basis = None;
    Ok(Some(WorkflowRunInput {
        run_id: number(object.get("id").or_else(|| object.get("run_id"))),
        run_number: number(object.get("run_number")),
        run_attempt: number(object.get("run_attempt")),
        name: string(object.get("name")),
        event,
        status: string(object.get("status")),
        conclusion: string(object.get("conclusion")),
        head_branch: string(object.get("head_branch")),
        ref_name,
        head_sha: head_sha.clone(),
        source_sha,
        source_sha_basis,
        observed_pull_request_head_sha: source_sha_from_pull,
        merge_sha,
        merge_ref,
        merge_sha_basis,
        referenced_workflows,
        referenced_workflow_evidence_issues,
        workflow_id: number(object.get("workflow_id")),
        workflow_path: string(object.get("path")),
        created_at: string(object.get("created_at")),
        run_started_at: string(object.get("run_started_at")),
        updated_at: string(object.get("updated_at")),
        api_completed_at: string(object.get("completed_at")),
    }))
}

fn parse_referenced_workflows(
    object: &Map<String, Value>,
    source_name: &str,
) -> Result<
    (
        Vec<ReferencedWorkflow>,
        Vec<ReferencedWorkflowEvidenceIssue>,
    ),
    WorkflowInputError,
> {
    let Some(values) = object.get("referenced_workflows") else {
        return Ok((Vec::new(), Vec::new()));
    };
    let Some(values) = values.as_array() else {
        return Err(WorkflowInputError::InvalidField {
            source_name: source_name.to_owned(),
            message: "referenced_workflows must be an array".to_owned(),
        });
    };
    let mut workflows = Vec::new();
    let mut issues = Vec::new();
    for (member_index, value) in values.iter().enumerate() {
        let Some(workflow) = value.as_object() else {
            issues.push(ReferencedWorkflowEvidenceIssue {
                member_index,
                message: "member must be an object".to_owned(),
                raw_json: value.to_string(),
            });
            continue;
        };

        let path = referenced_workflow_string(workflow, "path", member_index, &mut issues);
        let sha = referenced_workflow_string(workflow, "sha", member_index, &mut issues);
        let ref_name = referenced_workflow_string(workflow, "ref", member_index, &mut issues);
        if path.is_none() && sha.is_none() && ref_name.is_none() {
            issues.push(ReferencedWorkflowEvidenceIssue {
                member_index,
                message: "member has no recognized workflow fields".to_owned(),
                raw_json: value.to_string(),
            });
        }
        workflows.push(ReferencedWorkflow {
            path,
            sha,
            ref_name,
        });
    }
    Ok((workflows, issues))
}

fn referenced_workflow_string(
    object: &Map<String, Value>,
    field: &str,
    member_index: usize,
    issues: &mut Vec<ReferencedWorkflowEvidenceIssue>,
) -> Option<String> {
    let value = object.get(field)?;
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Null => None,
        Value::String(_) => {
            issues.push(ReferencedWorkflowEvidenceIssue {
                member_index,
                message: format!("field {field:?} must be non-empty"),
                raw_json: value.to_string(),
            });
            None
        }
        _ => {
            issues.push(ReferencedWorkflowEvidenceIssue {
                member_index,
                message: format!("field {field:?} must be a string"),
                raw_json: value.to_string(),
            });
            None
        }
    }
}

fn parse_job(
    value: &Value,
    _source_name: &str,
) -> Result<Option<WorkflowJobInput>, WorkflowInputError> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if !object.contains_key("run_id")
        && !object.contains_key("started_at")
        && !object.contains_key("completed_at")
    {
        return Ok(None);
    }
    let steps = object
        .get("steps")
        .and_then(Value::as_array)
        .map(|values| values.iter().map(parse_step).collect())
        .unwrap_or_default();
    Ok(Some(WorkflowJobInput {
        job_id: number(object.get("id").or_else(|| object.get("job_id"))),
        run_id: number(object.get("run_id")),
        run_attempt: number(object.get("run_attempt")),
        name: string(object.get("name")),
        status: string(object.get("status")),
        conclusion: string(object.get("conclusion")),
        started_at: string(object.get("started_at")),
        completed_at: string(object.get("completed_at")),
        runner_name: string(object.get("runner_name")),
        runner_group_name: string(object.get("runner_group_name")),
        runner_id: number(object.get("runner_id")),
        steps,
    }))
}

fn parse_step(value: &Value) -> WorkflowStepRecord {
    let Some(object) = value.as_object() else {
        return WorkflowStepRecord {
            duration_state: "unknown_shape".to_owned(),
            ..WorkflowStepRecord::default()
        };
    };
    let started_at = string(object.get("started_at"));
    let completed_at = string(object.get("completed_at"));
    let (duration_ms, duration_state) =
        timestamp_duration(started_at.as_deref(), completed_at.as_deref());
    WorkflowStepRecord {
        number: number(object.get("number")),
        name: string(object.get("name")),
        status: string(object.get("status")),
        conclusion: string(object.get("conclusion")),
        started_at,
        completed_at,
        duration_ms,
        duration_state,
    }
}

fn direct_merge_ref(ref_name: Option<&str>, pull_number: Option<u64>) -> Option<String> {
    direct_pull_ref(ref_name, pull_number, "merge")
}

fn direct_pull_head_ref(ref_name: Option<&str>, pull_number: Option<u64>) -> Option<String> {
    direct_pull_ref(ref_name, pull_number, "head")
}

fn direct_pull_ref(
    ref_name: Option<&str>,
    pull_number: Option<u64>,
    expected_tail: &str,
) -> Option<String> {
    let reference = ref_name?;
    let suffix = reference.strip_prefix("refs/pull/")?;
    let (number, tail) = suffix.split_once('/')?;
    if tail != expected_tail {
        return None;
    }
    let number = number.parse::<u64>().ok()?;
    if pull_number.is_some_and(|expected| expected != number) {
        return None;
    }
    Some(reference.to_owned())
}

fn valid_sha(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(ToOwned::to_owned)
}

fn number(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn as_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.is_empty())
}

fn timestamp(value: Option<&str>) -> Option<OffsetDateTime> {
    value.and_then(|value| OffsetDateTime::parse(value.trim(), &Rfc3339).ok())
}

fn timestamp_duration(start: Option<&str>, end: Option<&str>) -> (Option<u64>, String) {
    let Some(start) = timestamp(start) else {
        return (None, "unknown_start".to_owned());
    };
    let Some(end) = timestamp(end) else {
        return (None, "unknown_completion".to_owned());
    };
    let delta = end - start;
    let nanos = delta.whole_nanoseconds();
    if nanos < 0 {
        return (None, "invalid_order".to_owned());
    }
    let millis = nanos / 1_000_000;
    match u64::try_from(millis) {
        Ok(millis) => (Some(millis), "observed".to_owned()),
        Err(_) => (None, "overflow".to_owned()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobFreshness {
    Fresh,
    StaleBeforeAttempt,
    UnknownAttemptStart,
    UnknownJobStart,
    InvalidOrder,
    NotApplicableSkipped,
}

impl JobFreshness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::StaleBeforeAttempt => "stale_before_attempt",
            Self::UnknownAttemptStart => "unknown_attempt_start",
            Self::UnknownJobStart => "unknown_job_start",
            Self::InvalidOrder => "invalid_order",
            Self::NotApplicableSkipped => "not_applicable_skipped",
        }
    }
}

fn classify_job_freshness(run: &WorkflowRunInput, job: &WorkflowJobInput) -> JobFreshness {
    if is_skipped(job) {
        return JobFreshness::NotApplicableSkipped;
    }
    let Some(attempt_started) = timestamp(run.run_started_at.as_deref()) else {
        return JobFreshness::UnknownAttemptStart;
    };
    let Some(job_started) = timestamp(job.started_at.as_deref()) else {
        return JobFreshness::UnknownJobStart;
    };
    if timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref()).1
        == "invalid_order"
    {
        return JobFreshness::InvalidOrder;
    }
    if job_started < attempt_started {
        JobFreshness::StaleBeforeAttempt
    } else {
        JobFreshness::Fresh
    }
}

fn attempt_freshness_state(
    fresh_executed_jobs: usize,
    stale_executed_jobs: usize,
    freshness_unknown_jobs: usize,
) -> &'static str {
    if fresh_executed_jobs == 0 && stale_executed_jobs == 0 && freshness_unknown_jobs == 0 {
        "not_applicable_all_skipped"
    } else if freshness_unknown_jobs != 0 {
        "unknown_job_freshness"
    } else if stale_executed_jobs != 0 && fresh_executed_jobs != 0 {
        "mixed_stale_records"
    } else if stale_executed_jobs != 0 {
        "stale_records"
    } else {
        "complete_fresh"
    }
}

fn add_duration(sum: &mut u64, unknown: &mut usize, duration: Option<u64>) {
    match duration {
        Some(duration) => match sum.checked_add(duration) {
            Some(next) => *sum = next,
            None => *unknown = unknown.saturating_add(1),
        },
        None => *unknown = unknown.saturating_add(1),
    }
}

fn build_records(
    runs: Vec<WorkflowRunInput>,
    pages: Vec<JobPage>,
    options: &WorkflowCollectOptions,
) -> Vec<WorkflowJobRecord> {
    let (groups, duplicate_conflicts) = group_jobs(&pages);
    let page_stats = collect_page_stats(&pages);
    let mut used_groups = BTreeSet::new();
    let mut records = Vec::new();

    for run in runs {
        let key = (run.run_id, run.run_attempt);
        let jobs = groups.get(&key).cloned().unwrap_or_default();
        let matching_group = groups.contains_key(&key);
        let run_identity_known = run.run_id.is_some() && run.run_attempt.is_some();
        let attempt_match = run_identity_known
            && jobs
                .iter()
                .all(|job| job.run_id == run.run_id && job.run_attempt == run.run_attempt);
        if matching_group {
            used_groups.insert(key);
        }
        let conflict = duplicate_conflicts.contains(&key)
            || page_stats.get(&key).is_some_and(|stats| stats.conflict);
        records.extend(records_for_run(
            &run,
            jobs,
            page_stats.get(&key),
            options,
            attempt_match,
            conflict,
        ));
    }

    for (key, jobs) in groups {
        if used_groups.contains(&key) {
            continue;
        }
        let run = WorkflowRunInput {
            run_id: key.0,
            run_attempt: key.1,
            ..WorkflowRunInput::default()
        };
        let conflict = duplicate_conflicts.contains(&key)
            || page_stats.get(&key).is_some_and(|stats| stats.conflict);
        records.extend(records_for_run(
            &run,
            jobs,
            page_stats.get(&key),
            options,
            false,
            conflict,
        ));
    }

    records
}

fn group_jobs(pages: &[JobPage]) -> (WorkflowJobGroups, WorkflowJobConflicts) {
    let mut groups: BTreeMap<_, Vec<WorkflowJobInput>> = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for page in pages {
        for job in &page.jobs {
            let key = (job.run_id.or(page.run_id_hint), job.run_attempt);
            let group = groups.entry(key).or_default();
            if let Some(job_id) = job.job_id
                && group.iter().any(|existing| existing.job_id == Some(job_id))
            {
                conflicts.insert(key);
            }
            group.push(job.clone());
        }
    }
    (groups, conflicts)
}

fn deduplicate_jobs(jobs: &[WorkflowJobInput]) -> (Vec<&WorkflowJobInput>, BTreeSet<u64>) {
    let mut first_by_id = BTreeMap::new();
    let mut conflicting_ids = BTreeSet::new();
    for job in jobs {
        let Some(job_id) = job.job_id else {
            continue;
        };
        if let Some(first) = first_by_id.get(&job_id) {
            if *first != job {
                conflicting_ids.insert(job_id);
            }
        } else {
            first_by_id.insert(job_id, job);
        }
    }

    let mut seen_ids = BTreeSet::new();
    let mut aggregate = Vec::new();
    for job in jobs {
        if let Some(job_id) = job.job_id
            && (conflicting_ids.contains(&job_id) || !seen_ids.insert(job_id))
        {
            continue;
        }
        aggregate.push(job);
    }
    (aggregate, conflicting_ids)
}

fn collect_page_stats(pages: &[JobPage]) -> BTreeMap<(Option<u64>, Option<u64>), PageStats> {
    let mut stats = BTreeMap::new();
    for page in pages {
        let run_ids: BTreeSet<_> = page
            .jobs
            .iter()
            .filter_map(|job| job.run_id.or(page.run_id_hint))
            .collect();
        let attempts: BTreeSet<_> = page.jobs.iter().map(|job| job.run_attempt).collect();
        let page_keys: Vec<_> = if run_ids.len() <= 1 && attempts.len() <= 1 {
            vec![(
                run_ids.iter().next().copied().or(page.run_id_hint),
                attempts.iter().next().copied().flatten(),
            )]
        } else {
            run_ids
                .iter()
                .flat_map(|run_id| {
                    attempts
                        .iter()
                        .map(move |attempt| (Some(*run_id), *attempt))
                })
                .collect()
        };
        for key in page_keys {
            let entry = stats.entry(key).or_insert_with(PageStats::default);
            entry.page_count = entry.page_count.saturating_add(1);
            entry.malformed_entries = entry
                .malformed_entries
                .saturating_add(page.malformed_entries);
            if let Some(total_count) = page.total_count {
                match entry.expected_count {
                    None => entry.expected_count = Some(total_count),
                    Some(existing) if existing != total_count => entry.conflict = true,
                    Some(_) => {}
                }
            }
            if run_ids.len() > 1 || attempts.len() > 1 {
                entry.conflict = true;
            }
            if let Some(run_id_hint) = page.run_id_hint
                && page
                    .jobs
                    .iter()
                    .any(|job| job.run_id.is_some_and(|run_id| run_id != run_id_hint))
            {
                entry.conflict = true;
            }
            if let Some(page) = page.page {
                if !entry.pages.insert(page) {
                    entry.conflict = true;
                }
            } else {
                entry.page_metadata_missing = true;
            }
            if let Some(per_page) = page.per_page {
                if per_page == 0 {
                    entry.conflict = true;
                } else if let Some(existing) = entry.per_page {
                    if existing != per_page {
                        entry.conflict = true;
                    }
                } else {
                    entry.per_page = Some(per_page);
                }
            }
        }
    }
    for entry in stats.values_mut() {
        let Some(expected) = entry.expected_count else {
            continue;
        };
        if entry.page_count > 1 && entry.page_metadata_missing {
            entry.conflict = true;
        }
        if let Some(first) = entry.pages.first().copied()
            && (first != 1
                || entry
                    .pages
                    .iter()
                    .enumerate()
                    .any(|(index, page)| *page != index.saturating_add(1)))
        {
            entry.conflict = true;
        }
        if let Some(per_page) = entry.per_page {
            let expected_pages = expected.div_ceil(per_page);
            if !entry.pages.is_empty() && entry.pages.len() != expected_pages {
                entry.conflict = true;
            }
        }
    }
    stats
}

fn records_for_run(
    run: &WorkflowRunInput,
    jobs: Vec<WorkflowJobInput>,
    page_stats: Option<&PageStats>,
    options: &WorkflowCollectOptions,
    attempt_match: bool,
    duplicate_or_page_conflict: bool,
) -> Vec<WorkflowJobRecord> {
    let expected = page_stats.and_then(|stats| stats.expected_count);
    let observed = jobs.len();
    let malformed_job_entries = page_stats.map(|stats| stats.malformed_entries).unwrap_or(0);
    let all_job_ids_known = jobs.iter().all(|job| job.job_id.is_some());
    let run_identity_known = run.run_id.is_some() && run.run_attempt.is_some();
    let jobs_complete = expected.map(|expected| {
        expected == observed.saturating_add(malformed_job_entries)
            && all_job_ids_known
            && run_identity_known
            && attempt_match
            && malformed_job_entries == 0
            && !duplicate_or_page_conflict
    });
    let run_terminal = run
        .status
        .as_deref()
        .map(|status| status.eq_ignore_ascii_case("completed"));
    let (aggregate_jobs, conflicting_job_ids) = deduplicate_jobs(&jobs);
    let skipped_jobs = aggregate_jobs.iter().filter(|job| is_skipped(job)).count();
    let executed_jobs: Vec<&WorkflowJobInput> = aggregate_jobs
        .iter()
        .copied()
        .filter(|job| !is_skipped(job))
        .collect();
    let conflicting_executed_jobs = conflicting_job_ids
        .iter()
        .filter(|job_id| {
            jobs.iter()
                .any(|job| job.job_id == Some(**job_id) && !is_skipped(job))
        })
        .count();
    let mut execution_partial_sum_ms = 0_u64;
    let mut fresh_execution_partial_sum_ms = 0_u64;
    let mut stale_execution_partial_sum_ms = 0_u64;
    let mut execution_unknown_jobs = conflicting_executed_jobs;
    let mut freshness_unknown_jobs = conflicting_executed_jobs;
    let mut fresh_executed_jobs = 0_usize;
    let mut stale_executed_jobs = 0_usize;
    let mut fresh_execution_unknown_jobs = conflicting_executed_jobs;
    let mut stale_execution_unknown_jobs = conflicting_executed_jobs;
    let mut max_job_duration_ms = None;
    let mut max_job_name = None;
    let mut fresh_completion_times = Vec::new();
    for job in &executed_jobs {
        let freshness = classify_job_freshness(run, job);
        let (duration, _) =
            timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref());
        add_duration(
            &mut execution_partial_sum_ms,
            &mut execution_unknown_jobs,
            duration,
        );
        match freshness {
            JobFreshness::Fresh => {
                fresh_executed_jobs = fresh_executed_jobs.saturating_add(1);
                add_duration(
                    &mut fresh_execution_partial_sum_ms,
                    &mut fresh_execution_unknown_jobs,
                    duration,
                );
                if let Some(completion) = timestamp(job.completed_at.as_deref()) {
                    fresh_completion_times.push((completion, job.completed_at.clone()));
                }
            }
            JobFreshness::StaleBeforeAttempt => {
                stale_executed_jobs = stale_executed_jobs.saturating_add(1);
                if let Some(duration) = duration {
                    match stale_execution_partial_sum_ms.checked_add(duration) {
                        Some(next) => stale_execution_partial_sum_ms = next,
                        None => {
                            execution_unknown_jobs = execution_unknown_jobs.saturating_add(1);
                            stale_execution_unknown_jobs =
                                stale_execution_unknown_jobs.saturating_add(1);
                        }
                    }
                } else {
                    stale_execution_unknown_jobs = stale_execution_unknown_jobs.saturating_add(1);
                }
            }
            JobFreshness::UnknownAttemptStart
            | JobFreshness::UnknownJobStart
            | JobFreshness::InvalidOrder => {
                freshness_unknown_jobs = freshness_unknown_jobs.saturating_add(1);
            }
            JobFreshness::NotApplicableSkipped => {}
        }
        if let Some(duration) = duration
            && max_job_duration_ms.is_none_or(|current| duration > current)
        {
            max_job_duration_ms = Some(duration);
            max_job_name = job.name.clone();
        }
    }
    let execution_unobserved_jobs = expected.map(|expected| expected.saturating_sub(observed));

    let valid_execution_durations = executed_jobs
        .iter()
        .filter(|job| {
            timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref())
                .0
                .is_some()
        })
        .count();
    let executed_terminal = executed_jobs.iter().all(|job| job_is_terminal(job));
    let executed_timing_complete = Some(
        !executed_jobs.is_empty()
            && executed_terminal
            && conflicting_executed_jobs == 0
            && valid_execution_durations == executed_jobs.len(),
    );
    let freshness_state = attempt_freshness_state(
        fresh_executed_jobs,
        stale_executed_jobs,
        freshness_unknown_jobs,
    );
    let fresh_jobs: Vec<_> = executed_jobs
        .iter()
        .copied()
        .filter(|job| classify_job_freshness(run, job) == JobFreshness::Fresh)
        .collect();
    let fresh_timing_complete = !fresh_jobs.is_empty()
        && fresh_jobs.iter().all(|job| job_is_terminal(job))
        && fresh_execution_unknown_jobs == 0
        && fresh_jobs.iter().all(|job| {
            timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref())
                .0
                .is_some()
        });
    let execution_sum_ms = (jobs_complete == Some(true)
        && executed_timing_complete == Some(true)
        && execution_unknown_jobs == 0
        && freshness_state == "complete_fresh")
        .then_some(execution_partial_sum_ms);

    let valid_completion_times: Vec<_> = executed_jobs
        .iter()
        .filter_map(|job| {
            timestamp(job.completed_at.as_deref()).map(|time| (time, job.completed_at.clone()))
        })
        .collect();
    let executed_order_valid = executed_jobs.iter().all(|job| {
        timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref()).1
            != "invalid_order"
    });
    let executed_completion_complete = executed_terminal
        && conflicting_executed_jobs == 0
        && executed_order_valid
        && valid_completion_times.len() == executed_jobs.len();
    let run_completion = if run_terminal == Some(true)
        && jobs_complete == Some(true)
        && !executed_jobs.is_empty()
        && executed_completion_complete
    {
        max_timestamp(&valid_completion_times)
    } else {
        None
    };
    let fresh_completion = if run_terminal == Some(true) && fresh_timing_complete {
        max_timestamp(&fresh_completion_times)
    } else {
        None
    };
    let completion_state = if run_terminal != Some(true) {
        match run_terminal {
            None => "unknown_run_status",
            Some(false) => "unknown_run_nonterminal",
            Some(true) => "unknown_completion",
        }
    } else if jobs_complete != Some(true) {
        "unknown_job_count_or_pages"
    } else if executed_jobs.is_empty() {
        "unknown_no_executed_jobs"
    } else if !executed_completion_complete {
        "unknown_job_timestamps"
    } else if freshness_state != "complete_fresh" {
        match freshness_state {
            "mixed_stale_records" => "unknown_mixed_job_freshness",
            "stale_records" => "unknown_stale_job_freshness",
            _ => "unknown_job_freshness",
        }
    } else if run_completion.is_some() {
        "verified"
    } else {
        "unknown_completion"
    }
    .to_owned();

    let (run_lifetime_ms, run_lifetime_state) = match run.run_attempt {
        Some(1) => {
            let lifetime = run_completion.as_ref().and_then(|(_, completion)| {
                timestamp_duration(run.created_at.as_deref(), Some(completion.as_str())).0
            });
            let state = if lifetime.is_some() {
                "observed_initial_attempt"
            } else {
                "unknown_initial_attempt_timing"
            };
            (lifetime, state.to_owned())
        }
        Some(_) => (None, "unknown_rerun_created_at_is_original".to_owned()),
        None => (None, "unknown_attempt".to_owned()),
    };
    let attempt_wall_ms = if jobs_complete == Some(true)
        && freshness_state == "complete_fresh"
        && executed_timing_complete == Some(true)
    {
        run_completion.as_ref().and_then(|(_, completion)| {
            timestamp_duration(run.run_started_at.as_deref(), Some(completion.as_str())).0
        })
    } else {
        None
    };
    let fresh_attempt_wall_ms = fresh_completion.as_ref().and_then(|(_, completion)| {
        timestamp_duration(run.run_started_at.as_deref(), Some(completion.as_str())).0
    });
    let (pre_start_ms, pre_start_state) = pre_start(run);
    let (required_gate_completed_at, required_gate_state) = required_gate(
        run,
        &jobs,
        options,
        attempt_match,
        duplicate_or_page_conflict,
    );

    let base = BaseRecord {
        run,
        expected,
        observed,
        jobs_complete,
        malformed_job_entries,
        attempt_match,
        executed_timing_complete,
        attempt_freshness_state: freshness_state.to_owned(),
        completion_state,
        run_completion: run_completion.map(|(_, raw)| raw),
        run_lifetime_ms,
        run_lifetime_state,
        attempt_wall_ms,
        fresh_attempt_wall_ms,
        execution_sum_ms,
        execution_partial_sum_ms,
        fresh_execution_partial_sum_ms: (fresh_executed_jobs != 0)
            .then_some(fresh_execution_partial_sum_ms),
        stale_execution_partial_sum_ms: (stale_executed_jobs != 0)
            .then_some(stale_execution_partial_sum_ms),
        fresh_execution_unknown_jobs,
        stale_execution_unknown_jobs,
        fresh_executed_jobs,
        stale_executed_jobs,
        freshness_unknown_jobs,
        execution_unknown_jobs,
        execution_unobserved_jobs,
        skipped_jobs,
        pre_start_ms,
        pre_start_state,
        max_job_duration_ms,
        max_job_name,
        required_gate_completed_at,
        required_gate_state,
    };

    if jobs.is_empty() {
        return vec![base.record(None)];
    }
    jobs.iter().map(|job| base.record(Some(job))).collect()
}

struct BaseRecord<'a> {
    run: &'a WorkflowRunInput,
    expected: Option<usize>,
    observed: usize,
    jobs_complete: Option<bool>,
    malformed_job_entries: usize,
    attempt_match: bool,
    executed_timing_complete: Option<bool>,
    attempt_freshness_state: String,
    completion_state: String,
    run_completion: Option<String>,
    run_lifetime_ms: Option<u64>,
    run_lifetime_state: String,
    attempt_wall_ms: Option<u64>,
    fresh_attempt_wall_ms: Option<u64>,
    execution_sum_ms: Option<u64>,
    execution_partial_sum_ms: u64,
    fresh_execution_partial_sum_ms: Option<u64>,
    stale_execution_partial_sum_ms: Option<u64>,
    fresh_execution_unknown_jobs: usize,
    stale_execution_unknown_jobs: usize,
    fresh_executed_jobs: usize,
    stale_executed_jobs: usize,
    freshness_unknown_jobs: usize,
    execution_unknown_jobs: usize,
    execution_unobserved_jobs: Option<usize>,
    skipped_jobs: usize,
    pre_start_ms: Option<u64>,
    pre_start_state: String,
    max_job_duration_ms: Option<u64>,
    max_job_name: Option<String>,
    required_gate_completed_at: Option<String>,
    required_gate_state: String,
}

impl BaseRecord<'_> {
    fn record(&self, job: Option<&WorkflowJobInput>) -> WorkflowJobRecord {
        let (job_duration_ms, job_duration_state) = match job {
            Some(job) if is_skipped(job) => (
                timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref()).0,
                "skipped".to_owned(),
            ),
            Some(job) => timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref()),
            None => (None, "no_job".to_owned()),
        };
        WorkflowJobRecord {
            record_type: if job.is_some() {
                "job".to_owned()
            } else {
                "run_without_jobs".to_owned()
            },
            run_id: self.run.run_id,
            run_number: self.run.run_number,
            run_attempt: self.run.run_attempt,
            event: self.run.event.clone(),
            workflow_name: self.run.name.clone(),
            workflow_id: self.run.workflow_id,
            workflow_path: self.run.workflow_path.clone(),
            run_status: self.run.status.clone(),
            run_conclusion: self.run.conclusion.clone(),
            run_terminal: self
                .run
                .status
                .as_deref()
                .map(|status| status.eq_ignore_ascii_case("completed")),
            head_branch: self.run.head_branch.clone(),
            ref_name: self.run.ref_name.clone(),
            head_sha: self.run.head_sha.clone(),
            source_sha: self.run.source_sha.clone(),
            source_sha_basis: self.run.source_sha_basis.clone(),
            observed_pull_request_head_sha: self.run.observed_pull_request_head_sha.clone(),
            merge_sha: self.run.merge_sha.clone(),
            merge_ref: self.run.merge_ref.clone(),
            merge_sha_basis: self.run.merge_sha_basis.clone(),
            referenced_workflows: self.run.referenced_workflows.clone(),
            referenced_workflow_evidence_issues: self
                .run
                .referenced_workflow_evidence_issues
                .clone(),
            run_created_at: self.run.created_at.clone(),
            run_started_at: self.run.run_started_at.clone(),
            run_updated_at: self.run.updated_at.clone(),
            run_api_completed_at: self.run.api_completed_at.clone(),
            jobs_expected: self.expected,
            jobs_observed: self.observed,
            jobs_complete: self.jobs_complete,
            malformed_job_entries: self.malformed_job_entries,
            attempt_match: self.attempt_match,
            executed_timing_complete: self.executed_timing_complete,
            attempt_freshness_state: self.attempt_freshness_state.clone(),
            completion_state: self.completion_state.clone(),
            all_jobs_completed_at: self.run_completion.clone(),
            run_lifetime_ms: self.run_lifetime_ms,
            run_lifetime_state: self.run_lifetime_state.clone(),
            attempt_wall_ms: self.attempt_wall_ms,
            fresh_attempt_wall_ms: self.fresh_attempt_wall_ms,
            execution_sum_ms: self.execution_sum_ms,
            execution_partial_sum_ms: self.execution_partial_sum_ms,
            fresh_execution_partial_sum_ms: self.fresh_execution_partial_sum_ms,
            stale_execution_partial_sum_ms: self.stale_execution_partial_sum_ms,
            fresh_execution_unknown_jobs: self.fresh_execution_unknown_jobs,
            stale_execution_unknown_jobs: self.stale_execution_unknown_jobs,
            fresh_executed_jobs: self.fresh_executed_jobs,
            stale_executed_jobs: self.stale_executed_jobs,
            freshness_unknown_jobs: self.freshness_unknown_jobs,
            execution_unknown_jobs: self.execution_unknown_jobs,
            execution_unobserved_jobs: self.execution_unobserved_jobs,
            skipped_jobs: self.skipped_jobs,
            pre_start_ms: self.pre_start_ms,
            pre_start_state: self.pre_start_state.clone(),
            max_job_duration_ms: self.max_job_duration_ms,
            max_job_name: self.max_job_name.clone(),
            critical_path_ms: None,
            critical_path_state: "not_computed_without_dependency_graph".to_owned(),
            required_gate_completed_at: self.required_gate_completed_at.clone(),
            required_gate_state: self.required_gate_state.clone(),
            job_id: job.and_then(|job| job.job_id),
            job_attempt: job.and_then(|job| job.run_attempt),
            job_name: job.and_then(|job| job.name.clone()),
            job_status: job.and_then(|job| job.status.clone()),
            job_conclusion: job.and_then(|job| job.conclusion.clone()),
            runner_name: job.and_then(|job| job.runner_name.clone()),
            runner_group_name: job.and_then(|job| job.runner_group_name.clone()),
            runner_id: job.and_then(|job| job.runner_id),
            job_started_at: job.and_then(|job| job.started_at.clone()),
            job_completed_at: job.and_then(|job| job.completed_at.clone()),
            job_duration_ms,
            job_duration_state,
            job_freshness_state: job
                .map(|job| classify_job_freshness(self.run, job).as_str().to_owned())
                .unwrap_or_else(|| "no_job".to_owned()),
            steps: job.map(|job| job.steps.clone()).unwrap_or_default(),
        }
    }
}

fn is_skipped(job: &WorkflowJobInput) -> bool {
    job.conclusion
        .as_deref()
        .or(job.status.as_deref())
        .is_some_and(|value| value.eq_ignore_ascii_case("skipped"))
}

fn job_is_terminal(job: &WorkflowJobInput) -> bool {
    job.status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case("completed"))
}

fn max_timestamp(values: &[(OffsetDateTime, Option<String>)]) -> Option<(OffsetDateTime, String)> {
    values
        .iter()
        .filter_map(|(time, raw)| raw.clone().map(|raw| (*time, raw)))
        .max_by_key(|(time, _)| *time)
}

fn pre_start(run: &WorkflowRunInput) -> (Option<u64>, String) {
    if run.run_attempt.is_some_and(|attempt| attempt > 1) {
        return (None, "unknown_rerun_inter_attempt".to_owned());
    }
    let Some(created) = timestamp(run.created_at.as_deref()) else {
        return (None, "unknown_timestamps".to_owned());
    };
    let Some(started) = timestamp(run.run_started_at.as_deref()) else {
        return (None, "unknown_timestamps".to_owned());
    };
    let nanos = (started - created).whole_nanoseconds();
    if nanos < 0 {
        return (None, "unknown_invalid_order".to_owned());
    }
    match u64::try_from(nanos / 1_000_000) {
        Ok(milliseconds) => (Some(milliseconds), "unclassified".to_owned()),
        Err(_) => (None, "unknown_overflow".to_owned()),
    }
}

fn required_gate(
    run: &WorkflowRunInput,
    jobs: &[WorkflowJobInput],
    options: &WorkflowCollectOptions,
    attempt_match: bool,
    duplicate_or_page_conflict: bool,
) -> (Option<String>, String) {
    if options.required_job_names.is_empty() {
        return (None, "unknown_no_selector".to_owned());
    }
    let requested_names: BTreeSet<&str> = options
        .required_job_names
        .iter()
        .map(String::as_str)
        .collect();
    if !jobs.iter().any(|job| {
        job.name
            .as_deref()
            .is_some_and(|name| requested_names.contains(name))
    }) {
        return (None, "unknown_no_matching_job".to_owned());
    }
    if !attempt_match {
        return (None, "unknown_gate_identity".to_owned());
    }
    if duplicate_or_page_conflict {
        return (None, "unknown_gate_collection_conflict".to_owned());
    }

    let mut completed = Vec::with_capacity(requested_names.len());
    for wanted in requested_names {
        let matching: Vec<_> = jobs
            .iter()
            .filter(|job| job.name.as_deref() == Some(wanted))
            .collect();
        if matching.is_empty() {
            return (None, "unknown_gate_missing_job".to_owned());
        }
        if matching.len() != 1 {
            return (None, "unknown_gate_ambiguous".to_owned());
        }
        let job = matching[0];
        if job.job_id.is_none() || job.run_id.is_none() || job.run_attempt.is_none() {
            return (None, "unknown_gate_identity".to_owned());
        }
        if is_skipped(job) {
            return (None, "unknown_gate_skipped".to_owned());
        }
        if !job_is_terminal(job) {
            return (None, "unknown_gate_nonterminal".to_owned());
        }
        let (duration, duration_state) =
            timestamp_duration(job.started_at.as_deref(), job.completed_at.as_deref());
        if duration_state == "invalid_order" {
            return (None, "unknown_gate_invalid_order".to_owned());
        }
        if duration.is_none() {
            return (None, "unknown_gate_timestamps".to_owned());
        }
        let Some(completion) = timestamp(job.completed_at.as_deref()) else {
            return (None, "unknown_gate_timestamps".to_owned());
        };
        match classify_job_freshness(run, job) {
            JobFreshness::Fresh => {}
            JobFreshness::StaleBeforeAttempt => {
                return (None, "unknown_gate_stale_job".to_owned());
            }
            JobFreshness::UnknownAttemptStart
            | JobFreshness::UnknownJobStart
            | JobFreshness::InvalidOrder => {
                return (None, "unknown_gate_freshness".to_owned());
            }
            JobFreshness::NotApplicableSkipped => {
                return (None, "unknown_gate_skipped".to_owned());
            }
        }
        completed.push((completion, job.completed_at.clone()));
    }
    max_timestamp(&completed)
        .map(|(_, raw)| (Some(raw), "observed".to_owned()))
        .unwrap_or((None, "unknown_gate_completion".to_owned()))
}

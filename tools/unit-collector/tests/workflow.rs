#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "fixture assertions intentionally fail loudly"
)]

use unit_collector::{collect_workflow, WorkflowCollectOptions};

fn run_json(id: u64, attempt: u64, status: &str, ref_name: &str) -> String {
    format!(
        r#"{{
          "id": {id},
          "run_number": {id},
          "run_attempt": {attempt},
          "name": "CI",
          "event": "pull_request",
          "status": "{status}",
          "conclusion": "success",
          "head_branch": "feature",
          "ref": "{ref_name}",
          "head_sha": "merge-{id}",
          "pull_requests": [{{"number": 7, "head": {{"sha": "source-{id}"}}, "base": {{"repo": {{"url": "https://api.github.com/repos/tailrocks/parallax"}}}}}}],
          "referenced_workflows": [{{"path": "tailrocks/parallax/.github/workflows/reusable.yml", "sha": "workflow-{id}", "ref": "refs/pull/7/merge"}}],
          "workflow_id": 9,
          "path": ".github/workflows/ci.yml",
          "created_at": "2026-09-20T00:00:00Z",
          "run_started_at": "2026-09-20T00:00:05Z",
          "updated_at": "2099-01-01T00:00:00Z"
        }}"#
    )
}

fn job(
    id: u64,
    run_id: u64,
    attempt: u64,
    name: &str,
    conclusion: &str,
    start: &str,
    end: &str,
) -> String {
    format!(
        r#"{{
          "id": {id}, "run_id": {run_id}, "run_attempt": {attempt},
          "name": "{name}", "status": "completed", "conclusion": "{conclusion}",
          "started_at": "{start}", "completed_at": "{end}",
          "runner_name": "ubuntu-24.04", "runner_group_name": "GitHub Actions", "runner_id": 4,
          "steps": [{{"number": 1, "name": "check", "status": "completed", "conclusion": "success", "started_at": "{start}", "completed_at": "{end}"}}]
        }}"#
    )
}

#[test]
fn parallel_jobs_rank_by_raw_timestamps_and_keep_gate_separate() {
    let run = run_json(10, 1, "completed", "refs/pull/7/merge");
    let jobs = format!(
        r#"{{"total_count": 2, "page": 1, "per_page": 100, "jobs": [{}, {}]}}"#,
        job(
            101,
            10,
            1,
            "required",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:12Z"
        ),
        job(
            102,
            10,
            1,
            "slow",
            "success",
            "2026-09-20T00:00:11Z",
            "2026-09-20T00:00:15Z"
        ),
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions {
            required_job_names: vec!["required".to_owned()],
        },
    )
    .expect("parallel fixture parses");

    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(true)));
    assert!(records
        .iter()
        .all(|record| record.completion_state == "verified"));
    assert_eq!(
        records[0].all_jobs_completed_at.as_deref(),
        Some("2026-09-20T00:00:15Z")
    );
    assert_eq!(records[0].all_jobs_wall_ms, Some(15_000));
    assert_eq!(records[0].execution_sum_ms, Some(6_000));
    assert_eq!(
        records[0].required_gate_completed_at.as_deref(),
        Some("2026-09-20T00:00:12Z")
    );
    assert_eq!(records[0].required_gate_state, "observed");
    assert_eq!(records[0].critical_path_ms, None);
    assert_eq!(records[0].merge_sha.as_deref(), Some("merge-10"));
    assert_eq!(
        records[0].merge_sha_basis.as_deref(),
        Some("run.ref_and_run.head_sha")
    );
    assert_eq!(records[0].pre_start_ms, Some(5_000));
    assert_eq!(records[0].pre_start_state, "unclassified");
}

#[test]
fn skipped_inverted_timestamps_do_not_invalidate_real_completion() {
    let run = run_json(11, 1, "completed", "refs/pull/7/merge");
    let jobs = format!(
        r#"{{"total_count": 3, "jobs": [{}, {}, {}]}}"#,
        job(
            111,
            11,
            1,
            "a",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:12Z"
        ),
        job(
            112,
            11,
            1,
            "b",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:14Z"
        ),
        job(
            113,
            11,
            1,
            "skipped",
            "skipped",
            "2026-09-20T00:00:20Z",
            "2026-09-20T00:00:19Z"
        ),
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("skipped fixture parses");

    assert!(records
        .iter()
        .all(|record| record.completion_state == "verified"));
    assert!(records.iter().all(|record| record.skipped_jobs == 1));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms == Some(6_000)));
    let skipped = records
        .iter()
        .find(|record| record.job_name.as_deref() == Some("skipped"))
        .expect("skipped row");
    assert_eq!(skipped.job_duration_ms, None);
    assert_eq!(skipped.job_duration_state, "skipped");
}

#[test]
fn missing_count_or_timestamp_censors_completion_and_sum() {
    let run = run_json(12, 1, "completed", "refs/pull/7/merge");
    let jobs = format!(
        r#"{{"total_count": 2, "jobs": [{}]}}"#,
        job(
            121,
            12,
            1,
            "only",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:12Z"
        ),
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("incomplete fixture parses");
    assert_eq!(records[0].jobs_complete, Some(false));
    assert_eq!(records[0].completion_state, "unknown_job_count_or_pages");
    assert_eq!(records[0].all_jobs_completed_at, None);
    assert_eq!(records[0].execution_sum_ms, None);
    assert_eq!(records[0].execution_partial_sum_ms, 2_000);
    assert_eq!(records[0].execution_unobserved_jobs, Some(1));

    let missing_timestamp = format!(
        r#"{{"total_count": 1, "jobs": [{}]}}"#,
        job(122, 12, 1, "missing", "success", "2026-09-20T00:00:10Z", ""),
    );
    let records = collect_workflow(
        &[(
            "run.json".to_owned(),
            run_json(12, 1, "completed", "refs/pull/7/merge"),
        )],
        &[("jobs.json".to_owned(), missing_timestamp)],
        &WorkflowCollectOptions::default(),
    )
    .expect("missing timestamp fixture parses");
    assert_eq!(records[0].jobs_complete, Some(true));
    assert_eq!(records[0].completion_state, "unknown_job_timestamps");
    assert_eq!(records[0].execution_sum_ms, None);
    assert_eq!(records[0].execution_unknown_jobs, 1);
}

#[test]
fn wrapped_responses_and_attempt_mismatch_are_fail_closed() {
    let run = run_json(13, 1, "completed", "refs/pull/7/merge");
    let wrapped_run = format!(
        r#"{{"structuredContent":{{"content":[{{"type":"text","text":{}}}]}}}}"#,
        serde_json::to_string(&run).expect("encode wrapped run")
    );
    let mismatched_job = job(
        131,
        13,
        2,
        "attempt-two",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let wrapped_jobs = format!(
        r#"{{"content":[{{"type":"text","text":{}}}]}}"#,
        serde_json::to_string(&format!(
            r#"{{"total_count": 1, "jobs": [{}]}}"#,
            mismatched_job
        ))
        .expect("encode wrapped jobs")
    );
    let records = collect_workflow(
        &[("wrapped-run.json".to_owned(), wrapped_run)],
        &[("wrapped-jobs.json".to_owned(), wrapped_jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("wrapped fixture parses");

    assert_eq!(records.len(), 2);
    let run_row = records
        .iter()
        .find(|record| record.record_type == "run_without_jobs")
        .expect("run row retained");
    assert_eq!(run_row.run_attempt, Some(1));
    assert!(run_row.attempt_match);
    assert_eq!(run_row.jobs_observed, 0);
    let job_row = records
        .iter()
        .find(|record| record.job_id == Some(131))
        .expect("mismatched job retained");
    assert_eq!(job_row.run_attempt, Some(2));
    assert!(!job_row.attempt_match);
    assert_eq!(job_row.jobs_complete, Some(false));
    assert_eq!(job_row.run_status, None);
}

#[test]
fn missing_run_attempt_cannot_complete_a_counted_job_set() {
    let run = run_json(19, 1, "completed", "refs/pull/7/merge").replace("\"run_attempt\": 1,", "");
    let jobs = job(
        191,
        19,
        1,
        "missing-run-attempt",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    )
    .replace(", \"run_attempt\": 1", "");
    let jobs = format!(r#"{{"total_count": 1, "jobs": [{jobs}]}}"#);
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("missing run attempt fixture parses");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_attempt, None);
    assert!(!records[0].attempt_match);
    assert_eq!(records[0].jobs_complete, Some(false));
    assert_eq!(records[0].execution_sum_ms, None);
}

#[test]
fn nonterminal_or_inverted_executed_jobs_cannot_verify_completion() {
    let run = run_json(20, 1, "completed", "refs/pull/7/merge");
    let in_progress = job(
        201,
        20,
        1,
        "in-progress",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    )
    .replace("\"status\": \"completed\"", "\"status\": \"in_progress\"");
    let jobs = format!(r#"{{"total_count": 1, "jobs": [{in_progress}]}}"#);
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("nonterminal fixture parses");
    assert_eq!(records[0].jobs_complete, Some(true));
    assert_eq!(records[0].completion_state, "unknown_job_timestamps");
    assert_eq!(records[0].all_jobs_completed_at, None);
    assert_eq!(records[0].execution_sum_ms, None);

    let run = run_json(21, 1, "completed", "refs/pull/7/merge");
    let inverted = job(
        211,
        21,
        1,
        "inverted",
        "success",
        "2026-09-20T00:00:20Z",
        "2026-09-20T00:00:19Z",
    );
    let jobs = format!(r#"{{"total_count": 1, "jobs": [{inverted}]}}"#);
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("inverted fixture parses");
    assert_eq!(records[0].jobs_complete, Some(true));
    assert_eq!(records[0].completion_state, "unknown_job_timestamps");
    assert_eq!(records[0].all_jobs_completed_at, None);
    assert_eq!(records[0].execution_unknown_jobs, 1);
}

#[test]
fn required_gate_requires_terminal_and_valid_order() {
    let run = run_json(22, 1, "completed", "refs/pull/7/merge");
    let in_progress = job(
        221,
        22,
        1,
        "required",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    )
    .replace("\"status\": \"completed\"", "\"status\": \"in_progress\"");
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{in_progress}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required".to_owned()],
        },
    )
    .expect("nonterminal gate fixture parses");
    assert_eq!(records[0].required_gate_completed_at, None);
    assert_eq!(records[0].required_gate_state, "unknown_gate_nonterminal");

    let run = run_json(23, 1, "completed", "refs/pull/7/merge");
    let inverted = job(
        231,
        23,
        1,
        "required",
        "success",
        "2026-09-20T00:00:20Z",
        "2026-09-20T00:00:19Z",
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{inverted}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required".to_owned()],
        },
    )
    .expect("inverted gate fixture parses");
    assert_eq!(records[0].required_gate_completed_at, None);
    assert_eq!(records[0].required_gate_state, "unknown_gate_invalid_order");
}

#[test]
fn required_gate_requires_matching_identity_and_clean_pages() {
    let run = run_json(24, 1, "completed", "refs/pull/7/merge");
    let mismatched = job(
        241,
        24,
        2,
        "required",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{mismatched}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required".to_owned()],
        },
    )
    .expect("mismatched gate fixture parses");
    let mismatched_row = records
        .iter()
        .find(|record| record.job_id == Some(241))
        .expect("mismatched gate row");
    assert_eq!(mismatched_row.required_gate_completed_at, None);
    assert_eq!(mismatched_row.required_gate_state, "unknown_gate_identity");

    let run = run_json(25, 1, "completed", "refs/pull/7/merge");
    let duplicate = job(
        251,
        25,
        1,
        "required",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let records = collect_workflow(
        &[ ("run.json".to_owned(), run) ],
        &[(
            "jobs.json".to_owned(),
            format!(
                r#"{{"total_count": 2, "page": 1, "per_page": 100, "jobs": [{duplicate}, {duplicate}]}}"#
            ),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required".to_owned()],
        },
    )
    .expect("duplicate gate fixture parses");
    assert!(records.iter().all(|record| {
        record.required_gate_completed_at.is_none()
            && record.required_gate_state == "unknown_gate_collection_conflict"
    }));
}

#[test]
fn required_gate_rejects_partial_or_skipped_selector_sets() {
    let run = run_json(27, 1, "completed", "refs/pull/7/merge");
    let only_a = job(
        271,
        27,
        1,
        "required-a",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{only_a}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required-a".to_owned(), "required-b".to_owned()],
        },
    )
    .expect("partial selector fixture parses");
    assert_eq!(records[0].required_gate_completed_at, None);
    assert_eq!(records[0].required_gate_state, "unknown_gate_missing_job");

    let run = run_json(28, 1, "completed", "refs/pull/7/merge");
    let complete_a = job(
        281,
        28,
        1,
        "required-a",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let skipped_b = job(
        282,
        28,
        1,
        "required-b",
        "skipped",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 2, "jobs": [{complete_a}, {skipped_b}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required-a".to_owned(), "required-b".to_owned()],
        },
    )
    .expect("skipped selector fixture parses");
    assert_eq!(records[0].required_gate_completed_at, None);
    assert_eq!(records[0].required_gate_state, "unknown_gate_skipped");

    let run = run_json(29, 1, "completed", "refs/pull/7/merge");
    let missing_id = job(
        291,
        29,
        1,
        "required-a",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    )
    .replace("\"id\": 291, ", "");
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{missing_id}]}}"#),
        )],
        &WorkflowCollectOptions {
            required_job_names: vec!["required-a".to_owned()],
        },
    )
    .expect("missing selector identity fixture parses");
    assert_eq!(records[0].required_gate_completed_at, None);
    assert_eq!(records[0].required_gate_state, "unknown_gate_identity");
}

#[test]
fn duplicate_job_ids_are_retained_but_censor_aggregate_timing() {
    let run = run_json(14, 1, "completed", "refs/pull/7/merge");
    let duplicate = job(
        141,
        14,
        1,
        "duplicate",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let jobs = format!(
        r#"{{"total_count": 2, "page": 1, "per_page": 100, "jobs": [{duplicate}, {duplicate}]}}"#
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("duplicate fixture parses");

    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));
    assert!(records
        .iter()
        .all(|record| record.malformed_job_entries == 0));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| record.execution_partial_sum_ms == 2_000));
}

#[test]
fn conflicting_job_payloads_are_retained_but_excluded_from_partial_work() {
    let run = run_json(141, 1, "completed", "refs/pull/7/merge");
    let first = job(
        1411,
        141,
        1,
        "conflicting",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let second = job(
        1411,
        141,
        1,
        "conflicting",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:15Z",
    );
    let jobs =
        format!(r#"{{"total_count": 2, "page": 1, "per_page": 100, "jobs": [{first}, {second}]}}"#);
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("conflicting payload fixture parses");

    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));
    assert!(records
        .iter()
        .all(|record| record.execution_partial_sum_ms == 0));
    assert!(records
        .iter()
        .all(|record| record.execution_unknown_jobs == 1));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms.is_none()));
}

#[test]
fn malformed_entries_are_counted_and_missing_ids_are_retained() {
    let run = run_json(15, 1, "completed", "refs/pull/7/merge");
    let missing_id = r#"{
      "run_id": 15, "run_attempt": 1, "name": "missing-id", "status": "completed",
      "conclusion": "success", "started_at": "2026-09-20T00:00:10Z",
      "completed_at": "2026-09-20T00:00:12Z"
    }"#;
    let valid = job(
        151,
        15,
        1,
        "valid",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:14Z",
    );
    let jobs = format!(
        r#"{{"total_count": 3, "page": 1, "per_page": 100, "run_id": 15, "jobs": [{valid}, {missing_id}, {{}}]}}"#
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("malformed fixture parses");

    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));
    assert!(records
        .iter()
        .all(|record| record.malformed_job_entries == 2));
    assert!(records.iter().any(|record| record.job_id.is_none()));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms.is_none()));
}

#[test]
fn multiple_pages_without_identity_or_with_a_gap_are_censored() {
    let run = run_json(16, 1, "completed", "refs/pull/7/merge");
    let first = job(
        161,
        16,
        1,
        "first",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let second = job(
        162,
        16,
        1,
        "second",
        "success",
        "2026-09-20T00:00:11Z",
        "2026-09-20T00:00:14Z",
    );
    let pages_without_identity = vec![
        (
            "jobs-page-a.json".to_owned(),
            format!(r#"{{"total_count": 2, "jobs": [{first}]}}"#),
        ),
        (
            "jobs-page-b.json".to_owned(),
            format!(r#"{{"total_count": 2, "jobs": [{second}]}}"#),
        ),
    ];
    let records = collect_workflow(
        &[("run.json".to_owned(), run.clone())],
        &pages_without_identity,
        &WorkflowCollectOptions::default(),
    )
    .expect("missing page identity fixture parses");
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));

    let gap_pages = vec![
        (
            "jobs-page-1.json".to_owned(),
            format!(
                r#"{{"source_url":"https://api.github.com/repos/tailrocks/parallax/actions/runs/16/jobs?page=1&per_page=1","response":{{"total_count":2,"jobs":[{first}]}}}}"#
            ),
        ),
        (
            "jobs-page-3.json".to_owned(),
            format!(
                r#"{{"source_url":"https://api.github.com/repos/tailrocks/parallax/actions/runs/16/jobs?page=3&per_page=1","response":{{"total_count":2,"jobs":[{second}]}}}}"#
            ),
        ),
    ];
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &gap_pages,
        &WorkflowCollectOptions::default(),
    )
    .expect("page gap fixture parses");
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));
}

#[test]
fn conflicting_totals_mixed_attempts_and_duplicate_pages_are_censored() {
    let run = run_json(18, 1, "completed", "refs/pull/7/merge");
    let first = job(
        181,
        18,
        1,
        "first",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let second_attempt = job(
        182,
        18,
        2,
        "retry",
        "success",
        "2026-09-20T00:00:11Z",
        "2026-09-20T00:00:14Z",
    );
    let mixed_attempts = format!(
        r#"{{"total_count": 2, "page": 1, "per_page": 100, "jobs": [{first}, {second_attempt}]}}"#
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run.clone())],
        &[("mixed-attempts.json".to_owned(), mixed_attempts)],
        &WorkflowCollectOptions::default(),
    )
    .expect("mixed attempt fixture parses");
    let run_rows: Vec<_> = records
        .iter()
        .filter(|record| record.run_attempt == Some(1))
        .collect();
    assert!(run_rows
        .iter()
        .all(|record| record.jobs_complete == Some(false)));

    let second_same_attempt = job(
        183,
        18,
        1,
        "second",
        "success",
        "2026-09-20T00:00:11Z",
        "2026-09-20T00:00:14Z",
    );
    let conflicting_totals = vec![
        (
            "total-one.json".to_owned(),
            format!(r#"{{"total_count": 1, "page": 1, "per_page": 1, "jobs": [{first}]}}"#),
        ),
        (
            "total-two.json".to_owned(),
            format!(
                r#"{{"total_count": 2, "page": 2, "per_page": 1, "jobs": [{second_same_attempt}]}}"#
            ),
        ),
    ];
    let records = collect_workflow(
        &[("run.json".to_owned(), run.clone())],
        &conflicting_totals,
        &WorkflowCollectOptions::default(),
    )
    .expect("conflicting total fixture parses");
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));

    let duplicate_page = vec![
        (
            "duplicate-page-one.json".to_owned(),
            format!(r#"{{"total_count": 2, "page": 1, "per_page": 1, "jobs": [{first}]}}"#),
        ),
        (
            "duplicate-page-two.json".to_owned(),
            format!(
                r#"{{"total_count": 2, "page": 1, "per_page": 1, "jobs": [{second_same_attempt}]}}"#
            ),
        ),
    ];
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &duplicate_page,
        &WorkflowCollectOptions::default(),
    )
    .expect("duplicate page fixture parses");
    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(false)));
}

#[test]
fn referenced_workflow_sha_is_not_merge_proof() {
    let run = run_json(17, 1, "completed", "refs/heads/feature");
    let jobs = format!(
        r#"{{"total_count": 1, "jobs": [{}]}}"#,
        job(
            171,
            17,
            1,
            "one",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:12Z"
        )
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("reusable workflow fixture parses");

    assert_eq!(records[0].merge_sha, None);
    assert_eq!(records[0].merge_ref, None);
    assert_eq!(records[0].merge_sha_basis, None);
    assert_eq!(
        records[0].referenced_workflows[0].sha.as_deref(),
        Some("workflow-17")
    );
}

#[test]
fn malformed_referenced_workflow_members_are_preserved_as_evidence() {
    let run = run_json(26, 1, "completed", "refs/pull/7/merge").replace(
        r#""referenced_workflows": [{"path": "tailrocks/parallax/.github/workflows/reusable.yml", "sha": "workflow-26", "ref": "refs/pull/7/merge"}]"#,
        r#""referenced_workflows": [{"path": "tailrocks/parallax/.github/workflows/reusable.yml", "sha": "workflow-26", "ref": "refs/pull/7/merge"}, null, "bad", {"sha": 7}, {}]"#,
    );
    let jobs = format!(
        r#"{{"total_count": 1, "jobs": [{}]}}"#,
        job(
            261,
            26,
            1,
            "one",
            "success",
            "2026-09-20T00:00:10Z",
            "2026-09-20T00:00:12Z"
        )
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("malformed referenced workflow fixture parses");

    let issues = &records[0].referenced_workflow_evidence_issues;
    assert_eq!(issues.len(), 5);
    assert!(issues
        .iter()
        .any(|issue| issue.member_index == 1 && issue.raw_json == "null"));
    assert!(issues
        .iter()
        .any(|issue| issue.member_index == 2 && issue.raw_json == "\"bad\""));
    assert!(issues.iter().any(|issue| {
        issue.member_index == 3
            && issue.message == "field \"sha\" must be a string"
            && issue.raw_json == "7"
    }));
    assert!(issues.iter().any(|issue| {
        issue.member_index == 4
            && issue.message == "member has no recognized workflow fields"
            && issue.raw_json == "{}"
    }));
}

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "fixture assertions intentionally fail loudly"
)]

use unit_collector::{
    collect_workflow, parse_run_documents, render_workflow_summary, WorkflowCollectOptions,
};

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
fn replay_keeps_raw_run_head_and_live_pr_head_separate() {
    let runs = parse_run_documents(&[(
        "historical-run.json".to_owned(),
        r#"{
          "id": 35487077663,
          "run_attempt": 1,
          "event": "pull_request",
          "ref": null,
          "head_sha": "e1357dd620fe4bc18c9fa6e0d7b0c636a0f9175d",
          "pull_requests": [{
            "number": 968,
            "head": {"sha": "29279ab2bbf39d23bff052246c488dafdc6146c0"}
          }]
        }"#
        .to_owned(),
    )])
    .expect("historical run parses");

    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].head_sha.as_deref(),
        Some("e1357dd620fe4bc18c9fa6e0d7b0c636a0f9175d")
    );
    assert_eq!(runs[0].source_sha, None);
    assert_eq!(
        runs[0].source_sha_basis.as_deref(),
        Some("unknown.pull_request_source_not_proven")
    );
    assert_eq!(
        runs[0].observed_pull_request_head_sha.as_deref(),
        Some("29279ab2bbf39d23bff052246c488dafdc6146c0")
    );
    assert_eq!(runs[0].merge_sha, None);
}

#[test]
fn explicit_pull_refs_do_not_turn_api_heads_into_checkout_evidence() {
    for (reference, number, head, source) in [
        ("refs/pull/968/merge", 968, "1".repeat(40), None),
        (
            "refs/pull/968/head",
            968,
            "2".repeat(40),
            Some("2".repeat(40)),
        ),
        ("refs/pull/969/head", 968, "3".repeat(40), None),
        ("refs/pull/968/head", 968, "invalid".to_owned(), None),
    ] {
        let raw = serde_json::json!({
            "id": 1,
            "event": "pull_request",
            "ref": reference,
            "head_sha": head,
            "pull_requests": [{"number": number, "head": {"sha": "b".repeat(40)}}]
        });
        let runs = parse_run_documents(&[("pull-ref.json".to_owned(), raw.to_string())])
            .expect("explicit pull ref parses");
        assert_eq!(runs[0].source_sha, source);
        assert_eq!(runs[0].merge_sha, None);
        assert_eq!(runs[0].head_sha.as_deref(), Some(head.as_str()));
    }
}

#[test]
fn invalid_or_unrecognized_run_identity_fails_closed() {
    let runs = parse_run_documents(&[
        (
            "invalid-push.json".to_owned(),
            r#"{
              "id": 1,
              "event": "push",
              "ref": "refs/heads/main",
              "head_sha": "not-a-sha"
            }"#
            .to_owned(),
        ),
        (
            "unknown-event.json".to_owned(),
            r#"{
              "id": 2,
              "event": "workflow_run",
              "ref": "refs/heads/main",
              "head_sha": "1111111111111111111111111111111111111111"
            }"#
            .to_owned(),
        ),
        (
            "valid-push.json".to_owned(),
            r#"{
              "id": 3,
              "event": "push",
              "ref": "refs/heads/main",
              "head_sha": "2222222222222222222222222222222222222222"
            }"#
            .to_owned(),
        ),
        (
            "pull-request-target.json".to_owned(),
            r#"{
              "id": 4,
              "event": "pull_request_target",
              "ref": "refs/heads/main",
              "head_sha": "3333333333333333333333333333333333333333"
            }"#
            .to_owned(),
        ),
    ])
    .expect("identity fixtures parse");

    assert_eq!(runs[0].head_sha.as_deref(), Some("not-a-sha"));
    assert_eq!(runs[0].source_sha, None);
    assert_eq!(
        runs[0].source_sha_basis.as_deref(),
        Some("unknown.invalid_run_head_sha")
    );
    assert_eq!(runs[1].source_sha, None);
    assert_eq!(
        runs[1].source_sha_basis.as_deref(),
        Some("unknown.event_semantics:workflow_run")
    );
    assert_eq!(
        runs[2].source_sha.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );
    assert_eq!(
        runs[2].source_sha_basis.as_deref(),
        Some("run.head_sha.event=push")
    );
    assert_eq!(runs[3].source_sha, None);
    assert_eq!(
        runs[3].source_sha_basis.as_deref(),
        Some("unknown.pull_request_target_source_not_proven")
    );
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
    assert_eq!(records[0].run_lifetime_ms, Some(15_000));
    assert_eq!(records[0].run_lifetime_state, "observed_initial_attempt");
    assert_eq!(records[0].attempt_wall_ms, Some(10_000));
    assert_eq!(records[0].fresh_attempt_wall_ms, Some(10_000));
    assert_eq!(records[0].attempt_freshness_state, "complete_fresh");
    assert_eq!(records[0].execution_sum_ms, Some(6_000));
    assert_eq!(records[0].fresh_execution_partial_sum_ms, Some(6_000));
    assert_eq!(records[0].stale_execution_partial_sum_ms, None);
    assert_eq!(records[0].fresh_executed_jobs, 2);
    assert_eq!(records[0].stale_executed_jobs, 0);
    assert_eq!(records[0].freshness_unknown_jobs, 0);
    assert_eq!(
        records[0].required_gate_completed_at.as_deref(),
        Some("2026-09-20T00:00:12Z")
    );
    assert_eq!(records[0].required_gate_state, "observed");
    assert_eq!(records[0].critical_path_ms, None);
    assert_eq!(records[0].merge_sha, None);
    assert_eq!(records[0].merge_ref.as_deref(), Some("refs/pull/7/merge"));
    assert_eq!(records[0].merge_sha_basis, None);
    assert_eq!(records[0].source_sha, None);
    assert_eq!(
        records[0].observed_pull_request_head_sha.as_deref(),
        Some("source-10")
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
    assert!(records
        .iter()
        .all(|record| record.attempt_freshness_state == "complete_fresh"));
    assert!(records
        .iter()
        .all(|record| record.fresh_attempt_wall_ms == Some(9_000)));
    let skipped = records
        .iter()
        .find(|record| record.job_name.as_deref() == Some("skipped"))
        .expect("skipped row");
    assert_eq!(skipped.job_duration_ms, None);
    assert_eq!(skipped.job_duration_state, "skipped");
    assert_eq!(skipped.job_freshness_state, "not_applicable_skipped");
}

#[test]
fn rerun_freshness_censors_mixed_records_and_keeps_partial_evidence() {
    let run = run_json(30, 2, "completed", "refs/pull/7/merge")
        .replace("2026-09-20T00:00:05Z", "2026-09-20T00:00:10Z");
    let stale = job(
        301,
        30,
        2,
        "reused",
        "success",
        "2026-09-20T00:00:07Z",
        "2026-09-20T00:00:09Z",
    );
    let fresh = job(
        302,
        30,
        2,
        "rerun",
        "success",
        "2026-09-20T00:00:11Z",
        "2026-09-20T00:00:13Z",
    );
    let jobs = format!(r#"{{"total_count": 2, "jobs": [{stale}, {fresh}]}}"#);
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[("jobs.json".to_owned(), jobs)],
        &WorkflowCollectOptions::default(),
    )
    .expect("mixed rerun fixture parses");

    assert!(records
        .iter()
        .all(|record| record.jobs_complete == Some(true)));
    assert!(records.iter().all(|record| record.attempt_match));
    assert!(records
        .iter()
        .all(|record| record.attempt_freshness_state == "mixed_stale_records"));
    assert!(records
        .iter()
        .all(|record| record.completion_state == "unknown_mixed_job_freshness"));
    assert!(records
        .iter()
        .all(|record| record.run_lifetime_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| { record.run_lifetime_state == "unknown_rerun_created_at_is_original" }));
    assert!(records.iter().all(|record| record.pre_start_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| record.pre_start_state == "unknown_rerun_inter_attempt"));
    assert!(records
        .iter()
        .all(|record| record.attempt_wall_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| record.fresh_attempt_wall_ms == Some(3_000)));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| record.execution_partial_sum_ms == 4_000));
    assert!(records
        .iter()
        .all(|record| record.fresh_execution_partial_sum_ms == Some(2_000)));
    assert!(records
        .iter()
        .all(|record| record.stale_execution_partial_sum_ms == Some(2_000)));
    assert!(records
        .iter()
        .all(|record| record.fresh_execution_unknown_jobs == 0));
    assert!(records
        .iter()
        .all(|record| record.stale_execution_unknown_jobs == 0));
    assert!(records.iter().all(|record| record.fresh_executed_jobs == 1));
    assert!(records.iter().all(|record| record.stale_executed_jobs == 1));
    assert!(records
        .iter()
        .all(|record| record.freshness_unknown_jobs == 0));

    let stale_row = records
        .iter()
        .find(|record| record.job_name.as_deref() == Some("reused"))
        .expect("stale row");
    assert_eq!(stale_row.job_freshness_state, "stale_before_attempt");
    let fresh_row = records
        .iter()
        .find(|record| record.job_name.as_deref() == Some("rerun"))
        .expect("fresh row");
    assert_eq!(fresh_row.job_freshness_state, "fresh");

    let summary = render_workflow_summary(&records);
    assert!(summary.contains("Job freshness"));
    assert!(summary.contains("stale_before_attempt"));
    assert!(summary.contains("not current-attempt bottlenecks"));
}

#[test]
fn rerun_with_all_fresh_jobs_has_attempt_timing_but_no_initial_lifetime() {
    let run = run_json(31, 2, "completed", "refs/pull/7/merge");
    let first = job(
        311,
        31,
        2,
        "first",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    );
    let second = job(
        312,
        31,
        2,
        "second",
        "success",
        "2026-09-20T00:00:11Z",
        "2026-09-20T00:00:14Z",
    );
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 2, "jobs": [{first}, {second}]}}"#),
        )],
        &WorkflowCollectOptions::default(),
    )
    .expect("fresh rerun fixture parses");

    assert!(records
        .iter()
        .all(|record| record.attempt_freshness_state == "complete_fresh"));
    assert!(records
        .iter()
        .all(|record| record.completion_state == "verified"));
    assert!(records
        .iter()
        .all(|record| record.run_lifetime_ms.is_none()));
    assert!(records
        .iter()
        .all(|record| record.run_lifetime_state == "unknown_rerun_created_at_is_original"));
    assert!(records
        .iter()
        .all(|record| record.attempt_wall_ms == Some(9_000)));
    assert!(records
        .iter()
        .all(|record| record.fresh_attempt_wall_ms == Some(9_000)));
    assert!(records
        .iter()
        .all(|record| record.execution_sum_ms == Some(5_000)));
}

#[test]
fn missing_job_start_fails_freshness_without_rejecting_job_collection() {
    let run = run_json(32, 2, "completed", "refs/pull/7/merge");
    let missing_start = job(
        321,
        32,
        2,
        "missing-start",
        "success",
        "2026-09-20T00:00:10Z",
        "2026-09-20T00:00:12Z",
    )
    .replace("\"started_at\": \"2026-09-20T00:00:10Z\",", "");
    let records = collect_workflow(
        &[("run.json".to_owned(), run)],
        &[(
            "jobs.json".to_owned(),
            format!(r#"{{"total_count": 1, "jobs": [{missing_start}]}}"#),
        )],
        &WorkflowCollectOptions::default(),
    )
    .expect("missing job start fixture parses");

    assert_eq!(records[0].jobs_complete, Some(true));
    assert_eq!(records[0].attempt_freshness_state, "unknown_job_freshness");
    assert_eq!(records[0].job_freshness_state, "unknown_job_start");
    assert_eq!(records[0].attempt_wall_ms, None);
    assert_eq!(records[0].fresh_attempt_wall_ms, None);
    assert_eq!(records[0].execution_sum_ms, None);
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
    assert_eq!(records[0].attempt_wall_ms, None);
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
    assert_eq!(records[0].run_lifetime_ms, None);
    assert_eq!(
        records[0].run_lifetime_state,
        "unknown_initial_attempt_timing"
    );
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

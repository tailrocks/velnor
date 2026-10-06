//! Step-output workflow-command processing: the parse/merge/render half of
//! the workflow-command surface.
//!
//! Moved out of `executor.rs` (decomposition slice 2, GOAL 49/58). The
//! directive parser itself lives in `workflow_command` and the
//! command-file plan (`ScriptStepPlan`) in `script_step`; what lives here
//! is the glue the step runners call around captured step output: parse
//! `::directives` from stdout/stderr and merge them into one command
//! state, fold a failed command result into the step exit code, render
//! the step log under the same directive policy, and rewrite
//! command-file env paths for action containers. Behavior is
//! byte-identical to the pre-move code: bodies moved verbatim,
//! `pub(crate)` visibility only.

use crate::{
    executor::StepExecutionResult,
    script_step::StepCommandState,
    workflow_command::{
        parse_workflow_commands_with_job_env, rendered_output_lines_with_policy,
        DeprecatedCommandScope,
    },
};

pub(crate) fn parse_workflow_commands_from_output(
    stdout: &str,
    stderr: &str,
    job_env: &[(String, String)],
    scope: &mut DeprecatedCommandScope,
) -> StepCommandState {
    let mut state = parse_workflow_commands_with_job_env(stdout, job_env, scope);
    state.merge(parse_workflow_commands_with_job_env(stderr, job_env, scope));
    state
}

/// Upstream `RunStepAsync` command-result merge
/// (`CompositeActionHandler.cs` / `StepsRunner.cs`: "Merge execution
/// context result with command result"): a workflow command that failed
/// to process fails the step even when the process itself exited 0 —
/// e.g. `::echo::maybe` in an otherwise green script.
pub(crate) fn apply_command_result(mut result: StepExecutionResult) -> StepExecutionResult {
    if result.state.command_failed && result.exit_code == 0 {
        result.exit_code = 1;
    }
    result
}

/// Log lines for a skipped step. A skipped step still posts a log record, and
/// an EMPTY record is indistinguishable from a quiet successful step — that
/// hid the tailrocks/velnor#311 cascade where a daemon restart killed a step
/// mid-flight and every later (export/upload) step silently "succeeded"
/// without running. One explicit, ungrouped line keeps the skip visible in
/// the timeline feed. Skip semantics are unchanged (exit 0, skipped=true);
/// only visibility changes.
pub(crate) fn skipped_step_log_lines() -> Vec<String> {
    vec!["Step skipped: condition evaluated to false (a previous step failed or the if-condition was not met)".to_string()]
}

pub(crate) fn step_log_lines(
    display_name: &str,
    stdout: &str,
    stderr: &str,
    skipped: bool,
    prelude: &[String],
    step_debug: bool,
    allow_unsecure_stop_command_tokens: bool,
) -> Vec<String> {
    if skipped {
        return skipped_step_log_lines();
    }
    let step_name = if display_name.is_empty() {
        "step"
    } else {
        display_name
    };
    // GitHub's step log groups ONLY the header (command + with:/env:); output
    // stays visible below it, with `::group::`/`::warning::`-style workflow
    // commands converted IN PLACE to `##[...]` markers (actions/runner keeps
    // their position; reordering them to the end breaks user grouping).
    let mut lines = vec![format!("##[group]{step_name}")];
    lines.extend(prelude.iter().cloned());
    lines.push("##[endgroup]".to_string());
    lines.extend(rendered_output_lines_with_policy(
        stdout,
        stderr,
        step_debug,
        allow_unsecure_stop_command_tokens,
    ));
    lines
}

pub(crate) fn rewrite_command_file_env_for_action_container(env: &mut [(String, String)]) {
    for (name, value) in env {
        if matches!(
            name.as_str(),
            "GITHUB_OUTPUT" | "GITHUB_ENV" | "GITHUB_PATH" | "GITHUB_STATE" | "GITHUB_STEP_SUMMARY"
        ) && let Some(file_name) = value.strip_prefix("/__t/")
        {
            *value = format!("/github/file_commands/{file_name}");
        }
    }
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
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_root = std::env::temp_dir();
        let temp_root = temp_root.canonicalize().unwrap_or(temp_root);
        temp_root.join(format!(
            "velnor-command-output-test-{}-{nonce}-{sequence}",
            std::process::id()
        ))
    }

    /// Upstream `RunStepAsync` merges the command result into the step
    /// result: a workflow command that failed to process (e.g. an invalid
    /// `::echo::` value) fails the step even when the process exited 0.
    #[test]
    fn command_result_merge_fails_step_on_command_failure() {
        let failed_state = StepCommandState {
            command_failed: true,
            ..StepCommandState::default()
        };
        let failed = apply_command_result(StepExecutionResult {
            exit_code: 0,
            state: failed_state,
            skipped: false,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        });
        assert_eq!(failed.exit_code, 1);
        let clean = apply_command_result(StepExecutionResult {
            exit_code: 0,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        });
        assert_eq!(clean.exit_code, 0);
        let already_failed = apply_command_result(StepExecutionResult {
            exit_code: 3,
            state: StepCommandState {
                command_failed: true,
                ..StepCommandState::default()
            },
            skipped: false,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        });
        assert_eq!(already_failed.exit_code, 3);
    }

    #[test]
    fn unsecure_command_opt_in_flows_from_job_env_to_step_parsing() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(&workspace).unwrap();

        // The executor parses step output with the step's effective
        // environment, so a job-level opt-in honors the legacy command.
        let job_env = vec![(
            "ACTIONS_ALLOW_UNSECURE_COMMANDS".to_string(),
            "true".to_string(),
        )];
        let state = parse_workflow_commands_from_output(
            "::set-env name=RESTORED::yes\n",
            "",
            &job_env,
            &mut DeprecatedCommandScope::default(),
        );
        assert_eq!(state.env.get("RESTORED").map(String::as_str), Some("yes"));

        let refused = parse_workflow_commands_from_output(
            "::set-env name=MODE::x\n",
            "",
            &[],
            &mut DeprecatedCommandScope::default(),
        );
        assert!(refused.env.is_empty());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn step_log_lines_renders_workflow_commands_in_place() {
        // `::group::`/`::endgroup::` convert to ##[group] markers AT THEIR
        // ORIGINAL POSITION (GitHub keeps user grouping placement); state
        // commands like ::set-output are consumed invisibly.
        let stdout =
            "::group::Build phase\nsome build output\n::endgroup::\n::set-output name=x::42\n";
        let stderr = "";
        let lines = step_log_lines("Run tests", stdout, stderr, false, &[], false, false);

        let group_at = lines
            .iter()
            .position(|l| l == "##[group]Build phase")
            .expect("group marker in place");
        let output_at = lines
            .iter()
            .position(|l| l == "some build output")
            .expect("output present");
        let endgroup_at = lines
            .iter()
            .rposition(|l| l == "##[endgroup]")
            .expect("endgroup marker in place");
        assert!(
            group_at < output_at && output_at < endgroup_at,
            "group markers must wrap the output they grouped: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("::")),
            "raw workflow command lines must not leak: {lines:?}"
        );
    }

    #[test]
    fn step_log_lines_passes_through_normal_output() {
        let stdout = "cargo test passed\nall 42 tests passed\n";
        let stderr = "warning: unused import\n";
        let lines = step_log_lines("Run cargo test", stdout, stderr, false, &[], false, false);
        assert!(lines.iter().any(|l| l.contains("cargo test passed")));
        assert!(lines.iter().any(|l| l.contains("all 42 tests passed")));
        assert!(lines.iter().any(|l| l.contains("warning: unused import")));
        assert!(lines.iter().any(|l| l == "##[group]Run cargo test"));
        // GitHub closes the header group BEFORE the output: output lines stay
        // visible without expanding a group, and no "Finishing:" line exists.
        assert_eq!(lines[1], "##[endgroup]");
        assert!(!lines.iter().any(|l| l.starts_with("Finishing:")));
    }

    #[test]
    fn step_log_lines_includes_github_style_metadata_prelude() {
        let prelude = vec![
            "with:".to_string(),
            "  token: ***".to_string(),
            "env:".to_string(),
            "  RUN_TESTS: true".to_string(),
        ];
        let lines = step_log_lines("Run action", "done\n", "", false, &prelude, false, false);
        let joined = lines.join("\n");
        assert!(joined.contains("with:\n  token: ***"));
        assert!(joined.contains("env:\n  RUN_TESTS: true"));
        // Header group closes before output: done is OUTSIDE the group.
        assert!(joined.contains("##[endgroup]\ndone"));
    }

    #[test]
    fn step_log_lines_keeps_summary_out_of_the_log() {
        // GITHUB_STEP_SUMMARY renders in the run Summary tab via its own
        // Results Service upload — GitHub never inlines it into the step log.
        let stdout = "output line\n";
        let lines = step_log_lines("Summarize", stdout, "", false, &[], false, false);
        let joined = lines.join("\n");
        assert!(!joined.contains("Step summary:"));
        assert!(joined.contains("output line"));
    }

    #[test]
    fn step_log_lines_keeps_silent_executed_steps_expandable() {
        let lines = step_log_lines("Silent action", "", "", false, &[], false, false);
        assert_eq!(
            lines,
            vec![
                "##[group]Silent action".to_string(),
                "##[endgroup]".to_string(),
            ]
        );
    }

    #[test]
    fn step_log_lines_marks_skipped_steps_visibly() {
        let lines = step_log_lines("Skipped action", "", "", true, &[], false, false);
        assert_eq!(lines, skipped_step_log_lines());
        assert!(
            lines.iter().any(|line| line.contains("Step skipped:")),
            "a skipped step must carry an explicit marker, got {lines:?}"
        );
    }
}

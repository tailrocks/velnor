#![allow(async_fn_in_trait)]
//! Velnor self-hosted GitHub Actions runner.
//!
//! This crate is the runtime library behind the `velnorctl` command center:
//! every operator-facing CLI surface lives in `velnorctl`, and the plain
//! argument types in [`args`] are converted explicitly at that boundary
//! (Plan 064 dependency law — domain crates never depend on `clap`). The
//! interim [`scaffold`] facade exposes bootstrap and dispatch helpers until
//! Plan 079 deletes the crate after its runtime modules move.

#[cfg(all(not(debug_assertions), feature = "test-support"))]
compile_error!("test-support is forbidden in release-profile builds");
// NOTE: there is deliberately no `release-build + test-support` combination
// gate. The release profile is already fully covered above, so such a gate
// could only ever fire for dev-profile builds — exactly the harmless
// `--all-features` test path where `release-build` is inert without
// `VELNOR_RELEASE_BUILD=1` (see build.rs) and `test-support` is required
// for integration fixtures.

mod action;
/// Runner-owned action expansion contract exposed only to generator tests.
/// Production callers continue to use the runner's internal action planner;
/// the workflow generator uses this narrow feature to prove its consumer
/// fixtures against the same composite semantics.
#[cfg(feature = "test-support")]
pub mod action_contract {
    use std::collections::BTreeMap;

    pub use crate::action::{
        composite_action_invocations, parse_action_metadata, ActionInput, ActionMetadata,
        ActionOutput, ActionRuns, ActionRuntime, CompositeActionInvocation, CompositeActionOutputs,
        CompositeActionStep, LocalActionPlan, RepositoryActionPlan, ResolvedAction,
    };
    pub use crate::script_step::ScriptStep;

    /// Build the exact argv used by the runner for a composite `run` step.
    /// The generator's consumer harness uses this narrow test-support seam so
    /// shell selection stays owned by the runner rather than being guessed.
    pub fn script_command_args(step: &ScriptStep, script_path: &str) -> Vec<String> {
        step.shell.command_args(script_path)
    }

    /// Minimal step result needed to evaluate a composite condition with the
    /// same outcome/conclusion rules as job execution.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ActionStepStatus {
        pub exit_code: i32,
        pub skipped: bool,
        pub continue_on_error: bool,
    }

    /// Evaluate a fixture's action expression against runner-owned step output
    /// state.  Consumer tests use this narrow test-support seam instead of a
    /// string replacement evaluator, so output references exercise the same
    /// expression engine used by job execution.
    pub fn render_action_expression(
        value: &str,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Result<String, String> {
        render_action_expression_with_context(
            value,
            &BTreeMap::new(),
            &BTreeMap::new(),
            step_outputs,
        )
    }

    /// Render an action expression with the contexts supplied to a composite
    /// action's embedded steps.
    pub fn render_action_expression_with_context(
        value: &str,
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Result<String, String> {
        render_action_expression_with_step_context(
            value,
            action_inputs,
            action_env,
            step_outputs,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
    }

    /// Render a composite expression with step statuses and the current
    /// composite's aliases for its flattened child-step IDs.
    pub fn render_action_expression_with_step_context(
        value: &str,
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
        step_aliases: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let state = action_expression_state(
            action_inputs,
            action_env,
            step_outputs,
            step_statuses,
            step_aliases,
        )?;
        state
            .resolve_expressions(value)
            .map_err(|error| error.to_string())
    }

    /// Parse a `GITHUB_OUTPUT` command-file fragment with Runner-compatible
    /// file-command semantics. The shared parser handles standard records;
    /// this adapter covers its name/delimiter differences from FileCommandManager.
    pub fn parse_action_output_file_contents(
        contents: &str,
    ) -> Result<BTreeMap<String, String>, String> {
        let runner_commands = parse_runner_action_output_file_contents(contents)?;
        let shared_commands = crate::command_files::parse_command_file_contents(contents)
            .ok()
            .map(|commands| {
                commands
                    .into_iter()
                    .map(|command| (command.name, command.value))
                    .collect::<Vec<_>>()
            });
        let commands = shared_commands
            .filter(|commands| *commands == runner_commands)
            .unwrap_or(runner_commands);
        Ok(commands.into_iter().collect())
    }

    fn parse_runner_action_output_file_contents(
        contents: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let mut commands = Vec::new();
        let mut index = 0;

        while let Some((line, _newline)) = next_runner_action_output_line(contents, &mut index) {
            if line.is_empty() {
                continue;
            }

            let equals_index = line.find('=');
            let heredoc_index = line.find("<<");
            let is_key_value = equals_index.is_some_and(|equals_index| {
                heredoc_index.is_none_or(|heredoc_index| equals_index < heredoc_index)
            });
            if is_key_value {
                let Some((name, value)) = line.split_once('=') else {
                    return Err(format!("invalid command-file line: {line}"));
                };
                // Runner checks that the full line is nonempty here, rather
                // than checking the parsed name. Empty and whitespace names
                // therefore remain valid output keys.
                commands.push((name.to_owned(), value.to_owned()));
                continue;
            }

            let is_heredoc = heredoc_index.is_some_and(|heredoc_index| {
                equals_index.is_none_or(|equals_index| heredoc_index < equals_index)
            });
            if is_heredoc {
                let Some((name, delimiter)) = line.split_once("<<") else {
                    return Err(format!("invalid command-file line: {line}"));
                };
                if name.is_empty() || delimiter.is_empty() {
                    return Err(format!("invalid command-file heredoc header: {line}"));
                }

                let value_start = index;
                let mut value_end = value_start;
                let mut found_delimiter = false;
                while let Some((value_line, newline)) =
                    next_runner_action_output_line(contents, &mut index)
                {
                    if value_line == delimiter {
                        found_delimiter = true;
                        break;
                    }
                    if newline.is_empty() {
                        return Err(format!(
                            "heredoc for command '{name}' ended before its delimiter newline"
                        ));
                    }
                    value_end = index - newline.len();
                }
                if !found_delimiter {
                    return Err(format!(
                        "missing heredoc delimiter '{delimiter}' for command '{name}'"
                    ));
                }

                commands.push((name.to_owned(), contents[value_start..value_end].to_owned()));
                continue;
            }

            return Err(format!("invalid command-file line: {line}"));
        }

        Ok(commands)
    }

    fn next_runner_action_output_line<'a>(
        contents: &'a str,
        index: &mut usize,
    ) -> Option<(&'a str, &'a str)> {
        if *index >= contents.len() {
            return None;
        }

        let start = *index;
        let Some(line_feed_offset) = contents[start..].find('\n') else {
            *index = contents.len();
            return Some((&contents[start..], ""));
        };
        let line_feed = start + line_feed_offset;
        *index = line_feed + 1;

        // FileCommandManager.ReadLine recognizes CRLF as one newline only on
        // Windows. On other platforms the CR remains part of the line. Keep
        // that platform behavior so delimiter matching and the raw value
        // substring agree with the Runner implementation.
        #[cfg(windows)]
        let newline_start = if line_feed > start && contents.as_bytes()[line_feed - 1] == b'\r' {
            line_feed - 1
        } else {
            line_feed
        };
        #[cfg(not(windows))]
        let newline_start = line_feed;

        Some((
            &contents[start..newline_start],
            &contents[newline_start..line_feed + 1],
        ))
    }

    /// Evaluate a composite-step condition with the runner's typed condition
    /// evaluator, including implicit `success()` and status functions.
    pub fn evaluate_action_condition(
        condition: Option<&str>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
    ) -> Result<bool, String> {
        evaluate_action_condition_with_context(
            condition,
            &BTreeMap::new(),
            &BTreeMap::new(),
            step_outputs,
            step_statuses,
        )
    }

    /// Evaluate a composite-step condition with the runner's inputs, env,
    /// typed truthiness, implicit success, and status functions.
    pub fn evaluate_action_condition_with_context(
        condition: Option<&str>,
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
    ) -> Result<bool, String> {
        evaluate_action_condition_with_step_context(
            condition,
            action_inputs,
            action_env,
            step_outputs,
            step_statuses,
            &BTreeMap::new(),
        )
    }

    /// Evaluate a composite condition with aliases that expose flattened child
    /// results under the step IDs visible inside this composite.
    pub fn evaluate_action_condition_with_step_context(
        condition: Option<&str>,
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
        step_aliases: &BTreeMap<String, String>,
    ) -> Result<bool, String> {
        action_expression_state(
            action_inputs,
            action_env,
            step_outputs,
            step_statuses,
            step_aliases,
        )?
        .evaluate_condition(condition)
        .map_err(|error| error.to_string())
    }

    fn action_expression_state(
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
        step_aliases: &BTreeMap<String, String>,
    ) -> Result<crate::executor::JobExecutionState, String> {
        let state = action_expression_state_with_inputs(
            action_inputs,
            action_env,
            step_outputs,
            step_statuses,
            step_aliases,
        )?;
        let mut resolved_inputs = BTreeMap::new();
        for (name, value) in action_inputs {
            resolved_inputs.insert(
                name.clone(),
                state
                    .resolve_expressions(value)
                    .map_err(|error| error.to_string())?,
            );
        }
        if resolved_inputs == *action_inputs {
            Ok(state)
        } else {
            action_expression_state_with_inputs(
                &resolved_inputs,
                action_env,
                step_outputs,
                step_statuses,
                step_aliases,
            )
        }
    }

    fn action_expression_state_with_inputs(
        action_inputs: &BTreeMap<String, String>,
        action_env: &BTreeMap<String, String>,
        step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
        step_statuses: &BTreeMap<String, ActionStepStatus>,
        step_aliases: &BTreeMap<String, String>,
    ) -> Result<crate::executor::JobExecutionState, String> {
        fn context_object(values: &BTreeMap<String, String>) -> serde_json::Value {
            serde_json::Value::Object(
                values
                    .iter()
                    .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
                    .collect(),
            )
        }

        let context_data = [("inputs".to_owned(), context_object(action_inputs))];
        let base_env = action_env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let mut state =
            crate::executor::JobExecutionState::try_new_with_context(&base_env, &context_data)
                .map_err(|error| error.to_string())?;
        state.outputs = step_outputs.clone();
        for (step_id, status) in step_statuses {
            state.apply(
                step_id,
                &crate::executor::StepExecutionResult {
                    exit_code: status.exit_code,
                    state: crate::script_step::StepCommandState::default(),
                    skipped: status.skipped,
                    failure_ignored: status.continue_on_error,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            );
        }
        for (step_id, source_id) in step_aliases {
            if let Some(outputs) = state.outputs.get(source_id).cloned() {
                state.outputs.insert(step_id.clone(), outputs);
            }
            if let Some(outcome) = state.outcomes.get(source_id).copied() {
                state.outcomes.insert(step_id.clone(), outcome);
            }
            if let Some(conclusion) = state.conclusions.get(source_id).copied() {
                state.conclusions.insert(step_id.clone(), conclusion);
            }
        }
        Ok(state)
    }

    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test-support regressions use direct assertions"
    )]
    mod tests {
        use std::collections::BTreeMap;

        use super::{
            evaluate_action_condition, evaluate_action_condition_with_context,
            evaluate_action_condition_with_step_context, parse_action_output_file_contents,
            render_action_expression_with_step_context, ActionStepStatus,
        };

        #[test]
        fn action_conditions_keep_runner_typed_truthiness_and_status_defaults() {
            let outputs = BTreeMap::from([(
                "producer".to_owned(),
                BTreeMap::from([
                    ("false_string".to_owned(), "false".to_owned()),
                    ("zero_string".to_owned(), "0".to_owned()),
                    ("empty_string".to_owned(), String::new()),
                ]),
            )]);
            let statuses = BTreeMap::new();

            for (name, expected) in [
                ("false_string", true),
                ("zero_string", true),
                ("empty_string", false),
            ] {
                let condition = format!("${{{{ steps.producer.outputs.{name} }}}}");
                assert_eq!(
                    evaluate_action_condition(Some(&condition), &outputs, &statuses).unwrap(),
                    expected,
                    "output `{name}` must use expression truthiness"
                );
            }
            assert!(!evaluate_action_condition(Some("false"), &outputs, &statuses).unwrap());
            assert!(evaluate_action_condition(None, &outputs, &statuses).unwrap());
            assert!(evaluate_action_condition(Some(""), &outputs, &statuses).unwrap());
        }

        #[test]
        fn action_conditions_apply_implicit_success_and_continue_on_error() {
            let outputs = BTreeMap::new();
            let failed = BTreeMap::from([(
                "child".to_owned(),
                ActionStepStatus {
                    exit_code: 1,
                    skipped: false,
                    continue_on_error: false,
                },
            )]);
            assert!(!evaluate_action_condition(Some("true"), &outputs, &failed).unwrap());
            assert!(!evaluate_action_condition(Some("success()"), &outputs, &failed).unwrap());
            assert!(evaluate_action_condition(Some("failure()"), &outputs, &failed).unwrap());
            assert!(evaluate_action_condition(Some("always()"), &outputs, &failed).unwrap());

            let tolerated = BTreeMap::from([(
                "child".to_owned(),
                ActionStepStatus {
                    exit_code: 1,
                    skipped: false,
                    continue_on_error: true,
                },
            )]);
            assert!(evaluate_action_condition(Some("true"), &outputs, &tolerated).unwrap());
            assert!(!evaluate_action_condition(Some("failure()"), &outputs, &tolerated).unwrap());
        }

        #[test]
        fn action_conditions_receive_composite_inputs_and_environment() {
            let inputs = BTreeMap::from([
                ("run".to_owned(), "true".to_owned()),
                ("tolerate".to_owned(), "false".to_owned()),
            ]);
            let env = BTreeMap::from([("FLAG".to_owned(), "enabled".to_owned())]);

            assert!(evaluate_action_condition_with_context(
                Some("inputs.run == 'true' && env.FLAG == 'enabled'"),
                &inputs,
                &env,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap());
            assert!(!evaluate_action_condition_with_context(
                Some("inputs.tolerate == 'true'"),
                &inputs,
                &env,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap());
        }

        #[test]
        fn action_step_aliases_expose_outputs_outcome_and_conclusion() {
            let outputs = BTreeMap::from([(
                "parent-before".to_owned(),
                BTreeMap::from([("flag".to_owned(), "true".to_owned())]),
            )]);
            let statuses = BTreeMap::from([(
                "parent-before".to_owned(),
                ActionStepStatus {
                    exit_code: 9,
                    skipped: false,
                    continue_on_error: true,
                },
            )]);
            let aliases = BTreeMap::from([("before".to_owned(), "parent-before".to_owned())]);
            let expected = "true|failure|success";

            assert_eq!(
                render_action_expression_with_step_context(
                    "${{ steps.before.outputs.flag }}|${{ steps.before.outcome }}|${{ steps.before.conclusion }}",
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &outputs,
                    &statuses,
                    &aliases,
                )
                .unwrap(),
                expected
            );
            assert!(evaluate_action_condition_with_step_context(
                Some("steps.before.outputs.flag == 'true' && steps.before.outcome == 'failure' && steps.before.conclusion == 'success'"),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &outputs,
                &statuses,
                &aliases,
            )
            .unwrap());
        }

        #[test]
        fn action_output_parser_preserves_equals_inside_heredoc_values() {
            assert_eq!(
                parse_action_output_file_contents("result<<END\nfirst=one\nsecond=two\nEND\n")
                    .unwrap()
                    .get("result")
                    .map(String::as_str),
                Some("first=one\nsecond=two")
            );
            assert!(parse_action_output_file_contents("result<<END\nmissing end\n").is_err());
        }

        #[test]
        fn action_output_parser_preserves_heredoc_line_ending_bytes() {
            let lf = parse_action_output_file_contents("result<<END\nfirst=one\nsecond=two\nEND\n")
                .unwrap();
            assert_eq!(
                lf.get("result").map(String::as_str),
                Some("first=one\nsecond=two")
            );

            let crlf = parse_action_output_file_contents(
                "result<<END\r\nfirst=one\r\nsecond=two\r\nEND\r\n",
            )
            .unwrap();
            let expected = if cfg!(windows) {
                "first=one\r\nsecond=two"
            } else {
                "first=one\r\nsecond=two\r"
            };
            assert_eq!(crlf.get("result").map(String::as_str), Some(expected));
        }

        #[test]
        fn action_output_parser_matches_runner_name_and_delimiter_rules() {
            let outputs =
                parse_action_output_file_contents("=empty-name\n whitespace =spaced-name\n")
                    .unwrap();

            assert_eq!(outputs.get(""), Some(&"empty-name".to_owned()));
            assert_eq!(outputs.get(" whitespace "), Some(&"spaced-name".to_owned()));
            assert!(parse_action_output_file_contents("result<<\n\n").is_err());
        }
    }
}
mod admission;
pub mod args;
mod attestation;
mod buildkit;
mod cache;
mod capacity;
mod checkout;
mod command_files;
mod config;
pub use config::config_dir;
mod container;
pub mod daemon_instance;
pub mod docker;
mod docker_argv;
mod docker_lease;
pub mod execution;
mod executor;
mod expression;
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
mod fault_injection;
mod fs_copy;
pub(crate) mod gha_cache;
mod git_mirror;
mod github_adapter;
pub mod host_capacity;
mod job_claim;
mod job_message;
mod leftover_disk;
pub mod manifest;
pub(crate) mod mbx_store;
mod mise;
/// The locked-mise install contract the generator must emit against:
/// `install_args` tokens are tool keys the committed lock pins. Re-exported so
/// the generator's contract test proves its output passes this gate.
pub use mise::{is_valid_install_arg_token, lock_tool_keys, validate_install_args_against_lock};
pub mod node;
mod ops;
#[cfg(any(test, feature = "test-support"))]
pub mod permit_guard;
#[cfg(not(any(test, feature = "test-support")))]
mod permit_guard;
mod plan;
pub mod platform;
mod preflight;
pub mod protocol;
mod release;
/// The compile-time build identity, shared with `velnorctl --version` so the
/// operator CLI reports the same release/source SHA as `release export`.
pub use release::{embedded as embedded_build_identity, EmbeddedIdentity};
pub mod runner;
mod runtime_env;
pub mod scaleset;
mod sccache_compat;
mod script_step;
mod sd_notify;
pub mod service;
mod slot_log;
pub(crate) mod stable_workspace;
mod storage;
mod store_catalog;
mod telemetry;
pub mod trust_class;
pub mod trust_scope;
mod workflow_command;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Temporary migration scaffold (Plan 064).
///
/// Exposes the legacy binary's exact bootstrap sequence so `velnorctl` can
/// reuse it without spawning or duplicating anything. Removed before Plan 079;
/// not a compatibility promise.
pub mod scaffold {
    use crate::args::{self, Command};
    use anyhow::Result;
    use std::path::{Path, PathBuf};

    /// Initialize tracing exactly like the legacy binary bootstrap: long-running
    /// commands write spans to `<config-base>/logs/trace.jsonl`, one-shot
    /// commands only surface warnings on stderr.
    pub fn init_telemetry(log_dir: Option<&Path>) {
        crate::telemetry::init(log_dir);
    }

    /// Production admission preamble shared by every dispatch path:
    /// unconditional strict-capability environment enforcement plus compiled
    /// manifest integrity. Runs before any command is dispatched.
    pub fn enforce_admission() -> Result<()> {
        args::enforce_strict_capability_env()?;
        crate::manifest::assert_manifest_integrity()?;
        Ok(())
    }

    /// Telemetry selection for a parsed command, identical to the legacy
    /// bootstrap: long-running commands log spans, one-shot commands do not.
    pub fn telemetry_dir(command: &Command) -> Option<PathBuf> {
        match command {
            Command::Daemon(args) => crate::runner::daemon_config_dir(args)
                .ok()
                .map(|dir| dir.join("logs")),
            _ => None,
        }
    }

    /// Stable durable-store identity for the host running this daemon.
    ///
    /// Runner registration names select GitHub-facing endpoints and may vary
    /// per slot; operational rows use the same hostname projection as the
    /// runner's persistence path so API reads and writes share one identity.
    #[must_use]
    pub fn operational_instance_slug() -> String {
        #[cfg(unix)]
        let host =
            String::from_utf8_lossy(rustix::system::uname().nodename().to_bytes()).into_owned();
        #[cfg(not(unix))]
        let host = String::new();
        operational_instance_slug_from_host(&host)
    }

    fn operational_instance_slug_from_host(raw: &str) -> String {
        crate::ops::sanitize_slug_for_instance(raw)
    }

    pub async fn dispatch(command: Command) -> Result<()> {
        match command {
            Command::Cache(args) => crate::cache::run(args),
            Command::Capabilities(args) => crate::manifest::run(args),
            Command::Configure(args) => crate::runner::configure(args).await,
            Command::Daemon(args) => crate::runner::daemon(*args).await,
            Command::Preflight(args) => crate::preflight::preflight(args),
            Command::Remove(args) => crate::runner::remove(args).await,
            Command::Status(args) => crate::runner::status(args).await,
            Command::Storage(args) => crate::storage::run(args),
            Command::Doctor(args) => crate::runner::doctor(args).await,
            Command::Release(args) => crate::release::run(args),
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
        #[test]
        fn operational_identity_uses_the_host_slug_projection() {
            assert_eq!(
                super::operational_instance_slug_from_host("build host 1"),
                "build-host-1"
            );
            assert_eq!(
                super::operational_instance_slug_from_host("!!!"),
                "velnor-instance"
            );
        }
    }
}

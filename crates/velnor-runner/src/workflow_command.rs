#![allow(dead_code)]

use crate::script_step::{
    StepAnnotation, StepAnnotationLevel, StepCommandState, StepCommandTelemetry,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowCommand<'a> {
    name: &'a str,
    properties: BTreeMap<String, String>,
    value: String,
}

/// Commands `ActionCommandManager.Initialize` registers (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs): the stop command plus every
/// `IActionCommandExtension` except `internal-set-repo-path`, which is only
/// registered while a plugin enables it. A line naming anything else is not a
/// command at all — `ActionCommand.TryParseV2` refuses it — so it stays in the
/// log untouched.
const REGISTERED_COMMANDS: &[&str] = &[
    "stop-commands",
    "set-env",
    "set-output",
    "save-state",
    "add-mask",
    "add-path",
    "add-matcher",
    "remove-matcher",
    "debug",
    "warning",
    "error",
    "notice",
    "group",
    "endgroup",
    "echo",
];

/// `Constants.Runner.UnsupportedStopCommandTokenDisabled` (@397b032,
/// src/Runner.Common/Constants.cs).
const UNSUPPORTED_STOP_COMMAND_TOKEN_DISABLED: &str = "You cannot use a endToken that is an empty string, the string 'pause-logging', or another workflow command. For more information see: https://docs.github.com/actions/learn-github-actions/workflow-commands-for-github-actions#example-stopping-and-starting-workflow-commands or opt into insecure command execution by setting the `ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS` environment variable to `true`.";

/// `Constants.Variables.Actions.AllowUnsupportedStopCommandTokens`.
const ALLOW_UNSECURE_STOP_COMMAND_TOKENS: &str = "ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS";

/// `Constants.Variables.Actions.AllowUnsupportedCommands`.
const ALLOW_UNSECURE_COMMANDS: &str = "ACTIONS_ALLOW_UNSECURE_COMMANDS";

/// Names `SetEnvCommandExtension._setEnvBlockList` refuses (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs).
const SET_ENV_BLOCK_LIST: &[&str] = &["NODE_OPTIONS"];

/// `Constants.Runner.UnsupportedCommandMessageDisabled` (@397b032,
/// src/Runner.Common/Constants.cs), formatted with the command name.
fn unsupported_command_message_disabled(command: &str) -> String {
    format!(
        "The `{command}` command is disabled. Please upgrade to using Environment Files or opt into unsecure command execution by setting the `ACTIONS_ALLOW_UNSECURE_COMMANDS` environment variable to `true`. For more information see: https://github.blog/changelog/2020-10-01-github-actions-deprecating-set-env-and-add-path-commands/"
    )
}

/// `Constants.Runner.UnsupportedCommandMessage` (@397b032,
/// src/Runner.Common/Constants.cs), formatted with the command name: the
/// deprecation warning `SetOutputCommandExtension` and
/// `SaveStateCommandExtension` attach to every use.
fn unsupported_command_message(command: &str) -> String {
    format!(
        "The `{command}` command is deprecated and will be disabled soon. Please upgrade to using Environment Files. For more information see: https://github.blog/changelog/2022-10-11-github-actions-deprecating-save-state-and-set-output-commands/"
    )
}

/// Runner-wide opt-ins the command processors consult.
///
/// Upstream reads each of these from the runner process environment first and
/// then from the job `env` context (`SetEnvCommandExtension`,
/// `AddPathCommandExtension`, and `ValidateStopToken` all follow the same
/// process-then-`env`-context order). Both halves are modelled here: the
/// caller passes the step's effective environment — job env, step env, and
/// dynamic entries — as the `env`-context half.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CommandPolicy {
    allow_unsecure_commands: bool,
    allow_unsecure_stop_command_tokens: bool,
}

impl CommandPolicy {
    fn from_runner_env() -> Self {
        Self::from_job_env(&[])
    }

    /// Mirror the upstream lookup order exactly: the process environment
    /// wins, and the job env context is only consulted when the process
    /// value did not already opt in. The lookup is an exact-name match —
    /// upstream reads the `env` context through
    /// `CaseSensitiveDictionaryContextData` off Windows — and a repeated
    /// name resolves last-wins, matching process-environment semantics.
    fn from_job_env(job_env: &[(String, String)]) -> Self {
        let job_value = |name: &str| {
            job_env
                .iter()
                .rfind(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let allow_unsecure_commands =
            try_parse_bool(std::env::var(ALLOW_UNSECURE_COMMANDS).ok().as_deref())
                || try_parse_bool(job_value(ALLOW_UNSECURE_COMMANDS));
        let allow_unsecure_stop_command_tokens =
            convert_to_boolean(
                std::env::var(ALLOW_UNSECURE_STOP_COMMAND_TOKENS)
                    .ok()
                    .as_deref(),
            ) || convert_to_boolean(job_value(ALLOW_UNSECURE_STOP_COMMAND_TOKENS));
        Self {
            allow_unsecure_commands,
            allow_unsecure_stop_command_tokens,
        }
    }
}

/// `bool.TryParse`, which upstream uses for the `set-env` / `add-path` opt-in.
fn try_parse_bool(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// `StringUtil.ConvertToBoolean` (@397b032, src/Runner.Sdk/Util/StringUtil.cs).
fn convert_to_boolean(value: Option<&str>) -> bool {
    matches!(
        value.map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "$true")
    )
}

/// Job-global deprecated-command telemetry flags:
/// `Global.HasDeprecatedSetOutput` / `Global.HasDeprecatedSaveState`
/// (`IGlobalContext`, src/Runner.Sdk). Upstream publishes each
/// `DeprecatedCommand` telemetry entry once per JOB, not once per step, so
/// the job engine owns one of these and threads it through every full
/// step-output parse: N steps using `set-output` emit one `ActionCommand`
/// entry while the deprecation warning still fires per use. Streaming
/// mask-only parses take a throwaway: their telemetry is discarded by
/// design, so they must not consume the job's once-flags.
#[derive(Debug, Default)]
pub struct DeprecatedCommandScope {
    set_output: bool,
    save_state: bool,
}

pub fn parse_workflow_commands(
    output: &str,
    scope: &mut DeprecatedCommandScope,
) -> StepCommandState {
    parse_workflow_commands_with_policy(output, CommandPolicy::from_runner_env(), scope)
}

/// Parse step output with the step's effective environment supplying the
/// job-`env`-context half of the unsecure opt-ins, exactly as upstream
/// consults `context.ExpressionValues["env"]` after the process environment.
pub fn parse_workflow_commands_with_job_env(
    output: &str,
    job_env: &[(String, String)],
    scope: &mut DeprecatedCommandScope,
) -> StepCommandState {
    parse_workflow_commands_with_policy(output, CommandPolicy::from_job_env(job_env), scope)
}

fn parse_workflow_commands_with_policy(
    output: &str,
    policy: CommandPolicy,
    scope: &mut DeprecatedCommandScope,
) -> StepCommandState {
    let mut state = StepCommandState::default();
    let mut stopped_token = None::<String>;
    for line in output.lines() {
        let Some(command) = parse_workflow_command(line, stopped_token.as_deref()) else {
            continue;
        };
        if let Some(token) = stopped_token.as_deref() {
            // While stopped only the resume token is honored, and it resumes on
            // the command name alone — properties and data are ignored.
            if command.name.eq_ignore_ascii_case(token) {
                stopped_token = None;
            }
            continue;
        }
        let command_name = command.name.to_ascii_lowercase();
        match command_name.as_str() {
            "stop-commands" => {
                process_stop_commands(&mut state, command.value, policy, &mut stopped_token);
            }
            "set-output" => {
                process_set_output(&mut state, &mut scope.set_output, line, &command);
            }
            "set-env" => process_set_env(&mut state, line, &command, policy),
            "add-path" => process_add_path(&mut state, line, command.value, policy),
            "save-state" => {
                process_save_state(&mut state, &mut scope.save_state, line, &command);
            }
            "add-mask" => process_add_mask(&mut state, command.value),
            "add-matcher" | "remove-matcher" => {
                reject_problem_matcher(&mut state, line, &command_name);
            }
            // group/endgroup/debug render in place via
            // executor::rendered_output_line — no state to record here.
            "error" => {
                state.error_count += 1;
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Failure, &command));
            }
            "warning" => {
                state.warning_count += 1;
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Warning, &command));
            }
            "notice" => {
                state.notice_count += 1;
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Notice, &command));
            }
            _ => {}
        }
    }
    state
}

/// `ActionCommandManager.ValidateStopToken` plus the stop branch of
/// `TryProcessCommand` (@397b032, src/Runner.Worker/ActionCommandManager.cs).
///
/// An empty token, `pause-logging`, or the name of a registered command is
/// refused: upstream throws before `_stopProcessCommand` is set, so command
/// processing keeps running and a single attacker-controlled log line cannot
/// silence it. A token longer than six characters is itself a secret and gets
/// masked.
fn process_stop_commands(
    state: &mut StepCommandState,
    token: String,
    policy: CommandPolicy,
    stopped_token: &mut Option<String>,
) {
    let token_invalid = token.is_empty()
        || is_registered_command(&token)
        || token.eq_ignore_ascii_case("pause-logging");
    if token_invalid {
        state.telemetry.push(StepCommandTelemetry {
            message: format!("Invoked ::stopCommand:: with token: [{token}]"),
            kind: "ActionCommand".to_string(),
        });
        if !policy.allow_unsecure_stop_command_tokens {
            push_error(state, UNSUPPORTED_STOP_COMMAND_TOKEN_DISABLED.to_string());
            return;
        }
    }
    if token.encode_utf16().count() > 6 {
        state.masks.push(token.clone());
    }
    *stopped_token = Some(token);
}

/// `SetEnvCommandExtension.ProcessCommand` (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs).
///
/// `::set-env::` was disabled by CVE-2020-15228: unless the runner opts back in
/// with `ACTIONS_ALLOW_UNSECURE_COMMANDS`, the command throws, which the command
/// manager turns into two step errors and a failed command result.
fn process_set_env(
    state: &mut StepCommandState,
    line: &str,
    command: &WorkflowCommand<'_>,
    policy: CommandPolicy,
) {
    if !policy.allow_unsecure_commands {
        push_command_failure(state, line, unsupported_command_message_disabled("set-env"));
        return;
    }
    let Some(name) = command
        .properties
        .get("name")
        .filter(|name| !name.is_empty())
    else {
        push_command_failure(
            state,
            line,
            "Required field 'name' is missing in ##[set-env] command.".to_string(),
        );
        return;
    };
    if let Some(blocked) = blocked_set_env_name(name) {
        push_error(
            state,
            format!("Can't update {blocked} environment variable using ::set-env:: command."),
        );
        return;
    }
    state.env.insert(name.clone(), command.value.clone());
}

/// `AddPathCommandExtension.ProcessCommand` (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs): disabled by the same CVE, and
/// when opted back in the prepended path is de-duplicated last-write-wins.
fn process_add_path(
    state: &mut StepCommandState,
    line: &str,
    value: String,
    policy: CommandPolicy,
) {
    if !policy.allow_unsecure_commands {
        push_command_failure(
            state,
            line,
            unsupported_command_message_disabled("add-path"),
        );
        return;
    }
    if value.is_empty() {
        // ArgUtil.NotNullOrEmpty(command.Data, "path") throws ArgumentNullException.
        push_command_failure(
            state,
            line,
            "Value cannot be null. (Parameter 'path')".to_string(),
        );
        return;
    }
    state.path.retain(|path| path != &value);
    state.path.push(value);
}

/// `SetOutputCommandExtension.ProcessCommand` (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs).
///
/// The stdout command is deprecated but still honored — GitHub postponed the
/// removal (2023-07-24 changelog) and hosted runners warn-and-honor to this
/// day — so the output is stored exactly like upstream, with the deprecation
/// warning on every use and the telemetry once per job scope. Upstream gates
/// the warning on the server variable `DistributedTask.DeprecateStepOutputCommands`,
/// which Velnor has no channel for; the flag is on for github.com (warnings
/// observed on hosted runners), so warning unconditionally is hosted parity.
/// A missing or empty `name` throws upstream, which the command manager
/// turns into two step errors and a failed command result.
fn process_set_output(
    state: &mut StepCommandState,
    telemetry_emitted: &mut bool,
    line: &str,
    command: &WorkflowCommand<'_>,
) {
    push_warning(state, unsupported_command_message("set-output"));
    record_deprecated_command_telemetry(state, telemetry_emitted, "set-output");
    let Some(name) = command
        .properties
        .get("name")
        .filter(|name| !name.is_empty())
    else {
        push_command_failure(
            state,
            line,
            "Required field 'name' is missing in ##[set-output] command.".to_string(),
        );
        return;
    };
    state.outputs.insert(name.clone(), command.value.clone());
}

/// `SaveStateCommandExtension.ProcessCommand` (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs): the same deprecated-but-honored
/// shape as `set-output`, storing intra-action state instead of outputs.
fn process_save_state(
    state: &mut StepCommandState,
    telemetry_emitted: &mut bool,
    line: &str,
    command: &WorkflowCommand<'_>,
) {
    push_warning(state, unsupported_command_message("save-state"));
    record_deprecated_command_telemetry(state, telemetry_emitted, "save-state");
    let Some(name) = command
        .properties
        .get("name")
        .filter(|name| !name.is_empty())
    else {
        push_command_failure(
            state,
            line,
            "Required field 'name' is missing in ##[save-state] command.".to_string(),
        );
        return;
    };
    state.state.insert(name.clone(), command.value.clone());
}

/// Problem matchers (`AddMatcherCommandExtension` /
/// `RemoveMatcherCommandExtension` upstream) need a log-scanning regex engine
/// the runner does not have, so the commands cannot be honored — but silently
/// dropping a registered command is worse than refusing it: a workflow that
/// relies on matchers would pass with no annotations and nothing in the log
/// to say why. Reject loudly instead, in the same two-error shape the
/// command manager gives every other refused command.
fn reject_problem_matcher(state: &mut StepCommandState, line: &str, command: &str) {
    push_command_failure(
        state,
        line,
        format!(
            "The `{command}` command is not supported by this runner: problem matchers are not implemented."
        ),
    );
}

fn blocked_set_env_name(name: &str) -> Option<&'static str> {
    SET_ENV_BLOCK_LIST
        .iter()
        .find(|blocked| blocked.eq_ignore_ascii_case(name))
        .copied()
}

/// The `catch` around `extension.ProcessCommand` in
/// `ActionCommandManager.TryProcessCommand`: the failing line is reported, then
/// the exception message, and the step's command result becomes `Failed`.
fn push_command_failure(state: &mut StepCommandState, line: &str, message: String) {
    push_error(
        state,
        format!("Unable to process command '{line}' successfully."),
    );
    push_error(state, message);
}

fn is_registered_command(name: &str) -> bool {
    REGISTERED_COMMANDS
        .iter()
        .any(|command| command.eq_ignore_ascii_case(name))
}

fn push_error(state: &mut StepCommandState, message: String) {
    state.error_count += 1;
    state.annotations.push(StepAnnotation {
        level: StepAnnotationLevel::Failure,
        message,
        title: None,
        path: None,
        start_line: None,
        end_line: None,
        start_column: None,
        end_column: None,
    });
}

/// AddMaskCommandExtension.ProcessCommand (@397b032,
/// src/Runner.Worker/ActionCommandManager.cs): an all-whitespace value warns
/// instead of masking, and a multi-line value registers the whole value plus
/// every non-empty trimmed line, because child-process output reaches the log
/// one line at a time.
fn process_add_mask(state: &mut StepCommandState, value: String) {
    if value.trim().is_empty() {
        push_warning(
            state,
            "Can't add secret mask for empty string in ##[add-mask] command.".to_string(),
        );
        return;
    }
    for mask in std::iter::once(value.as_str()).chain(
        value
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|line| !line.is_empty()),
    ) {
        if !state.masks.iter().any(|known| known == mask) {
            state.masks.push(mask.to_string());
        }
    }
}

fn push_warning(state: &mut StepCommandState, message: String) {
    state.warning_count += 1;
    state.annotations.push(StepAnnotation {
        level: StepAnnotationLevel::Warning,
        message,
        title: None,
        path: None,
        start_line: None,
        end_line: None,
        start_column: None,
        end_column: None,
    });
}

fn record_deprecated_command_telemetry(
    state: &mut StepCommandState,
    telemetry_emitted: &mut bool,
    command: &str,
) {
    if std::mem::replace(telemetry_emitted, true) {
        return;
    }
    state.telemetry.push(StepCommandTelemetry {
        message: format!("DeprecatedCommand: {command}"),
        kind: "ActionCommand".to_string(),
    });
}

fn command_annotation(level: StepAnnotationLevel, command: &WorkflowCommand<'_>) -> StepAnnotation {
    let start_line = annotation_number(&command.properties, "line");
    StepAnnotation {
        level,
        message: command.value.clone(),
        title: annotation_string(&command.properties, "title"),
        path: annotation_string(&command.properties, "file"),
        start_line,
        end_line: annotation_number(&command.properties, "endLine")
            .or_else(|| annotation_number(&command.properties, "end_line"))
            .or(start_line),
        start_column: annotation_number(&command.properties, "col"),
        end_column: annotation_number(&command.properties, "endColumn")
            .or_else(|| annotation_number(&command.properties, "end_column"))
            .or_else(|| annotation_number(&command.properties, "col")),
    }
}

fn annotation_string(properties: &BTreeMap<String, String>, key: &str) -> Option<String> {
    properties
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn annotation_number(properties: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    properties.get(key)?.parse().ok()
}

fn format_annotation(kind: &str, command: &WorkflowCommand<'_>) -> String {
    let mut line = format!("{}: {}", title_case(kind), command.value);
    if let Some(title) = command
        .properties
        .get("title")
        .filter(|value| !value.is_empty())
    {
        line.push_str(&format!(" [{title}]"));
    }
    let location = annotation_location(&command.properties);
    if !location.is_empty() {
        line.push_str(&format!(" ({location})"));
    }
    line
}

fn annotation_location(properties: &BTreeMap<String, String>) -> String {
    let mut location = Vec::new();
    if let Some(file) = properties.get("file").filter(|value| !value.is_empty()) {
        location.push(file.clone());
    }
    if let Some(line) = properties.get("line").filter(|value| !value.is_empty()) {
        location.push(format!("line {line}"));
    }
    if let Some(column) = properties.get("col").filter(|value| !value.is_empty()) {
        location.push(format!("col {column}"));
    }
    location.join(":")
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn parse_workflow_command<'a>(
    line: &'a str,
    stopped_token: Option<&str>,
) -> Option<WorkflowCommand<'a>> {
    // ActionCommand.TryParseV2 (@397b032, src/Runner.Common/ActionCommand.cs):
    // `message = message.TrimStart()` before the `::` prefix test, so an
    // indented `  ::add-mask::secret` is still a command. Without the trim the
    // mask is never registered and the secret is printed verbatim.
    let line = line.trim_start().strip_prefix("::")?;
    let (header, value) = line.split_once("::")?;
    let (name, properties) = header
        .split_once(' ')
        .map(|(name, properties)| (name, parse_properties(properties)))
        .unwrap_or_else(|| (header, BTreeMap::new()));
    // TryParseV2 only yields a command whose name is registered. While a stop
    // token is active the runner registers that token too, which is how the
    // resume line is recognised.
    if !is_registered_command(name)
        && !stopped_token.is_some_and(|token| token.eq_ignore_ascii_case(name))
    {
        return None;
    }
    Some(WorkflowCommand {
        name,
        properties,
        value: unescape_data(value),
    })
}

fn parse_properties(properties: &str) -> BTreeMap<String, String> {
    properties
        .split(',')
        .filter_map(|property| {
            let (name, value) = property.split_once('=')?;
            Some((name.to_string(), unescape_property(value)))
        })
        .collect()
}

fn unescape_data(value: &str) -> String {
    value
        .replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%25", "%")
}

fn unescape_property(value: &str) -> String {
    unescape_data(&value.replace("%3A", ":").replace("%2C", ","))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-parse shorthand: a fresh job scope per call, so one output's
    /// assertions never inherit another parse's once-flags. Tests that pin
    /// the job-scope dedupe itself pass an explicit shared scope instead.
    fn parse_for_test(output: &str) -> StepCommandState {
        parse_workflow_commands(output, &mut DeprecatedCommandScope::default())
    }

    #[test]
    fn parses_state_changing_workflow_commands() {
        let state = parse_for_test(
            "::set-output name=answer::42\n\
             ::save-state name=cleanup::yes\n\
             ::add-mask::top-secret\n\
             ::error file=src/main.rs,line=7::broken\n\
             ::warning::careful\n\
             ::notice::noted\n",
        );

        assert_eq!(state.outputs["answer"], "42");
        assert!(state.env.is_empty());
        assert!(state.path.is_empty());
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.masks, vec!["top-secret"]);
        assert_eq!(state.error_count, 1);
        assert_eq!(state.warning_count, 3);
        assert_eq!(state.notice_count, 1);
        assert_eq!(state.annotations.len(), 5);
        assert_eq!(
            state.telemetry,
            vec![
                StepCommandTelemetry {
                    message: "DeprecatedCommand: set-output".to_string(),
                    kind: "ActionCommand".to_string(),
                },
                StepCommandTelemetry {
                    message: "DeprecatedCommand: save-state".to_string(),
                    kind: "ActionCommand".to_string(),
                }
            ]
        );
        assert_eq!(state.annotations[2].level, StepAnnotationLevel::Failure);
        assert_eq!(state.annotations[2].message, "broken");
        assert_eq!(state.annotations[2].path.as_deref(), Some("src/main.rs"));
        assert_eq!(state.annotations[2].start_line, Some(7));
        assert_eq!(state.annotations[2].end_line, Some(7));
    }

    #[test]
    fn unescapes_command_data_and_properties() {
        let state = parse_for_test("::set-output name=one%2Ctwo::a%0Ab%25c\n");

        assert_eq!(state.outputs["one,two"], "a\nb%c");
    }

    #[test]
    fn honors_workflow_commands_with_leading_whitespace() {
        let state = parse_for_test(
            "  ::add-mask::indented-secret\n\
             \t::set-output name=answer::42\n",
        );

        assert_eq!(state.masks, vec!["indented-secret"]);
        assert_eq!(state.outputs["answer"], "42");
    }

    #[test]
    fn masks_every_line_of_a_multi_line_secret() {
        let state = parse_for_test(
            "::add-mask::-----BEGIN KEY-----%0Aline-one%0A  line-two  %0A%0A-----END KEY-----\n",
        );

        assert_eq!(
            state.masks,
            vec![
                "-----BEGIN KEY-----\nline-one\n  line-two  \n\n-----END KEY-----",
                "-----BEGIN KEY-----",
                "line-one",
                "line-two",
                "-----END KEY-----",
            ]
        );
    }

    #[test]
    fn warns_instead_of_masking_a_blank_add_mask_value() {
        let state = parse_for_test("::add-mask::   \n");

        assert!(state.masks.is_empty());
        assert_eq!(state.warning_count, 1);
        assert_eq!(
            state.annotations[0].message,
            "Can't add secret mask for empty string in ##[add-mask] command."
        );
    }

    #[test]
    fn ignores_commands_between_stop_and_resume_token() {
        let state = parse_for_test(
            "::stop-commands::pause\n\
             ::set-output name=ignored::nope\n\
             ::error::ignored\n\
             ::pause::\n\
             ::set-output name=answer::42\n\
             ::warning::careful\n",
        );

        assert!(!state.outputs.contains_key("ignored"));
        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.error_count, 0);
        assert_eq!(state.warning_count, 2);
        assert_eq!(state.telemetry.len(), 1);
    }

    #[test]
    fn refuses_set_env_and_add_path_after_cve_2020_15228() {
        let state = parse_for_test(
            "::set-env name=MODE::release\n\
             ::add-path::/opt/tool\n",
        );

        assert!(state.env.is_empty());
        assert!(state.path.is_empty());
        assert_eq!(state.error_count, 4);
        assert_eq!(
            state
                .annotations
                .iter()
                .map(|annotation| annotation.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Unable to process command '::set-env name=MODE::release' successfully.",
                "The `set-env` command is disabled. Please upgrade to using Environment Files or opt into unsecure command execution by setting the `ACTIONS_ALLOW_UNSECURE_COMMANDS` environment variable to `true`. For more information see: https://github.blog/changelog/2020-10-01-github-actions-deprecating-set-env-and-add-path-commands/",
                "Unable to process command '::add-path::/opt/tool' successfully.",
                "The `add-path` command is disabled. Please upgrade to using Environment Files or opt into unsecure command execution by setting the `ACTIONS_ALLOW_UNSECURE_COMMANDS` environment variable to `true`. For more information see: https://github.blog/changelog/2020-10-01-github-actions-deprecating-set-env-and-add-path-commands/",
            ]
        );
        assert!(state
            .annotations
            .iter()
            .all(|annotation| annotation.level == StepAnnotationLevel::Failure));
    }

    #[test]
    fn applies_set_env_and_add_path_when_opted_in() {
        let policy = CommandPolicy {
            allow_unsecure_commands: true,
            allow_unsecure_stop_command_tokens: false,
        };
        let state = parse_workflow_commands_with_policy(
            "::set-env name=MODE::release\n\
             ::set-env name=GITHUB_REF::rewritten\n\
             ::set-env name=node_options::--require bad\n\
             ::set-env::nameless\n\
             ::add-path::/opt/tool\n\
             ::add-path::/opt/other\n\
             ::add-path::/opt/tool\n",
            policy,
            &mut DeprecatedCommandScope::default(),
        );

        assert_eq!(state.env["MODE"], "release");
        assert_eq!(state.env["GITHUB_REF"], "rewritten");
        assert!(!state.env.contains_key("node_options"));
        assert_eq!(state.path, vec!["/opt/other", "/opt/tool"]);
        assert_eq!(
            state
                .annotations
                .iter()
                .map(|annotation| annotation.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Can't update NODE_OPTIONS environment variable using ::set-env:: command.",
                "Unable to process command '::set-env::nameless' successfully.",
                "Required field 'name' is missing in ##[set-env] command.",
            ]
        );
    }

    #[test]
    fn refuses_invalid_stop_command_tokens_and_keeps_processing() {
        for token in ["", "stop-commands", "SET-ENV", "pause-logging"] {
            let state = parse_for_test(&format!(
                "::stop-commands::{token}\n::add-mask::still-secret\n"
            ));

            assert_eq!(
                state.masks,
                vec!["still-secret"],
                "token {token:?} disabled command processing"
            );
            assert_eq!(state.error_count, 1, "token {token:?}");
            assert_eq!(
                state.annotations[0].message,
                UNSUPPORTED_STOP_COMMAND_TOKEN_DISABLED
            );
            assert_eq!(
                state.telemetry,
                vec![StepCommandTelemetry {
                    message: format!("Invoked ::stopCommand:: with token: [{token}]"),
                    kind: "ActionCommand".to_string(),
                }]
            );
        }
    }

    #[test]
    fn honors_invalid_stop_command_tokens_when_opted_in() {
        let state = parse_workflow_commands_with_policy(
            "::stop-commands::pause-logging\n::add-mask::hidden\n::pause-logging::\n\
             ::add-mask::seen\n",
            CommandPolicy {
                allow_unsecure_commands: false,
                allow_unsecure_stop_command_tokens: true,
            },
            &mut DeprecatedCommandScope::default(),
        );

        assert_eq!(state.masks, vec!["pause-logging", "seen"]);
        assert_eq!(state.error_count, 0);
        assert_eq!(state.telemetry.len(), 1);
    }

    #[test]
    fn masks_stop_command_tokens_longer_than_six_characters() {
        let state = parse_for_test("::stop-commands::randomToken\n");

        assert_eq!(state.masks, vec!["randomToken"]);
        assert_eq!(state.error_count, 0);
        assert!(state.telemetry.is_empty());

        let short = parse_for_test("::stop-commands::short1\n");

        assert!(short.masks.is_empty());
        assert_eq!(short.error_count, 0);
    }

    #[test]
    fn resumes_on_the_token_command_name_alone() {
        let state = parse_for_test(
            "::stop-commands::myToken\n\
             ::set-output name=ignored::nope\n\
             \t::MYTOKEN prop=1::trailing data\n\
             ::set-output name=answer::42\n",
        );

        assert!(!state.outputs.contains_key("ignored"));
        assert_eq!(state.outputs["answer"], "42");
    }

    #[test]
    fn emits_deprecated_command_telemetry_once_per_job_scope() {
        let mut scope = DeprecatedCommandScope::default();
        let first = parse_workflow_commands(
            "::set-output name=one::1\n\
             ::set-output name=two::2\n\
             ::save-state name=cleanup::yes\n\
             ::save-state name=cleanup2::yes\n",
            &mut scope,
        );

        assert_eq!(
            first.telemetry,
            vec![
                StepCommandTelemetry {
                    message: "DeprecatedCommand: set-output".to_string(),
                    kind: "ActionCommand".to_string(),
                },
                StepCommandTelemetry {
                    message: "DeprecatedCommand: save-state".to_string(),
                    kind: "ActionCommand".to_string(),
                }
            ]
        );

        // A later step sharing the job scope honors the commands and warns
        // per use, but emits no further telemetry — upstream's
        // `Global.HasDeprecatedSetOutput` / `HasDeprecatedSaveState` gate.
        let second = parse_workflow_commands(
            "::set-output name=three::3\n::save-state name=later::yes\n",
            &mut scope,
        );

        assert_eq!(second.outputs["three"], "3");
        assert_eq!(second.state["later"], "yes");
        assert_eq!(second.warning_count, 2);
        assert!(second.telemetry.is_empty());
    }

    #[test]
    fn preserves_annotation_titles_and_group_boundaries() {
        let state = parse_for_test(
            "::group::Build\n\
             ::notice title=sccache stats::hit rate 80%25\n\
             ::debug::resolved key\n\
             ::endgroup::\n",
        );

        assert_eq!(state.notice_count, 1);
        assert_eq!(state.annotations[0].title.as_deref(), Some("sccache stats"));
    }

    #[test]
    fn warns_on_every_deprecated_output_command_use() {
        let state = parse_for_test(
            "::set-output name=one::1\n\
             ::set-output name=two::2\n\
             ::save-state name=cleanup::yes\n",
        );

        assert_eq!(state.outputs["one"], "1");
        assert_eq!(state.outputs["two"], "2");
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.error_count, 0);
        assert_eq!(state.warning_count, 3);
        assert_eq!(
            state
                .annotations
                .iter()
                .map(|annotation| annotation.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "The `set-output` command is deprecated and will be disabled soon. Please upgrade to using Environment Files. For more information see: https://github.blog/changelog/2022-10-11-github-actions-deprecating-save-state-and-set-output-commands/",
                "The `set-output` command is deprecated and will be disabled soon. Please upgrade to using Environment Files. For more information see: https://github.blog/changelog/2022-10-11-github-actions-deprecating-save-state-and-set-output-commands/",
                "The `save-state` command is deprecated and will be disabled soon. Please upgrade to using Environment Files. For more information see: https://github.blog/changelog/2022-10-11-github-actions-deprecating-save-state-and-set-output-commands/",
            ]
        );
        // The warning fires per use; the telemetry stays once per command
        // within this fresh job scope.
        assert_eq!(state.telemetry.len(), 2);
    }

    #[test]
    fn refuses_nameless_set_output_and_save_state() {
        let state = parse_for_test(
            "::set-output::nameless\n\
             ::set-output name=::empty\n\
             ::save-state::nameless\n",
        );

        assert!(state.outputs.is_empty());
        assert!(state.state.is_empty());
        // The deprecation warning still fires before the name check, exactly
        // like upstream: warn, telemetry, then the throw.
        assert_eq!(state.warning_count, 3);
        assert_eq!(state.error_count, 6);
        assert_eq!(
            state
                .annotations
                .iter()
                .filter(|annotation| annotation.level == StepAnnotationLevel::Failure)
                .map(|annotation| annotation.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Unable to process command '::set-output::nameless' successfully.",
                "Required field 'name' is missing in ##[set-output] command.",
                "Unable to process command '::set-output name=::empty' successfully.",
                "Required field 'name' is missing in ##[set-output] command.",
                "Unable to process command '::save-state::nameless' successfully.",
                "Required field 'name' is missing in ##[save-state] command.",
            ]
        );
    }

    #[test]
    fn rejects_problem_matcher_commands_loudly() {
        let state = parse_for_test(
            "::add-matcher::/github/workspace/matchers.json\n\
             ::remove-matcher owner=tsc::\n\
             ::remove-matcher::/github/workspace/matchers.json\n",
        );

        assert_eq!(state.error_count, 6);
        assert_eq!(state.warning_count, 0);
        assert_eq!(
            state
                .annotations
                .iter()
                .map(|annotation| annotation.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Unable to process command '::add-matcher::/github/workspace/matchers.json' successfully.",
                "The `add-matcher` command is not supported by this runner: problem matchers are not implemented.",
                "Unable to process command '::remove-matcher owner=tsc::' successfully.",
                "The `remove-matcher` command is not supported by this runner: problem matchers are not implemented.",
                "Unable to process command '::remove-matcher::/github/workspace/matchers.json' successfully.",
                "The `remove-matcher` command is not supported by this runner: problem matchers are not implemented.",
            ]
        );
        assert!(state
            .annotations
            .iter()
            .all(|annotation| annotation.level == StepAnnotationLevel::Failure));
    }

    #[test]
    fn honors_unsecure_opt_ins_from_job_env() {
        let job_env = vec![(ALLOW_UNSECURE_COMMANDS.to_string(), "true".to_string())];
        let state = parse_workflow_commands_with_job_env(
            "::set-env name=MODE::release\n\
             ::add-path::/opt/tool\n",
            &job_env,
            &mut DeprecatedCommandScope::default(),
        );

        assert_eq!(state.env["MODE"], "release");
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.error_count, 0);

        let stop_env = vec![(
            ALLOW_UNSECURE_STOP_COMMAND_TOKENS.to_string(),
            "1".to_string(),
        )];
        let stopped = parse_workflow_commands_with_job_env(
            "::stop-commands::pause-logging\n::add-mask::hidden\n::pause-logging::\n\
             ::add-mask::seen\n",
            &stop_env,
            &mut DeprecatedCommandScope::default(),
        );

        assert_eq!(stopped.masks, vec!["pause-logging", "seen"]);
        assert_eq!(stopped.error_count, 0);
    }

    #[test]
    fn job_env_opt_in_requires_an_exact_true_name_and_value() {
        // A false-valued job entry does not opt in.
        let refused = parse_workflow_commands_with_job_env(
            "::set-env name=MODE::release\n",
            &[(
                "ACTIONS_ALLOW_UNSECURE_COMMANDS".to_string(),
                "false".to_string(),
            )],
            &mut DeprecatedCommandScope::default(),
        );
        assert!(refused.env.is_empty());
        assert_eq!(refused.error_count, 2);

        // The env-context lookup is case-sensitive off Windows: a
        // differently-cased name is not the opt-in.
        let miscased = parse_workflow_commands_with_job_env(
            "::set-env name=MODE::release\n",
            &[(
                "actions_allow_unsecure_commands".to_string(),
                "true".to_string(),
            )],
            &mut DeprecatedCommandScope::default(),
        );
        assert!(miscased.env.is_empty());
        assert_eq!(miscased.error_count, 2);

        // A repeated name resolves last-wins, matching process semantics.
        let shadowed = parse_workflow_commands_with_job_env(
            "::set-env name=MODE::release\n",
            &[
                (
                    "ACTIONS_ALLOW_UNSECURE_COMMANDS".to_string(),
                    "true".to_string(),
                ),
                (
                    "ACTIONS_ALLOW_UNSECURE_COMMANDS".to_string(),
                    "false".to_string(),
                ),
            ],
            &mut DeprecatedCommandScope::default(),
        );
        assert!(shadowed.env.is_empty());
        assert_eq!(shadowed.error_count, 2);
    }

    #[test]
    fn workflow_command_parse_benchmark() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        let line =
            "::set-output name=answer::42\n::add-mask::secret\n::warning::careful\nnot a command\n";
        let output = line.repeat(250);

        let started = Instant::now();
        for _ in 0..2_000 {
            let state = parse_for_test(black_box(&output));
            assert_eq!(state.outputs["answer"], "42");
            assert_eq!(state.warning_count, 500);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "2k parses of a 1k-line mixed output took {elapsed:?}",
        );
    }
}

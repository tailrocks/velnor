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

pub fn parse_workflow_commands(output: &str) -> StepCommandState {
    let mut state = StepCommandState::default();
    let mut stopped_token = None::<String>;
    for line in output.lines() {
        if let Some(token) = stopped_token.as_deref() {
            if line == format!("::{token}::") {
                stopped_token = None;
            }
            continue;
        }
        let Some(command) = parse_workflow_command(line) else {
            continue;
        };
        match command.name {
            "stop-commands" => {
                if !command.value.is_empty() {
                    stopped_token = Some(command.value);
                }
            }
            "set-output" => {
                if let Some(name) = command.properties.get("name") {
                    state.outputs.insert(name.clone(), command.value);
                }
                record_deprecated_command_telemetry(&mut state, "set-output");
            }
            "set-env" => {
                if let Some(name) = command.properties.get("name") {
                    state.set_env(name.clone(), command.value);
                }
            }
            "add-path" => state.path.push(command.value),
            "save-state" => {
                if let Some(name) = command.properties.get("name") {
                    state.state.insert(name.clone(), command.value);
                }
                record_deprecated_command_telemetry(&mut state, "save-state");
            }
            "add-mask" => {
                if !command.value.is_empty() {
                    state.masks.push(command.value);
                }
            }
            "error" => {
                state.error_count += 1;
                state.log_lines.push(format_annotation("error", &command));
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Failure, &command));
            }
            "warning" => {
                state.warning_count += 1;
                state.log_lines.push(format_annotation("warning", &command));
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Warning, &command));
            }
            "notice" => {
                state.notice_count += 1;
                state.log_lines.push(format_annotation("notice", &command));
                state
                    .annotations
                    .push(command_annotation(StepAnnotationLevel::Notice, &command));
            }
            "debug" => state.log_lines.push(format!("##[debug]{}", command.value)),
            "group" => state.log_lines.push(format!("##[group]{}", command.value)),
            "endgroup" => state.log_lines.push("##[endgroup]".to_string()),
            _ => {}
        }
    }
    state
}

fn record_deprecated_command_telemetry(state: &mut StepCommandState, command: &str) {
    let message = format!("DeprecatedCommand: {command}");
    if !state
        .telemetry
        .iter()
        .any(|telemetry| telemetry.kind == "ActionCommand" && telemetry.message == message)
    {
        state.telemetry.push(StepCommandTelemetry {
            message,
            kind: "ActionCommand".to_string(),
        });
    }
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

fn parse_workflow_command(line: &str) -> Option<WorkflowCommand<'_>> {
    let line = line.strip_prefix("::")?;
    let (header, value) = line.split_once("::")?;
    let (name, properties) = header
        .split_once(' ')
        .map(|(name, properties)| (name, parse_properties(properties)))
        .unwrap_or_else(|| (header, BTreeMap::new()));
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

    #[test]
    fn parses_state_changing_workflow_commands() {
        let state = parse_workflow_commands(
            "::set-output name=answer::42\n\
             ::set-env name=MODE::release\n\
             ::set-env name=GITHUB_REF::evil\n\
             ::set-env name=RUNNER_TEMP::/bad\n\
             ::set-env name=NODE_OPTIONS::--require bad\n\
             ::set-env name=ACTIONS_RUNTIME_URL::https://runtime\n\
             ::add-path::/opt/tool\n\
             ::save-state name=cleanup::yes\n\
             ::add-mask::top-secret\n\
             ::error file=src/main.rs,line=7::broken\n\
             ::warning::careful\n\
             ::notice::noted\n",
        );

        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.env["MODE"], "release");
        assert_eq!(state.env["ACTIONS_RUNTIME_URL"], "https://runtime");
        assert!(!state.env.contains_key("GITHUB_REF"));
        assert!(!state.env.contains_key("RUNNER_TEMP"));
        assert!(!state.env.contains_key("NODE_OPTIONS"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.masks, vec!["top-secret"]);
        assert_eq!(state.error_count, 1);
        assert_eq!(state.warning_count, 1);
        assert_eq!(state.notice_count, 1);
        assert_eq!(state.annotations.len(), 3);
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
        assert_eq!(state.annotations[0].level, StepAnnotationLevel::Failure);
        assert_eq!(state.annotations[0].message, "broken");
        assert_eq!(state.annotations[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(state.annotations[0].start_line, Some(7));
        assert_eq!(state.annotations[0].end_line, Some(7));
        assert_eq!(
            state.log_lines,
            vec![
                "Error: broken (src/main.rs:line 7)".to_string(),
                "Warning: careful".to_string(),
                "Notice: noted".to_string()
            ]
        );
    }

    #[test]
    fn unescapes_command_data_and_properties() {
        let state = parse_workflow_commands("::set-output name=one%2Ctwo::a%0Ab%25c\n");

        assert_eq!(state.outputs["one,two"], "a\nb%c");
    }

    #[test]
    fn ignores_commands_between_stop_and_resume_token() {
        let state = parse_workflow_commands(
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
        assert_eq!(state.warning_count, 1);
        assert_eq!(state.telemetry.len(), 1);
    }

    #[test]
    fn emits_deprecated_command_telemetry_once_per_parse() {
        let state = parse_workflow_commands(
            "::set-output name=one::1\n\
             ::set-output name=two::2\n\
             ::save-state name=cleanup::yes\n\
             ::save-state name=cleanup2::yes\n",
        );

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
    }

    #[test]
    fn preserves_annotation_titles_and_group_boundaries() {
        let state = parse_workflow_commands(
            "::group::Build\n\
             ::notice title=sccache stats::hit rate 80%25\n\
             ::debug::resolved key\n\
             ::endgroup::\n",
        );

        assert_eq!(
            state.log_lines,
            vec![
                "##[group]Build".to_string(),
                "Notice: hit rate 80% [sccache stats]".to_string(),
                "##[debug]resolved key".to_string(),
                "##[endgroup]".to_string(),
            ]
        );
        assert_eq!(state.notice_count, 1);
        assert_eq!(state.annotations[0].title.as_deref(), Some("sccache stats"));
    }
}

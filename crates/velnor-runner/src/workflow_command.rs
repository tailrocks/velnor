#![allow(dead_code)]

use crate::script_step::StepCommandState;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowCommand<'a> {
    name: &'a str,
    properties: BTreeMap<String, String>,
    value: String,
}

pub fn parse_workflow_commands(output: &str) -> StepCommandState {
    let mut state = StepCommandState::default();
    for line in output.lines() {
        let Some(command) = parse_workflow_command(line) else {
            continue;
        };
        match command.name {
            "set-output" => {
                if let Some(name) = command.properties.get("name") {
                    state.outputs.insert(name.clone(), command.value);
                }
            }
            "set-env" => {
                if let Some(name) = command.properties.get("name") {
                    state.env.insert(name.clone(), command.value);
                }
            }
            "add-path" => state.path.push(command.value),
            "save-state" => {
                if let Some(name) = command.properties.get("name") {
                    state.state.insert(name.clone(), command.value);
                }
            }
            "add-mask" => {
                if !command.value.is_empty() {
                    state.masks.push(command.value);
                }
            }
            _ => {}
        }
    }
    state
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
             ::add-path::/opt/tool\n\
             ::save-state name=cleanup::yes\n\
             ::add-mask::top-secret\n\
             ::warning::ignored\n",
        );

        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.env["MODE"], "release");
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.masks, vec!["top-secret"]);
    }

    #[test]
    fn unescapes_command_data_and_properties() {
        let state = parse_workflow_commands("::set-output name=one%2Ctwo::a%0Ab%25c\n");

        assert_eq!(state.outputs["one,two"], "a\nb%c");
    }
}

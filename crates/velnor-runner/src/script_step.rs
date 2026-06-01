#![allow(dead_code)]

use crate::{
    command_files::{parse_command_file, FileCommand},
    container::Shell,
    job_message::{ActionReferenceType, ActionStep},
};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ScriptStep {
    pub id: String,
    pub script: String,
    pub shell: Shell,
    pub working_directory_container: String,
    pub env: Vec<(String, String)>,
    pub condition: Option<String>,
    pub continue_on_error: bool,
}

pub fn github_script_steps(
    steps: &[ActionStep],
    workspace_container: &str,
) -> Result<Vec<ScriptStep>> {
    github_script_steps_with_defaults(steps, workspace_container, &[])
}

pub fn github_script_steps_with_defaults(
    steps: &[ActionStep],
    workspace_container: &str,
    defaults: &[Value],
) -> Result<Vec<ScriptStep>> {
    let defaults = RunDefaults::from_job_defaults(defaults)?;
    let mut script_steps = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if !step.enabled {
            continue;
        }
        if step.reference_type() != Some(ActionReferenceType::Script) {
            continue;
        }

        script_steps.push(github_script_step(
            step,
            index,
            workspace_container,
            &defaults,
        )?);
    }
    Ok(script_steps)
}

pub fn has_enabled_non_script_steps(steps: &[ActionStep]) -> bool {
    steps
        .iter()
        .any(|step| step.enabled && step.reference_type() != Some(ActionReferenceType::Script))
}

fn github_script_step(
    step: &ActionStep,
    index: usize,
    workspace_container: &str,
    defaults: &RunDefaults,
) -> Result<ScriptStep> {
    let inputs = step
        .inputs
        .as_ref()
        .and_then(|value| value.as_object())
        .ok_or_else(|| anyhow::anyhow!("script step {} missing inputs object", index + 1))?;
    let script = string_input_field(inputs, &["script", "Script"]).ok_or_else(|| {
        anyhow::anyhow!(
            "script step {} missing script input; input keys: {}",
            index + 1,
            input_summary(inputs)
        )
    })?;
    let shell = string_input_field(inputs, &["shell", "Shell"])
        .or(defaults.shell.as_deref())
        .map(github_shell)
        .transpose()?
        .unwrap_or(Shell::Bash);
    let working_directory = string_input_field(
        inputs,
        &[
            "workingDirectory",
            "working-directory",
            "WorkingDirectory",
            "Working-Directory",
        ],
    )
    .or(defaults.working_directory.as_deref())
    .map(|path| workspace_path(workspace_container, path))
    .unwrap_or_else(|| workspace_container.to_string());

    Ok(ScriptStep {
        id: step_id(step, index),
        script: script.to_string(),
        shell,
        working_directory_container: working_directory,
        env: step_environment(step)?,
        condition: step.condition.clone(),
        continue_on_error: step_continue_on_error(step),
    })
}

#[derive(Debug, Default)]
struct RunDefaults {
    shell: Option<String>,
    working_directory: Option<String>,
}

impl RunDefaults {
    fn from_job_defaults(defaults: &[Value]) -> Result<Self> {
        let mut run_defaults = Self::default();
        for value in defaults {
            run_defaults.merge_value(value)?;
        }
        Ok(run_defaults)
    }

    fn merge_value(&mut self, value: &Value) -> Result<()> {
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        let run = object
            .get("run")
            .or_else(|| object.get("Run"))
            .or_else(|| object.get("RUN"));
        if let Some(run) = run.and_then(Value::as_object) {
            if let Some(shell) = string_field(run, &["shell", "Shell"]) {
                github_shell(shell)?;
                self.shell = Some(shell.to_string());
            }
            if let Some(working_directory) = string_field(
                run,
                &[
                    "workingDirectory",
                    "working-directory",
                    "WorkingDirectory",
                    "Working-Directory",
                ],
            ) {
                self.working_directory = Some(working_directory.to_string());
            }
        }
        Ok(())
    }
}

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str))
}

fn string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    direct_string_input_field(object, names)
        .or_else(|| nested_map_string_input_field(object, names))
}

fn direct_string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| input_value_as_str(object.get(*name)?))
}

fn nested_map_string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    let map = object.get("map").or_else(|| object.get("Map"))?;
    if let Some(map) = map.as_object() {
        return string_input_field(map, names);
    }
    map.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            let item = item.as_object()?;
            let name = input_name_field(item)?;
            if !names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            item.get("value")
                .or_else(|| item.get("Value"))
                .and_then(input_value_as_str)
        })
    })
}

fn input_name_field(object: &serde_json::Map<String, Value>) -> Option<&str> {
    ["name", "Name", "key", "Key"]
        .iter()
        .find_map(|name| object.get(*name).and_then(input_value_as_str))
}

fn input_value_as_str(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_object()
            .and_then(|object| string_field(object, &["value", "Value", "lit", "Lit"]))
    })
}

fn input_keys(object: &serde_json::Map<String, Value>) -> String {
    if object.is_empty() {
        return "none".to_string();
    }
    object.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn input_summary(object: &serde_json::Map<String, Value>) -> String {
    let mut summary = format!("top=[{}]", input_keys(object));
    if let Some(map) = object.get("map").or_else(|| object.get("Map")) {
        if let Some(map) = map.as_object() {
            summary.push_str(&format!(" map=[{}]", input_keys(map)));
        } else if let Some(items) = map.as_array() {
            let shapes = items
                .iter()
                .take(3)
                .filter_map(|item| item.as_object())
                .map(input_item_summary)
                .collect::<Vec<_>>()
                .join(" | ");
            summary.push_str(&format!(
                " map_array_len={} map_shapes=[{}]",
                items.len(),
                shapes
            ));
        } else {
            summary.push_str(&format!(" map_type={}", json_type(map)));
        }
    }
    summary
}

fn input_item_summary(object: &serde_json::Map<String, Value>) -> String {
    let mut parts = vec![input_keys(object)];
    for name in ["Key", "key", "Value", "value"] {
        let Some(value) = object.get(name) else {
            continue;
        };
        if let Some(inner) = value.as_object() {
            parts.push(format!("{name}=[{}]", input_keys(inner)));
        } else {
            parts.push(format!("{name}={}", json_type(value)));
        }
    }
    parts.join(" ")
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn step_continue_on_error(step: &ActionStep) -> bool {
    match step.continue_on_error.as_ref() {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub(crate) fn step_environment(step: &ActionStep) -> Result<Vec<(String, String)>> {
    environment_pairs(step.environment.as_ref())
}

fn environment_pairs(environment: Option<&Value>) -> Result<Vec<(String, String)>> {
    let Some(environment) = environment else {
        return Ok(Vec::new());
    };
    match environment {
        Value::Object(object) => {
            if object.get("type").or_else(|| object.get("Type")).is_some()
                && object.get("map").or_else(|| object.get("Map")).is_some()
            {
                return environment_pairs(object.get("map").or_else(|| object.get("Map")));
            }
            Ok(object
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
                .map(|(name, value)| (name.clone(), environment_value(value)))
                .collect())
        }
        Value::Array(items) => Ok(items.iter().filter_map(environment_pair_value).collect()),
        _ => bail!("step environment must be an object"),
    }
}

fn environment_pair_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Object(object) => {
            let name = object
                .get("key")
                .or_else(|| object.get("Key"))
                .or_else(|| object.get("name"))
                .or_else(|| object.get("Name"))
                .and_then(input_value_as_str)?;
            let value = object
                .get("value")
                .or_else(|| object.get("Value"))
                .map(environment_value)
                .unwrap_or_default();
            Some((name.to_string(), value))
        }
        Value::Array(pair) if pair.len() == 2 => {
            let name = input_value_as_str(&pair[0])?;
            Some((name.to_string(), environment_value(&pair[1])))
        }
        _ => None,
    }
}

fn environment_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .map(environment_value)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn github_shell(shell: &str) -> Result<Shell> {
    let shell = shell.split_whitespace().next().unwrap_or(shell);
    if shell.eq_ignore_ascii_case("bash") {
        Ok(Shell::Bash)
    } else if shell.eq_ignore_ascii_case("sh") {
        Ok(Shell::Sh)
    } else {
        bail!("unsupported run step shell '{shell}'")
    }
}

fn workspace_path(workspace_container: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!(
            "{}/{}",
            workspace_container.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    }
}

fn step_id(step: &ActionStep, index: usize) -> String {
    step.id
        .as_deref()
        .or(step.context_name.as_deref())
        .or(step.name.as_deref())
        .map(sanitize_step_id)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("step{}", index + 1))
}

fn sanitize_step_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ScriptStepPlan {
    pub script_host_path: PathBuf,
    pub script_container_path: String,
    pub shell: Shell,
    pub working_directory_container: String,
    pub env: Vec<(String, String)>,
    command_files: CommandFileSet,
}

impl ScriptStepPlan {
    pub fn prepare(step: &ScriptStep, temp_host: &Path) -> Result<Self> {
        Self::prepare_with_path(step, temp_host, &[])
    }

    pub fn prepare_with_path(
        step: &ScriptStep,
        temp_host: &Path,
        path_prepend: &[String],
    ) -> Result<Self> {
        fs::create_dir_all(temp_host).with_context(|| format!("create {}", temp_host.display()))?;
        let script_name = format!("{}.sh", step.id);
        let script_host_path = temp_host.join(&script_name);
        let script = script_with_path_prelude(&step.script, path_prepend);
        fs::write(&script_host_path, script)
            .with_context(|| format!("write {}", script_host_path.display()))?;

        let command_files = CommandFileSet::new(&step.id, temp_host);
        command_files.create_empty_files()?;

        Ok(Self {
            script_host_path,
            script_container_path: format!("/__t/{script_name}"),
            shell: step.shell,
            working_directory_container: step.working_directory_container.clone(),
            env: command_files.env(),
            command_files,
        })
    }

    pub fn collect_state(&self) -> Result<StepCommandState> {
        self.command_files.collect_state()
    }
}

#[derive(Debug, Clone)]
struct CommandFileSet {
    output: PathMapping,
    env: PathMapping,
    path: PathMapping,
    state: PathMapping,
    summary: PathMapping,
}

impl CommandFileSet {
    fn new(step_id: &str, temp_host: &Path) -> Self {
        Self {
            output: PathMapping::new(temp_host, step_id, "output"),
            env: PathMapping::new(temp_host, step_id, "env"),
            path: PathMapping::new(temp_host, step_id, "path"),
            state: PathMapping::new(temp_host, step_id, "state"),
            summary: PathMapping::new(temp_host, step_id, "summary"),
        }
    }

    fn create_empty_files(&self) -> Result<()> {
        for path in [
            &self.output.host,
            &self.env.host,
            &self.path.host,
            &self.state.host,
            &self.summary.host,
        ] {
            fs::write(path, "").with_context(|| format!("create {}", path.display()))?;
        }
        Ok(())
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("GITHUB_OUTPUT".into(), self.output.container.clone()),
            ("GITHUB_ENV".into(), self.env.container.clone()),
            ("GITHUB_PATH".into(), self.path.container.clone()),
            ("GITHUB_STATE".into(), self.state.container.clone()),
            ("GITHUB_STEP_SUMMARY".into(), self.summary.container.clone()),
        ]
    }

    fn collect_state(&self) -> Result<StepCommandState> {
        Ok(StepCommandState {
            outputs: commands_to_map(parse_command_file(&self.output.host)?),
            env: env_commands_to_map(parse_command_file(&self.env.host)?),
            path: fs::read_to_string(&self.path.host)?
                .lines()
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            state: commands_to_map(parse_command_file(&self.state.host)?),
            summary: fs::read_to_string(&self.summary.host)?,
            masks: Vec::new(),
            log_lines: Vec::new(),
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
        })
    }
}

#[derive(Debug, Clone)]
struct PathMapping {
    host: PathBuf,
    container: String,
}

impl PathMapping {
    fn new(temp_host: &Path, step_id: &str, name: &str) -> Self {
        let file_name = format!("{step_id}_{name}");
        Self {
            host: temp_host.join(&file_name),
            container: format!("/__t/{file_name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepCommandState {
    pub outputs: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub path: Vec<String>,
    pub state: BTreeMap<String, String>,
    pub summary: String,
    pub masks: Vec<String>,
    pub log_lines: Vec<String>,
    pub error_count: i32,
    pub warning_count: i32,
    pub notice_count: i32,
}

impl StepCommandState {
    pub(crate) fn set_env(&mut self, name: String, value: String) {
        if !is_blocked_env_mutation(&name) {
            self.env.insert(name, value);
        }
    }

    pub fn merge(&mut self, other: StepCommandState) {
        self.outputs.extend(other.outputs);
        self.env.extend(other.env);
        self.path.extend(other.path);
        self.state.extend(other.state);
        self.masks.extend(other.masks);
        self.log_lines.extend(other.log_lines);
        self.error_count += other.error_count;
        self.warning_count += other.warning_count;
        self.notice_count += other.notice_count;
        if !other.summary.is_empty() {
            self.summary.push_str(&other.summary);
        }
    }
}

fn commands_to_map(commands: Vec<FileCommand>) -> BTreeMap<String, String> {
    commands
        .into_iter()
        .map(|command| (command.name, command.value))
        .collect()
}

fn env_commands_to_map(commands: Vec<FileCommand>) -> BTreeMap<String, String> {
    commands
        .into_iter()
        .filter(|command| !is_blocked_env_mutation(&command.name))
        .map(|command| (command.name, command.value))
        .collect()
}

fn is_blocked_env_mutation(name: &str) -> bool {
    name.starts_with("GITHUB_") || name.starts_with("RUNNER_")
}

fn fix_script(script: &str) -> String {
    let mut fixed = script.replace("\r\n", "\n");
    if !fixed.ends_with('\n') {
        fixed.push('\n');
    }
    fixed
}

fn script_with_path_prelude(script: &str, path_prepend: &[String]) -> String {
    let fixed = fix_script(script);
    if path_prepend.is_empty() {
        return fixed;
    }

    let joined = path_prepend
        .iter()
        .map(|path| shell_single_quote(path))
        .collect::<Vec<_>>()
        .join(":");
    format!("export PATH={joined}:\"$PATH\"\n{fixed}")
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_step_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-step-test-{nonce}"))
    }

    #[test]
    fn prepares_script_and_command_file_env() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            script: "echo hello".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };

        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        assert_eq!(
            fs::read_to_string(&plan.script_host_path).unwrap(),
            "echo hello\n"
        );
        assert!(plan
            .env
            .contains(&("GITHUB_OUTPUT".into(), "/__t/step1_output".into())));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn collects_command_file_state() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            script: "echo test".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        fs::write(
            temp.join("step1_output"),
            "answer=42\nmulti<<EOF\none\ntwo\nEOF\n",
        )
        .unwrap();
        fs::write(
            temp.join("step1_env"),
            "NAME=value\nGITHUB_REF=evil\nRUNNER_TEMP=/bad\nACTIONS_RUNTIME_URL=https://runtime\n",
        )
        .unwrap();
        fs::write(temp.join("step1_path"), "/opt/tool\n\n").unwrap();
        fs::write(temp.join("step1_state"), "cleanup=yes\n").unwrap();
        fs::write(temp.join("step1_summary"), "summary text").unwrap();

        let state = plan.collect_state().unwrap();

        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.outputs["multi"], "one\ntwo");
        assert_eq!(state.env["NAME"], "value");
        assert_eq!(state.env["ACTIONS_RUNTIME_URL"], "https://runtime");
        assert!(!state.env.contains_key("GITHUB_REF"));
        assert!(!state.env.contains_key("RUNNER_TEMP"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.summary, "summary text");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn path_prelude_keeps_existing_shell_path() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            script: "tool --version".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let plan = ScriptStepPlan::prepare_with_path(
            &step,
            &temp,
            &["/opt/bin".to_string(), "/path/with'quote".to_string()],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&plan.script_host_path).unwrap(),
            "export PATH='/opt/bin':'/path/with'\\''quote':\"$PATH\"\ntool --version\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn maps_github_script_steps_to_internal_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "displayName": "Run tests",
                "enabled": true,
                "continueOnError": true,
                "reference": { "type": "Script" },
                "inputs": {
                    "script": "cargo test",
                    "shell": "bash",
                    "workingDirectory": "./crates"
                },
                "environment": {
                    "CARGO_TERM_COLOR": "always",
                    "CARGO_INCREMENTAL": 0,
                    "RENOVATE_ONBOARDING": false,
                    "TOKEN": "${{ github.token }}"
                }
            },
            {
                "id": "checkout",
                "reference": { "type": "Repository", "name": "actions/checkout" }
            },
            {
                "id": "disabled",
                "enabled": false,
                "reference": { "type": "Script" },
                "inputs": { "script": "echo skip" }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].id, "run-1");
        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
        assert_eq!(
            mapped[0].env,
            vec![
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("CARGO_TERM_COLOR".into(), "always".into()),
                ("RENOVATE_ONBOARDING".into(), "false".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ]
        );
        assert!(mapped[0].continue_on_error);
    }

    #[test]
    fn maps_run_service_capitalized_script_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "Script": { "Value": "cargo test" },
                    "Shell": { "Value": "bash" },
                    "WorkingDirectory": { "Value": "./crates" }
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_nested_input_map() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": {
                        "script": "cargo test",
                        "shell": "bash",
                        "workingDirectory": "./crates"
                    }
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_input_map_array() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "script", "type": 0 }, "Value": { "lit": "cargo test", "type": 0 } },
                        { "Key": { "lit": "shell", "type": 0 }, "Value": { "lit": "bash", "type": 0 } },
                        { "Key": { "lit": "workingDirectory", "type": 0 }, "Value": { "lit": "./crates", "type": 0 } }
                    ]
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_typed_step_environment() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "script" }, "Value": { "lit": "cargo test" } }
                    ]
                },
                "environment": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "CARGO_TERM_COLOR" }, "Value": { "lit": "always" } },
                        { "Key": { "lit": "CARGO_INCREMENTAL" }, "Value": { "value": 0 } },
                        { "Key": { "lit": "RENOVATE_ONBOARDING" }, "Value": { "value": false } },
                        { "Key": { "lit": "TOKEN" }, "Value": { "lit": "${{ github.token }}" } }
                    ]
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(
            mapped[0].env,
            vec![
                ("CARGO_TERM_COLOR".into(), "always".into()),
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("RENOVATE_ONBOARDING".into(), "false".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ]
        );
    }

    #[test]
    fn applies_job_run_defaults_to_script_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "ansible",
                "reference": { "type": "Script" },
                "inputs": { "script": "ansible-playbook --syntax-check site.yml" }
            },
            {
                "id": "override",
                "reference": { "type": "Script" },
                "inputs": {
                    "script": "cargo test",
                    "shell": "sh",
                    "workingDirectory": "./backend-rust"
                }
            }
        ]))
        .unwrap();
        let defaults = vec![serde_json::json!({
            "run": {
                "shell": "bash",
                "working-directory": "./ansible-configs"
            }
        })];

        let mapped = github_script_steps_with_defaults(&steps, "/__w/repo", &defaults).unwrap();

        assert_eq!(mapped[0].id, "ansible");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(
            mapped[0].working_directory_container,
            "/__w/repo/ansible-configs"
        );
        assert!(matches!(mapped[1].shell, Shell::Sh));
        assert_eq!(
            mapped[1].working_directory_container,
            "/__w/repo/backend-rust"
        );
    }

    #[test]
    fn rejects_unsupported_github_shell() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "reference": { "type": "Script" },
                "inputs": { "script": "echo hi", "shell": "pwsh" }
            }
        ]))
        .unwrap();

        let error = github_script_steps(&steps, "/__w/repo").unwrap_err();

        assert!(error.to_string().contains("unsupported run step shell"));
    }

    #[test]
    fn detects_enabled_non_script_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "enabled": false,
                "reference": { "type": "Repository", "name": "actions/checkout" }
            },
            {
                "enabled": true,
                "reference": { "type": "Script" },
                "inputs": { "script": "echo hi" }
            }
        ]))
        .unwrap();

        assert!(!has_enabled_non_script_steps(&steps));

        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "enabled": true,
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }
        ]))
        .unwrap();

        assert!(has_enabled_non_script_steps(&steps));
    }
}

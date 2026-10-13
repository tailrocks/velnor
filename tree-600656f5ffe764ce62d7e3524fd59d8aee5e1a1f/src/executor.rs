#![allow(dead_code)]

use crate::{
    action::JavaScriptActionInvocation,
    container::JobContainerSpec,
    script_step::{ScriptStep, ScriptStepPlan, StepCommandState},
    workflow_command::parse_workflow_commands,
};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::Path,
    process::{Command, ExitStatus},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult>;
}

pub fn render_context_expressions(value: &str, context_data: &[(String, Value)]) -> String {
    JobExecutionState::new_with_context(&[], context_data).resolve_expressions(value)
}

#[derive(Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("run {program} {}", args.join(" ")))?;

        Ok(CommandResult {
            code: exit_code(output.status)?,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepExecutionResult {
    pub exit_code: i32,
    pub state: StepCommandState,
    pub skipped: bool,
}

#[derive(Debug, Clone)]
pub enum ExecutableStep {
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
    },
}

impl ExecutableStep {
    fn id(&self) -> &str {
        match self {
            ExecutableStep::Script(step) => &step.id,
            ExecutableStep::JavaScript { step_id, .. } => step_id,
        }
    }

    fn condition(&self) -> Option<&str> {
        match self {
            ExecutableStep::Script(step) => step.condition.as_deref(),
            ExecutableStep::JavaScript { condition, .. } => condition.as_deref(),
        }
    }
}

pub struct DockerScriptExecutor<R> {
    runner: R,
}

impl<R> DockerScriptExecutor<R>
where
    R: CommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    pub fn execute_step(
        &mut self,
        container: &JobContainerSpec,
        step: &ScriptStep,
        temp_host: &Path,
    ) -> Result<StepExecutionResult> {
        let mut results = self.execute_steps(container, std::slice::from_ref(step), temp_host)?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("script step did not produce a result"))
    }

    pub fn execute_steps(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ScriptStep],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        self.run_docker(&container.create_network_args())?;
        self.run_docker(&container.start_args())?;

        let ordered = steps
            .iter()
            .cloned()
            .map(ExecutableStep::Script)
            .collect::<Vec<_>>();
        let result = self.execute_ordered_steps_in_started_container(
            container,
            &ordered,
            &[],
            &[],
            temp_host,
        );

        let cleanup_result = self.cleanup(container);
        if let Err(error) = cleanup_result {
            return Err(error.context("cleanup failed after script step"));
        }
        result
    }

    pub fn execute_ordered_steps(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        self.execute_ordered_steps_with_context(container, steps, base_env, &[], temp_host)
    }

    pub fn execute_ordered_steps_with_context(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        self.run_docker(&container.create_network_args())?;
        self.run_docker(&container.start_args())?;

        let result = self.execute_ordered_steps_in_started_container(
            container,
            steps,
            base_env,
            context_data,
            temp_host,
        );

        let cleanup_result = self.cleanup(container);
        if let Err(error) = cleanup_result {
            return Err(error.context("cleanup failed after ordered job steps"));
        }
        result
    }

    fn execute_ordered_steps_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        let mut results = Vec::new();
        let mut state = JobExecutionState::new_with_context(base_env, context_data);
        let mut step_error = None;
        for step in steps {
            let step_id = step.id().to_string();
            if !state.evaluate_condition(step.condition()) {
                let result = StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState::default(),
                    skipped: true,
                };
                state.apply(&step_id, &result);
                results.push(result);
                continue;
            }
            let result = (|| match step {
                ExecutableStep::Script(step) => {
                    let step = state.resolve_script_step(step);
                    let plan = ScriptStepPlan::prepare_with_path(&step, temp_host, &state.path)?;
                    let mut env = state.step_env(&[]);
                    env.extend(state.resolve_env(&step.env));
                    env.extend(plan.env.iter().cloned());
                    let exec_args = container.exec_script_args(
                        &plan.script_container_path,
                        plan.shell,
                        &plan.working_directory_container,
                        &env,
                    );
                    let step_result = self.runner.run("docker", &exec_args)?;
                    let mut command_state = plan.collect_state()?;
                    command_state.merge(parse_workflow_commands(&step_result.stdout));
                    Ok(StepExecutionResult {
                        exit_code: step_result.code,
                        state: command_state,
                        skipped: false,
                    })
                }
                ExecutableStep::JavaScript {
                    step_id,
                    invocation,
                    ..
                } => self.execute_javascript_action_in_started_container(
                    container, step_id, invocation, temp_host, &state,
                ),
            })();

            match result {
                Ok(result) => {
                    let failed = result.exit_code != 0;
                    state.apply(&step_id, &result);
                    results.push(result);
                    if failed {
                        break;
                    }
                }
                Err(error) => {
                    step_error = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = step_error {
            return Err(error);
        }
        Ok(results)
    }

    pub fn execute_javascript_action(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &JavaScriptActionInvocation,
        temp_host: &Path,
    ) -> Result<StepExecutionResult> {
        self.run_docker(&container.create_network_args())?;
        self.run_docker(&container.start_args())?;

        let result = (|| {
            self.execute_javascript_action_in_started_container(
                container,
                step_id,
                action,
                temp_host,
                &JobExecutionState::default(),
            )
        })();

        let cleanup_result = self.cleanup(container);
        if let Err(error) = cleanup_result {
            return Err(error.context("cleanup failed after JavaScript action"));
        }
        result
    }

    fn execute_javascript_action_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &JavaScriptActionInvocation,
        temp_host: &Path,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let command_files = ScriptStepPlan::prepare(
            &ScriptStep {
                id: step_id.to_string(),
                script: String::new(),
                shell: crate::container::Shell::Sh,
                working_directory_container: "/__w".to_string(),
                env: Vec::new(),
                condition: None,
            },
            temp_host,
        )?;
        let mut env = state.step_env(&[]);
        env.extend(state.resolve_env(&action.env));
        env.extend(command_files.env.iter().cloned());
        let exec_args = container.exec_process_args(
            "/__w",
            &env,
            &["node".to_string(), action.main_container_path.clone()],
        );
        let step_result = self.runner.run("docker", &exec_args)?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands(&step_result.stdout));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
        })
    }

    fn cleanup(&mut self, container: &JobContainerSpec) -> Result<()> {
        let container_result = self.run_docker(&container.remove_container_args());
        let network_result = self.run_docker(&container.remove_network_args());

        container_result?;
        network_result?;
        Ok(())
    }

    fn run_docker(&mut self, args: &[String]) -> Result<CommandResult> {
        let result = self.runner.run("docker", args)?;
        if result.code != 0 {
            bail!(
                "docker {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        Ok(result)
    }
}

#[derive(Debug, Default)]
struct JobExecutionState {
    env: BTreeMap<String, String>,
    context_data: BTreeMap<String, Value>,
    outputs: BTreeMap<String, BTreeMap<String, String>>,
    outcomes: BTreeMap<String, StepOutcome>,
    path: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    Success,
    Failure,
    Skipped,
}

impl StepOutcome {
    fn as_str(self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            StepOutcome::Failure => "failure",
            StepOutcome::Skipped => "skipped",
        }
    }
}

impl JobExecutionState {
    fn new(base_env: &[(String, String)]) -> Self {
        Self::new_with_context(base_env, &[])
    }

    fn new_with_context(base_env: &[(String, String)], context_data: &[(String, Value)]) -> Self {
        Self {
            env: base_env.iter().cloned().collect(),
            context_data: context_data.iter().cloned().collect(),
            outputs: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            path: Vec::new(),
        }
    }

    fn step_env(&self, command_file_env: &[(String, String)]) -> Vec<(String, String)> {
        let mut env: Vec<_> = self
            .env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        env.extend(command_file_env.iter().cloned());
        env
    }

    fn apply(&mut self, step_id: &str, result: &StepExecutionResult) {
        let outcome = if result.skipped {
            StepOutcome::Skipped
        } else if result.exit_code == 0 {
            StepOutcome::Success
        } else {
            StepOutcome::Failure
        };
        self.outcomes.insert(step_id.to_string(), outcome);

        if !result.state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), result.state.outputs.clone());
        }
        for (name, value) in &result.state.env {
            self.env.insert(name.clone(), value.clone());
        }
        for path in result.state.path.iter().rev() {
            self.path.insert(0, path.clone());
        }
    }

    fn resolve_script_step(&self, step: &ScriptStep) -> ScriptStep {
        ScriptStep {
            id: step.id.clone(),
            script: self.resolve_expressions(&step.script),
            shell: step.shell,
            working_directory_container: self
                .resolve_expressions(&step.working_directory_container),
            env: self.resolve_env(&step.env),
            condition: step.condition.clone(),
        }
    }

    fn resolve_env(&self, env: &[(String, String)]) -> Vec<(String, String)> {
        env.iter()
            .map(|(name, value)| (name.clone(), self.resolve_expressions(value)))
            .collect()
    }

    fn resolve_expressions(&self, value: &str) -> String {
        let mut rendered = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(start) = rest.find("${{") {
            rendered.push_str(&rest[..start]);
            let after_start = &rest[start + 3..];
            let Some(end) = after_start.find("}}") else {
                rendered.push_str(&rest[start..]);
                return rendered;
            };
            let expression = after_start[..end].trim();
            if let Some(value) = self.resolve_expression_value(expression) {
                rendered.push_str(&value);
            } else {
                rendered.push_str(&rest[start..start + 3 + end + 2]);
            }
            rest = &after_start[end + 2..];
        }
        rendered.push_str(rest);
        rendered
    }

    fn resolve_step_output_expression(&self, expression: &str) -> Option<&str> {
        let expression = expression.strip_prefix("steps.")?;
        let (step_id, expression) = expression.split_once(".outputs.")?;
        self.outputs
            .get(step_id)
            .and_then(|outputs| outputs.get(expression))
            .map(String::as_str)
    }

    fn resolve_expression_value(&self, expression: &str) -> Option<String> {
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if let Some(context) = parse_to_json(expression) {
            return self
                .resolve_context_data_value(context)
                .and_then(|value| serde_json::to_string(value).ok());
        }
        self.resolve_context_expression(expression)
    }

    fn resolve_context_expression(&self, expression: &str) -> Option<String> {
        if let Some(name) = expression.trim().strip_prefix("env.") {
            return self.env.get(name).cloned();
        }

        let env_name = match expression.trim() {
            "github.actor" => "GITHUB_ACTOR",
            "github.event_name" => "GITHUB_EVENT_NAME",
            "github.ref" => "GITHUB_REF",
            "github.repository" => "GITHUB_REPOSITORY",
            "github.run_id" => "GITHUB_RUN_ID",
            "github.run_number" => "GITHUB_RUN_NUMBER",
            "github.sha" => "GITHUB_SHA",
            "github.token" => "GITHUB_TOKEN",
            "github.workspace" => "GITHUB_WORKSPACE",
            "runner.arch" => "RUNNER_ARCH",
            "runner.os" => "RUNNER_OS",
            "runner.temp" => "RUNNER_TEMP",
            "runner.tool_cache" => "RUNNER_TOOL_CACHE",
            _ => return self.resolve_context_data_expression(expression),
        };
        self.env.get(env_name).cloned()
    }

    fn evaluate_condition(&self, condition: Option<&str>) -> bool {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return true;
        };
        self.evaluate_condition_expr(strip_expression(condition))
    }

    fn evaluate_condition_expr(&self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression == "always()" {
            return true;
        }
        if expression == "success()" {
            return !self
                .outcomes
                .values()
                .any(|outcome| *outcome == StepOutcome::Failure);
        }
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.evaluate_condition_expr(inner);
        }
        if let Some((left, right)) = split_top_level(expression, "||") {
            return self.evaluate_condition_expr(left) || self.evaluate_condition_expr(right);
        }
        if let Some((left, right)) = split_top_level(expression, "&&") {
            return self.evaluate_condition_expr(left) && self.evaluate_condition_expr(right);
        }
        if let Some((value, needle)) = parse_contains(expression) {
            return self
                .resolve_condition_value(value)
                .is_some_and(|value| value.contains(unquote(needle.trim())));
        }
        if let Some((left, right)) = expression.split_once("!=") {
            return self.resolve_condition_value(left).as_deref() != Some(unquote(right.trim()));
        }
        if let Some((left, right)) = expression.split_once("==") {
            return self.resolve_condition_value(left).as_deref() == Some(unquote(right.trim()));
        }
        if expression == "true" {
            return true;
        }
        if expression == "false" {
            return false;
        }
        if let Some(value) = self.resolve_condition_value(expression) {
            return value == "true" || value == "success";
        }
        true
    }

    fn resolve_condition_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if let Some(expression) = expression.strip_prefix("steps.") {
            let (step_id, field) = expression.split_once('.')?;
            if field == "outcome" || field == "conclusion" {
                return self
                    .outcomes
                    .get(step_id)
                    .map(|outcome| outcome.as_str().to_string());
            }
        }
        if expression == "runner.os" {
            return self
                .resolve_context_expression(expression)
                .or_else(|| Some("Linux".to_string()));
        }
        if let Some(value) = self.resolve_context_expression(expression) {
            return Some(value);
        }
        Some(unquote(expression).to_string())
    }

    fn resolve_context_data_expression(&self, expression: &str) -> Option<String> {
        self.resolve_context_data_value(expression)
            .and_then(context_value_string)
    }

    fn resolve_context_data_value(&self, expression: &str) -> Option<&Value> {
        let mut segments = expression.trim().split('.');
        let root = segments.next()?;
        let mut value = self.context_data.get(root)?;
        for segment in segments {
            value = value.get(segment)?;
        }
        Some(value)
    }
}

fn context_value_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_contains(expression: &str) -> Option<(&str, &str)> {
    let inner = expression
        .trim()
        .strip_prefix("contains(")?
        .strip_suffix(')')?;
    inner.split_once(',')
}

fn parse_to_json(expression: &str) -> Option<&str> {
    expression
        .trim()
        .strip_prefix("toJSON(")?
        .strip_suffix(')')
        .map(str::trim)
}

fn split_top_level<'a>(expression: &'a str, operator: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0_i32;
    for (index, ch) in expression.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && expression[index..].starts_with(operator) {
            return Some((
                expression[..index].trim(),
                expression[index + operator.len()..].trim(),
            ));
        }
    }
    None
}

fn strip_wrapping_parentheses(expression: &str) -> Option<&str> {
    let expression = expression.trim();
    let inner = expression.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0_i32;
    for (index, ch) in expression.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && index != expression.len() - 1 {
                    return None;
                }
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then_some(inner.trim())
}

fn strip_expression(condition: &str) -> &str {
    condition
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .unwrap_or(condition)
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(value)
}

fn exit_code(status: ExitStatus) -> Result<i32> {
    status
        .code()
        .ok_or_else(|| anyhow::anyhow!("process terminated by signal"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Shell;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
        codes: Vec<i32>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let code = if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct OutputWritingRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for OutputWritingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            if program == "docker" && args.first().is_some_and(|arg| arg == "exec") {
                if args
                    .iter()
                    .any(|arg| arg == "GITHUB_OUTPUT=/__t/producer_output")
                {
                    fs::write(self.temp.join("producer_output"), "answer=42\n")?;
                }
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct StdoutCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for StdoutCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stdout = if program == "docker" && args.first().is_some_and(|arg| arg == "exec") {
                "::set-output name=answer::42\n::add-path::/opt/tool\n".to_string()
            } else {
                String::new()
            };
            Ok(CommandResult {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        }
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-executor-test-{nonce}"))
    }

    fn container(temp: &Path) -> JobContainerSpec {
        JobContainerSpec {
            name: "job".into(),
            image: "ubuntu:24.04".into(),
            network: "net".into(),
            workspace_host: temp.join("work"),
            temp_host: temp.to_path_buf(),
            actions_host: temp.join("actions"),
            tools_host: temp.join("tools"),
            mount_docker_socket: false,
        }
    }

    #[test]
    fn executes_script_step_and_collects_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let step = ScriptStep {
            id: "step1".into(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let result = executor
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let calls = &executor.runner().calls;
        assert_eq!(
            calls[0],
            (
                "docker".into(),
                vec!["network", "create", "net"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            )
        );
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/__t/step1_output".into()));
        assert_eq!(
            calls[3].1,
            vec!["rm", "--force", "job"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            calls[4].1,
            vec!["network", "rm", "net"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn returns_script_exit_code_and_still_cleans_up() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let step = ScriptStep {
            id: "step1".into(),
            script: "exit 7".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 0, 7, 0, 0],
        });

        let result = executor
            .execute_step(&container(&temp), &step, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 7);
        assert_eq!(executor.runner().calls.len(), 5);
        assert_eq!(executor.runner().calls[3].1[0], "rm");
        assert_eq!(executor.runner().calls[4].1[0], "network");

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_multiple_steps_in_one_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ScriptStep {
                id: "step1".into(),
                script: "echo one".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            },
            ScriptStep {
                id: "step2".into(),
                script: "echo two".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            },
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_steps(&container(&temp), &steps, &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let calls = &executor.runner().calls;
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[0].1[0], "network");
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert_eq!(calls[3].1[0], "exec");
        assert_eq!(calls[4].1[0], "rm");
        assert_eq!(calls[5].1[0], "network");

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn job_state_flows_env_and_path_to_later_steps() {
        let mut state = JobExecutionState::default();
        state.apply(
            "producer",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                state: StepCommandState {
                    outputs: [("answer".to_string(), "42".to_string())].into(),
                    env: [("NAME".to_string(), "value".to_string())].into(),
                    path: vec!["/opt/tool".to_string()],
                    ..Default::default()
                },
            },
        );

        let env = state.step_env(&[("GITHUB_OUTPUT".into(), "/__t/out".into())]);

        assert!(env.contains(&("NAME".into(), "value".into())));
        assert!(env.contains(&("GITHUB_OUTPUT".into(), "/__t/out".into())));
        assert!(!env.iter().any(|(name, _)| name == "PATH"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(
            state.resolve_expressions("value=${{ steps.producer.outputs.answer }}"),
            "value=42"
        );
        assert_eq!(
            state.resolve_expressions("keep=${{ github.ref }}"),
            "keep=${{ github.ref }}"
        );
    }

    #[test]
    fn resolves_step_outputs_in_later_action_env() {
        let mut state = JobExecutionState::default();
        state.apply(
            "meta",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                state: StepCommandState {
                    outputs: [("tags".to_string(), "image:latest".to_string())].into(),
                    ..Default::default()
                },
            },
        );

        let env =
            state.resolve_env(&[("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into())]);

        assert_eq!(env, vec![("INPUT_TAGS".into(), "image:latest".into())]);
    }

    #[test]
    fn resolves_github_and_runner_context_expressions() {
        let state = JobExecutionState::new(&[
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("DOCS_SITE_URL".into(), "https://docs.example".into()),
            ("RUNNER_OS".into(), "Linux".into()),
        ]);

        assert_eq!(
            state.resolve_expressions("${{ github.ref }} ${{ github.sha }}"),
            "refs/heads/main abc123"
        );
        assert_eq!(
            state.resolve_env(&[
                ("INPUT_TOKEN".into(), "${{ github.token }}".into()),
                ("DOCS_SITE_URL".into(), "${{ env.DOCS_SITE_URL }}".into()),
                ("WORKSPACE".into(), "${{ github.workspace }}".into()),
                ("OS".into(), "${{ runner.os }}".into()),
            ]),
            vec![
                ("INPUT_TOKEN".into(), "ghs_token".into()),
                ("DOCS_SITE_URL".into(), "https://docs.example".into()),
                ("WORKSPACE".into(), "/__w".into()),
                ("OS".into(), "Linux".into()),
            ]
        );
    }

    #[test]
    fn resolves_job_context_data_expressions_and_conditions() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
            ],
            &[
                (
                    "matrix".into(),
                    serde_json::json!({
                        "target": "x86_64-apple-darwin",
                        "zigbuild": true
                    }),
                ),
                (
                    "needs".into(),
                    serde_json::json!({
                        "changes": {
                            "outputs": {
                                "bitcoin-processor": "false",
                                "bake-targets": "bitcoin-processor-app"
                            },
                            "result": "success"
                        },
                        "test-bitcoin-processor": {
                            "result": "failure"
                        }
                    }),
                ),
                (
                    "inputs".into(),
                    serde_json::json!({ "packages": "bitcoin-processor-app" }),
                ),
            ],
        );

        assert_eq!(
            state.resolve_expressions("target=${{ matrix.target }}"),
            "target=x86_64-apple-darwin"
        );
        assert_eq!(
            state.resolve_expressions("needs=${{ toJSON(needs.changes.outputs) }}"),
            r#"needs={"bake-targets":"bitcoin-processor-app","bitcoin-processor":"false"}"#
        );
        assert!(state.evaluate_condition(Some("matrix.zigbuild")));
        assert!(state.evaluate_condition(Some("contains(matrix.target, 'apple')")));
        assert!(state.evaluate_condition(Some("needs.test-bitcoin-processor.result == 'failure'")));
        assert!(state.evaluate_condition(Some(
            "needs.changes.outputs.bitcoin-processor == 'true' || (github.event_name == 'workflow_dispatch' && (inputs.packages == '' || contains(inputs.packages, 'bitcoin-processor-app')))"
        )));
        assert!(state.evaluate_condition(Some("needs.changes.outputs.bake-targets != ''")));
    }

    #[test]
    fn injects_resolved_step_environment_into_script_exec() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "env-step".into(),
            script: "echo env".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: vec![
                ("MODE".into(), "release".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ],
            condition: None,
        })];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("GITHUB_TOKEN".into(), "ghs_token".into())],
                &temp,
            )
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"MODE=release".into()));
        assert!(exec_args.contains(&"TOKEN=ghs_token".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_step_outputs_in_later_script_before_exec() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "echo answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workflow_commands_from_stdout_update_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(StdoutCommandRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["answer"], "42");
        assert_eq!(results[0].state.path, vec!["/opt/tool"]);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn skips_steps_when_output_condition_is_false() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo should-not-run".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.producer.outputs.answer == 'nope'".into()),
            }),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[1].skipped);
        assert!(!temp.join("consumer.sh").exists());
        let exec_count = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| args.first().is_some_and(|arg| arg == "exec"))
            .count();
        assert_eq!(exec_count, 1);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_step_outcome_conditions() {
        let mut state = JobExecutionState::default();
        state.apply(
            "sccache",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                state: StepCommandState::default(),
            },
        );
        state.apply(
            "disabled",
            &StepExecutionResult {
                exit_code: 0,
                skipped: true,
                state: StepCommandState::default(),
            },
        );

        assert!(state.evaluate_condition(Some("steps.sccache.outcome == 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.outcome != 'success'")));
        assert!(state.evaluate_condition(Some("runner.os == 'Linux'")));
    }

    #[test]
    fn executes_javascript_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = JavaScriptActionInvocation {
            node: "node20".into(),
            main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
            action_container_path: "/__a/_actions/acme_action/v1".into(),
            env: vec![
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_action/v1".into(),
                ),
                ("INPUT_NAME".into(), "value".into()),
            ],
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let result = executor
            .execute_javascript_action(&container(&temp), "action1", &action, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "exec");
        assert!(calls[2].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/__t/action1_output".into()));
        assert!(calls[2].1.ends_with(&[
            "node".into(),
            "/__a/_actions/acme_action/v1/dist/index.js".into()
        ]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_ordered_script_and_javascript_steps_in_one_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "step1".into(),
                script: "echo NAME=value >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
            ExecutableStep::JavaScript {
                step_id: "action1".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
                    action_container_path: "/__a/_actions/acme_action/v1".into(),
                    env: vec![
                        ("INPUT_NAME".into(), "value".into()),
                        ("TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "step2".into(),
                script: "echo done".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_REPOSITORY".into(), "acme/repo".into()),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 3);
        let calls = &executor.runner().calls;
        assert_eq!(calls.len(), 7);
        assert_eq!(calls[0].1[0], "network");
        assert_eq!(calls[1].1[0], "run");
        assert_eq!(calls[2].1[0], "exec");
        assert_eq!(calls[3].1[0], "exec");
        assert_eq!(calls[4].1[0], "exec");
        assert_eq!(calls[5].1[0], "rm");
        assert_eq!(calls[6].1[0], "network");
        assert!(calls[3].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[3].1.contains(&"GITHUB_REPOSITORY=acme/repo".into()));
        assert!(calls[3].1.contains(&"TOKEN=ghs_token".into()));
        assert!(calls[3]
            .1
            .contains(&"GITHUB_OUTPUT=/__t/action1_output".into()));

        fs::remove_dir_all(temp).unwrap();
    }
}

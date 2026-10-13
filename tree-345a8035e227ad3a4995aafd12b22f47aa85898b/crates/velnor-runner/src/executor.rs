#![allow(dead_code)]

use crate::{
    action::JavaScriptActionInvocation,
    container::JobContainerSpec,
    script_step::{ScriptStep, ScriptStepPlan, StepCommandState},
    workflow_command::parse_workflow_commands,
};
use anyhow::{bail, Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
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
    pub failure_ignored: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobExecutionSummary {
    pub step_results: Vec<StepExecutionResult>,
    pub job_outputs: BTreeMap<String, String>,
    pub step_logs: Vec<StepLog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    pub step_id: String,
    pub lines: Vec<String>,
    pub masks: Vec<String>,
    pub exit_code: i32,
    pub skipped: bool,
    pub failure_ignored: bool,
}

#[derive(Debug, Clone)]
pub enum ExecutableStep {
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    CompositeOutputs {
        step_id: String,
        outputs: BTreeMap<String, String>,
        condition: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct PostJavaScriptAction {
    step_id: String,
    invocation: JavaScriptActionInvocation,
}

impl ExecutableStep {
    fn id(&self) -> &str {
        match self {
            ExecutableStep::Script(step) => &step.id,
            ExecutableStep::JavaScript { step_id, .. } => step_id,
            ExecutableStep::CompositeOutputs { step_id, .. } => step_id,
        }
    }

    fn condition(&self) -> Option<&str> {
        match self {
            ExecutableStep::Script(step) => step.condition.as_deref(),
            ExecutableStep::JavaScript { condition, .. } => condition.as_deref(),
            ExecutableStep::CompositeOutputs { condition, .. } => condition.as_deref(),
        }
    }

    fn continue_on_error(&self) -> bool {
        match self {
            ExecutableStep::Script(step) => step.continue_on_error,
            ExecutableStep::JavaScript {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::CompositeOutputs { .. } => false,
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
        for service in &container.services {
            self.run_docker(&service.start_args())?;
        }
        self.run_docker(&container.start_args())?;

        let ordered = steps
            .iter()
            .cloned()
            .map(ExecutableStep::Script)
            .collect::<Vec<_>>();
        let result = self
            .execute_ordered_steps_in_started_container(
                container,
                &ordered,
                &[],
                &[],
                None,
                temp_host,
            )
            .map(|summary| summary.step_results);

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
        Ok(self
            .execute_ordered_steps_with_job_outputs(
                container,
                steps,
                base_env,
                context_data,
                None,
                temp_host,
            )?
            .step_results)
    }

    pub fn execute_ordered_steps_with_job_outputs(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        self.run_docker(&container.create_network_args())?;
        for service in &container.services {
            self.run_docker(&service.start_args())?;
        }
        self.run_docker(&container.start_args())?;

        let result = self.execute_ordered_steps_in_started_container(
            container,
            steps,
            base_env,
            context_data,
            job_outputs,
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
        job_outputs: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        let mut results = Vec::new();
        let mut step_logs = Vec::new();
        let mut base_env = base_env.to_vec();
        if let Some(event_path) = prepare_github_event_path(temp_host, context_data)? {
            base_env.push(("GITHUB_EVENT_PATH".to_string(), event_path));
        }
        let mut state = JobExecutionState::new_with_workspace(
            &base_env,
            context_data,
            &container.workspace_host,
        );
        let mut step_error = None;
        let mut post_actions = Vec::new();
        for step in steps {
            let step_id = step.id().to_string();
            if !state.evaluate_condition(step.condition()) {
                let result = StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState::default(),
                    skipped: true,
                    failure_ignored: false,
                    stdout: String::new(),
                    stderr: String::new(),
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
                        failure_ignored: false,
                        stdout: step_result.stdout,
                        stderr: step_result.stderr,
                    })
                }
                ExecutableStep::JavaScript {
                    step_id,
                    invocation,
                    ..
                } => self.execute_javascript_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    &invocation.main_container_path,
                    &[],
                    temp_host,
                    &state,
                ),
                ExecutableStep::CompositeOutputs { outputs, .. } => Ok(StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState {
                        outputs: state.evaluate_named_outputs(outputs),
                        ..StepCommandState::default()
                    },
                    skipped: false,
                    failure_ignored: false,
                    stdout: String::new(),
                    stderr: String::new(),
                }),
            })();

            match result {
                Ok(mut result) => {
                    let failed = result.exit_code != 0;
                    if let ExecutableStep::JavaScript { invocation, .. } = step {
                        if invocation.post_container_path.is_some() {
                            post_actions.push(PostJavaScriptAction {
                                step_id: step_id.clone(),
                                invocation: invocation.clone(),
                            });
                        }
                    }
                    if failed && step.continue_on_error() {
                        result.failure_ignored = true;
                    }
                    if let Some(log) = step_log(&step_id, &result) {
                        step_logs.push(log);
                    }
                    state.apply(&step_id, &result);
                    results.push(result);
                    if failed && !step.continue_on_error() {
                        break;
                    }
                }
                Err(error) => {
                    step_error = Some(error);
                    break;
                }
            }
        }
        for post_action in post_actions.into_iter().rev() {
            let result = self.execute_javascript_action_in_started_container(
                container,
                &format!("{}-post", post_action.step_id),
                &post_action.invocation,
                post_action
                    .invocation
                    .post_container_path
                    .as_deref()
                    .expect("post action must have post entrypoint"),
                &state.action_state_env(&post_action.step_id),
                temp_host,
                &state,
            );
            match result {
                Ok(result) => {
                    if let Some(log) = step_log(&format!("{}-post", post_action.step_id), &result) {
                        step_logs.push(log);
                    }
                    state.apply(&format!("{}-post", post_action.step_id), &result);
                    results.push(result);
                }
                Err(error) => {
                    if step_error.is_none() {
                        step_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = step_error {
            return Err(error);
        }
        Ok(JobExecutionSummary {
            job_outputs: evaluate_job_outputs(job_outputs, &state),
            step_results: results,
            step_logs,
        })
    }

    pub fn execute_javascript_action(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &JavaScriptActionInvocation,
        temp_host: &Path,
    ) -> Result<StepExecutionResult> {
        self.run_docker(&container.create_network_args())?;
        for service in &container.services {
            self.run_docker(&service.start_args())?;
        }
        self.run_docker(&container.start_args())?;

        let result = (|| {
            self.execute_javascript_action_in_started_container(
                container,
                step_id,
                action,
                &action.main_container_path,
                &[],
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
        entrypoint_container_path: &str,
        action_state_env: &[(String, String)],
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
                continue_on_error: false,
            },
            temp_host,
        )?;
        let mut env = state.step_env(&[]);
        env.extend(state.resolve_env(&action.env));
        env.extend(action_state_env.iter().cloned());
        env.extend(command_files.env.iter().cloned());
        let exec_args = container.exec_process_args(
            "/__w",
            &env,
            &["node".to_string(), entrypoint_container_path.to_string()],
        );
        let step_result = self.runner.run("docker", &exec_args)?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands(&step_result.stdout));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
            failure_ignored: false,
            stdout: step_result.stdout,
            stderr: step_result.stderr,
        })
    }

    fn cleanup(&mut self, container: &JobContainerSpec) -> Result<()> {
        let container_result = self.run_docker(&container.remove_container_args());
        let service_results = container
            .services
            .iter()
            .rev()
            .map(|service| self.run_docker(&service.remove_args()))
            .collect::<Vec<_>>();
        let network_result = self.run_docker(&container.remove_network_args());

        container_result?;
        for service_result in service_results {
            service_result?;
        }
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
    workspace_host: Option<PathBuf>,
    outputs: BTreeMap<String, BTreeMap<String, String>>,
    action_states: BTreeMap<String, BTreeMap<String, String>>,
    outcomes: BTreeMap<String, StepOutcome>,
    path: Vec<String>,
    masks: Vec<String>,
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
        Self::new_internal(base_env, context_data, None)
    }

    fn new_with_workspace(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: &Path,
    ) -> Self {
        Self::new_internal(base_env, context_data, Some(workspace_host.to_path_buf()))
    }

    fn new_internal(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: Option<PathBuf>,
    ) -> Self {
        Self {
            env: base_env.iter().cloned().collect(),
            context_data: context_data.iter().cloned().collect(),
            workspace_host,
            outputs: BTreeMap::new(),
            action_states: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            path: Vec::new(),
            masks: Vec::new(),
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
        if !result.state.state.is_empty() {
            self.action_states
                .insert(step_id.to_string(), result.state.state.clone());
        }
        for (name, value) in &result.state.env {
            self.env.insert(name.clone(), value.clone());
        }
        for path in result.state.path.iter().rev() {
            self.path.insert(0, path.clone());
        }
        self.masks.extend(result.state.masks.iter().cloned());
    }

    fn action_state_env(&self, step_id: &str) -> Vec<(String, String)> {
        self.action_states
            .get(step_id)
            .into_iter()
            .flat_map(|state| {
                state
                    .iter()
                    .map(|(name, value)| (format!("STATE_{name}"), value.clone()))
            })
            .collect()
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
            continue_on_error: step.continue_on_error,
        }
    }

    fn resolve_env(&self, env: &[(String, String)]) -> Vec<(String, String)> {
        env.iter()
            .map(|(name, value)| (name.clone(), self.resolve_expressions(value)))
            .collect()
    }

    fn evaluate_named_outputs(
        &self,
        outputs: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        outputs
            .iter()
            .filter_map(|(name, value)| {
                let value = self.resolve_expressions(value);
                (!value.is_empty()).then(|| (name.clone(), value))
            })
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
        let expression = expression.trim();
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.resolve_expression_value(inner);
        }
        if let Some((left, right)) = split_top_level(expression, "||") {
            let left = self.resolve_expression_value(left).unwrap_or_default();
            if expression_truthy(&left) {
                return Some(left);
            }
            return self.resolve_expression_value(right);
        }
        if let Some((left, right)) = split_top_level(expression, "&&") {
            let left = self.resolve_expression_value(left).unwrap_or_default();
            if expression_truthy(&left) {
                return self.resolve_expression_value(right);
            }
            return Some("false".to_string());
        }
        if let Some(inner) = expression.strip_prefix('!') {
            let value = self.resolve_expression_value(inner).unwrap_or_default();
            return Some((!expression_truthy(&value)).to_string());
        }
        if let Some((left, right)) = split_top_level(expression, "!=") {
            return Some(
                (self.resolve_expression_value(left).unwrap_or_default()
                    != self.resolve_expression_value(right).unwrap_or_default())
                .to_string(),
            );
        }
        if let Some((left, right)) = split_top_level(expression, "==") {
            return Some(
                (self.resolve_expression_value(left).unwrap_or_default()
                    == self.resolve_expression_value(right).unwrap_or_default())
                .to_string(),
            );
        }
        if let Some((value, needle)) = parse_contains(expression) {
            return Some(
                self.resolve_expression_value(value)
                    .is_some_and(|value| value.contains(unquote(needle.trim())))
                    .to_string(),
            );
        }
        if is_quoted(expression) {
            return Some(unquote(expression).to_string());
        }
        if expression == "true" || expression == "false" {
            return Some(expression.to_string());
        }
        if expression == "null" {
            return Some(String::new());
        }
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if expression.starts_with("steps.") && expression.contains(".outputs.") {
            return Some(String::new());
        }
        if let Some(context) = parse_to_json(expression) {
            return self
                .resolve_context_data_value(context)
                .and_then(|value| serde_json::to_string(value).ok());
        }
        if let Some(patterns) = parse_hash_files(expression) {
            return self
                .workspace_host
                .as_deref()
                .map(|workspace| hash_files(workspace, &patterns));
        }
        self.resolve_context_expression(expression)
    }

    fn resolve_context_expression(&self, expression: &str) -> Option<String> {
        if let Some(name) = expression.trim().strip_prefix("env.") {
            return self.env.get(name).cloned();
        }

        let env_name = match expression.trim() {
            "github.actor" => "GITHUB_ACTOR",
            "github.actor_id" => "GITHUB_ACTOR_ID",
            "github.action" => "GITHUB_ACTION",
            "github.action_path" => "GITHUB_ACTION_PATH",
            "github.action_ref" => "GITHUB_ACTION_REF",
            "github.action_repository" => "GITHUB_ACTION_REPOSITORY",
            "github.api_url" => "GITHUB_API_URL",
            "github.base_ref" => "GITHUB_BASE_REF",
            "github.event_name" => "GITHUB_EVENT_NAME",
            "github.event_path" => "GITHUB_EVENT_PATH",
            "github.graphql_url" => "GITHUB_GRAPHQL_URL",
            "github.head_ref" => "GITHUB_HEAD_REF",
            "github.ref" => "GITHUB_REF",
            "github.ref_protected" => "GITHUB_REF_PROTECTED",
            "github.ref_type" => "GITHUB_REF_TYPE",
            "github.repository" => "GITHUB_REPOSITORY",
            "github.repository_id" => "GITHUB_REPOSITORY_ID",
            "github.repository_owner" => "GITHUB_REPOSITORY_OWNER",
            "github.repository_owner_id" => "GITHUB_REPOSITORY_OWNER_ID",
            "github.retention_days" => "GITHUB_RETENTION_DAYS",
            "github.run_id" => "GITHUB_RUN_ID",
            "github.run_attempt" => "GITHUB_RUN_ATTEMPT",
            "github.run_number" => "GITHUB_RUN_NUMBER",
            "github.server_url" => "GITHUB_SERVER_URL",
            "github.sha" => "GITHUB_SHA",
            "github.token" => "GITHUB_TOKEN",
            "github.triggering_actor" => "GITHUB_TRIGGERING_ACTOR",
            "github.workflow" => "GITHUB_WORKFLOW",
            "github.workflow_ref" => "GITHUB_WORKFLOW_REF",
            "github.workflow_sha" => "GITHUB_WORKFLOW_SHA",
            "github.ref_name" => "GITHUB_REF_NAME",
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
        if expression == "failure()" {
            return self
                .outcomes
                .values()
                .any(|outcome| *outcome == StepOutcome::Failure);
        }
        if expression == "cancelled()" {
            return false;
        }
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.evaluate_condition_expr(inner);
        }
        if let Some(inner) = expression.strip_prefix('!') {
            return !self.evaluate_condition_expr(inner);
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

fn evaluate_job_outputs(
    job_outputs: Option<&Value>,
    state: &JobExecutionState,
) -> BTreeMap<String, String> {
    let Some(Value::Object(outputs)) = job_outputs else {
        return BTreeMap::new();
    };
    outputs
        .iter()
        .filter_map(|(name, value)| {
            let value = job_output_expression(value)?;
            let value = state.resolve_expressions(value);
            (!value.is_empty()).then(|| (name.clone(), value))
        })
        .collect()
}

fn step_log(step_id: &str, result: &StepExecutionResult) -> Option<StepLog> {
    let lines = step_log_lines(&result.stdout, &result.stderr);
    (!lines.is_empty()).then(|| StepLog {
        step_id: step_id.to_string(),
        lines,
        masks: result.state.masks.clone(),
        exit_code: result.exit_code,
        skipped: result.skipped,
        failure_ignored: result.failure_ignored,
    })
}

fn step_log_lines(stdout: &str, stderr: &str) -> Vec<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .map(ToOwned::to_owned)
        .collect()
}

fn prepare_github_event_path(
    temp_host: &Path,
    context_data: &[(String, Value)],
) -> Result<Option<String>> {
    let Some(payload) = github_event_payload(context_data) else {
        return Ok(None);
    };
    let event_dir = temp_host.join("_github_workflow");
    fs::create_dir_all(&event_dir).with_context(|| format!("create {}", event_dir.display()))?;
    let event_path = event_dir.join("event.json");
    fs::write(&event_path, payload).with_context(|| format!("write {}", event_path.display()))?;
    Ok(Some("/__t/_github_workflow/event.json".to_string()))
}

fn github_event_payload(context_data: &[(String, Value)]) -> Option<String> {
    let github = context_data
        .iter()
        .find_map(|(name, value)| (name == "github").then_some(value))?;
    let event = github.get("event")?;
    match event {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => serde_json::to_string(value).ok(),
    }
}

fn job_output_expression(value: &Value) -> Option<&str> {
    if let Some(value) = value.as_str() {
        return Some(value);
    }
    value
        .as_object()
        .and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("expression"))
                .or_else(|| object.get("Expression"))
        })
        .and_then(Value::as_str)
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

fn parse_hash_files(expression: &str) -> Option<Vec<String>> {
    let inner = expression
        .trim()
        .strip_prefix("hashFiles(")?
        .strip_suffix(')')?;
    let mut patterns = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
            continue;
        }
        let quote = rest.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let value_start = quote.len_utf8();
        let value_end = rest[value_start..].find(quote)? + value_start;
        patterns.push(rest[value_start..value_end].to_string());
        rest = rest[value_end + quote.len_utf8()..].trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
        } else if !rest.is_empty() {
            return None;
        }
    }
    Some(patterns)
}

fn hash_files(workspace: &Path, patterns: &[String]) -> String {
    let Ok(globs) = build_globs(patterns) else {
        return String::new();
    };
    let mut matches = Vec::new();
    collect_hash_file_matches(workspace, workspace, &globs, &mut matches);
    matches.sort();
    matches.dedup();
    if matches.is_empty() {
        return String::new();
    }

    let mut aggregate = Sha256::new();
    for path in matches {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        aggregate.update(Sha256::digest(bytes));
    }
    hex_digest(aggregate.finalize().as_slice())
}

fn build_globs(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build().context("build hashFiles glob set")
}

fn collect_hash_file_matches(
    workspace: &Path,
    dir: &Path,
    globs: &globset::GlobSet,
    matches: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_hash_file_matches(workspace, &path, globs, matches);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = path.strip_prefix(workspace) else {
            continue;
        };
        if globs.is_match(normalize_path(relative)) {
            matches.push(path);
        }
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn expression_truthy(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty() || value == "false" || value == "0" || value == "null")
}

fn is_quoted(value: &str) -> bool {
    (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"'))
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
                if args
                    .iter()
                    .any(|arg| arg == "GITHUB_STATE=/__t/cache_state")
                {
                    fs::write(self.temp.join("cache_state"), "primaryKey=linux-cache\n")?;
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
                "::set-output name=answer::42\n::add-path::/opt/tool\n::add-mask::hidden\nhidden\n"
                    .to_string()
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
            env: Vec::new(),
            services: Vec::new(),
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
            continue_on_error: false,
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
            continue_on_error: false,
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
                continue_on_error: false,
            },
            ScriptStep {
                id: "step2".into(),
                script: "echo two".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
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
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("answer".to_string(), "42".to_string())].into(),
                    env: [("NAME".to_string(), "value".to_string())].into(),
                    path: vec!["/opt/tool".to_string()],
                    masks: vec!["secret".to_string()],
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env = state.step_env(&[("GITHUB_OUTPUT".into(), "/__t/out".into())]);

        assert!(env.contains(&("NAME".into(), "value".into())));
        assert!(env.contains(&("GITHUB_OUTPUT".into(), "/__t/out".into())));
        assert!(!env.iter().any(|(name, _)| name == "PATH"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.masks, vec!["secret"]);
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
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("tags".to_string(), "image:latest".to_string())].into(),
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env =
            state.resolve_env(&[("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into())]);

        assert_eq!(env, vec![("INPUT_TAGS".into(), "image:latest".into())]);
    }

    #[test]
    fn resolves_github_and_runner_context_expressions() {
        let state = JobExecutionState::new(&[
            ("GITHUB_ACTION".into(), "setup".into()),
            (
                "GITHUB_ACTION_PATH".into(),
                "/__a/actions_setup-node/v4".into(),
            ),
            ("GITHUB_ACTION_REF".into(), "v4".into()),
            (
                "GITHUB_ACTION_REPOSITORY".into(),
                "actions/setup-node".into(),
            ),
            (
                "GITHUB_EVENT_PATH".into(),
                "/__t/_github_workflow/event.json".into(),
            ),
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_REPOSITORY_OWNER".into(), "acme".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
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
                ("ACTION".into(), "${{ github.action }}".into()),
                ("ACTION_PATH".into(), "${{ github.action_path }}".into()),
                ("ACTION_REF".into(), "${{ github.action_ref }}".into()),
                (
                    "ACTION_REPOSITORY".into(),
                    "${{ github.action_repository }}".into(),
                ),
                ("EVENT_PATH".into(), "${{ github.event_path }}".into()),
                ("OWNER".into(), "${{ github.repository_owner }}".into()),
                ("SERVER_URL".into(), "${{ github.server_url }}".into()),
                ("DOCS_SITE_URL".into(), "${{ env.DOCS_SITE_URL }}".into()),
                ("WORKSPACE".into(), "${{ github.workspace }}".into()),
                ("OS".into(), "${{ runner.os }}".into()),
            ]),
            vec![
                ("INPUT_TOKEN".into(), "ghs_token".into()),
                ("ACTION".into(), "setup".into()),
                ("ACTION_PATH".into(), "/__a/actions_setup-node/v4".into()),
                ("ACTION_REF".into(), "v4".into()),
                ("ACTION_REPOSITORY".into(), "actions/setup-node".into()),
                (
                    "EVENT_PATH".into(),
                    "/__t/_github_workflow/event.json".into()
                ),
                ("OWNER".into(), "acme".into()),
                ("SERVER_URL".into(), "https://github.com".into()),
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
                (
                    "secrets".into(),
                    serde_json::json!({ "DOCKERHUB_TOKEN": "docker_secret" }),
                ),
                (
                    "github".into(),
                    serde_json::json!({
                        "event": {
                            "pull_request": { "number": 42 },
                            "workflow_run": {
                                "head_sha": "def456",
                                "head_branch": "main"
                            }
                        }
                    }),
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
        assert_eq!(
            state.resolve_expressions(
                "tool=${{ matrix.zigbuild && 'rust zig cargo:cargo-zigbuild' || 'rust' }}"
            ),
            "tool=rust zig cargo:cargo-zigbuild"
        );
        assert_eq!(
            state.resolve_expressions(
                "push=${{ (github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true') || (github.event_name == 'workflow_dispatch' && inputs.packages) }}"
            ),
            "push=bitcoin-processor-app"
        );
        assert_eq!(
            state.resolve_expressions(
                "fallback=${{ steps.dispatch.outputs.docs || needs.changes.outputs.bake-targets }}"
            ),
            "fallback=bitcoin-processor-app"
        );
        assert_eq!(
            state.resolve_expressions(
                "enabled=${{ !inputs.publish && github.event_name == 'workflow_dispatch' }}"
            ),
            "enabled=true"
        );
        assert_eq!(
            state.resolve_expressions("pr=${{ github.event.pull_request.number }}"),
            "pr=42"
        );
        assert_eq!(
            state.resolve_expressions("head=${{ github.event.workflow_run.head_sha }}"),
            "head=def456"
        );
        assert_eq!(
            state.resolve_expressions("token=${{ secrets.DOCKERHUB_TOKEN }}"),
            "token=docker_secret"
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
            continue_on_error: false,
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
    fn writes_github_event_payload_and_injects_event_path() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "event-step".into(),
            script: "cat \"$GITHUB_EVENT_PATH\"".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        })];
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "event": {
                    "pull_request": { "number": 42 },
                    "workflow_run": { "head_sha": "abc123" }
                }
            }),
        )];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"GITHUB_EVENT_PATH=/__t/_github_workflow/event.json".into()));
        assert_eq!(
            fs::read_to_string(temp.join("_github_workflow/event.json")).unwrap(),
            r#"{"pull_request":{"number":42},"workflow_run":{"head_sha":"abc123"}}"#
        );
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
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
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
    fn evaluates_job_outputs_from_final_step_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "producer".into(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        })];
        let job_outputs = serde_json::json!({
            "answer": "${{ steps.producer.outputs.answer }}",
            "fallback": "${{ steps.missing.outputs.value || 'default' }}",
            "empty": "${{ steps.missing.outputs.value }}"
        });
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.step_results.len(), 1);
        assert_eq!(summary.job_outputs["answer"], "42");
        assert_eq!(summary.job_outputs["fallback"], "default");
        assert!(!summary.job_outputs.contains_key("empty"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn materializes_composite_outputs_as_outer_step_outputs() {
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
                continue_on_error: false,
            }),
            ExecutableStep::CompositeOutputs {
                step_id: "composite".into(),
                outputs: [(
                    "artifact-id".to_string(),
                    "${{ steps.producer.outputs.answer }}".to_string(),
                )]
                .into(),
                condition: None,
            },
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo artifact=${{ steps.composite.outputs.artifact-id }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[1].state.outputs["artifact-id"], "42");
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "echo artifact=42\n"
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
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(StdoutCommandRunner::default());

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                &temp,
            )
            .unwrap();

        let results = &summary.step_results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["answer"], "42");
        assert_eq!(results[0].state.path, vec!["/opt/tool"]);
        assert_eq!(summary.step_logs[0].step_id, "producer");
        assert!(summary.step_logs[0].lines.contains(&"hidden".to_string()));
        assert_eq!(summary.step_logs[0].masks, vec!["hidden"]);
        assert_eq!(summary.step_logs[0].exit_code, 0);
        assert!(!summary.step_logs[0].skipped);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_hash_files_in_action_env() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("nested")).unwrap();
        fs::write(workspace.join("Cargo.lock"), "root-lock\n").unwrap();
        fs::write(workspace.join("nested/Cargo.lock"), "nested-lock\n").unwrap();
        fs::write(workspace.join("ignored.txt"), "ignored\n").unwrap();
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"root-lock\n"));
        expected_hash.update(Sha256::digest(b"nested-lock\n"));
        let expected = hex_digest(expected_hash.finalize().as_slice());
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "cache".into(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                post_container_path: None,
                action_container_path: "/__a/_actions/cache".into(),
                env: vec![(
                    "INPUT_KEY".into(),
                    "cargo-${{ hashFiles('**/Cargo.lock') }}".into(),
                )],
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert!(!expected.is_empty());
        assert!(executor.runner().calls[2]
            .1
            .contains(&format!("INPUT_KEY=cargo-{expected}")));
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
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo should-not-run".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.producer.outputs.answer == 'nope'".into()),
                continue_on_error: false,
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
    fn continue_on_error_keeps_failure_outcome_but_runs_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "sccache".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/sccache/dist/index.js".into(),
                    post_container_path: None,
                    action_container_path: "/__a/_actions/sccache".into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: true,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable-sccache".into(),
                script: "echo SCCACHE_GHA_ENABLED=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.sccache.outcome == 'success'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "next".into(),
                script: "echo next".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 0, 7, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].exit_code, 7);
        assert!(results[0].failure_ignored);
        assert!(results[1].skipped);
        assert_eq!(results[2].exit_code, 0);
        let exec_count = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| args.first().is_some_and(|arg| arg == "exec"))
            .count();
        assert_eq!(exec_count, 2);
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
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        state.apply(
            "disabled",
            &StepExecutionResult {
                exit_code: 0,
                skipped: true,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(state.evaluate_condition(Some("steps.sccache.outcome == 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.outcome != 'success'")));
        assert!(state.evaluate_condition(Some("runner.os == 'Linux'")));
        assert!(state.evaluate_condition(Some("success()")));
        assert!(!state.evaluate_condition(Some("failure()")));
        assert!(!state.evaluate_condition(Some("cancelled()")));
        assert!(state.evaluate_condition(Some("!cancelled()")));

        state.apply(
            "failed",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(!state.evaluate_condition(Some("success()")));
        assert!(state.evaluate_condition(Some("failure()")));
        assert!(state.evaluate_condition(Some("failure() && !cancelled()")));
        assert!(state.evaluate_condition(Some("always() && failure()")));
    }

    #[test]
    fn executes_javascript_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = JavaScriptActionInvocation {
            node: "node20".into(),
            main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
            post_container_path: None,
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
                continue_on_error: false,
            }),
            ExecutableStep::JavaScript {
                step_id: "action1".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
                    post_container_path: None,
                    action_container_path: "/__a/_actions/acme_action/v1".into(),
                    env: vec![
                        ("INPUT_NAME".into(), "value".into()),
                        ("TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "step2".into(),
                script: "echo done".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
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

    #[test]
    fn executes_javascript_post_actions_in_reverse_order() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "cache".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                    post_container_path: Some("/__a/_actions/cache/dist/save.js".into()),
                    action_container_path: "/__a/_actions/cache".into(),
                    env: vec![("INPUT_KEY".into(), "linux-cache".into())],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "login".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/docker_login/dist/main.js".into(),
                    post_container_path: Some("/__a/_actions/docker_login/dist/post.js".into()),
                    action_container_path: "/__a/_actions/docker_login".into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        let exec_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| args.first().is_some_and(|arg| arg == "exec"))
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert!(
            exec_calls[0].ends_with(&["node".into(), "/__a/_actions/cache/dist/restore.js".into()])
        );
        assert!(exec_calls[1].ends_with(&[
            "node".into(),
            "/__a/_actions/docker_login/dist/main.js".into()
        ]));
        assert!(exec_calls[2].ends_with(&[
            "node".into(),
            "/__a/_actions/docker_login/dist/post.js".into()
        ]));
        assert!(
            exec_calls[3].ends_with(&["node".into(), "/__a/_actions/cache/dist/save.js".into()])
        );
        assert!(exec_calls[3].contains(&"STATE_primaryKey=linux-cache".into()));
        fs::remove_dir_all(temp).unwrap();
    }
}

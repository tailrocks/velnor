#![allow(dead_code)]

use crate::{
    action::{
        DockerActionInvocation, JavaScriptActionInvocation, NativeActionAdapter,
        NativeActionInvocation,
    },
    checkout::{configure_safe_directory, execute_checkout, CheckoutPlan},
    container::{sccache_host, JobContainerSpec},
    script_step::{ScriptStep, ScriptStepPlan, StepAnnotation, StepCommandState},
    workflow_command::parse_workflow_commands,
};
use anyhow::{bail, Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc::UnboundedSender;

const DOCKER_MOUNT_CHECK_FILE: &str = ".velnor-mount-check";

// ANSI color helpers for Velnor-authored adapter output.
// GitHub's UI renders these as colored spans (ansifg-* classes).
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult>;

    fn run_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        let _ = env;
        self.run(program, args)
    }

    fn run_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
    ) -> Result<CommandResult> {
        let _ = stdin;
        self.run(program, args)
    }
}

pub fn render_context_expressions(value: &str, context_data: &[(String, Value)]) -> String {
    JobExecutionState::new_with_context(&[], context_data).resolve_expressions(value)
}

pub fn render_expressions_with_context(
    value: &str,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> String {
    JobExecutionState::new_with_context(base_env, context_data).resolve_expressions(value)
}

fn node_action_image(runtime: &str, fallback: &str) -> String {
    if !fallback.trim().is_empty() {
        return fallback.to_string();
    }
    match runtime.strip_prefix("node") {
        Some("20") => "node:20-bookworm".to_string(),
        Some("24") => "node:24-bookworm".to_string(),
        Some(major) if major.chars().all(|ch| ch.is_ascii_digit()) && !major.is_empty() => {
            format!("node:{major}")
        }
        _ => fallback.to_string(),
    }
}

#[derive(Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
        // Called from spawn_blocking context — synchronous blocking is fine here.
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

    fn run_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        let mut command = Command::new(program);
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command
            .output()
            .with_context(|| format!("run {program} {}", args.join(" ")))?;

        Ok(CommandResult {
            code: exit_code(output.status)?,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn run_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
    ) -> Result<CommandResult> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {program} {}", args.join(" ")))?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin
                .write_all(stdin.as_bytes())
                .with_context(|| format!("write stdin for {program} {}", args.join(" ")))?;
        }
        let output = child
            .wait_with_output()
            .with_context(|| format!("wait for {program} {}", args.join(" ")))?;

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
    pub environment_url: Option<String>,
    pub step_logs: Vec<StepLog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    pub step_id: String,
    pub display_name: String,
    pub order: i32,
    pub lines: Vec<String>,
    pub masks: Vec<String>,
    pub annotations: Vec<StepAnnotation>,
    pub telemetry: Vec<crate::script_step::StepCommandTelemetry>,
    pub exit_code: i32,
    pub skipped: bool,
    pub failure_ignored: bool,
    pub error_count: i32,
    pub warning_count: i32,
    pub notice_count: i32,
    /// GITHUB_STEP_SUMMARY content for this step (empty when none).
    /// Uploaded separately to the Results Service summary endpoint.
    pub summary: String,
}

#[derive(Debug, Clone)]
pub enum ExecutableStep {
    CompositeStart {
        step_id: String,
    },
    CompositeEnd {
        step_id: String,
    },
    Checkout(CheckoutPlan),
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        display_name: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    Docker {
        step_id: String,
        display_name: String,
        invocation: DockerActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    Native {
        step_id: String,
        display_name: String,
        invocation: NativeActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    CompositeOutputs {
        step_id: String,
        outputs: BTreeMap<String, String>,
        condition: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepStartEvent {
    pub step_id: String,
    pub display_name: String,
    pub order: i32,
}

#[derive(Debug, Clone)]
struct PostJavaScriptAction {
    step_id: String,
    display_name: String,
    invocation: JavaScriptActionInvocation,
    condition: Option<String>,
    continue_on_error: bool,
}

#[derive(Debug, Clone)]
struct PostNativeAction {
    step_id: String,
    display_name: String,
    invocation: NativeActionInvocation,
    condition: Option<String>,
    continue_on_error: bool,
}

impl ExecutableStep {
    fn id(&self) -> &str {
        match self {
            ExecutableStep::CompositeStart { step_id } => step_id,
            ExecutableStep::CompositeEnd { step_id } => step_id,
            ExecutableStep::Checkout(plan) => &plan.step_id,
            ExecutableStep::Script(step) => &step.id,
            ExecutableStep::JavaScript { step_id, .. } => step_id,
            ExecutableStep::Docker { step_id, .. } => step_id,
            ExecutableStep::Native { step_id, .. } => step_id,
            ExecutableStep::CompositeOutputs { step_id, .. } => step_id,
        }
    }

    fn condition(&self) -> Option<&str> {
        match self {
            ExecutableStep::CompositeStart { .. } => None,
            ExecutableStep::CompositeEnd { .. } => None,
            ExecutableStep::Checkout(plan) => plan.condition.as_deref(),
            ExecutableStep::Script(step) => step.condition.as_deref(),
            ExecutableStep::JavaScript { condition, .. } => condition.as_deref(),
            ExecutableStep::Docker { condition, .. } => condition.as_deref(),
            ExecutableStep::Native { condition, .. } => condition.as_deref(),
            ExecutableStep::CompositeOutputs { condition, .. } => condition.as_deref(),
        }
    }

    fn continue_on_error(&self) -> bool {
        match self {
            ExecutableStep::CompositeStart { .. } => false,
            ExecutableStep::CompositeEnd { .. } => false,
            ExecutableStep::Checkout(plan) => plan.continue_on_error,
            ExecutableStep::Script(step) => step.continue_on_error,
            ExecutableStep::JavaScript {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::Docker {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::Native {
                continue_on_error, ..
            } => *continue_on_error,
            ExecutableStep::CompositeOutputs { .. } => false,
        }
    }

    fn reports_timeline_start(&self) -> bool {
        !matches!(
            self,
            ExecutableStep::CompositeStart { .. }
                | ExecutableStep::CompositeEnd { .. }
                | ExecutableStep::CompositeOutputs { .. }
        )
    }

    pub fn display_name(&self) -> &str {
        match self {
            ExecutableStep::CompositeStart { step_id } => step_id,
            ExecutableStep::CompositeEnd { step_id } => step_id,
            ExecutableStep::Checkout(plan) => {
                if plan.display_name.is_empty() {
                    &plan.step_id
                } else {
                    &plan.display_name
                }
            }
            ExecutableStep::Script(step) => &step.display_name,
            ExecutableStep::JavaScript { display_name, .. } => display_name,
            ExecutableStep::Docker { display_name, .. } => display_name,
            ExecutableStep::Native { display_name, .. } => display_name,
            ExecutableStep::CompositeOutputs { step_id, .. } => step_id,
        }
    }
}

pub struct DockerScriptExecutor<R> {
    runner: R,
    step_start_sender: Option<UnboundedSender<StepStartEvent>>,
    step_log_sender: Option<UnboundedSender<StepLog>>,
    initial_order: i32,
}

impl<R> DockerScriptExecutor<R>
where
    R: CommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            step_start_sender: None,
            step_log_sender: None,
            initial_order: 0,
        }
    }

    pub fn with_initial_order(mut self, order: i32) -> Self {
        self.initial_order = order;
        self
    }

    pub fn with_step_start_sender(mut self, sender: UnboundedSender<StepStartEvent>) -> Self {
        self.step_start_sender = Some(sender);
        self
    }

    pub fn with_step_log_sender(mut self, sender: UnboundedSender<StepLog>) -> Self {
        self.step_log_sender = Some(sender);
        self
    }

    pub fn into_runner(self) -> R {
        self.runner
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn emit_step_started(
        &self,
        step_id: impl Into<String>,
        display_name: impl Into<String>,
        order: &mut i32,
    ) {
        *order += 1;
        let Some(sender) = &self.step_start_sender else {
            return;
        };
        let _ = sender.send(StepStartEvent {
            step_id: step_id.into(),
            display_name: display_name.into(),
            order: *order,
        });
    }

    fn emit_step_log(&self, log: &StepLog) {
        if let Some(sender) = &self.step_log_sender {
            let _ = sender.send(log.clone());
        }
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
        self.start_job_environment(container)?;

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
        self.execute_ordered_steps_with_completion(
            container,
            steps,
            base_env,
            context_data,
            job_outputs,
            None,
            temp_host,
        )
    }

    pub fn execute_ordered_steps_with_completion(
        &mut self,
        container: &JobContainerSpec,
        steps: &[ExecutableStep],
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        job_outputs: Option<&Value>,
        environment_url: Option<&Value>,
        temp_host: &Path,
    ) -> Result<JobExecutionSummary> {
        self.start_job_environment(container)?;

        let result = self.execute_ordered_steps_in_started_container(
            container,
            steps,
            base_env,
            context_data,
            job_outputs,
            environment_url,
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
        environment_url: Option<&Value>,
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
            temp_host,
        );
        let mut step_error = None;
        let mut post_actions = Vec::new();
        let mut native_post_actions = Vec::new();
        let mut timeline_order = self.initial_order;
        for step in steps {
            match step {
                ExecutableStep::CompositeStart { step_id } => {
                    state.push_composite(step_id);
                    continue;
                }
                ExecutableStep::CompositeEnd { step_id } => {
                    state.pop_composite(step_id);
                    continue;
                }
                _ => {}
            }
            let step_id = step.id().to_string();
            let step_state = state.with_step_action(&step_id);
            if !step_state.evaluate_condition(step.condition()) {
                if step.reports_timeline_start() {
                    self.emit_step_started(
                        step_id.clone(),
                        step.display_name(),
                        &mut timeline_order,
                    );
                }
                let result = StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState::default(),
                    skipped: true,
                    failure_ignored: false,
                    stdout: String::new(),
                    stderr: String::new(),
                };
                if step.reports_timeline_start() {
                    let log =
                        step_log_with_name(&step_id, step.display_name(), timeline_order, &result);
                    self.emit_step_log(&log);
                    step_logs.push(log);
                }
                state.apply(&step_id, &result);
                results.push(result);
                continue;
            }
            let mut post_registered = false;
            if let ExecutableStep::JavaScript {
                step_id,
                invocation,
                continue_on_error,
                ..
            } = step
            {
                if let Some(pre_container_path) = invocation.pre_container_path.as_deref() {
                    if step_state.evaluate_post_condition(invocation.pre_condition.as_deref()) {
                        if invocation.post_container_path.is_some() {
                            post_actions.push(PostJavaScriptAction {
                                step_id: step_id.clone(),
                                display_name: step.display_name().to_string(),
                                invocation: invocation.clone(),
                                condition: invocation.post_condition.clone(),
                                continue_on_error: *continue_on_error,
                            });
                            post_registered = true;
                        }
                        let pre_step_id = uuid::Uuid::new_v4().to_string();
                        self.emit_step_started(
                            pre_step_id.clone(),
                            step.display_name(),
                            &mut timeline_order,
                        );
                        let mut result = self.execute_javascript_action_in_started_container(
                            container,
                            step_id,
                            invocation,
                            pre_container_path,
                            &step_state.action_state_env(step_id),
                            temp_host,
                            &step_state,
                        )?;
                        let failed = result.exit_code != 0;
                        if failed && *continue_on_error {
                            result.failure_ignored = true;
                        }
                        let log = step_log_with_name(
                            &pre_step_id,
                            step.display_name(),
                            timeline_order,
                            &result,
                        );
                        self.emit_step_log(&log);
                        step_logs.push(log);
                        state.apply(step_id, &result);
                        results.push(result);
                        if failed {
                            continue;
                        }
                    }
                }
            }
            let step_state = state.with_step_action(&step_id);
            if step.reports_timeline_start() {
                self.emit_step_started(step_id.clone(), step.display_name(), &mut timeline_order);
            }
            let result = (|| match step {
                ExecutableStep::CompositeStart { .. } | ExecutableStep::CompositeEnd { .. } => {
                    unreachable!("composite boundary steps are handled before execution")
                }
                ExecutableStep::Checkout(plan) => {
                    self.execute_checkout_step(container, plan, &step_state)
                }
                ExecutableStep::Script(step) => {
                    let step = step_state.resolve_script_step(step);
                    let plan =
                        ScriptStepPlan::prepare_with_path(&step, temp_host, &step_state.path)?;
                    let mut env = step_state.step_env(&[]);
                    env.extend(step.env.iter().cloned());
                    env.extend(plan.env.iter().cloned());
                    let exec_args = container.exec_script_args(
                        &plan.script_container_path,
                        plan.shell,
                        &plan.working_directory_container,
                        &env,
                    );
                    let step_result = self.runner.run("docker", &exec_args)?;
                    let mut command_state = plan.collect_state()?;
                    command_state.merge(parse_workflow_commands_from_output(
                        &step_result.stdout,
                        &step_result.stderr,
                    ));
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
                    &step_state.action_state_env(step_id),
                    temp_host,
                    &step_state,
                ),
                ExecutableStep::Docker {
                    step_id,
                    invocation,
                    ..
                } => self.execute_docker_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    temp_host,
                    &step_state,
                ),
                ExecutableStep::Native {
                    step_id,
                    invocation,
                    ..
                } => self.execute_native_action_in_started_container(
                    container,
                    step_id,
                    invocation,
                    &step_state,
                ),
                ExecutableStep::CompositeOutputs { outputs, .. } => Ok(StepExecutionResult {
                    exit_code: 0,
                    state: StepCommandState {
                        outputs: step_state.evaluate_named_outputs(outputs),
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
                    if let ExecutableStep::JavaScript {
                        invocation,
                        continue_on_error,
                        ..
                    } = step
                    {
                        if invocation.post_container_path.is_some() && !post_registered {
                            post_actions.push(PostJavaScriptAction {
                                step_id: step_id.clone(),
                                display_name: step.display_name().to_string(),
                                invocation: invocation.clone(),
                                condition: invocation.post_condition.clone(),
                                continue_on_error: *continue_on_error,
                            });
                        }
                    }
                    if let ExecutableStep::Native {
                        invocation,
                        continue_on_error,
                        ..
                    } = step
                    {
                        if let Some(condition) = native_post_condition(invocation.adapter) {
                            native_post_actions.push(PostNativeAction {
                                step_id: step_id.clone(),
                                display_name: step.display_name().to_string(),
                                invocation: invocation.clone(),
                                condition: Some(condition.to_string()),
                                continue_on_error: *continue_on_error,
                            });
                        }
                    }
                    if failed && step.continue_on_error() {
                        result.failure_ignored = true;
                    }
                    if step.reports_timeline_start() {
                        let log = step_log_with_name(
                            &step_id,
                            step.display_name(),
                            timeline_order,
                            &result,
                        );
                        self.emit_step_log(&log);
                        step_logs.push(log);
                    }
                    state.apply(&step_id, &result);
                    results.push(result);
                }
                Err(error) => {
                    step_error = Some(error);
                    break;
                }
            }
        }
        for post_action in native_post_actions.into_iter().rev() {
            if !state.evaluate_post_condition(post_action.condition.as_deref()) {
                continue;
            }
            let post_step_id = uuid::Uuid::new_v4().to_string();
            self.emit_step_started(
                post_step_id.clone(),
                {
                    let n = post_action
                        .display_name
                        .strip_prefix("Run ")
                        .unwrap_or(&post_action.display_name);
                    format!("Post Run {n}")
                },
                &mut timeline_order,
            );
            let result = self.execute_native_post_action(
                container,
                &post_action.step_id,
                &post_action.invocation,
                &state,
            );
            match result {
                Ok(mut result) => {
                    if result.exit_code != 0 && post_action.continue_on_error {
                        result.failure_ignored = true;
                    }
                    let post_name = {
                        let n = post_action
                            .display_name
                            .strip_prefix("Run ")
                            .unwrap_or(&post_action.display_name);
                        format!("Post Run {n}")
                    };
                    let log =
                        step_log_with_name(&post_step_id, &post_name, timeline_order, &result);
                    self.emit_step_log(&log);
                    step_logs.push(log);
                    state.apply(&post_step_id, &result);
                    results.push(result);
                }
                Err(error) => {
                    if step_error.is_none() {
                        step_error = Some(error);
                    }
                }
            }
        }
        for post_action in post_actions.into_iter().rev() {
            if !state.evaluate_post_condition(post_action.condition.as_deref()) {
                continue;
            }
            let js_post_step_id = uuid::Uuid::new_v4().to_string();
            self.emit_step_started(
                js_post_step_id.clone(),
                {
                    let n = post_action
                        .display_name
                        .strip_prefix("Run ")
                        .unwrap_or(&post_action.display_name);
                    format!("Post Run {n}")
                },
                &mut timeline_order,
            );
            let result = self.execute_javascript_action_in_started_container(
                container,
                &js_post_step_id,
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
                Ok(mut result) => {
                    if result.exit_code != 0 && post_action.continue_on_error {
                        result.failure_ignored = true;
                    }
                    let post_name_js = {
                        let n = post_action
                            .display_name
                            .strip_prefix("Run ")
                            .unwrap_or(&post_action.display_name);
                        format!("Post Run {n}")
                    };
                    let log = step_log_with_name(
                        &js_post_step_id,
                        &post_name_js,
                        timeline_order,
                        &result,
                    );
                    self.emit_step_log(&log);
                    step_logs.push(log);
                    state.apply(&js_post_step_id, &result);
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
            environment_url: evaluate_environment_url(environment_url, &state),
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
        self.start_job_environment(container)?;

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
                display_name: String::new(),
                script: String::new(),
                shell: crate::container::Shell::Sh,
                working_directory_container: "/__w".to_string(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            },
            temp_host,
        )?;
        let action_state = state.with_env(action_context_env(&action.env));
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        env.extend(action_state_env.iter().cloned());
        env.extend(command_files.env.iter().cloned());
        rewrite_command_file_env_for_action_container(&mut env);
        let node_image = node_action_image(&action.node, &container.node_action_image);
        let exec_args = container.run_node_action_args(
            "/__w",
            &env,
            &state.path,
            &node_image,
            entrypoint_container_path,
        );
        let step_result = self.runner.run("docker", &exec_args)?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands_from_output(
            &step_result.stdout,
            &step_result.stderr,
        ));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
            failure_ignored: false,
            stdout: step_result.stdout,
            stderr: step_result.stderr,
        })
    }

    fn execute_checkout_step(
        &mut self,
        container: &JobContainerSpec,
        plan: &CheckoutPlan,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let mut plan = plan.clone();
        if let Some(version) = plan.version.as_mut() {
            *version = state.resolve_expressions(version);
        }
        execute_checkout(&mut self.runner, &plan)?;
        configure_safe_directory(
            &container.home_host,
            &container.workspace_host,
            &plan.destination,
        )?;
        Ok(StepExecutionResult {
            exit_code: 0,
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn execute_docker_action_in_started_container(
        &mut self,
        container: &JobContainerSpec,
        step_id: &str,
        action: &DockerActionInvocation,
        temp_host: &Path,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        if let (Some(context_host), Some(dockerfile_host)) =
            (&action.build_context_host, &action.dockerfile_host)
        {
            self.run_docker(&container.build_docker_action_args(
                &action.image,
                dockerfile_host,
                context_host,
            ))?;
        }
        let command_files = ScriptStepPlan::prepare(
            &ScriptStep {
                id: step_id.to_string(),
                display_name: String::new(),
                script: String::new(),
                shell: crate::container::Shell::Sh,
                working_directory_container: "/__w".to_string(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            },
            temp_host,
        )?;
        let action_state = state.with_env(action_context_env(&action.env));
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        set_env_value(&mut env, "GITHUB_WORKSPACE", "/github/workspace");
        set_env_value(&mut env, "RUNNER_TEMP", "/github/runner_temp");
        env.extend(command_files.env.iter().cloned());
        rewrite_command_file_env_for_action_container(&mut env);
        let entrypoint = action
            .entrypoint
            .as_ref()
            .map(|value| state.resolve_expressions(value));
        let args = action
            .args
            .iter()
            .map(|value| state.resolve_expressions(value))
            .collect::<Vec<_>>();
        let exec_args = container.run_docker_action_args(
            "/github/workspace",
            &env,
            &action.image,
            entrypoint.as_deref(),
            &args,
        );
        let step_result = self.runner.run("docker", &exec_args)?;
        let mut state = command_files.collect_state()?;
        state.merge(parse_workflow_commands_from_output(
            &step_result.stdout,
            &step_result.stderr,
        ));
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
            skipped: false,
            failure_ignored: false,
            stdout: step_result.stdout,
            stderr: step_result.stderr,
        })
    }

    fn execute_native_action_in_started_container(
        &mut self,
        _container: &JobContainerSpec,
        step_id: &str,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        match action.adapter {
            NativeActionAdapter::Cache => native_cache(action, state),
            NativeActionAdapter::UploadArtifact => native_upload_artifact(action, state),
            NativeActionAdapter::DownloadArtifact => native_download_artifact(action, state),
            NativeActionAdapter::UploadPagesArtifact => native_upload_pages_artifact(action, state),
            NativeActionAdapter::DeployPages => Ok(native_deploy_pages(action, state)),
            NativeActionAdapter::Mise => self.native_mise(_container, action, state),
            NativeActionAdapter::Sccache => self.native_sccache(_container, action, state),
            NativeActionAdapter::SetupMold => self.native_setup_mold(_container, action, state),
            NativeActionAdapter::SetupJust => self.native_setup_just(_container, action, state),
            NativeActionAdapter::RustCache => native_rust_cache(action, state),
            NativeActionAdapter::Renovate => self.native_renovate(_container, action, state),
            NativeActionAdapter::GitHubRuntimeExport => {
                Ok(native_github_runtime_export(action, state))
            }
            NativeActionAdapter::PathsFilter => self.native_paths_filter(action, state),
            NativeActionAdapter::DockerSetupBuildx => {
                self.native_docker_setup_buildx(action, state)
            }
            NativeActionAdapter::DockerLogin => self.native_docker_login(action, state),
            NativeActionAdapter::DockerMetadata => Ok(native_docker_metadata(action, state)),
            NativeActionAdapter::DockerBuildPush => self.native_docker_build_push(action, state),
            NativeActionAdapter::DockerBake => self.native_docker_bake(action, state),
            _ => {
                bail!(
                    "native action adapter {:?} for step '{}' is declared but not implemented yet",
                    action.adapter,
                    step_id
                )
            }
        }
    }

    fn execute_native_post_action(
        &mut self,
        _container: &JobContainerSpec,
        step_id: &str,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        match action.adapter {
            NativeActionAdapter::Cache => native_cache_save(step_id, action, state),
            NativeActionAdapter::RustCache => native_rust_cache_save(step_id, action, state),
            // Sccache post: show stats then stop server. Soft-fail if not running.
            NativeActionAdapter::Sccache => {
                let result = self.native_shell(
                    _container,
                    state,
                    "sccache --show-stats 2>/dev/null || true; sccache --stop-server 2>/dev/null || true",
                )?;
                Ok(native_command_result(result, StepCommandState::default()))
            }
            _ => bail!(
                "native action adapter {:?} for step '{}' does not have a post action",
                action.adapter,
                step_id
            ),
        }
    }

    fn native_mise(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let install = input_truthy(&native_input_or(&action_state, action, "install", "true"));
        let install_args = native_input(action, &action_state, "install_args");
        let working_directory = native_input(action, &action_state, "working_directory");
        let script = setup_mise_script(install, &install_args, &working_directory);
        let result = self.native_shell(container, state, &script)?;
        Ok(native_command_result(
            result,
            StepCommandState {
                env: [
                    ("MISE_DATA_DIR".to_string(), "/opt/mise".to_string()),
                    ("MISE_CACHE_DIR".to_string(), "/opt/mise/cache".to_string()),
                    (
                        "MISE_CONFIG_DIR".to_string(),
                        "/opt/mise/config".to_string(),
                    ),
                ]
                .into(),
                path: vec![
                    // Mise shims for all mise-managed tools
                    "/opt/mise/shims".to_string(),
                    // Cargo bin for Rust tools pre-installed in the image (cargo, rustc, nextest)
                    "/root/.cargo/bin".to_string(),
                ],
                ..StepCommandState::default()
            },
        ))
    }

    fn native_sccache(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(
            container,
            state,
            // Exit 0 if sccache found and server started, or if not found (127 or 1).
            "sccache_bin=$(command -v sccache 2>/dev/null || true); [ -n \"$sccache_bin\" ] && sccache --start-server || true",
        )?;
        Ok(native_command_result(result, StepCommandState::default()))
    }

    fn native_setup_mold(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &setup_mold_script())?;
        Ok(native_command_result(result, StepCommandState::default()))
    }

    fn native_setup_just(
        &mut self,
        container: &JobContainerSpec,
        _action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let result = self.native_shell(container, state, &setup_just_script())?;
        Ok(native_command_result(
            result,
            StepCommandState {
                path: vec!["/root/.cargo/bin".to_string()],
                ..StepCommandState::default()
            },
        ))
    }

    fn native_shell(
        &mut self,
        container: &JobContainerSpec,
        state: &JobExecutionState,
        script: &str,
    ) -> Result<CommandResult> {
        let env = state.step_env(&[]);
        // Build PATH from accumulated state paths plus the container's default paths.
        // We must set it explicitly because OrbStack (on macOS) injects the host
        // macOS PATH into all container processes, which overrides the Dockerfile ENV.
        // Use HOME=/root so mise can locate its global config (written to /root at image
        // build time). RUSTUP_HOME and CARGO_HOME are set explicitly to separate the
        // toolchain store from the per-job artifact cache.
        let container_default_path =
            "/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
        let path_entries: Vec<&str> = state
            .path
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(container_default_path))
            .collect();
        let path = path_entries.join(":");
        let wrapped = format!(
            "export HOME=/root; export RUSTUP_HOME=/root/.rustup; export CARGO_HOME=/root/.cargo; export PATH={path}; {script}"
        );
        let args = container.exec_process_args(
            "/__w",
            &env,
            &["sh".to_string(), "-c".to_string(), wrapped],
        );
        self.runner.run("docker", &args)
    }

    fn native_renovate(
        &mut self,
        container: &JobContainerSpec,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let version = native_input(action, &action_state, "renovate-version");
        let image = native_input(action, &action_state, "renovate-image");
        let image = if !image.is_empty() {
            image
        } else if !version.is_empty() {
            format!("ghcr.io/renovatebot/renovate:{version}")
        } else {
            "ghcr.io/renovatebot/renovate:latest".to_string()
        };
        let token = native_input(action, &action_state, "token");
        let mut env = action_state.step_env(&[]);
        env.extend(action_state.resolve_env(&action.env));
        if !token.is_empty() {
            set_env_value(&mut env, "RENOVATE_TOKEN", &token);
            set_env_value(&mut env, "INPUT_TOKEN", &token);
        }
        if let Some(repository) = action_state.env.get("GITHUB_REPOSITORY") {
            set_env_value(&mut env, "RENOVATE_REPOSITORIES", repository);
        }
        let args = container.run_docker_action_args("/github/workspace", &env, &image, None, &[]);
        Ok(native_command_result(
            self.runner.run("docker", &args)?,
            StepCommandState::default(),
        ))
    }

    fn native_docker_setup_buildx(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let name = native_input_or(&action_state, action, "name", "velnor-builder");
        let driver = native_input_or(&action_state, action, "driver", "docker-container");
        let inspect_args = vec!["buildx".to_string(), "inspect".to_string(), name.clone()];
        let inspect_result = self.runner.run("docker", &inspect_args)?;
        let result = if inspect_result.code == 0 {
            let use_args = vec!["buildx".to_string(), "use".to_string(), name.clone()];
            self.runner.run("docker", &use_args)?
        } else {
            let mut args = vec![
                "buildx".to_string(),
                "create".to_string(),
                "--name".to_string(),
                name.clone(),
                "--driver".to_string(),
                driver,
                "--use".to_string(),
            ];
            if input_truthy(&native_input_or(&action_state, action, "install", "false")) {
                args.push("--bootstrap".to_string());
            }
            self.runner.run("docker", &args)?
        };
        Ok(native_command_result(
            result,
            StepCommandState {
                outputs: [
                    ("name".to_string(), name.clone()),
                    (
                        "driver".to_string(),
                        native_input_or(&action_state, action, "driver", "docker-container"),
                    ),
                    (
                        "platforms".to_string(),
                        "linux/amd64,linux/arm64".to_string(),
                    ),
                ]
                .into(),
                env: [("BUILDX_BUILDER".to_string(), name)].into(),
                ..StepCommandState::default()
            },
        ))
    }

    fn native_docker_login(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let registry = native_input_or(
            &action_state,
            action,
            "registry",
            "https://index.docker.io/v1/",
        );
        let username = native_input(action, &action_state, "username");
        let password = native_input(action, &action_state, "password");
        let args = vec![
            "login".to_string(),
            registry,
            "--username".to_string(),
            username,
            "--password-stdin".to_string(),
        ];
        Ok(native_command_result(
            self.runner.run_with_stdin("docker", &args, &password)?,
            StepCommandState::default(),
        ))
    }

    fn native_docker_build_push(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let context = native_input_or(&action_state, action, "context", ".");
        let mut args = vec!["buildx".to_string(), "build".to_string()];
        push_arg(
            &mut args,
            "--file",
            &native_input(action, &action_state, "file"),
        );
        push_arg(
            &mut args,
            "--platform",
            &native_input(action, &action_state, "platforms"),
        );
        for tag in input_values(&native_input(action, &action_state, "tags")) {
            push_arg(&mut args, "--tag", &tag);
        }
        for label in input_values(&native_input(action, &action_state, "labels")) {
            push_arg(&mut args, "--label", &label);
        }
        for cache in input_values(&native_input(action, &action_state, "cache-from")) {
            push_arg(&mut args, "--cache-from", &cache);
        }
        for cache in input_values(&native_input(action, &action_state, "cache-to")) {
            push_arg(&mut args, "--cache-to", &cache);
        }
        if input_truthy(&native_input(action, &action_state, "push")) {
            args.push("--push".to_string());
        }
        if input_truthy(&native_input(action, &action_state, "load")) {
            args.push("--load".to_string());
        }
        args.push(host_context_path(&action_state, &context));
        let env = action_state.step_env(&[]);
        Ok(native_command_result(
            self.runner.run_with_env("docker", &args, &env)?,
            StepCommandState::default(),
        ))
    }

    fn native_docker_bake(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let action_state = state.with_env(state.resolve_env(&action.env));
        let mut args = vec!["buildx".to_string(), "bake".to_string()];
        for file in input_values(&native_input(action, &action_state, "files")) {
            push_arg(
                &mut args,
                "--file",
                &host_context_path(&action_state, &file),
            );
        }
        for set in input_values(&native_input(action, &action_state, "set")) {
            push_arg(&mut args, "--set", &set);
        }
        if input_truthy(&native_input(action, &action_state, "push")) {
            args.push("--push".to_string());
        }
        args.extend(input_values(&native_input(
            action,
            &action_state,
            "targets",
        )));
        let env = action_state.step_env(&[]);
        Ok(native_command_result(
            self.runner.run_with_env("docker", &args, &env)?,
            StepCommandState::default(),
        ))
    }

    fn native_paths_filter(
        &mut self,
        action: &NativeActionInvocation,
        state: &JobExecutionState,
    ) -> Result<StepExecutionResult> {
        let filters = parse_paths_filter_rules(
            action
                .inputs
                .get("filters")
                .map(|value| state.resolve_expressions(value))
                .as_deref()
                .unwrap_or_default(),
        )?;
        let changed_files = self.changed_files_for_paths_filter(state)?;
        let mut outputs = BTreeMap::new();
        let mut matched_names = Vec::new();

        for (name, patterns) in filters {
            let globs = build_globs(&patterns)
                .with_context(|| format!("build paths-filter globs for '{name}'"))?;
            let matches = changed_files
                .iter()
                .filter(|file| globs.is_match(file.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let matched = !matches.is_empty();
            if matched {
                matched_names.push(name.clone());
            }
            outputs.insert(name.clone(), matched.to_string());
            outputs.insert(format!("{name}_count"), matches.len().to_string());
            outputs.insert(format!("{name}_files"), matches.join("\n"));
        }
        outputs.insert(
            "changes".to_string(),
            serde_json::to_string(&matched_names).context("serialize paths-filter changes")?,
        );

        Ok(StepExecutionResult {
            exit_code: 0,
            state: StepCommandState {
                outputs,
                ..StepCommandState::default()
            },
            skipped: false,
            failure_ignored: false,
            stdout: changed_files
                .iter()
                .map(|file| format!("changed: {file}"))
                .collect::<Vec<_>>()
                .join("\n"),
            stderr: String::new(),
        })
    }

    fn changed_files_for_paths_filter(&mut self, state: &JobExecutionState) -> Result<Vec<String>> {
        let workspace = state
            .workspace_host
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("paths-filter requires a workspace"))?;
        let base = paths_filter_base_ref(state);
        let head = paths_filter_head_ref(state);
        let mut args = vec![
            "-C".to_string(),
            workspace.display().to_string(),
            "diff".to_string(),
            "--name-only".to_string(),
        ];
        args.push(match (base.as_deref(), head.as_deref()) {
            (Some(base), Some(head)) if !base.is_empty() && !head.is_empty() => {
                format!("{base}..{head}")
            }
            (Some(base), _) if !base.is_empty() => format!("{base}..HEAD"),
            _ => "HEAD".to_string(),
        });
        let result = self.runner.run("git", &args)?;
        if result.code != 0 {
            bail!(
                "git {} failed with code {}: {}",
                args.join(" "),
                result.code,
                result.stderr
            );
        }
        let mut files = result
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(files)
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

    fn start_job_environment(&mut self, container: &JobContainerSpec) -> Result<()> {
        if let Err(error) = self.start_job_environment_once(container) {
            eprintln!("Docker job environment start failed, removing stale resources: {error:#}");
            self.cleanup_stale(container);
            self.start_job_environment_once(container)?;
        }
        Ok(())
    }

    fn start_job_environment_once(&mut self, container: &JobContainerSpec) -> Result<()> {
        fs::create_dir_all(container.temp_host.join("_github_workflow")).with_context(|| {
            format!(
                "create GitHub workflow directory under {}",
                container.temp_host.display()
            )
        })?;
        fs::create_dir_all(sccache_host(&container.temp_host)).with_context(|| {
            format!(
                "create shared sccache directory for {}",
                container.temp_host.display()
            )
        })?;
        self.run_docker(&container.create_network_args())?;
        for service in &container.services {
            self.run_docker(&service.start_args())?;
            self.wait_for_service(service)?;
        }
        self.run_docker(&container.start_args())?;
        if container.verify_bind_mounts {
            self.verify_bind_mounts(container)?;
        }
        Ok(())
    }

    fn verify_bind_mounts(&mut self, container: &JobContainerSpec) -> Result<()> {
        let marker = container.temp_host.join(DOCKER_MOUNT_CHECK_FILE);
        fs::write(&marker, "velnor\n")
            .with_context(|| format!("write Docker bind-mount marker {}", marker.display()))?;

        let args = container.exec_process_args(
            "/",
            &[],
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}"),
            ],
        );
        let result = self.runner.run("docker", &args)?;
        fs::remove_file(&marker).ok();
        if result.code != 0 {
            bail!(
                "Docker daemon cannot see Velnor bind-mounted work directories. \
                 Expected host temp '{}' to appear at '/__t' in container '{}'. \
                 Use a local Docker daemon or set --work-dir/--config-dir to a path visible to the daemon. stderr: {}",
                container.temp_host.display(),
                container.name,
                result.stderr
            );
        }
        Ok(())
    }

    fn cleanup_stale(&mut self, container: &JobContainerSpec) {
        self.run_docker(&container.remove_container_args()).ok();
        for service in container.services.iter().rev() {
            self.run_docker(&service.remove_args()).ok();
        }
        self.run_docker(&container.remove_network_args()).ok();
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

    fn wait_for_service(&mut self, service: &crate::container::ServiceContainerSpec) -> Result<()> {
        for _ in 0..30 {
            let result = self.run_docker(&service.health_status_args())?;
            match result.stdout.trim() {
                "healthy" | "running" | "" => return Ok(()),
                "exited" | "dead" => {
                    bail!(
                        "service container '{}' stopped before becoming ready",
                        service.name
                    )
                }
                _ => thread::sleep(Duration::from_secs(1)),
            }
        }
        bail!("service container '{}' did not become ready", service.name)
    }
}

fn action_context_env(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "GITHUB_ACTION"
                    | "GITHUB_ACTION_PATH"
                    | "GITHUB_ACTION_REPOSITORY"
                    | "GITHUB_ACTION_REF"
            )
        })
        .cloned()
        .collect()
}

fn set_env_value(env: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = env.iter_mut().find(|(key, _)| key == name) {
        *existing = value.to_string();
    } else {
        env.push((name.to_string(), value.to_string()));
    }
}

fn rewrite_command_file_env_for_action_container(env: &mut [(String, String)]) {
    for (name, value) in env {
        if matches!(
            name.as_str(),
            "GITHUB_OUTPUT" | "GITHUB_ENV" | "GITHUB_PATH" | "GITHUB_STATE" | "GITHUB_STEP_SUMMARY"
        ) {
            if let Some(file_name) = value.strip_prefix("/__t/") {
                *value = format!("/github/file_commands/{file_name}");
            }
        }
    }
}

fn setup_mise_script(install: bool, install_args: &str, working_directory: &str) -> String {
    let install_args = shell_single_quote(install_args);
    let working_directory = shell_single_quote(working_directory);
    let install_flag = if install { "1" } else { "" };
    format!(
        r#"set -e
# Use the euid's home (/root) so rustup-init doesn't fail the $HOME vs euid check.
export HOME=/root
bin="/opt/mise/bin"
mise_home="/opt/mise"
mkdir -p "$bin" "$mise_home/shims" "$mise_home/cache" "$mise_home/config" "/root/.cargo/bin"
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | MISE_INSTALL_PATH="$bin/mise" sh
fi
export PATH="$bin:$mise_home/shims:/root/.cargo/bin:$PATH"
export MISE_DATA_DIR="$mise_home"
export MISE_CACHE_DIR="$mise_home/cache"
export MISE_CONFIG_DIR="$mise_home/config"
# Ensure mise itself can find cargo for 'cargo:' backend tools.
export CARGO_HOME=/github/home/.cargo
export RUSTUP_HOME=/root/.rustup
install_args={install_args}
working_directory={working_directory}
if [ -n "$working_directory" ]; then
  cd "$working_directory"
fi
if [ -n "{install_flag}" ]; then
  # Trust the workspace config so mise actually reads mise.toml from the checkout.
  for f in "/__w/mise.toml" "/__w/.mise.toml" "/__w/.mise/config.toml"; do
    [ -f "$f" ] && mise trust "$f" 2>/dev/null || true
  done
  mise trust --all 2>/dev/null || true
  if [ -n "$install_args" ]; then
    mise install --verbose $install_args
  else
    mise install --verbose
  fi
else
  command -v mise >/dev/null 2>&1
fi
echo "mise install completed, cargo: $(command -v cargo 2>/dev/null || echo 'not found')"
mise --version
"#,
    )
}

fn setup_mold_script() -> String {
    r#"set -e
if ! command -v mold >/dev/null 2>&1; then
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y --no-install-recommends mold
  else
    echo "mold is not installed and apt-get is unavailable" >&2
    exit 1
  fi
fi
mold --version
# Wire mold as the Cargo linker (mirrors rui314/setup-mold upstream behavior).
CARGO_CFG="${CARGO_HOME:-$HOME/.cargo}/config.toml"
mkdir -p "$(dirname "$CARGO_CFG")"
if ! grep -qF '[target.x86_64-unknown-linux-gnu]' "$CARGO_CFG" 2>/dev/null; then
  cat >> "$CARGO_CFG" <<'MOLDEOF'
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
MOLDEOF
fi
if ! grep -qF '[target.aarch64-unknown-linux-gnu]' "$CARGO_CFG" 2>/dev/null; then
  cat >> "$CARGO_CFG" <<'MOLDEOF'
[target.aarch64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
MOLDEOF
fi
"#
    .to_string()
}

fn setup_just_script() -> String {
    r#"set -e
if command -v just >/dev/null 2>&1; then
  just --version
  exit 0
fi
if command -v apt-get >/dev/null 2>&1; then
  apt-get update
  apt-get install -y --no-install-recommends just
elif command -v cargo >/dev/null 2>&1; then
  export CARGO_HOME="${CARGO_HOME:-/github/home/.cargo}"
  mkdir -p "$CARGO_HOME/bin"
  cargo install just --locked
else
  echo "just is not installed and neither apt-get nor cargo is available" >&2
  exit 1
fi
just --version
"#
    .to_string()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn native_post_condition(adapter: NativeActionAdapter) -> Option<&'static str> {
    match adapter {
        NativeActionAdapter::Cache => Some("success()"),
        NativeActionAdapter::RustCache => Some("success() || env.CACHE_ON_FAILURE == 'true'"),
        // Sccache post step stops the server (always run, matches GitHub's behavior).
        NativeActionAdapter::Sccache => Some("always()"),
        _ => None,
    }
}

fn native_cache(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "key");
    let path = native_input(action, &action_state, "path");
    let restore_keys = native_input(action, &action_state, "restore-keys");
    let fail_on_cache_miss =
        input_truthy(&native_input(action, &action_state, "fail-on-cache-miss"));
    let lookup_only = input_truthy(&native_input(action, &action_state, "lookup-only"));
    let t0 = Instant::now();
    let matched_key = find_cache_match(&action_state, &key, &restore_keys)?;
    if let Some(matched_key) = &matched_key {
        if !lookup_only {
            restore_cache_paths(&action_state, matched_key, &path)?;
        }
    }
    let restore_ms = t0.elapsed().as_millis();
    let exact_hit = matched_key.as_deref() == Some(key.as_str());

    let mut outputs = BTreeMap::new();
    outputs.insert(
        "cache-hit".to_string(),
        matched_key
            .as_ref()
            .map(|_| exact_hit.to_string())
            .unwrap_or_default(),
    );
    outputs.insert("cache-primary-key".to_string(), key.clone());
    outputs.insert(
        "cache-matched-key".to_string(),
        matched_key.clone().unwrap_or_default(),
    );

    let mut state_values = BTreeMap::new();
    if !key.is_empty() {
        state_values.insert("primaryKey".to_string(), key.clone());
    }
    if let Some(matched_key) = &matched_key {
        state_values.insert("matchedKey".to_string(), matched_key.clone());
    }

    let mut stdout = String::new();
    if let Some(matched_key) = &matched_key {
        if lookup_only {
            stdout.push_str(&format!(
                "{ANSI_GREEN}Cache lookup found key: {matched_key}{ANSI_RESET}\n"
            ));
        } else {
            stdout.push_str(&format!(
                "{ANSI_GREEN}Cache restored from key: {matched_key}{ANSI_RESET} ({restore_ms}ms)\n"
            ));
        }
    } else {
        stdout.push_str(&format!("{ANSI_YELLOW}Cache not found for input keys: "));
        stdout.push_str(&cache_lookup_keys(&key, &restore_keys).join(", "));
        stdout.push_str(&format!("{ANSI_RESET}\n"));
    }
    if !path.is_empty() {
        stdout.push_str(&format!("{ANSI_CYAN}Cache path: {path}{ANSI_RESET}\n"));
    }

    Ok(StepExecutionResult {
        exit_code: if fail_on_cache_miss && matched_key.is_none() {
            1
        } else {
            0
        },
        state: StepCommandState {
            outputs,
            state: state_values,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr: if fail_on_cache_miss && matched_key.is_none() {
            "Cache not found and fail-on-cache-miss is true\n".to_string()
        } else {
            String::new()
        },
    })
}

fn native_rust_cache(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let shared_key = native_cache_key(action, &action_state, "shared-key");
    let cache_directories = native_input(action, &action_state, "cache-directories");
    let cache_on_failure = native_input_or(&action_state, action, "cache-on-failure", "false");
    let t0 = Instant::now();
    let matched = find_cache_match(&action_state, &shared_key, "")?;
    if let Some(matched_key) = &matched {
        restore_cache_paths(&action_state, matched_key, &cache_directories)?;
    }
    let restore_ms = t0.elapsed().as_millis();
    let mut outputs = BTreeMap::new();
    outputs.insert("cache-hit".to_string(), matched.is_some().to_string());
    if !shared_key.is_empty() {
        outputs.insert("cache-primary-key".to_string(), shared_key.clone());
    }
    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            env: [("CACHE_ON_FAILURE".to_string(), cache_on_failure)].into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: matched.map_or_else(
            || {
                format!(
                    "{ANSI_YELLOW}Rust cache miss for shared key '{shared_key}'{ANSI_RESET}\n"
                )
            },
            |key| {
                format!(
                    "{ANSI_GREEN}Rust cache restored from shared key '{key}'{ANSI_RESET} ({restore_ms}ms)\n"
                )
            },
        ),
        stderr: String::new(),
    })
}

fn native_cache_save(
    step_id: &str,
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "key");
    let path = native_input(action, &action_state, "path");
    let exact_hit = state
        .outputs
        .get(step_id)
        .and_then(|outputs| outputs.get("cache-hit"))
        .is_some_and(|value| value == "true");
    save_cache_result(&action_state, &key, &path, exact_hit)
}

fn native_rust_cache_save(
    step_id: &str,
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let key = native_cache_key(action, &action_state, "shared-key");
    let path = native_input(action, &action_state, "cache-directories");
    let exact_hit = state
        .outputs
        .get(step_id)
        .and_then(|outputs| outputs.get("cache-hit"))
        .is_some_and(|value| value == "true");
    save_cache_result(&action_state, &key, &path, exact_hit)
}

fn cache_lookup_keys(key: &str, restore_keys: &str) -> Vec<String> {
    std::iter::once(key)
        .chain(restore_keys.lines().map(str::trim))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn native_cache_key(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
    name: &str,
) -> String {
    native_input(action, state, name).trim().to_string()
}

fn find_cache_match(
    state: &JobExecutionState,
    key: &str,
    restore_keys: &str,
) -> Result<Option<String>> {
    let store = cache_store_dir(state)?;
    if key.is_empty() || !store.exists() {
        return Ok(None);
    }
    let exact = store.join(sanitize_artifact_name(key));
    if exact.exists() {
        return Ok(Some(key.to_string()));
    }
    for restore_key in restore_keys
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let mut matches = cache_entries_with_prefix(&store, restore_key)?;
        matches.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.key.cmp(&right.key))
        });
        if let Some(matched) = matches.into_iter().next() {
            return Ok(Some(matched.key));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheEntryMatch {
    key: String,
    created: u128,
}

fn cache_entries_with_prefix(store: &Path, prefix: &str) -> Result<Vec<CacheEntryMatch>> {
    let sanitized_prefix = sanitize_artifact_name(prefix);
    let mut matches = Vec::new();
    for entry in fs::read_dir(store).with_context(|| format!("read {}", store.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let sanitized_key = entry.file_name().to_string_lossy().to_string();
        if sanitized_key.starts_with(&sanitized_prefix) {
            let path = entry.path();
            matches.push(CacheEntryMatch {
                key: cache_key_from_metadata(&path).unwrap_or(sanitized_key),
                created: cache_entry_created(&path),
            });
        }
    }
    Ok(matches)
}

fn cache_entry_created(path: &Path) -> u128 {
    fs::read_to_string(path.join(".velnor-created"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .or_else(|| {
            path.metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_nanos)
        })
        .unwrap_or_default()
}

fn cache_timestamp() -> String {
    system_time_nanos(SystemTime::now())
        .unwrap_or_default()
        .to_string()
}

fn system_time_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn restore_cache_paths(state: &JobExecutionState, key: &str, paths: &str) -> Result<()> {
    let cache_dir = cache_store_dir(state)?.join(sanitize_artifact_name(key));
    for (index, path) in cache_paths(paths).into_iter().enumerate() {
        let source = cache_dir.join(index.to_string());
        if !source.exists() {
            continue;
        }
        let Some(destination) = resolve_cache_path(state, &path) else {
            continue;
        };
        fs::create_dir_all(&destination)
            .with_context(|| format!("create cache restore path {}", destination.display()))?;
        copy_dir_contents(&source, &destination)?;
    }
    Ok(())
}

fn save_cache_result(
    state: &JobExecutionState,
    key: &str,
    paths: &str,
    exact_hit: bool,
) -> Result<StepExecutionResult> {
    if key.is_empty() {
        return Ok(cache_save_step_result(
            0,
            "Cache save skipped because key is empty\n",
            "",
        ));
    }
    if exact_hit {
        return Ok(cache_save_step_result(
            0,
            &format!("Cache hit occurred on primary key '{key}', not saving cache\n"),
            "",
        ));
    }
    let t0 = Instant::now();
    let cache_dir = cache_store_dir(state)?.join(sanitize_artifact_name(key));
    fs::remove_dir_all(&cache_dir).ok();
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create cache directory {}", cache_dir.display()))?;
    fs::write(cache_dir.join(".velnor-key"), key)
        .with_context(|| format!("write cache metadata {}", cache_dir.display()))?;
    fs::write(cache_dir.join(".velnor-created"), cache_timestamp())
        .with_context(|| format!("write cache timestamp {}", cache_dir.display()))?;

    let mut saved = 0usize;
    for (index, path) in cache_paths(paths).into_iter().enumerate() {
        let Some(source) = resolve_cache_path(state, &path) else {
            continue;
        };
        if !source.exists() {
            continue;
        }
        let target = cache_dir.join(index.to_string());
        fs::create_dir_all(&target)
            .with_context(|| format!("create cache entry {}", target.display()))?;
        copy_cache_source(&source, &target)?;
        saved += 1;
    }
    let save_ms = t0.elapsed().as_millis();
    if saved == 0 {
        fs::remove_dir_all(&cache_dir).ok();
        return Ok(cache_save_step_result(
            0,
            "",
            &format!("Cache not saved because no paths exist for key '{key}'\n"),
        ));
    }
    Ok(cache_save_step_result(
        0,
        &format!(
            "{ANSI_GREEN}Saved cache '{key}' with {saved} path(s){ANSI_RESET} ({save_ms}ms)\n"
        ),
        "",
    ))
}

fn cache_save_step_result(exit_code: i32, stdout: &str, stderr: &str) -> StepExecutionResult {
    StepExecutionResult {
        exit_code,
        state: StepCommandState::default(),
        skipped: false,
        failure_ignored: false,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn native_upload_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let name = native_input_or(&action_state, action, "name", "artifact");
    let path_input = native_input(action, &action_state, "path");
    let if_no_files_found =
        native_input_or(&action_state, action, "if-no-files-found", "warn").to_ascii_lowercase();
    let include_hidden_files =
        input_truthy(&native_input(action, &action_state, "include-hidden-files"));
    let overwrite = input_truthy(&native_input(action, &action_state, "overwrite"));
    let artifact_dir = artifact_store_dir(state)?.join(sanitize_artifact_name(&name));
    if artifact_dir.exists() {
        // Always overwrite in Velnor: re-runs on the same slot reuse the artifact store,
        // and the latest run's content is what compare-results should see.
        let _ = overwrite; // silence unused warning; we always overwrite
        fs::remove_dir_all(&artifact_dir).ok();
    }
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("create artifact directory {}", artifact_dir.display()))?;

    let mut uploaded = Vec::new();
    for path in artifact_paths(&path_input) {
        for source in resolve_artifact_sources(state, &path)? {
            if !include_hidden_files && artifact_source_is_hidden(state, &source) {
                continue;
            }
            copy_artifact_source(&source, &artifact_dir, include_hidden_files)?;
            uploaded.push(source);
        }
    }

    if uploaded.is_empty() {
        fs::remove_dir_all(&artifact_dir).ok();
        let message = format!("No files were found with the provided path: {path_input}\n");
        return Ok(StepExecutionResult {
            exit_code: if if_no_files_found == "error" { 1 } else { 0 },
            state: StepCommandState::default(),
            skipped: false,
            failure_ignored: false,
            stdout: if if_no_files_found == "ignore" {
                String::new()
            } else {
                message.clone()
            },
            stderr: if if_no_files_found == "error" {
                message
            } else {
                String::new()
            },
        });
    }

    let artifact_id = artifact_id_for_name(state, &name);
    let digest = hash_artifact_dir(&artifact_dir)?;
    let results_url = action_state
        .env
        .get("ACTIONS_RESULTS_URL")
        .cloned()
        .unwrap_or_else(|| "https://results.actions".to_string());
    let artifact_url = format!(
        "{}/artifacts/{artifact_id}",
        results_url.trim_end_matches('/')
    );
    let mut outputs = BTreeMap::new();
    outputs.insert("artifact-id".to_string(), artifact_id);
    outputs.insert("artifact-url".to_string(), artifact_url);
    outputs.insert("artifact-digest".to_string(), digest);

    Ok(StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!(
            "Uploaded artifact '{name}' with {} path(s)\n",
            uploaded.len()
        ),
        stderr: String::new(),
    })
}

fn native_download_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let name = native_input(action, &action_state, "name");
    let pattern = native_input(action, &action_state, "pattern");
    let destination_input = native_input_or(&action_state, action, "path", ".");
    let merge_multiple = input_truthy(&native_input(action, &action_state, "merge-multiple"));
    let destination = resolve_host_path(state, &destination_input)
        .ok_or_else(|| anyhow::anyhow!("download-artifact requires a workspace or temp path"))?;
    fs::create_dir_all(&destination)
        .with_context(|| format!("create artifact download dir {}", destination.display()))?;

    let store = artifact_store_dir(state)?;
    let artifacts = matching_artifacts(&store, &name, &pattern)?;
    for (artifact_name, artifact_dir) in &artifacts {
        let target = if merge_multiple || !name.is_empty() {
            destination.clone()
        } else {
            destination.join(artifact_name)
        };
        fs::create_dir_all(&target)
            .with_context(|| format!("create artifact target {}", target.display()))?;
        copy_artifact_download_contents(artifact_dir, &target)?;
    }

    let mut outputs = BTreeMap::new();
    outputs.insert(
        "download-path".to_string(),
        resolve_container_path(state, &destination_input),
    );

    Ok(StepExecutionResult {
        exit_code: if artifacts.is_empty() { 1 } else { 0 },
        state: StepCommandState {
            outputs,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!("Downloaded {} artifact(s)\n", artifacts.len()),
        stderr: if artifacts.is_empty() {
            "No artifacts matched the requested name or pattern\n".to_string()
        } else {
            String::new()
        },
    })
}

fn native_upload_pages_artifact(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Result<StepExecutionResult> {
    let mut page_action = action.clone();
    page_action
        .inputs
        .entry("name".to_string())
        .or_insert_with(|| "github-pages".to_string());
    let result = native_upload_artifact(&page_action, state)?;
    let mut outputs = result.state.outputs.clone();
    if let Some(artifact_id) = outputs.get("artifact-id").cloned() {
        outputs.insert("artifact_id".to_string(), artifact_id);
    }
    Ok(StepExecutionResult {
        state: StepCommandState {
            outputs,
            ..result.state
        },
        ..result
    })
}

fn native_deploy_pages(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> StepExecutionResult {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let repository = action_state
        .env
        .get("GITHUB_REPOSITORY")
        .cloned()
        .unwrap_or_default();
    let page_url = pages_url_for_repository(&repository);
    let artifact_name = native_input_or(&action_state, action, "artifact_name", "github-pages");
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs: [("page_url".to_string(), page_url.clone())].into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!("Deployed Pages artifact '{artifact_name}' to {page_url}\n"),
        stderr: String::new(),
    }
}

fn native_docker_metadata(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> StepExecutionResult {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let images = input_values(&native_input(action, &action_state, "images"));
    let tags_input = native_input(action, &action_state, "tags");
    let mut tags = Vec::new();
    for image in &images {
        for tag in docker_metadata_tags(&action_state, &tags_input) {
            tags.push(format!("{image}:{tag}"));
        }
    }
    let labels = docker_metadata_labels(&action_state).join("\n");
    let tags = tags.join("\n");
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            outputs: [
                ("tags".to_string(), tags.clone()),
                ("labels".to_string(), labels.clone()),
                ("json".to_string(), docker_metadata_json(&tags, &labels)),
            ]
            .into(),
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout: format!(
            "{ANSI_CYAN}{ANSI_BOLD}Generated Docker metadata{ANSI_RESET}\n{tags}\n{labels}\n"
        ),
        stderr: String::new(),
    }
}

fn native_github_runtime_export(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> StepExecutionResult {
    let action_state = state.with_env(state.resolve_env(&action.env));
    let mut exported = BTreeMap::new();
    let mut stdout = String::new();
    for (name, value) in action_state.step_env(&[]) {
        if name.starts_with("ACTIONS_") {
            stdout.push_str(&format!("{name}={value}\n"));
            exported.insert(name, value);
        }
    }
    StepExecutionResult {
        exit_code: 0,
        state: StepCommandState {
            env: exported,
            ..StepCommandState::default()
        },
        skipped: false,
        failure_ignored: false,
        stdout,
        stderr: String::new(),
    }
}

fn native_input(action: &NativeActionInvocation, state: &JobExecutionState, name: &str) -> String {
    action
        .inputs
        .get(name)
        .or_else(|| action.inputs.get(&name.to_ascii_lowercase()))
        .map(|value| state.resolve_expressions(value))
        .unwrap_or_default()
}

fn native_input_or(
    state: &JobExecutionState,
    action: &NativeActionInvocation,
    name: &str,
    default: &str,
) -> String {
    let value = native_input(action, state, name);
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

fn input_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

fn artifact_store_dir(state: &JobExecutionState) -> Result<PathBuf> {
    let temp = state
        .temp_host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("artifact actions require a temp directory"))?;
    let run_key = artifact_run_key(state);
    let run_root = shared_work_root(temp);
    Ok(run_root.join("_velnor_artifacts").join(run_key))
}

fn cache_store_dir(state: &JobExecutionState) -> Result<PathBuf> {
    let temp = state
        .temp_host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache actions require a temp directory"))?;
    Ok(shared_work_root(temp).join("_velnor_caches"))
}

fn shared_work_root(temp: &Path) -> PathBuf {
    if temp.file_name().is_some_and(|name| name == "temp") {
        temp.parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| temp.to_path_buf())
    } else {
        temp.to_path_buf()
    }
}

fn artifact_run_key(state: &JobExecutionState) -> String {
    let run_id = state
        .env
        .get("GITHUB_RUN_ID")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("local");
    let attempt = state
        .env
        .get("GITHUB_RUN_ATTEMPT")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("1");
    sanitize_artifact_name(&format!("{run_id}-{attempt}"))
}

fn artifact_id_for_name(state: &JobExecutionState, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(artifact_run_key(state).as_bytes());
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut id_bytes = [0u8; 8];
    id_bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(id_bytes).to_string()
}

fn artifact_paths(paths: &str) -> Vec<String> {
    paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn resolve_artifact_sources(state: &JobExecutionState, path: &str) -> Result<Vec<PathBuf>> {
    if !has_glob_pattern(path) {
        let Some(source) = resolve_host_path(state, path) else {
            return Ok(Vec::new());
        };
        return Ok(if source.exists() {
            vec![source]
        } else {
            Vec::new()
        });
    }

    let Some((base, pattern)) = artifact_glob_base_and_pattern(state, path) else {
        return Ok(Vec::new());
    };
    if !base.exists() {
        return Ok(Vec::new());
    }
    let matcher = Glob::new(&normalize_glob_pattern(&pattern))?.compile_matcher();
    let mut matches = Vec::new();
    collect_artifact_glob_matches(&base, &base, &matcher, &mut matches)?;
    matches.sort();
    Ok(matches)
}

fn has_glob_pattern(path: &str) -> bool {
    path.chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn artifact_glob_base_and_pattern(
    state: &JobExecutionState,
    path: &str,
) -> Option<(PathBuf, String)> {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix("/__w/") {
        return state
            .workspace_host
            .as_ref()
            .map(|base| (base.clone(), rest.to_string()));
    }
    if let Some(rest) = path.strip_prefix("/github/workspace/") {
        return state
            .workspace_host
            .as_ref()
            .map(|base| (base.clone(), rest.to_string()));
    }
    if let Some(rest) = path.strip_prefix("/__t/") {
        return state
            .temp_host
            .as_ref()
            .map(|base| (base.clone(), rest.to_string()));
    }
    if let Some(rest) = path.strip_prefix("/github/runner_temp/") {
        return state
            .temp_host
            .as_ref()
            .map(|base| (base.clone(), rest.to_string()));
    }
    if let Some(rest) = path.strip_prefix("/tmp/") {
        return state
            .temp_host
            .as_ref()
            .map(|base| (base.clone(), rest.to_string()));
    }
    if Path::new(path).is_absolute() {
        return absolute_glob_base_and_pattern(path);
    }
    state
        .workspace_host
        .as_ref()
        .map(|base| (base.clone(), path.to_string()))
}

fn absolute_glob_base_and_pattern(path: &str) -> Option<(PathBuf, String)> {
    let slash_index = path
        .char_indices()
        .take_while(|(_, ch)| !matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
        .filter_map(|(index, ch)| (ch == '/').then_some(index))
        .last()
        .unwrap_or(0);
    let (base, pattern) = path.split_at(slash_index + 1);
    let base = PathBuf::from(base);
    let pattern = pattern.trim_start_matches('/').to_string();
    Some((base, pattern))
}

fn normalize_glob_pattern(pattern: &str) -> String {
    pattern.replace('\\', "/")
}

fn collect_artifact_glob_matches(
    root: &Path,
    current: &Path,
    matcher: &globset::GlobMatcher,
    matches: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(current).with_context(|| format!("read {}", current.display()))? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if matcher.is_match(relative) {
            matches.push(path.clone());
        }
        if entry.file_type()?.is_dir() {
            collect_artifact_glob_matches(root, &path, matcher, matches)?;
        }
    }
    Ok(())
}

fn cache_paths(paths: &str) -> Vec<String> {
    artifact_paths(paths)
}

fn resolve_cache_path(state: &JobExecutionState, path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path == "~" {
        return state_home_host(state);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return state_home_host(state).map(|home| home.join(rest));
    }
    resolve_host_path(state, path)
}

fn state_home_host(state: &JobExecutionState) -> Option<PathBuf> {
    let temp = state.temp_host.as_ref()?;
    if temp.file_name().is_some_and(|name| name == "temp") {
        return temp.parent().map(|job_dir| job_dir.join("home"));
    }
    Some(temp.join("home"))
}

fn resolve_host_path(state: &JobExecutionState, path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    if let Some(rest) = path.strip_prefix("/__w/") {
        return state.workspace_host.as_ref().map(|base| base.join(rest));
    }
    if path == "/__w" || path == "/github/workspace" {
        return state.workspace_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/github/workspace/") {
        return state.workspace_host.as_ref().map(|base| base.join(rest));
    }
    if let Some(rest) = path.strip_prefix("/__t/") {
        return state.temp_host.as_ref().map(|base| base.join(rest));
    }
    if path == "/__t" || path == "/github/runner_temp" {
        return state.temp_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/github/runner_temp/") {
        return state.temp_host.as_ref().map(|base| base.join(rest));
    }
    if path == "/tmp" {
        return state.temp_host.clone();
    }
    if let Some(rest) = path.strip_prefix("/tmp/") {
        return state.temp_host.as_ref().map(|base| base.join(rest));
    }
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        state
            .workspace_host
            .as_ref()
            .map(|base| base.join(candidate))
    }
}

fn resolve_container_path(state: &JobExecutionState, path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "." {
        return state
            .env
            .get("GITHUB_WORKSPACE")
            .cloned()
            .unwrap_or_else(|| "/__w".to_string());
    }
    if path.starts_with("/__w")
        || path.starts_with("/github/workspace")
        || path.starts_with("/__t")
        || path.starts_with("/github/runner_temp")
        || path.starts_with("/tmp")
    {
        return path.to_string();
    }
    if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        let workspace = state
            .env
            .get("GITHUB_WORKSPACE")
            .map(String::as_str)
            .unwrap_or("/__w");
        format!(
            "{}/{}",
            workspace.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    }
}

fn artifact_source_is_hidden(state: &JobExecutionState, source: &Path) -> bool {
    let relative = state
        .workspace_host
        .as_deref()
        .and_then(|base| source.strip_prefix(base).ok())
        .or_else(|| {
            state
                .temp_host
                .as_deref()
                .and_then(|base| source.strip_prefix(base).ok())
        })
        .unwrap_or(source);
    path_has_hidden_component(relative)
}

fn path_has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(value) => value
            .to_str()
            .is_some_and(|name| name.len() > 1 && name.starts_with('.')),
        _ => false,
    })
}

fn copy_artifact_source(source: &Path, artifact_dir: &Path, include_hidden: bool) -> Result<()> {
    if source.is_dir() {
        copy_dir_contents_filtered(source, artifact_dir, include_hidden)
    } else {
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("artifact source has no file name"))?;
        fs::copy(source, artifact_dir.join(file_name))
            .with_context(|| format!("copy artifact file {}", source.display()))?;
        Ok(())
    }
}

fn copy_cache_source(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        copy_dir_contents(source, destination)
    } else {
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("cache source has no file name"))?;
        fs::copy(source, destination.join(file_name))
            .with_context(|| format!("copy cache file {}", source.display()))?;
        Ok(())
    }
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            fs::create_dir_all(&destination_path)
                .with_context(|| format!("create {}", destination_path.display()))?;
            copy_dir_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::copy(&source_path, &destination_path)
                .with_context(|| format!("copy {}", source_path.display()))?;
        }
    }
    Ok(())
}

fn copy_artifact_download_contents(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("create artifact download dir {}", destination.display()))?;
    normalize_artifact_dir_permissions(destination)?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_artifact_download_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
                normalize_artifact_dir_permissions(parent)?;
            }
            fs::copy(&source_path, &destination_path)
                .with_context(|| format!("copy {}", source_path.display()))?;
            normalize_artifact_file_permissions(&destination_path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn normalize_artifact_file_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("set artifact file permissions {}", path.display()))
}

#[cfg(not(unix))]
fn normalize_artifact_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn normalize_artifact_dir_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("set artifact directory permissions {}", path.display()))
}

#[cfg(not(unix))]
fn normalize_artifact_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn copy_dir_contents_filtered(
    source: &Path,
    destination: &Path,
    include_hidden: bool,
) -> Result<()> {
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !include_hidden && path.file_name().is_some_and(hidden_file_name) {
            continue;
        }
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("create directory {}", target.display()))?;
            copy_dir_contents_filtered(&path, &target, include_hidden)?;
        } else {
            fs::copy(&path, &target)
                .with_context(|| format!("copy {} to {}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

fn hidden_file_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.len() > 1 && name.starts_with('.'))
}

fn matching_artifacts(store: &Path, name: &str, pattern: &str) -> Result<Vec<(String, PathBuf)>> {
    let mut artifacts = Vec::new();
    if !store.exists() {
        return Ok(artifacts);
    }
    let matcher = if !pattern.is_empty() {
        let mut builder = GlobSetBuilder::new();
        builder.add(Glob::new(pattern)?);
        Some(builder.build().context("build artifact pattern")?)
    } else {
        None
    };
    for entry in fs::read_dir(store).with_context(|| format!("read {}", store.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let artifact_name = entry.file_name().to_string_lossy().to_string();
        let matched = if !name.is_empty() {
            artifact_name == sanitize_artifact_name(name)
        } else if let Some(matcher) = &matcher {
            matcher.is_match(&artifact_name)
        } else {
            true
        };
        if matched {
            artifacts.push((artifact_name, entry.path()));
        }
    }
    artifacts.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(artifacts)
}

fn cache_key_from_metadata(path: &Path) -> Option<String> {
    fs::read_to_string(path.join(".velnor-key"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn hash_artifact_dir(path: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(path, &mut files)?;
    files.sort();
    let mut aggregate = Sha256::new();
    for file in files {
        aggregate.update(Sha256::digest(fs::read(file)?));
    }
    Ok(hex_digest(aggregate.finalize().as_slice()))
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn sanitize_artifact_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn pages_url_for_repository(repository: &str) -> String {
    let Some((owner, repo)) = repository.split_once('/') else {
        return String::new();
    };
    format!("https://{owner}.github.io/{repo}/")
}

fn native_command_result(result: CommandResult, state: StepCommandState) -> StepExecutionResult {
    StepExecutionResult {
        exit_code: result.code,
        state,
        skipped: false,
        failure_ignored: false,
        stdout: result.stdout,
        stderr: result.stderr,
    }
}

fn push_arg(args: &mut Vec<String>, name: &str, value: &str) {
    if !value.trim().is_empty() {
        args.push(name.to_string());
        args.push(value.to_string());
    }
}

fn input_values(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn host_context_path(state: &JobExecutionState, value: &str) -> String {
    resolve_host_path(state, value)
        .unwrap_or_else(|| PathBuf::from(value))
        .display()
        .to_string()
}

fn docker_metadata_tags(state: &JobExecutionState, input: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let fields = docker_metadata_fields(line);
        if fields
            .get("enable")
            .is_some_and(|value| !input_truthy(value))
        {
            continue;
        }
        match fields.get("type").map(String::as_str) {
            Some("raw") => {
                if let Some(value) = fields.get("value").filter(|value| !value.is_empty()) {
                    tags.push(value.clone());
                }
            }
            Some("sha") => {
                let sha = state
                    .env
                    .get("GITHUB_SHA")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let prefix = fields
                    .get("prefix")
                    .cloned()
                    .unwrap_or_else(|| "sha-".to_string());
                let value = if fields.get("format").is_some_and(|value| value == "long") {
                    sha
                } else {
                    sha.chars().take(7).collect()
                };
                tags.push(format!("{prefix}{value}"));
            }
            Some("ref") => {
                let event = fields.get("event").map(String::as_str).unwrap_or("");
                match event {
                    "branch" => {
                        let git_ref = state.env.get("GITHUB_REF").cloned().unwrap_or_default();
                        if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
                            let tag = docker_sanitize_tag(branch);
                            if !tag.is_empty() {
                                tags.push(tag);
                            }
                        }
                    }
                    "tag" => {
                        let git_ref = state.env.get("GITHUB_REF").cloned().unwrap_or_default();
                        if let Some(tag_name) = git_ref.strip_prefix("refs/tags/") {
                            let tag = docker_sanitize_tag(tag_name);
                            if !tag.is_empty() {
                                tags.push(tag);
                            }
                        }
                    }
                    "pr" => {
                        if let Some(pr_number) = state
                            .resolve_context_data_expression("github.event.pull_request.number")
                            .filter(|v| !v.is_empty())
                        {
                            tags.push(format!("pr-{pr_number}"));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if tags.is_empty() {
        let sha = state
            .env
            .get("GITHUB_SHA")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        tags.push(format!("sha-{}", sha.chars().take(7).collect::<String>()));
    }
    tags
}

fn docker_metadata_fields(line: &str) -> BTreeMap<String, String> {
    line.split(',')
        .filter_map(|field| field.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn docker_sanitize_tag(name: &str) -> String {
    // Docker tag chars: [a-zA-Z0-9_.-]; replace others with '-'; max 128 chars.
    // Leading '.' or '-' are invalid — strip them.
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_start_matches(['.', '-']);
    trimmed.chars().take(128).collect()
}

fn docker_metadata_labels(state: &JobExecutionState) -> Vec<String> {
    let repository = state
        .env
        .get("GITHUB_REPOSITORY")
        .cloned()
        .unwrap_or_default();
    let server = state
        .env
        .get("GITHUB_SERVER_URL")
        .cloned()
        .unwrap_or_else(|| "https://github.com".to_string());
    let source = if repository.is_empty() {
        server
    } else {
        format!("{}/{}", server.trim_end_matches('/'), repository)
    };
    vec![format!("org.opencontainers.image.source={source}")]
}

fn docker_metadata_json(tags: &str, labels: &str) -> String {
    serde_json::json!({
        "tags": tags.lines().collect::<Vec<_>>(),
        "labels": labels.lines().collect::<Vec<_>>(),
    })
    .to_string()
}

fn parse_paths_filter_rules(filters: &str) -> Result<Vec<(String, Vec<String>)>> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(filters).context("parse filters")?;
    let Some(mapping) = value.as_mapping() else {
        bail!("paths-filter filters input must be a YAML mapping")
    };
    let mut rules = Vec::new();
    for (name, patterns) in mapping {
        let name = name
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("paths-filter name must be a string"))?
            .to_string();
        let patterns = paths_filter_patterns(patterns)
            .with_context(|| format!("parse paths-filter patterns for '{name}'"))?;
        rules.push((name, patterns));
    }
    Ok(rules)
}

fn paths_filter_patterns(value: &serde_yaml::Value) -> Result<Vec<String>> {
    match value {
        serde_yaml::Value::Sequence(sequence) => sequence
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("paths-filter pattern must be a string"))
            })
            .collect(),
        serde_yaml::Value::String(value) => Ok(vec![value.clone()]),
        _ => bail!("paths-filter patterns must be a string or string list"),
    }
}

fn paths_filter_base_ref(state: &JobExecutionState) -> Option<String> {
    state
        .resolve_context_data_expression("github.event.pull_request.base.sha")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            state
                .resolve_context_data_expression("github.event.before")
                .filter(|value| !value.is_empty())
        })
        .or_else(|| state.env.get("GITHUB_BASE_REF").cloned())
}

fn paths_filter_head_ref(state: &JobExecutionState) -> Option<String> {
    state
        .resolve_context_data_expression("github.event.pull_request.head.sha")
        .filter(|value| !value.is_empty())
        .or_else(|| state.env.get("GITHUB_SHA").cloned())
}

#[derive(Debug, Default)]
struct JobExecutionState {
    env: BTreeMap<String, String>,
    context_data: BTreeMap<String, Value>,
    workspace_host: Option<PathBuf>,
    temp_host: Option<PathBuf>,
    outputs: BTreeMap<String, BTreeMap<String, String>>,
    action_states: BTreeMap<String, BTreeMap<String, String>>,
    outcomes: BTreeMap<String, StepOutcome>,
    conclusions: BTreeMap<String, StepOutcome>,
    path: Vec<String>,
    masks: Vec<String>,
    composite_stack: Vec<String>,
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
        Self::new_internal(base_env, context_data, None, None)
    }

    fn new_with_workspace(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: &Path,
        temp_host: &Path,
    ) -> Self {
        Self::new_internal(
            base_env,
            context_data,
            Some(workspace_host.to_path_buf()),
            Some(temp_host.to_path_buf()),
        )
    }

    fn new_internal(
        base_env: &[(String, String)],
        context_data: &[(String, Value)],
        workspace_host: Option<PathBuf>,
        temp_host: Option<PathBuf>,
    ) -> Self {
        let mut state = Self {
            env: base_env.iter().cloned().collect(),
            context_data: context_data.iter().cloned().collect(),
            workspace_host,
            temp_host,
            outputs: BTreeMap::new(),
            action_states: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            conclusions: BTreeMap::new(),
            path: Vec::new(),
            masks: Vec::new(),
            composite_stack: Vec::new(),
        };
        state.env = state
            .env
            .iter()
            .map(|(name, value)| (name.clone(), state.resolve_expressions(value)))
            .collect();
        state
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

    fn with_step_action(&self, step_id: &str) -> Self {
        let mut state = Self {
            env: self.env.clone(),
            context_data: self.context_data.clone(),
            workspace_host: self.workspace_host.clone(),
            temp_host: self.temp_host.clone(),
            outputs: self.outputs.clone(),
            action_states: self.action_states.clone(),
            outcomes: self.outcomes.clone(),
            conclusions: self.conclusions.clone(),
            path: self.path.clone(),
            masks: self.masks.clone(),
            composite_stack: self.composite_stack.clone(),
        };
        state
            .env
            .insert("GITHUB_ACTION".to_string(), step_id.to_string());
        state
    }

    fn with_env(&self, env: Vec<(String, String)>) -> Self {
        let mut state = Self {
            env: self.env.clone(),
            context_data: self.context_data.clone(),
            workspace_host: self.workspace_host.clone(),
            temp_host: self.temp_host.clone(),
            outputs: self.outputs.clone(),
            action_states: self.action_states.clone(),
            outcomes: self.outcomes.clone(),
            conclusions: self.conclusions.clone(),
            path: self.path.clone(),
            masks: self.masks.clone(),
            composite_stack: self.composite_stack.clone(),
        };
        for (name, value) in env {
            state.env.insert(name, value);
        }
        state
    }

    fn push_composite(&mut self, step_id: &str) {
        self.composite_stack.push(step_id.to_string());
    }

    fn pop_composite(&mut self, step_id: &str) {
        if self
            .composite_stack
            .last()
            .is_some_and(|scope| scope == step_id)
        {
            self.composite_stack.pop();
        }
    }

    fn apply(&mut self, step_id: &str, result: &StepExecutionResult) {
        let outcome = if result.skipped {
            StepOutcome::Skipped
        } else if result.exit_code == 0 {
            StepOutcome::Success
        } else {
            StepOutcome::Failure
        };
        let conclusion = if result.failure_ignored && outcome == StepOutcome::Failure {
            StepOutcome::Success
        } else {
            outcome
        };
        self.outcomes.insert(step_id.to_string(), outcome);
        self.conclusions.insert(step_id.to_string(), conclusion);

        if !result.state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), result.state.outputs.clone());
        }
        if !result.state.state.is_empty() {
            self.action_states
                .entry(step_id.to_string())
                .or_default()
                .extend(result.state.state.clone());
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
            display_name: step.display_name.clone(),
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
                (!github_string_eq(
                    &self.resolve_expression_value(left).unwrap_or_default(),
                    &self.resolve_expression_value(right).unwrap_or_default(),
                ))
                .to_string(),
            );
        }
        if let Some((left, right)) = split_top_level(expression, "==") {
            return Some(
                github_string_eq(
                    &self.resolve_expression_value(left).unwrap_or_default(),
                    &self.resolve_expression_value(right).unwrap_or_default(),
                )
                .to_string(),
            );
        }
        if let Some((value, needle)) = parse_contains(expression) {
            return Some(
                self.resolve_expression_value(value)
                    .is_some_and(|value| github_contains(&value, unquote(needle.trim())))
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
        if expression.trim().starts_with("format(") {
            let context_data: Vec<_> = self
                .context_data
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            return crate::script_step::evaluate_github_format(expression.trim(), &context_data);
        }
        self.resolve_context_expression(expression)
    }

    fn resolve_context_expression(&self, expression: &str) -> Option<String> {
        if let Some(name) = expression.trim().strip_prefix("env.") {
            return self.env.get(name).cloned();
        }
        if expression.trim() == "job.status" {
            return Some(self.job_status().to_string());
        }
        if expression.trim() == "github.action_status" {
            return Some(self.action_status().to_string());
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
            "runner.debug" => "RUNNER_DEBUG",
            "runner.environment" => "RUNNER_ENVIRONMENT",
            "runner.name" => "RUNNER_NAME",
            "runner.os" => "RUNNER_OS",
            "runner.temp" => "RUNNER_TEMP",
            "runner.tool_cache" => "RUNNER_TOOL_CACHE",
            "runner.workspace" => "RUNNER_WORKSPACE",
            _ => return self.resolve_context_data_expression(expression),
        };
        self.env
            .get(env_name)
            .cloned()
            .or_else(|| self.resolve_context_data_expression(expression))
    }

    fn job_status(&self) -> &'static str {
        if self
            .conclusions
            .values()
            .any(|outcome| *outcome == StepOutcome::Failure)
        {
            "failure"
        } else {
            "success"
        }
    }

    fn action_status(&self) -> &'static str {
        let Some(scope) = self.composite_stack.last() else {
            return self.job_status();
        };
        if self.conclusions.iter().any(|(step_id, outcome)| {
            *outcome == StepOutcome::Failure
                && step_id
                    .strip_prefix(scope)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        }) {
            "failure"
        } else {
            "success"
        }
    }

    fn evaluate_condition(&self, condition: Option<&str>) -> bool {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return self.evaluate_condition_expr("success()");
        };
        let expression = strip_expression(condition);
        if condition_has_status_check(expression) {
            return self.evaluate_condition_expr(expression);
        }
        self.evaluate_condition_expr("success()") && self.evaluate_condition_expr(expression)
    }

    fn evaluate_post_condition(&self, condition: Option<&str>) -> bool {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return true;
        };
        let expression = strip_expression(condition);
        if condition_has_status_check(expression) {
            return self.evaluate_condition_expr(expression);
        }
        self.evaluate_condition_expr("success()") && self.evaluate_condition_expr(expression)
    }

    fn evaluate_condition_expr(&self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression == "always()" {
            return true;
        }
        if expression == "success()" {
            return !self
                .conclusions
                .values()
                .any(|outcome| *outcome == StepOutcome::Failure);
        }
        if expression == "failure()" {
            return self
                .conclusions
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
                .is_some_and(|value| github_contains(&value, unquote(needle.trim())));
        }
        if let Some((left, right)) = split_top_level(expression, "!=") {
            let left = self
                .resolve_condition_comparison_value(left)
                .unwrap_or_default();
            let right = self
                .resolve_condition_comparison_value(right)
                .unwrap_or_default();
            return !github_string_eq(&left, &right);
        }
        if let Some((left, right)) = split_top_level(expression, "==") {
            let left = self
                .resolve_condition_comparison_value(left)
                .unwrap_or_default();
            let right = self
                .resolve_condition_comparison_value(right)
                .unwrap_or_default();
            return github_string_eq(&left, &right);
        }
        if expression == "true" {
            return true;
        }
        if expression == "false" {
            return false;
        }
        if let Some(value) = self.resolve_condition_value(expression) {
            return expression_truthy(&value);
        }
        true
    }

    fn resolve_condition_comparison_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if condition_returns_bool(expression) {
            return Some(self.evaluate_condition_expr(expression).to_string());
        }
        self.resolve_condition_value(expression)
    }

    fn resolve_condition_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Some(output) = self.resolve_step_output_expression(expression) {
            return Some(output.to_string());
        }
        if let Some(expression) = expression.strip_prefix("steps.") {
            let (step_id, field) = expression.split_once('.')?;
            if field == "outcome" {
                return self
                    .outcomes
                    .get(step_id)
                    .map(|outcome| outcome.as_str().to_string());
            }
            if field == "conclusion" {
                return self
                    .conclusions
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
            .or_else(|| missing_context_value(expression))
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

fn missing_context_value(expression: &str) -> Option<String> {
    let expression = expression.trim();
    if expression.starts_with("github.event.") {
        return Some(String::new());
    }
    let root = expression.split('.').next()?;
    matches!(root, "matrix" | "needs" | "inputs" | "vars" | "secrets").then(String::new)
}

fn evaluate_job_outputs(
    job_outputs: Option<&Value>,
    state: &JobExecutionState,
) -> BTreeMap<String, String> {
    job_output_pairs(job_outputs)
        .into_iter()
        .filter_map(|(name, value)| {
            let value = state.resolve_expressions(&value);
            (!value.is_empty()).then_some((name, value))
        })
        .collect()
}

fn evaluate_environment_url(
    environment_url: Option<&Value>,
    state: &JobExecutionState,
) -> Option<String> {
    environment_url
        .and_then(template_string_value)
        .map(|value| state.resolve_expressions(value))
        .filter(|value| !value.is_empty())
}

fn template_string_value(value: &Value) -> Option<&str> {
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
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
        })
        .and_then(template_string_value)
}

fn job_output_pairs(job_outputs: Option<&Value>) -> Vec<(String, String)> {
    match job_outputs {
        Some(Value::Object(outputs)) => {
            if outputs
                .get("type")
                .or_else(|| outputs.get("Type"))
                .is_some()
                && outputs.get("map").or_else(|| outputs.get("Map")).is_some()
            {
                return job_output_pairs(outputs.get("map").or_else(|| outputs.get("Map")));
            }
            outputs
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
                .filter_map(|(name, value)| {
                    job_output_expression(value).map(|value| (name.clone(), value.to_string()))
                })
                .collect()
        }
        Some(Value::Array(outputs)) => outputs.iter().filter_map(job_output_pair_value).collect(),
        _ => Vec::new(),
    }
}

fn job_output_pair_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Object(object) => {
            let key = object.get("Key").or_else(|| object.get("key"))?;
            let value = object.get("Value").or_else(|| object.get("value"))?;
            Some((
                job_output_name(key)?.to_string(),
                job_output_expression(value)?.to_string(),
            ))
        }
        Value::Array(pair) if pair.len() == 2 => Some((
            job_output_name(&pair[0])?.to_string(),
            job_output_expression(&pair[1])?.to_string(),
        )),
        _ => None,
    }
}

fn job_output_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.as_object().and_then(|object| {
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .and_then(job_output_name)
        })
    })
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
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
        })
        .and_then(job_output_expression)
}

fn step_log(step_id: &str, order: i32, result: &StepExecutionResult) -> StepLog {
    step_log_with_name(step_id, "", order, result)
}

fn step_log_with_name(
    step_id: &str,
    display_name: &str,
    order: i32,
    result: &StepExecutionResult,
) -> StepLog {
    let lines = step_log_lines(
        &result.stdout,
        &result.stderr,
        &result.state.log_lines,
        &result.state.summary,
    );
    StepLog {
        step_id: step_id.to_string(),
        display_name: display_name.to_string(),
        order,
        lines,
        masks: result.state.masks.clone(),
        annotations: result.state.annotations.clone(),
        telemetry: result.state.telemetry.clone(),
        exit_code: result.exit_code,
        skipped: result.skipped,
        failure_ignored: result.failure_ignored,
        error_count: result.state.error_count,
        warning_count: result.state.warning_count,
        notice_count: result.state.notice_count,
        summary: result.state.summary.clone(),
    }
}

fn step_log_lines(
    stdout: &str,
    stderr: &str,
    command_lines: &[String],
    summary: &str,
) -> Vec<String> {
    let mut lines = stdout
        .lines()
        .chain(stderr.lines())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    lines.extend(command_lines.iter().cloned());
    if !summary.is_empty() {
        lines.push("Step summary:".to_string());
        lines.extend(summary.lines().map(ToOwned::to_owned));
    }
    lines
}

fn parse_workflow_commands_from_output(stdout: &str, stderr: &str) -> StepCommandState {
    let mut state = parse_workflow_commands(stdout);
    state.merge(parse_workflow_commands(stderr));
    state
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
    Ok(Some("/github/workflow/event.json".to_string()))
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
    split_top_level(inner, ",")
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
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in expression.char_indices() {
        if let Some(quote_char) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote_char {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
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
    !(value.is_empty()
        || value.eq_ignore_ascii_case("false")
        || value == "0"
        || value.eq_ignore_ascii_case("null"))
}

fn github_string_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn github_contains(value: &str, needle: &str) -> bool {
    value
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn condition_returns_bool(expression: &str) -> bool {
    let expression = expression.trim();
    if matches!(
        expression,
        "always()" | "success()" | "failure()" | "cancelled()" | "true" | "false"
    ) {
        return true;
    }
    if expression.starts_with('!') {
        return true;
    }
    let expression = strip_wrapping_parentheses(expression).unwrap_or(expression);
    split_top_level(expression, "||").is_some()
        || split_top_level(expression, "&&").is_some()
        || split_top_level(expression, "!=").is_some()
        || split_top_level(expression, "==").is_some()
        || parse_contains(expression).is_some()
}

fn condition_has_status_check(expression: &str) -> bool {
    ["always()", "success()", "failure()", "cancelled()"]
        .iter()
        .any(|function| expression.contains(function))
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
    use crate::container::{ServiceContainerSpec, Shell};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
        stdin: Vec<String>,
        env: Vec<Vec<(String, String)>>,
        codes: Vec<i32>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(String::new());
            self.env.push(Vec::new());
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

        fn run_with_env(
            &mut self,
            program: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(String::new());
            self.env.push(env.to_vec());
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

        fn run_with_stdin(
            &mut self,
            program: &str,
            args: &[String],
            stdin: &str,
        ) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            self.stdin.push(stdin.to_string());
            self.env.push(Vec::new());
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

    #[derive(Default)]
    struct GitDiffRunner {
        calls: Vec<(String, Vec<String>)>,
        stdout: String,
    }

    impl CommandRunner for GitDiffRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: if program == "git" {
                    self.stdout.clone()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct FailingPostRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for FailingPostRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let code = if args.iter().any(|arg| {
                arg.contains("/__a/_actions/sccache/dist/show_stats/index.js")
                    || arg.contains(
                        "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js",
                    )
            }) {
                1
            } else {
                0
            };
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct CheckoutOutputRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for CheckoutOutputRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stdout = if program == "docker"
                && args.first().is_some_and(|arg| arg == "exec")
                && args.iter().any(|arg| arg == "/__t/source.sh")
            {
                "::set-output name=sha::def456\n".to_string()
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

    struct OutputWritingRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for OutputWritingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let action_process = program == "docker"
                && (args.first().is_some_and(|arg| arg == "exec")
                    || args.iter().any(|arg| arg.starts_with("node:")));
            if action_process {
                if has_container_env_path(args, "GITHUB_OUTPUT", "producer_output") {
                    fs::write(self.temp.join("producer_output"), "answer=42\n")?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "check-image_output") {
                    fs::write(self.temp.join("check-image_output"), "exists=false\n")?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "paths_output") {
                    fs::write(
                        self.temp.join("paths_output"),
                        "construct=true\nconstruct_count=1\nchanges=[\"construct\"]\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "upload-artifact_output") {
                    fs::write(
                        self.temp.join("upload-artifact_output"),
                        "artifact-id=777\nartifact-url=https://results.actions/artifacts/777\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "deploy-pages_output") {
                    fs::write(
                        self.temp.join("deploy-pages_output"),
                        "page_url=https://jackin-project.github.io/jackin/\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "deployment_output") {
                    fs::write(
                        self.temp.join("deployment_output"),
                        "page_url=https://jackin-project.github.io/jackin/\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_OUTPUT", "sitemap_output") {
                    fs::write(
                        self.temp.join("sitemap_output"),
                        "url=https://jackin-project.github.io/jackin/sitemap.xml\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_STATE", "cache_state") {
                    fs::write(self.temp.join("cache_state"), "primaryKey=linux-cache\n")?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "pather_path") {
                    fs::write(self.temp.join("pather_path"), "/root/.cargo/bin\n")?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "mise_path") {
                    fs::write(self.temp.join("mise_path"), "/opt/mise/shims\n")?;
                }
                if has_container_env_path(args, "GITHUB_ENV", "toolchain_env") {
                    fs::write(
                        self.temp.join("toolchain_env"),
                        "CARGO_HOME=/github/home/.cargo\n",
                    )?;
                }
                if has_container_env_path(args, "GITHUB_PATH", "toolchain_path") {
                    fs::write(self.temp.join("toolchain_path"), "/root/.cargo/bin\n")?;
                }
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct PhaseStateRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for PhaseStateRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let state_file = has_container_env_path(args, "GITHUB_STATE", "wrapped_state")
                .then(|| self.temp.join("wrapped_state"));
            if program == "docker" {
                match (args.last().map(String::as_str), state_file) {
                    (Some("/__a/_actions/wrapped/dist/pre.js"), Some(path)) => {
                        fs::write(path, "preKey=pre-value\n")?;
                    }
                    (Some("/__a/_actions/wrapped/dist/main.js"), Some(path)) => {
                        fs::write(path, "mainKey=main-value\n")?;
                    }
                    _ => {}
                }
            }
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct EnvAndFailureRunner {
        calls: Vec<(String, Vec<String>)>,
        temp: PathBuf,
    }

    impl CommandRunner for EnvAndFailureRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            if has_container_env_path(args, "GITHUB_ENV", "enable_env") {
                fs::write(self.temp.join("enable_env"), "CACHE_ON_FAILURE=true\n")?;
            }
            let code = if args.iter().any(|arg| arg == "/__t/fail.sh") {
                1
            } else {
                0
            };
            Ok(CommandResult {
                code,
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
                "::set-output name=answer::42\n::add-path::/opt/tool\n::add-mask::hidden\n::error::broken\nhidden\n"
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

    #[derive(Default)]
    struct SilentCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for SilentCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    struct StderrCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for StderrCommandRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let stderr = if program == "docker" && args.first().is_some_and(|arg| arg == "exec") {
                "::set-output name=answer::42\n::add-path::/opt/tool\n::add-mask::hidden\n::warning::slow\nhidden\n"
                    .to_string()
            } else {
                String::new()
            };
            Ok(CommandResult {
                code: 0,
                stdout: String::new(),
                stderr,
            })
        }
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("velnor-executor-test-{nonce}-{sequence}"))
    }

    fn host_temp_script_path(container_path: &str, temp: &Path) -> PathBuf {
        temp.join(container_path.trim_start_matches("/__t/"))
    }

    fn has_container_env_path(args: &[String], name: &str, file_name: &str) -> bool {
        args.iter().any(|arg| {
            arg == &format!("{name}=/__t/{file_name}")
                || arg == &format!("{name}=/github/file_commands/{file_name}")
        })
    }

    fn container(temp: &Path) -> JobContainerSpec {
        JobContainerSpec {
            name: "job".into(),
            image: "ubuntu:24.04".into(),
            network: "net".into(),
            workspace_host: temp.join("work"),
            temp_host: temp.to_path_buf(),
            home_host: temp.join("home"),
            actions_host: temp.join("actions"),
            tools_host: temp.join("tools"),
            mount_docker_socket: false,
            env: Vec::new(),
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: String::new(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: false,
        }
    }

    #[test]
    fn verifies_docker_bind_mount_visibility_when_enabled() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.verify_bind_mounts = true;
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor.start_job_environment_once(&spec).unwrap();

        let runner = executor.runner();
        let calls = &runner.calls;
        assert_eq!(calls[2].1[0], "exec");
        assert!(calls[2]
            .1
            .contains(&format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}")));
        assert!(!temp.join(DOCKER_MOUNT_CHECK_FILE).exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn bind_mount_preflight_reports_invisible_daemon_paths() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut spec = container(&temp);
        spec.verify_bind_mounts = true;
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });

        let error = executor.start_job_environment_once(&spec).unwrap_err();

        assert!(error.to_string().contains("Docker daemon cannot see"));
        assert!(!temp.join(DOCKER_MOUNT_CHECK_FILE).exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn selects_node_action_image_from_runtime() {
        assert_eq!(
            node_action_image("node24", "velnor/job-ubuntu:24.04"),
            "velnor/job-ubuntu:24.04"
        );
        assert_eq!(node_action_image("node20", ""), "node:20-bookworm");
        assert_eq!(node_action_image("node24", ""), "node:24-bookworm");
        assert_eq!(node_action_image("node16", ""), "node:16");
        assert_eq!(node_action_image("bogus", ""), "");
    }

    #[test]
    fn executes_script_step_and_collects_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
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
        let runner = executor.runner();
        let calls = &runner.calls;
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
            display_name: String::new(),
            script: "exit 7".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
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
                display_name: String::new(),
                script: "echo one".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            },
            ScriptStep {
                id: "step2".into(),
                display_name: String::new(),
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
        let runner = executor.runner();
        let calls = &runner.calls;
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
    fn native_github_runtime_export_exports_actions_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "github-runtime".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::GitHubRuntimeExport,
                inputs: [("github-token".into(), "ghs_token".into())].into(),
                env: vec![("ACTIONS_CUSTOM".into(), "${{ env.CUSTOM_RUNTIME }}".into())],
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
                    (
                        "ACTIONS_RUNTIME_URL".into(),
                        "https://runtime.actions".into(),
                    ),
                    ("CUSTOM_RUNTIME".into(), "custom-value".into()),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            results[0].state.env["ACTIONS_RUNTIME_TOKEN"],
            "runtime-token"
        );
        assert_eq!(
            results[0].state.env["ACTIONS_RUNTIME_URL"],
            "https://runtime.actions"
        );
        assert_eq!(results[0].state.env["ACTIONS_CUSTOM"], "custom-value");
        assert!(!results[0].state.env.contains_key("GITHUB_TOKEN"));
        assert!(results[0]
            .stdout
            .contains("ACTIONS_RUNTIME_TOKEN=runtime-token"));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_paths_filter_outputs_target_shapes() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "filter".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::PathsFilter,
                inputs: [(
                    "filters".into(),
                    "construct:\n  - 'docker/construct/**'\n  - '.github/workflows/construct.yml'\ndocs:\n  - 'docs/**'\n".into(),
                )]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(GitDiffRunner {
            calls: Vec::new(),
            stdout: "docker/construct/Dockerfile\ndocs/index.md\nREADME.md\n".into(),
        });

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[("GITHUB_SHA".into(), "head-sha".into())],
                &[(
                    "github".into(),
                    serde_json::json!({
                        "event": {
                            "pull_request": {
                                "base": { "sha": "base-sha" },
                                "head": { "sha": "head-sha" }
                            }
                        }
                    }),
                )],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].state.outputs["construct"], "true");
        assert_eq!(results[0].state.outputs["construct_count"], "1");
        assert_eq!(
            results[0].state.outputs["construct_files"],
            "docker/construct/Dockerfile"
        );
        assert_eq!(results[0].state.outputs["docs"], "true");
        assert_eq!(
            results[0].state.outputs["changes"],
            "[\"construct\",\"docs\"]"
        );
        assert!(results[0].stdout.contains("changed: README.md"));
        assert!(executor.runner().calls.iter().any(|(program, args)| {
            program == "git" && args.contains(&"base-sha..head-sha".into())
        }));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_reports_miss_without_node_sidecar() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("crate/src")).unwrap();
        fs::write(
            workspace.join("crate/src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    (
                        "key".into(),
                        "rust-script-${{ runner.os }}-${{ hashFiles('crate/**/*.rs') }}".into(),
                    ),
                    (
                        "restore-keys".into(),
                        "rust-script-${{ runner.os }}-\n".into(),
                    ),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"pub fn answer() -> u8 { 42 }\n"));
        let expected_hash = hex_digest(expected_hash.finalize().as_slice());
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("RUNNER_OS".into(), "Linux".into())],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "");
        assert_eq!(
            results[0].state.outputs["cache-primary-key"],
            format!("rust-script-Linux-{expected_hash}")
        );
        assert_eq!(
            results[0].state.state["primaryKey"],
            format!("rust-script-Linux-{expected_hash}")
        );
        assert!(results[0]
            .stdout
            .contains("Cache not found for input keys: rust-script-Linux-"));
        assert!(results[0]
            .stdout
            .contains("Cache path: ~/.cache/rust-script"));
        assert!(results[1]
            .stderr
            .contains("Cache not saved because no paths exist"));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_can_fail_on_miss() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "target".into()),
                    ("key".into(), "linux-cache".into()),
                    ("fail-on-cache-miss".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 1);
        assert_eq!(results[0].state.outputs["cache-hit"], "");
        assert!(results[0].stderr.contains("fail-on-cache-miss"));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_cache_fail_on_miss_is_quiet_on_hit() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-strict".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &[], &save_temp)
            .unwrap();

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-strict".into()),
                    ("fail-on-cache-miss".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let restore_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &[], &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].exit_code, 0);
        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert!(restore_results[0].stderr.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_trims_folded_yaml_primary_key() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let folded_key = "rust-script-Linux-deadbeef\n";
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), folded_key.into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let save_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &[], &save_temp)
            .unwrap();

        assert_eq!(
            save_results[0].state.outputs["cache-primary-key"],
            "rust-script-Linux-deadbeef"
        );
        assert!(root
            .join("_velnor_caches/rust-script-Linux-deadbeef/0/state.bin")
            .exists());

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "rust-script-Linux-deadbeef".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let restore_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &[], &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "cached\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_saves_and_restores_from_shared_workdir() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let restore_temp = root.join("restore-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let env = vec![("GITHUB_RUN_ID".into(), "123456".into())];
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-abc".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let save_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &env, &save_temp)
            .unwrap();

        assert_eq!(save_results[0].state.outputs["cache-hit"], "");
        assert!(save_results[1]
            .stdout
            .contains("Saved cache 'linux-rust-script-abc'"));
        assert!(root
            .join("_velnor_caches/linux-rust-script-abc/0/state.bin")
            .exists());

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-abc".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let restore_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &env, &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "true");
        assert_eq!(
            restore_results[0].state.outputs["cache-matched-key"],
            "linux-rust-script-abc"
        );
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "cached\n"
        );
        assert!(restore_results[1]
            .stdout
            .contains("Cache hit occurred on primary key"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_lookup_only_does_not_restore_paths() {
        let root = temp_dir();
        let save_temp = root.join("save-job/temp");
        let lookup_temp = root.join("lookup-job/temp");
        fs::create_dir_all(root.join("save-job/home/.cache/rust-script")).unwrap();
        fs::create_dir_all(root.join("lookup-job/home")).unwrap();
        fs::write(
            root.join("save-job/home/.cache/rust-script/state.bin"),
            "cached\n",
        )
        .unwrap();
        let save = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-lookup".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&save_temp), &save, &[], &save_temp)
            .unwrap();

        let lookup = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "linux-rust-script-lookup".into()),
                    ("lookup-only".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let lookup_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&lookup_temp), &lookup, &[], &lookup_temp)
            .unwrap();

        assert_eq!(lookup_results[0].exit_code, 0);
        assert_eq!(lookup_results[0].state.outputs["cache-hit"], "true");
        assert!(lookup_results[0].stdout.contains("Cache lookup found key"));
        assert!(!root
            .join("lookup-job/home/.cache/rust-script/state.bin")
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cache_restore_key_uses_newest_prefix_match() {
        let root = temp_dir();
        let restore_temp = root.join("restore-job/temp");
        let store = root.join("_velnor_caches");
        let old_cache = store.join("rust-linux-a-old");
        let new_cache = store.join("rust-linux-z-new");
        fs::create_dir_all(old_cache.join("0")).unwrap();
        fs::create_dir_all(new_cache.join("0")).unwrap();
        fs::create_dir_all(root.join("restore-job/home")).unwrap();
        fs::write(old_cache.join(".velnor-key"), "rust-linux-a-old").unwrap();
        fs::write(old_cache.join(".velnor-created"), "1").unwrap();
        fs::write(old_cache.join("0/state.bin"), "old\n").unwrap();
        fs::write(new_cache.join(".velnor-key"), "rust-linux-z-new").unwrap();
        fs::write(new_cache.join(".velnor-created"), "2").unwrap();
        fs::write(new_cache.join("0/state.bin"), "new\n").unwrap();

        let restore = vec![ExecutableStep::Native {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Cache,
                inputs: [
                    ("path".into(), "~/.cache/rust-script".into()),
                    ("key".into(), "rust-linux-exact-miss".into()),
                    ("restore-keys".into(), "rust-linux-\n".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let restore_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&restore_temp), &restore, &[], &restore_temp)
            .unwrap();

        assert_eq!(restore_results[0].state.outputs["cache-hit"], "false");
        assert_eq!(
            restore_results[0].state.outputs["cache-matched-key"],
            "rust-linux-z-new"
        );
        assert_eq!(
            fs::read_to_string(root.join("restore-job/home/.cache/rust-script/state.bin")).unwrap(),
            "new\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_docker_adapters_invoke_docker_cli_without_node_sidecars() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work/backend-rust/bitcoin-processor-app")).unwrap();
        fs::write(
            temp.join("work/backend-rust/bitcoin-processor-app/Dockerfile"),
            "FROM scratch\n",
        )
        .unwrap();
        fs::write(
            temp.join("work/backend-rust/docker-bake.hcl"),
            "target \"app\" {}\n",
        )
        .unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "buildx".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DockerSetupBuildx,
                    inputs: [
                        ("name".into(), "velnor-builder".into()),
                        ("driver".into(), "docker-container".into()),
                        ("install".into(), "true".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "login".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DockerLogin,
                    inputs: [
                        ("username".into(), "docker-user".into()),
                        ("password".into(), "${{ secrets.DOCKER_TOKEN }}".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "meta".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DockerMetadata,
                    inputs: [
                        ("images".into(), "chainargos/rust-bitcoin-processor".into()),
                        (
                            "tags".into(),
                            "type=raw,value=latest,enable=false\n\
type=sha,format=long,prefix=,enable=true"
                                .into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DockerBuildPush,
                    inputs: [
                        ("context".into(), ".".into()),
                        (
                            "file".into(),
                            "backend-rust/bitcoin-processor-app/Dockerfile".into(),
                        ),
                        ("platforms".into(), "linux/amd64".into()),
                        ("push".into(), "false".into()),
                        ("tags".into(), "${{ steps.meta.outputs.tags }}".into()),
                        ("labels".into(), "${{ steps.meta.outputs.labels }}".into()),
                        (
                            "cache-from".into(),
                            "type=gha,scope=bitcoin-processor-app-pr".into(),
                        ),
                        (
                            "cache-to".into(),
                            "type=gha,scope=bitcoin-processor-app-pr,mode=max".into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DockerBake,
                    inputs: [
                        ("files".into(), "backend-rust/docker-bake.hcl".into()),
                        ("targets".into(), "bitcoin-processor-app".into()),
                        (
                            "set".into(),
                            "*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max"
                                .into(),
                        ),
                    ]
                    .into(),
                    env: [
                        (
                            "PUSH".into(),
                            "${{ github.event_name != 'pull_request' && 'true' || 'false' }}"
                                .into(),
                        ),
                        ("SHA".into(), "${{ github.sha }}".into()),
                        (
                            "PR_NUMBER".into(),
                            "${{ github.event.pull_request.number }}".into(),
                        ),
                    ]
                    .into(),
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
                    ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
                    ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                    ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
                    ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
                ],
                &[
                    (
                        "secrets".into(),
                        serde_json::json!({ "DOCKER_TOKEN": "docker-token" }),
                    ),
                    (
                        "github".into(),
                        serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
                    ),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 5);
        assert_eq!(
            results[2].state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:abcdef1234567890"
        );
        assert!(results[2].state.outputs["labels"].contains(
            "org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo"
        ));
        let runner = executor.runner();
        let calls = &runner.calls;
        assert!(calls.iter().any(|(program, args)| {
            program == "docker"
                && args.starts_with(&[
                    "buildx".into(),
                    "create".into(),
                    "--name".into(),
                    "velnor-builder".into(),
                ])
        }));
        let login_call = calls.iter().position(|(program, args)| {
            program == "docker"
                && args
                    == &[
                        "login".to_string(),
                        "https://index.docker.io/v1/".to_string(),
                        "--username".to_string(),
                        "docker-user".to_string(),
                        "--password-stdin".to_string(),
                    ]
        });
        assert!(login_call.is_some());
        assert_eq!(runner.stdin[login_call.unwrap()], "docker-token");
        let build_call = calls.iter().position(|(program, args)| {
            program == "docker"
                && args.first().is_some_and(|arg| arg == "buildx")
                && args.get(1).is_some_and(|arg| arg == "build")
                && !args.contains(&"--load".into())
                && !args.contains(&"--push".into())
                && args.contains(&"--tag".into())
                && args.contains(&"chainargos/rust-bitcoin-processor:abcdef1234567890".into())
                && args.contains(&"type=gha,scope=bitcoin-processor-app-pr,mode=max".into())
        });
        assert!(build_call.is_some());
        let build_env = &runner.env[build_call.unwrap()];
        assert!(build_env.contains(&("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into())));
        assert!(build_env.contains(&("ACTIONS_CACHE_URL".into(), "https://cache.actions".into())));
        let bake_call = calls.iter().position(|(program, args)| {
            program == "docker"
                && args.first().is_some_and(|arg| arg == "buildx")
                && args.get(1).is_some_and(|arg| arg == "bake")
                && args.contains(&"--set".into())
                && args.contains(&"*.cache-to=type=gha,scope=rust-workspace,mode=max".into())
                && args.contains(&"bitcoin-processor-app".into())
        });
        assert!(bake_call.is_some());
        let bake_env = &runner.env[bake_call.unwrap()];
        assert!(bake_env.contains(&("PUSH".into(), "false".into())));
        assert!(bake_env.contains(&("SHA".into(), "abcdef1234567890".into())));
        assert!(bake_env.contains(&("PR_NUMBER".into(), "42".into())));
        assert_eq!(
            calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_docker_build_push_honors_load_input_separately() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "build-push".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DockerBuildPush,
                inputs: [
                    ("context".into(), ".".into()),
                    ("push".into(), "false".into()),
                    ("load".into(), "true".into()),
                    ("tags".into(), "example/app:test".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = &executor.runner().calls;
        assert!(calls.iter().any(|(program, args)| {
            program == "docker"
                && args.starts_with(&["buildx".into(), "build".into()])
                && args.contains(&"--load".into())
                && !args.contains(&"--push".into())
        }));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_docker_metadata_matches_target_pr_and_publish_tags() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        let tags_input = "type=raw,value=latest,enable=${{ inputs.publish }}\n\
type=sha,format=long,prefix=,enable=${{ inputs.publish }}\n\
type=raw,value=pr-${{ github.event.pull_request.number }},enable=${{ !inputs.publish && github.event_name == 'pull_request' }}";
        let steps = vec![ExecutableStep::Native {
            step_id: "pr-meta".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DockerMetadata,
                inputs: [
                    ("images".into(), "${{ inputs.image }}".into()),
                    ("tags".into(), tags_input.into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
                    ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                ],
                &[
                    (
                        "inputs".into(),
                        serde_json::json!({
                            "image": "chainargos/rust-bitcoin-processor",
                            "publish": false
                        }),
                    ),
                    (
                        "github".into(),
                        serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
                    ),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(
            results[0].state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:pr-42"
        );

        let publish_state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "push".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                (
                    "GITHUB_REPOSITORY".into(),
                    "ChainArgos/java-monorepo".into(),
                ),
            ],
            &[(
                "inputs".into(),
                serde_json::json!({
                    "image": "chainargos/rust-bitcoin-processor",
                    "publish": true
                }),
            )],
        );
        let publish_action = NativeActionInvocation {
            adapter: NativeActionAdapter::DockerMetadata,
            inputs: [
                ("images".into(), "${{ inputs.image }}".into()),
                ("tags".into(), tags_input.into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let publish_result = native_docker_metadata(&publish_action, &publish_state);
        assert_eq!(
            publish_result.state.outputs["tags"],
            "chainargos/rust-bitcoin-processor:latest\nchainargos/rust-bitcoin-processor:abcdef1234567890"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_docker_metadata_ref_branch_generates_branch_tag() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                ("GITHUB_REPOSITORY".into(), "org/repo".into()),
            ],
            &[],
        );
        let action = NativeActionInvocation {
            adapter: NativeActionAdapter::DockerMetadata,
            inputs: [
                ("images".into(), "ghcr.io/org/repo/fixture".into()),
                (
                    "tags".into(),
                    "type=ref,event=branch\ntype=sha,prefix=sha-".into(),
                ),
            ]
            .into(),
            env: Vec::new(),
        };
        let result = native_docker_metadata(&action, &state);
        assert_eq!(
            result.state.outputs["tags"],
            "ghcr.io/org/repo/fixture:main\nghcr.io/org/repo/fixture:sha-abcdef1"
        );
    }

    #[test]
    fn native_docker_metadata_ref_branch_skipped_for_tag_ref() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_REF".into(), "refs/tags/v1.0.0".into()),
                ("GITHUB_SHA".into(), "abcdef1234567890".into()),
                ("GITHUB_REPOSITORY".into(), "org/repo".into()),
            ],
            &[],
        );
        let action = NativeActionInvocation {
            adapter: NativeActionAdapter::DockerMetadata,
            inputs: [
                ("images".into(), "ghcr.io/org/repo/fixture".into()),
                ("tags".into(), "type=ref,event=branch".into()),
            ]
            .into(),
            env: Vec::new(),
        };
        let result = native_docker_metadata(&action, &state);
        // no branch ref → falls back to sha default
        assert!(result.state.outputs["tags"].starts_with("ghcr.io/org/repo/fixture:sha-"));
    }

    #[test]
    fn native_tool_adapters_use_job_container_without_node_sidecars() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "mise".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::Mise,
                    inputs: [("install".into(), "false".into())].into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "setup-mold".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::SetupMold,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::Sccache,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "setup-just".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::SetupJust,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::RustCache,
                    inputs: [
                        ("shared-key".into(), "kestra-rust-build-cache".into()),
                        ("cache-on-failure".into(), "true".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_WORKSPACE".into(), "/__w".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 7); // 5 main + sccache-post + rust-cache-post
        assert!(results[0].state.path.contains(&"/opt/mise/shims".into()));
        assert!(results[3].state.path.contains(&"/root/.cargo/bin".into()));
        assert_eq!(results[4].state.outputs["cache-hit"], "false");
        assert_eq!(results[4].state.env["CACHE_ON_FAILURE"], "true");
        let docker_exec_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(program, args)| {
                program == "docker" && args.first().is_some_and(|arg| arg == "exec")
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        // mise, mold, just (main steps) + sccache start + sccache post (show-stats + stop)
        assert_eq!(docker_exec_calls.len(), 5);
        assert!(docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("https://mise.run"))));
        assert!(docker_exec_calls.iter().any(|args| args
            .iter()
            .any(|arg| arg.contains("apt-get install -y --no-install-recommends mold"))));
        assert!(docker_exec_calls.iter().any(|args| args
            .iter()
            .any(|arg| arg.contains("apt-get install -y --no-install-recommends just"))));
        assert!(docker_exec_calls
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("sccache --show-stats"))));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn starts_and_waits_for_service_before_job_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let mut container = container(&temp);
        container.services.push(ServiceContainerSpec {
            name: "svc".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "net".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        });
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo ok".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor.execute_step(&container, &step, &temp).unwrap();

        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1, vec!["network", "create", "net"]);
        assert_eq!(calls[1].1[0], "run");
        assert!(calls[1].1.windows(2).any(|pair| pair == ["--name", "svc"]));
        assert_eq!(calls[2].1[0], "inspect");
        assert_eq!(calls[3].1[0], "run");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn retries_start_after_removing_stale_job_resources() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo one".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![1, 0, 0, 0, 0, 0, 0, 0],
        });

        let results = executor
            .execute_steps(&container(&temp), &steps, &temp)
            .unwrap();

        assert_eq!(results.len(), 1);
        let calls = &executor.runner().calls;
        assert_eq!(calls[0].1, vec!["network", "create", "net"]);
        assert_eq!(calls[1].1, vec!["rm", "--force", "job"]);
        assert_eq!(calls[2].1, vec!["network", "rm", "net"]);
        assert_eq!(calls[3].1, vec!["network", "create", "net"]);
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
    fn runtime_checkout_resolves_ref_from_prior_step_output() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "source".into(),
                display_name: String::new(),
                script: "echo source".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Checkout(CheckoutPlan {
                step_id: "checkout2".into(),
                display_name: String::new(),
                clone_url: "https://github.com/jackin-project/jackin.git".into(),
                version: Some("${{ steps.source.outputs.sha }}".into()),
                destination: temp.join("workspace"),
                token: None,
                fetch_depth: None,
                fetch_tags: false,
                persist_credentials: true,
                clean: true,
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(CheckoutOutputRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let fetch = executor
            .runner()
            .calls
            .iter()
            .find(|(program, args)| program == "git" && args.contains(&"fetch".to_string()))
            .unwrap();
        assert!(fetch.1.contains(&"def456".to_string()));
        assert!(!fetch
            .1
            .iter()
            .any(|arg| arg.contains("steps.source.outputs.sha")));
        fs::remove_dir_all(temp).unwrap();
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
                "/github/workflow/event.json".into(),
            ),
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_REPOSITORY_OWNER".into(), "acme".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("DOCS_SITE_URL".into(), "https://docs.example".into()),
            ("RUNNER_OS".into(), "Linux".into()),
            ("RUNNER_NAME".into(), "velnor".into()),
            ("RUNNER_ENVIRONMENT".into(), "self-hosted".into()),
            ("RUNNER_WORKSPACE".into(), "/__w".into()),
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
                ("RUNNER_NAME".into(), "${{ runner.name }}".into()),
                (
                    "RUNNER_ENVIRONMENT".into(),
                    "${{ runner.environment }}".into()
                ),
                ("RUNNER_WORKSPACE".into(), "${{ runner.workspace }}".into()),
                ("ACTION_STATUS".into(), "${{ github.action_status }}".into()),
            ]),
            vec![
                ("INPUT_TOKEN".into(), "ghs_token".into()),
                ("ACTION".into(), "setup".into()),
                ("ACTION_PATH".into(), "/__a/actions_setup-node/v4".into()),
                ("ACTION_REF".into(), "v4".into()),
                ("ACTION_REPOSITORY".into(), "actions/setup-node".into()),
                ("EVENT_PATH".into(), "/github/workflow/event.json".into()),
                ("OWNER".into(), "acme".into()),
                ("SERVER_URL".into(), "https://github.com".into()),
                ("DOCS_SITE_URL".into(), "https://docs.example".into()),
                ("WORKSPACE".into(), "/__w".into()),
                ("OS".into(), "Linux".into()),
                ("RUNNER_NAME".into(), "velnor".into()),
                ("RUNNER_ENVIRONMENT".into(), "self-hosted".into()),
                ("RUNNER_WORKSPACE".into(), "/__w".into()),
                ("ACTION_STATUS".into(), "success".into()),
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
                        "repository": "jackin-project/jackin",
                        "event": {
                            "pull_request": { "number": 42 },
                            "workflow_run": {
                                "conclusion": "success",
                                "event": "push",
                                "head_sha": "def456",
                                "head_branch": "main",
                                "head_repository": {
                                    "full_name": "jackin-project/jackin"
                                }
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
            state.resolve_expressions("literal=${{ 'a || b && c == d' }}"),
            "literal=a || b && c == d"
        );
        assert_eq!(
            state.resolve_expressions(
                "push=${{ (github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true') || (github.event_name == 'workflow_dispatch' && inputs.packages) }}"
            ),
            "push=bitcoin-processor-app"
        );
        assert_eq!(
            state.resolve_expressions(
                "selected=${{ contains(inputs.packages, 'BITCOIN-PROCESSOR-APP') }}"
            ),
            "selected=true"
        );
        assert_eq!(
            state.resolve_expressions("comma=${{ contains('alpha,beta', 'BETA') }}"),
            "comma=true"
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
            state.resolve_expressions("event=${{ github.event_name == 'WORKFLOW_DISPATCH' }}"),
            "event=true"
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
            state.resolve_expressions(
                "same=${{ github.event.workflow_run.head_repository.full_name == github.repository }}"
            ),
            "same=true"
        );
        assert_eq!(
            state.resolve_expressions("token=${{ secrets.DOCKERHUB_TOKEN }}"),
            "token=docker_secret"
        );
        assert_eq!(
            state.resolve_expressions("missing=${{ github.event.issue.number }}"),
            "missing="
        );
        assert_eq!(
            state.resolve_expressions("missing-input=${{ inputs.publish }}"),
            "missing-input="
        );
        assert!(state.evaluate_condition(Some("matrix.zigbuild")));
        assert!(state.evaluate_condition(Some("matrix.target")));
        assert!(!state.evaluate_condition(Some("inputs.publish")));
        assert!(!state.evaluate_condition(Some("secrets.MISSING_TOKEN")));
        assert!(state.evaluate_condition(Some("contains(matrix.target, 'apple')")));
        assert!(state.evaluate_condition(Some("needs.test-bitcoin-processor.result == 'failure'")));
        assert!(state.evaluate_condition(Some(
            "needs.changes.outputs.bitcoin-processor == 'true' || (github.event_name == 'workflow_dispatch' && (inputs.packages == '' || contains(inputs.packages, 'bitcoin-processor-app')))"
        )));
        assert!(
            state.evaluate_condition(Some("contains(inputs.packages, 'BITCOIN-PROCESSOR-APP')"))
        );
        assert!(state.evaluate_condition(Some("needs.changes.outputs.bake-targets != ''")));
        assert!(
            !state.evaluate_condition(Some("(github.event_name == 'workflow_dispatch') == false"))
        );
        assert!(
            state.evaluate_condition(Some("(github.event_name == 'workflow_dispatch') == true"))
        );

        let false_state = JobExecutionState::new_with_context(
            &[],
            &[(
                "matrix".into(),
                serde_json::json!({
                    "zigbuild": false
                }),
            )],
        );
        assert!(!false_state.evaluate_condition(Some("matrix.zigbuild")));
    }

    #[test]
    fn target_workflow_run_preview_gate_matches_jackin_shape() {
        let state = JobExecutionState::new_with_context(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_run".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ],
            &[(
                "github".into(),
                serde_json::json!({
                    "repository": "jackin-project/jackin",
                    "event": {
                        "workflow_run": {
                            "conclusion": "success",
                            "event": "push",
                            "head_repository": {
                                "full_name": "jackin-project/jackin"
                            },
                            "head_branch": "main",
                            "head_sha": "def456"
                        }
                    }
                }),
            )],
        );

        assert!(state.evaluate_condition(Some(
            "github.event_name == 'workflow_run' && \
             github.event.workflow_run.conclusion == 'success' && \
             github.event.workflow_run.event == 'push' && \
             github.event.workflow_run.head_repository.full_name == github.repository && \
             github.event.workflow_run.head_branch == 'main'"
        )));
        assert_eq!(
            state.resolve_expressions("sha=${{ github.event.workflow_run.head_sha }}"),
            "sha=def456"
        );
    }

    #[test]
    fn target_workflow_expressions_use_supported_subset() {
        let workflow_roots = [
            Path::new("/tmp/velnor-targets/jackin/.github/workflows"),
            Path::new("/tmp/velnor-targets/java-monorepo/.github/workflows"),
        ];
        if workflow_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let workspace = std::env::current_dir().unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_REF_NAME".into(), "main".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                ("GITHUB_RUN_ID".into(), "123456".into()),
                ("GITHUB_RUN_NUMBER".into(), "42".into()),
                ("GITHUB_SHA".into(), "abc123".into()),
                ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ("GITHUB_WORKFLOW".into(), "CI".into()),
                ("GITHUB_WORKSPACE".into(), "/__w".into()),
                ("RUNNER_OS".into(), "Linux".into()),
                ("DOCS_SITE_URL".into(), "https://docs.example".into()),
                (
                    "JACKIN_REPO_EDIT_URL".into(),
                    "https://github.com/jackin-project/jackin/edit/main".into(),
                ),
                (
                    "JACKIN_REPO_BLOB_URL".into(),
                    "https://github.com/jackin-project/jackin/blob/main".into(),
                ),
                (
                    "REGISTRY_IMAGE".into(),
                    "docker.io/jackinproject/jackin".into(),
                ),
                ("DIGEST_DIR".into(), "/tmp/digests".into()),
                ("BUILDX_BUILDER".into(), "velnor-builder".into()),
            ],
            &target_expression_context(),
            &workspace,
            &workspace,
        );

        for root in workflow_roots.into_iter().filter(|root| root.exists()) {
            for path in workflow_files(root) {
                let contents = fs::read_to_string(&path).unwrap();
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents).unwrap();
                let mut strings = Vec::new();
                collect_yaml_strings(&yaml, &mut strings);
                for value in strings {
                    if value.contains("${{") {
                        let rendered = state.resolve_expressions(value);
                        assert!(
                            !rendered.contains("${{"),
                            "{} left unresolved expression in {value:?}: {rendered:?}",
                            path.display()
                        );
                    }
                }

                let mut conditions = Vec::new();
                collect_yaml_key_strings(&yaml, "if", &mut conditions);
                for condition in conditions {
                    state.evaluate_condition(Some(condition));
                }
            }
        }
    }

    #[test]
    fn cached_target_action_metadata_expressions_use_supported_subset() {
        let action_roots = [
            Path::new("/tmp/velnor-actions"),
            Path::new("/tmp/velnor-targets/jackin/.github/actions"),
        ];
        if action_roots.iter().all(|root| !root.exists()) {
            return;
        }

        let workspace = std::env::current_dir().unwrap();
        let state = JobExecutionState::new_with_workspace(
            &[
                ("GITHUB_ACTION".into(), "setup".into()),
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_action/v1".into(),
                ),
                ("CACHE_HIT".into(), "false".into()),
                ("CACHE_KEY".into(), "node_modules-cache".into()),
                ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                ("GITHUB_RUN_ID".into(), "123456".into()),
                ("GITHUB_RUN_NUMBER".into(), "42".into()),
                ("GITHUB_SHA".into(), "abc123".into()),
                ("GITHUB_TOKEN".into(), "ghs_token".into()),
                ("GITHUB_WORKSPACE".into(), "/__w".into()),
                ("RUNNER_OS".into(), "Linux".into()),
                ("RUNNER_TEMP".into(), "/__t".into()),
                ("RUNNER_TOOL_CACHE".into(), "/__tool".into()),
            ],
            &target_expression_context(),
            &workspace,
            &workspace,
        );

        for root in action_roots.into_iter().filter(|root| root.exists()) {
            for path in action_metadata_files(root) {
                let contents = fs::read_to_string(&path).unwrap();
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(&contents).unwrap();
                let mut strings = Vec::new();
                collect_yaml_strings(&yaml, &mut strings);
                for value in strings {
                    if value.contains("${{") {
                        let rendered = state.resolve_expressions(value);
                        assert!(
                            !rendered.contains("${{"),
                            "{} left unresolved expression in {value:?}: {rendered:?}",
                            path.display()
                        );
                    }
                }

                let mut conditions = Vec::new();
                collect_yaml_key_strings(&yaml, "if", &mut conditions);
                for condition in conditions {
                    state.evaluate_condition(Some(condition));
                }
            }
        }
    }

    #[test]
    fn injects_resolved_step_environment_into_script_exec() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "env-step".into(),
            display_name: String::new(),
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
    fn initial_environment_expressions_are_resolved() {
        let state = JobExecutionState::new(&[
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("OUTER".into(), "${{ github.token }}".into()),
            ("REVISION".into(), "rev-${{ github.sha }}".into()),
        ]);

        let env = state.step_env(&[]);

        assert!(env.contains(&("OUTER".into(), "ghs_token".into())));
        assert!(env.contains(&("REVISION".into(), "rev-abc123".into())));
    }

    #[test]
    fn target_jackin_release_job_env_resolves_needs_version() {
        let state = JobExecutionState::new_with_context(
            &[(
                "VERSION".into(),
                "${{ needs.check-version.outputs.version }}".into(),
            )],
            &[(
                "needs".into(),
                serde_json::json!({
                    "check-version": {
                        "outputs": {
                            "version": "0.6.0"
                        },
                        "result": "success"
                    }
                }),
            )],
        );

        let env = state.step_env(&[]);

        assert!(env.contains(&("VERSION".into(), "0.6.0".into())));
    }

    #[test]
    fn script_steps_receive_github_action_context() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "build".into(),
            display_name: String::new(),
            script: "echo ${{ github.action }}".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: vec![("ACTION_NAME".into(), "${{ github.action }}".into())],
            condition: Some("${{ github.action == 'build' }}".into()),
            continue_on_error: false,
        })];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let exec_args = &executor.runner().calls[2].1;
        assert!(exec_args.contains(&"GITHUB_ACTION=build".into()));
        assert!(exec_args.contains(&"ACTION_NAME=build".into()));
        let script_path = exec_args
            .iter()
            .position(|arg| arg.ends_with(".sh"))
            .and_then(|index| exec_args.get(index))
            .expect("script path should be present");
        let script = fs::read_to_string(host_temp_script_path(script_path, &temp)).unwrap();
        assert_eq!(script, "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\necho build\n");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn script_step_environment_is_not_resolved_twice() {
        let mut state = JobExecutionState::new(&[("GITHUB_TOKEN".into(), "ghs_token".into())]);
        state.apply(
            "producer",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    env: [("OUTER".into(), "${{ github.token }}".into())].into(),
                    ..StepCommandState::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let resolved = state.resolve_env(&[("GENERATED".into(), "${{ env.OUTER }}".into())]);

        assert_eq!(
            resolved,
            vec![("GENERATED".into(), "${{ github.token }}".into())]
        );
    }

    #[test]
    fn writes_github_event_payload_and_injects_event_path() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "event-step".into(),
            display_name: String::new(),
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
        assert!(exec_args.contains(&"GITHUB_EVENT_PATH=/github/workflow/event.json".into()));
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
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
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
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_check_image_output_gates_java_monorepo_build_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let condition = "steps.check-image.outputs.exists == 'false'";
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "check-image".into(),
                display_name: String::new(),
                script: r#"if [ "$(just exists ${{ inputs.package }})" = "true" ]; then
  echo "exists=true" >> "$GITHUB_OUTPUT"
else
  echo "exists=false" >> "$GITHUB_OUTPUT"
fi"#
                .into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::JavaScript {
                step_id: "login-docker-hub".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login-action/v4/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_login-action/v4".into(),
                    env: vec![
                        (
                            "INPUT_USERNAME".into(),
                            "${{ secrets.DOCKERHUB_USERNAME }}".into(),
                        ),
                        (
                            "INPUT_PASSWORD".into(),
                            "${{ secrets.DOCKERHUB_TOKEN }}".into(),
                        ),
                    ],
                },
                condition: Some(condition.into()),
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "setup-buildx".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path:
                        "/__a/_actions/docker_setup-buildx-action/v4/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action/v4".into(),
                    env: Vec::new(),
                },
                condition: Some(condition.into()),
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "build-docker-image".into(),
                display_name: String::new(),
                script: "just build-${{ inputs.package }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: vec![("GITHUB_TOKEN".into(), "${{ secrets.GITHUB_TOKEN }}".into())],
                condition: Some(condition.into()),
                continue_on_error: false,
            }),
        ];
        let context = vec![
            (
                "inputs".into(),
                serde_json::json!({ "package": "bitcoin-processor-app" }),
            ),
            (
                "secrets".into(),
                serde_json::json!({
                    "DOCKERHUB_USERNAME": "docker_user",
                    "DOCKERHUB_TOKEN": "docker_secret",
                    "GITHUB_TOKEN": "ghs_token"
                }),
            ),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|result| !result.skipped));
        assert_eq!(results[0].state.outputs["exists"], "false");
        assert_eq!(
            fs::read_to_string(temp.join("build-docker-image.sh")).unwrap(),
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\njust build-bitcoin-processor-app\n"
        );
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[0].contains(&"INPUT_USERNAME=docker_user".into()));
        assert!(node_calls[0].contains(&"INPUT_PASSWORD=docker_secret".into()));
        let build_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| args.iter().any(|arg| arg == "/__t/build-docker-image.sh"))
            .expect("build script should execute");
        assert!(build_exec.1.contains(&"GITHUB_TOKEN=ghs_token".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_job_outputs_from_final_step_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "producer".into(),
            display_name: String::new(),
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
    fn target_docs_environment_url_uses_deployment_step_output() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "deployment".into(),
            display_name: String::new(),
            script: "echo page_url=https://example.com/docs/ >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        })];
        let environment_url = serde_json::json!({
            "type": "String",
            "value": "${{ steps.deployment.outputs.page_url }}"
        });
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_completion(
                &container(&temp),
                &steps,
                &[],
                &[],
                None,
                Some(&environment_url),
                &temp,
            )
            .unwrap();

        assert_eq!(
            summary.environment_url.as_deref(),
            Some("https://jackin-project.github.io/jackin/")
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docs_sitemap_step_receives_deployment_page_url() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "deployment".into(),
                display_name: String::new(),
                script: "echo deploy".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "sitemap".into(),
                display_name: String::new(),
                script: r#"echo "url=${PAGE_URL%/}/sitemap.xml" >> "$GITHUB_OUTPUT""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![(
                    "PAGE_URL".into(),
                    "${{ steps.deployment.outputs.page_url }}".into(),
                )],
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let sitemap_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_program, args)| args.iter().any(|arg| arg.ends_with("/sitemap.sh")))
            .expect("sitemap script should be executed");
        assert!(sitemap_exec
            .1
            .contains(&"PAGE_URL=https://jackin-project.github.io/jackin/".into()));
        assert_eq!(
            fs::read_to_string(temp.join("sitemap.sh")).unwrap(),
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\necho \"url=${PAGE_URL%/}/sitemap.xml\" >> \"$GITHUB_OUTPUT\"\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_check_deployed_docs_runtime_inputs_resolve_after_sitemap_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "deployment".into(),
                display_name: String::new(),
                script: "echo deploy".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "sitemap".into(),
                display_name: String::new(),
                script: r#"echo "url=${PAGE_URL%/}/sitemap.xml" >> "$GITHUB_OUTPUT""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![(
                    "PAGE_URL".into(),
                    "${{ steps.deployment.outputs.page_url }}".into(),
                )],
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "check-deployed-1".into(),
                display_name: String::new(),
                script: r#"lychee --dump "${{ inputs.sitemap-url }}""#.into(),
                shell: Shell::Bash,
                working_directory_container: "/__w/repo".into(),
                env: vec![
                    (
                        "SITEMAP_URL".into(),
                        "${{ steps.sitemap.outputs.url }}".into(),
                    ),
                    ("EDIT_URL".into(), "${{ env.JACKIN_REPO_EDIT_URL }}".into()),
                    ("BLOB_URL".into(), "${{ env.JACKIN_REPO_BLOB_URL }}".into()),
                ],
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    (
                        "JACKIN_REPO_EDIT_URL".into(),
                        "https://github.com/jackin-project/jackin/edit/main".into(),
                    ),
                    (
                        "JACKIN_REPO_BLOB_URL".into(),
                        "https://github.com/jackin-project/jackin/blob/main".into(),
                    ),
                ],
                &temp,
            )
            .unwrap();

        let check_exec = executor
            .runner()
            .calls
            .iter()
            .find(|(_program, args)| args.iter().any(|arg| arg.ends_with("/check-deployed-1.sh")))
            .expect("check-deployed script should be executed");
        assert!(check_exec
            .1
            .contains(&("SITEMAP_URL=https://jackin-project.github.io/jackin/sitemap.xml".into())));
        assert!(check_exec
            .1
            .contains(&("EDIT_URL=https://github.com/jackin-project/jackin/edit/main".into())));
        assert!(check_exec
            .1
            .contains(&("BLOB_URL=https://github.com/jackin-project/jackin/blob/main".into())));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn evaluates_typed_job_outputs_from_final_step_state() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Script(ScriptStep {
            id: "producer".into(),
            display_name: String::new(),
            script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
        })];
        let job_outputs = serde_json::json!({
            "type": "map",
            "map": [
                {
                    "Key": { "lit": "answer" },
                    "Value": { "lit": "${{ steps.producer.outputs.answer }}" }
                },
                {
                    "Key": { "lit": "fallback" },
                    "Value": { "value": "${{ steps.missing.outputs.value || 'default' }}" }
                },
                {
                    "Key": { "lit": "empty" },
                    "Value": { "lit": "${{ steps.missing.outputs.value }}" }
                }
            ]
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
    fn target_rust_docker_job_outputs_resolve_after_filter_and_targets_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::CompositeOutputs {
                step_id: "filter".into(),
                outputs: BTreeMap::from([("bitcoin-processor".into(), "false".into())]),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "targets".into(),
                outputs: BTreeMap::from([("list".into(), "bitcoin-processor-app".into())]),
                condition: None,
            },
        ];
        let job_outputs = serde_json::json!({
            "bitcoin-processor": "${{ github.event_name == 'workflow_dispatch' && 'true' || steps.filter.outputs.bitcoin-processor }}",
            "bake-targets": "${{ steps.targets.outputs.list }}"
        });
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let summary = executor
            .execute_ordered_steps_with_job_outputs(
                &container(&temp),
                &steps,
                &[("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into())],
                &[],
                Some(&job_outputs),
                &temp,
            )
            .unwrap();

        assert_eq!(summary.job_outputs["bitcoin-processor"], "true");
        assert_eq!(summary.job_outputs["bake-targets"], "bitcoin-processor-app");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_jackin_dispatch_or_filter_job_output_uses_runtime_fallback() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::CompositeOutputs {
                step_id: "dispatch".into(),
                outputs: BTreeMap::new(),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "filter".into(),
                outputs: BTreeMap::from([("docs".into(), "true".into())]),
                condition: None,
            },
        ];
        let job_outputs = serde_json::json!({
            "docs": "${{ steps.dispatch.outputs.docs || steps.filter.outputs.docs }}"
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

        assert_eq!(summary.job_outputs["docs"], "true");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_jackin_release_job_outputs_collect_platform_shas() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::CompositeOutputs {
            step_id: "shas".into(),
            outputs: BTreeMap::from([
                ("arm64_linux".into(), "sha-arm64-linux".into()),
                ("arm64_macos".into(), "sha-arm64-macos".into()),
                ("capsule_arm64_linux".into(), "sha-capsule-arm64".into()),
                ("capsule_x86_linux".into(), "sha-capsule-x86".into()),
                ("x86_linux".into(), "sha-x86-linux".into()),
                ("x86_macos".into(), "sha-x86-macos".into()),
            ]),
            condition: None,
        }];
        let job_outputs = serde_json::json!({
            "arm64_linux": "${{ steps.shas.outputs.arm64_linux }}",
            "arm64_macos": "${{ steps.shas.outputs.arm64_macos }}",
            "capsule_arm64_linux": "${{ steps.shas.outputs.capsule_arm64_linux }}",
            "capsule_x86_linux": "${{ steps.shas.outputs.capsule_x86_linux }}",
            "x86_linux": "${{ steps.shas.outputs.x86_linux }}",
            "x86_macos": "${{ steps.shas.outputs.x86_macos }}"
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

        assert_eq!(summary.job_outputs["arm64_linux"], "sha-arm64-linux");
        assert_eq!(summary.job_outputs["arm64_macos"], "sha-arm64-macos");
        assert_eq!(
            summary.job_outputs["capsule_arm64_linux"],
            "sha-capsule-arm64"
        );
        assert_eq!(summary.job_outputs["capsule_x86_linux"], "sha-capsule-x86");
        assert_eq!(summary.job_outputs["x86_linux"], "sha-x86-linux");
        assert_eq!(summary.job_outputs["x86_macos"], "sha-x86-macos");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn materializes_composite_outputs_as_outer_step_outputs() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
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
                display_name: String::new(),
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
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\necho artifact=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn emits_step_started_events_for_real_executable_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
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
                display_name: String::new(),
                script: "echo artifact=${{ steps.composite.outputs.artifact-id }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        })
        .with_step_start_sender(sender);

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        assert_eq!(
            events,
            vec![
                StepStartEvent {
                    step_id: "producer".into(),
                    display_name: String::new(),
                    order: 1
                },
                StepStartEvent {
                    step_id: "consumer".into(),
                    display_name: String::new(),
                    order: 2
                }
            ]
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn emits_step_logs_with_timeline_order_after_each_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut executor =
            DockerScriptExecutor::new(StdoutCommandRunner::default()).with_step_log_sender(sender);

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();
        let mut logs = Vec::new();
        while let Ok(log) = receiver.try_recv() {
            logs.push(log);
        }

        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].step_id, "producer");
        assert_eq!(logs[0].order, 1);
        assert_eq!(logs[1].step_id, "consumer");
        assert_eq!(logs[1].order, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn reports_silent_and_skipped_steps_for_completion() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "silent".into(),
                display_name: String::new(),
                script: "true".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "skipped".into(),
                display_name: String::new(),
                script: "echo unreachable".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("${{ false }}".into()),
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(SilentCommandRunner::default());

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

        assert_eq!(summary.step_results.len(), 2);
        assert_eq!(summary.step_logs.len(), 2);
        assert_eq!(summary.step_logs[0].step_id, "silent");
        assert_eq!(summary.step_logs[0].order, 1);
        assert!(summary.step_logs[0].lines.is_empty());
        assert!(!summary.step_logs[0].skipped);
        assert_eq!(summary.step_logs[1].step_id, "skipped");
        assert_eq!(summary.step_logs[1].order, 2);
        assert!(summary.step_logs[1].lines.is_empty());
        assert!(summary.step_logs[1].skipped);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workflow_commands_from_stdout_update_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
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
        assert_eq!(results[0].state.error_count, 1);
        assert_eq!(results[0].state.warning_count, 0);
        assert_eq!(results[0].state.notice_count, 0);
        assert_eq!(summary.step_logs[0].step_id, "producer");
        assert!(summary.step_logs[0].lines.contains(&"hidden".to_string()));
        assert_eq!(summary.step_logs[0].masks, vec!["hidden"]);
        assert_eq!(summary.step_logs[0].error_count, 1);
        assert_eq!(summary.step_logs[0].exit_code, 0);
        assert!(!summary.step_logs[0].skipped);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\nexport PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workflow_commands_from_stderr_update_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "node old-action.js".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(StderrCommandRunner::default());

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
        assert_eq!(results[0].state.warning_count, 1);
        assert_eq!(summary.step_logs[0].masks, vec!["hidden"]);
        assert_eq!(summary.step_logs[0].warning_count, 1);
        assert_eq!(
            fs::read_to_string(temp.join("consumer.sh")).unwrap(),
            "export HOME=/root\nexport RUSTUP_HOME=/root/.rustup\nexport CARGO_HOME=/root/.cargo\nexport PATH='/opt/tool':\"$PATH\"\necho answer=42\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn step_log_includes_step_summary_file_content() {
        let result = StepExecutionResult {
            exit_code: 0,
            state: StepCommandState {
                summary: "### cache stats\nhit rate: 80%\n".into(),
                ..StepCommandState::default()
            },
            skipped: false,
            failure_ignored: false,
            stdout: "done\n".into(),
            stderr: String::new(),
        };

        let log = step_log("sccache", 1, &result);

        assert_eq!(
            log.lines,
            vec![
                "done".to_string(),
                "Step summary:".to_string(),
                "### cache stats".to_string(),
                "hit rate: 80%".to_string()
            ]
        );
    }

    #[test]
    fn resolves_hash_files_in_action_env() {
        let temp = temp_dir();
        let workspace = temp.join("work");
        fs::create_dir_all(workspace.join("kestra-docker-containers/app/src")).unwrap();
        fs::create_dir_all(workspace.join("kestra-docker-containers/app")).unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/app/src/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/app/build.toml"),
            "image = 'app'\n",
        )
        .unwrap();
        fs::write(
            workspace.join("kestra-docker-containers/justfile"),
            "build:\n",
        )
        .unwrap();
        fs::write(workspace.join("ignored.txt"), "ignored\n").unwrap();
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"image = 'app'\n"));
        expected_hash.update(Sha256::digest(b"fn main() {}\n"));
        expected_hash.update(Sha256::digest(b"build:\n"));
        let expected = hex_digest(expected_hash.finalize().as_slice());
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node24".into(),
                pre_container_path: None,
                pre_condition: None,
                main_container_path: "/__a/_actions/actions_cache/dist/restore/index.js".into(),
                post_container_path: None,
                post_condition: None,
                action_container_path: "/__a/_actions/actions_cache".into(),
                env: vec![
                    ("INPUT_PATH".into(), "~/.cache/rust-script".into()),
                    (
                        "INPUT_KEY".into(),
                        "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}".into(),
                    ),
                    (
                        "INPUT_RESTORE-KEYS".into(),
                        "rust-script-${{ runner.os }}-\n".into(),
                    ),
                ],
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("RUNNER_OS".into(), "Linux".into())],
                &temp,
            )
            .unwrap();

        assert!(!expected.is_empty());
        let cache_args = &executor.runner().calls[2].1;
        assert!(cache_args.contains(&"INPUT_PATH=~/.cache/rust-script".into()));
        assert!(cache_args.contains(&format!("INPUT_KEY=rust-script-Linux-{expected}")));
        assert!(cache_args.contains(&"INPUT_RESTORE-KEYS=rust-script-Linux-\n".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn skips_steps_when_output_condition_is_false() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "producer".into(),
                display_name: String::new(),
                script: "echo answer=42 >> $GITHUB_OUTPUT".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                display_name: String::new(),
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
    fn target_required_job_cancelled_need_condition_fails_and_skips_ok_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "check-failed".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some(
                    "needs.check.result == 'failure' || needs.check.result == 'cancelled'".into(),
                ),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "bitcoin-cancelled".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some(
                    "needs.test-bitcoin-processor.result == 'failure' || \
                     needs.test-bitcoin-processor.result == 'cancelled'"
                        .into(),
                ),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "ok".into(),
                display_name: String::new(),
                script: "echo OK".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let context = vec![(
            "needs".into(),
            serde_json::json!({
                "check": { "result": "success" },
                "test-bitcoin-processor": { "result": "cancelled" }
            }),
        )];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1],
        });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &context, &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert!(results[0].skipped);
        assert_eq!(results[1].exit_code, 1);
        assert!(!results[1].failure_ignored);
        assert!(results[2].skipped);
        let exec_scripts = executor
            .runner()
            .calls
            .iter()
            .filter_map(|(_, args)| {
                (args.first().is_some_and(|arg| arg == "exec"))
                    .then(|| args.last().cloned())
                    .flatten()
            })
            .collect::<Vec<_>>();
        assert_eq!(exec_scripts, vec!["/__t/bitcoin-cancelled.sh"]);
        assert!(!temp.join("check-failed.sh").exists());
        assert!(!temp.join("ok.sh").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn continue_on_error_keeps_failure_outcome_but_runs_later_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/sccache/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/sccache".into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: true,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable-sccache".into(),
                display_name: String::new(),
                script: "echo SCCACHE_GHA_ENABLED=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.sccache.outcome == 'success'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "next".into(),
                display_name: String::new(),
                script: "echo next".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("success()".into()),
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
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
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "exec")
                    || (args.first().is_some_and(|arg| arg == "run")
                        && args.contains(&"node:20-bookworm".into()))
            })
            .count();
        assert_eq!(exec_count, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn conditions_without_status_check_require_success() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "plain-condition".into(),
                display_name: String::new(),
                script: "echo plain".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("steps.fail.outcome == 'failure'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "status-condition".into(),
                display_name: String::new(),
                script: "echo status".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: Some("failure() && steps.fail.outcome == 'failure'".into()),
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 1, 0, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 3);
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
        assert!(state.evaluate_condition(Some("steps.sccache.conclusion == 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.outcome != 'success'")));
        assert!(state.evaluate_condition(Some("steps.disabled.conclusion == 'skipped'")));
        assert!(state.evaluate_condition(Some("runner.os == 'Linux'")));
        assert!(state.evaluate_condition(Some("runner.os == 'linux'")));
        assert!(state.evaluate_condition(Some("runner.os != 'windows'")));
        assert_eq!(state.resolve_expressions("${{ job.status }}"), "success");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "success"
        );
        assert!(state.evaluate_condition(Some("job.status == 'success'")));
        assert!(state.evaluate_condition(Some("github.action_status == 'success'")));
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
        assert_eq!(state.resolve_expressions("${{ job.status }}"), "failure");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
        assert!(state.evaluate_condition(Some("always() && job.status == 'failure'")));
        assert!(state.evaluate_condition(Some("always() && github.action_status == 'failure'")));

        let mut ignored_state = JobExecutionState::default();
        ignored_state.apply(
            "ignored",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: true,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert!(ignored_state.evaluate_condition(Some("steps.ignored.outcome == 'failure'")));
        assert!(ignored_state.evaluate_condition(Some("steps.ignored.conclusion == 'success'")));
        assert!(ignored_state.evaluate_condition(Some("success()")));
        assert!(!ignored_state.evaluate_condition(Some("failure()")));
        assert_eq!(
            ignored_state.resolve_expressions("${{ job.status }}"),
            "success"
        );
    }

    #[test]
    fn github_action_status_uses_current_composite_scope() {
        let mut state = JobExecutionState::default();
        state.apply(
            "top-level-failure",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        state.push_composite("composite");

        assert_eq!(state.resolve_expressions("${{ job.status }}"), "failure");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "success"
        );

        state.apply(
            "composite-child",
            &StepExecutionResult {
                exit_code: 1,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState::default(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
        state.pop_composite("composite");
        assert_eq!(
            state.resolve_expressions("${{ github.action_status }}"),
            "failure"
        );
    }

    #[test]
    fn executes_javascript_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = JavaScriptActionInvocation {
            node: "node20".into(),
            pre_container_path: None,
            pre_condition: None,
            main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
            post_container_path: None,
            post_condition: None,
            action_container_path: "/__a/_actions/acme_action/v1".into(),
            env: vec![
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_action/v1".into(),
                ),
                ("INPUT_NAME".into(), "value".into()),
                (
                    "INPUT_ACTION_PATH".into(),
                    "${{ github.action_path }}".into(),
                ),
            ],
        };
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let result = executor
            .execute_javascript_action(&container(&temp), "action1", &action, &temp)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "run");
        assert!(calls[2].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[2]
            .1
            .contains(&"INPUT_ACTION_PATH=/__a/_actions/acme_action/v1".into()));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/action1_output".into()));
        assert!(calls[2].1.ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/acme_action/v1/dist/index.js".into()
        ]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_docker_action_with_inputs_and_command_files() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = DockerActionInvocation {
            image: "alpine:3.20".into(),
            build_context_host: None,
            dockerfile_host: None,
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            env: vec![
                (
                    "GITHUB_ACTION_PATH".into(),
                    "/__a/_actions/acme_docker/v1".into(),
                ),
                ("INPUT_NAME".into(), "value".into()),
                (
                    "INPUT_ACTION_PATH".into(),
                    "${{ github.action_path }}".into(),
                ),
            ],
            entrypoint: Some("/entrypoint.sh".into()),
            args: vec!["arg1".into()],
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 1);
        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "run");
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--workdir", "/github/workspace"]));
        assert!(calls[2].1.contains(&format!(
            "{}:/github/workspace",
            temp.join("work").display()
        )));
        assert!(calls[2]
            .1
            .contains(&format!("{}:/github/runner_temp", temp.display())));
        assert!(calls[2]
            .1
            .contains(&format!("{}:/github/file_commands", temp.display())));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_WORKSPACE=/github/workspace".into()));
        assert!(calls[2]
            .1
            .contains(&"RUNNER_TEMP=/github/runner_temp".into()));
        assert!(calls[2].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[2]
            .1
            .contains(&"INPUT_ACTION_PATH=/__a/_actions/acme_docker/v1".into()));
        assert!(calls[2]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/docker1_output".into()));
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/entrypoint.sh"]));
        assert!(calls[2].1.ends_with(&["alpine:3.20".into(), "arg1".into()]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn resolves_docker_action_entrypoint_and_args_at_execution_time() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let action = DockerActionInvocation {
            image: "alpine:3.20".into(),
            build_context_host: None,
            dockerfile_host: None,
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            env: Vec::new(),
            entrypoint: Some("${{ env.DOCKER_ENTRYPOINT }}".into()),
            args: vec![
                "pr-${{ github.event.pull_request.number }}".into(),
                "${{ secrets.DOCKER_TOKEN }}".into(),
            ],
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
        }];
        let context = vec![
            (
                "github".into(),
                serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
            ),
            (
                "secrets".into(),
                serde_json::json!({ "DOCKER_TOKEN": "secret-token" }),
            ),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &[("DOCKER_ENTRYPOINT".into(), "/entrypoint.sh".into())],
                &context,
                &temp,
            )
            .unwrap();

        let calls = &executor.runner().calls;
        assert!(calls[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/entrypoint.sh"]));
        assert!(calls[2].1.ends_with(&[
            "alpine:3.20".into(),
            "pr-42".into(),
            "secret-token".into()
        ]));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn builds_local_docker_action_before_running_it() {
        let temp = temp_dir();
        let action_dir = temp.join("actions/acme");
        fs::create_dir_all(&action_dir).unwrap();
        let action = DockerActionInvocation {
            image: "velnor-action-acme-docker-v1-root".into(),
            build_context_host: Some(action_dir.clone()),
            dockerfile_host: Some(action_dir.join("Dockerfile")),
            action_container_path: "/__a/_actions/acme_docker/v1".into(),
            env: Vec::new(),
            entrypoint: None,
            args: Vec::new(),
        };
        let steps = vec![ExecutableStep::Docker {
            step_id: "docker1".into(),
            display_name: String::new(),
            invocation: action,
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let calls = &executor.runner().calls;
        assert_eq!(calls[2].1[0], "build");
        assert!(calls[2]
            .1
            .contains(&"velnor-action-acme-docker-v1-root".into()));
        assert_eq!(calls[3].1[0], "run");
        assert!(calls[3]
            .1
            .contains(&"velnor-action-acme-docker-v1-root".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_ordered_script_and_javascript_steps_in_one_container() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "step1".into(),
                display_name: String::new(),
                script: "echo NAME=value >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::JavaScript {
                step_id: "action1".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
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
                display_name: String::new(),
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
        assert_eq!(calls[3].1[0], "run");
        assert_eq!(calls[4].1[0], "exec");
        assert_eq!(calls[5].1[0], "rm");
        assert_eq!(calls[6].1[0], "network");
        assert!(calls[3].1.contains(&"INPUT_NAME=value".into()));
        assert!(calls[3].1.contains(&"GITHUB_REPOSITORY=acme/repo".into()));
        assert!(calls[3].1.contains(&"TOKEN=ghs_token".into()));
        assert!(calls[3]
            .1
            .contains(&"GITHUB_OUTPUT=/github/file_commands/action1_output".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_cache_and_artifact_actions_receive_runtime_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Native {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::Cache,
                    inputs: [
                        ("path".into(), "~/.cache/rust-script".into()),
                        (
                            "key".into(),
                            "rust-script-${{ runner.os }}-${{ hashFiles('kestra-docker-containers/**/*.rs', 'kestra-docker-containers/**/build.toml', 'kestra-docker-containers/justfile') }}".into(),
                        ),
                        (
                            "restore-keys".into(),
                            "rust-script-${{ runner.os }}-\n".into(),
                        ),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "upload".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::UploadArtifact,
                    inputs: [
                        (
                            "name".into(),
                            "construct-digest-${{ matrix.platform }}".into(),
                        ),
                        (
                            "path".into(),
                            "${{ env.DIGEST_DIR }}/${{ matrix.platform }}.digest".into(),
                        ),
                        ("if-no-files-found".into(), "error".into()),
                        ("retention-days".into(), "1".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "download".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DownloadArtifact,
                    inputs: [
                        ("pattern".into(), "construct-digest-*".into()),
                        ("path".into(), "${{ env.DIGEST_DIR }}".into()),
                        ("merge-multiple".into(), "true".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Native {
                step_id: "github-runtime".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::GitHubRuntimeExport,
                    inputs: [("github-token".into(), "ghs_token".into())].into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_RETENTION_DAYS".into(), "90".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("RUNNER_OS".into(), "Linux".into()),
            ("DIGEST_DIR".into(), "/__w/digests".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            (
                "ACTIONS_RUNTIME_URL".into(),
                "https://runtime.actions".into(),
            ),
            (
                "ACTIONS_RESULTS_URL".into(),
                "https://results.actions".into(),
            ),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
        ];
        let workspace = temp.join("work/kestra-docker-containers");
        fs::create_dir_all(workspace.join("app/src")).unwrap();
        fs::write(workspace.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(workspace.join("app/build.toml"), "image = 'app'\n").unwrap();
        fs::write(workspace.join("justfile"), "build:\n").unwrap();
        fs::create_dir_all(temp.join("work/digests")).unwrap();
        fs::write(temp.join("work/digests/linux-amd64.digest"), "sha256:abc\n").unwrap();
        let mut expected_hash = Sha256::new();
        expected_hash.update(Sha256::digest(b"image = 'app'\n"));
        expected_hash.update(Sha256::digest(b"fn main() {}\n"));
        expected_hash.update(Sha256::digest(b"build:\n"));
        let expected_hash = hex_digest(expected_hash.finalize().as_slice());
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &runtime_env,
                &[(
                    "matrix".into(),
                    serde_json::json!({ "platform": "linux-amd64" }),
                )],
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 0);
        assert_eq!(results[0].state.outputs["cache-hit"], "");
        assert_eq!(
            results[0].state.outputs["cache-primary-key"],
            format!("rust-script-Linux-{expected_hash}")
        );
        assert!(results[0]
            .stdout
            .contains("Cache path: ~/.cache/rust-script"));
        let expected_artifact_id = artifact_id_for_name(
            &JobExecutionState::new(&runtime_env),
            "construct-digest-linux-amd64",
        );
        assert_eq!(
            results[1].state.outputs["artifact-id"],
            expected_artifact_id
        );
        assert_eq!(
            results[1].state.outputs["artifact-url"],
            format!("https://results.actions/artifacts/{expected_artifact_id}")
        );
        assert_eq!(results[2].state.outputs["download-path"], "/__w/digests");
        assert_eq!(
            results[3].state.env["ACTIONS_RUNTIME_TOKEN"],
            "runtime-token"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_artifacts_are_shared_across_jobs_in_same_run_workdir() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work/digests")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(
            upload_temp.join("work/digests/linux-amd64.digest"),
            "sha256:abc\n",
        )
        .unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::UploadArtifact,
                inputs: [
                    ("name".into(), "construct-digest-linux-amd64".into()),
                    ("path".into(), "/__w/digests/linux-amd64.digest".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DownloadArtifact,
                inputs: [
                    ("pattern".into(), "construct-digest-*".into()),
                    ("path".into(), "/__w/downloaded".into()),
                    ("merge-multiple".into(), "true".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/linux-amd64.digest")).unwrap(),
            "sha256:abc\n"
        );
        assert!(root
            .join("_velnor_artifacts/123456-1/construct-digest-linux-amd64/linux-amd64.digest")
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_download_artifact_all_mode_uses_named_directories() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(upload_temp.join("work/linux.txt"), "linux\n").unwrap();
        fs::write(upload_temp.join("work/macos.txt"), "macos\n").unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        for (name, path) in [
            ("artifact-linux", "/__w/linux.txt"),
            ("artifact-macos", "/__w/macos.txt"),
        ] {
            let upload = vec![ExecutableStep::Native {
                step_id: format!("upload-{name}"),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::UploadArtifact,
                    inputs: [
                        ("name".into(), name.into()),
                        ("path".into(), path.into()),
                        ("if-no-files-found".into(), "error".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            }];
            DockerScriptExecutor::new(RecordingRunner::default())
                .execute_ordered_steps(
                    &container(&upload_temp),
                    &upload,
                    &runtime_env,
                    &upload_temp,
                )
                .unwrap();
        }
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DownloadArtifact,
                inputs: [("path".into(), "/__w/downloaded".into())].into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/artifact-linux/linux.txt"))
                .unwrap(),
            "linux\n"
        );
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloaded/artifact-macos/macos.txt"))
                .unwrap(),
            "macos\n"
        );
        assert!(!download_temp.join("work/downloaded/linux.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_download_artifact_reports_container_download_path() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        fs::write(upload_temp.join("work/output.txt"), "artifact\n").unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/github/workspace".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::UploadArtifact,
                inputs: [
                    ("name".into(), "output".into()),
                    ("path".into(), "/github/workspace/output.txt".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DownloadArtifact,
                inputs: [
                    ("name".into(), "output".into()),
                    ("path".into(), "downloads/output".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            results[0].state.outputs["download-path"],
            "/github/workspace/downloads/output"
        );
        assert_eq!(
            fs::read_to_string(download_temp.join("work/downloads/output/output.txt")).unwrap(),
            "artifact\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn native_download_artifact_normalizes_permissions() {
        let root = temp_dir();
        let upload_temp = root.join("upload-job/temp");
        let download_temp = root.join("download-job/temp");
        fs::create_dir_all(upload_temp.join("work/bin")).unwrap();
        fs::create_dir_all(download_temp.join("work")).unwrap();
        let script = upload_temp.join("work/bin/tool.sh");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let runtime_env = vec![
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        let upload = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::UploadArtifact,
                inputs: [
                    ("name".into(), "bin".into()),
                    ("path".into(), "/__w/bin".into()),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&upload_temp),
                &upload,
                &runtime_env,
                &upload_temp,
            )
            .unwrap();
        let download = vec![ExecutableStep::Native {
            step_id: "download".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DownloadArtifact,
                inputs: [
                    ("name".into(), "bin".into()),
                    ("path".into(), "/__w/downloaded".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&download_temp),
                &download,
                &runtime_env,
                &download_temp,
            )
            .unwrap();

        let downloaded_dir = download_temp.join("work/downloaded");
        let downloaded_file = downloaded_dir.join("tool.sh");
        assert_eq!(
            fs::metadata(&downloaded_dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&downloaded_file).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_upload_artifact_expands_target_release_globs() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        fs::write(temp.join("work/jackin-1.2.3-x86_64.tar.gz"), "archive\n").unwrap();
        fs::write(
            temp.join("work/jackin-1.2.3-x86_64.tar.gz.sha256"),
            "hash\n",
        )
        .unwrap();
        fs::write(temp.join("work/ignore.txt"), "no\n").unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::UploadArtifact,
                inputs: [
                    ("name".into(), "jackin-x86_64-unknown-linux-gnu".into()),
                    (
                        "path".into(),
                        "jackin-*.tar.gz\njackin-*.tar.gz.sha256".into(),
                    ),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/jackin-1.2.3-x86_64.tar.gz")
            )
            .unwrap(),
            "archive\n"
        );
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/jackin-1.2.3-x86_64.tar.gz.sha256")
            )
            .unwrap(),
            "hash\n"
        );
        assert!(!temp
            .join("_velnor_artifacts/local-1/jackin-x86_64-unknown-linux-gnu/ignore.txt")
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_excludes_hidden_files_by_default() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work/dist/.well-known")).unwrap();
        fs::write(temp.join("work/dist/app.js"), "app\n").unwrap();
        fs::write(temp.join("work/dist/.env"), "secret\n").unwrap();
        fs::write(temp.join("work/dist/.well-known/assetlinks.json"), "{}\n").unwrap();
        let upload = |include_hidden_files: Option<&str>, name: &str| {
            let mut inputs: BTreeMap<String, String> = [
                ("name".into(), name.into()),
                ("path".into(), "dist".into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into();
            if let Some(value) = include_hidden_files {
                inputs.insert("include-hidden-files".into(), value.into());
            }
            vec![ExecutableStep::Native {
                step_id: format!("upload-{name}"),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::UploadArtifact,
                    inputs,
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            }]
        };

        let default_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload(None, "default"), &[], &temp)
            .unwrap();
        let explicit_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &upload(Some("true"), "explicit"),
                &[],
                &temp,
            )
            .unwrap();

        assert_eq!(default_results[0].exit_code, 0);
        assert_eq!(explicit_results[0].exit_code, 0);
        assert!(temp
            .join("_velnor_artifacts/local-1/default/app.js")
            .exists());
        assert!(!temp.join("_velnor_artifacts/local-1/default/.env").exists());
        assert!(!temp
            .join("_velnor_artifacts/local-1/default/.well-known")
            .exists());
        assert!(temp
            .join("_velnor_artifacts/local-1/explicit/.env")
            .exists());
        assert!(temp
            .join("_velnor_artifacts/local-1/explicit/.well-known/assetlinks.json")
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_requires_overwrite_for_duplicate_name() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("work")).unwrap();
        fs::write(temp.join("work/first.txt"), "first\n").unwrap();
        fs::write(temp.join("work/second.txt"), "second\n").unwrap();
        let upload = |path: &str, overwrite: Option<&str>| {
            let mut inputs: BTreeMap<String, String> = [
                ("name".into(), "duplicate".into()),
                ("path".into(), path.into()),
                ("if-no-files-found".into(), "error".into()),
            ]
            .into();
            if let Some(value) = overwrite {
                inputs.insert("overwrite".into(), value.into());
            }
            vec![ExecutableStep::Native {
                step_id: format!("upload-{}", path.replace('.', "-")),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::UploadArtifact,
                    inputs,
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            }]
        };

        let first_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload("first.txt", None), &[], &temp)
            .unwrap();
        let duplicate_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &upload("second.txt", None), &[], &temp)
            .unwrap();

        assert_eq!(first_results[0].exit_code, 0);
        // Velnor always overwrites on duplicate (re-runs on same slot reuse artifact store).
        assert_eq!(duplicate_results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(temp.join("_velnor_artifacts/local-1/duplicate/second.txt"))
                .unwrap(),
            "second\n"
        );
        let overwrite_results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(
                &container(&temp),
                &upload("second.txt", Some("true")),
                &[],
                &temp,
            )
            .unwrap();
        assert_eq!(overwrite_results[0].exit_code, 0);
        assert!(!temp
            .join("_velnor_artifacts/local-1/duplicate/first.txt")
            .exists());
        assert_eq!(
            fs::read_to_string(temp.join("_velnor_artifacts/local-1/duplicate/second.txt"))
                .unwrap(),
            "second\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_upload_artifact_maps_container_tmp_to_host_temp() {
        let temp = temp_dir();
        fs::create_dir_all(temp.join("jackin-construct-digests")).unwrap();
        fs::write(
            temp.join("jackin-construct-digests/amd64.digest"),
            "sha256:abc\n",
        )
        .unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "upload".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::UploadArtifact,
                inputs: [
                    ("name".into(), "construct-digest-amd64".into()),
                    (
                        "path".into(),
                        "/tmp/jackin-construct-digests/amd64.digest".into(),
                    ),
                    ("if-no-files-found".into(), "error".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];

        let results = DockerScriptExecutor::new(RecordingRunner::default())
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(
                temp.join("_velnor_artifacts/local-1/construct-digest-amd64/amd64.digest")
            )
            .unwrap(),
            "sha256:abc\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_artifact_ids_are_stable_per_run_and_name() {
        let state = JobExecutionState::new(&[
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_ATTEMPT".into(), "1".into()),
        ]);

        let first = artifact_id_for_name(&state, "construct-digest-linux-amd64");
        let repeat = artifact_id_for_name(&state, "construct-digest-linux-amd64");
        let second = artifact_id_for_name(&state, "construct-digest-linux-arm64");

        assert_eq!(first, repeat);
        assert_ne!(first, second);
        assert!(first.parse::<u64>().is_ok());
    }

    #[test]
    fn target_paths_filter_receives_event_context_and_outputs_gate_steps() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "paths".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/dorny_paths-filter/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/dorny_paths-filter".into(),
                    env: vec![
                        ("INPUT_TOKEN".into(), "${{ github.token }}".into()),
                        ("INPUT_BASE".into(), "main".into()),
                        (
                            "INPUT_FILTERS".into(),
                            "construct:\n  - 'construct/**'\n  - '.github/workflows/construct.yml'"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "consume".into(),
                display_name: String::new(),
                script: "echo construct changed".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: Some("steps.paths.outputs.construct == 'true'".into()),
                continue_on_error: false,
            }),
        ];
        let context_data = vec![(
            "github".into(),
            serde_json::json!({
                "event": {
                    "pull_request": {
                        "number": 42,
                        "base": { "sha": "base-sha" }
                    },
                    "repository": { "default_branch": "main" }
                }
            }),
        )];
        let base_env = vec![
            ("GITHUB_EVENT_NAME".into(), "pull_request".into()),
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_SHA".into(), "head-sha".into()),
            ("GITHUB_REF".into(), "refs/pull/42/merge".into()),
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps_with_context(
                &container(&temp),
                &steps,
                &base_env,
                &context_data,
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state.outputs["construct"], "true");
        assert_eq!(results[0].state.outputs["construct_count"], "1");
        assert!(!results[1].skipped);
        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(node_call.contains(&"INPUT_TOKEN=ghs_token".into()));
        assert!(node_call.contains(&"INPUT_BASE=main".into()));
        assert!(node_call
            .iter()
            .any(|arg| arg.starts_with("INPUT_FILTERS=construct:\n")));
        assert!(node_call.contains(&"GITHUB_EVENT_NAME=pull_request".into()));
        assert!(node_call.contains(&"GITHUB_EVENT_PATH=/github/workflow/event.json".into()));
        assert!(node_call.contains(&"GITHUB_REPOSITORY=jackin-project/jackin".into()));
        assert!(node_call.contains(&"GITHUB_WORKSPACE=/__w".into()));
        assert!(executor.runner().calls.iter().any(|(_, args)| {
            args.first().is_some_and(|arg| arg == "exec")
                && args.iter().any(|arg| arg == "/__t/consume.sh")
        }));
        let event = fs::read_to_string(temp.join("_github_workflow/event.json")).unwrap();
        assert!(event.contains("\"pull_request\""));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_pages_actions_receive_runtime_env_and_outputs() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let outputs = [(
            "artifact_id".to_string(),
            "${{ steps.upload-artifact.outputs.artifact-id }}".to_string(),
        )]
        .into();
        let steps = vec![
            ExecutableStep::CompositeStart {
                step_id: "pages-artifact".into(),
            },
            ExecutableStep::Script(ScriptStep {
                id: "pages-artifact-1".into(),
                display_name: String::new(),
                script: "tar --directory \"$INPUT_PATH\" -cvf \"$RUNNER_TEMP/artifact.tar\" ."
                    .into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: vec![("INPUT_PATH".into(), "docs/dist".into())],
                condition: Some("runner.os == 'Linux'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::Native {
                step_id: "upload-artifact".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::UploadArtifact,
                    inputs: [
                        ("name".into(), "github-pages".into()),
                        ("path".into(), "${{ runner.temp }}/artifact.tar".into()),
                        ("retention-days".into(), "1".into()),
                        ("if-no-files-found".into(), "error".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "pages-artifact".into(),
                outputs,
                condition: None,
            },
            ExecutableStep::CompositeEnd {
                step_id: "pages-artifact".into(),
            },
            ExecutableStep::Native {
                step_id: "deploy-pages".into(),
                display_name: String::new(),
                invocation: NativeActionInvocation {
                    adapter: NativeActionAdapter::DeployPages,
                    inputs: [
                        ("token".into(), "${{ github.token }}".into()),
                        ("artifact_name".into(), "github-pages".into()),
                    ]
                    .into(),
                    env: Vec::new(),
                },
                condition: Some("steps.pages-artifact.outputs.artifact_id != ''".into()),
                continue_on_error: false,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("GITHUB_RUN_ID".into(), "123456".into()),
            ("GITHUB_RUN_NUMBER".into(), "12".into()),
            ("GITHUB_SHA".into(), "abc123".into()),
            ("GITHUB_ACTOR".into(), "donbeave".into()),
            ("GITHUB_ACTION".into(), "deploy-pages".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            ("GITHUB_API_URL".into(), "https://api.github.com".into()),
            ("GITHUB_RETENTION_DAYS".into(), "90".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            (
                "ACTIONS_RESULTS_URL".into(),
                "https://results.actions".into(),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_URL".into(),
                "https://oidc.actions/token".into(),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN".into(),
                "id-token-request-token".into(),
            ),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });
        fs::write(temp.join("artifact.tar"), "pages archive\n").unwrap();

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        let expected_artifact_id =
            artifact_id_for_name(&JobExecutionState::new(&runtime_env), "github-pages");
        assert_eq!(
            results[2].state.outputs["artifact_id"],
            expected_artifact_id
        );
        assert_eq!(
            results[3].state.outputs["page_url"],
            "https://jackin-project.github.io/jackin/"
        );
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 0);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docker_javascript_actions_receive_socket_and_cli_mounts() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "buildx".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    env: vec![("INPUT_INSTALL".into(), "true".into())],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "login".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login-action/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_login-action".into(),
                    env: vec![
                        ("INPUT_USERNAME".into(), "docker-user".into()),
                        ("INPUT_PASSWORD".into(), "docker-token".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "metadata".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_metadata-action/dist/index.cjs"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_metadata-action".into(),
                    env: vec![
                        ("INPUT_IMAGES".into(), "ghcr.io/chainargos/app".into()),
                        ("INPUT_TAGS".into(), "type=sha".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_build-push-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_build-push-action".into(),
                    env: vec![
                        ("INPUT_CONTEXT".into(), ".".into()),
                        ("INPUT_PUSH".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    post_container_path: Some(
                        "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    ),
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_bake-action".into(),
                    env: vec![
                        ("INPUT_FILES".into(), "docker-bake.hcl".into()),
                        ("INPUT_TARGETS".into(), "app".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps(
                &spec,
                &steps,
                &[
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 6);
        for call in &node_calls {
            assert!(call.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
            assert!(call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
            assert!(call.contains(
                &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
            ));
            assert!(call.contains(&"GITHUB_TOKEN=ghs_token".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_INSTALL=true".into()));
        assert!(node_calls[1].contains(&"INPUT_USERNAME=docker-user".into()));
        assert!(node_calls[2].contains(&"INPUT_IMAGES=ghcr.io/chainargos/app".into()));
        assert!(node_calls[3].contains(&"INPUT_CONTEXT=.".into()));
        assert!(node_calls[4].contains(&"INPUT_FILES=docker-bake.hcl".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn native_setup_buildx_reuses_existing_builder() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "buildx".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::DockerSetupBuildx,
                inputs: [
                    ("name".into(), "jackin-construct".into()),
                    ("driver".into(), "docker-container".into()),
                ]
                .into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results[0].exit_code, 0);
        assert_eq!(results[0].state.outputs["name"], "jackin-construct");
        assert_eq!(results[0].state.env["BUILDX_BUILDER"], "jackin-construct");
        let calls = &executor.runner().calls;
        let inspect_call = calls
            .iter()
            .position(|(program, args)| {
                program == "docker"
                    && args
                        == &[
                            "buildx".to_string(),
                            "inspect".to_string(),
                            "jackin-construct".to_string(),
                        ]
            })
            .unwrap();
        let use_call = calls
            .iter()
            .position(|(program, args)| {
                program == "docker"
                    && args
                        == &[
                            "buildx".to_string(),
                            "use".to_string(),
                            "jackin-construct".to_string(),
                        ]
            })
            .unwrap();
        assert!(inspect_call < use_call);
        assert!(!calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "buildx")
                && args.get(1).is_some_and(|arg| arg == "create")));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_docker_action_inputs_match_current_workflows() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "buildx-reusable".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    env: vec![
                        (
                            "INPUT_NAME".into(),
                            "chainargos-${{ inputs.app }}-builder".into(),
                        ),
                        ("INPUT_DRIVER".into(), "docker-container".into()),
                        ("INPUT_CLEANUP".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "buildx-jackin".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_setup-buildx-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_setup-buildx-action".into(),
                    env: vec![
                        ("INPUT_NAME".into(), "${{ env.BUILDX_BUILDER }}".into()),
                        ("INPUT_DRIVER".into(), "docker-container".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "metadata".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_metadata-action/dist/index.cjs"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_metadata-action".into(),
                    env: vec![
                        ("INPUT_IMAGES".into(), "${{ inputs.image }}".into()),
                        (
                            "INPUT_TAGS".into(),
                            "type=raw,value=latest,enable=${{ inputs.publish }}\n\
type=sha,format=long,prefix=,enable=${{ inputs.publish }}\n\
type=raw,value=pr-${{ github.event.pull_request.number }},enable=${{ !inputs.publish && github.event_name == 'pull_request' }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "meta".into(),
                outputs: BTreeMap::from([
                    (
                        "tags".into(),
                        "chainargos/rust-bitcoin-processor:pr-42".into(),
                    ),
                    (
                        "labels".into(),
                        "org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo"
                            .into(),
                    ),
                ]),
                condition: None,
            },
            ExecutableStep::CompositeOutputs {
                step_id: "cache".into(),
                outputs: BTreeMap::from([
                    ("from".into(), "type=gha,scope=bitcoin-processor-app-pr".into()),
                    (
                        "to".into(),
                        "type=gha,scope=bitcoin-processor-app-pr,mode=max".into(),
                    ),
                ]),
                condition: None,
            },
            ExecutableStep::JavaScript {
                step_id: "build-push".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_build-push-action/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_build-push-action".into(),
                    env: vec![
                        ("INPUT_CONTEXT".into(), ".".into()),
                        (
                            "INPUT_FILE".into(),
                            "backend-rust/${{ inputs.app }}/Dockerfile".into(),
                        ),
                        ("INPUT_PLATFORMS".into(), "linux/amd64".into()),
                        ("INPUT_PUSH".into(), "${{ inputs.publish }}".into()),
                        ("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into()),
                        ("INPUT_LABELS".into(), "${{ steps.meta.outputs.labels }}".into()),
                        ("INPUT_CACHE-FROM".into(), "${{ steps.cache.outputs.from }}".into()),
                        ("INPUT_CACHE-TO".into(), "${{ steps.cache.outputs.to }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "bake".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_bake-action/dist/index.cjs".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/docker_bake-action".into(),
                    env: vec![
                        ("INPUT_FILES".into(), "backend-rust/docker-bake.hcl".into()),
                        ("INPUT_TARGETS".into(), "${{ needs.changes.outputs.bake-targets }}".into()),
                        (
                            "INPUT_SET".into(),
                            "*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max\n\
bitcoin-processor-app.push=${{ (github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true') || (github.event_name == 'workflow_dispatch' && inputs.push) }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());
        let context = vec![
            (
                "inputs".into(),
                serde_json::json!({
                    "app": "bitcoin-processor-app",
                    "image": "chainargos/rust-bitcoin-processor",
                    "publish": false,
                    "push": true
                }),
            ),
            (
                "github".into(),
                serde_json::json!({ "event": { "pull_request": { "number": 42 } } }),
            ),
            (
                "needs".into(),
                serde_json::json!({
                    "changes": {
                        "outputs": {
                            "bake-targets": "bitcoin-processor-app",
                            "bitcoin-processor": "true"
                        }
                    }
                }),
            ),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &spec,
                &steps,
                &[
                    ("GITHUB_EVENT_NAME".into(), "workflow_dispatch".into()),
                    (
                        "GITHUB_REPOSITORY".into(),
                        "ChainArgos/java-monorepo".into(),
                    ),
                    ("BUILDX_BUILDER".into(), "jackin-construct".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &context,
                &temp,
            )
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 5);
        for call in &node_calls {
            assert!(call.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
            assert!(call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
            assert!(call.contains(
                &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
            ));
        }
        assert!(
            node_calls[0].contains(&"INPUT_NAME=chainargos-bitcoin-processor-app-builder".into())
        );
        assert!(node_calls[0].contains(&"INPUT_DRIVER=docker-container".into()));
        assert!(node_calls[0].contains(&"INPUT_CLEANUP=false".into()));
        assert!(node_calls[1].contains(&"INPUT_NAME=jackin-construct".into()));
        assert!(node_calls[2].contains(&"INPUT_IMAGES=chainargos/rust-bitcoin-processor".into()));
        assert!(node_calls[2].contains(
            &("INPUT_TAGS=type=raw,value=latest,enable=false\n\
type=sha,format=long,prefix=,enable=false\n\
type=raw,value=pr-42,enable=false")
                .into()
        ));
        assert!(node_calls[3]
            .contains(&"INPUT_FILE=backend-rust/bitcoin-processor-app/Dockerfile".into()));
        assert!(node_calls[3].contains(&"INPUT_PUSH=false".into()));
        assert!(
            node_calls[3].contains(&"INPUT_TAGS=chainargos/rust-bitcoin-processor:pr-42".into())
        );
        assert!(node_calls[3].contains(
            &"INPUT_LABELS=org.opencontainers.image.source=https://github.com/ChainArgos/java-monorepo".into()
        ));
        assert!(node_calls[3]
            .contains(&"INPUT_CACHE-FROM=type=gha,scope=bitcoin-processor-app-pr".into()));
        assert!(node_calls[3]
            .contains(&"INPUT_CACHE-TO=type=gha,scope=bitcoin-processor-app-pr,mode=max".into()));
        assert!(node_calls[4].contains(&"INPUT_FILES=backend-rust/docker-bake.hcl".into()));
        assert!(node_calls[4].contains(&"INPUT_TARGETS=bitcoin-processor-app".into()));
        assert!(node_calls[4].contains(
            &("INPUT_SET=*.cache-from=type=gha,scope=rust-workspace\n\
*.cache-to=type=gha,scope=rust-workspace,mode=max\n\
bitcoin-processor-app.push=true")
                .into()
        ));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_renovate_action_receives_docker_cli_socket_and_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::Native {
            step_id: "renovate".into(),
            display_name: String::new(),
            invocation: NativeActionInvocation {
                adapter: NativeActionAdapter::Renovate,
                inputs: [
                    ("token".into(), "${{ secrets.RENOVATE_TOKEN }}".into()),
                    ("renovate-version".into(), "43".into()),
                ]
                .into(),
                env: vec![
                    (
                        "RENOVATE_REPOSITORIES".into(),
                        "${{ github.repository }}".into(),
                    ),
                    ("RENOVATE_ONBOARDING".into(), "false".into()),
                    ("LOG_LEVEL".into(), "debug".into()),
                ],
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut spec = container(&temp);
        spec.mount_docker_socket = true;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        let context = vec![(
            "secrets".into(),
            serde_json::json!({ "RENOVATE_TOKEN": "renovate-token" }),
        )];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        executor
            .execute_ordered_steps_with_context(
                &spec,
                &steps,
                &[(
                    "GITHUB_REPOSITORY".into(),
                    "ChainArgos/java-monorepo".into(),
                )],
                &context,
                &temp,
            )
            .unwrap();

        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"ghcr.io/renovatebot/renovate:43".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(node_call.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(node_call.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(node_call.contains(&"INPUT_TOKEN=renovate-token".into()));
        assert!(node_call.contains(&"RENOVATE_TOKEN=renovate-token".into()));
        assert!(node_call.contains(&"RENOVATE_REPOSITORIES=ChainArgos/java-monorepo".into()));
        assert!(node_call.contains(&"RENOVATE_ONBOARDING=false".into()));
        assert!(node_call.contains(&"LOG_LEVEL=debug".into()));
        assert!(node_call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
        assert!(node_call.ends_with(&["ghcr.io/renovatebot/renovate:43".into()]));
        assert_eq!(
            executor
                .runner()
                .calls
                .iter()
                .filter(|(_, args)| args.first().is_some_and(|arg| arg == "run")
                    && args.iter().any(|arg| arg.starts_with("node:")))
                .count(),
            0
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_actions_receive_prior_github_path_entries() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "pather".into(),
                display_name: String::new(),
                script: "echo /root/.cargo/bin >> $GITHUB_PATH".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::JavaScript {
                step_id: "cargo-install".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cargo-install/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/cargo-install".into(),
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

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let node_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .unwrap();
        assert!(node_call
            .last()
            .is_some_and(|arg| arg.contains("export PATH='/root/.cargo/bin':\"$PATH\"")));
        assert!(node_call.last().is_some_and(
            |arg| arg.contains("exec node '/__a/_actions/cargo-install/dist/index.js'")
        ));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_setup_actions_share_home_toolcache_and_path() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "mise".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/jdx_mise-action/dist/index.js".into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/jdx_mise-action".into(),
                    env: vec![
                        ("INPUT_INSTALL".into(), "false".into()),
                        ("INPUT_CACHE".into(), "false".into()),
                        ("INPUT_GITHUB_TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "setup-python".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/actions_setup-python/dist/setup/index.js"
                        .into(),
                    post_container_path: Some(
                        "/__a/_actions/actions_setup-python/dist/cache-save/index.js".into(),
                    ),
                    post_condition: Some("success()".into()),
                    action_container_path: "/__a/_actions/actions_setup-python".into(),
                    env: vec![
                        ("INPUT_PYTHON-VERSION".into(), "3.13".into()),
                        (
                            "INPUT_TOKEN".into(),
                            "${{ github.server_url == 'https://github.com' && github.token || '' }}"
                                .into(),
                        ),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let base_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &base_env, &temp)
            .unwrap();

        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 3);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
            assert!(call.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_GITHUB_TOKEN=ghs_token".into()));
        assert!(node_calls[0].contains(&"GITHUB_PATH=/github/file_commands/mise_path".into()));
        assert!(node_calls[1].contains(&"INPUT_PYTHON-VERSION=3.13".into()));
        assert!(node_calls[1].contains(&"INPUT_TOKEN=ghs_token".into()));
        assert!(
            node_calls[1].contains(&"GITHUB_PATH=/github/file_commands/setup-python_path".into())
        );
        assert!(node_calls[1]
            .last()
            .is_some_and(|arg| { arg.contains("export PATH='/opt/mise/shims':\"$PATH\"") }));
        assert!(node_calls[2].ends_with(&[
            "node:24-bookworm".into(),
            "sh".into(),
            "-lc".into(),
            "export PATH='/opt/mise/shims':\"$PATH\"\nexec node '/__a/_actions/actions_setup-python/dist/cache-save/index.js'".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_rust_tool_installers_share_cargo_home_path_and_cache_env() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::Script(ScriptStep {
                id: "toolchain".into(),
                display_name: String::new(),
                script: "echo CARGO_HOME=/github/home/.cargo >> \"$GITHUB_ENV\"\necho /root/.cargo/bin >> \"$GITHUB_PATH\"".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: Some("runner.os != 'Windows'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "setup-mold".into(),
                display_name: String::new(),
                script: "echo mold 2.41.0".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: Some("runner.os == 'Linux'".into()),
                continue_on_error: false,
            }),
            ExecutableStep::JavaScript {
                step_id: "setup-just".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/extractions_setup-crate/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/extractions_setup-crate".into(),
                    env: vec![
                        ("INPUT_REPO".into(), "casey/just".into()),
                        ("INPUT_GITHUB-TOKEN".into(), "${{ github.token }}".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "cargo-install".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/baptiste0928_cargo-install/dist/index.js"
                        .into(),
                    post_container_path: None,
                    post_condition: None,
                    action_container_path: "/__a/_actions/baptiste0928_cargo-install".into(),
                    env: vec![
                        ("INPUT_CRATE".into(), "cargo-binstall".into()),
                        ("INPUT_VERSION".into(), "latest".into()),
                        ("INPUT_LOCKED".into(), "true".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
        ];
        let runtime_env = vec![
            ("GITHUB_TOKEN".into(), "ghs_token".into()),
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
            ("RUNNER_OS".into(), "Linux".into()),
        ];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert!(!results[0].skipped);
        assert!(!results[1].skipped);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"CARGO_HOME=/github/home/.cargo".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
            assert!(call
                .last()
                .is_some_and(|arg| { arg.contains("export PATH='/root/.cargo/bin':\"$PATH\"") }));
        }
        assert!(node_calls[0].contains(&"INPUT_REPO=casey/just".into()));
        assert!(node_calls[0].contains(&"INPUT_GITHUB-TOKEN=ghs_token".into()));
        assert!(node_calls[1].contains(&"INPUT_CRATE=cargo-binstall".into()));
        assert!(node_calls[1].contains(&"INPUT_VERSION=latest".into()));
        assert!(node_calls[1].contains(&"INPUT_LOCKED=true".into()));
        assert!(node_calls[1].contains(&"ACTIONS_RUNTIME_TOKEN=runtime-token".into()));
        assert!(node_calls[1].contains(&"ACTIONS_CACHE_URL=https://cache.actions".into()));
        assert!(node_calls[1].contains(&"ACTIONS_CACHE_SERVICE_V2=True".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_javascript_post_actions_in_reverse_order() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                    post_container_path: Some("/__a/_actions/cache/dist/save.js".into()),
                    post_condition: None,
                    action_container_path: "/__a/_actions/cache".into(),
                    env: vec![("INPUT_KEY".into(), "linux-cache".into())],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::JavaScript {
                step_id: "login".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/docker_login/dist/main.js".into(),
                    post_container_path: Some("/__a/_actions/docker_login/dist/post.js".into()),
                    post_condition: None,
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
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert!(exec_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/cache/dist/restore.js".into()
        ]));
        assert!(exec_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/docker_login/dist/main.js".into()
        ]));
        assert!(exec_calls[2].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/docker_login/dist/post.js".into()
        ]));
        assert!(exec_calls[3].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/cache/dist/save.js".into()
        ]));
        assert!(exec_calls[3].contains(&"STATE_primaryKey=linux-cache".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn executes_javascript_pre_action_before_main() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "cache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/cache/dist/pre.js".into()),
                pre_condition: Some("always()".into()),
                main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                post_container_path: None,
                post_condition: None,
                action_container_path: "/__a/_actions/cache".into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(OutputWritingRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert!(node_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/cache/dist/pre.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/cache/dist/restore.js".into()
        ]));
        assert!(node_calls[1].contains(&"STATE_primaryKey=linux-cache".into()));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_action_state_accumulates_across_pre_and_main_for_post() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "wrapped".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/wrapped/dist/pre.js".into()),
                pre_condition: None,
                main_container_path: "/__a/_actions/wrapped/dist/main.js".into(),
                post_container_path: Some("/__a/_actions/wrapped/dist/post.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/wrapped".into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(PhaseStateRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        let post_call = executor
            .runner()
            .calls
            .iter()
            .find(|(_, args)| {
                args.iter()
                    .any(|arg| arg == "/__a/_actions/wrapped/dist/post.js")
            })
            .unwrap();
        assert!(post_call.1.contains(&"STATE_preKey=pre-value".into()));
        assert!(post_call.1.contains(&"STATE_mainKey=main-value".into()));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_pre_action_registers_post_before_main() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "wrapped".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: Some("/__a/_actions/wrapped/dist/pre.js".into()),
                pre_condition: None,
                main_container_path: "/__a/_actions/wrapped/dist/main.js".into(),
                post_container_path: Some("/__a/_actions/wrapped/dist/post.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/wrapped".into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: false,
        }];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 9, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 9);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[0].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/wrapped/dist/pre.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/wrapped/dist/post.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn skips_javascript_post_action_when_post_if_is_false() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/cache/dist/restore.js".into(),
                    post_container_path: Some("/__a/_actions/cache/dist/save.js".into()),
                    post_condition: Some("success()".into()),
                    action_container_path: "/__a/_actions/cache".into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner {
            calls: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            codes: vec![0, 0, 0, 1, 0, 0],
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[1].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .count();
        assert_eq!(node_calls, 1);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_post_action_inherits_continue_on_error() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![ExecutableStep::JavaScript {
            step_id: "sccache".into(),
            display_name: String::new(),
            invocation: JavaScriptActionInvocation {
                node: "node20".into(),
                pre_container_path: None,
                pre_condition: None,
                main_container_path: "/__a/_actions/sccache/dist/setup/index.js".into(),
                post_container_path: Some("/__a/_actions/sccache/dist/show_stats/index.js".into()),
                post_condition: None,
                action_container_path: "/__a/_actions/sccache".into(),
                env: Vec::new(),
            },
            condition: None,
            continue_on_error: true,
        }];
        let mut executor = DockerScriptExecutor::new(FailingPostRunner { calls: Vec::new() });

        let results = executor
            .execute_ordered_steps_with_context(&container(&temp), &steps, &[], &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].exit_code, 0);
        assert!(!results[0].failure_ignored);
        assert_eq!(results[1].exit_code, 1);
        assert!(results[1].failure_ignored);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_sccache_action_soft_fails_and_gates_wrapper_step() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "sccache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path:
                        "/__a/_actions/mozilla-actions_sccache-action/dist/setup/index.js".into(),
                    post_container_path: Some(
                        "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js"
                            .into(),
                    ),
                    post_condition: None,
                    action_container_path: "/__a/_actions/mozilla-actions_sccache-action".into(),
                    env: vec![
                        (
                            "INPUT_TOKEN".into(),
                            "${{ github.server_url == 'https://github.com' && github.token || '' }}"
                                .into(),
                        ),
                        ("INPUT_DISABLE_ANNOTATIONS".into(), "false".into()),
                    ],
                },
                condition: None,
                continue_on_error: true,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable-sccache".into(),
                display_name: String::new(),
                script: "echo RUSTC_WRAPPER=sccache >> \"$GITHUB_ENV\"".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w".into(),
                env: Vec::new(),
                condition: Some("steps.sccache.outcome == 'success'".into()),
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(FailingPostRunner { calls: Vec::new() });

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[
                    ("GITHUB_TOKEN".into(), "ghs_token".into()),
                    ("GITHUB_SERVER_URL".into(), "https://github.com".into()),
                    ("GITHUB_REPOSITORY".into(), "jackin-project/jackin".into()),
                    ("RUNNER_TEMP".into(), "/__t".into()),
                ],
                &temp,
            )
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].exit_code, 0);
        assert!(!results[1].skipped);
        assert_eq!(results[2].exit_code, 1);
        assert!(results[2].failure_ignored);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(call.contains(&"INPUT_TOKEN=ghs_token".into()));
            assert!(call.contains(&"INPUT_DISABLE_ANNOTATIONS=false".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=jackin-project/jackin".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
        }
        assert!(node_calls[0].ends_with(&[
            "node:24-bookworm".into(),
            "node".into(),
            "/__a/_actions/mozilla-actions_sccache-action/dist/setup/index.js".into()
        ]));
        assert!(node_calls[1].ends_with(&[
            "node:24-bookworm".into(),
            "node".into(),
            "/__a/_actions/mozilla-actions_sccache-action/dist/show_stats/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn javascript_post_if_reads_env_after_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/rust-cache/dist/restore/index.js".into(),
                    post_container_path: Some("/__a/_actions/rust-cache/dist/save/index.js".into()),
                    post_condition: Some("success() || env.CACHE_ON_FAILURE == 'true'".into()),
                    action_container_path: "/__a/_actions/rust-cache".into(),
                    env: Vec::new(),
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable".into(),
                display_name: String::new(),
                script: "echo CACHE_ON_FAILURE=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let mut executor = DockerScriptExecutor::new(EnvAndFailureRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &[], &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[2].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:20-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        assert!(node_calls[1].ends_with(&[
            "node:20-bookworm".into(),
            "node".into(),
            "/__a/_actions/rust-cache/dist/save/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn target_rust_cache_receives_runtime_env_and_posts_on_failure() {
        let temp = temp_dir();
        fs::create_dir_all(&temp).unwrap();
        let steps = vec![
            ExecutableStep::JavaScript {
                step_id: "rust-cache".into(),
                display_name: String::new(),
                invocation: JavaScriptActionInvocation {
                    node: "node24".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/Swatinem_rust-cache/dist/restore/index.js"
                        .into(),
                    post_container_path: Some(
                        "/__a/_actions/Swatinem_rust-cache/dist/save/index.js".into(),
                    ),
                    post_condition: Some("success() || env.CACHE_ON_FAILURE == 'true'".into()),
                    action_container_path: "/__a/_actions/Swatinem_rust-cache".into(),
                    env: vec![
                        (
                            "INPUT_CACHE-DIRECTORIES".into(),
                            "~/.cache/rust-script".into(),
                        ),
                        ("INPUT_SHARED-KEY".into(), "kestra-rust-build-cache".into()),
                        ("INPUT_CACHE-ON-FAILURE".into(), "true".into()),
                    ],
                },
                condition: None,
                continue_on_error: false,
            },
            ExecutableStep::Script(ScriptStep {
                id: "enable".into(),
                display_name: String::new(),
                script: "echo CACHE_ON_FAILURE=true >> $GITHUB_ENV".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
            ExecutableStep::Script(ScriptStep {
                id: "fail".into(),
                display_name: String::new(),
                script: "exit 1".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/kestra-docker-containers".into(),
                env: Vec::new(),
                condition: None,
                continue_on_error: false,
            }),
        ];
        let runtime_env = vec![
            (
                "GITHUB_REPOSITORY".into(),
                "ChainArgos/java-monorepo".into(),
            ),
            ("GITHUB_WORKSPACE".into(), "/__w".into()),
            ("RUNNER_TEMP".into(), "/__t".into()),
            ("ACTIONS_RUNTIME_TOKEN".into(), "runtime-token".into()),
            ("ACTIONS_CACHE_URL".into(), "https://cache.actions".into()),
            ("ACTIONS_CACHE_SERVICE_V2".into(), "True".into()),
        ];
        let mut executor = DockerScriptExecutor::new(EnvAndFailureRunner {
            calls: Vec::new(),
            temp: temp.clone(),
        });

        let results = executor
            .execute_ordered_steps(&container(&temp), &steps, &runtime_env, &temp)
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[2].exit_code, 1);
        let node_calls = executor
            .runner()
            .calls
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|arg| arg == "run")
                    && args.contains(&"node:24-bookworm".into())
            })
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(node_calls.len(), 2);
        for call in &node_calls {
            assert!(call.contains(&"HOME=/github/home".into()));
            assert!(call.contains(&"GITHUB_REPOSITORY=ChainArgos/java-monorepo".into()));
            assert!(call.contains(&"GITHUB_WORKSPACE=/__w".into()));
            assert!(call.contains(&"RUNNER_TEMP=/__t".into()));
            assert!(call.contains(&"ACTIONS_RUNTIME_TOKEN=runtime-token".into()));
            assert!(call.contains(&"ACTIONS_CACHE_URL=https://cache.actions".into()));
            assert!(call.contains(&"ACTIONS_CACHE_SERVICE_V2=True".into()));
        }
        assert!(node_calls[0].contains(&"INPUT_SHARED-KEY=kestra-rust-build-cache".into()));
        assert!(node_calls[0].contains(&"INPUT_CACHE-ON-FAILURE=true".into()));
        assert!(node_calls[1].contains(&"CACHE_ON_FAILURE=true".into()));
        assert!(node_calls[1].ends_with(&[
            "node:24-bookworm".into(),
            "node".into(),
            "/__a/_actions/Swatinem_rust-cache/dist/save/index.js".into()
        ]));
        fs::remove_dir_all(temp).unwrap();
    }

    fn target_expression_context() -> Vec<(String, Value)> {
        vec![
            (
                "github".into(),
                serde_json::json!({
                    "repository": "jackin-project/jackin",
                    "event": {
                        "pull_request": { "number": 42 },
                        "workflow_run": {
                            "conclusion": "success",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "def456",
                            "head_repository": {
                                "full_name": "jackin-project/jackin"
                            }
                        }
                    }
                }),
            ),
            (
                "inputs".into(),
                serde_json::json!({
                    "app": "bitcoin-processor-app",
                    "image": "docker.io/chainargos/bitcoin-processor-app",
                    "package": "prod",
                    "packages": "bitcoin-processor-app",
                    "publish": false,
                    "push": true,
                    "targets": "bitcoin-processor-app"
                }),
            ),
            (
                "matrix".into(),
                serde_json::json!({
                    "arch": "x86_64",
                    "os": "ubuntu-latest",
                    "platform": "linux-amd64",
                    "runner": "ubuntu-latest",
                    "target": "x86_64-unknown-linux-gnu",
                    "zigbuild": true,
                    "zigbuild_target": "x86_64-unknown-linux-gnu"
                }),
            ),
            (
                "needs".into(),
                serde_json::json!({
                    "changes": {
                        "outputs": {
                            "bake-targets": "bitcoin-processor-app",
                            "bitcoin-processor": "true",
                            "blockchain-explorer": "true",
                            "coingecko-pricing": "true",
                            "construct": "true",
                            "docs": "true",
                            "eth-grpc-server": "true",
                            "eth-processor": "true",
                            "is_publish": "false",
                            "legacy-grpc-server": "true",
                            "rust": "true",
                            "tron-grpc-server": "true",
                            "tron-processor": "true"
                        },
                        "result": "success"
                    },
                    "check": { "result": "success" },
                    "check-version": {
                        "outputs": { "version": "1.2.3" },
                        "result": "success"
                    },
                    "docker-bake": { "result": "success" },
                    "release": {
                        "outputs": {
                            "arm64_linux": "sha",
                            "arm64_macos": "sha",
                            "capsule_arm64_linux": "sha",
                            "capsule_x86_linux": "sha",
                            "x86_linux": "sha",
                            "x86_macos": "sha"
                        },
                        "result": "success"
                    },
                    "source-changed": {
                        "outputs": {
                            "sha": "abc123",
                            "source": "true"
                        },
                        "result": "success"
                    },
                    "test-bitcoin-processor": { "result": "success" },
                    "test-blockchain-explorer": { "result": "success" },
                    "test-coingecko-pricing": { "result": "success" },
                    "test-eth-grpc-server": { "result": "success" },
                    "test-eth-processor": { "result": "success" },
                    "test-legacy-grpc-server": { "result": "success" },
                    "test-tron-grpc-server": { "result": "success" },
                    "test-tron-processor": { "result": "success" }
                }),
            ),
            (
                "secrets".into(),
                serde_json::json!({
                    "DOCKERHUB_TOKEN": "secret",
                    "DOCKERHUB_USERNAME": "user",
                    "GH_READONLY_TOKEN": "secret",
                    "GITHUB_TOKEN": "secret",
                    "HOMEBREW_TAP_TOKEN": "secret",
                    "RENOVATE_TOKEN": "secret"
                }),
            ),
        ]
    }

    fn workflow_files(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut files = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn action_metadata_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_action_metadata_files(root, &mut files);
        files.sort();
        files
    }

    fn collect_action_metadata_files(root: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_action_metadata_files(&path, files);
                continue;
            }
            if path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| matches!(file_name, "action.yml" | "action.yaml"))
            {
                files.push(path);
            }
        }
    }

    fn collect_yaml_strings<'a>(value: &'a serde_yaml::Value, strings: &mut Vec<&'a str>) {
        match value {
            serde_yaml::Value::String(value) => strings.push(value),
            serde_yaml::Value::Sequence(sequence) => {
                for value in sequence {
                    collect_yaml_strings(value, strings);
                }
            }
            serde_yaml::Value::Mapping(map) => {
                for (key, value) in map {
                    collect_yaml_strings(key, strings);
                    collect_yaml_strings(value, strings);
                }
            }
            _ => {}
        }
    }

    fn collect_yaml_key_strings<'a>(
        value: &'a serde_yaml::Value,
        name: &str,
        strings: &mut Vec<&'a str>,
    ) {
        match value {
            serde_yaml::Value::Mapping(map) => {
                for (key, value) in map {
                    if key.as_str() == Some(name) {
                        if let Some(value) = value.as_str() {
                            strings.push(value);
                        }
                    }
                    collect_yaml_key_strings(value, name, strings);
                }
            }
            serde_yaml::Value::Sequence(sequence) => {
                for value in sequence {
                    collect_yaml_key_strings(value, name, strings);
                }
            }
            _ => {}
        }
    }
}

#![allow(dead_code)]

use crate::{
    action::JavaScriptActionInvocation,
    container::JobContainerSpec,
    script_step::{ScriptStep, ScriptStepPlan, StepCommandState},
};
use anyhow::{bail, Context, Result};
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
}

#[derive(Debug, Clone)]
pub enum ExecutableStep {
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        invocation: JavaScriptActionInvocation,
    },
}

impl ExecutableStep {
    fn id(&self) -> &str {
        match self {
            ExecutableStep::Script(step) => &step.id,
            ExecutableStep::JavaScript { step_id, .. } => step_id,
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
        let result =
            self.execute_ordered_steps_in_started_container(container, &ordered, &[], temp_host);

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
        self.run_docker(&container.create_network_args())?;
        self.run_docker(&container.start_args())?;

        let result =
            self.execute_ordered_steps_in_started_container(container, steps, base_env, temp_host);

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
        temp_host: &Path,
    ) -> Result<Vec<StepExecutionResult>> {
        let mut results = Vec::new();
        let mut state = JobExecutionState::new(base_env);
        let mut step_error = None;
        for step in steps {
            let step_id = step.id().to_string();
            let result = (|| match step {
                ExecutableStep::Script(step) => {
                    let step = state.resolve_script_step(step);
                    let plan = ScriptStepPlan::prepare_with_path(&step, temp_host, &state.path)?;
                    let env = state.step_env(&plan.env);
                    let exec_args = container.exec_script_args(
                        &plan.script_container_path,
                        plan.shell,
                        &plan.working_directory_container,
                        &env,
                    );
                    let step_result = self.runner.run("docker", &exec_args)?;
                    let command_state = plan.collect_state()?;
                    Ok(StepExecutionResult {
                        exit_code: step_result.code,
                        state: command_state,
                    })
                }
                ExecutableStep::JavaScript {
                    step_id,
                    invocation,
                } => self.execute_javascript_action_in_started_container(
                    container, step_id, invocation, temp_host, &state,
                ),
            })();

            match result {
                Ok(result) => {
                    let failed = result.exit_code != 0;
                    state.apply(&step_id, &result.state);
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
            },
            temp_host,
        )?;
        let mut env = state.step_env(&command_files.env);
        env.extend(state.resolve_env(&action.env));
        let exec_args = container.exec_process_args(
            "/__w",
            &env,
            &["node".to_string(), action.main_container_path.clone()],
        );
        let step_result = self.runner.run("docker", &exec_args)?;
        let state = command_files.collect_state()?;
        Ok(StepExecutionResult {
            exit_code: step_result.code,
            state,
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
    outputs: BTreeMap<String, BTreeMap<String, String>>,
    path: Vec<String>,
}

impl JobExecutionState {
    fn new(base_env: &[(String, String)]) -> Self {
        Self {
            env: base_env.iter().cloned().collect(),
            outputs: BTreeMap::new(),
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

    fn apply(&mut self, step_id: &str, step_state: &StepCommandState) {
        if !step_state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), step_state.outputs.clone());
        }
        for (name, value) in &step_state.env {
            self.env.insert(name.clone(), value.clone());
        }
        for path in step_state.path.iter().rev() {
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
            if let Some(output) = self.resolve_step_output_expression(expression) {
                rendered.push_str(output);
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
            },
            ScriptStep {
                id: "step2".into(),
                script: "echo two".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
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
            &StepCommandState {
                outputs: [("answer".to_string(), "42".to_string())].into(),
                env: [("NAME".to_string(), "value".to_string())].into(),
                path: vec!["/opt/tool".to_string()],
                ..Default::default()
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
            &StepCommandState {
                outputs: [("tags".to_string(), "image:latest".to_string())].into(),
                ..Default::default()
            },
        );

        let env =
            state.resolve_env(&[("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into())]);

        assert_eq!(env, vec![("INPUT_TAGS".into(), "image:latest".into())]);
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
            }),
            ExecutableStep::Script(ScriptStep {
                id: "consumer".into(),
                script: "echo answer=${{ steps.producer.outputs.answer }}".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
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
            }),
            ExecutableStep::JavaScript {
                step_id: "action1".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    main_container_path: "/__a/_actions/acme_action/v1/dist/index.js".into(),
                    action_container_path: "/__a/_actions/acme_action/v1".into(),
                    env: vec![("INPUT_NAME".into(), "value".into())],
                },
            },
            ExecutableStep::Script(ScriptStep {
                id: "step2".into(),
                script: "echo done".into(),
                shell: Shell::Sh,
                working_directory_container: "/__w/repo".into(),
            }),
        ];
        let mut executor = DockerScriptExecutor::new(RecordingRunner::default());

        let results = executor
            .execute_ordered_steps(
                &container(&temp),
                &steps,
                &[("GITHUB_REPOSITORY".into(), "acme/repo".into())],
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
        assert!(calls[3]
            .1
            .contains(&"GITHUB_OUTPUT=/__t/action1_output".into()));

        fs::remove_dir_all(temp).unwrap();
    }
}

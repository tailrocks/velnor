//! Scripted fault injection at the [`CommandRunner`](crate::executor::CommandRunner) seam.
//!
//! `CommandRunner` is the single choke point for every host process spawn, so
//! one decorator here injects Docker and git faults into any production code
//! path that spawns processes — checkout, container lifecycle, cache probes —
//! without touching call sites. Each injected fault names the
//! `velnor-bench` fault-catalogue class it exercises (see
//! `crates/velnor-bench/src/fault.rs`); the conformance tests below run real
//! production functions against injected failures and assert the production
//! containment behaviour, not the decorator's bookkeeping.
//!
//! This module is unit-test tooling. It never ships in any build: the module
//! is compiled only for `cfg(test)`. If a future integration harness (such as
//! the `velnor-job` benchmark driver) needs scripted injection, the gate
//! widens together with that user — not before.

use std::time::Duration;

use anyhow::{bail, Result};

use crate::executor::{CommandResult, CommandRunner, CommandStream, SpawnedProcess};

/// What an injected fault does to one intercepted invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FaultAction {
    /// The child "ran" and failed: a non-zero exit with stderr, exactly as a
    /// real failing child surfaces. The inner runner is not called.
    Fail { code: i32, stderr: String },
    /// Sleep first, then run the invocation for real. Models a slow daemon or
    /// remote without changing the outcome.
    Delay { duration: Duration },
    /// Fail the spawn itself, as an unreachable daemon or an expired deadline
    /// surfaces. The inner runner is not called.
    SpawnError { message: String },
}

/// One scripted fault: which invocations it hits, how often, and what it does.
#[derive(Debug, Clone)]
pub(crate) struct FaultRule {
    program: String,
    required_args: Vec<String>,
    remaining: usize,
    action: FaultAction,
}

impl FaultRule {
    /// Fault the next `times` invocations of `program` whose argument vector
    /// contains every string in `required_args` (exact argument equality).
    pub(crate) fn new(
        program: &str,
        required_args: &[&str],
        times: usize,
        action: FaultAction,
    ) -> Self {
        Self {
            program: program.to_owned(),
            required_args: required_args.iter().map(|arg| (*arg).to_owned()).collect(),
            remaining: times,
            action,
        }
    }

    fn matches(&self, program: &str, args: &[String]) -> bool {
        self.remaining > 0
            && self.program == program
            && self
                .required_args
                .iter()
                .all(|want| args.iter().any(|have| have == want))
    }
}

/// One intercepted invocation, faulted or passed through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterceptedCall {
    pub program: String,
    pub args: Vec<String>,
    /// Which catalogue class was injected, if any. `None` passed through.
    pub faulted: bool,
}

/// A [`CommandRunner`] that injects scripted faults into an inner runner.
#[derive(Debug)]
pub(crate) struct FaultInjectingRunner<R> {
    inner: R,
    rules: Vec<FaultRule>,
    intercepted: Vec<InterceptedCall>,
}

impl<R> FaultInjectingRunner<R> {
    pub(crate) fn new(inner: R, rules: Vec<FaultRule>) -> Self {
        Self {
            inner,
            rules,
            intercepted: Vec::new(),
        }
    }

    /// Every intercepted invocation, in order.
    pub(crate) fn intercepted(&self) -> &[InterceptedCall] {
        &self.intercepted
    }

    /// How many invocations were faulted rather than passed through.
    pub(crate) fn faulted_count(&self) -> usize {
        self.intercepted.iter().filter(|call| call.faulted).count()
    }

    pub(crate) fn into_inner(self) -> R {
        self.inner
    }

    fn intercept(&mut self, program: &str, args: &[String]) -> Option<FaultAction> {
        let mut action = None;
        for rule in &mut self.rules {
            if rule.matches(program, args) {
                rule.remaining -= 1;
                action = Some(rule.action.clone());
                break;
            }
        }
        self.intercepted.push(InterceptedCall {
            program: program.to_owned(),
            args: args.to_vec(),
            faulted: action.is_some(),
        });
        action
    }

    fn fail_result(code: i32, stderr: String) -> CommandResult {
        CommandResult {
            code,
            stdout: String::new(),
            stderr,
        }
    }
}

impl<R: CommandRunner> CommandRunner for FaultInjectingRunner<R> {
    fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run(program, args)
            }
            None => self.inner.run(program, args),
        }
    }

    fn is_host_process_runner(&self) -> bool {
        self.inner.is_host_process_runner()
    }

    fn spawn(&mut self, program: &str, args: &[String]) -> Result<SpawnedProcess> {
        self.inner.spawn(program, args)
    }

    fn kill(&mut self, process: &SpawnedProcess) -> Result<()> {
        self.inner.kill(process)
    }

    fn run_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run_timeout(program, args, timeout)
            }
            None => self.inner.run_timeout(program, args, timeout),
        }
    }

    fn run_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run_timeout_with_env(program, args, env, timeout)
            }
            None => self.inner.run_timeout_with_env(program, args, env, timeout),
        }
    }

    fn run_streaming_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => {
                on_output(CommandStream::Stderr, &stderr);
                Ok(Self::fail_result(code, stderr))
            }
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner
                    .run_streaming_timeout(program, args, timeout, on_output)
            }
            None => self
                .inner
                .run_streaming_timeout(program, args, timeout, on_output),
        }
    }

    fn run_streaming_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => {
                on_output(CommandStream::Stderr, &stderr);
                Ok(Self::fail_result(code, stderr))
            }
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner
                    .run_streaming_timeout_with_env(program, args, env, timeout, on_output)
            }
            None => self
                .inner
                .run_streaming_timeout_with_env(program, args, env, timeout, on_output),
        }
    }

    fn run_streaming(
        &mut self,
        program: &str,
        args: &[String],
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => {
                on_output(CommandStream::Stderr, &stderr);
                Ok(Self::fail_result(code, stderr))
            }
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run_streaming(program, args, on_output)
            }
            None => self.inner.run_streaming(program, args, on_output),
        }
    }

    fn run_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run_with_env(program, args, env)
            }
            None => self.inner.run_with_env(program, args, env),
        }
    }

    fn run_with_stdin_timeout(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner
                    .run_with_stdin_timeout(program, args, stdin, timeout)
            }
            None => self
                .inner
                .run_with_stdin_timeout(program, args, stdin, timeout),
        }
    }

    fn run_with_stdin_timeout_with_env(
        &mut self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        stdin: &str,
        timeout: Duration,
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner
                    .run_with_stdin_timeout_with_env(program, args, env, stdin, timeout)
            }
            None => self
                .inner
                .run_with_stdin_timeout_with_env(program, args, env, stdin, timeout),
        }
    }

    fn run_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &str,
    ) -> Result<CommandResult> {
        match self.intercept(program, args) {
            Some(FaultAction::Fail { code, stderr }) => Ok(Self::fail_result(code, stderr)),
            Some(FaultAction::SpawnError { message }) => bail!("{message}"),
            Some(FaultAction::Delay { duration }) => {
                std::thread::sleep(duration);
                self.inner.run_with_stdin(program, args, stdin)
            }
            None => self.inner.run_with_stdin(program, args, stdin),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inner runner that succeeds every invocation and records what it saw, so
    /// the tests prove what the decorator forwarded versus faulted.
    #[derive(Debug, Default)]
    struct SucceedingRunner {
        seen: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for SucceedingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.seen.push((program.to_owned(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
                stdout: "inner-ok".to_owned(),
                stderr: String::new(),
            })
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_matching_invocation_fails_without_reaching_the_inner_runner() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "docker",
                &["pull"],
                1,
                FaultAction::Fail {
                    code: 1,
                    stderr: "Error response from daemon: not found".to_owned(),
                },
            )],
        );
        let result = runner
            .run("docker", &args(&["pull", "example.invalid/image:latest"]))
            .expect("injected failure is a result, not a spawn error");
        assert_eq!(result.code, 1);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.contains("not found"));
        assert_eq!(runner.faulted_count(), 1);
        assert!(runner.into_inner().seen.is_empty());
    }

    #[test]
    fn a_rule_fires_only_its_budget_then_passes_through() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "git",
                &["fetch"],
                1,
                FaultAction::Fail {
                    code: 128,
                    stderr: "fatal: unable to connect".to_owned(),
                },
            )],
        );
        let first = runner
            .run("git", &args(&["fetch", "origin"]))
            .expect("first fetch");
        assert_eq!(first.code, 128);
        let second = runner
            .run("git", &args(&["fetch", "origin"]))
            .expect("second fetch");
        assert_eq!(second.code, 0);
        assert_eq!(second.stdout, "inner-ok");
        assert_eq!(runner.faulted_count(), 1);
        assert_eq!(runner.intercepted().len(), 2);
        assert_eq!(runner.into_inner().seen.len(), 1);
    }

    #[test]
    fn an_unmatched_program_passes_through_untouched() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "docker",
                &["pull"],
                usize::MAX,
                FaultAction::Fail {
                    code: 1,
                    stderr: "nope".to_owned(),
                },
            )],
        );
        // Same argument, different program: the rule must not fire.
        let result = runner
            .run("podman", &args(&["pull", "example.invalid/image:latest"]))
            .expect("podman passes through");
        assert_eq!(result.code, 0);
        // Same program, missing the required argument: must not fire either.
        let result = runner
            .run("docker", &args(&["images"]))
            .expect("docker images passes through");
        assert_eq!(result.code, 0);
        assert_eq!(runner.faulted_count(), 0);
        assert_eq!(runner.into_inner().seen.len(), 2);
    }

    #[test]
    fn a_spawn_error_surfaces_as_an_error_not_a_result() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "docker",
                &[],
                1,
                FaultAction::SpawnError {
                    message: "injected fault docker-daemon-unreachable: \
                              Cannot connect to the Docker daemon"
                        .to_owned(),
                },
            )],
        );
        let error = runner
            .run_timeout("docker", &args(&["info"]), Duration::from_secs(5))
            .expect_err("spawn errors fail the call");
        assert!(error.to_string().contains("docker-daemon-unreachable"));
        assert_eq!(runner.faulted_count(), 1);
    }

    #[test]
    fn a_delayed_invocation_still_runs_for_real() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "docker",
                &["inspect"],
                1,
                FaultAction::Delay {
                    duration: Duration::from_millis(25),
                },
            )],
        );
        let started = std::time::Instant::now();
        let result = runner
            .run_with_env("docker", &args(&["inspect", "x"]), &[])
            .expect("delayed inspect");
        assert!(started.elapsed() >= Duration::from_millis(25));
        assert_eq!(result.stdout, "inner-ok");
        assert_eq!(runner.faulted_count(), 1);
        assert_eq!(runner.into_inner().seen.len(), 1);
    }

    #[test]
    fn a_streaming_failure_still_reports_stderr_to_the_consumer() {
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "git",
                &["fetch"],
                1,
                FaultAction::Fail {
                    code: 128,
                    stderr: "fatal: the remote hung up".to_owned(),
                },
            )],
        );
        let mut streamed = Vec::new();
        let result = runner
            .run_streaming("git", &args(&["fetch"]), &mut |stream, text: &str| {
                streamed.push((stream, text.to_owned()));
            })
            .expect("streaming failure is a result");
        assert_eq!(result.code, 128);
        assert_eq!(
            streamed,
            vec![(
                CommandStream::Stderr,
                "fatal: the remote hung up".to_owned()
            )]
        );
    }

    #[test]
    fn every_run_entry_point_intercepts() {
        // A fault must apply no matter which trait method the caller uses;
        // otherwise the injection silently misses the production path.
        let mut runner = FaultInjectingRunner::new(
            SucceedingRunner::default(),
            vec![FaultRule::new(
                "docker",
                &[],
                usize::MAX,
                FaultAction::Fail {
                    code: 125,
                    stderr: "injected".to_owned(),
                },
            )],
        );
        let argv = args(&["ps"]);
        let timeout = Duration::from_secs(1);
        assert_eq!(runner.run("docker", &argv).expect("run").code, 125);
        assert_eq!(
            runner
                .run_timeout("docker", &argv, timeout)
                .expect("run_timeout")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_timeout_with_env("docker", &argv, &[], timeout)
                .expect("run_timeout_with_env")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_with_env("docker", &argv, &[])
                .expect("run_with_env")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_with_stdin("docker", &argv, "")
                .expect("run_with_stdin")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_with_stdin_timeout("docker", &argv, "", timeout)
                .expect("run_with_stdin_timeout")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_with_stdin_timeout_with_env("docker", &argv, &[], "", timeout)
                .expect("run_with_stdin_timeout_with_env")
                .code,
            125
        );
        let mut sink = |_: CommandStream, _: &str| {};
        assert_eq!(
            runner
                .run_streaming("docker", &argv, &mut sink)
                .expect("run_streaming")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_streaming_timeout("docker", &argv, timeout, &mut sink)
                .expect("run_streaming_timeout")
                .code,
            125
        );
        assert_eq!(
            runner
                .run_streaming_timeout_with_env("docker", &argv, &[], timeout, &mut sink)
                .expect("run_streaming_timeout_with_env")
                .code,
            125
        );
        assert_eq!(runner.faulted_count(), 10);
        assert!(runner.into_inner().seen.is_empty());
    }

    /// Inner runner for the production-path proofs: succeeds like a healthy
    /// host while recording the calls production actually issued.
    #[derive(Debug, Default)]
    struct HealthyHostRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl CommandRunner for HealthyHostRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_owned(), args.to_vec()));
            Ok(CommandResult {
                code: 0,
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
            let _ = env;
            self.run(program, args)
        }
    }

    fn checkout_plan(destination: &std::path::Path) -> crate::checkout::CheckoutPlan {
        crate::checkout::CheckoutPlan {
            step_id: "checkout".into(),
            display_name: String::new(),
            clone_url: "https://github.com/acme/repo.git".into(),
            version: Some("abc123".into()),
            destination: destination.to_path_buf(),
            token: Some("token".into()),
            fetch_depth: Some(1),
            fetch_tags: false,
            persist_credentials: false,
            clean: false,
            lfs: false,
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }
    }

    fn fault_test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "velnor-fault-injection-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn an_injected_fetch_failure_fails_the_real_checkout_closed() {
        // Catalogue class git-fetch-failure: production must surface the
        // failure in the step log with its exit code and stop the checkout
        // sequence instead of checking out whatever FETCH_HEAD happens to be.
        let destination = fault_test_dir("fetch-failure");
        let plan = checkout_plan(&destination);
        let mut runner = FaultInjectingRunner::new(
            HealthyHostRunner::default(),
            vec![FaultRule::new(
                "git",
                &["fetch"],
                1,
                FaultAction::Fail {
                    code: 128,
                    stderr: "fatal: unable to connect to github.com".to_owned(),
                },
            )],
        );
        let mut log = Vec::new();
        let error = crate::checkout::execute_checkout(&mut runner, &plan, &mut log)
            .expect_err("an injected fetch failure must fail checkout");
        assert!(
            error.to_string().contains("128"),
            "unexpected error: {error:#}"
        );
        assert!(
            log.iter()
                .any(|line| line.contains("[command]git") && line.contains("fetch")),
            "the step log must show the failed fetch: {log:?}"
        );
        assert!(
            log.iter().any(|line| line.contains("unable to connect")),
            "the step log must carry the failure reason: {log:?}"
        );
        // Nothing after the fetch ran: no workspace checkout, no mtime pass.
        let inner = runner.into_inner();
        assert!(
            !inner.calls.iter().any(|(_, args)| args
                .iter()
                .any(|arg| arg == "checkout" || arg == "--format=%ct")),
            "checkout continued past a failed fetch: {:?}",
            inner.calls
        );
        std::fs::remove_dir_all(&destination).ok();
    }

    #[test]
    fn an_unreachable_remote_fails_the_real_checkout_loudly() {
        // Catalogue class git-remote-unreachable: a spawn-level failure is an
        // error, never a silent skip of the checkout.
        let destination = fault_test_dir("remote-unreachable");
        let plan = checkout_plan(&destination);
        let mut runner = FaultInjectingRunner::new(
            HealthyHostRunner::default(),
            vec![FaultRule::new(
                "git",
                &[],
                usize::MAX,
                FaultAction::SpawnError {
                    message: "injected fault git-remote-unreachable: \
                              failed to resolve host github.com"
                        .to_owned(),
                },
            )],
        );
        let mut log = Vec::new();
        let error = crate::checkout::execute_checkout(&mut runner, &plan, &mut log)
            .expect_err("an unreachable remote must fail checkout");
        assert!(
            error.to_string().contains("git-remote-unreachable"),
            "unexpected error: {error:#}"
        );
        std::fs::remove_dir_all(&destination).ok();
    }

    #[test]
    fn a_rule_that_matches_nothing_leaves_production_untouched() {
        let destination = fault_test_dir("no-match");
        let plan = checkout_plan(&destination);
        let mut runner = FaultInjectingRunner::new(
            HealthyHostRunner::default(),
            vec![FaultRule::new(
                "docker",
                &["pull"],
                usize::MAX,
                FaultAction::Fail {
                    code: 1,
                    stderr: "must not fire".to_owned(),
                },
            )],
        );
        let mut log = Vec::new();
        crate::checkout::execute_checkout(&mut runner, &plan, &mut log)
            .expect("checkout with no matching rule must succeed");
        assert_eq!(runner.faulted_count(), 0);
        assert!(
            runner
                .into_inner()
                .calls
                .iter()
                .any(|(_, args)| args.contains(&"fetch".to_owned())),
            "production never issued its fetch"
        );
        std::fs::remove_dir_all(&destination).ok();
    }
}
